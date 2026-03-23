#![allow(dead_code)]

use std::collections::HashMap;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, Role,
    ToolDefinition, UsageStats,
    common::{SseLineDecoder, infer_initiator, normalize_tool_name, send_streaming_request},
};

pub struct OpenAiProvider {
    base_url: String,
    model: String,
    api_key: String,
    extra_headers: Vec<(String, String)>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::new_with_headers(base_url, model, api_key, vec![])
    }

    pub fn new_with_headers(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            extra_headers,
            client: reqwest::Client::new(),
        }
    }

    /// Clone this provider, replacing only the model name.
    pub fn with_model(&self, model: impl Into<String>) -> Self {
        Self {
            base_url: self.base_url.clone(),
            model: model.into(),
            api_key: self.api_key.clone(),
            extra_headers: self.extra_headers.clone(),
            client: self.client.clone(),
        }
    }

    fn stream_inner(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream {
        let url = format!("{}/chat/completions", self.base_url);
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            let oai_messages: Vec<OaiMessage> = to_oai_messages(&messages);

            let body = ChatRequest {
                model,
                messages: oai_messages,
                stream: true,
                stream_options: Some(StreamOptions { include_usage: true }),
                tools: if tools.is_empty() {
                    None
                } else {
                    Some(tools.iter().map(to_oai_tool).collect())
                },
            };

            if let Ok(json) = serde_json::to_string_pretty(&body) {
                log::debug!("[TAU_DEBUG] → request:\n{json}");
            }

            let mut req = client
                .post(&url)
                .bearer_auth(&api_key)
                .json(&body);
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

            let response = match send_streaming_request(req, "OpenAI").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut sse = SseLineDecoder::new();
            // Accumulate partial tool-call deltas keyed by index.
            let mut tool_calls: HashMap<u32, PartialToolCall> = HashMap::new();
            let mut emitted_tool_intent = false;
            let mut line_num = 0usize;

            'outer: while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        yield LlmEvent::Error(e.to_string());
                        return;
                    }
                };
                sse.push_bytes(&bytes);

                while let Some(line) = sse.next_data_line() {
                    if line == "[DONE]" {
                        // Flush any accumulated tool calls.
                        let mut calls: Vec<PartialToolCall> = {
                            let mut v: Vec<(u32, PartialToolCall)> = tool_calls.drain().collect();
                            v.sort_by_key(|(i, _)| *i);
                            v.into_iter().map(|(_, tc)| tc).collect()
                        };
                        for (i, tc) in calls.iter_mut().enumerate() {
                            let args: serde_json::Value =
                                serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                            yield LlmEvent::ToolCall {
                                id: tc.id.clone().unwrap_or_else(|| format!("call_{i}")),
                                name: tc.name.clone(),
                                args,
                            };
                        }
                        yield LlmEvent::Done;
                        break 'outer;
                    }

                    log::debug!("[TAU_DEBUG] ← chunk {line_num}: {line}");
                    line_num += 1;

                    let chunk: ChatChunk = match serde_json::from_str(&line) {
                        Ok(c) => c,
                        Err(e) => {
                            yield LlmEvent::Error(format!("Parse error: {e}"));
                            return;
                        }
                    };

                    if let Some(usage) = chunk.usage {
                        yield LlmEvent::Usage(UsageStats {
                            input_tokens: usage.prompt_tokens,
                            output_tokens: usage.completion_tokens,
                            total_tokens: usage.total_tokens,
                        });
                    }

                    for choice in chunk.choices {
                        let delta = choice.delta;

                        // Text tokens.
                        if let Some(content) = delta.content
                            && !content.is_empty() {
                                yield LlmEvent::Token {
                                    text: content,
                                    phase: if emitted_tool_intent {
                                        AssistantPhase::Provisional
                                    } else {
                                        AssistantPhase::Unknown
                                    },
                                };
                            }

                        // Tool-call delta fragments — merge into accumulator.
                        if !delta.tool_calls.is_empty() && !emitted_tool_intent {
                            emitted_tool_intent = true;
                            yield LlmEvent::ToolIntentStart;
                        }
                        for tc_delta in delta.tool_calls {
                            let entry = tool_calls
                                .entry(tc_delta.index)
                                .or_default();
                            if let Some(id) = tc_delta.id {
                                entry.id = Some(id);
                            }
                            if let Some(name) = tc_delta.function.name {
                                entry.name.push_str(&name);
                            }
                            if let Some(args) = tc_delta.function.arguments {
                                entry.arguments.push_str(&args);
                            }
                        }

                        // When finish_reason == "tool_calls" the arguments are
                        // complete.  We flush at [DONE] above, but also handle
                        // it here for providers that set finish_reason before
                        // [DONE] on the same or next chunk.
                        if choice.finish_reason.as_deref() == Some("stop") && tool_calls.is_empty() {
                            // Normal text finish — Done will be emitted at [DONE].
                        }
                    }
                }
            }
        })
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<OaiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OaiToolDef>>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct OaiMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct OaiToolCallItem {
    id: String,
    r#type: &'static str,
    function: OaiToolCallFunction,
}

