use serde::{Deserialize, Serialize};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    ToolDefinition, UsageStats,
    common::{
        StreamControl, build_http_client, infer_initiator, send_streaming_request, stream_sse_lines,
    },
    provider_format::to_openai_wire,
};

pub struct OpenAiProvider {
    base_url: String,
    model: String,
    api_key: String,
    extra_headers: Vec<(String, String)>,
    /// Optional stable key used to influence OpenAI prompt-cache routing.
    /// When set, the value is sent as `prompt_cache_key` in every request,
    /// which helps the API route to the same machine that already cached the
    /// prompt prefix for this session.
    prompt_cache_key: Option<String>,
    /// Optional reasoning effort level (e.g. "low", "medium", "high") sent
    /// as `reasoning_effort` in the request body.  Only meaningful for
    /// models that support reasoning/thinking (e.g. DeepSeek-R1, o-series).
    reasoning_effort: Option<String>,
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
            prompt_cache_key: None,
            reasoning_effort: None,
            client: build_http_client(),
        }
    }

    /// Set a stable `prompt_cache_key` for this provider instance.
    ///
    /// The key is sent with every request to help OpenAI route requests to the
    /// same server that already holds the cached prompt prefix, improving cache
    /// hit rates.  A session ID is a good choice — it is stable across
    /// restarts of the application for the same session file.
    pub fn with_prompt_cache_key(mut self, key: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(key.into());
        self
    }

    /// Set a reasoning effort level for this provider instance.
    ///
    /// The value is sent as `reasoning_effort` in the request body (e.g.
    /// "low", "medium", "high").  Only models that support reasoning/
    /// thinking (e.g. DeepSeek-R1, OpenAI o-series) will act on it.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    fn stream_inner(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream {
        let url = format!("{}/chat/completions", self.base_url);
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let prompt_cache_key = self.prompt_cache_key.clone();
        let reasoning_effort = self.reasoning_effort.clone();
        let client = self.client.clone();

        log::debug!(
            "OpenAiProvider::stream_inner: model={} prompt_cache_key={:?} reasoning_effort={:?}",
            model,
            prompt_cache_key,
            reasoning_effort,
        );

        Box::pin(async_stream::stream! {
            let oai_messages: Vec<serde_json::Value> = to_openai_wire(&messages);

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
                prompt_cache_key,
                reasoning_effort,
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

            // Accumulate partial tool-call deltas keyed by index.
            let mut tool_calls: std::collections::HashMap<u32, PartialToolCall> = std::collections::HashMap::new();

            let mut stream = stream_sse_lines("OpenAI", response, move |line, events| {
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
                        events.push(LlmEvent::ToolCall {
                            id: tc.id.clone().unwrap_or_else(|| format!("call_{i}")),
                            name: tc.name.clone(),
                            args,
                        });
                    }
                    events.push(LlmEvent::Done);
                    return StreamControl::Done;
                }

                let chunk: ChatChunk = match serde_json::from_str(line) {
                    Ok(c) => c,
                    Err(e) => {
                        events.push(LlmEvent::Error(ProviderError::other("OpenAI", format!("Parse error: {e}"))));
                        return StreamControl::Done;
                    }
                };

                if let Some(usage) = chunk.usage {
                    events.push(LlmEvent::Usage(UsageStats {
                        input_tokens: usage.prompt_tokens,
                        output_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                        cached_tokens: usage
                            .prompt_tokens_details
                            .and_then(|d| d.cached_tokens),
                    }));
                }

                for choice in chunk.choices {
                    let delta = choice.delta;

                    if let Some(reasoning) = delta.reasoning_content
                        && !reasoning.is_empty()
                    {
                        events.push(LlmEvent::ThinkingToken(reasoning));
                    }

                    if let Some(content) = delta.content
                        && !content.is_empty() {
                            events.push(LlmEvent::Token {
                                text: content,
                                phase: if !tool_calls.is_empty() {
                                    AssistantPhase::Provisional
                                } else {
                                    AssistantPhase::Unknown
                                },
                            });
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
                        // Emit ToolCallStart once we have both id and name.
                        if !entry.started
                            && entry.id.is_some()
                            && !entry.name.is_empty()
                        {
                            entry.started = true;
                            events.push(LlmEvent::ToolCallStart {
                                id: entry.id.clone().unwrap(),
                                name: entry.name.clone(),
                            });
                        }
                        if let Some(args) = tc_delta.function.arguments
                            && !args.is_empty()
                        {
                            let id = entry.id.clone().unwrap_or_default();
                            entry.arguments.push_str(&args);
                            events.push(LlmEvent::ToolCallArgsDelta {
                                id,
                                partial_json: args,
                            });
                        }
                    }
                }

                StreamControl::Continue
            });

            use futures_util::StreamExt as _;
            while let Some(ev) = stream.next().await {
                yield ev;
            }
        })
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OaiToolDef>>,
    /// Stable key used to influence prompt-cache routing on the OpenAI side.
    /// See <https://platform.openai.com/docs/guides/prompt-caching>.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    /// Reasoning effort level ("low", "medium", "high") for models that
    /// support configurable reasoning/thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
}

