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
    #[serde(default)]
    pub status: StatusServer,
    pub projects: Vec<Project>,
}

/// Deliberately no listen-address option: private access uses a localhost proxy.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusServer {
    pub enabled: bool,
    pub port: u16,
}
impl Default for StatusServer {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8044,
        }
    }
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
    pub provider: Provider,
    /// Model IDs/aliases understood by the selected CLI; omitted values use provider defaults.
    pub small_model: Option<String>,
    pub big_model: Option<String>,
    /// Explicit effort overrides; omission leaves CLI configuration/defaults intact.
    pub small_model_effort: Option<String>,
    pub big_model_effort: Option<String>,
    pub claude: PathBuf,
    /// JSON file containing user-scoped mcpServers (normally ~/.claude.json).
    pub claude_mcp_config: PathBuf,
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    pub mcp_server: String,
    pub github_token_file: PathBuf,
}
impl Default for Agents {
    fn default() -> Self {
        Self {
            provider: Provider::Codex,
            small_model: None,
            big_model: None,
            small_model_effort: None,
            big_model_effort: None,
            claude: "claude".into(),
            claude_mcp_config: "~/.claude.json".into(),
            codex: "codex".into(),
            codex_home: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~/.codex")),
            mcp_server: "appflowy".into(),
            github_token_file: "~/.config/maistro/github-token".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Codex,
    Claude,
}
impl Agents {
    pub fn validate_efforts(&self) -> Result<()> {
        for (name, effort) in [
            ("small_model_effort", &self.small_model_effort),
            ("big_model_effort", &self.big_model_effort),
        ] {
            self.provider
                .validate_effort(&format!("agents.{name}"), effort.as_deref())?;
        }
        Ok(())
    }

    pub fn small_model(&self) -> &str {
        self.small_model.as_deref().unwrap_or(match self.provider {
            Provider::Codex => "gpt-6-luna",
            Provider::Claude => "haiku",
        })
    }
    pub fn big_model(&self) -> &str {
        self.big_model.as_deref().unwrap_or(match self.provider {
            Provider::Codex => "gpt-6-astra",
            Provider::Claude => "opus",
        })
    }
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn validate_effort(self, field: &str, effort: Option<&str>) -> Result<()> {
        let supported: &[&str] = match self {
            Self::Codex => &[
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ],
            Self::Claude => &["low", "medium", "high", "xhigh", "max"],
        };
        if let Some(effort) = effort {
            ensure!(
                supported.contains(&effort),
                "{field}: unsupported effort {effort:?} for {}; supported values: {} (omit to inherit configuration/defaults)",
                self.name(),
                supported.join(", ")
            );
        }
        Ok(())
    }

    fn validate_model(self, field: &str, model: &str) -> Result<()> {
        ensure!(
            !model.trim().is_empty()
                && !model.starts_with('-')
                && !model.chars().any(char::is_control),
            "{field}: {} requires a nonempty model ID or alias, not a CLI option",
            self.name()
        );
        // Custom CLI aliases remain supported; reject recognizable IDs from the other provider.
        let incompatible = match self {
            Self::Codex => {
                model.starts_with("claude-")
                    || [
                        "haiku",
                        "sonnet",
                        "opus",
                        "sonnet[1m]",
                        "opus[1m]",
                        "opusplan",
                    ]
                    .contains(&model)
            }
            Self::Claude => model.starts_with("gpt-") || ["o1", "o3", "o4-mini"].contains(&model),
        };
        ensure!(
            !incompatible,
            "{field}: model {model:?} belongs to another provider; choose a {} model ID or alias, or omit to use defaults",
            self.name()
        );
        Ok(())
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
    /// Independent overrides of agents.big_model and agents.big_model_effort.
    pub worker_model: Option<String>,
    pub worker_model_effort: Option<String>,
    pub repo: PathBuf,
    pub appflowy_url: Option<String>,
    pub appflowy_page: Option<String>,
    /// Link identifying one Shortcut workflow in the configured MCP workspace.
    pub shortcut_url: Option<String>,
    /// Exact workflow name in the configured MCP workspace; must resolve uniquely.
    pub shortcut_workflow: Option<String>,
    /// Positive workflow ID in the configured MCP workspace.
    pub shortcut_workflow_id: Option<u64>,
    /// Optional read-only API polling; both fields must be supplied together.
    pub shortcut_workspace: Option<String>,
    #[serde(skip_serializing)]
    pub shortcut_token_file: Option<PathBuf>,
    /// Selected provider MCP server name; defaults to shortcut or agents.mcp_server.
    pub mcp_server: Option<String>,
    pub mode: Mode,
    pub base_branch: String,
    #[serde(default = "origin")]
    pub remote: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub statuses: Statuses,
}
impl Project {
    pub fn worker_model<'a>(&'a self, agents: &'a Agents) -> &'a str {
        self.worker_model
            .as_deref()
            .unwrap_or_else(|| agents.big_model())
    }
    pub fn worker_model_effort<'a>(&'a self, agents: &'a Agents) -> Option<&'a str> {
        self.worker_model_effort
            .as_deref()
            .or(agents.big_model_effort.as_deref())
    }
    pub fn validate_worker_settings(&self, agents: &Agents) -> Result<()> {
        if let Some(model) = &self.worker_model {
            agents
                .provider
                .validate_model(&format!("projects.{}.worker_model", self.id), model)?;
        }
        agents.provider.validate_effort(
            &format!("projects.{}.worker_model_effort", self.id),
            self.worker_model_effort.as_deref(),
        )
    }

    pub fn shortcut_api(&self) -> bool {
        self.shortcut_token_file.is_some()
    }
    pub fn is_shortcut(&self) -> bool {
        self.shortcut_url.is_some()
            || self.shortcut_workflow.is_some()
            || self.shortcut_workflow_id.is_some()
    }
    pub fn source_name(&self) -> &'static str {
        if self.is_shortcut() {
            "Shortcut"
        } else {
            "AppFlowy"
        }
    }
    pub fn mcp_server<'a>(&'a self, agents: &'a Agents) -> &'a str {
        self.mcp_server.as_deref().unwrap_or_else(|| {
            if self.is_shortcut() {
                "shortcut"
            } else {
                &agents.mcp_server
            }
        })
    }
    pub fn board_url(&self) -> Option<&str> {
        self.shortcut_url
            .as_deref()
            .or(self.appflowy_url.as_deref())
    }
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
    #[serde(with = "optional_status")]
    pub review: Option<String>,
    pub done: String,
    pub blocked: String,
    #[serde(with = "optional_status")]
    pub cancelled: Option<String>,
}
// Omission uses Statuses::default; only an explicit false disables a state.
mod optional_status {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Label(String),
            Disabled(bool),
        }
        match Value::deserialize(d)? {
            Value::Label(label) => Ok(Some(label)),
            Value::Disabled(false) => Ok(None),
            Value::Disabled(true) => {
                Err(serde::de::Error::custom("status must be a label or false"))
            }
        }
    }

