use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use maistro::{
    codex::Codex,
    config::{Config, Project},
    engine::Engine,
    git,
    github::Github,
    store::Store,
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};
#[derive(Parser)]
#[command(
    name = "maistro",
    version,
    about = "Durable AppFlowy/Codex coding orchestrator"
)]
struct Cli {
    #[arg(short, long, global = true, default_value = "config.toml")]
    config: PathBuf,
    /// Emit JSON for scripts instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    #[command(hide = true)]
    InternalBridge {
        #[arg(long)]
        socket: PathBuf,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    #[command(hide = true)]
    InternalMcp {
        #[arg(long)]
        scope: PathBuf,
    },
    /// Run the daemon; --once performs one reconciliation cycle.
    Run {
        #[arg(long)]
        once: bool,
    },
    /// Check repositories, isolation, GitHub permissions, models and boards (read-only).
    Validate,
    /// Open the selected configuration file in $EDITOR.
    Config,
    /// Print configured projects.
    Projects,
    /// Print durable execution state and diagnostics.
    Status,
    /// Project configuration and dispatch controls.
    Project {
        #[command(subcommand)]
        command: ProjectAction,
    },
    /// Stop new dispatch and hold automatic merges; local work may finish.
    Pause,
    /// Resume dispatch and merge eligibility.
    Resume,
    /// Retry a terminal run (requires unchanged Ready story; resets retry budget).
    Retry { id: String },
    /// Cancel a run; never deletes its branch or PR.
    Cancel { id: String },
    /// Request validated configuration reload after active work drains.
    Reload,
}
#[derive(Subcommand)]
enum ProjectAction {
    Add,
    Pause { id: String },
    Resume { id: String },
}
#[tokio::main]
async fn main() -> Result<()> {
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    let logging = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr);
    if cli.json {
        logging.json().init();
    } else {
        logging.with_ansi(false).init();
    }
    if matches!(cli.command, Action::Config) {
        return edit_config(&cli.config);
    }
    if let Action::InternalBridge { socket, command } = &cli.command {
        let code = maistro::network::bridge(socket, command).await?;
        std::process::exit(code);
    }
    if let Action::InternalMcp { scope } = &cli.command {
        return maistro::mcp_guard::serve(scope).await;
    }
    if matches!(
        cli.command,
        Action::Project {
            command: ProjectAction::Add
        }
    ) {
        return add_project(&cli.config, cli.json).await;
    }
    let c = Config::load(&cli.config)?;
    match cli.command {
        Action::Run { once } => Engine::new(c)?.run_daemon(&cli.config, once).await?,
        Action::Projects => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&c.projects)?);
            } else {
                print_projects(&c.projects);
            }
        }
        Action::Validate => validate(c, cli.json).await?,
        other => {
            let store = Store::open(&c.orchestrator.state_db)?;
            store.sync_config(&c)?;
            let is_retry = matches!(&other, Action::Retry { .. });
            let confirmation = match &other {
                Action::Pause => Some("Pause requested for all projects.".to_owned()),
                Action::Resume => Some("Resume requested for all projects.".to_owned()),
                Action::Reload => Some("Configuration reload requested.".to_owned()),
                Action::Project {
                    command: ProjectAction::Pause { id },
                } => Some(format!("Project {id} paused.")),
                Action::Project {
                    command: ProjectAction::Resume { id },
                } => Some(format!("Project {id} resumed.")),
                Action::Retry { id } => Some(format!("Retry requested for {id}.")),
                Action::Cancel { id } => Some(format!("Cancellation requested for {id}.")),
                _ => None,
            };
            match other {
                Action::Status => {
                    let diagnostics = store.diagnostics()?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&diagnostics)?);
                    } else {
                        print_status(&diagnostics);
                    }
                }
                Action::Pause => store.set_control("paused", "true")?,
                Action::Resume => store.set_control("paused", "false")?,
                Action::Reload => store.set_control("reload", &uuid::Uuid::new_v4().to_string())?,
                Action::Project {
                    command: ProjectAction::Pause { id },
                } => store.project_pause(&id, true)?,
                Action::Project {
                    command: ProjectAction::Resume { id },
                } => store.project_pause(&id, false)?,
                Action::Retry { id } | Action::Cancel { id } => {
                    let found: Vec<_> = store
                        .runs()?
                        .into_iter()
                        .filter(|r| r.id == id || r.story.story_id == id)
                        .collect();
                    ensure!(
                        found.len() == 1,
                        "ID must identify exactly one run; use a full run ID from status"
                    );
                    let op = if is_retry { "retry" } else { "cancel" };
                    store.set_control(&format!("{op}:{}", found[0].id), "true")?;
                }
                _ => unreachable!(),
            }
            if !cli.json
                && let Some(message) = confirmation
            {
                println!("{message}");
            }
        }
    }
    Ok(())
}
async fn validate(c: Config, json: bool) -> Result<()> {
    let engine = Engine::new(c.clone())?;
    engine.preflight().await?;
    let token = tokio_util::sync::CancellationToken::new();
    for p in c.projects.iter().filter(|p| p.enabled) {
        let scan = engine.codex.scan(p, &token).await?;
        engine.store.record_scan(&scan)?;
        if json {
            println!(
                "{}",
                serde_json::json!({"project":p.id,"board":scan.board_id,"stories":scan.stories.len(),"valid":true})
            );
        } else {
            println!(
                "{}: valid (board {}, {} stories)",
                p.id,
                scan.board_id,
                scan.stories.len()
            );
        }
    }
    Ok(())
}
fn prompt(label: &str, default: &str) -> Result<String> {
    eprint!("{label} [{default}]: ");
    io::stderr().flush()?;
    let mut line = String::new();
    ensure!(io::stdin().read_line(&mut line)? > 0, "input closed");
    let value = line.trim();
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}
async fn add_project(path: &Path, json: bool) -> Result<()> {
    let target = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = target.parent().context("config parent missing")?;
    let mut c = if path.exists() {
        Config::load(path)?
    } else {
        Config {
            orchestrator: Default::default(),
            agents: Default::default(),
            github: Default::default(),
            projects: vec![],
        }
    };
    c.resolve_runtime_paths(parent)?;
    let id = prompt("Stable project ID", "")?;
    let name = prompt("Project name", &id)?;
    let board = prompt("AppFlowy URL or exact page name", "")?;
    let repo = PathBuf::from(prompt("Absolute repository path", "")?);
    let base_branch = prompt("Base branch", "main")?;
    let max_workers = prompt("Worker limit (MVP: 1)", "1")?.parse()?;
    let remote = git::git(&repo, &["remote", "get-url", "origin"]).await?;
    let github_repo = remote
        .trim_end_matches(".git")
        .strip_prefix("https://github.com/")
        .or_else(|| {
            remote
                .trim_end_matches(".git")
                .strip_prefix("git@github.com:")
        })
        .context("origin must point to GitHub")?
        .to_owned();
    let commands = prompt(
        "Test commands as JSON argument arrays",
        r#"[["cargo","test","--locked","--offline"]]"#,
    )?;
    let checks = prompt("Required CI check names, comma-separated", "test")?;
    let p = Project {
        id,
        name,
        repo,
        appflowy_url: board.starts_with("https://").then(|| board.clone()),
        appflowy_page: (!board.starts_with("https://")).then_some(board),
        base_branch,
        max_workers,
        continue_while_prs_pending: true,
        enabled: true,
        github_repo,
        required_checks: checks
            .split(',')
            .map(|s| s.trim().into())
            .filter(|s: &String| !s.is_empty())
            .collect(),
        test_commands: serde_json::from_str(&commands)?,
        statuses: Default::default(),
        protected_paths: vec![],
        allowed_paths: vec![],
        toolchain_paths: vec![],
    };
    c.projects.push(p.clone());
    c.validate()?;
    git::validate(&p).await?;
    Github::new(&c.github.token_env)?
        .validate(&p.github_repo, c.github.auto_merge)
        .await?;
    Codex::new(c.clone())
        .scan(&p, &tokio_util::sync::CancellationToken::new())
        .await?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".maistro-config-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(toml::to_string_pretty(&c)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, &target)?;
    std::fs::File::open(parent)?.sync_all()?;
    if json {
        println!("{}", serde_json::json!({"added": p.id, "config": target}));
    } else {
        println!("Added {} to {}", p.id, target.display());
    }
    Ok(())
}