#[derive(Deserialize, Default)]
struct ChunkUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    total_tokens: Option<usize>,
    /// Nested details carrying per-category token counts.
    prompt_tokens_details: Option<PromptTokensDetails>,
}

/// Nested object inside `usage.prompt_tokens_details`.
#[derive(Deserialize, Default)]
struct PromptTokensDetails {
    cached_tokens: Option<usize>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
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
    /// Whether we have already emitted `ToolCallStart` for this call.
    started: bool,
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
            super::common::fetch_model_list::<ModelsResponse, _>(
                &client,
                &url,
                "OpenAI",
                Some(&api_key),
                &extra_headers,
                |r| r.data.into_iter().map(|m| m.id).collect(),
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::infer_initiator;
    use crate::llm::Message;
    use crate::llm::common::normalize_tool_name;
    use crate::llm::provider_format::to_openai_wire;

    #[test]
    fn normalize_tool_name_maps_emoji_aliases_and_passthrough() {
        assert_eq!(normalize_tool_name("👀"), "read_file");
        assert_eq!(normalize_tool_name("✏️"), "write_file");
        assert_eq!(normalize_tool_name("✍️"), "write_file");
        assert_eq!(normalize_tool_name("📝"), "edit_file");
        assert_eq!(normalize_tool_name("💻"), "bash");
        assert_eq!(normalize_tool_name("🔍"), "find_files");
        assert_eq!(normalize_tool_name("🧑"), "ask_user");
        assert_eq!(normalize_tool_name("❓"), "ask_user");
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

        let out = to_openai_wire(&messages);
        assert_eq!(out.len(), 4);

        let assistant = &out[0];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "I will call tools");
        let calls = assistant["tool_calls"].as_array().expect("tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "read_file");
        assert_eq!(calls[1]["id"], "call_2");
        assert_eq!(calls[1]["function"]["name"], "bash");

        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_1");
        assert_eq!(out[2]["role"], "tool");
        assert_eq!(out[2]["tool_call_id"], "call_2");
        assert_eq!(out[3]["role"], "user");
        assert_eq!(out[3]["content"], "thanks");
    }

    #[test]
    fn to_oai_messages_skips_empty_assistant_without_tool_calls() {
        let out = to_openai_wire(&[Message::assistant("")]);
        assert!(out.is_empty());
    }

    #[test]
    fn to_oai_messages_handles_standalone_tool_call_with_fallback_id() {
        let mut tc = Message::tool_call("provided", "custom", serde_json::json!({"x": 1}));
        tc.tool_call_id = None;

        let out = to_openai_wire(&[tc]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        let calls = out[0]["tool_calls"].as_array().expect("tool calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_0");
        assert_eq!(calls[0]["function"]["name"], "custom");
    }

    #[test]
    fn to_oai_messages_keeps_non_assistant_roles_direct() {
        let messages = vec![Message::system("rules"), Message::user("hello")];
        let out = to_openai_wire(&messages);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["role"], "user");
    }
}
