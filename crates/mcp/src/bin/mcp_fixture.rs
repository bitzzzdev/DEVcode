//! Canned stdio MCP server for integration tests. Newline-delimited JSON-RPC.
//! Behavior: initialize -> ok; notifications/initialized -> no reply; tools/list -> one
//! tool "echo"; tools/call echo -> {echo: arguments}; ping -> {}; anything else ->
//! method-not-found error.

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
        let id = message.get("id").cloned();
        let is_notification = id.is_none();
        let result = match method {
            "initialize" => Some(serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "fixture", "version": "0.0.0" },
            })),
            "ping" => Some(serde_json::json!({})),
            "tools/list" => Some(serde_json::json!({
                "tools": [{
                    "name": "echo",
                    "description": "echoes input",
                    "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
                }],
            })),
            "tools/call" => {
                let args = message["params"]["arguments"].clone();
                Some(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({ "echo": args }).to_string() }],
                    "isError": false,
                }))
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
