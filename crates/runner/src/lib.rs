//! Shared agent loop: admission (inbox protocol) and step execution. Used by
//! `DEVcode run` (headless CLI) and by the HTTP `session.prompt` endpoint (background run).
//! Limits mirror the headless runner: static system prompt, cost 0 until a pricing table
//! exists, tools execute after the stream completes, no retries/compaction/permissions.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use devcode_ai::{ChatMessage, ModelRef, ToolCall, ToolSpec};
use devcode_core::session_message::HistoryRow;

pub const SYSTEM_PROMPT: &str =
    "You are DEVcode, an open-source AI coding agent running in a headless CLI. \
     You have tools to read, write, edit files, search (glob/grep), and run shell commands. \
     Work autonomously: make the change rather than describing it. Keep responses concise. \
     When you are done, stop calling tools. Always use absolute paths or paths relative to the working directory.";

pub struct Execution {
    pub prompt: String,
    pub model: ModelRef,
    /// Continue this session; when None, a new session is created for `directory`.
    pub session_id: Option<String>,
    /// Directory the (new or existing) session runs in: create-time project resolution for
    /// new sessions, tool cwd for the run.
    pub directory: PathBuf,
    pub max_steps: u32,
    pub delivery: String,
}

/// Admit one prompt: durable `session.inbox.enqueued` + inbox row, then delivery
/// (`session.inbox.delivered` + user message insert). `item` is the wire
/// `SessionInbox.User` object returned by the prompt endpoint.
pub struct Admitted {
    pub inbox_id: String,
    pub item: Value,
}

pub fn model_json(model: &ModelRef) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("id".into(), Value::String(model.id.clone()));
    map.insert(
        "providerID".into(),
        Value::String(model.provider_id.clone()),
    );
    if let Some(variant) = &model.variant {
        map.insert("variant".into(), Value::String(variant.clone()));
    }
    Value::Object(map)
}

pub fn admit(
    conn: &mut rusqlite::Connection,
    session_id: &str,
    prompt: &str,
    delivery: &str,
    explicit_id: Option<&str>,
    payload_extra: Option<&Value>,
) -> Result<Admitted, String> {
    let message_id = explicit_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("msg_{}", devcode_core::ascending()));
    let mut payload = json!({ "text": prompt });
    if let Some(Value::Object(extra)) = payload_extra {
        for (key, value) in extra {
            payload[key] = value.clone();
        }
    }
    let item = json!({
        "type": "user",
        "payload": payload,
        "delivery": delivery,
    });
    let enqueued = emit(
        conn,
        session_id,
        "session.inbox.enqueued",
        json!({ "sessionID": session_id, "inboxID": message_id, "item": item }),
        now_millis(),
    )?;
    devcode_core::session_message::project_admitted(
        conn,
        &message_id,
        session_id,
        &item,
        enqueued.seq,
        enqueued.created,
    )
    .map_err(|error| format!("failed to project inbox admission: {error}"))?;
    let delivered = emit(
        conn,
        session_id,
        "session.inbox.delivered",
        json!({ "sessionID": session_id, "inboxID": message_id }),
        enqueued.created,
    )?;
    let mut data = json!({ "text": prompt, "time": { "created": delivered.created } });
    if let Some((row_type, payload)) =
        devcode_core::session_message::project_delivered(conn, &message_id, session_id)
            .map_err(|error| format!("failed to consume inbox item: {error}"))?
    {
        for key in ["files", "agents", "skills", "metadata"] {
            if let Some(value) = payload.get(key) {
                data[key] = value.clone();
            }
        }
        devcode_core::session_message::insert_message(
            conn,
            &message_id,
            session_id,
            &row_type,
            delivered.seq,
            delivered.created,
            data,
        )
        .map_err(|error| format!("failed to insert user message: {error}"))?;
    }
    let mut item = item;
    item["id"] = json!(message_id);
    item["sessionID"] = json!(session_id);
    item["time"] = json!({ "created": enqueued.created });
    Ok(Admitted {
        inbox_id: message_id,
        item,
    })
}

/// Admit + run one full agent loop: bracket execution with
/// `session.execution.started`/`succeeded`/`failed`, then mark the session idle.
/// `on_text` receives assistant deltas; `on_tool` receives (tool name, argument summary).
/// Returns the session id.
pub fn execute(
    execution: &Execution,
    paths: &devcode_util::Paths,
    on_text: &mut dyn FnMut(&str),
    on_tool: &mut dyn FnMut(&str, &str),
) -> Result<String, String> {
    let database_path = devcode_core::database_path(paths);
    let mut database = devcode_core::Database::open(&database_path).map_err(|error| {
        format!(
            "failed to open database {}: {error}",
            database_path.display()
        )
    })?;
    let conn = &mut database.conn;

    let directory = execution
        .directory
        .canonicalize()
        .map_err(|error| format!("directory {}: {error}", execution.directory.display()))?;
    let directory_string = directory.to_string_lossy().into_owned();

    let record = match &execution.session_id {
        Some(id) => devcode_core::get_session(conn, id)
            .map_err(|error| format!("failed to read session {id}: {error}"))?
            .ok_or_else(|| format!("session not found: {id}"))?,
        None => devcode_core::create_session_for_directory(
            conn,
            &directory_string,
            now_millis(),
            None,
            env!("CARGO_PKG_VERSION"),
        )
        .map_err(|error| format!("failed to create session: {error}"))?,
    };
    let session_id = record.id.clone();

    admit(
        conn,
        &session_id,
        &execution.prompt,
        &execution.delivery,
        None,
        None,
    )?;

    let started = now_millis();
    emit(
        conn,
        &session_id,
        "session.execution.started",
        json!({ "sessionID": session_id }),
        started,
    )?;

    let plugins = load_plugin_host(paths);
    let mut mcp = load_mcp_runtime(paths, &plugins);
    let outcome = step_loop(
        conn,
        paths,
        &execution.model,
        &directory,
        execution.max_steps,
        on_text,
        on_tool,
        &session_id,
        &plugins,
        &mut mcp,
    );
    let finished = now_millis();
    match outcome {
        Ok(()) => {
            emit(
                conn,
                &session_id,
                "session.execution.succeeded",
                json!({ "sessionID": session_id }),
                finished,
            )?;
            devcode_core::session_message::mark_idle(conn, &session_id, "succeeded", finished)
                .map_err(|error| format!("failed to mark session idle: {error}"))?;
        }
        Err(message) => {
            let _ = emit(
                conn,
                &session_id,
                "session.execution.failed",
                json!({
                    "sessionID": session_id,
                    "error": { "type": "execution.error", "message": message },
                }),
                finished,
            );
            let _ = devcode_core::session_message::mark_idle(conn, &session_id, "failed", finished);
            return Err(message);
        }
    }
    Ok(session_id)
}

