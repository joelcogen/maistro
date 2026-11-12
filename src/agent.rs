use crate::{
    config::{Config, Mode, Project, Provider},
    github,
    model::{self, Assignment, Coordination, Report, Snapshot},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct AgentRunner {
    pub config: Config,
}

impl AgentRunner {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Reuse the selected server transport/auth; expose only coordination tools.
    pub fn mcp_config(&self, project: &Project) -> Result<toml::Value> {
        if self.config.agents.provider == Provider::Claude {
            return crate::claude::poll_config(&self.config, project);
        }
        let config: toml::Value = toml::from_str(&std::fs::read_to_string(
            self.config.agents.codex_home.join("config.toml"),
        )?)?;
        let mut server = config
            .get("mcp_servers")
            .and_then(|v| v.get(project.mcp_server(&self.config.agents)))
            .with_context(|| {
                format!(
                    "{}: configured MCP server {} missing",
                    project.id,
                    project.mcp_server(&self.config.agents)
                )
            })?
            .clone();
        let table = server.as_table_mut().context("invalid MCP configuration")?;
        let (reads, writes) = source_tools(project);
        table.insert(
            "enabled_tools".into(),
            toml::Value::Array(reads.iter().chain(writes).map(|s| (*s).into()).collect()),
        );
        table.remove("disabled_tools");
        table.insert("enabled".into(), true.into());
        table.insert("required".into(), true.into());
        let policies = table
            .entry("tools")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .context("invalid MCP tool policies")?;
        for tool in writes {
            let policy = policies
                .entry(*tool)
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .context("invalid MCP tool policy")?;
            policy.insert("approval_mode".into(), "approve".into());
        }
        Ok(server)
    }

    pub async fn validate_cli(&self) -> Result<()> {
        github::read_token(&self.config.agents.github_token_file)?;
        if self.config.agents.provider == Provider::Claude {
            return crate::claude::validate(&self.config).await;
        }
        let mut cmd = Command::new(&self.config.agents.codex);
        cmd.args(["exec", "--help"]);
        let help = process::checked(cmd, Duration::from_secs(15)).await?;
        for flag in [
            "--json",
            "--output-schema",
            "--ephemeral",
            "--ignore-user-config",
        ] {
            ensure!(help.contains(flag), "installed Codex lacks {flag}");
        }
        let mut cmd = Command::new(&self.config.agents.codex);
        cmd.args(["login", "status"])
            .env("CODEX_HOME", &self.config.agents.codex_home);
        process::checked(cmd, Duration::from_secs(15)).await?;
        for project in self.config.projects.iter().filter(|p| p.enabled) {
            self.mcp_config(project)?;
        }
        Ok(())
    }

    async fn invoke(
        &self,
        project: Option<&Project>,
        coordinator_projects: &[&Project],
        prompt: &str,
        schema: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        if self.config.agents.provider == Provider::Claude {
            return crate::claude::invoke(
                &self.config,
                project,
                coordinator_projects,
                prompt,
                schema,
                cancel,
            )
            .await;
        }
        let runtime = &self.config.orchestrator.runtime_dir;
        let log_dir = runtime.join("logs");
        std::fs::create_dir_all(&log_dir)?;
        // Schema/results are transient handoff files, never a recovery store.
        let temporary = tempfile::Builder::new()
            .prefix("invocation-")
            .tempdir_in(runtime)?;
        let output = temporary.path().join("result.json");
        let schema_path = temporary.path().join("schema.json");
        std::fs::write(&schema_path, serde_json::to_vec(&schema)?)?;
        let (role, model, cwd, timeout) = match project {
            Some(p) => (
                p.id.as_str(),
                self.config.agents.big_model(),
                p.repo.as_path(),
                self.config.orchestrator.worker_timeout_seconds,
            ),
            None => (
                "coordinator",
                self.config.agents.small_model(),
                temporary.path(),
                self.config.orchestrator.coordinator_timeout_seconds,
            ),
        };
        let log = log_dir.join(format!("{role}-{}.jsonl", uuid::Uuid::new_v4()));
        let mut cmd = Command::new(&self.config.agents.codex);
        cmd.current_dir(cwd)
            .env("CODEX_HOME", &self.config.agents.codex_home)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "--no-daemon",
                "-a",
                "never",
                "exec",
                "--ephemeral",
                "--json",
                "--color",
                "never",
                "--skip-git-repo-check",
                "-m",
                model,
                "-C",
            ])
            .arg(cwd)
            .arg("--output-schema")
            .arg(&schema_path)
            .arg("-o")
            .arg(&output);
        for setting in ["features.multi_agent=false", "features.hooks=false"] {
            cmd.args(["-c", setting]);
        }
        if project.is_some() {
            github::configure_worker(
                &mut cmd,
                &github::read_token(&self.config.agents.github_token_file)?,
            )?;
            // Inherit normal configuration, tools, PATH and credentials. No outer sandbox.
            cmd.args([
                "--sandbox",
                "danger-full-access",
                "-c",
                "shell_environment_policy.inherit=\"all\"",
                "-c",
                "shell_environment_policy.ignore_default_excludes=true",
                "-c",
                "shell_environment_policy.exclude=[]",
                "-c",
                "shell_environment_policy.include_only=[]",
            ]);
            // The coordinator owns all configured story sources, including other projects' servers.
            let mut servers = std::collections::BTreeSet::from([
                self.config.agents.mcp_server.as_str(),
                "shortcut",
            ]);
            servers.extend(
                self.config
                    .projects
                    .iter()
                    .map(|p| p.mcp_server(&self.config.agents)),
            );
            // An absent server must not become an incomplete entry containing only
            // enabled=false: Codex still deserializes its transport configuration.
            let user_config: toml::Value = toml::from_str(&std::fs::read_to_string(
                self.config.agents.codex_home.join("config.toml"),
            )?)?;
            servers.retain(|name| {
                user_config
                    .get("mcp_servers")
                    .and_then(|servers| servers.get(*name))
                    .is_some()
            });
            for server in servers {
                cmd.args(["-c", &format!("mcp_servers.{server}.enabled=false")]);
            }
        } else {
            cmd.env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .env_remove("MAISTRO_GITHUB_TOKEN");
            cmd.args(["--ignore-user-config", "--sandbox", "read-only"]);
            for setting in [
                "features.shell_tool=false",
                "features.unified_exec=false",
                "features.apps=false",
                "features.plugins=false",
                "web_search=\"disabled\"",
            ] {
                cmd.args(["-c", setting]);
            }
            let mut servers = std::collections::BTreeSet::new();
            for project in coordinator_projects {
                let server = project.mcp_server(&self.config.agents);
                if servers.insert(server) {
                    cmd.args([
                        "-c",
                        &format!(
                            "mcp_servers.{server}={}",
                            inline_toml(&self.mcp_config(project)?)?
                        ),
                    ]);
                }
            }
        }
        cmd.arg("-");
        tracing::info!(role, model, log=%log.display(), "agent starting");
        let result = process::run(
            cmd,
            prompt,
            Duration::from_secs(timeout),
            cancel,
            Some(&log),
        )
        .await
        .with_context(|| format!("{role}: agent failed; inspect {}", log.display()))?;
        ensure!(
            result.code == 0,
            "{role}: Codex exited {}; inspect {}",
            result.code,
            log.display()
        );
        let mut completed = false;
        for line in result.stdout.lines() {
            let event: Value = serde_json::from_str(line).context("invalid Codex JSON event")?;
            ensure!(
                event["type"] != "turn.failed" && event["type"] != "error",
                "{role}: Codex turn failed; inspect {}",
                log.display()
            );
            completed |= event["type"] == "turn.completed";
        }
        ensure!(completed, "{role}: Codex did not complete its turn");
        let bytes = std::fs::read(&output).context("agent did not produce a structured result")?;
        serde_json::from_slice(&bytes).context("invalid agent result")
    }

    pub async fn coordinate(
        &self,
        snapshot: &Snapshot,
        selected: &[String],
        cancel: &CancellationToken,
    ) -> Result<Coordination> {
        let projects: Vec<_> = self
            .config
            .projects
            .iter()
            .filter(|p| p.enabled && selected.contains(&p.id))
            .collect();
        let project_context: Vec<_> = projects
            .iter()
            .map(|p| {
                let mut value = serde_json::to_value(p).expect("serializable project");
                value["mcp_server"] = json!(p.mcp_server(&self.config.agents));
                value["source"] = json!(p.source_name());
                value
            })
            .collect();
        let prompt = format!(
            "{}\n\nPROJECTS={}\nSNAPSHOT={}\n",
            include_str!("../prompts/coordinator.md"),
            serde_json::to_string(&project_context)?,
            serde_json::to_string(snapshot)?
        );
        let result: Coordination = serde_json::from_value(
            self.invoke(
                None,
                &projects,
                &prompt,
                model::coordination_schema(),
                cancel,
            )
            .await?,
        )?;
        result.validate(
            &projects.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            snapshot,
        )?;
        Ok(result)
    }

    pub async fn work(
        &self,
        project: &Project,
        assignment: &Assignment,
        cancel: &CancellationToken,
    ) -> Result<Report> {
        let prompt = worker_prompt(project, assignment)?;
        let report: Report = serde_json::from_value(
            self.invoke(Some(project), &[], &prompt, model::report_schema(), cancel)
                .await?,
        )?;
        report.validate(assignment.effective_mode(project.mode))?;
        Ok(report)
    }
}

