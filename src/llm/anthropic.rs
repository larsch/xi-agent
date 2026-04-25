use serde::Deserialize;
use std::collections::HashMap;

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    Role, ToolDefinition, UsageStats,
    common::{
        StreamControl, build_http_client, infer_initiator, send_streaming_request, stream_sse_lines,
    },
    provider_format::to_anthropic_wire,
};
use crate::provider::{context_window_for_model, scaled_token_budget};

// ── Typed SSE event structs ───────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: MessageStartPayload,
    },
    ContentBlockStart {
        index: u64,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u64,
        delta: ContentDelta,
    },
    ContentBlockStop {
        index: u64,
    },
    MessageDelta {
        delta: MessageDeltaPayload,
        usage: Option<MessageDeltaUsage>,
    },
    MessageStop,
    Error {
        error: AnthropicApiError,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MessageStartPayload {
    usage: Option<MessageUsage>,
}

#[derive(Deserialize)]
struct MessageUsage {
    input_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    ToolUse {
        id: String,
        name: String,
    },
    Text,
    Thinking,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MessageDeltaPayload {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct AnthropicApiError {
    message: String,
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct AnthropicProvider {
    base_url: String,
    model: String,
    api_key: String,
    extra_headers: Vec<(String, String)>,
    /// When true, authenticate with `Authorization: Bearer …` (GitHub Copilot
    /// proxy).  When false, use `x-api-key` (direct Anthropic API).
    bearer_auth: bool,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new_with_headers(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        bearer_auth: bool,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            bearer_auth,
            extra_headers,
            client: build_http_client(),
        }
    }

    fn stream_inner(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let bearer_auth = self.bearer_auth;
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            let system: Option<String> = messages
                .iter()
                .find(|m| m.role == Role::System)
                .map(|m| m.content.clone());

            let anthropic_messages = to_anthropic_wire(&messages);

            let context_window = context_window_for_model(&model).unwrap_or(200_000);
            let max_tokens = scaled_token_budget(context_window, 8_000, 8_000);
            let mut body = serde_json::json!({
                "model": model,
                "messages": anthropic_messages,
                "max_tokens": max_tokens,
                "stream": true,
            });

            if let Some(sys) = system {
                body["system"] = serde_json::Value::String(sys);
            }

            if !tools.is_empty() {
                body["tools"] = serde_json::json!(convert_tools(&tools));
            }

            log::debug!(
                "[TAU_DEBUG] → anthropic request:\n{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );

            let mut req = client.post(&url).json(&body);
            if bearer_auth {
                req = req.bearer_auth(&api_key);
            } else {
                req = req.header("x-api-key", &api_key);
            }
            req = req.header("anthropic-version", "2023-06-01");

            let use_dynamic_initiator = extra_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("X-Initiator"));
            for (k, v) in &extra_headers {
                if !k.eq_ignore_ascii_case("X-Initiator") {
                    req = req.header(k.as_str(), v.as_str());
                }
            }
            if use_dynamic_initiator {
                req = req.header("X-Initiator", infer_initiator(&messages));
            }

            let response = match send_streaming_request(req, "Anthropic").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            // Track streaming tool_use blocks: content index → accumulated state.
            let mut tool_blocks: HashMap<u64, ToolBlock> = HashMap::new();
            let mut emitted_tool_intent = false;
            // Input token count from the message_start event; combined with
            // output tokens from message_delta/message_stop before emitting Usage.
            let mut input_tokens_from_start: Option<usize> = None;

            let mut stream = stream_sse_lines("Anthropic", response, move |data, events| {
                if data == "[DONE]" {
                    events.push(LlmEvent::Done);
                    return StreamControl::Done;
                }

                let ev: AnthropicEvent = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        events.push(LlmEvent::Error(ProviderError::other("Anthropic", format!("Parse error: {e}"))));
                        return StreamControl::Done;
                    }
                };

                match ev {
                    AnthropicEvent::MessageStart { message } => {
                        if let Some(usage) = message.usage {
                            input_tokens_from_start = usage.input_tokens;
                        }
                    }

                    AnthropicEvent::ContentBlockStart { index, content_block } => {
                        if let ContentBlock::ToolUse { id, name } = content_block {
                            if !emitted_tool_intent {
                                emitted_tool_intent = true;
                                events.push(LlmEvent::ToolIntentStart);
                            }
                            tool_blocks.insert(
                                index,
                                ToolBlock { id, name, partial_json: String::new() },
                            );
                        }
                    }

                    AnthropicEvent::ContentBlockDelta { index, delta } => {
                        match delta {
                            ContentDelta::TextDelta { text } if !text.is_empty() => {
                                events.push(LlmEvent::Token {
                                    text,
                                    phase: if emitted_tool_intent {
                                        AssistantPhase::Provisional
                                    } else {
                                        AssistantPhase::Unknown
                                    },
                                });
                            }
                            ContentDelta::ThinkingDelta { thinking } if !thinking.is_empty() => {
                                events.push(LlmEvent::ThinkingToken(thinking));
                            }
                            ContentDelta::InputJsonDelta { partial_json } => {
                                if let Some(block) = tool_blocks.get_mut(&index) {
                                    block.partial_json.push_str(&partial_json);
                                }
                            }
                            _ => {}
                        }
                    }

                    AnthropicEvent::ContentBlockStop { index } => {
                        if let Some(block) = tool_blocks.remove(&index) {
                            let args: serde_json::Value =
                                serde_json::from_str(&block.partial_json)
                                    .unwrap_or(serde_json::Value::Object(Default::default()));
                            events.push(LlmEvent::ToolCall {
                                id: block.id,
                                name: block.name,
                                args,
                            });
                        }
                    }

                    AnthropicEvent::MessageDelta { delta, usage } => {
                        if let Some(usage_stats) = build_usage_from_delta(usage, &mut input_tokens_from_start) {
                            events.push(LlmEvent::Usage(usage_stats));
                        }
                        if delta.stop_reason.as_deref() == Some("max_tokens")
                            && !tool_blocks.is_empty()
                        {
                            events.push(LlmEvent::Error(ProviderError::other(
                                "Anthropic",
                                "Response truncated by token limit mid-tool-call; \
                                 tool arguments incomplete.",
                            )));
                            return StreamControl::Done;
                        }
                    }

                    AnthropicEvent::MessageStop => {
                        events.push(LlmEvent::Done);
                        return StreamControl::Done;
                    }

                    AnthropicEvent::Error { error } => {
                        events.push(LlmEvent::Error(ProviderError::other("Anthropic", error.message)));
                        return StreamControl::Done;
                    }

                    AnthropicEvent::Unknown => {}
                }

                StreamControl::Continue
            });

            use futures_util::StreamExt as _;
            while let Some(ev) = stream.next().await {
                yield ev;
            }

            yield LlmEvent::Done;
        })
    }
}