/// Run the agent loop for an already-admitted prompt (background HTTP run). Opens its
/// own database connection, so the caller must not hold one.
pub fn run_admitted(
    paths: &devcode_util::Paths,
    model: &ModelRef,
    session_id: &str,
    max_steps: u32,
    on_text: &mut dyn FnMut(&str),
    on_tool: &mut dyn FnMut(&str, &str),
) -> Result<(), String> {
    let database_path = devcode_core::database_path(paths);
    let mut database = devcode_core::Database::open(&database_path).map_err(|error| {
        format!(
            "failed to open database {}: {error}",
            database_path.display()
        )
    })?;
    let conn = &mut database.conn;
    let record = devcode_core::get_session(conn, session_id)
        .map_err(|error| format!("failed to read session {session_id}: {error}"))?
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let directory = PathBuf::from(&record.directory);

    let started = now_millis();
    emit(
        conn,
        session_id,
        "session.execution.started",
        json!({ "sessionID": session_id }),
        started,
    )?;
    let plugins = load_plugin_host(paths);
    let mut mcp = load_mcp_runtime(paths, &plugins);
    let outcome = step_loop(
        conn, paths, model, &directory, max_steps, on_text, on_tool, session_id, &plugins, &mut mcp,
    );
    let finished = now_millis();
    match outcome {
        Ok(()) => {
            emit(
                conn,
                session_id,
                "session.execution.succeeded",
                json!({ "sessionID": session_id }),
                finished,
            )?;
            devcode_core::session_message::mark_idle(conn, session_id, "succeeded", finished)
                .map_err(|error| format!("failed to mark session idle: {error}"))?;
            Ok(())
        }
        Err(message) => {
            let _ = emit(
                conn,
                session_id,
                "session.execution.failed",
                json!({
                    "sessionID": session_id,
                    "error": { "type": "execution.error", "message": message },
                }),
                finished,
            );
            let _ = devcode_core::session_message::mark_idle(conn, session_id, "failed", finished);
            Err(message)
        }
    }
}

const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read",
    "write",
    "edit",
    "glob",
    "grep",
    "shell",
    "skill",
    "list_mcp_resources",
    "read_mcp_resource",
];

/// Load the user's native plugins for one run. Per-run `dlopen` is negligible; the
/// alternative (a shared static host) would hold cdylib mappings for the process and
/// complicate reload semantics on the service. Load failures print to stderr and do
/// not abort the run.
/// Connect to every MCP server enabled in `<config>/devcode.jsonc` (`mcp.servers`,
/// same format the Bun build reads), plus servers contributed by plugins. An entry
/// already present in user config wins, including `disabled: true`. Per-run
/// connections; unreachable servers are reported on stderr and skipped.
fn load_mcp_runtime(paths: &devcode_util::Paths, plugins: &devcode_plugin::Host) -> devcode_mcp::Runtime {
    let db_path = devcode_core::database_path(paths);
    let config_path = paths.config.join("devcode.jsonc");
    let config = match devcode_mcp::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            devcode_mcp::McpConfig::default()
        }
    };
    let servers = merge_plugin_mcp_servers(config.servers, plugins);
    devcode_mcp::Runtime::connect_all(&servers, std::time::Duration::from_secs(5), &db_path)
}

/// User config wins on name conflict; plugin contributions fill the gaps.
pub fn merge_plugin_mcp_servers(
    servers: std::collections::BTreeMap<String, devcode_mcp::ServerConfig>,
    plugins: &devcode_plugin::Host,
) -> std::collections::BTreeMap<String, devcode_mcp::ServerConfig> {
    merge_contributed_servers(servers, &plugins.mcp_servers())
}

/// Same merge for contribution lists collected elsewhere (e.g. cached at startup).
pub fn merge_contributed_servers(
    mut servers: std::collections::BTreeMap<String, devcode_mcp::ServerConfig>,
    contributed: &[(String, serde_json::Value)],
) -> std::collections::BTreeMap<String, devcode_mcp::ServerConfig> {
    for (name, spec) in contributed {
        if servers.contains_key(name) {
            continue;
        }
        match devcode_mcp::parse_server(name, spec) {
            Some(server) => {
                servers.insert(name.clone(), server);
            }
            None => eprintln!("plugin contributed an unusable mcp server entry: {name}"),
        }
    }
    servers
}

/// Load embedded skills, those under `<config>/skills/`, and skills contributed by
/// plugins (Bun semantics: an existing skill with the same id is preserved).
fn load_skills(paths: &devcode_util::Paths, plugins: &devcode_plugin::Host) -> Vec<devcode_skills::Skill> {
    let mut skills = devcode_skills::load(&paths.config, env!("CARGO_PKG_VERSION"));
    for skill in plugins.skills() {
        if !skills.iter().any(|existing| existing.id == skill.id) {
            skills.push(skill);
        }
    }
    skills
}

