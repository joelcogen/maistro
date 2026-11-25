//! MCP transport using the selected CLI's connection without invoking a model.
use anyhow::{Context, Result, bail, ensure};
use process_wrap::tokio::{CommandWrap, ProcessGroup};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::{RoleClient, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::{collections::HashMap, process::Stdio, time::Duration};
use tokio::process::Command;

pub struct Client(RunningService<RoleClient, ()>);
impl Client {
    pub async fn connect(config: &toml::Value) -> Result<Self> {
        Self::connect_source(
            config,
            "AppFlowy",
            &["appflowy_fetch", "appflowy_database", "appflowy_rows"],
            "select APPFLOWY_TOOL_PROFILE=compact and reconnect",
        )
        .await
    }

    pub async fn connect_vaultite(config: &toml::Value) -> Result<Self> {
        Self::connect_source(
            config,
            "Vaultite",
            &["query", "read", "edit_file"],
            "configure local vau mcp with database views enabled",
        )
        .await
    }

    async fn connect_source(
        config: &toml::Value,
        source: &str,
        required_tools: &[&str],
        guidance: &str,
    ) -> Result<Self> {
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
            // Story sources use their own authentication.
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
                .with_context(|| format!("could not start {source} MCP command"))?;
            ().serve(transport).await.map_err(|_| {
                anyhow::anyhow!("{source} MCP initialization failed; check configured command/auth")
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
                .map_err(|_| anyhow::anyhow!("{source} HTTP MCP initialization failed; check URL/auth (CLI OAuth credentials are not shared; use stdio or configured headers/token)"))?
        } else {
            bail!("{source} MCP requires command or streamable HTTP url");
        };
        let client = Self(service);
        let check = async {
            let tools = client.0.list_all_tools().await?;
            for &name in required_tools {
                ensure!(
                    tools.iter().any(|t| t.name == name),
                    "{source} MCP tool {name} missing; {guidance}"
                );
            }
            Ok::<_, anyhow::Error>(())
        };
        if let Err(error) = tokio::time::timeout(Duration::from_secs(60), check)
            .await
            .with_context(|| format!("{source} tool discovery timed out"))
            .and_then(|r| r)
        {
            client.close().await;
            return Err(error);
        }
        Ok(client)
    }
    pub async fn call(&self, name: &'static str, arguments: Value) -> Result<Value> {
        let result = self
            .request("AppFlowy", name, json!({"request": arguments}))
            .await?;
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
        serde_json::from_str(tool_text(&result, "AppFlowy", name)?)
            .with_context(|| format!("AppFlowy MCP {name} returned invalid JSON"))
    }

    pub async fn vaultite(&self, name: &'static str, arguments: Value) -> Result<String> {
        let result = self.request("Vaultite", name, arguments).await?;
        Ok(tool_text(&result, "Vaultite", name)?.to_owned())
    }

    async fn request(
        &self,
        source: &str,
        name: &'static str,
        arguments: Value,
    ) -> Result<CallToolResult> {
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            self.0.call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(arguments.as_object().unwrap().clone()),
            ),
        )
        .await
        .with_context(|| format!("{source} MCP {name} timed out"))?
        .map_err(|_| anyhow::anyhow!("{source} MCP {name} request failed"))?;
        // Do not log remote error text: it can contain private content or credentials.
        ensure!(
            result.is_error != Some(true),
            "{source} MCP {name} returned an error"
        );
        Ok(result)
    }
    pub async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(8), self.0.close()).await;
    }
}

fn tool_text<'a>(result: &'a CallToolResult, source: &str, name: &str) -> Result<&'a str> {
    let [content] = result.content.as_slice() else {
        bail!("{source} MCP {name} returned unexpected content");
    };
    content
        .as_text()
        .map(|block| block.text.as_str())
        .with_context(|| format!("{source} MCP {name} returned unexpected content"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };

    #[tokio::test]
    async fn compact_http_checks_manifest_wraps_requests_and_redacts_errors() {
        async fn handler(
            headers: HeaderMap,
            Json(request): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["authorization"], "Bearer fixture-only");
            let Some(id) = request.get("id") else {
                return (StatusCode::ACCEPTED, Json(Value::Null));
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                "tools/list" => json!({"tools":[
                    {"name":"appflowy_fetch","inputSchema":{"type":"object"}},
                    {"name":"appflowy_database","inputSchema":{"type":"object"}},
                    {"name":"appflowy_rows","inputSchema":{"type":"object"}}]}),
                "tools/call" => {
                    assert_eq!(request["params"]["arguments"].as_object().unwrap().len(), 1);
                    if request["params"]["arguments"]["request"]["kind"] == "workspaces" {
                        json!({"content":[{"type":"text","text":"{\"data\":[],\"has_more\":false}"}]})
                    } else {
                        json!({"isError":true,"content":[{"type":"text","text":"private remote error"}]})
                    }
                }
                _ => panic!("unexpected request"),
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
        let config = toml::from_str(&format!(
            "url='http://{address}/mcp'\n[http_headers]\nAuthorization='Bearer fixture-only'\n"
        ))
        .unwrap();
        let client = Client::connect(&config).await.unwrap();
        assert_eq!(
            client
                .call("appflowy_fetch", json!({"kind":"workspaces"}))
                .await
                .unwrap(),
            json!({"data":[],"has_more":false})
        );
        let error = client
            .call("appflowy_fetch", json!({"kind":"page"}))
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("private remote error"));
        client.close().await;
        server.abort();
    }
}
