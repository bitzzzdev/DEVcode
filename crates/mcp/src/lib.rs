//! MCP (Model Context Protocol) client for the Rust build.
//!
//! Supports local (stdio, newline-delimited JSON-RPC) and remote (Streamable HTTP)
//! servers, the initialize handshake, tools/list and tools/call. Remote servers may
//! authenticate with OAuth 2.0 (RFC 9728 discovery, PKCE, DCR, loopback callback)
//! via the `oauth` module; tokens are persisted in the shared `credential` table.
//! Config comes from `mcp.servers` in devcode.jsonc, same shape as the Bun build.
//!
//! Tool names are exposed namespaced as `<server>_<tool>` (server name normalized the
//! same way the Bun build normalizes: `[^a-zA-Z0-9_-] -> _`, then dots to underscores).

pub mod oauth;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-03-26";
pub const CLIENT_NAME: &str = "devcode";

#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    /// OAuth config present and not `false`. OAuth is not ported yet (Phase 10b).
    pub needs_oauth: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub enum ServerConfig {
    Local(LocalConfig),
    Remote(RemoteConfig),
}

#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    pub servers: BTreeMap<String, ServerConfig>,
}

/// Strip `//` and `/* */` comments from JSONC, keeping string literals intact.
pub fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Read `mcp.servers` from a devcode.jsonc file. Missing file -> empty config.
pub fn load_config(path: &Path) -> Result<McpConfig, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(McpConfig::default())
        }
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let value: Value = serde_json::from_str(&strip_jsonc(&raw))
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let mut config = McpConfig::default();
    let Some(servers) = value
        .get("mcp")
        .and_then(|mcp| mcp.get("servers"))
        .and_then(Value::as_object)
    else {
        return Ok(config);
    };
    for (name, entry) in servers {
        let disabled = entry
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match entry.get("type").and_then(Value::as_str).unwrap_or("") {
            "local" => {
                let command: Vec<String> = entry
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if command.is_empty() {
                    continue;
                }
                let environment = entry
                    .get("environment")
                    .and_then(Value::as_object)
                    .map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default();
                config.servers.insert(
                    name.clone(),
                    ServerConfig::Local(LocalConfig {
                        command,
                        cwd: entry.get("cwd").and_then(Value::as_str).map(str::to_owned),
                        environment,
                        disabled,
                    }),
                );
            }
            "remote" => {
                let Some(url) = entry.get("url").and_then(Value::as_str) else {
                    continue;
                };
                let headers = entry
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default();
                let oauth = entry.get("oauth");
                let needs_oauth = oauth
                    .map(|value| value != &Value::Bool(false))
                    .unwrap_or(false);
                config.servers.insert(
                    name.clone(),
                    ServerConfig::Remote(RemoteConfig {
                        url: url.to_owned(),
                        headers,
                        needs_oauth,
                        disabled,
                    }),
                );
            }
            _ => {}
        }
    }
    Ok(config)
}

pub fn normalize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `<server>_<tool>` with both parts normalized (Bun effectiveName equivalent).
pub fn namespaced_tool_name(server: &str, tool: &str) -> String {
    format!("{}_{}", normalize_name(server), normalize_name(tool))
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub enum Connection {
    Stdio(StdioConnection),
    Http(HttpConnection),
}

impl Connection {
    pub fn connect(
        name: &str,
        config: &ServerConfig,
        connect_timeout: Duration,
        db_path: &Path,
    ) -> Result<Connection, String> {
        match config {
            ServerConfig::Local(local) => {
                StdioConnection::connect(name, local, connect_timeout).map(Connection::Stdio)
            }
            ServerConfig::Remote(remote) => {
                if remote.needs_oauth {
                    // OAuth is supported now; the connection will load the stored token or fail
                    // with a clear "run DEVcode mcp auth <name>" error when none is available.
                }
                let mut connection = HttpConnection {
                    name: name.to_owned(),
                    url: remote.url.clone(),
                    headers: remote.headers.clone(),
                    session_id: None,
                    next_id: 1,
                    timeout: connect_timeout,
                    needs_oauth: remote.needs_oauth,
                    db_path: db_path.to_path_buf(),
                };
                connection.handshake(connect_timeout)?;
                Ok(Connection::Http(connection))
            }
        }
    }

    pub fn list_tools(&mut self, timeout: Duration) -> Result<Vec<ToolDef>, String> {
        let result = self.request("tools/list", json!({}), timeout)?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .filter_map(|tool| {
                Some(ToolDef {
                    name: tool.get("name")?.as_str()?.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                })
            })
            .collect())
    }

    /// Call a tool and return its text content joined (non-text content is dropped, like
    /// the Bun build's default text rendering for unsupported content).
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: &Value,
        timeout: Duration,
    ) -> Result<String, String> {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            timeout,
        )?;
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(tool_text(&result).unwrap_or_else(|| "tool reported an error".to_owned()));
        }
        Ok(tool_text(&result).unwrap_or_else(|| result.to_string()))
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        match self {
            Connection::Stdio(stdio) => stdio.request(method, params, timeout),
            Connection::Http(http) => http.request(method, params, timeout),
        }
    }
}

