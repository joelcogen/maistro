//! Claude Code print-mode adapter. Authentication stays with the user's CLI.
use crate::{
    agent::source_tools,
    config::{Config, Project},
    github, process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

fn servers(config: &Config) -> Result<Map<String, Value>> {
    let bytes = std::fs::read(&config.agents.claude_mcp_config)
        .context("read agents.claude_mcp_config (user-scoped Claude MCP JSON)")?;
    let value: Value = serde_json::from_slice(&bytes).context("invalid Claude MCP JSON")?;
    value["mcpServers"]
        .as_object()
        .cloned()
        .context("Claude MCP configuration needs a top-level mcpServers object")
}

fn server(config: &Config, project: &Project) -> Result<Value> {
    let name = project.mcp_server(&config.agents);
    let value = servers(config)?.remove(name).with_context(|| {
        format!(
            "{}: configured Claude MCP server {name} missing",
            project.id
        )
    })?;
    ensure!(value.is_object(), "invalid Claude MCP server {name}");
    Ok(value)
}

/// Adapt Claude's transport keys for the existing read-only Rust poller.
/// Resolve the same environment syntax as Claude before passing it to Rust.
pub(crate) fn poll_config(config: &Config, project: &Project) -> Result<toml::Value> {
    let mut value = server(config, project)?;
    expand_env(&mut value)?;
    let object = value.as_object_mut().unwrap();
    match object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("stdio")
    {
        "stdio" => ensure!(
            object.get("command").and_then(Value::as_str).is_some(),
            "Claude stdio MCP requires command"
        ),
        "http" | "streamable-http" => ensure!(
            object.get("url").and_then(Value::as_str).is_some(),
            "Claude HTTP MCP requires url"
        ),
        _ => anyhow::bail!(
            "AppFlowy polling supports only stdio or streamable HTTP Claude MCP servers"
        ),
    }
    object.remove("type");
    if let Some(headers) = object.remove("headers") {
        object.insert("http_headers".into(), headers);
    }
    toml::Value::try_from(value).context("invalid Claude MCP transport configuration")
}

fn expand_env(value: &mut Value) -> Result<()> {
    match value {
        Value::String(s) => {
            let mut expanded = String::new();
            let mut rest = s.as_str();
            while let Some((before, after)) = rest.split_once("${") {
                expanded.push_str(before);
                let (expression, tail) = after
                    .split_once('}')
                    .context("unterminated Claude MCP environment expansion")?;
                let (name, default) = expression
                    .split_once(":-")
                    .map_or((expression, None), |(n, d)| (n, Some(d)));
                let replacement = std::env::var(name)
                    .ok()
                    .or_else(|| default.map(str::to_owned))
                    .context("Claude MCP environment variable missing")?;
                expanded.push_str(&replacement);
                rest = tail;
            }
            expanded.push_str(rest);
            *s = expanded;
        }
        Value::Array(values) => {
            for v in values {
                expand_env(v)?;
            }
        }
        Value::Object(values) => {
            for v in values.values_mut() {
                expand_env(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) async fn validate(config: &Config) -> Result<()> {
    let mut cmd = Command::new(&config.agents.claude);
    cmd.arg("--help");
    let help = process::checked(cmd, Duration::from_secs(15)).await?;
    if config.agents.small_model_effort.is_some() || config.agents.big_model_effort.is_some() {
        ensure!(
            help.contains("--effort"),
            "installed Claude Code lacks --effort; upgrade it or omit agents.small_model_effort and agents.big_model_effort"
        );
    }
    for flag in [
        "--print",
        "--output-format",
        "--json-schema",
        "--model",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--mcp-config",
        "--tools",
        "--allowedTools",
        "--disallowedTools",
        "--setting-sources",
        "--settings",
        "--permission-mode",
        "--dangerously-skip-permissions",
    ] {
        ensure!(help.contains(flag), "installed Claude Code lacks {flag}");
    }
    let mut cmd = Command::new(&config.agents.claude);
    cmd.args(["auth", "status"]);
    let status = process::checked(cmd, Duration::from_secs(15)).await?;
    let status: Value =
        serde_json::from_str(&status).context("invalid Claude authentication status")?;
    ensure!(
        status["loggedIn"] == true,
        "Claude Code is not logged in; authenticate before starting Maistro"
    );
    for project in config.projects.iter().filter(|p| p.enabled) {
        if project.is_shortcut() {
            server(config, project)?;
        } else {
            poll_config(config, project)?;
        }
    }
    Ok(())
}

pub(crate) async fn invoke(
    config: &Config,
    project: Option<&Project>,
    coordinator_projects: &[&Project],
    prompt: &str,
    schema: Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let runtime = &config.orchestrator.runtime_dir;
    let logs = runtime.join("logs");
    std::fs::create_dir_all(&logs)?;
    let temporary = tempfile::Builder::new()
        .prefix("invocation-")
        .tempdir_in(runtime)?;
    let (role, model, effort, cwd, timeout) = match project {
        Some(p) => (
            p.id.as_str(),
            config.agents.big_model(),
            config.agents.big_model_effort.as_deref(),
            p.repo.as_path(),
            config.orchestrator.worker_timeout_seconds,
        ),
        None => (
            "coordinator",
            config.agents.small_model(),
            config.agents.small_model_effort.as_deref(),
            temporary.path(),
            config.orchestrator.coordinator_timeout_seconds,
        ),
    };
    let log = logs.join(format!("{role}-{}.jsonl", uuid::Uuid::new_v4()));
    let mut cmd = Command::new(&config.agents.claude);
    cmd.current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--model",
            model,
            "--json-schema",
        ])
        .arg(serde_json::to_string(&schema)?)
        .args([
            "--strict-mcp-config",
            "--settings",
            r#"{"disableAllHooks":true}"#,
            "--disallowedTools",
            "Agent,Task",
        ]);
    if let Some(effort) = effort {
        // Claude's environment override outranks --effort. Only remove it when
        // Maistro explicitly configures this role; otherwise preserve inheritance.
        cmd.env_remove("CLAUDE_CODE_EFFORT_LEVEL")
            .args(["--effort", effort]);
    }
    let mut connections = Map::new();
    if project.is_some() {
        github::configure_worker(
            &mut cmd,
            &github::read_token(&config.agents.github_token_file)?,
        )?;
        cmd.arg("--dangerously-skip-permissions");
        // Keep other user-scoped MCP tools, but never expose any story sources.
        connections = servers(config)?;
        connections.remove(&config.agents.mcp_server);
        connections.remove("shortcut");
        for p in &config.projects {
            connections.remove(p.mcp_server(&config.agents));
        }
    } else {
        cmd.env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("MAISTRO_GITHUB_TOKEN");
        cmd.args([
            "--setting-sources",
            "",
            "--tools",
            "",
            "--permission-mode",
            "dontAsk",
        ]);
        let mut allowed = vec![];
        for p in coordinator_projects {
            let name = p.mcp_server(&config.agents);
            connections.insert(name.into(), server(config, p)?);
            let (reads, writes) = source_tools(p);
            allowed.extend(
                reads
                    .iter()
                    .chain(writes)
                    .map(|tool| format!("mcp__{name}__{tool}")),
            );
        }
        cmd.arg("--allowedTools").arg(allowed.join(","));
    }
    // Credentials in MCP entries must never appear in process arguments.
    let mcp_path = temporary.path().join("mcp.json");
    std::fs::write(
        &mcp_path,
        serde_json::to_vec(&json!({"mcpServers": connections}))?,
    )?;
    cmd.arg("--mcp-config").arg(&mcp_path);
    tracing::info!(role, model, configured_effort=effort.unwrap_or("inherited/default"), provider="claude", log=%log.display(), "agent starting");
    let result = process::run(
        cmd,
        prompt,
        Duration::from_secs(timeout),
        cancel,
        Some(&log),
    )
    .await
    .with_context(|| format!("{role}: Claude agent failed; inspect {}", log.display()))?;
    ensure!(
        result.code == 0,
        "{role}: Claude exited {}; inspect {}",
        result.code,
        log.display()
    );
    parse_result(&result.stdout)
        .with_context(|| format!("{role}: invalid Claude result; inspect {}", log.display()))
}

fn parse_result(stdout: &str) -> Result<Value> {
    let mut output = None;
    for line in stdout.lines() {
        let event: Value = serde_json::from_str(line).context("invalid Claude JSON event")?;
        ensure!(event["type"] != "error", "Claude reported an error");
        if event["type"] == "result" {
            ensure!(output.is_none(), "Claude returned multiple results");
            ensure!(
                event["subtype"] == "success" && event["is_error"] == false,
                "Claude turn failed"
            );
            let value = event
                .get("structured_output")
                .filter(|v| v.is_object())
                .context("Claude did not produce a structured result")?;
            output = Some(value.clone());
        }
    }
    output.context("Claude did not complete its turn")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_result_requires_successful_complete_turn() {
        let good = json!({"type":"result", "subtype":"success", "is_error":false, "structured_output":{"outcome":"blocked"}});
        assert_eq!(
            parse_result(&format!("{{\"type\":\"system\"}}\n{good}\n")).unwrap()["outcome"],
            "blocked"
        );
        for bad in ["".into(), "not json".into(), json!({"type":"result","subtype":"success","is_error":false}).to_string(), json!({"type":"result","subtype":"error_max_turns","is_error":true,"structured_output":{}}).to_string(), format!("{good}\n{good}"), format!("{good}\n{{\"type\":\"error\"}}") ] {
            assert!(parse_result(&bad).is_err(), "accepted {bad}");
        }
    }
    #[test]
    fn mcp_expansion_handles_defaults_and_missing_variables_without_leaking_values() {
        let mut value = json!({"args":["${MAISTRO_TEST_UNSET_819:---flag}", "x${MAISTRO_TEST_UNSET_819:-value}y"]});
        expand_env(&mut value).unwrap();
        assert_eq!(value, json!({"args":["--flag", "xvaluey"]}));
        assert!(expand_env(&mut json!("${MAISTRO_TEST_UNSET_819}")).is_err());
        assert!(expand_env(&mut json!("${broken")).is_err());
    }
}
