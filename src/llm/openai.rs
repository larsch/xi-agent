use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmRequestContext, LlmStream, Message, ModelListFuture,
    ProviderError, ToolDefinition, UsageStats,
    common::{
        StreamControl, build_http_client, infer_initiator, sanitize_json_control_chars,
        send_streaming_request, stream_sse_lines,
    },
    provider_format::to_openai_wire,
};

pub struct OpenAiProvider {
    base_url: String,
    model: String,
    api_key: String,
    extra_headers: Vec<(String, String)>,
    /// Optional reasoning effort level (e.g. "low", "medium", "high") sent
    /// as `reasoning_effort` in the request body.  Only meaningful for
    /// models that support reasoning/thinking (e.g. DeepSeek-R1, o-series).
    reasoning_effort: Option<String>,
    /// DeepSeek-style `thinking` param (`{"type": "enabled"}`).  Sent in
    /// addition to `reasoning_effort` when using the DeepSeek API, which
    /// does not support OpenAI's `reasoning_effort` parameter.
    deepseek_thinking: Option<serde_json::Value>,
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
            reasoning_effort: None,
            deepseek_thinking: None,
            client: build_http_client(),
        }
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

    /// Enable DeepSeek-style thinking mode.
    ///
    /// Sends `"thinking": {"type": "enabled"}` in the request body when
    /// `level` is anything other than [`ThinkingLevel::Off`].  DeepSeek's
    /// API does not accept OpenAI-style `reasoning_effort`, so this is used
    /// instead for DeepSeek endpoints.
    pub fn with_deepseek_thinking(mut self, level: crate::thinking::ThinkingLevel) -> Self {
        self.deepseek_thinking = match level {
            crate::thinking::ThinkingLevel::Off => None,
            _ => Some(serde_json::json!({"type": "enabled"})),
        };
        self
    }

    /// Look up the context-window size for `model` in the cache populated from
    /// an OpenAI-compatible `/models` response.
    ///
    /// Returns `None` when discovery has not run yet or the endpoint does not
    /// report a context window for this model.
    pub fn cached_context_window(model: &str) -> Option<usize> {
        let map = context_cache().read().ok()?;
        map.get(model).copied()
    }

    /// Spawn a background `/models` fetch that records context-window sizes.
    ///
    /// Called at provider build time so the size is known before the first
    /// turn instead of only after the model picker has been opened.  No-op when
    /// the current model's window is already cached, or when there is no tokio
    /// runtime to spawn on (the background refresh is an optimisation only).
    pub fn spawn_context_window_refresh(&self) {
        if Self::cached_context_window(&self.model).is_some() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            log::debug!("context-window discovery skipped: no tokio runtime");
            return;
        };
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        handle.spawn(async move {
            fetch_and_cache_model_contexts(&client, &base_url, &api_key, &extra_headers).await;
        });
    }

    fn stream_inner(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        context: LlmRequestContext,
    ) -> LlmStream {
        let url = format!("{}/chat/completions", self.base_url);
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let prompt_cache_key = context.prompt_cache_key.clone();
        let reasoning_effort = self.reasoning_effort.clone();
        let deepseek_thinking = self.deepseek_thinking.clone();
        let client = self.client.clone();

        log::debug!(
            "OpenAiProvider::stream_inner: model={} prompt_cache_key={:?} reasoning_effort={:?} deepseek_thinking={}",
            model,
            prompt_cache_key,
            reasoning_effort,
            deepseek_thinking.is_some(),
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
                thinking: deepseek_thinking,
            };

            if let Ok(payload) = serde_json::to_value(&body) {
                crate::debug_log::log_structured(
                    log::Level::Debug,
                    "xi::llm::openai",
                    serde_json::json!({
                        "event": "llm_request",
                        "provider": "openai",
                        "payload": payload,
                    }),
                );
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
                        let sanitized = sanitize_json_control_chars(&tc.arguments);
                        match serde_json::from_str(&sanitized) {
                            Ok(args) => {
                                events.push(LlmEvent::ToolCall {
                                    id: tc.id.clone().unwrap_or_else(|| format!("call_{i}")),
                                    name: tc.name.clone(),
                                    args,
                                });
                            }
                            Err(e) => {
                                events.push(LlmEvent::Error(ProviderError::other(
                                    "OpenAI",
                                    format!(
                                        "failed to parse JSON arguments for tool call '{}': {e}",
                                        tc.name,
                                    ),
                                )));
                                return StreamControl::Done;
                            }
                        }
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
    /// DeepSeek-style thinking parameter (`{"type": "enabled"}`).  Mutually
    /// exclusive with `reasoning_effort` in practice — DeepSeek rejects
    /// `reasoning_effort` and OpenAI ignores `thinking`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<serde_json::Value>,
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
    /// Context window as reported by DeepSeek and several other
    /// OpenAI-compatible gateways.
    #[serde(default)]
    context_window: Option<usize>,
    /// OpenRouter's spelling of the same field.
    #[serde(default)]
    context_length: Option<usize>,
    /// vLLM's spelling of the same field.
    #[serde(default)]
    max_model_len: Option<usize>,
}

impl ModelEntry {
    /// Best available context window for this model, ignoring zero values.
    fn context_size(&self) -> Option<usize> {
        self.context_window
            .or(self.context_length)
            .or(self.max_model_len)
            .filter(|n| *n > 0)
    }
}

// ── Context-window cache ──────────────────────────────────────────────────────

/// Process-global cache of context windows reported by an OpenAI-compatible
/// `/models` endpoint, keyed by the exact model `id`.
///
/// Populated by [`OpenAiProvider::list_models`] and by the background refresh
/// spawned from [`OpenAiProvider::spawn_context_window_refresh`].  Models whose
/// entry omits every context-window field are not inserted, so the caller's
/// hard-coded fallback still applies.
static MODEL_CONTEXT_CACHE: OnceLock<RwLock<HashMap<String, usize>>> = OnceLock::new();

fn context_cache() -> &'static RwLock<HashMap<String, usize>> {
    MODEL_CONTEXT_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn record_model_contexts(entries: &[ModelEntry]) {
    let discovered: Vec<(String, usize)> = entries
        .iter()
        .filter_map(|e| Some((e.id.clone(), e.context_size()?)))
        .collect();
    if discovered.is_empty() {
        return;
    }
    if let Ok(mut map) = context_cache().write() {
        for (id, context_window) in discovered {
            log::debug!("openai model {id} context_window={context_window}");
            map.insert(id, context_window);
        }
    }
}

/// Fetch `{base_url}/models` and record the context windows it reports.
///
/// Best-effort: failures (offline endpoint, 401, gateways that omit the field)
/// are logged at debug level and leave the caller's fallback untouched.
async fn fetch_and_cache_model_contexts(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    extra_headers: &[(String, String)],
) {
    let url = format!("{base_url}/models");
    let result = super::common::fetch_model_list::<ModelsResponse, _>(
        client,
        &url,
        "OpenAI",
        Some(api_key),
        extra_headers,
        |r| {
            record_model_contexts(&r.data);
            r.data.into_iter().map(|m| m.id).collect()
        },
    )
    .await;

    match result {
        Ok(ids) => log::debug!("context-window discovery: {} models from {url}", ids.len()),
        Err(e) => log::debug!("context-window discovery failed for {url}: {e}"),
    }
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
    fn stream_chat(&self, messages: Vec<Message>, context: LlmRequestContext) -> LlmStream {
        self.stream_inner(messages, vec![], context)
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        context: LlmRequestContext,
    ) -> LlmStream {
        self.stream_inner(messages, tools, context)
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
                |r| {
                    record_model_contexts(&r.data);
                    r.data.into_iter().map(|m| m.id).collect()
                },
            )
            .await
        })
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::context_cache;

    /// Insert a synthetic entry into the global context-window cache.
    /// Use unique model names in tests to avoid cross-test pollution.
    pub fn seed_context_window(model: &str, context_window: usize) {
        context_cache()
            .write()
            .expect("context cache poisoned")
            .insert(model.to_string(), context_window);
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

    // ── Context-window discovery ──────────────────────────────────────────

    use super::OpenAiProvider;
    use crate::llm::LlmProvider;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// `/models` bodies with every context-window spelling xi understands,
    /// plus one entry that reports none.  The first entry mirrors a real
    /// DeepSeek response (extra and nested fields must not break parsing).
    const MODELS_BODY: &str = r#"{
        "object": "list",
        "data": [
            {
                "id": "__oai_ctx_window__",
                "object": "model",
                "owned_by": "deepseek",
                "name": "DeepSeek-V4.1-Flash",
                "context_window": 1048576,
                "max_output_tokens": 393216,
                "input_modalities": ["text", "image"],
                "output_modalities": ["text"],
                "effort": {"supported_levels": ["low", "high", "max"], "default_level": "high"},
                "api_capabilities": {"anthropic_messages": {"tool_update": "addition-only"}}
            },
            {"id": "__oai_ctx_length__", "context_length": 131072},
            {"id": "__oai_max_model_len__", "max_model_len": 32768},
            {"id": "__oai_no_ctx__", "object": "model"}
        ]
    }"#;

    async fn mock_models_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(MODELS_BODY, "application/json"))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn list_models_caches_reported_context_windows() {
        let server = mock_models_server().await;
        let provider = OpenAiProvider::new(server.uri(), "__oai_ctx_window__", "test-key");

        let models = provider.list_models().await.expect("list models");

        assert_eq!(models.len(), 4);
        assert_eq!(
            OpenAiProvider::cached_context_window("__oai_ctx_window__"),
            Some(1_048_576)
        );
        assert_eq!(
            OpenAiProvider::cached_context_window("__oai_ctx_length__"),
            Some(131_072)
        );
        assert_eq!(
            OpenAiProvider::cached_context_window("__oai_max_model_len__"),
            Some(32_768)
        );
        assert_eq!(
            OpenAiProvider::cached_context_window("__oai_no_ctx__"),
            None,
            "entries without a context-window field must not be cached"
        );
    }

    #[tokio::test]
    async fn spawn_context_window_refresh_populates_cache_in_background() {
        let server = mock_models_server().await;
        let provider = OpenAiProvider::new(server.uri(), "__oai_max_model_len__", "test-key");

        provider.spawn_context_window_refresh();

        // The refresh runs in a spawned task, so poll instead of assuming it
        // has completed by the time we look.
        for _ in 0..100 {
            if OpenAiProvider::cached_context_window("__oai_ctx_window__").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert_eq!(
            OpenAiProvider::cached_context_window("__oai_ctx_window__"),
            Some(1_048_576)
        );
    }
}
