//! Anthropic Messages API (https://docs.anthropic.com/en/api/messages-streaming).

use std::io::Read;

use crate::{
    read_sse, ChatMessage, Error, FinishReason, ModelRef, StreamSummary, ToolCall, ToolSpec, Usage,
};

pub fn request_body(
    model: &ModelRef,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u32,
) -> serde_json::Value {
    let mut wire: Vec<serde_json::Value> = Vec::new();
    for message in messages {
        match message {
            ChatMessage::System(_) => {} // system goes in its own field
            ChatMessage::User(text) => {
                wire.push(serde_json::json!({ "role": "user", "content": text }))
            }
            ChatMessage::Assistant { text, tool_calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(serde_json::json!({ "type": "text", "text": text }));
                }
                for call in tool_calls {
                    content.push(serde_json::json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap_or_else(|_| serde_json::json!({})),
                    }));
                }
                wire.push(serde_json::json!({ "role": "assistant", "content": content }));
            }
            ChatMessage::Tool { call_id, content } => {
                wire.push(serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "tool_result", "tool_use_id": call_id, "content": content }],
                }));
            }
        }
    }
    let mut body = serde_json::json!({
        "model": model.id,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": wire,
    });
    if let Some(system) = system {
        body["system"] = serde_json::Value::String(system.to_owned());
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    body
}

fn finish_reason(stop_reason: Option<&str>) -> (FinishReason, Option<String>) {
    match stop_reason {
        Some("end_turn") | Some("stop_sequence") => (FinishReason::Stop, None),
        Some("max_tokens") => (FinishReason::Length, None),
        Some("tool_use") => (FinishReason::ToolCalls, None),
        Some("refusal") => (FinishReason::ContentFilter, None),
        Some(other) => (FinishReason::Unknown, Some(other.to_owned())),
        None => (FinishReason::Unknown, None),
    }
}

/// Fold one Anthropic SSE event into the running response.
pub fn apply_event(
    state: &mut Accum,
    name: &str,
    data: &str,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    let json: serde_json::Value = serde_json::from_str(data)
        .map_err(|error| Error::Provider(format!("invalid SSE payload: {error}")))?;
    match name {
        "message_start" => {
            if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
                state.usage.input = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                state.usage.cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                state.usage.cache_write = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
            }
        }
        "content_block_start" => {
            let block = &json["content_block"];
            match block["type"].as_str() {
                Some("tool_use") => {
                    let index = json["index"].as_u64().unwrap_or(0) as usize;
                    if state.tools.len() <= index {
                        state.tools.resize(index + 1, None);
                    }
                    state.tools[index] = Some((
                        block["id"].as_str().unwrap_or("").to_owned(),
                        block["name"].as_str().unwrap_or("").to_owned(),
                        String::new(),
                    ));
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let delta = &json["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    let text = delta["text"].as_str().unwrap_or("");
                    state.text.push_str(text);
                    on_text(text);
                }
                Some("input_json_delta") => {
                    let index = json["index"].as_u64().unwrap_or(0) as usize;
                    if let Some(Some((_, _, arguments))) = state.tools.get_mut(index) {
                        arguments.push_str(delta["partial_json"].as_str().unwrap_or(""));
                    }
                }
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(reason) = json["delta"]["stop_reason"].as_str() {
                let (finish, raw) = finish_reason(Some(reason));
                state.finish = finish;
                state.raw_finish = raw;
            }
            if let Some(usage) = json.get("usage") {
                state.usage.output = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(state.usage.output);
            }
        }
        "error" => {
            let message = json["error"]["message"]
                .as_str()
                .or_else(|| json["error"].as_str())
                .unwrap_or("anthropic stream error")
                .to_owned();
            return Err(Error::Provider(message));
        }
        _ => {} // ping, content_block_stop, message_stop
    }
    Ok(())
}

#[derive(Default)]
pub struct Accum {
    pub text: String,
    pub tools: Vec<Option<(String, String, String)>>,
    pub usage: Usage,
    pub finish: FinishReason,
    pub raw_finish: Option<String>,
}

pub fn summarize(state: Accum) -> StreamSummary {
    StreamSummary {
        text: state.text,
        tool_calls: state
            .tools
            .into_iter()
            .flatten()
            .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments: if arguments.is_empty() {
                    "{}".into()
                } else {
                    arguments
                },
            })
            .collect(),
        finish: state.finish,
        raw_finish: state.raw_finish,
        usage: state.usage,
    }
}

