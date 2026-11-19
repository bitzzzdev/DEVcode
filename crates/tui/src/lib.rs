//! Minimal ratatui front-end for the Rust service. Status: Phase 8 MVP.
//! Talks to the managed service over the same HTTP/basic-auth contract the Bun
//! TUI uses. No theme port, no sidebar plugins, no session forking yet.

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

pub struct SessionRow {
    pub id: String,
    pub title: String,
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

pub struct App {
    base: String,
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
    // number of messages last seen for the selected session; poll rebuilds on change
    seen_len: usize,
    last_sessions_refresh: Instant,
    last_messages_refresh: Instant,
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
        seen_len: usize::MAX,
        last_sessions_refresh: Instant::now() - Duration::from_secs(60),
        last_messages_refresh: Instant::now() - Duration::from_secs(60),
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
    let response = ureq::get(&format!("{}{}", client.base, path))
        .set("authorization", &client.auth)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(http_error)?;
    response.into_json::<Value>().map_err(|e| e.to_string())
}

fn http_post(client: &App, path: &str, body: &Value) -> Result<Value, String> {
    let response = ureq::post(&format!("{}{}", client.base, path))
        .set("authorization", &client.auth)
        .set("content-type", "application/json")
        .timeout(Duration::from_secs(10))
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
                }
                self.last_messages_refresh = Instant::now();
            }
            Err(error) => self.set_status(error, true),
        }
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
        let (command, argument) = parse_command(&self.input);
        match command {
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
            _ => {}
        }
        let text = argument.to_owned();
        if text.is_empty() {
            return;
        }
        self.input.clear();
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
        .block(Block::default().borders(Borders::RIGHT).title(" sessions "))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");
    frame.render_stateful_widget(sidebar, main[0], &mut list_state);

    // transcript
    app.viewport_h = right[0].height.saturating_sub(2).max(1);
    let lines: Vec<Line> = app
        .transcript
        .iter()
        .map(|line| {
            let style = if line.starts_with("you ›") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("assistant ›") {
                Style::default().fg(Color::White)
            } else if line.starts_with("  [") {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(line.clone(), style))
        })
        .collect();
    let max_scroll = app.max_scroll();
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }
    let transcript = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(transcript, right[0]);

    // input
    let input = Paragraph::new(format!("> {}", app.input))
        .block(Block::default().borders(Borders::TOP).title(" message "));
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
    let help = " ↑↓ sessions · Enter send · /new /mcps · PgUp/PgDn scroll · Ctrl-C quit";
    let status_line = if app.status.is_empty() {
        format!(" {model}{help}")
    } else {
        format!(" {} | {model}{help}", app.status)
    };
    let style = if app.status_err {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(Paragraph::new(status_line).style(style), root[1]);

    if app.mcp.is_some() {
        draw_mcp(frame, app);
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
        .title(format!(" MCP servers ({}) ", dialog.servers.len()));

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
                "connected" => Color::Green,
                "pending" => Color::Yellow,
                "failed" => Color::Red,
                "needs_auth" => Color::Yellow,
                "disabled" => Color::DarkGray,
                _ => Color::Gray,
            };
            let label_color = match server.status.as_str() {
                "failed" => Color::Red,
                "needs_auth" => Color::Yellow,
                "connected" => Color::Green,
                _ => Color::DarkGray,
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
    fn renders_shell() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            base: String::new(),
            auth: String::new(),
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
            seen_len: 1,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
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
            seen_len: 1,
            last_sessions_refresh: Instant::now(),
            last_messages_refresh: Instant::now(),
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
