//! Provider layer for the Rust build. Ports the Anthropic Messages API and
//! OpenAI-compatible chat completions used by the Bun build's provider layer,
//! with the same streaming semantics. Sync, no async runtime.
//!
//! Not ported yet: model catalog/metadata (models.dev), pricing tables,
//! OAuth credentials (GitHub Copilot, Anthropic), retry policy, rate limiting.

pub mod anthropic;
pub mod openai;

use std::io::{BufRead, BufReader, Read};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Unknown,
}

impl Default for FinishReason {
    fn default() -> Self {
        FinishReason::Unknown
    }
}

impl FinishReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool-calls",
            FinishReason::ContentFilter => "content-filter",
            FinishReason::Error => "error",
            FinishReason::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRef {
    pub id: String,
    pub provider_id: String,
    pub variant: Option<String>,
}

/// Parse `providerID/model` (optionally `#variant`), like schema/src/model.ts.
pub fn parse_model(input: &str) -> Result<ModelRef, String> {
    let (provider, rest) = input
        .split_once('/')
        .ok_or_else(|| format!("Invalid model reference: {input}"))?;
    if provider.is_empty() {
        return Err(format!("Invalid model reference: {input}"));
    }
    let (id, variant) = match rest.split_once('#') {
        Some((id, variant)) => (id.to_owned(), Some(variant.to_owned())),
        None => (rest.to_owned(), None),
    };
    if id.is_empty() {
        return Err(format!("Invalid model reference: {input}"));
    }
    Ok(ModelRef {
        id,
        provider_id: provider.to_owned(),
        variant,
    })
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider_id, self.id)?;
        if let Some(variant) = &self.variant {
            write!(f, "#{variant}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub input: f64,
    pub output: f64,
    pub reasoning: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Usage {
    pub fn zero() -> Self {
        Usage::default()
    }

    /// USD cost from per-million-token prices.
    pub fn cost(&self, input_per_mtok: f64, output_per_mtok: f64) -> f64 {
        self.input / 1_000_000.0 * input_per_mtok + self.output / 1_000_000.0 * output_per_mtok
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Done {
        finish: FinishReason,
        raw_finish: Option<String>,
        usage: Usage,
    },
}

#[derive(Debug)]
pub struct StreamSummary {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    pub raw_finish: Option<String>,
    pub usage: Usage,
}

#[derive(Debug)]
pub enum Error {
    Http(String),
    Io(std::io::Error),
    /// Non-2xx response; body included (provider error details).
    Status(u16, String),
    Provider(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(message) => write!(f, "{message}"),
            Error::Io(error) => write!(f, "{error}"),
            Error::Status(status, body) => write!(f, "HTTP {status}: {body}"),
            Error::Provider(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

/// Parse an SSE byte stream, invoking `on_event(name, data)` for each event.
/// `name` defaults to "message" when the stream carries no `event:` line.
pub fn read_sse(reader: impl Read, mut on_event: impl FnMut(&str, &str)) -> std::io::Result<()> {
    let mut name = String::from("message");
    let mut data = String::new();
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if line.is_empty() {
            if !data.is_empty() {
                on_event(&name, data.trim_end());
                data.clear();
                name = String::from("message");
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            name = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
        // Unknown fields (id:, retry:, comments) are ignored.
    }
    if !data.is_empty() {
        on_event(&name, data.trim_end());
    }
    Ok(())
}

/// Dispatch a streaming chat completion to the provider for `model.provider_id`.
/// Credentials come from the environment: ANTHROPIC_API_KEY, OPENAI_API_KEY
/// (or OPENAI_BASE_URL for compatible endpoints).
pub fn stream_chat(
    model: &ModelRef,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u32,
    on_text: &mut dyn FnMut(&str),
) -> Result<StreamSummary, Error> {
    match model.provider_id.as_str() {
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| Error::Provider("ANTHROPIC_API_KEY is not set".into()))?;
            let base_url = std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into());
            anthropic::stream(
                &base_url, &api_key, model, system, messages, tools, max_tokens, on_text,
            )
        }
        // Unknown providers use the OpenAI-compatible endpoint, which is the common denominator.
        "openai" | _ => {
            let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
                Error::Provider(
                    "OPENAI_API_KEY is not set (or set it for the OPENAI_BASE_URL endpoint)".into(),
                )
            })?;
            let base_url = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            openai::stream(
                &base_url, &api_key, model, system, messages, tools, max_tokens, on_text,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_model_and_variant() {
        assert_eq!(
            parse_model("anthropic/claude-sonnet-4-5").unwrap(),
            ModelRef {
                id: "claude-sonnet-4-5".into(),
                provider_id: "anthropic".into(),
                variant: None
            }
        );
        assert_eq!(
            parse_model("openai/gpt-5-mini#high").unwrap(),
            ModelRef {
                id: "gpt-5-mini".into(),
                provider_id: "openai".into(),
                variant: Some("high".into())
            }
        );
        assert!(parse_model("noslash").is_err());
        assert!(parse_model("/model").is_err());
    }

    #[test]
    fn read_sse_handles_multiline_and_no_event_field() {
        let fixture = "data: first\ndata: second\n\ndata: third\n\n";
        let mut events = Vec::new();
        read_sse(fixture.as_bytes(), |name, data| {
            events.push((name.to_owned(), data.to_owned()))
        })
        .unwrap();
        assert_eq!(
            events,
            vec![
                ("message".to_owned(), "first\nsecond".to_owned()),
                ("message".to_owned(), "third".to_owned())
            ]
        );
    }
}