fn edit_config(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR")
        .context("Set $EDITOR to the editor command to use (for example, nano or 'code --wait')")?;
    ensure!(!editor.trim().is_empty(), "$EDITOR must not be empty");
    // Pass the path separately so spaces and shell metacharacters stay literal.
    // An absolute path also prevents a filename beginning with '-' becoming an option.
    let target = std::path::absolute(path)?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("exec {editor} \"$1\""))
        .arg("maistro-config")
        .arg(&target)
        .status()
        .context("could not launch $EDITOR")?;
    ensure!(status.success(), "$EDITOR exited unsuccessfully: {status}");
    Ok(())
}

fn print_projects(projects: &[Project]) {
    println!("Projects ({})", projects.len());
    for p in projects {
        println!(
            "\n{} — {} ({})",
            p.id,
            p.name,
            if p.enabled { "enabled" } else { "disabled" }
        );
        println!("  Repository: {}", p.repo.display());
        println!(
            "  Board: {}",
            p.appflowy_url
                .as_deref()
                .or(p.appflowy_page.as_deref())
                .unwrap_or("not configured")
        );
        println!("  GitHub: {}", p.github_repo);
        println!("  Base branch: {}", p.base_branch);
    }
}

fn print_status(d: &serde_json::Value) {
    println!(
        "Dispatch: {}",
        if d["paused"] == true {
            "paused"
        } else {
            "enabled"
        }
    );
    println!("Events: {}", d["event_count"]);
    println!("Pending board updates: {}", d["pending_projections"]);
    if let Some(projects) = d["projects"].as_array() {
        println!("\nProjects ({})", projects.len());
        for p in projects {
            println!(
                "  {} — {}",
                p["id"].as_str().unwrap_or("unknown"),
                if p["paused"] == true {
                    "paused"
                } else {
                    "not paused"
                }
            );
            println!(
                "    Last scan (Unix seconds): {}",
                p["last_scan"]
                    .as_i64()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "never".into())
            );
        }
    }
    if let Some(runs) = d["runs"].as_array() {
        println!("\nRuns ({})", runs.len());
        if runs.is_empty() {
            println!("  No runs recorded.");
        }
        for r in runs {
            println!(
                "\n  {} — {}",
                r["id"].as_str().unwrap_or("unknown"),
                r["story"]["title"].as_str().unwrap_or("untitled")
            );
            println!(
                "    Project: {}",
                r["project_id"].as_str().unwrap_or("unknown")
            );
            println!(
                "    Story: {}",
                r["story"]["story_id"].as_str().unwrap_or("unknown")
            );
            println!(
                "    State: {} | Attempt: {} | Failures: {}",
                r["state"].as_str().unwrap_or("unknown").replace('_', " "),
                r["attempt"],
                r["failures"]
            );
            for (key, label) in [
                ("branch", "Branch"),
                ("worktree", "Worktree"),
                ("pr_url", "PR"),
                ("last_error", "Last error"),
            ] {
                if let Some(value) = r[key].as_str().filter(|s| !s.is_empty()) {
                    println!("    {label}: {}", value.replace('\n', "\n      "));
                }
            }
        }
    }
}