pub fn worker_prompt(project: &Project, assignment: &Assignment) -> Result<String> {
    let mode = assignment.effective_mode(project.mode);
    let branch = if mode == Mode::Pr {
        let branch = assignment
            .pr
            .as_ref()
            .and_then(|pr| pr.branch.clone())
            .unwrap_or_else(|| format!("maistro/{}/{}", project.id, assignment.story_id));
        ensure!(
            branch != project.base_branch && branch != "HEAD" && !branch.starts_with('-'),
            "PR story branch must be a branch name distinct from the base branch"
        );
        // Validate the literal ref, without --branch's @{-n} checkout expansion.
        let valid = std::process::Command::new("git")
            .args(["check-ref-format", &format!("refs/heads/{branch}")])
            .output()
            .context("validate PR story branch")?;
        ensure!(valid.status.success(), "invalid PR story branch name");
        Some(branch)
    } else {
        None
    };
    let custom = project
        .prompt_file
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()
        .context("read project prompt file")?
        .unwrap_or_default();
    let git = json!({
        "mode":mode, "base_branch":project.base_branch, "remote":project.remote,
        "story_branch": branch,
        "story_marker":format!("{} story: {}", project.source_name(), assignment.story_id)
    });
    Ok(format!(
        "{}\n\nPROJECT INSTRUCTIONS:\n{}\n\nREPOSITORY={}\nGIT={}\nASSIGNMENT={}\n",
        include_str!("../prompts/worker.md").replace(
            "{{GIT_MODE_INSTRUCTIONS}}",
            match mode {
                Mode::Pr => include_str!("../prompts/worker-pr.md"),
                Mode::Master => include_str!("../prompts/worker-master.md"),
            }
        ),
        custom,
        project.repo.display(),
        git,
        serde_json::to_string(assignment)?
    ))
}

