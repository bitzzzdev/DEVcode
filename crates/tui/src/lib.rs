//! Minimal ratatui front-end for the Rust service. Talks to the managed service
//! over the same HTTP/basic-auth contract the Bun TUI uses. Theme colors come
//! from the in-app `Theme` (dark by default); sidebar plugins and session
//! forking are later increments.

use std::io::{Stdout, Write};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use base64::Engine;
use devcode_service as svc;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::{json, Value};

const AUTH_USER: &str = "devcode";
const SESSION_PAGE: &str = "/api/session?order=desc&limit=20";

const SPINNER_FRAMES: [&str; 10] = [
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];

pub struct SessionRow {
    pub id: String,
    pub title: String,
}

/// Messages from the background SSE worker (`GET /api/session/{id}/events`).
/// `Delta` carries one streamed text chunk of the in-flight run; `Ended`
/// marks the terminal `session.execution.*` frame (or a dropped stream).
enum SseMsg {
    Delta(String),
    Ended,
}

pub struct McpRow {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
    pub integration_id: Option<String>,
}

/// The /mcps overlay. The Rust service reports a config-derived status
/// (never connected/failed), and has no connect/disconnect API, so this is
/// a read-only view with a sign-in hint for OAuth-gated servers.
pub struct McpDialog {
    pub servers: Vec<McpRow>,
    pub selected: usize,
    pub detail: bool,
    pub error: Option<String>,
}

/// The /prompts overlay: the merged MCP prompt catalog across connected
/// servers. Enter runs a prompt that takes no arguments immediately, or drops
/// `/server:name ` into the input for the user to complete.
pub struct PromptsDialog {
    pub prompts: Vec<McpPromptEntry>,
    pub selected: usize,
}

/// TUI color theme. Defaults to the dark palette of the JS TUI default devcode
/// theme; later increments resolve user themes from the theme registry.
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Color,
    pub background_panel: Color,
    pub background_element: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool: Color,
    pub status: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::Rgb(10, 10, 10),
            background_panel: Color::Rgb(20, 20, 20),
            background_element: Color::Rgb(30, 30, 30),
            foreground: Color::Rgb(238, 238, 238),
            muted: Color::Rgb(128, 128, 128),
            accent: Color::Rgb(157, 124, 216),
            user: Color::Rgb(92, 156, 245),
            assistant: Color::Rgb(238, 238, 238),
            tool: Color::Rgb(128, 128, 128),
            status: Color::Rgb(128, 128, 128),
            error: Color::Rgb(224, 108, 117),
            warning: Color::Rgb(245, 167, 66),
            success: Color::Rgb(127, 216, 143),
            info: Color::Rgb(86, 182, 194),
        }
    }
}

impl Theme {
    /// Named dark themes ported from the JS TUI assets. Returns None when the
    /// name is unknown, so callers can fall back to the default devcode palette.
    pub fn named(name: &str) -> Option<Theme> {
        let rgb = |r: u8, g: u8, b: u8| Color::Rgb(r, g, b);
        let theme = match name {
            "devcode" => return Some(Theme::default()),
            "dracula" => Theme {
                background: rgb(40, 42, 54),
                background_panel: rgb(50, 52, 66),
                background_element: rgb(68, 71, 90),
                foreground: rgb(248, 248, 242),
                muted: rgb(98, 114, 164),
                accent: rgb(139, 233, 253),
                user: rgb(189, 147, 249),
                assistant: rgb(248, 248, 242),
                tool: rgb(98, 114, 164),
                status: rgb(98, 114, 164),
                error: rgb(255, 85, 85),
                warning: rgb(241, 250, 140),
                success: rgb(80, 250, 123),
                info: rgb(255, 184, 108),
            },
            "catppuccin" => Theme {
                background: rgb(30, 30, 46),
                background_panel: rgb(37, 37, 56),
                background_element: rgb(49, 50, 68),
                foreground: rgb(205, 214, 244),
                muted: rgb(147, 153, 178),
                accent: rgb(245, 194, 231),
                user: rgb(137, 180, 250),
                assistant: rgb(205, 214, 244),
                tool: rgb(147, 153, 178),
                status: rgb(147, 153, 178),
                error: rgb(243, 139, 168),
                warning: rgb(249, 226, 175),
                success: rgb(166, 227, 161),
                info: rgb(148, 226, 213),
            },
            "gruvbox" => Theme {
                background: rgb(40, 40, 40),
                background_panel: rgb(50, 48, 47),
                background_element: rgb(60, 56, 54),
                foreground: rgb(235, 219, 178),
                muted: rgb(146, 131, 116),
                accent: rgb(142, 192, 124),
                user: rgb(131, 165, 152),
                assistant: rgb(235, 219, 178),
                tool: rgb(146, 131, 116),
                status: rgb(146, 131, 116),
                error: rgb(251, 73, 52),
                warning: rgb(254, 128, 25),
                success: rgb(184, 187, 38),
                info: rgb(250, 189, 47),
            },
            "tokyonight" => Theme {
                background: rgb(26, 27, 38),
                background_panel: rgb(35, 36, 49),
                background_element: rgb(45, 46, 62),
                foreground: rgb(200, 211, 245),
                muted: rgb(130, 139, 184),
                accent: rgb(255, 150, 108),
                user: rgb(130, 170, 255),
                assistant: rgb(200, 211, 245),
                tool: rgb(130, 139, 184),
                status: rgb(130, 139, 184),
                error: rgb(255, 117, 127),
                warning: rgb(255, 150, 108),
                success: rgb(195, 232, 141),
                info: rgb(130, 170, 255),
            },
            _ => return None,
        };
        Some(theme)
    }
}

/// Named theme identifiers selectable with `/theme`.
pub const THEME_NAMES: [&str; 5] = ["devcode", "dracula", "catppuccin", "gruvbox", "tokyonight"];

pub struct App {
    base: String,
    theme: Theme,
    theme_name: String,
    auth: String,
    sessions: Vec<SessionRow>,
    selected: usize,
    transcript: Vec<String>,
    scroll: u16,
    viewport_h: u16,
    input: String,
    model: Option<String>,
    status: String,
    status_err: bool,
    mcp: Option<McpDialog>,
    prompts: Option<PromptsDialog>,
    // merged prompt catalog cache for the `/server:name args` submit path;
    // populated on /prompts open, or lazily on first candidate submit
    mcp_prompt_cache: Option<Vec<McpPromptEntry>>,
    // number of messages last seen for the selected session; poll rebuilds on change
    seen_len: usize,
    last_sessions_refresh: Instant,
    last_messages_refresh: Instant,
    // provisional assistant text of the in-flight run (from the SSE delta
    // stream); reconciled by the message poll, cleared on rebuild
    stream_buffer: String,
    spinner_frame: u8,
    sse_rx: Option<std::sync::mpsc::Receiver<SseMsg>>,
}

