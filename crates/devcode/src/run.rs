//! Headless agent loop: `DEVcode run "<prompt>"`.
//!
//! Creates (or continues with --session) a session in the current directory,
//! streams one user prompt through the selected model, executes the six core
//! tools, and persists everything to the same on-disk format the Bun build
//! writes: durable events first, projections second. Deviations/limits:
//! - Static system prompt; AGENTS.md instruction discovery is not ported yet.
//! - Cost is recorded as 0 until a pricing table exists (usage tokens are recorded).
//! - Tool calls are executed after the stream completes (no streaming tool loop).
//! - Success/failure only: no retries, no permission prompts, no compaction.

use std::io::Write as _;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use devcode_ai::{ChatMessage, ModelRef, ToolCall, ToolSpec};
use devcode_core::session_message::HistoryRow;

const SYSTEM_PROMPT: &str =
    "You are DEVcode, an open-source AI coding agent running in a headless CLI. \
     You have tools to read, write, edit files, search (glob/grep), and run shell commands. \
     Work autonomously: make the change rather than describing it. Keep responses concise. \
     When you are done, stop calling tools. Always use absolute paths or paths relative to the working directory.";

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

pub struct RunOptions {
    pub prompt: String,
    pub model: ModelRef,
    pub session_id: Option<String>,
    pub workdir: std::path::PathBuf,
    pub max_steps: u32,
}

pub fn parse_args(args: &[String]) -> Result<RunOptions, String> {
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut model: Option<String> = std::env::var("DEVCODE_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut session_id: Option<String> = None;
    let mut workdir: Option<std::path::PathBuf> = None;
    let mut max_steps: u32 = 32;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--model" => {
                index += 1;
                model = Some(args.get(index).cloned().ok_or("--model requires a value")?);
            }
            "--session" => {
                index += 1;
                session_id = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("--session requires a value")?,
                );
            }
            "--workdir" => {
                index += 1;
                workdir = Some(std::path::PathBuf::from(
                    args.get(index)
                        .cloned()
                        .ok_or("--workdir requires a value")?,
                ));
            }
            "--max-steps" => {
                index += 1;
                max_steps = args
                    .get(index)
                    .and_then(|value| value.parse::<u32>().ok())
                    .ok_or("--max-steps requires a positive integer")?;
            }
            "--help" | "-h" => return Err(help()),
            other => prompt_parts.push(other.to_owned()),
        }
        index += 1;
    }
    let prompt = prompt_parts.join(" ").trim().to_owned();
    if prompt.is_empty() {
        return Err(format!("a prompt is required\n{}", help()));
    }
    let model = model.ok_or_else(|| {
        format!(
            "no model selected; pass --model provider/model[#variant] or set DEVCODE_MODEL\n{}",
            help()
        )
    })?;
    let model = devcode_ai::parse_model(&model)?;
    Ok(RunOptions {
        prompt,
        model,
        session_id,
        workdir: workdir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into())),
        max_steps,
    })
}

fn help() -> String {
    "usage: DEVcode run [--model provider/model[#variant]] [--session ID] [--workdir PATH] [--max-steps N] <prompt>".to_owned()
}