fn tool_text(result: &Value) -> Option<String> {
    let texts: Vec<&str> = result
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            (item.get("type")?.as_str()? == "text").then_some(item.get("text")?.as_str()?)
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

// ---- stdio ----

struct StdioReader {
    receiver: Receiver<Value>,
}

pub struct StdioConnection {
    #[allow(dead_code)]
    name: String,
    child: Child,
    stdin: ChildStdin,
    reader: StdioReader,
    next_id: u64,
}

impl StdioConnection {
    fn connect(
        name: &str,
        config: &LocalConfig,
        timeout: Duration,
    ) -> Result<StdioConnection, String> {
        if config.command.is_empty() {
            return Err(format!("MCP server \"{name}\" has an empty command"));
        }
        let mut command = Command::new(&config.command[0]);
        command
            .args(&config.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &config.environment {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| {
            format!(
                "MCP server \"{name}\" failed to spawn {:?}: {error}",
                config.command
            )
        })?;
        let stdin = child.stdin.take().ok_or("MCP child stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("MCP child stdout unavailable")?;
        let (sender, receiver) = channel::<Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let mut connection = StdioConnection {
            name: name.to_owned(),
            child,
            stdin,
            reader: StdioReader { receiver },
            next_id: 1,
        };
        let timeout = timeout.min(Duration::from_secs(10));
        connection.handshake(timeout)?;
        Ok(connection)
    }

    fn handshake(&mut self, timeout: Duration) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        self.notify("notifications/initialized", json!({}))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(self.stdin, "{message}").map_err(|error| format!("MCP write failed: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("MCP write failed: {error}"))
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{message}").map_err(|error| format!("MCP write failed: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("MCP write failed: {error}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("MCP request {method} timed out"));
            }
            let message =
                self.reader.receiver.recv_timeout(remaining).map_err(|_| {
                    format!("MCP request {method} timed out waiting for a response")
                })?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    return Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("MCP error")
                        .to_owned());
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            // server -> client request: respond method-not-supported (or roots/list empty)
            if message.get("method").is_some() && message.get("id").is_some() {
                let reply_id = message.get("id").cloned().unwrap_or(Value::Null);
                let reply = if message["method"] == "roots/list" {
                    json!({ "jsonrpc": "2.0", "id": reply_id, "result": { "roots": [] } })
                } else {
                    json!({ "jsonrpc": "2.0", "id": reply_id, "error": { "code": -32601, "message": "method not supported by devcode" } })
                };
                let _ = writeln!(self.stdin, "{reply}");
                let _ = self.stdin.flush();
            }
            // notifications: drop
        }
    }
}

impl Drop for StdioConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- Streamable HTTP ----

pub struct HttpConnection {
    name: String,
    url: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
    next_id: u64,
    #[allow(dead_code)]
    timeout: Duration,
    needs_oauth: bool,
    db_path: PathBuf,
}

impl HttpConnection {
    fn handshake(&mut self, timeout: Duration) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        // fire-and-forget notification
        let _ = self.post(
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
            timeout,
        );
        Ok(())
    }

    fn load_oauth_token(&mut self) -> Result<(), String> {
        if !self.needs_oauth || self.headers.contains_key("authorization") {
            return Ok(());
        }
        let integration_id = oauth::CredentialStore::integration_id(&self.name, &self.url);
        let store = oauth::CredentialStore::open(&self.db_path.to_string_lossy()).map_err(|error| format!("open credential store: {error}"))?;
        match store.get(&integration_id) {
            Ok(Some(stored)) if stored.kind == "oauth" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if stored.expires == 0 || stored.expires > now {
                    self.headers.insert("authorization".into(), format!("Bearer {}", stored.access));
                    return Ok(());
                }
                drop(store);
                self.refresh_oauth_token()
            }
            _ => Err(format!(
                "MCP server \"{}\" requires OAuth authentication. Run: DEVcode mcp auth {}",
                self.name, self.name
            )),
        }
    }

    /// Force a token refresh and replace the cached Authorization header.
    fn refresh_oauth_token(&mut self) -> Result<(), String> {
        let integration_id = oauth::CredentialStore::integration_id(&self.name, &self.url);
        let store = oauth::CredentialStore::open(&self.db_path.to_string_lossy())
            .map_err(|error| format!("open credential store: {error}"))?;
        let stored = store
            .get(&integration_id)
            .map_err(|error| format!("credential store: {error}"))?
            .ok_or_else(|| format!("no stored credentials for MCP server \"{}\". Run: DEVcode mcp auth {}", self.name, self.name))?;
        let agent = ureq::AgentBuilder::new().build();
        let refreshed = oauth::refresh(&agent, &self.name, &self.url, &stored)
            .map_err(|error| format!("token refresh failed: {error}. Run: DEVcode mcp auth {}", self.name))?;
        store
            .save(&integration_id, "default", &refreshed)
            .map_err(|error| format!("save refreshed token: {error}"))?;
        self.headers.insert("authorization".into(), format!("Bearer {}", refreshed.access));
        Ok(())
    }

    fn post(&mut self, body: Value, timeout: Duration) -> Result<ureq::Response, String> {
        // Load (or refresh) the token before capturing headers into the request.
        if self.needs_oauth {
            self.load_oauth_token()?;
        }
        let mut request = ureq::post(&self.url)
            .set("content-type", "application/json")
            .set("accept", "application/json, text/event-stream")
            .set("MCP-Protocol-Version", PROTOCOL_VERSION)
            .timeout(timeout);
        for (key, value) in &self.headers {
            request = request.set(key, value);
        }
        if let Some(session_id) = &self.session_id {
            request = request.set("MCP-Session-Id", session_id);
        }
        request.send_json(body).map_err(|error| match error {
            ureq::Error::Status(code, response) => {
                let body = response.into_string().unwrap_or_default();
                format!("MCP server \"{}\" returned HTTP {code}: {body}", self.name)
            }
            ureq::Error::Transport(transport) => {
                format!("MCP server \"{}\" unreachable: {transport}", self.name)
            }
        })
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let mut response = self.post(body.clone(), timeout);
        // A stored token may have been revoked server-side. Refresh once and retry on HTTP 401.
        if self.needs_oauth {
            if let Err(ref message) = response {
                if message.contains("HTTP 401") {
                    self.headers.remove("authorization");
                    self.refresh_oauth_token()?;
                    response = self.post(body, timeout);
                }
            }
        }
        let response = response?;
        // session id handed out on initialize
        if let Some(session_id) = response.header("mcp-session-id").map(str::to_owned) {
            self.session_id = Some(session_id);
        }
        let content_type = response.content_type().to_owned();
        let text = response.into_string().unwrap_or_default();
        let message = if content_type.contains("text/event-stream") {
            parse_sse_for_id(&text, id).ok_or_else(|| {
                format!(
                    "MCP server \"{}\" SSE response contained no message with id {id}",
                    self.name
                )
            })?
        } else {
            serde_json::from_str::<Value>(&text).map_err(|error| {
                format!(
                    "MCP server \"{}\" returned invalid JSON: {error}",
                    self.name
                )
            })?
        };
        if let Some(error) = message.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP error")
                .to_owned());
        }
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn parse_sse_for_id(body: &str, id: u64) -> Option<Value> {
    for block in body.split("\n\n") {
        let mut data_line = None;
        for line in block.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                data_line = Some(data.trim().to_owned());
            }
        }
        let Some(data) = data_line else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Some(value);
        }
    }
    None
}

