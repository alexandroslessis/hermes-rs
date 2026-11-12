//! Hermes's MCP client (`tools/mcp_tool*.py`, MIT): connects to the configured MCP servers (a
//! local command over stdio, or a Streamable HTTP URL), lists their tools and offers them to the
//! agent as `mcp__<server>__<tool>`. Servers connect in the background; a server that fails shows
//! its error in Settings and its tools are simply absent.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool};
use rmcp::service::{RoleClient, RunningService};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `_DEFAULT_TOOL_TIMEOUT` and `_DEFAULT_CONNECT_TIMEOUT`.
const DEFAULT_TOOL_TIMEOUT: u64 = 300;
const DEFAULT_CONNECT_TIMEOUT: u64 = 60;
/// `MCP_TOOL_NAME_PREFIX` and the provider limit on function names.
const PREFIX: &str = "mcp__";
const MAX_NAME_LENGTH: usize = 64;

fn default_true() -> bool {
    true
}

/// One `mcp_servers` entry (Hermes's keys; also the shape Claude Desktop and Cursor use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpServerConfig {
    /// A local server: the program to run and its arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// A remote server: its Streamable HTTP endpoint and headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Seconds per tool call (default 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Seconds to connect and list tools (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "ToolFilter::is_empty")]
    pub tools: ToolFilter,
    /// Not Hermes's: `true` says the server's results are the user's own (a local notes server),
    /// so reading them doesn't make the session "careful" (taint, Phase 6). Off by default: what
    /// an MCP server returns may be text a stranger wrote.
    #[serde(default, skip_serializing_if = "is_false")]
    pub trusted: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `tools.include` / `tools.exclude`: which of a server's tools are offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl ToolFilter {
    fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    fn allows(&self, tool: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|t| t == tool))
            && !self.exclude.iter().any(|t| t == tool)
    }
}

/// Reads `mcp_servers` as Hermes users write it, or a pasted `{"mcpServers": {...}}`.
pub fn parse_servers(text: &str) -> Result<BTreeMap<String, McpServerConfig>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("Not valid JSON: {e}"))?;
    let servers = value
        .get("mcpServers")
        .or_else(|| value.get("mcp_servers"))
        .cloned()
        .unwrap_or(value);
    let servers: BTreeMap<String, McpServerConfig> =
        serde_json::from_value(servers).map_err(|e| format!("Invalid server entry: {e}"))?;
    for (name, server) in &servers {
        match (&server.command, &server.url) {
            (Some(_), Some(_)) => {
                return Err(format!("{name}: give either a command or a url, not both"));
            }
            (None, None) => {
                return Err(format!(
                    "{name}: needs a command (local server) or a url (remote server)"
                ));
            }
            _ => {}
        }
    }
    Ok(servers)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Disabled,
    Connecting,
    Connected { tools: usize },
    Failed(String),
}

type Client = RunningService<RoleClient, ()>;

struct Server {
    config: McpServerConfig,
    status: ServerStatus,
    /// Offered tools under their prefixed names, with the server's own names.
    tools: Vec<(Tool, String)>,
    client: Option<Arc<Client>>,
    /// Bumped on every (re)connect, so a stale connection attempt doesn't overwrite a newer one.
    generation: u64,
}

/// The MCP servers every session shares.
#[derive(Default)]
pub struct McpManager {
    servers: Mutex<BTreeMap<String, Server>>,
}

/// `sanitize_mcp_name_component`.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `mcp_prefixed_tool_name`: `mcp__<server>__<tool>`, clamped to 64 characters with a stable hash.
pub fn prefixed_name(server: &str, tool: &str) -> String {
    let full = format!("{PREFIX}{}__{}", sanitize(server), sanitize(tool));
    if full.len() <= MAX_NAME_LENGTH {
        return full;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in full.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let suffix = format!("_{:08x}", hash as u32);
    format!("{}{suffix}", &full[..MAX_NAME_LENGTH - suffix.len()])
}

/// `_normalize_mcp_input_schema`: an object schema the providers accept.
fn normalize_schema(schema: &JsonObject) -> JsonObject {
    let mut schema = schema.clone();
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        schema.insert("type".into(), json!("object"));
    }
    if !schema.get("properties").is_some_and(Value::is_object) {
        schema.insert("properties".into(), json!({}));
    }
    schema
}

