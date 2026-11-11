//! Setup mutations are limited to one new child database, never an existing board.
use crate::{
    board::CreatedBoard,
    config::Agents,
    mcp_guard::{send, server_command},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};
use tokio::io::{AsyncBufReadExt, BufReader};

pub(crate) const WRITE_TOOLS: &[&str] = &[
    "create_database",
    "create_database_field",
    "update_database_field",
    "update_database_field_type_option",
    "update_database_view",
    "update_database_view_settings",
    "create_select_option",
    "update_select_options",
];
#[derive(Serialize, Deserialize)]
pub(crate) struct Scope {
    pub agents: Agents,
    pub workspace_id: String,
    pub parent_view_id: String,
}
#[derive(Default)]
struct Guard {
    creation_attempted: bool,
    board: Option<CreatedBoard>,
}
impl Guard {
    fn allow(&mut self, name: &str, args: &Value, scope: &Scope) -> Result<()> {
        if crate::codex::READ_TOOLS.contains(&name) {
            return Ok(());
        }
        ensure!(
            WRITE_TOOLS.contains(&name),
            "tool not permitted during board setup"
        );
        ensure!(
            args["workspace_id"] == scope.workspace_id,
            "wrong setup workspace"
        );
        if name == "create_database" {
            ensure!(
                !self.creation_attempted,
                "board creation already attempted; inspect the parent page before retrying"
            );
            ensure!(
                args["parent_view_id"] == scope.parent_view_id
                    && args["name"] == "Board"
                    && (args["layout"] == "Board"
                        || args["layout"] == "board"
                        || args["layout"] == 1),
                "wrong setup parent, name or layout"
            );
            self.creation_attempted = true;
        } else {
            let board = self
                .board
                .as_ref()
                .context("no confirmed new database to configure")?;
            if name.starts_with("update_database_view") {
                ensure!(
                    args["view_id"] == board.view_id,
                    "cannot change another view"
                );
            } else {
                ensure!(
                    args["database_id"] == board.database_id,
                    "cannot change another database"
                );
            }
            if let Some(id) = args.get("database_id") {
                ensure!(id == &board.database_id, "wrong database");
            }
            if let Some(id) = args.get("view_id") {
                ensure!(id == &board.view_id, "wrong view");
            }
            ensure!(
                args.get("parent_view_id").is_none(),
                "cannot reparent a database during setup"
            );
        }
        Ok(())
    }
    fn observe(&mut self, result: &Value, scope: &Scope) {
        if result["isError"] == true {
            return;
        }
        let mut values = vec![result.clone()];
        if let Some(content) = result["content"].as_array() {
            for block in content {
                if let Some(text) = block["text"].as_str()
                    && let Ok(value) = serde_json::from_str(text)
                {
                    values.push(value);
                }
            }
        }
        fn id(value: &Value, key: &str) -> Option<String> {
            match value {
                Value::Object(o) => o
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                    .map(str::to_owned)
                    .or_else(|| o.values().find_map(|v| id(v, key))),
                Value::Array(a) => a.iter().find_map(|v| id(v, key)),
                _ => None,
            }
        }
        for value in values {
            if let (Some(database_id), Some(view_id)) =
                (id(&value, "database_id"), id(&value, "view_id"))
                && database_id != view_id
                && view_id != scope.parent_view_id
            {
                self.board = Some(CreatedBoard {
                    workspace_id: scope.workspace_id.clone(),
                    database_id,
                    view_id,
                });
                return;
            }
        }
    }
}
pub async fn serve(path: &Path) -> Result<()> {
    let scope: Scope = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut child = server_command(&scope.agents)?.spawn()?;
    let mut input = child.stdin.take().context("missing MCP stdin")?;
    let mut responses = BufReader::new(child.stdout.take().context("missing MCP stdout")?).lines();
    let mut requests = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    let mut pending = HashMap::new();
    let mut guard = Guard::default();
    loop {
        tokio::select! {
            line = requests.next_line() => {
                let Some(line) = line? else { break; };
                ensure!(line.len() <= 16 * 1024 * 1024, "oversized MCP request");
                let request: Value = serde_json::from_str(&line)?;
                if request["method"] == "tools/call" {
                    let name = request["params"]["name"].as_str().unwrap_or("");
                    if let Err(error) = guard.allow(name, &request["params"]["arguments"], &scope) {
                        send(&mut output, &json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":true,"content":[{"type":"text","text":error.to_string()}]}})).await?;
                        continue;
                    }
                    pending.insert(request["id"].to_string(), name.to_owned());
                } else {
                    ensure!(matches!(request["method"].as_str(), Some("initialize" | "notifications/initialized" | "notifications/cancelled" | "tools/list" | "resources/list" | "resources/templates/list" | "resources/read" | "prompts/list" | "ping")), "unsupported setup MCP method");
                }
                send(&mut input, &request).await?;
            }
            line = responses.next_line() => {
                let Some(line) = line? else { break; };
                ensure!(line.len() <= 16 * 1024 * 1024, "oversized MCP response");
                let response: Value = serde_json::from_str(&line)?;
                if pending.remove(&response["id"].to_string()).as_deref() == Some("create_database") && response.get("error").is_none() {
                    guard.observe(&response["result"], &scope);
                }
                send(&mut output, &response).await?;
            }
        }
    }
    child.kill().await.ok();
    child.wait().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_can_only_mutate_its_new_database_and_create_once() {
        let scope = Scope {
            agents: Agents::default(),
            workspace_id: uuid::Uuid::new_v4().to_string(),
            parent_view_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut guard = Guard::default();
        let database = uuid::Uuid::new_v4().to_string();
        let view = uuid::Uuid::new_v4().to_string();
        let update = json!({"workspace_id":scope.workspace_id,"database_id":database});
        assert!(
            guard
                .allow("update_database_field", &update, &scope)
                .is_err()
        );
        let create = json!({"workspace_id":scope.workspace_id,"parent_view_id":scope.parent_view_id,"name":"Board","layout":"Board"});
        guard.allow("create_database", &create, &scope).unwrap();
        assert!(guard.allow("create_database", &create, &scope).is_err());
        guard.observe(&json!({"content":[{"text":json!({"database_id":database,"view_id":view}).to_string()}]}), &scope);
        guard
            .allow("update_database_field", &update, &scope)
            .unwrap();
        assert!(
            guard
                .allow(
                    "update_database_field",
                    &json!({"workspace_id":scope.workspace_id,"database_id":scope.parent_view_id}),
                    &scope
                )
                .is_err()
        );
        assert!(guard.allow("delete_database", &update, &scope).is_err());
        assert!(guard.allow("update_row_cells", &update, &scope).is_err());
        assert!(
            guard
                .allow(
                    "update_database_view",
                    &json!({"workspace_id":scope.workspace_id,"view_id":scope.parent_view_id}),
                    &scope
                )
                .is_err()
        );
    }
}