pub fn run() -> ExitCode {
    let paths = devcode_util::paths("devcode");
    let registration_path = paths.state.join("service.json");
    let registration = match ensure_service(&paths, &registration_path) {
        Some(registration) => registration,
        None => return ExitCode::FAILURE,
    };
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{AUTH_USER}:{}", registration.password).as_bytes())
    );
    let base = registration.url.trim_end_matches('/').to_owned();
    let mut app = App {
        base,
        theme: Theme::default(),
        theme_name: "devcode".to_owned(),
        auth,
        sessions: Vec::new(),
        selected: 0,
        transcript: vec!["no sessions yet: type a message and press Enter".to_owned()],
        scroll: 0,
        viewport_h: 1,
        input: String::new(),
        model: std::env::var("DEVCODE_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        status: String::new(),
        status_err: false,
        mcp: None,
        prompts: None,
        mcp_prompt_cache: None,
        seen_len: usize::MAX,
        last_sessions_refresh: Instant::now() - Duration::from_secs(60),
        last_messages_refresh: Instant::now() - Duration::from_secs(60),
        stream_buffer: String::new(),
        spinner_frame: 0,
        sse_rx: None,
    };
    app.refresh_sessions();
    app.refresh_messages(true);

    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use crossterm::ExecutableCommand;
    let mut stdout = std::io::stdout();
    if enable_raw_mode().is_err() || stdout.execute(EnterAlternateScreen).is_err() {
        eprintln!("failed to enter the terminal screen");
        return ExitCode::FAILURE;
    }
    let outcome = event_loop(&mut app, &mut stdout);
    let _ = disable_raw_mode();
    let _ = stdout.execute(LeaveAlternateScreen);
    let mut stdout = std::io::stdout();
    let _ = stdout.flush();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("terminal error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn ensure_service(
    paths: &devcode_util::Paths,
    registration_path: &std::path::Path,
) -> Option<svc::Registration> {
    if let Some(registration) = svc::Registration::read(registration_path) {
        if svc::probe(&registration, 800) == svc::Health::Ready {
            return Some(registration);
        }
    }
    // Ask `service start` to handle discovery/spawn/incumbent rules; it detaches
    // the server and waits for readiness.
    let exe = std::env::current_exe().ok()?;
    let status = Command::new(exe)
        .args(["service", "start"])
        .stdin(std::process::Stdio::null())
        .status();
    if status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("failed to start the managed service (DEVcode service start)");
        return None;
    }
    let registration = svc::Registration::read(registration_path)?;
    for _ in 0..50 {
        if svc::probe(&registration, 200) == svc::Health::Ready {
            return Some(registration);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = paths;
    eprintln!("the managed service did not become ready in time");
    None
}

fn http_get(client: &App, path: &str) -> Result<Value, String> {
    http_get_with_timeout(client, path, Duration::from_secs(10))
}

fn http_get_with_timeout(client: &App, path: &str, timeout: Duration) -> Result<Value, String> {
    let response = ureq::get(&format!("{}{}", client.base, path))
        .set("authorization", &client.auth)
        .timeout(timeout)
        .call()
        .map_err(http_error)?;
    response.into_json::<Value>().map_err(|e| e.to_string())
}

fn http_post(client: &App, path: &str, body: &Value) -> Result<Value, String> {
    http_post_with_timeout(client, path, body, Duration::from_secs(10))
}

fn http_post_with_timeout(
    client: &App,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let response = ureq::post(&format!("{}{}", client.base, path))
        .set("authorization", &client.auth)
        .set("content-type", "application/json")
        .timeout(timeout)
        .send_json(body.clone())
        .map_err(http_error)?;
    response.into_json::<Value>().map_err(|e| e.to_string())
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_json::<Value>().unwrap_or(Value::Null);
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| body.to_string());
            format!("HTTP {code}: {message}")
        }
        ureq::Error::Transport(transport) => format!("service unreachable: {transport}"),
    }
}

/// Parse the /api/mcp wire into rows. Pure; tested.
pub fn mcp_rows(body: &Value) -> Vec<McpRow> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let status = row
                        .get("status")
                        .and_then(|status| status.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    McpRow {
                        name: row
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        error: row
                            .get("status")
                            .and_then(|status| status.get("error"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        status,
                        integration_id: row
                            .get("integrationID")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn mcp_status_label(status: &str) -> &'static str {
    match status {
        "connected" => "Connected ✓",
        "pending" => "Connecting…",
        "failed" => "Failed !",
        "disabled" => "Disabled ○",
        "needs_auth" => "Sign in required →",
        _ => "Unknown",
    }
}

/// Render the message list wire into plain transcript lines. Pure; tested.
pub fn transcript_lines(messages: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for message in messages {
        match message.get("type").and_then(Value::as_str).unwrap_or("") {
            "user" => {
                let text = message
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for (index, line) in text.lines().enumerate() {
                    lines.push(if index == 0 {
                        format!("you › {line}")
                    } else {
                        format!("     {line}")
                    });
                }
            }
            "assistant" => {
                if let Some(content) = message.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                for line in part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .lines()
                                {
                                    lines.push(format!("assistant › {line}"));
                                }
                            }
                            Some("tool") => {
                                let name =
                                    part.get("name").and_then(Value::as_str).unwrap_or("tool");
                                let status = part
                                    .get("state")
                                    .and_then(|state| state.get("status"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown");
                                lines.push(format!("  [{name}] {status}"));
                            }
                            _ => {}
                        }
                    }
                } else if let Some(text) = message.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        lines.push(format!("assistant › {text}"));
                    }
                }
            }
            _ => {} // system/snapshot/model-switched rows: nothing to show
        }
    }
    if lines.is_empty() {
        lines.push("(no messages yet)".to_owned());
    }
    lines
}

/// Split one assistant prose line into styled markdown spans: headings,
/// bold text and inline code. Pure; tested.
fn markdown_line_spans(line: &str, theme: &Theme) -> Vec<(String, Style)> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        let text = trimmed.trim_start_matches('#').trim_start().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        return vec![(text, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))];
    }
    let parts: Vec<&str> = line.split('`').collect();
    let mut spans: Vec<(String, Style)> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index % 2 == 1 {
            spans.push((part.to_string(), Style::default().fg(theme.success)));
            continue;
        }
        spans.extend(text_spans(part, theme));
    }
    if spans.is_empty() {
        spans.push((line.to_owned(), Style::default().fg(theme.assistant)));
    }
    spans
}

