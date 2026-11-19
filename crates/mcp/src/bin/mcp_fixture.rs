//! Canned stdio MCP server for integration tests. Newline-delimited JSON-RPC.
//! Behavior: initialize -> ok (advertises tools, resources and prompts unless
//! hidden); notifications/initialized -> no reply; tools/list -> one tool
//! "echo"; tools/call echo -> {echo: arguments}; ping -> {}; resources/list ->
//! one resource "fixture://demo"; resources/templates/list -> one template
//! "fixture://item/{id}"; resources/read -> text contents for fixture:// URIs
//! (error for others); prompts/list -> one prompt "greet" with argument
//! "who"; prompts/get greet -> two text messages (exercises newline join);
//! anything else -> method-not-found error.
//! FIXTURE_NO_RESOURCES=1 hides the resources capability; FIXTURE_NO_PROMPTS=1
//! hides the prompts capability; FIXTURE_NO_TOOLS=1 hides the tools capability
//! and answers tools/list with method-not-found (prompts-only server shape).

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
            continue;
        };
        // Capability hiders for the capability-gate tests.
        let no_resources = std::env::var_os("FIXTURE_NO_RESOURCES").is_some();
        let no_prompts = std::env::var_os("FIXTURE_NO_PROMPTS").is_some();
        let no_tools = std::env::var_os("FIXTURE_NO_TOOLS").is_some();
        let id = message.get("id").cloned();
        let is_notification = id.is_none();
        let result = match method {
            "initialize" => {
                let mut capabilities = serde_json::Map::new();
                if !no_tools {
                    capabilities.insert("tools".into(), serde_json::json!({ "listChanged": false }));
                }
                if !no_resources {
                    capabilities.insert(
                        "resources".into(),
                        serde_json::json!({ "subscribe": false, "listChanged": false }),
                    );
                }
                if !no_prompts {
                    capabilities.insert(
                        "prompts".into(),
                        serde_json::json!({ "listChanged": false }),
                    );
                }
                Some(serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": capabilities,
                    "serverInfo": { "name": "fixture", "version": "0.0.0" },
                }))
            }
            "ping" => Some(serde_json::json!({})),
            "tools/list" => {
                if no_tools {
                    None // method not found, like a prompts-only server
                } else {
                    Some(serde_json::json!({
                        "tools": [{
                            "name": "echo",
                            "description": "echoes input",
                            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
                        }],
                    }))
                }
            }
            "tools/call" => {
                let args = message["params"]["arguments"].clone();
                Some(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({ "echo": args }).to_string() }],
                    "isError": false,
                }))
            }
            "resources/list" => Some(serde_json::json!({
                "resources": [{
                    "uri": "fixture://demo",
                    "name": "demo",
                    "description": "fixture document",
                    "mimeType": "text/plain",
                }],
            })),
            "resources/templates/list" => Some(serde_json::json!({
                "resourceTemplates": [{
                    "uriTemplate": "fixture://item/{id}",
                    "name": "item",
                    "description": "parameterized fixture",
                    "mimeType": "text/plain",
                }],
            })),
            "prompts/list" => Some(serde_json::json!({
                "prompts": [{
                    "name": "greet",
                    "description": "greets someone",
                    "arguments": [{ "name": "who", "description": "who to greet", "required": true }],
                }],
            })),
            "prompts/get" => {
                let name = message["params"]["name"].as_str().unwrap_or("").to_owned();
                let who = message["params"]["arguments"]["who"].as_str().unwrap_or("").to_owned();
                match name.as_str() {
                    "greet" => Some(serde_json::json!({
                        "messages": [
                            { "role": "user", "content": { "type": "text", "text": format!("Hello, {who}!") } },
                            { "role": "assistant", "content": { "type": "text", "text": "second line" } },
                        ],
                    })),
                    _ => {
                        let reply = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id.unwrap_or(serde_json::Value::Null),
                            "error": { "code": -32602, "message": format!("unknown prompt: {name}") },
                        });
                        let _ = writeln!(stdout, "{reply}");
                        let _ = stdout.flush();
                        continue;
                    }
                }
            }
            "resources/read" => {
                let uri = message["params"]["uri"].as_str().unwrap_or("").to_owned();
                match uri.as_str() {
                    "fixture://demo" | "fixture://demo/" => Some(serde_json::json!({
                        "contents": [{ "type": "text", "uri": uri, "text": "fixture body", "mimeType": "text/plain" }],
                    })),
                    _ if uri.starts_with("fixture://item/") => Some(serde_json::json!({
                        "contents": [{ "type": "text", "uri": uri, "text": format!("item {}", &uri["fixture://item/".len()..]), "mimeType": "text/plain" }],
                    })),
                    _ => {
                        let reply = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id.unwrap_or(serde_json::Value::Null),
                            "error": { "code": -32602, "message": format!("unknown resource: {uri}") },
                        });
                        let _ = writeln!(stdout, "{reply}");
                        let _ = stdout.flush();
                        continue;
                    }
                }
            }
            _ => None,
        };
        if is_notification {
            continue;
        }
        let reply = match result {
            Some(result) => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id.unwrap_or(serde_json::Value::Null), "result": result })
            }
            None => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id.unwrap_or(serde_json::Value::Null), "error": { "code": -32601, "message": format!("method not found: {method}") } })
            }
        };
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}