fn inline_toml(value: &toml::Value) -> Result<String> {
    Ok(match value {
        toml::Value::Table(table) => format!(
            "{{{}}}",
            table
                .iter()
                .map(|(k, v)| Ok(format!("{}={}", serde_json::to_string(k)?, inline_toml(v)?)))
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        toml::Value::Array(array) => format!(
            "[{}]",
            array
                .iter()
                .map(inline_toml)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        toml::Value::String(s) => serde_json::to_string(s)?,
        other => other.to_string(),
    })
}

pub(crate) fn source_tools(
    project: &Project,
) -> (&'static [&'static str], &'static [&'static str]) {
    let reads: &[&str] = if project.is_shortcut() {
        &[
            "list_workflows",
            "get_workflow",
            "list_workflow_states",
            "search_stories",
            "get_story",
            "list_story_comments",
            "get_doc",
            "search_docs",
            "get_epic",
            "get_iteration",
            "read_file",
        ]
    } else {
        &[
            "get_workspaces",
            "get_workspace_folder",
            "list_databases",
            "get_database_fields",
            "get_database_row_ids",
            "get_database_row_details",
            "get_page",
            "get_page_markdown",
            "list_comments",
        ]
    };
    let writes: &[&str] = if project.is_shortcut() {
        &["update_story", "create_story_comment"]
    } else {
        &["update_row_cells", "append_comment"]
    };
    (reads, writes)
}