// ---- agent-loop integration helpers ----

/// One discovered MCP tool, namespaced for model visibility.
#[derive(Debug, Clone)]
pub struct AgentTool {
    pub namespaced: String,
    pub server: String,
    pub tool: ToolDef,
}

pub struct Runtime {
    pub tools: Vec<AgentTool>,
    connections: Vec<(String, Connection)>,
}

impl Runtime {
    /// Connect to every enabled server and discover tools. Servers that fail to
    /// connect are reported on stderr and skipped (the run continues without them).
    pub fn connect_all(servers: &BTreeMap<String, ServerConfig>, timeout: Duration, db_path: &Path) -> Runtime {
        let mut runtime = Runtime {
            tools: Vec::new(),
            connections: Vec::new(),
        };
        for (name, config) in servers {
            if matches!(config, ServerConfig::Local(local) if local.disabled)
                || matches!(config, ServerConfig::Remote(remote) if remote.disabled)
            {
                continue;
            }
            match Connection::connect(name, config, timeout, db_path) {
                Ok(mut connection) => match connection.list_tools(timeout) {
                    Ok(tools) => {
                        for tool in tools {
                            runtime.tools.push(AgentTool {
                                namespaced: namespaced_tool_name(name, &tool.name),
                                server: name.clone(),
                                tool,
                            });
                        }
                        runtime.connections.push((name.clone(), connection));
                    }
                    Err(error) => eprintln!("MCP server \"{name}\" tools/list failed: {error}"),
                },
                Err(error) => eprintln!("{error}"),
            }
        }
        runtime
    }

    pub fn is_mcp_tool(&self, name: &str) -> Option<&AgentTool> {
        self.tools.iter().find(|tool| tool.namespaced == name)
    }

    pub fn call(
        &mut self,
        name: &str,
        arguments: &Value,
        timeout: Duration,
    ) -> Option<Result<String, String>> {
        let index = self.tools.iter().position(|tool| tool.namespaced == name)?;
        let server = self.tools[index].server.clone();
        let tool_name = self.tools[index].tool.name.clone();
        let (_, connection) = self
            .connections
            .iter_mut()
            .find(|(name, _)| *name == server)?;
        Some(connection.call_tool(&tool_name, arguments, timeout))
    }
}