/// Convert the stored transcript to model-facing messages (to-llm-message.ts, simplified).
fn history_to_chat(rows: &[HistoryRow]) -> Vec<ChatMessage> {
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

/// One full run: enqueue the prompt, stream steps until the model stops calling
/// tools (or --max-steps is exhausted), then mark the session idle.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    match run_inner(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(options: &RunOptions) -> Result<(), String> {
    let paths = devcode_util::paths("devcode");
    let database_path = devcode_core::database_path(&paths);
    let mut database = devcode_core::Database::open(&database_path).map_err(|error| {
        format!(
            "failed to open database {}: {error}",
            database_path.display()
        )
    })?;
    let conn = &mut database.conn;

    let directory = options
        .workdir
        .canonicalize()
        .map_err(|error| format!("--workdir {}: {error}", options.workdir.display()))?;
    let directory_string = directory.to_string_lossy().into_owned();

    let record = match &options.session_id {
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
    eprintln!("session: {session_id} ({directory_string})");

    // Admit the prompt as an inbox item, then deliver it (durable event + projection each).
    let message_id = format!("msg_{}", devcode_core::ascending());
    let item = json!({
        "type": "user",
        "payload": { "text": options.prompt },
        "delivery": "steer",
    });
    let enqueued = emit(
        conn,
        &session_id,
        "session.inbox.enqueued",
        json!({ "sessionID": session_id, "inboxID": message_id, "item": item }),
        now_millis(),
    )?;
    devcode_core::session_message::project_admitted(
        conn,
        &message_id,
        &session_id,
        &item,
        enqueued.seq,
        enqueued.created,
    )
    .map_err(|error| format!("failed to project inbox admission: {error}"))?;
    let delivered = emit(
        conn,
        &session_id,
        "session.inbox.delivered",
        json!({ "sessionID": session_id, "inboxID": message_id }),
        enqueued.created,
    )?;
    if let Some((row_type, payload)) =
        devcode_core::session_message::project_delivered(conn, &message_id, &session_id)
            .map_err(|error| format!("failed to consume inbox item: {error}"))?
    {
        let mut data =
            json!({ "text": payload["text"].clone(), "time": { "created": delivered.created } });
        if let Some(files) = payload.get("files") {
            data["files"] = files.clone();
        }
        if let Some(agents) = payload.get("agents") {
            data["agents"] = agents.clone();
        }
        if let Some(skills) = payload.get("skills") {
            data["skills"] = skills.clone();
        }
        if let Some(metadata) = payload.get("metadata") {
            data["metadata"] = metadata.clone();
        }
        devcode_core::session_message::insert_message(
            conn,
            &message_id,
            &session_id,
            &row_type,
            delivered.seq,
            delivered.created,
            data,
        )
        .map_err(|error| format!("failed to insert user message: {error}"))?;
    }

    let started = now_millis();
    emit(
        conn,
        &session_id,
        "session.execution.started",
        json!({ "sessionID": session_id }),
        started,
    )?;

    let model_json = {
        let mut map = serde_json::Map::new();
        map.insert("id".into(), Value::String(options.model.id.clone()));
        map.insert(
            "providerID".into(),
            Value::String(options.model.provider_id.clone()),
        );
        if let Some(variant) = &options.model.variant {
            map.insert("variant".into(), Value::String(variant.clone()));
        }
        Value::Object(map)
    };

    let tools = tool_specs();
    let outcome = step_loop(
        conn,
        &paths,
        options,
        &session_id,
        &directory,
        &model_json,
        &tools,
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
            Ok(())
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
            Err(message)
        }
    }
}
/// Stream steps until the model stops calling tools or the step budget runs out.
fn step_loop(
    conn: &mut rusqlite::Connection,
    paths: &devcode_util::Paths,
    options: &RunOptions,
    session_id: &str,
    directory: &std::path::Path,
    model_json: &Value,
    tools: &[ToolSpec],
) -> Result<(), String> {
    let mut exhausted = true;
    for _step in 0..options.max_steps {
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
            model_json,
            started,
        )
        .map_err(|error| format!("failed to insert assistant message: {error}"))?;

        let mut text = String::new();
        let mut first_token = false;
        let summary = {
            let mut on_text = |delta: &str| {
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
                print!("{delta}");
                let _ = std::io::stdout().flush();
            };
            devcode_ai::stream_chat(
                &options.model,
                Some(SYSTEM_PROMPT),
                &messages,
                tools,
                8192,
                &mut on_text,
            )
            .map_err(|error| error.to_string())?
        };
        let _ = std::io::stdout().flush();

        let finished = now_millis();
        let mut parts: Vec<Value> = Vec::new();
        if !text.is_empty() {
            parts.push(json!({ "type": "text", "text": text }));
        }
        for call in &summary.tool_calls {
            eprintln!(
                "[tool] {} {}",
                call.name,
                call.arguments.lines().next().unwrap_or("").trim()
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
            println!();
            exhausted = false;
            break;
        }
    }
    if exhausted {
        return Err(format!(
            "stopped after {} steps without a final answer",
            options.max_steps
        ));
    }
    Ok(())
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