/// Build a `UsageStats` from a `MessageDelta` usage payload, merging in the
/// input tokens captured from the earlier `MessageStart` event.
fn build_usage_from_delta(
    usage: Option<MessageDeltaUsage>,
    input_tokens_from_start: &mut Option<usize>,
) -> Option<UsageStats> {
    let output = usage.as_ref().and_then(|u| u.output_tokens);
    let input = input_tokens_from_start.take();
    let total = match (input, output) {
        (Some(i), Some(o)) => Some(i.saturating_add(o)),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    if input.is_none() && output.is_none() {
        None
    } else {
        Some(UsageStats {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        })
    }
}

// ── LlmProvider impl ──────────────────────────────────────────────────────────

impl LlmProvider for AnthropicProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        self.stream_inner(messages, vec![])
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        self.stream_inner(messages, tools)
    }

    fn list_models(&self) -> ModelListFuture {
        let models = known_models();
        Box::pin(async move { Ok(models) })
    }
}

// ── Known Claude models available via GitHub Copilot ─────────────────────────

fn known_models() -> Vec<String> {
    vec![
        "claude-sonnet-4.6".to_string(),
        "claude-opus-4.6".to_string(),
        "claude-sonnet-4.5".to_string(),
        "claude-opus-4.5".to_string(),
        "claude-sonnet-4".to_string(),
        "claude-haiku-4.5".to_string(),
    ]
}