/// System prompt for this run: the static head, the available-skills section,
/// ambient instructions (AGENTS.md files and configured `instructions`, rendered
/// the way the Bun build renders the `core/instructions` source), the date, and
/// the environment block. Order matches the Bun build's instruction combine.
fn system_prompt(paths: &devcode_util::Paths, skills: &[devcode_skills::Skill], directory: &Path) -> String {
    let root = git_toplevel(directory);
    let git_repo = if root.is_some() { "yes" } else { "no" };
    let root_ref = root.as_deref().unwrap_or(directory);
    let mut prompt = format!(
        "{SYSTEM_PROMPT}\n\n{}",
        devcode_skills::available_skills_section(skills)
    );
    let instructions = load_instructions(paths, directory, root_ref);
    if !instructions.is_empty() {
        prompt = format!("{prompt}\n\n{instructions}");
    }
    format!(
        "{prompt}\n\nToday's date: {}\n\nHere is some useful information about the environment you are running in:\n<env>\n  Working directory: {}\n  Workspace root folder: {}\n  Is directory a git repo: {git_repo}\n  Platform: {}\n  Prefer {} over generic system temporary directories such as /tmp; it is pre-created and approved for external access.\n</env>",
        date_string(),
        directory.display(),
        root_ref.display(),
        std::env::consts::OS,
        paths.tmp.display(),
    )
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `Date.prototype.toDateString()` (e.g. "Thu Nov 19 2026") for the builtin `core/date`.
fn date_string() -> String {
    let days = now_millis().div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 (day 0) was a Thursday; index WEEKDAYS with Sunday = 0.
    let weekday = ((days + 4) % 7 + 7) % 7;
    format!("{} {} {:02} {}", WEEKDAYS[weekday as usize], MONTHS[(month - 1) as usize], day, year)
}

// Howard Hinnant's civil_from_days algorithm. Input: days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Ambient instructions rendered as the Bun build's `core/instructions` source:
/// `Instructions from: <path>\n<content>` sections joined by `\n\n`.
///
/// Sources, in order: the global `<config>/AGENTS.md`, the AGENTS.md ancestor walk
/// from `start` to the stop bound (nearest first; stop is `start`'s git toplevel, or
/// $HOME when `start` is under $HOME), then configured `instructions` from
/// `<config>/devcode.jsonc` (URLs fetched with a 5 s timeout; `~/`, absolute file
/// paths, absolute globs and relative patterns globbed upward to the git root).
///
/// Deviations from the Bun build (documented in the changelog): a file that fails to
/// read is skipped with a stderr note instead of poisoning the whole instruction set,
/// remote fetch failures are skipped (no last-good cache), and Rust glob does not
/// match dotfiles with `*` (no `dot: true` equivalent).
fn load_instructions(paths: &devcode_util::Paths, start: &Path, root: &Path) -> String {
    let global = paths.config.join("AGENTS.md");
    let mut files: Vec<(String, String)> = Vec::new();
    if let Some(content) = read_instruction(&global) {
        files.push((global.display().to_string(), content));
    }

    let stop = if contains_dir(&paths.home, start) {
        paths.home.clone()
    } else {
        root.to_path_buf()
    };
    let mut seen = std::collections::HashSet::new();
    for directory in ancestors(start, &stop) {
        let candidate = directory.join("AGENTS.md");
        if !seen.insert(candidate.clone()) {
            continue;
        }
        if let Some(content) = read_instruction(&candidate) {
            files.push((candidate.display().to_string(), content));
        }
    }

    files.extend(configured_instructions(paths, start, root));
    files
        .iter()
        .map(|(path, content)| format!("Instructions from: {path}\n{content}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn read_instruction(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("skipping instruction file {}: {error}", path.display());
            None
        }
    }
}

/// Ancestor directories from `start` to `stop` inclusive, nearest first.
fn ancestors(start: &Path, stop: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current = start.to_path_buf();
    loop {
        out.push(current.clone());
        if current == stop {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    out
}

/// Lexical containment: `candidate` is `ancestor` or sits underneath it.
fn contains_dir(ancestor: &Path, candidate: &Path) -> bool {
    candidate.starts_with(ancestor)
}

/// `git rev-parse --show-toplevel` from `start`; None for non-git directories.
fn git_toplevel(start: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!toplevel.is_empty()).then(|| PathBuf::from(toplevel))
}

/// Configured `instructions` from top-level `instructions` in `<config>/devcode.jsonc`.
/// URLs are fetched with a 5 s timeout; a failed fetch is skipped (no last-good
/// cache, unlike the Bun build). `~/` expands to $HOME; an absolute path whose
/// parent exists scans that one basename; an absolute path without an existing
/// parent and relative patterns are globbed.
fn configured_instructions(paths: &devcode_util::Paths, start: &Path, root: &Path) -> Vec<(String, String)> {
    let sources = load_configured_sources(&paths.config.join("devcode.jsonc"));
    if sources.is_empty() {
        return Vec::new();
    }
    let mut watched = std::collections::HashSet::new();
    let mut files = Vec::new();
    for source in &sources {
        if source.starts_with("http://") || source.starts_with("https://") {
            match fetch_instruction(source) {
                Ok(content) => files.push((source.clone(), content)),
                Err(error) => eprintln!("skipping instruction url {source}: {error}"),
            }
            continue;
        }
        let expanded = source
            .strip_prefix("~/")
            .map(|rest| paths.home.join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| source.clone());
        for candidate in configured_matches(&expanded, start, root) {
            let Ok(resolved) = std::path::absolute(&candidate).map_err(|e| e.to_string()) else {
                continue;
            };
            if !watched.insert(resolved.clone()) {
                continue;
            }
            if let Some(content) = read_instruction(&resolved) {
                files.push((resolved.display().to_string(), content));
            }
        }
    }
    files
}

fn load_configured_sources(config_file: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(config_file) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&devcode_mcp::strip_jsonc(&raw)) else {
        eprintln!("failed to parse {}", config_file.display());
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    value
        .get("instructions")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|source| seen.insert((*source).to_owned()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn configured_matches(expanded: &str, start: &Path, root: &Path) -> Vec<String> {
    let path = Path::new(expanded);
    if path.is_absolute() {
        let directory = path.parent().unwrap_or(Path::new("/"));
        if directory.is_dir() {
            // Literal parent exists: scan the basename as a glob inside it.
            let pattern = format!("{}/{}", directory.display(), path.file_name().unwrap_or_default().to_string_lossy());
            return sorted_glob(&pattern);
        }
        // No literal parent: treat the whole source as an absolute glob.
        return sorted_glob(&expanded.replace(std::path::MAIN_SEPARATOR, "/"));
    }
    // Relative patterns search upward from the location to the project root.
    let mut out = Vec::new();
    for directory in ancestors(start, root) {
        out.extend(sorted_glob(&format!("{}/{expanded}", directory.display())));
    }
    out
}

fn sorted_glob(pattern: &str) -> Vec<String> {
    let mut matches: Vec<String> = glob::glob(pattern)
        .map(|paths| paths.flatten().map(|path| path.display().to_string()).collect())
        .unwrap_or_default();
    matches.sort();
    matches
}

fn fetch_instruction(url: &str) -> Result<String, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .get(url)
        .call()
        .map_err(|error| error.to_string())?;
    response.into_string().map_err(|error| error.to_string())
}

pub fn load_plugin_host(paths: &devcode_util::Paths) -> devcode_plugin::Host {
    let dir = paths.config.join("plugins");
    let mut host = devcode_plugin::Host::empty();
    if !dir.is_dir() {
        return host;
    }
    // Same trust model as the service-startup loader: the user's own plugins directory.
    let (_loaded, errors) = unsafe { host.load_dir(&dir) };
    for error in &errors {
        eprintln!("plugin failed to load: {error}");
    }
    host
}

/// Stream steps until the model stops calling tools or the step budget runs out.
#[allow(clippy::too_many_arguments)]
fn step_loop(
    conn: &mut rusqlite::Connection,
    paths: &devcode_util::Paths,
    model: &ModelRef,
    directory: &Path,
    max_steps: u32,
    on_text: &mut dyn FnMut(&str),
    on_tool: &mut dyn FnMut(&str, &str),
    session_id: &str,
    plugins: &devcode_plugin::Host,
    mcp: &mut devcode_mcp::Runtime,
) -> Result<(), String> {
    let skills = load_skills(paths, plugins);
    let system = system_prompt(paths, &skills, directory);
    let model_json = model_json(model);
    let tools = tool_specs(plugins, mcp);
    let mut exhausted = true;
    for _step in 0..max_steps {
        let rows = devcode_core::session_message::history(conn, session_id)
            .map_err(|error| format!("failed to read history: {error}"))?;
        let messages = history_to_chat(&rows);

        let assistant_id = format!("msg_{}", devcode_core::ascending());
        let started = now_millis();
        let started_event = emit(
            conn,
            session_id,
            "session.step.started",
            json!({
                "sessionID": session_id,
                "assistantMessageID": assistant_id,
                "agent": "build",
                "model": model_json,
                "started": started,
            }),
            started,
        )?;
        devcode_core::session_message::insert_assistant(
            conn,
            &assistant_id,
            session_id,
            started_event.seq,
            "build",
            &model_json,
            started,
        )
        .map_err(|error| format!("failed to insert assistant message: {error}"))?;

        let mut text = String::new();
        let mut first_token = false;
        let summary = {
            let mut delta_sink = |delta: &str| {
                if !first_token && !delta.is_empty() {
                    first_token = true;
                    let _ = emit(
                        conn,
                        session_id,
                        "session.step.first-token",
                        json!({ "sessionID": session_id, "assistantMessageID": assistant_id }),
                        now_millis(),
                    );
                }
                text.push_str(delta);
                on_text(delta);
            };
            devcode_ai::stream_chat(
                model,
                Some(system.as_str()),
                &messages,
                &tools,
                8192,
                &mut delta_sink,
            )
            .map_err(|error| error.to_string())?
        };

        let finished = now_millis();
        let mut parts: Vec<Value> = Vec::new();
        if !text.is_empty() {
            parts.push(json!({ "type": "text", "text": text }));
        }
        for call in &summary.tool_calls {
            on_tool(
                &call.name,
                call.arguments.lines().next().unwrap_or("").trim(),
            );
            parts.push(execute_tool(
                paths, call, directory, session_id, plugins, mcp, &skills,
            ));
        }

        emit(
            conn,
            session_id,
            "session.message.content.updated",
            json!({
                "sessionID": session_id,
                "messageID": assistant_id,
                "content": parts,
            }),
            finished,
        )?;
        devcode_core::session_message::replace_parts(
            conn,
            &assistant_id,
            session_id,
            &parts,
            started,
            finished,
        )
        .map_err(|error| format!("failed to replace assistant parts: {error}"))?;

        let cost = 0.0_f64; // pricing table not ported yet; tokens are recorded
        let tokens = tokens_value(&summary.usage);
        emit(
            conn,
            session_id,
            "session.step.ended",
            json!({
                "sessionID": session_id,
                "assistantMessageID": assistant_id,
                "finish": summary.finish.as_str(),
                "rawFinish": summary.raw_finish,
                "cost": cost,
                "tokens": tokens,
            }),
            finished,
        )?;
        devcode_core::session_message::finish_assistant(
            conn,
            &assistant_id,
            Some(summary.finish.as_str()),
            summary.raw_finish.as_deref(),
            cost,
            &tokens,
            finished,
        )
        .map_err(|error| format!("failed to finish assistant message: {error}"))?;
        devcode_core::session_message::apply_usage(conn, session_id, cost, &tokens, finished)
            .map_err(|error| format!("failed to apply usage: {error}"))?;

        if summary.finish != devcode_ai::FinishReason::ToolCalls {
            exhausted = false;
            break;
        }
    }
    if exhausted {
        return Err(format!(
            "stopped after {max_steps} steps without a final answer"
        ));
    }
    Ok(())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
fn tool_specs(plugins: &devcode_plugin::Host, mcp: &devcode_mcp::Runtime) -> Vec<ToolSpec> {
    let mut specs = builtin_tool_specs();
    // Plugin and MCP tools shadowing a builtin (or each other, MCP wins) are dropped:
    // first-seen wins for the model, and the router below enforces the same order.
    let mut taken: Vec<String> = BUILTIN_TOOL_NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    for plugin in plugins.tool_specs() {
        if taken.contains(&plugin.name) {
            continue;
        }
        taken.push(plugin.name.clone());
        specs.push(ToolSpec {
            name: plugin.name,
            description: plugin.description,
            input_schema: plugin.input_schema,
        });
    }
    for tool in &mcp.tools {
        if taken.contains(&tool.namespaced) {
            continue;
        }
        taken.push(tool.namespaced.clone());
        specs.push(ToolSpec {
            name: tool.namespaced.clone(),
            description: format!("[MCP:{}] {}", tool.server, tool.tool.description),
            input_schema: tool.tool.input_schema.clone(),
        });
    }
    specs
}

fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read".into(),
            description: "Read a file (1-based line numbers) or list a directory.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory path" },
                    "offset": { "type": "integer", "description": "First line/entry to read (1-based)" },
                    "limit": { "type": "integer", "description": "Max lines/entries (default and cap 2000)" }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write".into(),
            description: "Write a file, creating parent directories. Overwrites existing content.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "edit".into(),
            description: "Replace an exact, unique string in a file. Use replaceAll for every occurrence.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "oldString": { "type": "string" },
                    "newString": { "type": "string" },
                    "replaceAll": { "type": "boolean" }
                },
                "required": ["path", "oldString", "newString"]
            }),
        },
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob pattern. Returns paths, cap 100.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. **/*.rs" },
                    "path": { "type": "string", "description": "Search root (defaults to cwd)" },
                    "hidden": { "type": "boolean", "description": "Include hidden files" },
                    "limit": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents by regex or literal string. Returns file/line matches, cap 100.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "File or directory (defaults to cwd)" },
                    "include": { "type": "string", "description": "Filename glob filter, e.g. *.rs" },
                    "literal": { "type": "boolean" },
                    "caseSensitive": { "type": "boolean" },
                    "limit": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "skill".into(),
            description: "Load a specialized skill's instructions and resources into the current conversation when the task at hand matches its description.\n\nThe skill ID must match an available skill or a skill explicitly referenced by the user.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The ID of an available skill or a skill explicitly referenced by the user" }
                },
                "required": ["id"]
            }),
        },
        ToolSpec {
            name: "list_mcp_resources".into(),
            description: "List documents, records, and other data exposed by MCP servers. Use this when the user refers to something that is not a local file, such as a URI with a custom scheme, then load a match with read_mcp_resource. Each entry names the server to read it from. Templates are resources addressed by a parameter such as a record ID; fill in the uriTemplate placeholders before reading.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string", "description": "MCP server name as configured. Omit to list every server." }
                }
            }),
        },
        ToolSpec {
            name: "read_mcp_resource".into(),
            description: "Read one MCP resource by server and URI. Not for local files. Always return the full contents rather than slicing or filtering them; oversized output is truncated automatically and the full content is saved to a file you can read.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string", "description": "The server field of the discovered resource." },
                    "uri": { "type": "string", "description": "Exact resource URI from discovery, an expanded uriTemplate, or the user." }
                },
                "required": ["server", "uri"]
            }),
        },
        ToolSpec {
            name: "shell".into(),
            description: "Run a shell command (`sh -c`). Returns combined stdout/stderr. Default timeout 120s.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "workdir": { "type": "string" },
                    "timeout": { "type": "integer", "description": "Timeout in milliseconds" }
                },
                "required": ["command"]
            }),
        },
    ]
}
/// Convert the stored transcript to model-facing messages (to-llm-message.ts, simplified).
pub fn history_to_chat(rows: &[HistoryRow]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for row in rows {
        match row.row_type.as_str() {
            "user" | "synthetic" | "skill" => {
                let text = row.data["text"].as_str().unwrap_or("").to_owned();
                if !text.is_empty() {
                    messages.push(ChatMessage::User(text));
                }
            }
            "system" => messages.push(ChatMessage::System(
                row.data["text"].as_str().unwrap_or("").to_owned(),
            )),
            "assistant" => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for part in &row.parts {
                    match part["type"].as_str() {
                        Some("text") => text.push_str(part["text"].as_str().unwrap_or("")),
                        Some("tool") => {
                            let input = &part["state"]["input"];
                            tool_calls.push(ToolCall {
                                id: part["id"].as_str().unwrap_or_default().to_owned(),
                                name: part["name"].as_str().unwrap_or_default().to_owned(),
                                arguments: if input.is_string() {
                                    input.as_str().unwrap_or("{}").to_owned()
                                } else {
                                    input.to_string()
                                },
                            });
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() || !tool_calls.is_empty() {
                    messages.push(ChatMessage::Assistant { text, tool_calls });
                }
                for part in &row.parts {
                    if part["type"].as_str() != Some("tool") {
                        continue;
                    }
                    let content = part["state"]["content"]
                        .as_array()
                        .and_then(|content| {
                            content
                                .iter()
                                .find(|item| item["type"].as_str() == Some("text"))
                        })
                        .and_then(|item| item["text"].as_str())
                        .map(str::to_owned);
                    if let Some(content) = content {
                        messages.push(ChatMessage::Tool {
                            call_id: part["id"].as_str().unwrap_or_default().to_owned(),
                            content,
                        });
                    }
                }
            }
            _ => {} // model-switched, agent-switched, location-switched, compaction, shell: skipped
        }
    }
    messages
}
fn tool_text_content(text: &str) -> Value {
    json!([{ "type": "text", "text": text }])
}
fn completed_tool_part(
    call: &ToolCall,
    arguments: &Value,
    output: &str,
    metadata: Value,
    started: i64,
    finished: i64,
) -> Value {
    json!({
        "type": "tool",
        "id": call.id,
        "name": call.name,
        "executed": true,
        "state": {
            "status": "completed",
            "input": arguments,
            "content": tool_text_content(output),
            "metadata": metadata,
        },
        "time": { "created": started, "ran": started, "completed": finished },
    })
}
fn error_tool_part(
    call: &ToolCall,
    arguments: &Value,
    message: &str,
    started: i64,
    finished: i64,
) -> Value {
    json!({
        "type": "tool",
        "id": call.id,
        "name": call.name,
        "executed": true,
        "state": {
            "status": "error",
            "input": arguments,
            "error": { "type": "tool.execution", "message": message },
            "content": tool_text_content(message),
        },
        "time": { "created": started, "ran": started, "completed": finished },
    })
}
/// Execute one tool call, returning the completed/errored tool part and the text content.
fn execute_tool(
    paths: &devcode_util::Paths,
    call: &ToolCall,
    workdir: &std::path::Path,
    session_id: &str,
    plugins: &devcode_plugin::Host,
    mcp: &mut devcode_mcp::Runtime,
    skills: &[devcode_skills::Skill],
) -> Value {
    let started = now_millis();
    let arguments: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
    let cwd = workdir.to_path_buf();
    let input_path = |key: &str| {
        arguments[key].as_str().map(|value| {
            let path = std::path::PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
    };
    let result: Result<String, String> = match call.name.as_str() {
        "read" => {
            let path = input_path("path").ok_or_else(|| "read requires a path".to_string());
            path.and_then(|path| {
                devcode_tools::read(
                    &path,
                    arguments["offset"].as_u64().map(|value| value as usize),
                    arguments["limit"].as_u64().map(|value| value as usize),
                )
                .map(|out| out.text)
                .map_err(|error| error.to_string())
            })
        }
        "write" => input_path("path")
            .ok_or_else(|| "write requires a path".to_string())
            .and_then(|path| {
                devcode_tools::write(&path, arguments["content"].as_str().unwrap_or(""))
                    .map_err(|error| error.to_string())
            }),
        "edit" => input_path("path")
            .ok_or_else(|| "edit requires a path".to_string())
            .and_then(|path| {
                devcode_tools::edit(
                    &path,
                    arguments["oldString"].as_str().unwrap_or(""),
                    arguments["newString"].as_str().unwrap_or(""),
                    arguments["replaceAll"].as_bool().unwrap_or(false),
                )
                .map_err(|error| error.to_string())
            }),
        "glob" => {
            let root = input_path("path").unwrap_or_else(|| cwd.clone());
            devcode_tools::glob(
                arguments["pattern"].as_str().unwrap_or("*"),
                &root,
                arguments["hidden"].as_bool().unwrap_or(false),
                arguments["limit"].as_u64().map(|value| value as usize),
            )
            .map(|paths| {
                if paths.is_empty() {
                    "No files matched".to_owned()
                } else {
                    paths.join("\n")
                }
            })
            .map_err(|error| error.to_string())
        }
        "grep" => {
            let root = input_path("path").unwrap_or_else(|| cwd.clone());
            devcode_tools::grep(
                arguments["pattern"].as_str().unwrap_or(""),
                &root,
                arguments["include"].as_str(),
                arguments["literal"].as_bool().unwrap_or(false),
                arguments["caseSensitive"].as_bool().unwrap_or(true),
                arguments["limit"].as_u64().map(|value| value as usize),
            )
            .map(|matches| {
                devcode_tools::grep_to_model_content(
                    &matches,
                    matches.len() >= devcode_tools::SEARCH_DEFAULT_LIMIT,
                    devcode_tools::SEARCH_DEFAULT_LIMIT,
                )
            })
            .map_err(|error| error.to_string())
        }
        "shell" => {
            let command = arguments["command"]
                .as_str()
                .ok_or_else(|| "shell requires a command".to_string());
            command.and_then(|command| {
                devcode_tools::shell(
                    paths,
                    command,
                    Some(
                        arguments["workdir"]
                            .as_str()
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|| cwd.clone())
                            .as_path(),
                    ),
                    arguments["timeout"].as_u64(),
                    Some(session_id),
                )
                .map(|out| {
                    let mut text = out.output;
                    if let Some(code) = out.exit_code {
                        if code != 0 {
                            text.push_str(&format!("\n(exit code: {code})"));
                        }
                    }
                    if out.timed_out {
                        text.push_str("\n(command timed out and was killed)");
                    }
                    text
                })
                .map_err(|error| error.to_string())
            })
        }
        "skill" => arguments["id"]
            .as_str()
            .ok_or_else(|| "skill requires an id".to_string())
            .and_then(|id| {
                skills
                    .iter()
                    .find(|skill| skill.id == id)
                    .map(devcode_skills::to_model_output)
                    .ok_or_else(|| format!("unable to load skill {id}"))
            }),
        "list_mcp_resources" => {
            let server = arguments.get("server").and_then(Value::as_str);
            mcp.resource_catalog(server, std::time::Duration::from_secs(120))
                .map(|catalog| catalog.to_string())
        }
        "read_mcp_resource" => {
            let server = arguments
                .get("server")
                .and_then(Value::as_str)
                .ok_or_else(|| "read_mcp_resource requires a server".to_string());
            let uri = arguments
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| "read_mcp_resource requires a uri".to_string());
            server.and_then(|server| {
                uri.and_then(|uri| {
                    mcp.read_resource(server, uri, std::time::Duration::from_secs(120))
                        .map(|content| {
                            let capped = devcode_tools::cap_output(paths, &content.to_string());
                            let mut text = capped.text;
                            if let Some(path) = capped.overflow_path {
                                text.push_str(&format!(
                                    "\n(output truncated; the full content was saved to {})",
                                    path.display()
                                ));
                            }
                            text
                        })
                })
            })
        }
        other => {
            if let Some(result) = mcp.call(other, &arguments, std::time::Duration::from_secs(120)) {
                result
            } else if let Some(result) = plugins.call_tool(other, &arguments) {
                result.map(|value| value.to_string())
            } else {
                Err(format!("unknown tool: {other}"))
            }
        }
    };
    let finished = now_millis();
    match result {
        Ok(output) => completed_tool_part(call, &arguments, &output, json!({}), started, finished),
        Err(message) => error_tool_part(call, &arguments, &message, started, finished),
    }
}
fn emit(
    conn: &mut rusqlite::Connection,
    session_id: &str,
    event_type: &str,
    data: Value,
    created: i64,
) -> Result<devcode_core::SerializedEvent, String> {
    devcode_core::append_durable(conn, session_id, event_type, &data, created, None)
        .map_err(|error| format!("failed to append {event_type}: {error}"))
}
fn tokens_value(usage: &devcode_ai::Usage) -> Value {
    json!({
        "input": usage.input,
        "output": usage.output,
        "reasoning": usage.reasoning,
        "cache": { "read": usage.cache_read, "write": usage.cache_write },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_plugin_host(tag: &str) -> (devcode_plugin::Host, PathBuf) {
        let workspace_target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("debug")
            .join("libdevcode_plugin_fixture.so");
        if !workspace_target.exists() {
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "--locked", "-p", "devcode-plugin-fixture"])
                .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
                .status()
                .expect("failed to invoke cargo for the fixture build");
            assert!(status.success(), "fixture cdylib build failed");
        }
        let dir = std::env::temp_dir().join(format!(
            "devcode-runner-plugin-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("fixture");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::copy(&workspace_target, nested.join("libfixture.so")).unwrap();
        std::fs::write(
            nested.join("plugin.json"),
            serde_json::json!({
                "id": "fixture",
                "version": "0.1.0-rust",
                "description": "fixture",
                "library": "libfixture.so",
            })
            .to_string(),
        )
        .unwrap();
        let mut host = devcode_plugin::Host::empty();
        let (loaded, errors) = unsafe { host.load_dir(&dir) };
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(loaded.len(), 1);
        (host, dir)
    }

    #[test]
    fn plugin_tools_appear_in_specs_and_route_execution() {
        let (host, dir) = fixture_plugin_host("9c");
        let mut empty_mcp = devcode_mcp::Runtime::connect_all(
            &std::collections::BTreeMap::new(),
            std::time::Duration::from_millis(10),
            std::path::Path::new(":memory:"),
        );
        let specs = tool_specs(&host, &empty_mcp);
        let echo = specs
            .iter()
            .find(|spec| spec.name == "fixture_echo")
            .expect("plugin tool missing from specs");
        assert_eq!(echo.description, "echoes its input");
        assert!(
            specs.iter().any(|spec| spec.name == "shell"),
            "builtins preserved"
        );
        assert_eq!(
            specs
                .iter()
                .filter(|spec| spec.name == "fixture_echo")
                .count(),
            1,
            "no duplicates"
        );
        let paths = devcode_util::paths("devcode");
        let call = ToolCall {
            id: "call_1".into(),
            name: "fixture_echo".into(),
            arguments: r#"{"text":"hello"}"#.into(),
        };
        let skills = load_skills(&paths, &host);
        let part = execute_tool(
            &paths,
            &call,
            &dir,
            "ses_test",
            &host,
            &mut empty_mcp,
            &skills,
        );
        assert_eq!(part["state"]["status"], "completed");
        let content = part["state"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(content.contains("hello"), "echo output missing: {content}");
        // builtin tools shadow same-named plugin tools and still take precedence,
        // so a plugin cannot override shell/grep/etc.
        let call = ToolCall {
            id: "call_2".into(),
            name: "shell".into(),
            arguments: r#"{"command":"printf builtin-wins"}"#.into(),
        };
        let skills = load_skills(&paths, &host);
        let part = execute_tool(
            &paths,
            &call,
            &dir,
            "ses_test",
            &host,
            &mut empty_mcp,
            &skills,
        );
        assert_eq!(part["state"]["status"], "completed");
        assert!(part["state"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("builtin-wins"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_resource_tools_appear_in_specs_and_route() {
        let (host, dir) = fixture_plugin_host("mcp-resources");
        let mut empty_mcp = devcode_mcp::Runtime::connect_all(
            &std::collections::BTreeMap::new(),
            std::time::Duration::from_millis(10),
            std::path::Path::new(":memory:"),
        );
        let specs = tool_specs(&host, &empty_mcp);
        for name in ["list_mcp_resources", "read_mcp_resource"] {
            assert!(
                specs.iter().any(|spec| spec.name == name),
                "{name} missing from builtin tool specs"
            );
        }
        let paths = devcode_util::paths("devcode");
        let skills: Vec<devcode_skills::Skill> = Vec::new();

        // No configured servers: the catalog is empty, not an error.
        let call = ToolCall {
            id: "call_resources_1".into(),
            name: "list_mcp_resources".into(),
            arguments: r#"{}"#.into(),
        };
        let part = execute_tool(&paths, &call, &dir, "ses_test", &host, &mut empty_mcp, &skills);
        assert_eq!(part["state"]["status"], "completed");
        let output: Value = serde_json::from_str(part["state"]["content"][0]["text"].as_str().unwrap())
            .unwrap();
        assert_eq!(output, json!({ "resources": [], "templates": [] }));

        // Unknown server in the filter: Bun's MCP.NotFoundError message, as a tool error part.
        let call = ToolCall {
            id: "call_resources_2".into(),
            name: "list_mcp_resources".into(),
            arguments: r#"{"server":"unknown"}"#.into(),
        };
        let part = execute_tool(&paths, &call, &dir, "ses_test", &host, &mut empty_mcp, &skills);
        assert_eq!(part["state"]["status"], "error");
        assert!(part["state"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("MCP server not found: unknown"));

        // read_mcp_resource requires server and uri.
        let call = ToolCall {
            id: "call_resources_3".into(),
            name: "read_mcp_resource".into(),
            arguments: r#"{}"#.into(),
        };
        let part = execute_tool(&paths, &call, &dir, "ses_test", &host, &mut empty_mcp, &skills);
        assert_eq!(part["state"]["status"], "error");
        assert!(part["state"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires a server"));

        let call = ToolCall {
            id: "call_resources_4".into(),
            name: "read_mcp_resource".into(),
            arguments: r#"{"server":"unknown","uri":"fixture://demo"}"#.into(),
        };
        let part = execute_tool(&paths, &call, &dir, "ses_test", &host, &mut empty_mcp, &skills);
        assert_eq!(part["state"]["status"], "error");
        assert!(part["state"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("MCP server not found: unknown"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn row(row_type: &str, data: Value, parts: Vec<Value>) -> HistoryRow {
        HistoryRow {
            id: format!("msg_{row_type}"),
            row_type: row_type.to_owned(),
            seq: 0,
            data,
            parts,
        }
    }

    #[test]
    fn user_and_assistant_roundtrip() {
        let rows = vec![
            row("user", json!({ "text": "hello" }), vec![]),
            row(
                "assistant",
                json!({ "agent": "build" }),
                vec![
                    json!({ "type": "text", "text": "working" }),
                    json!({ "type": "tool", "id": "call_1", "name": "shell",
                            "state": { "status": "completed", "input": { "command": "ls" },
                                       "content": [{ "type": "text", "text": "out" }] } }),
                ],
            ),
        ];
        let messages = history_to_chat(&rows);
        assert!(matches!(&messages[0], ChatMessage::User(text) if text == "hello"));
        match &messages[1] {
            ChatMessage::Assistant { text, tool_calls } => {
                assert_eq!(text, "working");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].arguments, "{\"command\":\"ls\"}");
            }
            other => panic!("expected assistant, got {other:?}"),
        }
        assert!(
            matches!(&messages[2], ChatMessage::Tool { call_id, content } if call_id == "call_1" && content == "out")
        );
    }

    #[test]
    fn skips_empty_messages_and_unknown_types() {
        let rows = vec![
            row("user", json!({ "text": "" }), vec![]),
            row("model-switched", json!({}), vec![]),
            row(
                "assistant",
                json!({}),
                vec![json!({ "type": "reasoning", "text": "hmm" })],
            ),
        ];
        assert!(history_to_chat(&rows).is_empty());
    }

    #[test]
    fn string_input_tool_arguments_survive() {
        let rows = vec![row(
            "assistant",
            json!({}),
            vec![json!({ "type": "tool", "id": "c", "name": "edit",
                        "state": { "status": "completed", "input": "raw args", "content": [] } })],
        )];
        let messages = history_to_chat(&rows);
        match &messages[0] {
            ChatMessage::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls[0].arguments, "raw args")
            }
            other => panic!("expected assistant, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod instruction_tests {
    use super::*;

    fn test_paths(root: &Path) -> devcode_util::Paths {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        devcode_util::Paths {
            home: root.join("home"),
            data: root.join("data"),
            cache: root.join("cache"),
            config: root.join("config"),
            state: root.join("state"),
            tmp: root.join("tmp"),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("devcode-runner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn instructions_walk_nearest_first_with_global() {
        let root = scratch("inst-walk");
        let paths = test_paths(&root);
        let sub = paths.home.join("repo").join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(paths.config.join("AGENTS.md"), "global instructions").unwrap();
        std::fs::write(paths.home.join("repo").join("AGENTS.md"), "repo instructions").unwrap();
        std::fs::write(sub.join("AGENTS.md"), "sub instructions").unwrap();
        let rendered = load_instructions(&paths, &sub, &sub);
        let global = rendered.find("global instructions").expect("global file missing");
        let sub_pos = rendered.find("sub instructions").expect("sub file missing");
        let repo = rendered.find("repo instructions").expect("repo file missing");
        assert!(global < sub_pos, "global must render before project files");
        assert!(sub_pos < repo, "nearest directory must render first");
        assert!(rendered.contains(&format!("Instructions from: {}", sub.join("AGENTS.md").display())));
    }

    #[test]
    fn instructions_configured_file_entries() {
        let root = scratch("inst-configured");
        let paths = test_paths(&root);
        let sub = paths.home.join("work");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("extra.md"), "extra doc").unwrap();
        std::fs::write(paths.home.join("from-home.md"), "home doc").unwrap();
        std::fs::write(
            paths.config.join("devcode.jsonc"),
            r#"{
  // both a relative glob and a home-relative entry
  "instructions": ["extra.md", "~/from-home.md"]
}"#,
        )
        .unwrap();
        let rendered = load_instructions(&paths, &sub, &sub);
        assert!(rendered.contains("extra doc"), "relative pattern missing: {rendered}");
        assert!(rendered.contains("home doc"), "~/ entry missing: {rendered}");
        assert!(rendered.contains("Instructions from:"), "render must carry paths");
    }

    #[test]
    fn instructions_configured_url_entry() {
        let root = scratch("inst-url");
        let paths = test_paths(&root);
        let sub = paths.home.join("work");
        std::fs::create_dir_all(&sub).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = "remote instructions";
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            use std::io::{Read, Write};
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });

        std::fs::write(
            paths.config.join("devcode.jsonc"),
            format!(r#"{{ "instructions": ["http://{address}/instructions.md"] }}"#),
        )
        .unwrap();
        let rendered = load_instructions(&paths, &sub, &sub);
        assert!(
            rendered.contains(&format!("Instructions from: http://{address}/instructions.md")),
            "url source must render as its path: {rendered}"
        );
        assert!(rendered.contains("remote instructions"), "url content missing: {rendered}");
    }

    #[test]
    fn instructions_git_toplevel_bounds_the_walk() {
        let root = scratch("inst-git");
        let paths = test_paths(&root);
        let repo = root.join("checkout");
        let nested = repo.join("packages").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        // A bound outside any repo (git init must succeed or this test is skipped).
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repo)
            .status();
        if !init.map(|status| status.success()).unwrap_or(false) {
            eprintln!("git unavailable; skipping");
            return;
        }
        // Start is NOT under home here, so the walk bound is the git toplevel.
        std::fs::write(repo.join("AGENTS.md"), "repo-root instructions").unwrap();
        std::fs::write(root.join("AGENTS.md"), "outside-repo instructions").unwrap();
        let root_path = git_toplevel(&nested);
        assert_eq!(root_path.as_deref(), Some(repo.as_path()));
        let rendered = load_instructions(&paths, &nested, root_path.as_deref().unwrap_or(&nested));
        assert!(rendered.contains("repo-root instructions"), "repo root must be in the walk: {rendered}");
        assert!(!rendered.contains("outside-repo instructions"), "walk must stop at the git toplevel: {rendered}");
    }

    #[test]
    fn date_string_matches_todatestring_shape() {
        let date = date_string();
        let bytes = date.as_bytes();
        assert_eq!(bytes.len(), 15, "expected e.g. 'Thu Nov 19 2026': {date}");
        assert_eq!(bytes[3], b' ');
        assert_eq!(bytes[7], b' ');
        assert_eq!(bytes[10], b' ');
    }

    #[test]
    fn system_prompt_carries_builtins_and_instructions() {
        let root = scratch("inst-prompt");
        let paths = test_paths(&root);
        let sub = paths.home.join("repo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(paths.config.join("AGENTS.md"), "prompt-level instructions").unwrap();
        let skills = vec![];
        let prompt = system_prompt(&paths, &skills, &sub);
        assert!(prompt.contains("prompt-level instructions"), "instructions missing from system prompt");
        assert!(prompt.contains("Instructions from:"), "render shape missing");
        assert!(prompt.contains("Today's date:"), "date builtin missing");
        assert!(prompt.contains("<env>"), "environment builtin missing");
        assert!(prompt.contains(&format!("Working directory: {}", sub.display())));
        assert!(prompt.contains("Is directory a git repo: no"));
    }
}