#[derive(Serialize)]
struct OaiToolCallFunction {
    name: String,
    arguments: String, // JSON-encoded string
}

#[derive(Serialize)]
struct OaiToolDef {
    r#type: &'static str,
    function: OaiFunctionDef,
}

#[derive(Serialize)]
struct OaiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ── SSE response types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    total_tokens: Option<usize>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: u32,
    id: Option<String>,
    #[serde(default)]
    function: ToolCallFunctionDelta,
}

#[derive(Deserialize, Default)]
struct ToolCallFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct PartialToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

// ── Model list response ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

// ── Serialisation helpers ─────────────────────────────────────────────────────

/// Convert a tau `Message` history to OpenAI Chat Completions messages.
///
/// The OpenAI API requires that tool calls and their accompanying text live in
/// *one* assistant message, followed by one `"role":"tool"` message per result.
/// Tau stores them as separate `Role::Assistant` + `Role::ToolCall` +
/// `Role::ToolResult` messages, interleaved when there are multiple calls in a
/// single turn.  This function:
///
/// 1. Merges a `Role::Assistant` message with any immediately following
///    `Role::ToolCall` messages into a single assistant message that carries
///    both `content` and `tool_calls`.
/// 2. Collects the corresponding `Role::ToolResult` messages and emits them
///    after the merged assistant message, preserving order.
/// 3. Skips empty assistant messages that have no content and no tool calls
///    (e.g. an aborted turn with no output).
fn to_oai_messages(messages: &[Message]) -> Vec<OaiMessage> {
    let mut result: Vec<OaiMessage> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        match msg.role {
            Role::Assistant => {
                // Look ahead and collect alternating ToolCall / ToolResult pairs
                // that belong to this turn.
                let mut j = i + 1;
                let mut tool_calls: Vec<OaiToolCallItem> = Vec::new();
                let mut tool_results: Vec<OaiMessage> = Vec::new();

                while j < messages.len() && messages[j].role == Role::ToolCall {
                    let tc = &messages[j];
                    let call_idx = tool_calls.len();
                    tool_calls.push(OaiToolCallItem {
                        id: tc
                            .tool_call_id
                            .clone()
                            .unwrap_or_else(|| format!("call_{call_idx}")),
                        r#type: "function",
                        function: OaiToolCallFunction {
                            name: normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default())
                                .to_string(),
                            arguments: tc
                                .tool_args
                                .as_ref()
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "{}".to_string()),
                        },
                    });
                    j += 1;

                    // Each ToolCall is immediately followed by its ToolResult.
                    if j < messages.len() && messages[j].role == Role::ToolResult {
                        tool_results.push(to_oai_message(&messages[j]));
                        j += 1;
                    }
                }

                let content = if msg.content.is_empty() {
                    None
                } else {
                    Some(msg.content.clone())
                };
                let tool_calls_opt = if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                };

                // Skip completely empty assistant messages (no content, no tool calls).
                if content.is_some() || tool_calls_opt.is_some() {
                    result.push(OaiMessage {
                        role: "assistant",
                        content,
                        tool_calls: tool_calls_opt,
                        tool_call_id: None,
                    });
                    result.extend(tool_results);
                }

                i = j;
            }

            // Standalone ToolCall without a preceding Assistant — shouldn't happen
            // in normal agent flow but handled gracefully.
            Role::ToolCall => {
                result.push(to_oai_message(msg));
                i += 1;
            }

            _ => {
                result.push(to_oai_message(msg));
                i += 1;
            }
        }
    }

    result
}

