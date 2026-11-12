//! Read-only MCP transport. Reuses Codex's configured connection, never Codex itself.
use anyhow::{Context, Result, bail, ensure};
use process_wrap::tokio::{CommandWrap, ProcessGroup};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    service::{RoleClient, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;
use std::{collections::HashMap, process::Stdio, time::Duration};
use tokio::process::Command;

pub struct Client(RunningService<RoleClient, ()>);
impl Client {
    pub async fn connect(config: &toml::Value) -> Result<Self> {
        let service = if let Some(command) = config.get("command").and_then(toml::Value::as_str) {
            let mut cmd = Command::new(command);
            if let Some(args) = config.get("args").and_then(toml::Value::as_array) {
                for arg in args {
                    cmd.arg(arg.as_str().context("invalid MCP argument")?);
                }
            }
            if let Some(cwd) = config.get("cwd").and_then(toml::Value::as_str) {
                cmd.current_dir(cwd);
            }
            // AppFlowy needs its own environment/auth, never the worker's GitHub token.
            cmd.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
            if let Some(env) = config.get("env").and_then(toml::Value::as_table) {
                for (k, v) in env {
                    cmd.env(k, v.as_str().context("invalid MCP environment")?);
                }
            }
            let mut command = CommandWrap::from(cmd);
            command.wrap(ProcessGroup::leader());
            let (transport, _) = TokioChildProcess::builder(command)
                .stderr(Stdio::null())
                .spawn()
                .context("could not start AppFlowy MCP command")?;
            ().serve(transport).await.map_err(|_| {
                anyhow::anyhow!("AppFlowy MCP initialization failed; check configured command/auth")
            })?
        } else if let Some(url) = config.get("url").and_then(toml::Value::as_str) {
            let mut headers = HashMap::new();
            for (key, from_env) in [("http_headers", false), ("env_http_headers", true)] {
                if let Some(values) = config.get(key).and_then(toml::Value::as_table) {
                    for (name, value) in values {
                        let value = value.as_str().context("invalid MCP header configuration")?;
                        let value = if from_env {
                            std::env::var(value)
                                .context("MCP header environment variable missing")?
                        } else {
                            value.to_owned()
                        };
                        headers.insert(name.parse()?, value.parse()?);
                    }
                }
            }
            let mut transport_config =
                StreamableHttpClientTransportConfig::with_uri(url.to_owned())
                    .custom_headers(headers);
            if let Some(env) = config
                .get("bearer_token_env_var")
                .and_then(toml::Value::as_str)
            {
                transport_config = transport_config.auth_header(
                    std::env::var(env).context("MCP bearer token environment variable missing")?,
                );
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            ().serve(StreamableHttpClientTransport::with_client(client, transport_config)).await
                .map_err(|_| anyhow::anyhow!("AppFlowy HTTP MCP initialization failed; check URL/auth (Codex OAuth credentials are not shared; use stdio or configured headers/token)"))?
        } else {
            bail!("AppFlowy MCP requires command or streamable HTTP url");
        };
        Ok(Self(service))
    }
    pub async fn call(&self, name: &'static str, arguments: Value) -> Result<Value> {
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            self.0.call_tool(
                CallToolRequestParams::new(name).with_arguments(
                    arguments
                        .as_object()
                        .context("MCP arguments must be an object")?
                        .clone(),
                ),
            ),
        )
        .await
        .context("AppFlowy MCP read timed out")?
        .map_err(|_| anyhow::anyhow!("AppFlowy MCP {name} request failed"))?;
        // Do not log remote error text: it can contain private content or credentials.
        ensure!(
            result.is_error != Some(true),
            "AppFlowy MCP {name} returned an error"
        );
        if let Some(mut data) = result.structured_content {
            // FastMCP wraps non-object return values in {"result": ...}.
            if data
                .as_object()
                .is_some_and(|o| o.len() == 1 && o.contains_key("result"))
            {
                data = data["result"].take();
            }
            return Ok(data);
        }
        let texts: Vec<_> = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect();
        ensure!(
            texts.len() == result.content.len(),
            "AppFlowy MCP {name} returned unexpected content"
        );
        if matches!(
            name,
            "get_workspaces"
                | "list_databases"
                | "get_database_fields"
                | "get_database_row_ids"
                | "get_database_row_details"
        ) {
            // FastMCP can emit one JSON text block per list item, including zero
            // blocks for an empty list. Keep an already encoded array intact.
            let values: Vec<Value> = texts
                .iter()
                .map(|text| serde_json::from_str(text))
                .collect::<std::result::Result<_, _>>()
                .with_context(|| format!("AppFlowy MCP {name} returned invalid JSON"))?;
            if values.len() == 1 && values[0].is_array() {
                return Ok(values.into_iter().next().unwrap());
            }
            ensure!(
                values.iter().all(|v| v.is_object()
                    && (v.get("id").is_some()
                        || v.get("row_id").is_some()
                        || v.get("workspace_id").is_some())),
                "AppFlowy MCP {name} returned an incomplete or unexpected list"
            );
            return Ok(Value::Array(values));
        }
        ensure!(
            texts.len() == 1,
            "AppFlowy MCP {name} returned unexpected content"
        );
        if name == "get_page_markdown" {
            // FastMCP strings are plain text, not JSON. Preserve exact content.
            return Ok(Value::String(texts[0].to_owned()));
        }
        serde_json::from_str(texts[0])
            .with_context(|| format!("AppFlowy MCP {name} returned invalid JSON"))
    }
    pub async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(8), self.0.close()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use serde_json::json;

    #[tokio::test]
    async fn streamable_http_reuses_headers_and_handles_text_results_and_tool_errors() {
        async fn handler(
            headers: HeaderMap,
            Json(request): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["authorization"], "Bearer fixture-only");
            assert_eq!(headers["x-appflowy-client"], "maistro-fixture");
            let Some(id) = request.get("id") else {
                return (StatusCode::ACCEPTED, Json(Value::Null));
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion": request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                "tools/call" => match request["params"]["name"].as_str().unwrap() {
                    "get_workspaces" => {
                        json!({"content":[{"type":"text","text":"[]"}], "_meta":{"volatile":123}})
                    }
                    "list_databases" => {
                        json!({"content":[{"type":"text","text":"{\"id\":\"one\"}"},{"type":"text","text":"{\"id\":\"two\"}"}]})
                    }
                    "get_database_fields" => {
                        json!({"content":[{"type":"text","text":"{\"id\":\"field\"}"}]})
                    }
                    "get_database_row_ids" => json!({"content":[]}),
                    "get_database_row_details" => {
                        json!({"content":[{"type":"text","text":"{\"partial\":true,\"rows\":[]}"}]})
                    }
                    "get_page_markdown" => json!({"content":[{"type":"text","text":"[1,2]"}]}),
                    _ => {
                        json!({"isError":true,"content":[{"type":"text","text":"private remote error"}]})
                    }
                },
                _ => panic!("unexpected MCP request"),
            };
            (
                StatusCode::OK,
                Json(json!({"jsonrpc":"2.0","id":id,"result":result})),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/mcp", post(handler)))
                .await
                .unwrap();
        });
        let config = toml::from_str(&format!("url='http://{address}/mcp'\n[http_headers]\nAuthorization='Bearer fixture-only'\nx-appflowy-client='maistro-fixture'\n")).unwrap();
        let client = Client::connect(&config).await.unwrap();
        assert_eq!(
            client.call("get_workspaces", json!({})).await.unwrap(),
            json!([])
        );
        assert_eq!(
            client.call("get_page_markdown", json!({})).await.unwrap(),
            json!("[1,2]")
        );
        assert_eq!(
            client.call("list_databases", json!({})).await.unwrap(),
            json!([{"id":"one"},{"id":"two"}])
        );
        assert_eq!(
            client.call("get_database_fields", json!({})).await.unwrap(),
            json!([{"id":"field"}])
        );
        assert_eq!(
            client
                .call("get_database_row_ids", json!({}))
                .await
                .unwrap(),
            json!([])
        );
        assert!(
            client
                .call("get_database_row_details", json!({}))
                .await
                .is_err()
        );
        let error = client.call("get_page", json!({})).await.unwrap_err();
        assert!(!format!("{error:#}").contains("private remote error"));
        client.close().await;
        server.abort();
    }
}