/// `_render_call_tool_result`: `{"result": text}` (with `structuredContent` when it says more than
/// the text), or `{"error": text}`. Images stay as image blocks for vision models.
pub fn render_result(result: CallToolResult) -> CallToolResult {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for block in &result.content {
        match block {
            ContentBlock::Text(text) => texts.push(text.text.clone()),
            ContentBlock::Image(_) => images.push(block.clone()),
            ContentBlock::Resource(resource) => {
                if let Some(text) = serde_json::to_value(&resource.resource)
                    .ok()
                    .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_string))
                {
                    texts.push(text);
                }
            }
            ContentBlock::ResourceLink(link) => texts.push(format!("[resource: {}]", link.uri)),
            ContentBlock::Audio(_) => texts.push("[audio content omitted]".into()),
            _ => {}
        }
    }
    let text = texts.join("\n");
    if result.is_error == Some(true) {
        let message = if text.trim().is_empty() {
            "MCP tool returned an error".to_string()
        } else {
            text
        };
        return CallToolResult::error(vec![ContentBlock::text(
            json!({"error": message}).to_string(),
        )]);
    }
    let structured = result.structured_content.clone().filter(|structured| {
        // Dropped when a text block already carries the same JSON (the spec's dual-emit).
        !texts
            .iter()
            .any(|t| serde_json::from_str::<Value>(t).ok().as_ref() == Some(structured))
    });
    let payload = match (structured, text.trim().is_empty()) {
        (Some(structured), true) => json!({"result": structured}),
        (Some(structured), false) => json!({"result": text, "structuredContent": structured}),
        (None, _) => json!({"result": text}),
    };
    let mut content = vec![ContentBlock::text(payload.to_string())];
    content.extend(images);
    CallToolResult::success(content)
}

/// Finds a program the way a shell would (`npx` → `npx.cmd`); batch files run through `cmd /c`.
fn command_for(program: &str, args: &[String]) -> tokio::process::Command {
    let path = std::path::Path::new(program);
    let resolved = if path.extension().is_some() || path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                ["exe", "cmd", "bat"]
                    .iter()
                    .map(|ext| dir.join(format!("{program}.{ext}")))
                    .find(|candidate| candidate.is_file())
            })
        })
    };
    let resolved = resolved.unwrap_or_else(|| path.to_path_buf());
    let is_batch = resolved
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut command = if is_batch {
        let mut command = tokio::process::Command::new("cmd");
        command.arg("/c").arg(&resolved);
        command
    } else {
        tokio::process::Command::new(&resolved)
    };
    command.args(args);
    command.creation_flags(0x0800_0000);
    command
}

/// Connects and lists the server's tools.
async fn connect(config: &McpServerConfig) -> Result<(Client, Vec<Tool>), String> {
    let client = if let Some(url) = &config.url {
        use rmcp::transport::StreamableHttpClientTransport;
        use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
        let mut headers = HashMap::new();
        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.clone());
        for (name, value) in &config.headers {
            if name.eq_ignore_ascii_case("authorization") {
                transport_config = transport_config
                    .auth_header(value.strip_prefix("Bearer ").unwrap_or(value).to_string());
                continue;
            }
            let name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("bad header name {name}: {e}"))?;
            let value = http::HeaderValue::from_str(value)
                .map_err(|e| format!("bad header value for {name}: {e}"))?;
            headers.insert(name, value);
        }
        let transport =
            StreamableHttpClientTransport::from_config(transport_config.custom_headers(headers));
        ().serve(transport).await.map_err(|e| format!("{e}"))?
    } else {
        let program = config.command.as_deref().ok_or("no command or url")?;
        let mut command = command_for(program, &config.args);
        command.envs(&config.env);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        let (transport, _) = rmcp::transport::TokioChildProcess::builder(command)
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {program}: {e}"))?;
        ().serve(transport).await.map_err(|e| format!("{e}"))?
    };
    let tools = client
        .list_all_tools()
        .await
        .map_err(|e| format!("listing tools failed: {e}"))?;
    Ok((client, tools))
}