fn to_oai_message(msg: &Message) -> OaiMessage {
    match msg.role {
        Role::ToolCall => OaiMessage {
            role: "assistant",
            content: None,
            tool_calls: Some(vec![OaiToolCallItem {
                id: msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_0".to_string()),
                r#type: "function",
                function: OaiToolCallFunction {
                    name: msg.tool_name.clone().unwrap_or_default(),
                    arguments: msg
                        .tool_args
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string()),
                },
            }]),
            tool_call_id: None,
        },
        Role::ToolResult => OaiMessage {
            role: "tool",
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: msg.tool_call_id.clone(),
        },
        Role::System => OaiMessage {
            role: "system",
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        },
        Role::User => OaiMessage {
            role: "user",
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        },
        Role::Assistant => OaiMessage {
            role: "assistant",
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        },
    }
}

fn to_oai_tool(tool: &ToolDefinition) -> OaiToolDef {
    OaiToolDef {
        r#type: "function",
        function: OaiFunctionDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        },
    }
}

// ── LlmProvider impl ──────────────────────────────────────────────────────────

impl LlmProvider for OpenAiProvider {
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
        let url = format!("{}/models", self.base_url);
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let client = self.client.clone();
        Box::pin(async move {
            let mut req = client.get(&url).bearer_auth(&api_key);
            for (k, v) in &extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            log::debug!("→ GET {url}");
            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("openai list_models error: {e}");
                    return Err(format!("request failed: {e}"));
                }
            };
            let status = response.status();
            log::debug!("← HTTP {status} from openai list_models");
            if !status.is_success() {
                let msg = if status.as_u16() == 401 {
                    "401 Unauthorized — run /login to authenticate".to_string()
                } else {
                    format!("HTTP {status}")
                };
                log::warn!("openai list_models failed: {msg}");
                return Err(msg);
            }
            let models: ModelsResponse = match response.json().await {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("openai list_models parse error: {e}");
                    return Err(format!("failed to parse response: {e}"));
                }
            };
            let mut ids: Vec<String> = models.data.into_iter().map(|m| m.id).collect();
            ids.sort();
            Ok(ids)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{infer_initiator, normalize_tool_name, to_oai_messages};
    use crate::llm::{Message, Role};

    #[test]
    fn normalize_tool_name_maps_emoji_aliases_and_passthrough() {
        assert_eq!(normalize_tool_name("👀"), "read_file");
        assert_eq!(normalize_tool_name("✍️"), "write_file");
        assert_eq!(normalize_tool_name("📝"), "edit_file");
        assert_eq!(normalize_tool_name("💻"), "bash");
        assert_eq!(normalize_tool_name("🔍"), "find_files");
        assert_eq!(normalize_tool_name("custom_tool"), "custom_tool");
    }

    #[test]
    fn infer_initiator_depends_on_last_message_role() {
        assert_eq!(infer_initiator(&[]), "user");
        assert_eq!(infer_initiator(&[Message::user("hi")]), "user");
        assert_eq!(infer_initiator(&[Message::assistant("ok")]), "agent");
    }

    #[test]
    fn to_oai_messages_merges_assistant_with_tool_calls_and_results() {
        let messages = vec![
            Message::assistant("I will call tools"),
            Message::tool_call("call_1", "👀", serde_json::json!({"path": "a.txt"})),
            Message::tool_result("call_1", "contents", false),
            Message::tool_call("call_2", "bash", serde_json::json!({"command": "echo hi"})),
            Message::tool_result("call_2", "hi", false),
            Message::user("thanks"),
        ];

        let out = to_oai_messages(&messages);
        assert_eq!(out.len(), 4);

        let assistant = &out[0];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.content.as_deref(), Some("I will call tools"));
        let calls = assistant.tool_calls.as_ref().expect("tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].function.name, "bash");

        assert_eq!(out[1].role, "tool");
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(out[2].role, "tool");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(out[3].role, "user");
        assert_eq!(out[3].content.as_deref(), Some("thanks"));
    }

    #[test]
    fn to_oai_messages_skips_empty_assistant_without_tool_calls() {
        let out = to_oai_messages(&[Message::assistant("")]);
        assert!(out.is_empty());
    }

    #[test]
    fn to_oai_messages_handles_standalone_tool_call_with_fallback_id() {
        let mut tc = Message::tool_call("provided", "custom", serde_json::json!({"x": 1}));
        tc.tool_call_id = None;

        let out = to_oai_messages(&[tc]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "assistant");
        let calls = out[0].tool_calls.as_ref().expect("tool calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(calls[0].function.name, "custom");
    }

    #[test]
    fn to_oai_messages_keeps_non_assistant_roles_direct() {
        let messages = vec![Message::system("rules"), Message::user("hello")];
        let out = to_oai_messages(&messages);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role, Role::System.as_str());
        assert_eq!(out[1].role, Role::User.as_str());
    }
}
