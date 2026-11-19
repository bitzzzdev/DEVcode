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
        if let Some(server) = parse_server(name, entry) {
            config.servers.insert(name.clone(), server);
        }
    }
    Ok(config)
}

/// Parse one `mcp.servers` entry into a [`ServerConfig`]. Entries with an unknown
/// type or missing required fields yield `None`.
pub fn parse_server(_name: &str, entry: &Value) -> Option<ServerConfig> {
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
                return None;
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
            Some(ServerConfig::Local(LocalConfig {
                command,
                cwd: entry.get("cwd").and_then(Value::as_str).map(str::to_owned),
                environment,
                disabled,
            }))
        }
        "remote" => {
            let url = entry.get("url").and_then(Value::as_str)?;
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
            Some(ServerConfig::Remote(RemoteConfig {
                url: url.to_owned(),
                headers,
                needs_oauth,
                disabled,
            }))
        }
        _ => None,
    }
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
                    resources_advertised: false,
                    prompts_advertised: false,
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

    /// Whether the server advertised a `resources` capability at initialize.
    /// Servers that do not advertise it yield empty lists / no resource reads,
    /// mirroring the Bun build's capability gate.
    pub fn resources_advertised(&self) -> bool {
        match self {
            Connection::Stdio(connection) => connection.resources_advertised,
            Connection::Http(connection) => connection.resources_advertised,
        }
    }

    /// List the server's resources (MCP `resources/list`). Raw entries, untagged
    /// (the catalog aggregator tags them with the server name). Servers without
    /// a `resources` capability yield an empty list, like the Bun build.
    pub fn list_resources(&mut self, timeout: Duration) -> Result<Vec<Value>, String> {
        if !self.resources_advertised() {
            return Ok(Vec::new());
        }
        let result = self.request("resources/list", json!({}), timeout)?;
        Ok(result
            .get("resources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// List the server's resource templates (MCP `resources/templates/list`).
    /// Servers that declare `resources` but do not implement template listing
    /// yield an empty list instead of an error (some servers do this).
    pub fn list_resource_templates(&mut self, timeout: Duration) -> Result<Vec<Value>, String> {
        if !self.resources_advertised() {
            return Ok(Vec::new());
        }
        match self.request("resources/templates/list", json!({}), timeout) {
            Ok(result) => Ok(result
                .get("resourceTemplates")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Read one resource (MCP `resources/read`). Returns the raw `contents` array.
    pub fn read_resource(&mut self, uri: &str, timeout: Duration) -> Result<Value, String> {
        let result = self.request("resources/read", json!({ "uri": uri }), timeout)?;
        Ok(result.get("contents").cloned().unwrap_or(json!([])))
    }

    /// Whether the server advertised a `prompts` capability at initialize.
    /// Servers that do not advertise it yield empty lists / no prompt gets,
    /// mirroring the Bun build's capability gate.
    pub fn prompts_advertised(&self) -> bool {
        match self {
            Connection::Stdio(connection) => connection.prompts_advertised,
            Connection::Http(connection) => connection.prompts_advertised,
        }
    }

    /// List the server's prompts (MCP `prompts/list`). Raw entries, untagged
    /// (the catalog aggregator tags them with the server name). Servers without
    /// a `prompts` capability yield an empty list, like the Bun build.
    pub fn list_prompts(&mut self, timeout: Duration) -> Result<Vec<Value>, String> {
        if !self.prompts_advertised() {
            return Ok(Vec::new());
        }
        let result = self.request("prompts/list", json!({}), timeout)?;
        Ok(result
            .get("prompts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Invoke one prompt (MCP `prompts/get`) with positional-ish arguments.
    /// Returns the raw `messages` array.
    pub fn get_prompt(
        &mut self,
        name: &str,
        arguments: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let result = self.request(
            "prompts/get",
            json!({ "name": name, "arguments": arguments }),
            timeout,
        )?;
        Ok(result.get("messages").cloned().unwrap_or(json!([])))
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
    resources_advertised: bool,
    prompts_advertised: bool,
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
            resources_advertised: false,
            prompts_advertised: false,
        };
        let timeout = timeout.min(Duration::from_secs(10));
        connection.handshake(timeout)?;
        Ok(connection)
    }

    fn handshake(&mut self, timeout: Duration) -> Result<(), String> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        self.resources_advertised = result
            .get("capabilities")
            .and_then(|value| value.get("resources"))
            .map(|value| value != &Value::Bool(false))
            .unwrap_or(false);
        self.prompts_advertised = result
            .get("capabilities")
            .and_then(|value| value.get("prompts"))
            .map(|value| value != &Value::Bool(false))
            .unwrap_or(false);
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
    resources_advertised: bool,
    prompts_advertised: bool,
}

impl HttpConnection {
    fn handshake(&mut self, timeout: Duration) -> Result<(), String> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        self.resources_advertised = result
            .get("capabilities")
            .and_then(|value| value.get("resources"))
            .map(|value| value != &Value::Bool(false))
            .unwrap_or(false);
        self.prompts_advertised = result
            .get("capabilities")
            .and_then(|value| value.get("prompts"))
            .map(|value| value != &Value::Bool(false))
            .unwrap_or(false);
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
    /// Every server name from the loaded config, connected or not. Distinguishes
    /// "unknown server" (Bun's MCP.NotFoundError) from "configured but not
    /// currently usable" (Bun's empty catalog / "not connected" ToolFailure).
    configured: Vec<String>,
}

impl Runtime {
    /// Connect to every enabled server and discover tools. Servers that fail to
    /// connect are reported on stderr and skipped (the run continues without them).
    pub fn connect_all(servers: &BTreeMap<String, ServerConfig>, timeout: Duration, db_path: &Path) -> Runtime {
        let mut runtime = Runtime {
            tools: Vec::new(),
            connections: Vec::new(),
            configured: servers.keys().cloned().collect(),
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
                    Err(error) => {
                        // Keep the connection: prompts/resources do not depend on
                        // tools/list (a prompts-only server is common). The run
                        // path simply contributes no tools from this server.
                        eprintln!("MCP server \"{name}\" tools/list failed: {error}");
                        runtime.connections.push((name.clone(), connection));
                    }
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

    /// Aggregate the MCP resource catalog. With `server`, only that server's
    /// catalog is returned; without, the merged catalog across every connected
    /// server (failing servers are skipped, like the Bun build's per-server
    /// fallback to an empty catalog). Errors from a single server propagate
    /// only in the filtered form, matching Bun's `resources({ server })`.
    pub fn resource_catalog(&mut self, server: Option<&str>, timeout: Duration) -> Result<Value, String> {
        match server {
            Some(name) => match self.connections.iter_mut().find(|(entry, _)| entry.as_str() == name) {
                Some((name, connection)) => {
                    let (resources, templates) = catalog_from(connection, name, timeout)?;
                    Ok(json!({ "resources": resources, "templates": templates }))
                }
                None => {
                    if self.configured.iter().any(|entry| entry == name) {
                        Ok(json!({ "resources": [], "templates": [] }))
                    } else {
                        Err(format!("MCP server not found: {name}"))
                    }
                }
            },
            None => {
                let mut resources = Vec::new();
                let mut templates = Vec::new();
                for (name, connection) in &mut self.connections {
                    match catalog_from(connection, name, timeout) {
                        Ok((server_resources, server_templates)) => {
                            resources.extend(server_resources);
                            templates.extend(server_templates);
                        }
                        Err(error) => eprintln!("MCP server \"{name}\" resources/list failed: {error}"),
                    }
                }
                Ok(json!({ "resources": resources, "templates": templates }))
            }
        }
    }

    /// Aggregate the MCP prompt catalog. With `server`, only that server's
    /// catalog is returned; without, the merged catalog across every connected
    /// server (failing servers are skipped, like the Bun build's per-server
    /// fallback to an empty catalog). Errors from a single server propagate
    /// only in the filtered form.
    pub fn prompt_catalog(&mut self, server: Option<&str>, timeout: Duration) -> Result<Value, String> {
        match server {
            Some(name) => match self.connections.iter_mut().find(|(entry, _)| entry.as_str() == name) {
                Some((name, connection)) => {
                    let prompts = prompt_entries(connection, name, timeout)?;
                    Ok(json!({ "prompts": prompts }))
                }
                None => {
                    if self.configured.iter().any(|entry| entry == name) {
                        Ok(json!({ "prompts": [] }))
                    } else {
                        Err(format!("MCP server not found: {name}"))
                    }
                }
            },
            None => {
                let mut prompts = Vec::new();
                for (name, connection) in &mut self.connections {
                    match prompt_entries(connection, name, timeout) {
                        Ok(entries) => prompts.extend(entries),
                        Err(error) => eprintln!("MCP server \"{name}\" prompts/list failed: {error}"),
                    }
                }
                Ok(json!({ "prompts": prompts }))
            }
        }
    }

    /// Invoke one MCP prompt (`prompts/get`). Positional `args` are mapped onto
    /// the server's advertised argument names in order (missing arguments become
    /// `""`), mirroring the Bun command plugin. Returns the joined (newline) and
    /// trimmed text of the returned text message parts. Failures carry the
    /// messages the Bun build produces: an unknown server errors
    /// `MCP server not found: <name>`; everything else (configured-but-unavailable
    /// server, prompts not advertised, prompt missing from the list, or a
    /// `prompts/get` failure) errors `MCP prompt not found: <server>:<name>`.
    pub fn invoke_prompt(
        &mut self,
        server: &str,
        name: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Value, String> {
        let not_found = || format!("MCP prompt not found: {server}:{name}");
        let connection = self
            .connections
            .iter_mut()
            .find(|(entry, _)| entry.as_str() == server)
            .map(|(_, connection)| connection);
        let Some(connection) = connection else {
            return if self.configured.iter().any(|entry| entry == server) {
                Err(not_found())
            } else {
                Err(format!("MCP server not found: {server}"))
            };
        };
        if !connection.prompts_advertised() {
            return Err(not_found());
        }
        let prompts = connection.list_prompts(timeout).unwrap_or_default();
        let entry = prompts
            .iter()
            .find(|prompt| prompt.get("name").and_then(Value::as_str) == Some(name));
        let Some(entry) = entry else {
            return Err(not_found());
        };
        let argument_names: Vec<String> = entry
            .get("arguments")
            .and_then(Value::as_array)
            .map(|arguments| {
                arguments
                    .iter()
                    .filter_map(|argument| argument.get("name").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let arguments: BTreeMap<String, String> = argument_names
            .iter()
            .enumerate()
            .map(|(index, argument_name)| {
                (
                    argument_name.clone(),
                    args.get(index).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let messages = connection.get_prompt(name, &arguments, timeout).map_err(|_| not_found())?;
        let text = messages
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| text_of_content(message.get("content")))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default();
        Ok(json!({ "server": server, "name": name, "text": text }))
    }

    /// Read one MCP resource. Errors carry the exact tool-facing message the
    /// Bun build produces (the runner surfaces them verbatim).
    pub fn read_resource(&mut self, server: &str, uri: &str, timeout: Duration) -> Result<Value, String> {
        match self.connections.iter_mut().find(|(name, _)| name == server) {
            Some((name, connection)) => {
                if !connection.resources_advertised() {
                    return Err(format!(
                        "MCP server \"{server}\" is not connected or does not expose resources"
                    ));
                }
                let contents = connection
                    .read_resource(uri, timeout)
                    .map_err(|error| format!("Unable to read MCP resource {server}:{uri}: {error}"))?;
                Ok(json!({ "server": name, "uri": uri, "contents": contents }))
            }
            None => {
                if self.configured.iter().any(|entry| entry == server) {
                    Err(format!(
                        "MCP server \"{server}\" is not connected or does not expose resources"
                    ))
                } else {
                    Err(format!("MCP server not found: {server}"))
                }
            }
        }
    }
}

/// The server's prompt catalog, tagged with the server name:
/// `{ server, name, description?, title?, arguments? }` per entry.
fn prompt_entries(connection: &mut Connection, server: &str, timeout: Duration) -> Result<Vec<Value>, String> {
    if !connection.prompts_advertised() {
        return Ok(Vec::new());
    }
    Ok(connection
        .list_prompts(timeout)?
        .into_iter()
        .filter_map(|prompt| prompt_entry(server, &prompt))
        .collect())
}

fn prompt_entry(server: &str, prompt: &Value) -> Option<Value> {
    let name = prompt.get("name")?.as_str()?.to_owned();
    let mut entry = json!({ "server": server, "name": name });
    if let Some(title) = prompt.get("title").and_then(Value::as_str) {
        entry["title"] = json!(title);
    }
    if let Some(description) = prompt.get("description").and_then(Value::as_str) {
        entry["description"] = json!(description);
    }
    if let Some(arguments) = prompt.get("arguments").and_then(Value::as_array) {
        let arguments: Vec<Value> = arguments
            .iter()
            .filter_map(|argument| {
                let name = argument.get("name")?.as_str()?.to_owned();
                let mut entry = json!({ "name": name });
                if let Some(description) = argument.get("description").and_then(Value::as_str) {
                    entry["description"] = json!(description);
                }
                if let Some(required) = argument.get("required").and_then(Value::as_bool) {
                    entry["required"] = json!(required);
                }
                Some(entry)
            })
            .collect();
        entry["arguments"] = json!(arguments);
    }
    Some(entry)
}

/// Bun's `promptMessageText`: string contents pass through; text message
/// parts yield `text`; anything else yields "".
fn text_of_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(object)) => {
            if object.get("type").and_then(Value::as_str) != Some("text") {
                return String::new();
            }
            object.get("text").and_then(Value::as_str).unwrap_or_default().to_owned()
        }
        _ => String::new(),
    }
}
/// One server's catalog, tagged with the server name (Bun's `toCatalog`).
fn catalog_from(connection: &mut Connection, server: &str, timeout: Duration) -> Result<(Vec<Value>, Vec<Value>), String> {
    let resources = connection
        .list_resources(timeout)?
        .into_iter()
        .filter_map(|resource| resource_entry(server, &resource))
        .collect();
    let templates = connection
        .list_resource_templates(timeout)?
        .into_iter()
        .filter_map(|template| template_entry(server, &template))
        .collect();
    Ok((resources, templates))
}

fn resource_entry(server: &str, resource: &Value) -> Option<Value> {
    let uri = resource.get("uri")?.as_str()?.to_owned();
    let name = resource.get("name")?.as_str()?.to_owned();
    let mut entry = json!({ "server": server, "name": name, "uri": uri });
    if let Some(description) = resource.get("description").and_then(Value::as_str) {
        entry["description"] = json!(description);
    }
    if let Some(mime_type) = resource.get("mimeType").and_then(Value::as_str) {
        entry["mimeType"] = json!(mime_type);
    }
    Some(entry)
}

fn template_entry(server: &str, template: &Value) -> Option<Value> {
    let uri_template = template.get("uriTemplate")?.as_str()?.to_owned();
    let name = template.get("name")?.as_str()?.to_owned();
    let mut entry = json!({ "server": server, "name": name, "uriTemplate": uri_template });
    if let Some(description) = template.get("description").and_then(Value::as_str) {
        entry["description"] = json!(description);
    }
    if let Some(mime_type) = template.get("mimeType").and_then(Value::as_str) {
        entry["mimeType"] = json!(mime_type);
    }
    Some(entry)
}