    pub fn serialize<S: Serializer>(value: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(label) => label.serialize(s),
            None => false.serialize(s),
        }
    }
}
impl Default for Statuses {
    fn default() -> Self {
        Self {
            backlog: "Backlog".into(),
            ready: "Ready".into(),
            in_progress: "In Progress".into(),
            review: Some("Review".into()),
            done: "Done".into(),
            blocked: "Blocked".into(),
            cancelled: Some("Cancelled".into()),
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
        config.agents.claude_mcp_config = resolve(&config.agents.claude_mcp_config, root)?;
        config.agents.codex_home = resolve(&config.agents.codex_home, root)?;
        config.agents.github_token_file = resolve(&config.agents.github_token_file, root)?;
        if config.agents.codex.components().count() > 1 {
            config.agents.codex = resolve(&config.agents.codex, root)?;
        }
        if config.agents.claude.components().count() > 1 {
            config.agents.claude = resolve(&config.agents.claude, root)?;
        }
        for p in &mut config.projects {
            if let Some(path) = &p.shortcut_token_file {
                ensure!(
                    !path.as_os_str().is_empty(),
                    "{}: shortcut_token_file must not be empty",
                    p.id
                );
                p.shortcut_token_file = Some(resolve(path, root)?);
            }
            p.repo = resolve(&p.repo, root)?
                .canonicalize()
                .with_context(|| format!("{}: repository does not exist", p.id))?;
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        self.agents.validate_efforts()?;
        ensure!(
            self.status.port > 0,
            "status.port must be between 1 and 65535"
        );
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
        for (name, model) in [
            ("small_model", self.agents.small_model()),
            ("big_model", self.agents.big_model()),
        ] {
            self.agents
                .provider
                .validate_model(&format!("agents.{name}"), model)?;
        }
        let executable = match self.agents.provider {
            Provider::Codex => &self.agents.codex,
            Provider::Claude => &self.agents.claude,
        };
        ensure!(
            !executable.as_os_str().is_empty(),
            "agent executable must not be empty"
        );
        let mut ids = HashSet::new();
        let mut repos = HashSet::new();
        let mut servers = std::collections::HashMap::new();
        for p in &self.projects {
            p.validate_worker_settings(&self.agents)?;
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
                [
                    p.appflowy_url.is_some(),
                    p.appflowy_page.is_some(),
                    p.shortcut_url.is_some(),
                    p.shortcut_workflow.is_some(),
                    p.shortcut_workflow_id.is_some(),
                ]
                .into_iter()
                .filter(|present| *present)
                .count()
                    == 1,
                "{}: specify exactly one appflowy_url, appflowy_page, shortcut_url, shortcut_workflow or shortcut_workflow_id",
                p.id
            );
            ensure!(
                p.appflowy_url
                    .as_ref()
                    .or(p.appflowy_page.as_ref())
                    .or(p.shortcut_url.as_ref())
                    .or(p.shortcut_workflow.as_ref())
                    .is_none_or(|s| !s.trim().is_empty()),
                "empty board target"
            );
            ensure!(
                p.shortcut_workflow_id.is_none_or(|id| id > 0),
                "{}: shortcut_workflow_id must be a positive integer",
                p.id
            );
            ensure!(
                !p.base_branch.trim().is_empty()
                    && !p.base_branch.starts_with('-')
                    && !p.remote.trim().is_empty()
                    && !p.remote.starts_with('-'),
                "invalid base branch or remote"
            );
            ensure!(
                p.shortcut_workspace.is_some() == p.shortcut_token_file.is_some(),
                "{}: shortcut_workspace and shortcut_token_file must be configured together",
                p.id
            );
            if let Some(workspace) = &p.shortcut_workspace {
                ensure!(
                    p.is_shortcut(),
                    "{}: Shortcut API settings require a Shortcut workflow",
                    p.id
                );
                ensure!(
                    !workspace.is_empty()
                        && workspace
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "{}: invalid shortcut_workspace slug",
                    p.id
                );
                ensure!(
                    p.shortcut_token_file
                        .as_ref()
                        .is_some_and(|p| !p.as_os_str().is_empty()),
                    "{}: shortcut_token_file must not be empty",
                    p.id
                );
                if let Some(url) = &p.shortcut_url {
                    let url = reqwest::Url::parse(url).context("invalid shortcut_url")?;
                    ensure!(
                        url.path_segments().and_then(|mut s| s.next()) == Some(workspace),
                        "{}: shortcut_url workspace does not match shortcut_workspace",
                        p.id
                    );
                }
            }
            let server = p.mcp_server(&self.agents);
            ensure!(identifier(server), "{}: invalid MCP server name", p.id);
            if let Some(previous) = servers.insert(server, p.is_shortcut()) {
                ensure!(
                    previous == p.is_shortcut(),
                    "MCP server {server} cannot serve both AppFlowy and Shortcut"
                );
            }
            if let Some(url) = &p.shortcut_url {
                let url = reqwest::Url::parse(url).context("invalid shortcut_url")?;
                ensure!(
                    url.scheme() == "https"
                        && url.host_str() == Some("app.shortcut.com")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none()
                        && url
                            .path_segments()
                            .is_some_and(|s| s.filter(|s| !s.is_empty()).count() >= 2),
                    "{}: shortcut_url must be an https://app.shortcut.com/<workspace>/... workflow link",
                    p.id
                );
            }
            let s = &p.statuses;
            let statuses: Vec<&String> =
                [&s.backlog, &s.ready, &s.in_progress, &s.done, &s.blocked]
                    .into_iter()
                    .chain(s.review.iter())
                    .chain(s.cancelled.iter())
                    .collect();
            ensure!(
                p.mode != Mode::Pr || s.review.is_some(),
                "{}: review cannot be disabled in PR mode",
                p.id
            );
            ensure!(
                statuses.iter().all(|s| !s.trim().is_empty())
                    && statuses.iter().copied().collect::<HashSet<_>>().len() == statuses.len(),
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