impl McpManager {
    /// Applies the configured servers: new and changed ones (re)connect in the background, removed
    /// and disabled ones are stopped. Unchanged connections are kept.
    pub fn configure(self: &Arc<Self>, servers: BTreeMap<String, McpServerConfig>) {
        let mut current = self.servers.lock();
        current.retain(|name, server| servers.get(name) == Some(&server.config));
        for (name, config) in servers {
            if current.contains_key(&name) {
                continue;
            }
            let enabled = config.enabled;
            current.insert(
                name.clone(),
                Server {
                    config: config.clone(),
                    status: if enabled {
                        ServerStatus::Connecting
                    } else {
                        ServerStatus::Disabled
                    },
                    tools: Vec::new(),
                    client: None,
                    generation: 0,
                },
            );
            if enabled {
                self.start(name, config, 0);
            }
        }
    }

    fn start(self: &Arc<Self>, name: String, config: McpServerConfig, generation: u64) {
        let manager = self.clone();
        tokio::spawn(async move {
            let wait =
                Duration::from_secs(config.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT));
            let outcome = match tokio::time::timeout(wait, connect(&config)).await {
                Ok(outcome) => outcome,
                Err(_) => Err(format!("timed out connecting after {}s", wait.as_secs())),
            };
            manager.connected(&name, generation, outcome);
        });
    }

    fn connected(&self, name: &str, generation: u64, outcome: Result<(Client, Vec<Tool>), String>) {
        let mut servers = self.servers.lock();
        let Some(server) = servers.get_mut(name).filter(|s| s.generation == generation) else {
            return;
        };
        match outcome {
            Ok((client, tools)) => {
                let offered: Vec<(Tool, String)> = tools
                    .into_iter()
                    .filter(|tool| server.config.tools.allows(&tool.name))
                    .map(|tool| {
                        let description = tool
                            .description
                            .clone()
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| format!("MCP tool {} from {name}", tool.name));
                        let offered = Tool::new(
                            prefixed_name(name, &tool.name),
                            description,
                            Arc::new(normalize_schema(&tool.input_schema)),
                        );
                        (offered, tool.name.to_string())
                    })
                    .collect();
                tracing::info!("MCP server {name}: {} tools", offered.len());
                server.status = ServerStatus::Connected {
                    tools: offered.len(),
                };
                server.tools = offered;
                server.client = Some(Arc::new(client));
            }
            Err(error) => {
                tracing::warn!("MCP server {name} failed: {error}");
                server.status = ServerStatus::Failed(error);
            }
        }
    }

    /// Reconnects one server (Settings' Retry).
    pub fn reconnect(self: &Arc<Self>, name: &str) {
        let restart = {
            let mut servers = self.servers.lock();
            servers
                .get_mut(name)
                .filter(|s| s.config.enabled)
                .map(|server| {
                    server.generation += 1;
                    server.status = ServerStatus::Connecting;
                    server.tools.clear();
                    server.client = None;
                    (server.config.clone(), server.generation)
                })
        };
        if let Some((config, generation)) = restart {
            self.start(name.to_string(), config, generation);
        }
    }

    pub fn statuses(&self) -> Vec<(String, ServerStatus)> {
        self.servers
            .lock()
            .iter()
            .map(|(name, server)| (name.clone(), server.status.clone()))
            .collect()
    }

    /// The tools of every connected server, under their `mcp__` names.
    pub fn tools(&self) -> Vec<Tool> {
        self.servers
            .lock()
            .values()
            .flat_map(|server| server.tools.iter().map(|(tool, _)| tool.clone()))
            .collect()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    /// The server an `mcp__` tool belongs to, and whether the user marked it `trusted`.
    pub fn server_of(&self, tool_name: &str) -> Option<(String, bool)> {
        self.servers.lock().iter().find_map(|(name, server)| {
            server
                .tools
                .iter()
                .any(|(tool, _)| tool.name == tool_name)
                .then(|| (name.clone(), server.config.trusted))
        })
    }

    /// Calls an `mcp__` tool on its server, within the server's timeout.
    pub async fn call(&self, name: &str, arguments: JsonObject) -> CallToolResult {
        let found = self
            .servers
            .lock()
            .iter()
            .find_map(|(server_name, server)| {
                server
                    .tools
                    .iter()
                    .find(|(tool, _)| tool.name == name)
                    .map(|(_, original)| {
                        (
                            server_name.clone(),
                            original.clone(),
                            server.client.clone(),
                            server.config.timeout.unwrap_or(DEFAULT_TOOL_TIMEOUT),
                        )
                    })
            });
        let error = |message: String| {
            CallToolResult::error(vec![ContentBlock::text(
                json!({"error": message}).to_string(),
            )])
        };
        let Some((server, original, Some(client), timeout)) = found else {
            return error(format!(
                "MCP tool {name} is not available (its server is not connected)."
            ));
        };
        let params = CallToolRequestParams::new(original).with_arguments(arguments);
        match tokio::time::timeout(Duration::from_secs(timeout), client.call_tool(params)).await {
            Ok(Ok(result)) => render_result(result),
            Ok(Err(e)) => error(format!("MCP server {server}: {e}")),
            Err(_) => error(format!("MCP tool {name} timed out after {timeout}s")),
        }
    }

    /// Connects a server over an already-open transport (tests).
    #[doc(hidden)]
    pub async fn connect_transport<T, E, A>(&self, name: &str, transport: T) -> Result<(), String>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.connect_transport_with(name, McpServerConfig::default(), transport)
            .await
    }

    /// [`Self::connect_transport`] for a server with this configuration (tests).
    #[doc(hidden)]
    pub async fn connect_transport_with<T, E, A>(
        &self,
        name: &str,
        config: McpServerConfig,
        transport: T,
    ) -> Result<(), String>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client = ().serve(transport).await.map_err(|e| format!("{e}"))?;
        let tools = client.list_all_tools().await.map_err(|e| format!("{e}"))?;
        self.servers.lock().insert(
            name.to_string(),
            Server {
                config,
                status: ServerStatus::Connecting,
                tools: Vec::new(),
                client: None,
                generation: 0,
            },
        );
        self.connected(name, 0, Ok((client, tools)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_hermes() {
        assert_eq!(
            prefixed_name("my-server", "get.time"),
            "mcp__my_server__get_time"
        );
        let long = prefixed_name(&"s".repeat(40), &"t".repeat(40));
        assert_eq!(long.len(), 64);
        assert_eq!(long, prefixed_name(&"s".repeat(40), &"t".repeat(40)));
    }

    #[test]
    fn parses_hermes_and_pasted_configs() {
        let servers = parse_servers(
            r#"{"mcpServers": {"time": {"command": "uvx", "args": ["mcp-server-time"]},
                "remote": {"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer x"}, "enabled": false}}}"#,
        )
        .unwrap();
        assert_eq!(servers["time"].args, ["mcp-server-time"]);
        assert!(servers["time"].enabled && !servers["remote"].enabled);
        assert!(
            parse_servers(r#"{"x": {}}"#)
                .unwrap_err()
                .contains("needs a command")
        );
        assert!(parse_servers("nope").is_err());
    }

    #[test]
    fn results_render_like_hermes() {
        let ok = render_result(CallToolResult::success(vec![ContentBlock::text("12:00")]));
        assert_eq!(
            ok.content[0].as_text().unwrap().text,
            r#"{"result":"12:00"}"#
        );
        let failed = render_result(CallToolResult::error(vec![ContentBlock::text("boom")]));
        assert_eq!(failed.is_error, Some(true));
        assert_eq!(
            failed.content[0].as_text().unwrap().text,
            r#"{"error":"boom"}"#
        );
        let filter = ToolFilter {
            include: vec![],
            exclude: vec!["delete".into()],
        };
        assert!(filter.allows("read") && !filter.allows("delete"));
    }
}
