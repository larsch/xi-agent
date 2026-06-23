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
use crate::context_window::{context_window_for_model, scaled_token_budget};

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
    /// Tokens read from the prompt cache (cache hit).  Present when a cache
    /// entry was found for part of the prompt prefix.
    #[serde(default)]
    cache_read_input_tokens: Option<usize>,
    /// Tokens written into the prompt cache (cache creation).  Present when
    /// uncached content was stored for future requests.
    #[serde(default)]
    cache_creation_input_tokens: Option<usize>,
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
    /// The message_delta event may include updated cache-read and cache-creation
    /// counts that override the values from message_start.
    #[serde(default)]
    cache_read_input_tokens: Option<usize>,
    #[serde(default)]
    cache_creation_input_tokens: Option<usize>,
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
                // System prompt as an array of text blocks with a cache
                // breakpoint so the instruction prefix is cached across turns.
                body["system"] = serde_json::json!([
                    {
                        "type": "text",
                        "text": sys,
                        "cache_control": { "type": "ephemeral" }
                    }
                ]);
            }

            // Add a cache breakpoint on the last user content block so the
            // conversation history (system prompt, tool definitions, previous
            // turns) is cached across requests.
            if let Some(messages_array) = body["messages"].as_array_mut() {
                apply_cache_breakpoint(messages_array);
            }

            if !tools.is_empty() {
                body["tools"] = serde_json::json!(convert_tools(&tools));
            }

            crate::debug_log::log_structured(
                log::Level::Debug,
                "xi::llm::anthropic",
                serde_json::json!({
                    "event": "llm_request",
                    "provider": "anthropic",
                    "payload": &body,
                }),
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
            // Token-usage state accumulated from message_start and message_delta
            // events.  message_start provides initial counts; message_delta may
            // override them and adds output_tokens.
            let mut input_tokens: Option<usize> = None;
            let mut cache_read_tokens: Option<usize> = None;
            // cache_creation_tokens is captured for future cost-reporting use.
            let mut _cache_creation_tokens: Option<usize> = None;

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
                            input_tokens = usage.input_tokens;
                            cache_read_tokens = usage.cache_read_input_tokens;
                            _cache_creation_tokens = usage.cache_creation_input_tokens;
                        }
                    }

                    AnthropicEvent::ContentBlockStart { index, content_block } => {
                        if let ContentBlock::ToolUse { id, name } = content_block {
                            events.push(LlmEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            });
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
                                    phase: if !tool_blocks.is_empty() {
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
                                    events.push(LlmEvent::ToolCallArgsDelta {
                                        id: block.id.clone(),
                                        partial_json: partial_json.clone(),
                                    });
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
                        // Merge any cache-token updates from the delta event
                        // into our running values.  The API may update these at
                        // this point in the stream.
                        if let Some(ref u) = usage {
                            if let Some(v) = u.cache_read_input_tokens {
                                cache_read_tokens = Some(v);
                            }
                            if let Some(v) = u.cache_creation_input_tokens {
                                _cache_creation_tokens = Some(v);
                            }
                        }
                        if let Some(usage_stats) = build_usage_from_delta(
                            usage,
                            &mut input_tokens,
                            &mut cache_read_tokens,
                        ) {
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

/// Add a `cache_control` breakpoint to the last user message in an Anthropic
/// messages array so that the conversation prefix (system prompt, tool
/// definitions, previous turns) is cached across requests.
///
/// For a string-typed `content` the message is wrapped into an array of text
/// blocks with `cache_control` on the single element.  For an already-array
/// `content` the annotation is added to the last cacheable block (text, image,
/// or tool_result).
fn apply_cache_breakpoint(messages: &mut [serde_json::Value]) {
    let last_user = match messages
        .iter_mut()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
    {
        Some(m) => m,
        None => return,
    };

    let content = match last_user.get_mut("content") {
        Some(c) => c,
        None => return,
    };

    if content.is_string() {
        let text = std::mem::take(content);
        let text = text.as_str().unwrap_or_default().to_string();
        *content = serde_json::json!([
            {
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral" }
            }
        ]);
    } else if let Some(blocks) = content.as_array_mut()
        && let Some(last_block) = blocks.last_mut()
        && last_block
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| matches!(t, "text" | "image" | "tool_result"))
        && let Some(obj) = last_block.as_object_mut()
    {
        obj.insert(
            "cache_control".to_string(),
            serde_json::json!({ "type": "ephemeral" }),
        );
    }
}

/// Build a `UsageStats` from a `MessageDelta` usage payload, merging in the
/// input and cache counts captured from the earlier `MessageStart` event.
///
/// The `cache_read` parameter is consumed (taken) here to match the
/// once-per-stream semantics: the function is called once from
/// `message_delta`.
fn build_usage_from_delta(
    usage: Option<MessageDeltaUsage>,
    input_tokens: &mut Option<usize>,
    cache_read_tokens: &mut Option<usize>,
) -> Option<UsageStats> {
    let output = usage.as_ref().and_then(|u| u.output_tokens);
    let input = input_tokens.take();
    let cached = cache_read_tokens.take();
    let total = match (input, output) {
        (Some(i), Some(o)) => Some(i.saturating_add(o)),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    if input.is_none() && output.is_none() && cached.is_none() {
        None
    } else {
        Some(UsageStats {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
            cached_tokens: cached,
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
    fn message_start_captures_cache_fields() {
        let ev = parse(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":30}}}"#,
        );
        let AnthropicEvent::MessageStart { message } = ev else {
            panic!("wrong variant")
        };
        let usage = message.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.cache_read_input_tokens, Some(50));
        assert_eq!(usage.cache_creation_input_tokens, Some(30));
    }

    #[test]
    fn message_start_missing_cache_fields_default_to_none() {
        // Cache fields are optional; they default to None when absent.
        let ev = parse(r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#);
        let AnthropicEvent::MessageStart { message } = ev else {
            panic!("wrong variant")
        };
        let usage = message.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(10));
        assert!(usage.cache_read_input_tokens.is_none());
        assert!(usage.cache_creation_input_tokens.is_none());
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
    fn message_delta_with_cache_fields() {
        let ev = parse(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20,"cache_read_input_tokens":200,"cache_creation_input_tokens":100}}"#,
        );
        let AnthropicEvent::MessageDelta { delta: _, usage } = ev else {
            panic!("wrong variant")
        };
        let u = usage.unwrap();
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.cache_read_input_tokens, Some(200));
        assert_eq!(u.cache_creation_input_tokens, Some(100));
    }

    #[test]
    fn message_delta_missing_cache_fields_default_to_none() {
        let ev = parse(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
        );
        let AnthropicEvent::MessageDelta { delta: _, usage } = ev else {
            panic!("wrong variant")
        };
        let u = usage.unwrap();
        assert_eq!(u.output_tokens, Some(5));
        assert!(u.cache_read_input_tokens.is_none());
        assert!(u.cache_creation_input_tokens.is_none());
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
        let mut cached = Some(30usize);
        let usage = build_usage_from_delta(
            Some(MessageDeltaUsage {
                output_tokens: Some(20),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            &mut input,
            &mut cached,
        );
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.total_tokens, Some(120));
        assert_eq!(u.cached_tokens, Some(30));
        // Both sources should be consumed.
        assert!(input.is_none());
        assert!(cached.is_none());
    }

    #[test]
    fn build_usage_returns_none_when_no_data() {
        let mut input = None;
        let mut cached = None;
        let usage = build_usage_from_delta(None, &mut input, &mut cached);
        assert!(usage.is_none());
    }

    #[test]
    fn build_usage_includes_cache_hits() {
        let mut input = Some(200usize);
        let mut cached = Some(50usize);
        let usage = build_usage_from_delta(
            Some(MessageDeltaUsage {
                output_tokens: Some(30),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            &mut input,
            &mut cached,
        );
        let u = usage.unwrap();
        assert_eq!(u.cached_tokens, Some(50));
        assert_eq!(u.total_tokens, Some(230)); // 200 + 30 (cached not included)
    }

    #[test]
    fn build_usage_cache_only_no_input_or_output() {
        let mut input = None;
        let mut cached = Some(42usize);
        let usage = build_usage_from_delta(None, &mut input, &mut cached);
        let u = usage.unwrap();
        assert_eq!(u.cached_tokens, Some(42));
        assert!(u.input_tokens.is_none());
        assert!(u.output_tokens.is_none());
        assert!(u.total_tokens.is_none());
    }
}