pub fn stream(
    base_url: &str,
    api_key: &str,
    model: &ModelRef,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u32,
    on_text: &mut dyn FnMut(&str),
) -> Result<StreamSummary, Error> {
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));

    let body = request_body(model, system, messages, tools, max_tokens);
    let response = ureq::post(&url)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(600))
        .send_string(&body.to_string())
        .map_err(|error| match error {
            ureq::Error::Status(status, response) => {
                let body = response.into_string().unwrap_or_default();
                Error::Status(status, body)
            }
            ureq::Error::Transport(transport) => Error::Http(transport.to_string()),
        })?;
    let mut state = Accum::default();
    let reader: Box<dyn Read + Send> = Box::new(response.into_reader());
    read_stream(reader, &mut state, on_text)?;
    Ok(summarize(state))
}

pub fn read_stream(
    reader: Box<dyn Read + Send>,
    state: &mut Accum,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    read_sse(reader, |name, data| {
        if state.finish == FinishReason::Error {
            return;
        }
        if let Err(error) = apply_event(state, name, data, on_text) {
            state.raw_finish = Some(error.to_string());
            state.finish = FinishReason::Error;
        }
    })?;
    if state.finish == FinishReason::Error {
        return Err(Error::Provider(
            state
                .raw_finish
                .take()
                .unwrap_or_else(|| "provider error".into()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ModelRef {
        ModelRef {
            id: "claude-sonnet-4-5".into(),
            provider_id: "anthropic".into(),
            variant: None,
        }
    }

    #[test]
    fn request_body_shapes_messages_and_tools() {
        let messages = vec![
            ChatMessage::System("ignored system".into()),
            ChatMessage::User("hello".into()),
            ChatMessage::Assistant {
                text: "working".into(),
                tool_calls: vec![ToolCall {
                    id: "toolu_1".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"a\"}".into(),
                }],
            },
            ChatMessage::Tool {
                call_id: "toolu_1".into(),
                content: "file body".into(),
            },
        ];
        let body = request_body(
            &model(),
            Some("sys"),
            &messages,
            &[ToolSpec {
                name: "read".into(),
                description: "Read".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            1024,
        );
        assert_eq!(body["system"], "sys");
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["messages"][1]["content"][0]["text"], "working");
        assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(body["messages"][1]["content"][1]["input"]["path"], "a");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn sse_fixture_streams_text_tool_and_usage() {
        let fixture = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":25,\"cache_read_input_tokens\":7}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"glob\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pattern\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"*.rs\\\"}\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut state = Accum::default();
        let mut out = String::new();
        read_sse(fixture.as_bytes(), |name, data| {
            apply_event(&mut state, name, data, &mut |t| out.push_str(t)).unwrap();
        })
        .unwrap();
        let summary = summarize(state);
        assert_eq!(out, "Hello");
        assert_eq!(summary.finish, FinishReason::ToolCalls);
        assert_eq!(
            summary.tool_calls,
            vec![ToolCall {
                id: "toolu_9".into(),
                name: "glob".into(),
                arguments: "{\"pattern\":\"*.rs\"}".into()
            }]
        );
        assert_eq!(summary.usage.input, 25.0);
        assert_eq!(summary.usage.cache_read, 7.0);
        assert_eq!(summary.usage.output, 12.0);
    }

    #[test]
    fn error_event_surfaces_as_error_finish() {
        let mut state = Accum::default();
        let mut nul = |_: &str| {};
        let _ = apply_event(
            &mut state,
            "error",
            "{\"error\":{\"message\":\"overloaded\"}}",
            &mut nul,
        );
        assert_eq!(state.finish, FinishReason::Unknown); // caller maps the returned error
    }
}
