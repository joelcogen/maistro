//! Stdio MCP policy boundary for committed projections. Luna still calls the configured MCP.
use crate::{
    config::Agents,
    config::Statuses,
    model::{Projection, Scan, Status},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
#[derive(Serialize, Deserialize)]
pub struct Scope {
    pub agents: Agents,
    pub scan: Scan,
    pub projection: Projection,
    pub statuses: Statuses,
    pub receipt: PathBuf,
    pub state_db: PathBuf,
}
#[derive(Default)]
pub struct Guard {
    fields: HashMap<String, Value>,
    comment_present: bool,
    status_fields: std::collections::HashSet<String>,
}
impl Guard {
    fn collect(&mut self, v: &Value, s: &Scope) {
        match v {
            Value::Object(m) => {
                if let (Some(id), Some(name)) = (
                    m.get("id").and_then(Value::as_str),
                    m.get("name").and_then(Value::as_str),
                ) {
                    let name = name.to_lowercase();
                    let expected = match name.as_str() {
                        "execution id" | "execution_id" => Some(json!(s.projection.run_id)),
                        "pr url" | "pr_url" => {
                            Some(json!(s.projection.pr_url.as_deref().unwrap_or("")))
                        }
                        "last error" | "last_error" => Some(json!(s.projection.message)),
                        "retry count" | "retry_count" => {
                            Some(json!(s.projection.attempt.to_string()))
                        }
                        "status" => {
                            let label = match s.projection.status {
                                Status::Backlog => &s.statuses.backlog,
                                Status::Ready => &s.statuses.ready,
                                Status::InProgress => &s.statuses.in_progress,
                                Status::Review => &s.statuses.review,
                                Status::Done => &s.statuses.done,
                                Status::Blocked => &s.statuses.blocked,
                                Status::Cancelled => &s.statuses.cancelled,
                            };
                            find_option(v, label).map(Value::String)
                        }
                        _ => None,
                    };
                    if let Some(value) = expected {
                        if name == "status" {
                            self.status_fields.insert(id.to_string());
                        }
                        self.fields.insert(id.to_string(), value);
                    }
                }
                for v in m.values() {
                    self.collect(v, s);
                }
            }
            Value::Array(a) => {
                for v in a {
                    self.collect(v, s);
                }
            }
            Value::String(text) => {
                if let Ok(v) = serde_json::from_str::<Value>(text) {
                    self.collect(&v, s);
                }
            }
            _ => {}
        }
    }
    pub fn observe(&mut self, tool: &str, result: &Value, s: &Scope) {
        if tool == "get_database_fields" {
            self.collect(result, s);
        }
        if tool == "list_comments" {
            self.comment_present |= contains_comment(result, &s.projection.message);
        }
    }
    pub fn allow(&self, name: &str, a: &Value, s: &Scope) -> Result<()> {
        ensure!(
            s.projection.status != Status::Cancelled,
            "Cancelled is user-owned; Maistro cannot project it"
        );
        ensure!(
            a["workspace_id"].as_str() == Some(&s.scan.workspace_id),
            "projection workspace mismatch"
        );
        match name {
            "update_row_cells" => {
                ensure!(
                    a["database_id"].as_str() == Some(&s.scan.board_id)
                        && a["row_id"].as_str() == Some(&s.projection.story_id),
                    "projection row/database mismatch"
                );
                let cells: Value = serde_json::from_str(
                    a["cells"].as_str().context("cells must be JSON string")?,
                )?;
                let cells = cells.as_object().context("cells must be object")?;
                ensure!(!cells.is_empty(), "empty projection");
                for (k, v) in cells {
                    ensure!(
                        self.fields.get(k) == Some(v),
                        "field/value outside committed projection: field {k}, expected {}",
                        self.fields
                            .get(k)
                            .map(Value::to_string)
                            .unwrap_or_else(|| "no permitted value".into())
                    );
                }
            }
            "append_comment" => {
                ensure!(
                    a["target_type"] == "row"
                        && a["target_id"].as_str() == Some(&s.projection.story_id)
                        && a["content"].as_str() == Some(&s.projection.message),
                    "comment outside committed projection"
                );
                for key in ["block_id", "reply_comment_id"] {
                    ensure!(
                        a.get(key).is_none_or(|v| v.is_null() || v == ""),
                        "projection cannot attach/reply elsewhere"
                    );
                }
                ensure!(
                    a.get("database_view_id")
                        .is_none_or(|v| v == "" || v.as_str() == Some(&s.scan.view_id)),
                    "wrong board view"
                );
            }
            _ => anyhow::bail!("write tool not permitted"),
        }
        Ok(())
    }
}
fn contains_comment(v: &Value, message: &str) -> bool {
    match v {
        Value::String(s) => {
            s == message
                || serde_json::from_str::<Value>(s)
                    .ok()
                    .is_some_and(|v| contains_comment(&v, message))
        }
        Value::Array(a) => a.iter().any(|v| contains_comment(v, message)),
        Value::Object(m) => m.values().any(|v| contains_comment(v, message)),
        _ => false,
    }
}
fn find_option(v: &Value, label: &str) -> Option<String> {
    match v {
        Value::Object(m) => {
            if m.get("name").and_then(Value::as_str) == Some(label) {
                return m.get("id").and_then(Value::as_str).map(str::to_owned);
            }
            m.values().find_map(|v| find_option(v, label))
        }
        Value::Array(a) => a.iter().find_map(|v| find_option(v, label)),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| find_option(&v, label)),
        _ => None,
    }
}
pub(crate) async fn send(w: &mut (impl tokio::io::AsyncWrite + Unpin), v: &Value) -> Result<()> {
    w.write_all(v.to_string().as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
pub(crate) fn server_command(agents: &Agents) -> Result<Command> {
    let cfg: toml::Value = toml::from_str(&std::fs::read_to_string(
        agents.codex_home.join("config.toml"),
    )?)?;
    let server = cfg
        .get("mcp_servers")
        .and_then(|v| v.get(&agents.mcp_server))
        .context("configured AppFlowy MCP server missing")?;
    let executable = server
        .get("command")
        .and_then(toml::Value::as_str)
        .context("guard currently requires the configured stdio MCP transport")?;
    let mut c = Command::new(executable);
    if let Some(a) = server.get("args").and_then(toml::Value::as_array) {
        for a in a {
            c.arg(a.as_str().context("invalid MCP argument")?);
        }
    }
    if let Some(env) = server.get("env").and_then(toml::Value::as_table) {
        for (k, v) in env {
            c.env(k, v.as_str().context("invalid MCP environment")?);
        }
    }
    if let Some(cwd) = server.get("cwd").and_then(toml::Value::as_str) {
        c.current_dir(cwd);
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    Ok(c)
}
pub async fn serve(path: &Path) -> Result<()> {
    let scope: Scope = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut child = server_command(&scope.agents)?.spawn()?;
    let mut target = child.stdin.take().unwrap();
    let mut server_lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut client_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    let mut guard = Guard::default();
    let mut pending: HashMap<String, (String, Value)> = HashMap::new();
    loop {
        tokio::select! {
            line=client_lines.next_line()=>{
                let Some(line)=line? else {break;};
                ensure!(line.len()<=16*1024*1024,"oversized MCP request");
                let v:Value=serde_json::from_str(&line)?;
                if let Some(response)=inspect_request(&v,&scope,&guard,&mut pending)? {send(&mut output,&response).await?;}else{send(&mut target,&v).await?;}
            },
            line=server_lines.next_line()=>{
                let Some(line)=line? else {break;};
                ensure!(line.len()<=16*1024*1024,"oversized MCP response");
                let v:Value=serde_json::from_str(&line)?;
                observe_response(&v,&scope,&mut guard,&mut pending)?;
                send(&mut output,&v).await?;
            }
        }
    }
    child.kill().await.ok();
    child.wait().await.ok();
    Ok(())
}
fn live_scope(s: &Scope) -> Result<()> {
    let store = crate::store::Store::open(&s.state_db)?;
    let run = store.get(&s.projection.run_id)?;
    ensure!(
        run.project_id == s.projection.project_id && run.story.story_id == s.projection.story_id,
        "projection identity mismatch"
    );
    let status = match run.state {
        crate::store::State::Queued => Status::Ready,
        crate::store::State::Running => Status::InProgress,
        crate::store::State::Reviewing
        | crate::store::State::WaitingCi
        | crate::store::State::Merging => Status::Review,
        crate::store::State::Completed => Status::Done,
        crate::store::State::Blocked
            if run
                .last_error
                .as_deref()
                .is_some_and(|s| s.starts_with("Dependency ")) =>
        {
            Status::Ready
        }
        crate::store::State::Blocked | crate::store::State::Failed => Status::Blocked,
        crate::store::State::Cancelled => anyhow::bail!("cancelled runs cannot change the board"),
    };
    ensure!(
        status == s.projection.status,
        "projection superseded by a newer state"
    );
    ensure!(
        !store.runs()?.iter().any(|r| r.project_id == run.project_id
            && r.story.story_id == run.story.story_id
            && r.id != run.id
            && r.state.active()),
        "newer run owns story projection"
    );
    Ok(())
}
fn inspect_request(
    v: &Value,
    s: &Scope,
    g: &Guard,
    pending: &mut HashMap<String, (String, Value)>,
) -> Result<Option<Value>> {
    if v["method"] == "tools/call" {
        let name = v["params"]["name"].as_str().unwrap_or("");
        let args = &v["params"]["arguments"];
        if !crate::codex::READ_TOOLS.contains(&name)
            && let Err(e) = live_scope(s).and_then(|_| g.allow(name, args, s))
        {
            return Ok(Some(
                json!({"jsonrpc":"2.0","id":v["id"],"result":{"isError":true,"content":[{"type":"text","text":e.to_string()}]}}),
            ));
        }
        if name == "append_comment" && g.comment_present {
            return Ok(Some(
                json!({"jsonrpc":"2.0","id":v["id"],"result":{"content":[{"type":"text","text":"Exact projection comment already present; no duplicate appended"}]}}),
            ));
        }
        pending.insert(v["id"].to_string(), (name.into(), args.clone()));
    } else if let Some(method) = v["method"].as_str() {
        ensure!(
            matches!(
                method,
                "initialize"
                    | "notifications/initialized"
                    | "notifications/cancelled"
                    | "tools/list"
                    | "resources/list"
                    | "resources/templates/list"
                    | "resources/read"
                    | "prompts/list"
                    | "ping"
            ),
            "unsupported MCP method"
        );
    }
    Ok(None)
}
fn observe_response(
    v: &Value,
    s: &Scope,
    g: &mut Guard,
    pending: &mut HashMap<String, (String, Value)>,
) -> Result<()> {
    if let Some((tool, args)) = pending.remove(&v["id"].to_string())
        && v.get("error").is_none()
        && v["result"]["isError"] != true
    {
        if (tool == "get_database_fields"
            && args["workspace_id"].as_str() == Some(&s.scan.workspace_id)
            && args["database_id"].as_str() == Some(&s.scan.board_id))
            || (tool == "list_comments"
                && args["target_id"].as_str() == Some(&s.projection.story_id))
        {
            g.observe(&tool, &v["result"], s);
        }
        if tool == "append_comment" {
            g.comment_present = true;
        }
        if tool == "update_row_cells" || g.comment_present {
            use std::io::Write;
            let status_updated = tool == "update_row_cells"
                && g.status_fields.len() == 1
                && serde_json::from_str::<Value>(args["cells"].as_str().unwrap_or("{}"))
                    .ok()
                    .is_some_and(|v| {
                        g.status_fields
                            .iter()
                            .any(|key| v.get(key) == g.fields.get(key))
                    });
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&s.receipt)?;
            writeln!(
                f,
                "{}",
                json!({"tool":tool,"status_updated":status_updated,"comment_present":g.comment_present})
            )?;
            f.sync_all()?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_row_and_requirement_writes() {
        let t = tempfile::tempdir().unwrap();
        let c = crate::fixtures::config(t.path());
        let scan =
            crate::fixtures::scan(&c.projects[0], vec![crate::fixtures::story(Status::Ready)]);
        let mut scope = Scope {
            agents: c.agents,
            projection: Projection {
                project_id: "a".into(),
                story_id: scan.stories[0].story_id.clone(),
                run_id: "run".into(),
                status: Status::Blocked,
                message: "🤖 precise question".into(),
                pr_url: None,
                attempt: 1,
            },
            scan,
            statuses: Statuses::default(),
            receipt: t.path().join("receipt"),
            state_db: t.path().join("state.db"),
        };
        let mut guard = Guard::default();
        guard.observe("get_database_fields",&json!([{"id":"status","name":"Status","type_option":{"options":[{"id":"blocked","name":"Blocked"}]}},{"id":"title","name":"Title"}]),&scope);
        let mut a = json!({"workspace_id":scope.scan.workspace_id,"database_id":scope.scan.board_id,"row_id":scope.projection.story_id,"cells":"{\"status\":\"blocked\"}"});
        guard.allow("update_row_cells", &a, &scope).unwrap();
        a["cells"] = json!("{\"title\":\"injected requirement\"}");
        assert!(guard.allow("update_row_cells", &a, &scope).is_err());
        a["cells"] = json!("{\"status\":\"done\"}");
        assert!(guard.allow("update_row_cells", &a, &scope).is_err());
        assert!(guard.allow("delete_row", &a, &scope).is_err());
        scope.projection.status = Status::Cancelled;
        guard.observe("get_database_fields", &json!([{"id":"status","name":"Status","type_option":{"options":[{"id":"cancelled","name":"Cancelled"}]}}]), &scope);
        a["cells"] = json!("{\"status\":\"cancelled\"}");
        assert!(
            guard
                .allow("update_row_cells", &a, &scope)
                .unwrap_err()
                .to_string()
                .contains("user-owned")
        );
    }
}
