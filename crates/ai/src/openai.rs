//! OpenAI-compatible chat completions (OpenAI, OpenRouter, Groq, and the
//! many providers exposing /chat/completions). Streams SSE.

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
    if let Some(system) = system {
        wire.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for message in messages {
        match message {
            ChatMessage::System(text) => {
                wire.push(serde_json::json!({ "role": "system", "content": text }))
            }
            ChatMessage::User(text) => {
                wire.push(serde_json::json!({ "role": "user", "content": text }))
            }
            ChatMessage::Assistant { text, tool_calls } => {
                let mut message = serde_json::json!({ "role": "assistant" });
                if !text.is_empty() {
                    message["content"] = serde_json::Value::String(text.clone());
                } else {
                    message["content"] = serde_json::Value::Null;
                }
                if !tool_calls.is_empty() {
                    message["tool_calls"] = serde_json::Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                serde_json::json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": { "name": call.name, "arguments": call.arguments },
                                })
                            })
                            .collect(),
                    );
                }
                wire.push(message);
            }
            ChatMessage::Tool { call_id, content } => {
                wire.push(serde_json::json!({ "role": "tool", "tool_call_id": call_id, "content": content }));
            }
        }
    }
    let mut body = serde_json::json!({
        "model": model.id,
        "max_tokens": max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": wire,
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| serde_json::json!({
                    "type": "function",
                    "function": { "name": tool.name, "description": tool.description, "parameters": tool.input_schema },
                }))
                .collect(),
        );
    }
    body
}

fn finish_reason(reason: Option<&str>) -> (FinishReason, Option<String>) {
    match reason {
        Some("stop") => (FinishReason::Stop, None),
        Some("length") => (FinishReason::Length, None),
        Some("tool_calls") | Some("function_call") => (FinishReason::ToolCalls, None),
        Some("content_filter") => (FinishReason::ContentFilter, None),
        Some(other) => (FinishReason::Unknown, Some(other.to_owned())),
        None => (FinishReason::Unknown, None),
    }
}

#[derive(Default)]
pub struct Accum {
    pub text: String,
    pub tools: Vec<Option<(String, String, String)>>,
    pub usage: Usage,
    pub finish: FinishReason,
    pub raw_finish: Option<String>,
}

pub fn apply_chunk(
    state: &mut Accum,
    data: &str,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    let json: serde_json::Value = serde_json::from_str(data)
        .map_err(|error| Error::Provider(format!("invalid SSE payload: {error}")))?;
    if let Some(error) = json.get("error") {
        let message = error["message"]
            .as_str()
            .unwrap_or("openai-compatible stream error")
            .to_owned();
        return Err(Error::Provider(message));
    }
    for choice in json["choices"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let delta = &choice["delta"];
        if let Some(content) = delta["content"].as_str() {
            if !content.is_empty() {
                state.text.push_str(content);
                on_text(content);
            }
        }
        for tool_call in delta["tool_calls"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let index = tool_call["index"].as_u64().unwrap_or(0) as usize;
            if state.tools.len() <= index {
                state.tools.resize(index + 1, None);
            }
            let entry = state.tools[index]
                .get_or_insert_with(|| (String::new(), String::new(), String::new()));
            if let Some(id) = tool_call["id"].as_str() {
                if !id.is_empty() {
                    entry.0 = id.to_owned();
                }
            }
            if let Some(name) = tool_call["function"]["name"].as_str() {
                if !name.is_empty() {
                    entry.1 = name.to_owned();
                }
            }
            if let Some(arguments) = tool_call["function"]["arguments"].as_str() {
                entry.2.push_str(arguments);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            let (finish, raw) = finish_reason(Some(reason));
            state.finish = finish;
            state.raw_finish = raw;
        }
    }
    if let Some(usage) = json.get("usage").filter(|usage| !usage.is_null()) {
        state.usage.input = usage["prompt_tokens"].as_f64().unwrap_or(state.usage.input);
        state.usage.output = usage["completion_tokens"]
            .as_f64()
            .unwrap_or(state.usage.output);
        state.usage.cache_read = usage["prompt_tokens_details"]["cached_tokens"]
            .as_f64()
            .unwrap_or(state.usage.cache_read);
    }
    Ok(())
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
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = request_body(model, system, messages, tools, max_tokens);
    let response = ureq::post(&url)
        .set("authorization", &format!("Bearer {api_key}"))
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
    read_sse(reader, |_, data| {
        if data == "[DONE]" || state.finish == FinishReason::Error {
            return;
        }
        if let Err(error) = apply_chunk(&mut state, data, on_text) {
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
    Ok(summarize(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ModelRef {
        ModelRef {
            id: "gpt-5-mini".into(),
            provider_id: "openai".into(),
            variant: None,
        }
    }

    #[test]
    fn request_body_shapes_messages_and_tools() {
        let messages = vec![
            ChatMessage::User("hi".into()),
            ChatMessage::Assistant {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "shell".into(),
                    arguments: "{\"command\":\"ls\"}".into(),
                }],
            },
            ChatMessage::Tool {
                call_id: "call_1".into(),
                content: "out".into(),
            },
        ];
        let body = request_body(
            &model(),
            Some("sys"),
            &messages,
            &[ToolSpec {
                name: "shell".into(),
                description: "Run".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
            512,
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            "shell"
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
    }

    #[test]
    fn sse_fixture_assembles_tool_call_and_usage() {
        let fixture = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_7\",\"function\":{\"name\":\"glob\",\"arguments\":\"{\\\"pat\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"tern\\\":\\\"*.rs\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":31,\"completion_tokens\":9,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut state = Accum::default();
        let mut out = String::new();
        read_sse(fixture.as_bytes(), |_, data| {
            if data != "[DONE]" {
                apply_chunk(&mut state, data, &mut |t| out.push_str(t)).unwrap();
            }
        })
        .unwrap();
        let summary = summarize(state);
        assert_eq!(out, "Hello");
        assert_eq!(summary.finish, FinishReason::ToolCalls);
        assert_eq!(summary.tool_calls[0].arguments, "{\"pattern\":\"*.rs\"}");
        assert_eq!(summary.usage.input, 31.0);
        assert_eq!(summary.usage.cache_read, 4.0);
        assert_eq!(summary.usage.output, 9.0);
    }

    #[test]
    fn error_chunk_surfaces_as_provider_error() {
        let mut state = Accum::default();
        let mut nul = |_: &str| {};
        let error = apply_chunk(
            &mut state,
            "{\"error\":{\"message\":\"bad key\"}}",
            &mut nul,
        )
        .unwrap_err();
        assert!(matches!(error, Error::Provider(message) if message == "bad key"));
    }
}
