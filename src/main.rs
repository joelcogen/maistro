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
    /// Print configured projects as JSON.
    Projects,
    /// Print durable execution state and diagnostics as JSON.
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
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
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
        return add_project(&cli.config).await;
    }
    let c = Config::load(&cli.config)?;
    match cli.command {
        Action::Run { once } => Engine::new(c)?.run_daemon(&cli.config, once).await?,
        Action::Projects => println!("{}", serde_json::to_string_pretty(&c.projects)?),
        Action::Validate => validate(c).await?,
        other => {
            let store = Store::open(&c.orchestrator.state_db)?;
            store.sync_config(&c)?;
            let is_retry = matches!(&other, Action::Retry { .. });
            match other {
                Action::Status => {
                    println!("{}", serde_json::to_string_pretty(&store.diagnostics()?)?)
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
        }
    }
    Ok(())
}
async fn validate(c: Config) -> Result<()> {
    let engine = Engine::new(c.clone())?;
    engine.preflight().await?;
    let token = tokio_util::sync::CancellationToken::new();
    for p in c.projects.iter().filter(|p| p.enabled) {
        let scan = engine.codex.scan(p, &token).await?;
        engine.store.record_scan(&scan)?;
        println!(
            "{}",
            serde_json::json!({"project":p.id,"board":scan.board_id,"stories":scan.stories.len(),"valid":true})
        );
    }
    Ok(())
}
fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    ensure!(io::stdin().read_line(&mut line)? > 0, "input closed");
    let value = line.trim();
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}
async fn add_project(path: &Path) -> Result<()> {
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
    println!("Added {} to {}", p.id, target.display());
    Ok(())
}
