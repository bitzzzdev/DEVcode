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

    let outcome = step_loop(
        conn,
        paths,
        &execution.model,
        &directory,
        execution.max_steps,
        on_text,
        on_tool,
        &session_id,
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
    let outcome = step_loop(
        conn, paths, model, &directory, max_steps, on_text, on_tool, session_id,
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
) -> Result<(), String> {
    let model_json = model_json(model);
    let tools = tool_specs();
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
                Some(SYSTEM_PROMPT),
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
            parts.push(execute_tool(paths, call, directory, session_id));
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
fn tool_specs() -> Vec<ToolSpec> {
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
        other => Err(format!("unknown tool: {other}")),
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
