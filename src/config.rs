use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub orchestrator: Orchestrator,
    #[serde(default)]
    pub agents: Agents,
    pub projects: Vec<Project>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Orchestrator {
    pub poll_interval_seconds: u64,
    pub worker_timeout_seconds: u64,
    pub coordinator_timeout_seconds: u64,
    pub runtime_dir: PathBuf,
}
impl Default for Orchestrator {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 30,
            worker_timeout_seconds: 3600,
            coordinator_timeout_seconds: 300,
            runtime_dir: "~/.local/state/maistro".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Agents {
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    pub mcp_server: String,
}
impl Default for Agents {
    fn default() -> Self {
        Self {
            codex: "codex".into(),
            codex_home: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~/.codex")),
            mcp_server: "appflowy".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Pr,
    Master,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub repo: PathBuf,
    pub appflowy_url: Option<String>,
    pub appflowy_page: Option<String>,
    pub mode: Mode,
    pub base_branch: String,
    #[serde(default = "origin")]
    pub remote: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub statuses: Statuses,
}
fn origin() -> String {
    "origin".into()
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Statuses {
    pub backlog: String,
    pub ready: String,
    pub in_progress: String,
    pub review: String,
    pub done: String,
    pub blocked: String,
    pub cancelled: String,
}
impl Default for Statuses {
    fn default() -> Self {
        Self {
            backlog: "Backlog".into(),
            ready: "Ready".into(),
            in_progress: "In Progress".into(),
            review: "Review".into(),
            done: "Done".into(),
            blocked: "Blocked".into(),
            cancelled: "Cancelled".into(),
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("open configuration file")?;
        let mut config: Self = toml::from_str(&std::fs::read_to_string(&path)?)
            .context("invalid configuration; legacy pipeline settings must be migrated (see config.example.toml)")?;
        let root = path.parent().unwrap();
        config.orchestrator.runtime_dir = resolve(&config.orchestrator.runtime_dir, root)?;
        config.agents.codex_home = resolve(&config.agents.codex_home, root)?;
        if config.agents.codex.components().count() > 1 {
            config.agents.codex = resolve(&config.agents.codex, root)?;
        }
        for p in &mut config.projects {
            p.repo = resolve(&p.repo, root)?
                .canonicalize()
                .with_context(|| format!("{}: repository does not exist", p.id))?;
            if let Some(path) = &mut p.prompt_file {
                *path = resolve(path, root)?;
                std::fs::read_to_string(&*path)
                    .with_context(|| format!("{}: read prompt_file", p.id))?;
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.projects.is_empty(), "configure at least one project");
        ensure!(
            self.orchestrator.poll_interval_seconds > 0
                && self.orchestrator.worker_timeout_seconds > 0
                && self.orchestrator.coordinator_timeout_seconds > 0,
            "timeouts and polling interval must be positive"
        );
        ensure!(
            identifier(&self.agents.mcp_server),
            "invalid MCP server name"
        );
        let mut ids = HashSet::new();
        let mut repos = HashSet::new();
        for p in &self.projects {
            ensure!(
                identifier(&p.id) && ids.insert(&p.id),
                "invalid or duplicate project ID: {}",
                p.id
            );
            ensure!(
                p.repo.is_absolute() && p.repo.is_dir(),
                "{}: repository must be an existing absolute directory",
                p.id
            );
            ensure!(
                repos.insert(&p.repo),
                "projects must have different repositories"
            );
            ensure!(
                p.appflowy_url.is_some() != p.appflowy_page.is_some(),
                "{}: specify exactly one appflowy_url or appflowy_page",
                p.id
            );
            ensure!(
                p.appflowy_url
                    .as_ref()
                    .or(p.appflowy_page.as_ref())
                    .is_some_and(|s| !s.trim().is_empty()),
                "empty board target"
            );
            ensure!(
                !p.base_branch.trim().is_empty()
                    && !p.base_branch.starts_with('-')
                    && !p.remote.trim().is_empty()
                    && !p.remote.starts_with('-'),
                "invalid base branch or remote"
            );
            let s = &p.statuses;
            let statuses = [
                &s.backlog,
                &s.ready,
                &s.in_progress,
                &s.review,
                &s.done,
                &s.blocked,
                &s.cancelled,
            ];
            ensure!(
                statuses.iter().all(|s| !s.trim().is_empty())
                    && statuses.into_iter().collect::<HashSet<_>>().len() == 7,
                "{}: status labels must be distinct and nonempty",
                p.id
            );
            ensure!(
                !self.orchestrator.runtime_dir.starts_with(&p.repo),
                "runtime_dir must be outside managed repositories"
            );
        }
        Ok(())
    }
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn resolve(path: &Path, root: &Path) -> Result<PathBuf> {
    if let Ok(rest) = path.strip_prefix("~") {
        return Ok(PathBuf::from(
            std::env::var_os("HOME").context("HOME is required for ~/ paths")?,
        )
        .join(rest));
    }
    Ok(if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    })
}