fn text_spans(text: &str, theme: &Theme) -> Vec<(String, Style)> {
    let mut spans: Vec<(String, Style)> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(open) = rest.find("**") {
            let Some(closing) = rest[open + 2..].find("**").map(|i| open + 2 + i + 2) else {
                break;
            };
            let content_end = closing - 2;
            let before = rest[..open].to_owned();
            if !before.is_empty() {
                spans.push((before, Style::default().fg(theme.foreground)));
            }
            let bold_text = rest[open + 2..content_end].to_owned();
            if !bold_text.is_empty() {
                spans.push((bold_text, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
            }
            rest = &rest[closing..];
            continue;
        }
        spans.push((rest.to_owned(), Style::default().fg(theme.foreground)));
        break;
    }
    spans
}

/// Parse the input line. Returns (command, argument). "" command = plain prompt.
pub fn parse_command(input: &str) -> (&str, &str) {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('/') {
        match rest.split_once(' ') {
            Some((command, argument)) => (command, argument.trim()),
            None => (rest, ""),
        }
    } else {
        ("", trimmed)
    }
}

/// One MCP prompt from the merged catalog (Bun's `Mcp.Prompt & { server }`).
#[derive(Clone, Debug, PartialEq)]
pub struct McpPromptEntry {
    pub server: String,
    pub name: String,
    pub description: Option<String>,
    /// Argument names in advertised order; empty when the prompt takes none.
    pub arguments: Vec<String>,
}

/// Sanitize one command-name part (Bun: [^a-zA-Z0-9_-] -> `_`).
pub fn sanitize_command_part(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// The Bun slash-command name for an MCP prompt: `server:prompt`, both sanitized.
pub fn mcp_prompt_command_name(server: &str, name: &str) -> String {
    format!("{}:{}", sanitize_command_part(server), sanitize_command_part(name))
}

/// Parse positional prompt arguments from an argument string, mirroring Bun's
/// `argsRegex` + `quoteTrimRegex`: `[Image N]` tokens, "double quoted",
/// 'single quoted' or bare runs, with quotes trimmed. Unterminated quotes
/// match nothing and are ignored (Bun behaves the same).
pub fn parse_prompt_arguments(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut args = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        if character == '[' {
            if let Some(length) = match_image_token(&chars[index..]) {
                args.push(chars[index..index + length].iter().collect());
                index += length;
                continue;
            }
        }
        if character == '"' || character == '\'' {
            let quote = character;
            let mut cursor = index + 1;
            while cursor < chars.len() && chars[cursor] != quote {
                cursor += 1;
            }
            if cursor < chars.len() {
                args.push(chars[index + 1..cursor].iter().collect());
                index = cursor + 1;
            } else {
                // Unterminated quote: Bun's regex matches nothing at the quote
                // itself; skip it and let the run read as a bare token.
                index += 1;
            }
            continue;
        }
        let start = index;
        while index < chars.len()
            && !chars[index].is_whitespace()
            && chars[index] != '"'
            && chars[index] != '\''
        {
            index += 1;
        }
        args.push(chars[start..index].iter().collect());
    }
    args
}

/// Case-insensitive match of `[Image\s+\d+]` at the start of `chars`; returns its length.
fn match_image_token(chars: &[char]) -> Option<usize> {
    let rest: String = chars.iter().collect();
    let lower = rest.to_ascii_lowercase();
    let prefix = "[image";
    if !lower.starts_with(prefix) {
        return None;
    }
    let mut index = prefix.len();
    let mut saw_space = false;
    while index < rest.len() && rest.as_bytes()[index].is_ascii_whitespace() {
        saw_space = true;
        index += 1;
    }
    if !saw_space {
        return None;
    }
    let digit_start = index;
    while index < rest.len() && rest.as_bytes()[index].is_ascii_digit() {
        index += 1;
    }
    if index == digit_start {
        return None;
    }
    if index < rest.len() && rest.as_bytes()[index] == b']' {
        return Some(index + 1);
    }
    None
}

/// Parse the /api/mcp/prompts wire into entries. Pure; tested.
pub fn mcp_prompt_rows(body: &Value) -> Vec<McpPromptEntry> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let server = row.get("server")?.as_str()?.to_owned();
                    let name = row.get("name")?.as_str()?.to_owned();
                    let arguments = row
                        .get("arguments")
                        .and_then(Value::as_array)
                        .map(|arguments| {
                            arguments
                                .iter()
                                .filter_map(|argument| {
                                    argument.get("name").and_then(Value::as_str).map(str::to_owned)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(McpPromptEntry {
                        server,
                        name,
                        description: row
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl App {
    fn set_status(&mut self, message: String, err: bool) {
        self.status = message;
        self.status_err = err;
    }

    fn selected_id(&self) -> Option<String> {
        self.sessions
            .get(self.selected)
            .map(|session| session.id.clone())
    }

    fn refresh_sessions(&mut self) {
        match http_get(self, SESSION_PAGE) {
            Ok(body) => {
                let selected_id = self.selected_id();
                self.sessions = body
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .map(|row| SessionRow {
                                id: row
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                title: row
                                    .get("title")
                                    .and_then(Value::as_str)
                                    .filter(|title| !title.is_empty())
                                    .unwrap_or_else(|| {
                                        row.get("id").and_then(Value::as_str).unwrap_or("?")
                                    })
                                    .chars()
                                    .take(40)
                                    .collect(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.selected = selected_id
                    .and_then(|id| self.sessions.iter().position(|s| s.id == id))
                    .unwrap_or(0);
                self.last_sessions_refresh = Instant::now();
            }
            Err(error) => self.set_status(error, true),
        }
    }

    fn refresh_messages(&mut self, force: bool) {
        let Some(id) = self.selected_id() else {
            self.transcript = vec!["no sessions yet: type a message and press Enter".to_owned()];
            self.seen_len = 0;
            return;
        };
        match http_get(
            self,
            &format!("/api/session/{id}/message?order=asc&limit=200"),
        ) {
            Ok(body) => {
                let rows = body
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if force || rows.len() != self.seen_len {
                    self.transcript = transcript_lines(&rows);
                    self.seen_len = rows.len();
                    self.scroll = self.max_scroll();
                    self.stream_buffer.clear();
                }
                self.last_messages_refresh = Instant::now();
            }
            Err(error) => self.set_status(error, true),
        }
    }

    /// Drain every pending SSE message for the selected run.
    fn drain_sse(&mut self) {
        let mut ended = false;
        if let Some(rx) = &self.sse_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    SseMsg::Delta(text) => {
                        self.stream_buffer.push_str(&text);
                        // Defensive cap: a runaway stream must not grow the
                        // transcript without bound between polls.
                        if self.stream_buffer.len() > 256 * 1024 {
                            self.stream_buffer.clear();
                        }
                    }
                    SseMsg::Ended => ended = true,
                }
            }
        }
        if ended {
            self.sse_rx = None;
            // Force a full rebuild on the next poll; the buffer is cleared there.
            self.seen_len = usize::MAX;
        }
    }

    /// Open the SSE stream for a freshly admitted run; deltas accumulate in
    /// `stream_buffer` and render as a provisional assistant block. The
    /// stream closes itself at the run's terminal event; the 750 ms message
    /// poll remains the reconciliation path.
    fn start_sse(&mut self, session_id: String) {
        self.stream_buffer.clear();
        let (tx, rx) = std::sync::mpsc::channel::<SseMsg>();
        self.sse_rx = Some(rx);
        let base = self.base.clone();
        let auth = self.auth.clone();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let url = format!("{base}/api/session/{session_id}/events");
            let result = ureq::get(&url)
                .set("authorization", &auth)
                .set("accept", "text/event-stream")
                .call();
            let Ok(response) = result else {
                let _ = tx.send(SseMsg::Ended);
                return;
            };
            let mut reader = std::io::BufReader::new(response.into_reader());
            let mut event = String::new();
            let mut data = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            if !event.is_empty() {
                                if event == "delta" {
                                    if let Some(text) = serde_json::from_str::<Value>(&data)
                                        .ok()
                                        .and_then(|value| {
                                            value.get("text").and_then(Value::as_str).map(str::to_owned)
                                        })
                                    {
                                        if tx.send(SseMsg::Delta(text)).is_err() {
                                            return;
                                        }
                                    }
                                } else if event.starts_with("session.execution.") {
                                    if tx.send(SseMsg::Ended).is_err() {
                                        return;
                                    }
                                }
                            }
                            event.clear();
                            data.clear();
                            continue;
                        }
                        if let Some(value) = trimmed.strip_prefix("event:") {
                            event = value.trim().to_owned();
                            continue;
                        }
                        if let Some(value) = trimmed.strip_prefix("data:") {
                            if !data.is_empty() {
                                data.push('\n');
                            }
                            data.push_str(value.trim());
                        }
                        // comment (keepalive) and other lines are ignored
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(SseMsg::Ended);
        });
    }

    fn max_scroll(&self) -> u16 {
        self.transcript
            .len()
            .saturating_sub(self.viewport_h as usize) as u16
    }

    fn create_session(&mut self) -> Option<String> {
        match http_post(self, "/api/session", &json!({})) {
            Ok(body) => {
                let id = body
                    .get("data")
                    .and_then(|data| data.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.refresh_sessions();
                if let Some(id) = &id {
                    if let Some(position) = self.sessions.iter().position(|s| &s.id == id) {
                        self.selected = position;
                    }
                    self.seen_len = usize::MAX;
                    self.refresh_messages(true);
                }
                id
            }
            Err(error) => {
                self.set_status(error, true);
                None
            }
        }
    }

    fn send(&mut self) {
        let (command, argument) = {
            let (command, argument) = parse_command(&self.input);
            (command.to_owned(), argument.to_owned())
        };
        match command.as_str() {
            "quit" | "exit" => return, // handled by caller via should_quit
            "new" => {
                self.input.clear();
                self.create_session();
                return;
            }
            "mcps" | "mcp" => {
                self.input.clear();
                self.open_mcp();
                return;
            }
            "prompts" => {
                self.input.clear();
                self.open_prompts();
                return;
            }
            "model" => {
                if argument.is_empty() {
                    self.set_status(
                        format!("model: {}", self.model.as_deref().unwrap_or("unset")),
                        false,
                    );
                } else {
                    self.model = Some(argument.to_owned());
                    self.set_status(format!("model set to {argument}"), false);
                }
                self.input.clear();
                return;
            }
            "theme" | "themes" => {
                if argument.is_empty() {
                    let current = self.theme_name.clone();
                    let next = THEME_NAMES
                        .iter()
                        .skip_while(|name| **name != current)
                        .nth(1)
                        .copied()
                        .unwrap_or(THEME_NAMES[0]);
                    self.theme = Theme::named(next).unwrap_or_default();
                    self.theme_name = next.to_owned();
                    self.set_status(format!("theme: {next}"), false);
                } else {
                    let next = THEME_NAMES
                        .iter()
                        .find(|name| name.eq_ignore_ascii_case(&argument))
                        .copied();
                    let Some(next) = next else {
                        self.set_status(format!("unknown theme: {argument}"), true);
                        self.input.clear();
                        return;
                    };
                    self.theme = Theme::named(next).unwrap_or_default();
                    self.theme_name = next.to_owned();
                    self.set_status(format!("theme set to {next}"), false);
                }
                self.input.clear();
                return;
            }
            _ => {}
        }
        // MCP prompt slash commands (Bun: `/server:prompt args...`).
        if command.contains(':') && self.resolve_mcp_prompt_command(&command, &argument) {
            return;
        }
        let text = argument.to_owned();
        if text.is_empty() {
            if !command.is_empty() {
                self.set_status(format!("unknown command: /{command}"), true);
                self.input.clear();
            }
            return;
        }
        self.input.clear();
        self.submit_text(&text);
    }

    /// The merged MCP prompt catalog, cached in memory. Lazily fetches on a
    /// cache miss; returns None (status set on error) when the fetch fails.
    fn mcp_prompt_catalog(&mut self) -> Option<Vec<McpPromptEntry>> {
        if let Some(cache) = &self.mcp_prompt_cache {
            return Some(cache.clone());
        }
        match http_get_with_timeout(self, "/api/mcp/prompts", Duration::from_secs(30)) {
            Ok(body) => {
                let prompts = mcp_prompt_rows(&body);
                self.mcp_prompt_cache = Some(prompts.clone());
                Some(prompts)
            }
            Err(error) => {
                self.set_status(error, true);
                None
            }
        }
    }

    /// Resolve `command` against the cached catalog (`sanitized_server:sanitized_prompt`),
    /// invoke the prompt with the positional arguments in `argument`, and submit
    /// the resulting text as a user message. Returns true when the command was
    /// handled (or failed with a status message).
    fn resolve_mcp_prompt_command(&mut self, command: &str, argument: &str) -> bool {
        let Some(catalog) = self.mcp_prompt_catalog() else {
            self.input.clear();
            return true; // error already shown
        };
        let Some(entry) = catalog
            .into_iter()
            .find(|entry| mcp_prompt_command_name(&entry.server, &entry.name) == command)
        else {
            return false; // not an MCP prompt command
        };
        self.input.clear();
        let args = parse_prompt_arguments(argument);
        match self.invoke_mcp_prompt(&entry, args) {
            Ok(text) => {
                let text = text.trim().to_owned();
                if text.is_empty() {
                    self.set_status("prompt returned no text".to_owned(), true);
                } else {
                    self.submit_text(&text);
                }
            }
            Err(error) => self.set_status(error, true),
        }
        true
    }

    fn invoke_mcp_prompt(
        &self,
        entry: &McpPromptEntry,
        args: Vec<String>,
    ) -> Result<String, String> {
        let body = json!({ "server": entry.server, "name": entry.name, "args": args });
        let response =
            http_post_with_timeout(self, "/api/mcp/prompts/get", &body, Duration::from_secs(30))?;
        Ok(response
            .get("data")
            .and_then(|data| data.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn open_prompts(&mut self) {
        match http_get_with_timeout(self, "/api/mcp/prompts", Duration::from_secs(30)) {
            Ok(body) => {
                let prompts = mcp_prompt_rows(&body);
                self.mcp_prompt_cache = Some(prompts.clone());
                self.prompts = Some(PromptsDialog {
                    prompts,
                    selected: 0,
                });
            }
            Err(error) => self.set_status(error, true),
        }
    }

    fn submit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let id = match self.selected_id() {
            Some(id) => id,
            None => match self.create_session() {
                Some(id) => id,
                None => return,
            },
        };
        let mut body = json!({ "text": text, "resume": true });
        if let Some(model) = &self.model {
            body["model"] = json!(model);
        }
        match http_post(self, &format!("/api/session/{id}/prompt"), &body) {
            Ok(_) => {
                self.set_status("sent".to_owned(), false);
                self.last_messages_refresh = Instant::now() - Duration::from_secs(60);
                self.start_sse(id);
            }
            Err(error) => self.set_status(error, true),
        }
    }

    fn open_mcp(&mut self) {
        match http_get(self, "/api/mcp") {
            Ok(body) => {
                self.mcp = Some(McpDialog {
                    servers: mcp_rows(&body),
                    selected: 0,
                    detail: false,
                    error: None,
                });
            }
            Err(error) => self.set_status(error, true),
        }
    }

    fn should_quit(&self) -> bool {
        matches!(parse_command(&self.input).0, "quit" | "exit")
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(20)])
        .split(root[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(main[1]);

    // sidebar
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|session| ListItem::new(session.title.clone()))
        .collect();
    let mut list_state = ListState::default();
    if !app.sessions.is_empty() {
        list_state.select(Some(app.selected));
    }
    let sidebar = List::new(items)
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .title(" sessions ")
                .border_style(Style::default().fg(app.theme.background_element).bg(app.theme.background))
                .style(Style::default().bg(app.theme.background)),
        )
        .highlight_style(
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");
    frame.render_stateful_widget(sidebar, main[0], &mut list_state);

    // transcript
    app.viewport_h = right[0].height.saturating_sub(2).max(1);
    let mut lines: Vec<Line> = app
        .transcript
        .iter()
        .map(|line| {
            let style = if line.starts_with("you ›") {
                // user rows sit on the raised panel background like the JS frame
                Style::default().fg(app.theme.user).bg(app.theme.background_panel)
            } else if line.starts_with("assistant ›") {
                Style::default().fg(app.theme.assistant).bg(app.theme.background)
            } else if let Some(close) = line.find(']') {
                // tool rows: "[name] status" -> color the row by outcome
                let status = line[close + 1..].trim();
                let fg = if status == "error" || status == "failed" {
                    app.theme.error
                } else {
                    app.theme.tool
                };
                Style::default().fg(fg).bg(app.theme.background)
            } else {
                Style::default().fg(app.theme.foreground).bg(app.theme.background)
            };
            if line.starts_with("assistant ›") {
                let text = line.strip_prefix("assistant ›").unwrap_or(line).trim_start();
                let mut spans = vec![Span::styled("assistant ›".to_owned(), style)];
                spans.extend(
                    markdown_line_spans(text, &app.theme)
                        .into_iter()
                        .map(|(content, content_style)| Span::styled(content, content_style)),
                );
                return Line::from(spans);
            }
            Line::from(Span::styled(line.clone(), style))
        })
        .collect();
    if !app.stream_buffer.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "assistant › (streaming)",
            Style::default().fg(app.theme.assistant),
        )));
        for text in app.stream_buffer.lines() {
            lines.push(Line::from(Span::styled(
                text.to_owned(),
                Style::default().fg(app.theme.foreground),
            )));
        }
        lines.push(Line::from(Span::styled(
            SPINNER_FRAMES[app.spinner_frame as usize].to_owned(),
            Style::default().fg(app.theme.accent),
        )));
    }
    let max_scroll = (lines.len().saturating_sub(app.viewport_h as usize)) as u16;
    if !app.stream_buffer.is_empty() {
        // follow the tail while streaming
        app.scroll = max_scroll;
    }
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }
    let transcript = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().bg(app.theme.background))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(transcript, right[0]);

    // input
    let input = Paragraph::new(format!("> {}", app.input))
        .block(Block::default().borders(Borders::TOP).title(" message "))
        .style(Style::default().fg(app.theme.foreground).bg(app.theme.background));
    frame.render_widget(input, right[1]);
    let cursor_x = right[1].x + 2 + app.input.chars().count() as u16 + 1;
    frame.set_cursor_position((
        cursor_x.min(right[1].x + right[1].width.saturating_sub(1)),
        right[1].y + 1,
    ));

    // status bar
    let model = app
        .model
        .as_deref()
        .unwrap_or("model unset (/model provider/model)");
    let help = " ↑↓ sessions · Enter send · /new /mcps /prompts · PgUp/PgDn scroll · Ctrl-C quit";
    let status_line = if app.status.is_empty() {
        format!(" {model}{help}")
    } else {
        format!(" {} | {model}{help}", app.status)
    };
    frame.render_widget(
        Paragraph::new(status_line)
            .style(Style::default().fg(if app.status_err { app.theme.error } else { app.theme.status }).bg(app.theme.background)),
        root[1],
    );

    if app.mcp.is_some() {
        draw_mcp(frame, app);
    }
    if app.prompts.is_some() {
        draw_prompts(frame, app);
    }
}

fn draw_prompts(frame: &mut Frame<'_>, app: &mut App) {
    let Some(dialog) = &app.prompts else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(4).clamp(36, 76);
    let height = area.height.saturating_sub(4).clamp(8, 20);
    let rect = ratatui::layout::Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" MCP prompts ({}) ", dialog.prompts.len()))
        .border_style(Style::default().fg(app.theme.background_element).bg(app.theme.background))
        .style(Style::default().bg(app.theme.background));

    if dialog.prompts.is_empty() {
        let empty = Paragraph::new(
            "No MCP prompts found.\n\nServers declare prompts with a prompts capability. Esc to close.",
        )
        .block(block)
        .wrap(Wrap { trim: false });
        frame.render_widget(empty, rect);
        return;
    }

    let items: Vec<ListItem> = dialog
        .prompts
        .iter()
        .map(|entry| {
            let command = mcp_prompt_command_name(&entry.server, &entry.name);
            let args = entry.arguments.join(" ");
            let description = entry.description.clone().unwrap_or_default();
            let mut spans = vec![Span::styled(
                format!("/{command}"),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )];
            if !args.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(args, Style::default().fg(app.theme.foreground)));
            }
            if !description.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(description, Style::default().fg(app.theme.muted)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(
        dialog.selected.min(dialog.prompts.len().saturating_sub(1)),
    ));
    let list = List::new(items)
        .block(block.title_bottom(" ↑↓ select · Enter run · Esc close "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");
    frame.render_stateful_widget(list, rect, &mut list_state);
}

fn handle_prompts_key(app: &mut App, code: crossterm::event::KeyCode) {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Esc => app.prompts = None,
        KeyCode::Up | KeyCode::Down => {
            let delta: isize = if code == KeyCode::Up { -1 } else { 1 };
            if let Some(dialog) = app.prompts.as_mut() {
                if dialog.prompts.is_empty() {
                    return;
                }
                let last = dialog.prompts.len() as isize - 1;
                dialog.selected = (dialog.selected as isize + delta).clamp(0, last) as usize;
            }
        }
        KeyCode::Enter => {
            let entry = app
                .prompts
                .as_ref()
                .and_then(|dialog| dialog.prompts.get(dialog.selected).cloned());
            let Some(entry) = entry else {
                return;
            };
            if entry.arguments.is_empty() {
                // No arguments: run it now and send the result as a user message.
                app.prompts = None;
                match app.invoke_mcp_prompt(&entry, Vec::new()) {
                    Ok(text) => {
                        let text = text.trim().to_owned();
                        if text.is_empty() {
                            app.set_status("prompt returned no text".to_owned(), true);
                        } else {
                            app.submit_text(&text);
                        }
                    }
                    Err(error) => app.set_status(error, true),
                }
            } else {
                // Needs arguments: drop `/server:name ` into the input for the
                // user to complete, like the Bun command palette.
                app.input = format!(
                    "/{} ",
                    mcp_prompt_command_name(&entry.server, &entry.name)
                );
                app.prompts = None;
            }
        }
        KeyCode::Backspace => {}
        _ => {}
    }
}

