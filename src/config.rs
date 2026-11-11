use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub orchestrator: Orchestrator,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub github: Github,
    pub projects: Vec<Project>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Orchestrator {
    pub poll_interval_seconds: u64,
    pub max_concurrent_workers: usize,
    pub state_db: PathBuf,
    pub worktree_root: PathBuf,
    pub log_root: PathBuf,
    pub max_attempts: u32,
    pub process_timeout_seconds: u64,
    pub ci_timeout_seconds: u64,
    pub retain_worktrees: bool,
}
impl Default for Orchestrator {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 60,
            max_concurrent_workers: 2,
            state_db: "~/.local/state/maistro/state.db".into(),
            worktree_root: "~/.local/state/maistro/worktrees".into(),
            log_root: "~/.local/state/maistro/logs".into(),
            max_attempts: 3,
            process_timeout_seconds: 3600,
            ci_timeout_seconds: 7200,
            retain_worktrees: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Agents {
    pub coordinator_model: String,
    pub developer_model: String,
    pub reviewer_model: String,
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    pub mcp_server: String,
    pub coordinator_timeout_seconds: u64,
}
impl Default for Agents {
    fn default() -> Self {
        Self {
            coordinator_model: "gpt-6-luna".into(),
            developer_model: "gpt-6-astra".into(),
            reviewer_model: "gpt-6-astra".into(),
            codex: "codex".into(),
            codex_home: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
                }),
            mcp_server: "appflowy".into(),
            coordinator_timeout_seconds: 300,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Github {
    pub auto_merge: bool,
    pub merge_method: String,
    pub token_env: String,
}
impl Default for Github {
    fn default() -> Self {
        Self {
            auto_merge: false,
            merge_method: "squash".into(),
            token_env: "MAISTRO_GITHUB_TOKEN".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo: PathBuf,
    pub appflowy_url: Option<String>,
    pub appflowy_page: Option<String>,
    #[serde(default = "main_branch")]
    pub base_branch: String,
    #[serde(default = "one")]
    pub max_workers: usize,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub github_repo: String,
    #[serde(default)]
    pub required_checks: Vec<String>,
    #[serde(default)]
    pub test_commands: Vec<Vec<String>>,
    #[serde(default)]
    pub statuses: Statuses,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub toolchain_paths: Vec<PathBuf>,
}
fn main_branch() -> String {
    "main".into()
}
fn one() -> usize {
    1
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        let path = path
            .canonicalize()
            .context("configuration file not found")?;
        let mut c: Self = toml::from_str(&std::fs::read_to_string(&path)?)
            .context("invalid TOML configuration")?;
        let root = path.parent().unwrap();
        c.resolve_runtime_paths(root)?;
        c.validate()?;
        Ok(c)
    }
    pub fn resolve_runtime_paths(&mut self, config_root: &Path) -> Result<()> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for p in [
            &mut self.orchestrator.state_db,
            &mut self.orchestrator.worktree_root,
            &mut self.orchestrator.log_root,
        ] {
            *p = resolve_runtime_path(p, config_root, home.as_deref())?;
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.projects.is_empty(), "configure at least one project");
        ensure!(
            self.orchestrator.max_concurrent_workers > 0
                && self.orchestrator.poll_interval_seconds > 0
                && self.orchestrator.max_attempts > 0
                && self.orchestrator.process_timeout_seconds > 0
                && self.orchestrator.ci_timeout_seconds > 0,
            "limits must be positive"
        );
        ensure!(
            self.github.merge_method == "squash",
            "only squash merging is supported"
        );
        ensure!(!self.github.token_env.is_empty(), "token_env is required");
        for m in [
            &self.agents.coordinator_model,
            &self.agents.developer_model,
            &self.agents.reviewer_model,
        ] {
            ensure!(!m.trim().is_empty(), "model identifier is empty");
        }
        let mut ids = HashSet::new();
        let mut repos = HashSet::new();
        let mut github_repos = HashSet::new();
        for p in &self.projects {
            ensure!(
                valid_id(&p.id) && ids.insert(&p.id),
                "invalid or duplicate project ID: {}",
                p.id
            );
            ensure!(
                p.repo.is_absolute() && p.repo.is_dir(),
                "{}: repo must be an existing absolute directory",
                p.id
            );
            let repo = p.repo.canonicalize()?;
            ensure!(
                repos.insert(repo.clone()),
                "multiple projects map to the same repository"
            );
            ensure!(
                !self.orchestrator.worktree_root.starts_with(&repo)
                    && !self.orchestrator.state_db.starts_with(&repo)
                    && !self.orchestrator.log_root.starts_with(&repo),
                "state and worktree roots must be outside managed repositories"
            );
            ensure!(p.max_workers == 1, "MVP supports one worker per repository");
            match (&p.appflowy_url, &p.appflowy_page) {
                (Some(u), None) => {
                    let u = reqwest::Url::parse(u)?;
                    ensure!(
                        u.scheme() == "https" && u.username().is_empty() && u.password().is_none(),
                        "board URL must be HTTPS without credentials"
                    );
                }
                (None, Some(n)) if !n.trim().is_empty() => {}
                _ => bail!("{}: specify exactly one AppFlowy URL or page name", p.id),
            }
            ensure!(
                github_repos.insert(p.github_repo.to_ascii_lowercase()),
                "projects must use distinct GitHub repositories"
            );
            ensure!(
                p.github_repo.split('/').count() == 2 && p.github_repo.split('/').all(valid_id),
                "github_repo must be owner/repository"
            );
            ensure!(
                !p.base_branch.starts_with('-') && !p.base_branch.is_empty(),
                "invalid base branch"
            );
            ensure!(
                p.test_commands
                    .iter()
                    .all(|v| !v.is_empty() && !v[0].is_empty()),
                "empty test command"
            );
            ensure!(
                p.allowed_paths
                    .iter()
                    .chain(&p.protected_paths)
                    .all(|s| !s.is_empty()
                        && !s.starts_with('/')
                        && !s.split('/').any(|x| x == "..")),
                "policy paths must be relative prefixes"
            );
            ensure!(
                p.toolchain_paths
                    .iter()
                    .all(|p| p.is_absolute() && p.exists()),
                "toolchain paths must exist and be absolute"
            );
            let s = &p.statuses;
            let labels = [
                &s.backlog,
                &s.ready,
                &s.in_progress,
                &s.review,
                &s.done,
                &s.blocked,
                &s.cancelled,
            ];
            ensure!(
                labels.iter().all(|s| !s.trim().is_empty())
                    && labels.into_iter().collect::<HashSet<_>>().len() == 7,
                "status labels must be nonempty and distinct"
            );
        }
        Ok(())
    }
}
fn resolve_runtime_path(path: &Path, config_root: &Path, home: Option<&Path>) -> Result<PathBuf> {
    if let Ok(relative) = path.strip_prefix("~") {
        let home = home
            .filter(|h| h.is_absolute())
            .context("HOME must be an absolute path to resolve ~/ runtime paths")?;
        Ok(home.join(relative))
    } else if path.is_relative() {
        Ok(config_root.join(path))
    } else {
        Ok(path.to_path_buf())
    }
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('-')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        && s != "."
        && s != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_path_resolution_preserves_overrides_and_rejects_missing_home() {
        let root = Path::new("/configuration");
        let path = Path::new("~/.local/state/maistro/state.db");
        assert_eq!(
            resolve_runtime_path(path, root, Some(Path::new("/home/alice"))).unwrap(),
            Path::new("/home/alice/.local/state/maistro/state.db")
        );
        assert!(resolve_runtime_path(path, root, None).is_err());
        assert!(resolve_runtime_path(path, root, Some(Path::new("relative"))).is_err());
        assert_eq!(
            resolve_runtime_path(Path::new("runtime/logs"), root, None).unwrap(),
            Path::new("/configuration/runtime/logs")
        );
        assert_eq!(
            resolve_runtime_path(Path::new("/custom/logs"), root, None).unwrap(),
            Path::new("/custom/logs")
        );
    }
}