// ── Message conversion ────────────────────────────────────────────────────────

fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

// ── Pending tool-use block accumulator ────────────────────────────────────────

struct ToolBlock {
    id: String,
    name: String,
    partial_json: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> AnthropicEvent {
        serde_json::from_str(json).expect("parse failed")
    }

    #[test]
    fn message_start_captures_input_tokens() {
        let ev = parse(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":42,"output_tokens":0}}}"#,
        );
        let AnthropicEvent::MessageStart { message } = ev else {
            panic!("wrong variant")
        };
        assert_eq!(message.usage.unwrap().input_tokens, Some(42));
    }

    #[test]
    fn content_block_start_text_variant() {
        let ev =
            parse(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#);
        let AnthropicEvent::ContentBlockStart {
            index,
            content_block,
        } = ev
        else {
            panic!()
        };
        assert_eq!(index, 0);
        assert!(matches!(content_block, ContentBlock::Text));
    }

    #[test]
    fn content_block_start_tool_use_variant() {
        let ev = parse(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_123","name":"read_file"}}"#,
        );
        let AnthropicEvent::ContentBlockStart {
            index,
            content_block,
        } = ev
        else {
            panic!()
        };
        assert_eq!(index, 1);
        let ContentBlock::ToolUse { id, name } = content_block else {
            panic!()
        };
        assert_eq!(id, "toolu_123");
        assert_eq!(name, "read_file");
    }

    #[test]
    fn content_block_delta_text_delta() {
        let ev = parse(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
        );
        let AnthropicEvent::ContentBlockDelta { index, delta } = ev else {
            panic!()
        };
        assert_eq!(index, 0);
        let ContentDelta::TextDelta { text } = delta else {
            panic!()
        };
        assert_eq!(text, "hello");
    }

    #[test]
    fn content_block_delta_input_json() {
        let ev = parse(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"k\":"}}"#,
        );
        let AnthropicEvent::ContentBlockDelta { index, delta } = ev else {
            panic!()
        };
        assert_eq!(index, 1);
        let ContentDelta::InputJsonDelta { partial_json } = delta else {
            panic!()
        };
        assert_eq!(partial_json, "{\"k\":");
    }

    #[test]
    fn content_block_stop() {
        let ev = parse(r#"{"type":"content_block_stop","index":2}"#);
        let AnthropicEvent::ContentBlockStop { index } = ev else {
            panic!()
        };
        assert_eq!(index, 2);
    }

    #[test]
    fn message_delta_with_usage() {
        let ev = parse(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#,
        );
        let AnthropicEvent::MessageDelta { delta, usage } = ev else {
            panic!()
        };
        assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(usage.unwrap().output_tokens, Some(15));
    }

    #[test]
    fn message_stop_variant() {
        let ev = parse(r#"{"type":"message_stop"}"#);
        assert!(matches!(ev, AnthropicEvent::MessageStop));
    }

    #[test]
    fn error_event_parses_message() {
        let ev =
            parse(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#);
        let AnthropicEvent::Error { error } = ev else {
            panic!()
        };
        assert_eq!(error.message, "Overloaded");
    }

    #[test]
    fn unknown_event_type_ignored() {
        let ev = parse(r#"{"type":"ping"}"#);
        assert!(matches!(ev, AnthropicEvent::Unknown));
    }

    #[test]
    fn build_usage_merges_input_and_output() {
        let mut input = Some(100usize);
        let usage = build_usage_from_delta(
            Some(MessageDeltaUsage {
                output_tokens: Some(20),
            }),
            &mut input,
        );
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.total_tokens, Some(120));
        // input_tokens_from_start should be consumed
        assert!(input.is_none());
    }

    #[test]
    fn build_usage_returns_none_when_no_data() {
        let mut input = None;
        let usage = build_usage_from_delta(None, &mut input);
        assert!(usage.is_none());
    }
}