fn draw_mcp(frame: &mut Frame<'_>, app: &mut App) {
    let Some(dialog) = &app.mcp else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(4).clamp(36, 68);
    let height = area.height.saturating_sub(4).clamp(8, 20);
    let rect = ratatui::layout::Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" MCP servers ({}) ", dialog.servers.len()))
        .border_style(Style::default().fg(app.theme.background_element).bg(app.theme.background))
        .style(Style::default().bg(app.theme.background));

    if dialog.detail {
        let content = mcp_detail_text(dialog);
        let body = Paragraph::new(content)
            .block(block.title(format!(" MCP server: {} ", dialog.servers[dialog.selected].name)))
            .wrap(Wrap { trim: false });
        frame.render_widget(body, rect);
        return;
    }

    if dialog.servers.is_empty() {
        let empty = Paragraph::new(
            "No MCP servers configured.\n\nAdd [mcp.servers] entries to devcode.jsonc, or install an MCP plugin.\nEsc to close.",
        )
        .block(block)
        .wrap(Wrap { trim: false });
        frame.render_widget(empty, rect);
        return;
    }

    let items: Vec<ListItem> = dialog
        .servers
        .iter()
        .map(|server| {
            let dot_color = match server.status.as_str() {
                "connected" => app.theme.success,
                "pending" => app.theme.warning,
                "failed" => app.theme.error,
                "needs_auth" => app.theme.warning,
                "disabled" => app.theme.muted,
                _ => app.theme.muted,
            };
            let label_color = match server.status.as_str() {
                "failed" => app.theme.error,
                "needs_auth" => app.theme.warning,
                "connected" => app.theme.success,
                _ => app.theme.muted,
            };
            ListItem::new(Line::from(vec![
                Span::styled("● ", Style::default().fg(dot_color)),
                Span::styled(server.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(mcp_status_label(&server.status), Style::default().fg(label_color)),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(dialog.selected.min(dialog.servers.len().saturating_sub(1))));
    let list = List::new(items)
        .block(block.title_bottom(" ↑↓ select · Enter details · Esc close "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");
    frame.render_stateful_widget(list, rect, &mut list_state);
}

fn mcp_detail_text(dialog: &McpDialog) -> String {
    let Some(server) = dialog.servers.get(dialog.selected) else {
        return String::new();
    };
    let status_label = match server.status.as_str() {
        "connected" => "Connected",
        "pending" => "Connecting",
        "failed" => "Failed",
        "disabled" => "Disabled",
        "needs_auth" => "Sign in required",
        other => other,
    };
    let mut lines = vec![format!("Status: {status_label}")];
    if let Some(integration_id) = &server.integration_id {
        lines.push(format!("Integration: {integration_id}"));
    }
    if dialog.error.as_deref().is_some_and(|error| !error.is_empty()) {
        lines.push(String::new());
        lines.push(dialog.error.clone().unwrap_or_default());
    } else if let Some(error) = &server.error {
        lines.push(String::new());
        lines.push(error.clone());
    }
    if server.status == "needs_auth" {
        lines.push(String::new());
        lines.push(format!("Run: DEVcode mcp auth {}", server.name));
    } else if server.status == "pending" {
        lines.push(String::new());
        lines.push("Rust MCP connections are opened per run.".to_owned());
    }
    lines.push(String::new());
    lines.push("Esc to go back".to_owned());
    lines.join("\n")
}

fn handle_mcp_key(app: &mut App, code: crossterm::event::KeyCode) {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Esc => {
            if app.mcp.as_ref().is_some_and(|dialog| dialog.detail) {
                if let Some(dialog) = app.mcp.as_mut() {
                    dialog.detail = false;
                }
            } else {
                app.mcp = None;
            }
        }
        KeyCode::Up | KeyCode::Down => {
            let delta: isize = if code == KeyCode::Up { -1 } else { 1 };
            if let Some(dialog) = app.mcp.as_mut() {
                if dialog.detail || dialog.servers.is_empty() {
                    return;
                }
                let last = dialog.servers.len() as isize - 1;
                dialog.selected = (dialog.selected as isize + delta).clamp(0, last) as usize;
            }
        }
        KeyCode::Enter => {
            if let Some(dialog) = app.mcp.as_mut() {
                if dialog.servers.is_empty() {
                    return;
                }
                dialog.detail = !dialog.detail;
            }
        }
        KeyCode::Backspace => {
            if let Some(dialog) = app.mcp.as_mut() {
                if dialog.detail {
                    dialog.detail = false;
                }
            }
        }
        _ => {}
    }
}

fn event_loop(app: &mut App, stdout: &mut Stdout) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{Event, KeyCode, KeyModifiers};

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        terminal.draw(|frame| draw(frame, app))?;

        let poll_timeout = Duration::from_millis(100);
        if crossterm::event::poll(poll_timeout)? {
            match crossterm::event::read()? {
                Event::Key(key) => {
                    use crossterm::event::KeyEventKind;
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let quit = matches!(key.code, KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL));
                    if quit {
                        return Ok(())
                    }
                    if app.prompts.is_some() {
                        handle_prompts_key(app, key.code);
                        continue;
                    }
                    if app.mcp.is_some() {
                        handle_mcp_key(app, key.code);
                        continue;
                    }
                    match key.code {
                        KeyCode::Char(character) => app.input.push(character),
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Enter => {
                            app.send();
                            if app.should_quit() {
                                return Ok(());
                            }
                        }
                        KeyCode::Up => {
                            if app.selected > 0 {
                                app.selected -= 1;
                                app.seen_len = usize::MAX;
                                app.refresh_messages(true);
                            }
                        }
                        KeyCode::Down => {
                            if app.selected + 1 < app.sessions.len() {
                                app.selected += 1;
                                app.seen_len = usize::MAX;
                                app.refresh_messages(true);
                            }
                        }
                        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(5),
                        KeyCode::PageDown => app.scroll = (app.scroll + 5).min(app.max_scroll()),
                        KeyCode::Home => app.scroll = 0,
                        KeyCode::End => app.scroll = app.max_scroll(),
                        _ => {}
                    }
                }
                Event::Resize(_, _) => terminal.clear()?,
                _ => {}
            }
        }

        app.drain_sse();
        if !app.stream_buffer.is_empty() {
            // the event loop polls every 100ms; advance the spinner frame on each pass
            app.spinner_frame = (app.spinner_frame + 1) % SPINNER_FRAMES.len() as u8;
        }
        if app.last_messages_refresh.elapsed() >= Duration::from_millis(750) {
            app.refresh_messages(false);
        }
        if app.last_sessions_refresh.elapsed() >= Duration::from_secs(3) {
            app.refresh_sessions();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcript_renders_user_and_assistant_and_tools() {
        let messages = vec![
            json!({"type": "user", "text": "hello\nworld"}),
            json!({"type": "assistant", "content": [
                {"type": "text", "text": "hi there"},
                {"type": "tool", "name": "shell", "state": {"status": "completed"}},
            ]}),
            json!({"type": "system"}),
        ];
        let lines = transcript_lines(&messages);
        assert_eq!(lines[0], "you › hello");
        assert_eq!(lines[1], "     world");
        assert_eq!(lines[2], "assistant › hi there");
        assert_eq!(lines[3], "  [shell] completed");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn transcript_empty_shows_placeholder() {
        assert_eq!(transcript_lines(&[]), vec!["(no messages yet)".to_owned()]);
    }

    #[test]
    fn markdown_line_spans_bold_and_inline_code() {
        let spans = markdown_line_spans("plain **bold** and `code`", &Theme::default());
        let texts: Vec<&str> = spans.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, vec!["plain ", "bold", " and ", "code"]);
    }

    #[test]
    fn markdown_line_spans_heading_is_accent_bold() {
        let spans = markdown_line_spans("## Sub heading", &Theme::default());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "Sub heading");
        assert!(spans[0].1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn renders_shell() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
            theme: Theme::default(),
            theme_name: "devcode".to_owned(),
            sessions: vec![SessionRow {
                id: "ses_1".to_owned(),
                title: "demo".to_owned(),
            }],
            selected: 0,
            transcript: vec!["you › hi".to_owned()],
            scroll: 0,
            viewport_h: 10,
            input: "abc".to_owned(),
            model: Some("openai/x".to_owned()),
            status: String::new(),
            status_err: false,
            mcp: None,
            prompts: None,
            mcp_prompt_cache: None,
            seen_len: 1,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
            stream_buffer: String::new(),
            spinner_frame: 0,
            sse_rx: None,
        };
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("demo"), "sidebar session title missing");
        assert!(text.contains("you › hi"), "transcript line missing");
        assert!(text.contains("openai/x"), "model in status bar missing");
        assert!(text.contains("abc"), "input text missing");
    }

    #[test]
    fn streaming_block_renders_provisional_assistant_text() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
            theme: Theme::default(),
            theme_name: "devcode".to_owned(),
            sessions: vec![SessionRow {
                id: "ses_1".to_owned(),
                title: "demo".to_owned(),
            }],
            selected: 0,
            transcript: vec!["you › hi".to_owned()],
            scroll: 0,
            viewport_h: 10,
            input: String::new(),
            model: Some("openai/x".to_owned()),
            status: String::new(),
            status_err: false,
            mcp: None,
            prompts: None,
            mcp_prompt_cache: None,
            seen_len: 1,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
            stream_buffer: "partial output\nsecond line".to_owned(),
            spinner_frame: 0,
            sse_rx: None,
        };
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("(streaming)"), "streaming header missing: {text}");
        assert!(text.contains("partial output"), "streamed text missing: {text}");
        assert!(text.contains("second line"), "streamed text missing: {text}");
        assert!(
            text.contains(SPINNER_FRAMES[app.spinner_frame as usize]),
            "spinner glyph missing: {text}",
        );
    }

    #[test]
    fn drain_sse_accumulates_and_forces_rebuild_on_end() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
            theme: Theme::default(),
            theme_name: "devcode".to_owned(),
            sessions: Vec::new(),
            selected: 0,
            transcript: Vec::new(),
            scroll: 0,
            viewport_h: 10,
            input: String::new(),
            model: None,
            status: String::new(),
            status_err: false,
            mcp: None,
            prompts: None,
            mcp_prompt_cache: None,
            seen_len: 3,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
            stream_buffer: String::new(),
            spinner_frame: 0,
            sse_rx: Some(rx),
        };
        tx.send(SseMsg::Delta("hel".to_owned())).unwrap();
        tx.send(SseMsg::Delta("lo".to_owned())).unwrap();
        app.drain_sse();
        assert_eq!(app.stream_buffer, "hello");
        assert_eq!(app.seen_len, 3, "buffer alone must not force an early rebuild");
        tx.send(SseMsg::Ended).unwrap();
        app.drain_sse();
        assert!(app.sse_rx.is_none(), "ended stream must be dropped");
        assert_eq!(app.seen_len, usize::MAX, "end must force the next poll to rebuild");
    }

    #[test]
    fn command_parsing() {
        assert_eq!(
            parse_command("/model openai/gpt-5"),
            ("model", "openai/gpt-5")
        );
        assert_eq!(parse_command("/new"), ("new", ""));
        assert_eq!(parse_command("just a prompt"), ("", "just a prompt"));
        assert_eq!(parse_command("   "), ("", ""));
        assert_eq!(parse_command("/quit"), ("quit", ""));
        assert_eq!(parse_command("/mcps"), ("mcps", ""));
        assert_eq!(parse_command("/prompts"), ("prompts", ""));
        assert_eq!(parse_command("/fixture:greet a b"), ("fixture:greet", "a b"));
    }

    #[test]
    fn mcp_prompt_rows_parse_wire() {
        let body = json!({ "data": [
            { "server": "figma", "name": "diagram", "description": "draw it", "arguments": [{ "name": "what", "required": true }] },
            { "server": "fs", "name": "plain" },
            { "server": "missing-name" },
        ] });
        let rows = mcp_prompt_rows(&body);
        assert_eq!(rows.len(), 2, "incomplete rows are skipped");
        assert_eq!(rows[0].server, "figma");
        assert_eq!(rows[0].name, "diagram");
        assert_eq!(rows[0].description.as_deref(), Some("draw it"));
        assert_eq!(rows[0].arguments, vec!["what".to_string()]);
        assert_eq!(rows[1].description, None);
        assert_eq!(rows[1].arguments, Vec::<String>::new());
        assert_eq!(mcp_prompt_rows(&json!({})).len(), 0);
        assert_eq!(mcp_prompt_rows(&json!({ "data": null })).len(), 0);
    }

    #[test]
    fn sanitize_and_command_name() {
        assert_eq!(sanitize_command_part("my server!"), "my_server_");
        assert_eq!(sanitize_command_part("ok-name_1"), "ok-name_1");
        assert_eq!(sanitize_command_part(""), "");
        assert_eq!(mcp_prompt_command_name("my srv", "do it!"), "my_srv:do_it_");
    }

    #[test]
    fn prompt_arguments_parsing() {
        assert_eq!(parse_prompt_arguments("a b c"), vec!["a", "b", "c"]);
        assert_eq!(
            parse_prompt_arguments(r#"say "hello world" now"#),
            vec!["say", "hello world", "now"]
        );
        assert_eq!(
            parse_prompt_arguments("say 'it's me'"),
            vec!["say", "it", "s", "me"]
        );
        assert_eq!(parse_prompt_arguments(""), Vec::<String>::new());
        assert_eq!(parse_prompt_arguments("one  two   three"), vec!["one", "two", "three"]);
        assert_eq!(parse_prompt_arguments("[Image 3] describe"), vec!["[Image 3]", "describe"]);
        assert_eq!(parse_prompt_arguments("[image 42]"), vec!["[image 42]"]);
        assert_eq!(parse_prompt_arguments("[Image x]"), vec!["[Image", "x]"]);
        assert_eq!(parse_prompt_arguments(r#""unterminated"#), vec!["unterminated"]);
        assert_eq!(parse_prompt_arguments(r#""""#), vec![""]);
        assert_eq!(
            parse_prompt_arguments(r#"a "b c" d 'e f' g"#),
            vec!["a", "b c", "d", "e f", "g"]
        );
    }

    #[test]
    fn prompts_dialog_renders_command_names() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
            theme: Theme::default(),
            theme_name: "devcode".to_owned(),
            sessions: vec![],
            selected: 0,
            transcript: vec![],
            scroll: 0,
            viewport_h: 10,
            input: String::new(),
            model: None,
            status: String::new(),
            status_err: false,
            mcp: None,
            prompts: Some(PromptsDialog {
                prompts: vec![McpPromptEntry {
                    server: "fixture".to_owned(),
                    name: "greet".to_owned(),
                    description: Some("greets someone".to_owned()),
                    arguments: vec!["who".to_owned()],
                }],
                selected: 0,
            }),
            mcp_prompt_cache: None,
            seen_len: 0,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
            stream_buffer: String::new(),
            spinner_frame: 0,
            sse_rx: None,
        };
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("MCP prompts"), "dialog title missing");
        assert!(text.contains("/fixture:greet"), "command name missing");
        assert!(text.contains("who"), "argument hint missing");
    }

    #[test]
    fn mcp_rows_parse_wire() {
        let body = json!({
            "data": [
                { "name": "figma", "status": { "status": "needs_auth", "error": "auth required" }, "integrationID": "mcp_deadbeef" },
                { "name": "local", "status": { "status": "pending" } },
                { "name": "dormant", "status": { "status": "disabled" } },
            ]
        });
        let rows = mcp_rows(&body);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "figma");
        assert_eq!(rows[0].status, "needs_auth");
        assert_eq!(rows[0].error.as_deref(), Some("auth required"));
        assert_eq!(rows[0].integration_id.as_deref(), Some("mcp_deadbeef"));
        assert_eq!(rows[1].status, "pending");
        assert_eq!(rows[1].integration_id, None);
        assert_eq!(rows[2].status, "disabled");
        assert_eq!(mcp_rows(&json!({})).len(), 0);
        assert_eq!(mcp_rows(&json!({ "data": null })).len(), 0);
    }

    #[test]
    fn renders_mcp_dialog() {
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
            theme: Theme::default(),
            theme_name: "devcode".to_owned(),
            sessions: vec![],
            selected: 0,
            transcript: vec!["you › hi".to_owned()],
            scroll: 0,
            viewport_h: 10,
            input: String::new(),
            model: None,
            status: String::new(),
            status_err: false,
            mcp: Some(McpDialog {
                servers: vec![
                    McpRow {
                        name: "figma".to_owned(),
                        status: "needs_auth".to_owned(),
                        error: Some("MCP server \"figma\" requires OAuth authentication. Run: DEVcode mcp auth figma".to_owned()),
                        integration_id: Some("mcp_deadbeef".to_owned()),
                    },
                    McpRow {
                        name: "notes".to_owned(),
                        status: "pending".to_owned(),
                        error: None,
                        integration_id: None,
                    },
                ],
                selected: 0,
                detail: false,
                error: None,
            }),
            prompts: None,
            mcp_prompt_cache: None,
            seen_len: 1,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
            stream_buffer: String::new(),
            spinner_frame: 0,
            sse_rx: None,
        };
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("MCP servers (2)"), "dialog title missing: {text}");
        assert!(text.contains("figma"), "figma row missing: {text}");
        assert!(text.contains("Sign in required"), "needs_auth label missing: {text}");
        assert!(text.contains("notes"), "pending row missing: {text}");
        assert!(text.contains("Connecting"), "pending label missing: {text}");

        // The detail view shows the integration and the auth hint.
        app.mcp.as_mut().unwrap().detail = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("mcp_deadbeef"), "integration id missing: {text}");
        assert!(text.contains("DEVcode mcp auth figma"), "auth hint missing: {text}");
    }
}
