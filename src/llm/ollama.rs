use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    ToolDefinition, UsageStats,
    common::{build_http_client, send_streaming_request},
    provider_format::to_ollama_wire,
};

// ── Model context-window cache ────────────────────────────────────────────────

/// Process-global cache mapping Ollama model names to their context-window size
/// (in tokens), populated by [`OllamaProvider::list_models`] via `/api/show`.
static OLLAMA_CONTEXT_CACHE: OnceLock<RwLock<HashMap<String, usize>>> = OnceLock::new();

fn context_cache() -> &'static RwLock<HashMap<String, usize>> {
    OLLAMA_CONTEXT_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub struct OllamaProvider {
    pub base_url: String,
    pub model: String,
    /// Optional Bearer token injected as `Authorization: Bearer <api_key>`.
    /// Used when connecting to an authenticated proxy such as Open WebUI.
    pub api_key: Option<String>,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            client: build_http_client(),
        }
    }

    /// Look up the context-window size for `model` from the cache populated by
    /// [`OllamaProvider::list_models`].  Returns `None` on cache miss.
    pub fn cached_context_window(model: &str) -> Option<usize> {
        let map = context_cache().read().ok()?;
        map.get(model).copied()
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    stream: bool,
}

#[derive(Serialize)]
struct ChatRequestWithTools {
    model: String,
    messages: Vec<serde_json::Value>,
    tools: Vec<OllamaToolDef>,
    stream: bool,
}

/// Tool definition sent in the request.
#[derive(Serialize)]
struct OllamaToolDef {
    r#type: &'static str,
    function: OllamaFunctionDef,
}

#[derive(Serialize)]
struct OllamaFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatChunk {
    message: ChunkMessage,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<usize>,
    #[serde(default)]
    eval_count: Option<usize>,
}

#[derive(Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
    /// Present when the model decides to call a tool.
    #[serde(default)]
    tool_calls: Vec<ToolCallChunk>,
}

#[derive(Deserialize)]
struct ToolCallChunk {
    function: ToolCallFunction,
}

#[derive(Deserialize)]
struct ToolCallFunction {
    name: String,
    /// Ollama may return `arguments` as a JSON object **or** as a
    /// string-encoded JSON object depending on the model/version.
    /// `coerce_arguments` normalises the string case.
    arguments: serde_json::Value,
}

/// Normalise tool-call arguments: if Ollama returned them as a JSON string
/// (e.g. `"{\"path\":\".\"}"`), parse that string into an object.
/// Returns the value unchanged if it is already an object or array.
fn coerce_arguments(v: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::String(s) = &v
        && let Ok(parsed) = serde_json::from_str(s)
    {
        return parsed;
    }
    v
}
#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

/// Response from `POST /api/show` — we only need the `model_info` map which
/// contains architecture-specific parameters including `llama.context_length`.
#[derive(Deserialize, Default)]
struct ShowResponse {
    #[serde(default)]
    model_info: HashMap<String, serde_json::Value>,
}

// ── History serialisation ─────────────────────────────────────────────────────

// ── NDJSON helper ─────────────────────────────────────────────────────────────
//
// Parses an Ollama NDJSON chunk and emits the corresponding LlmEvents.
// Returns `true` when the stream is finished (done=true or error).
fn parse_ndjson_line(
    line: &str,
    events: &mut Vec<LlmEvent>,
    emitted_tool_intent: &mut bool,
) -> bool {
    if line.is_empty() {
        return false;
    }
    match serde_json::from_str::<ChatChunk>(line) {
        Ok(chunk) => {
            if !chunk.message.tool_calls.is_empty() {
                if !*emitted_tool_intent {
                    *emitted_tool_intent = true;
                    events.push(LlmEvent::ToolIntentStart);
                }
                for (i, tc) in chunk.message.tool_calls.iter().enumerate() {
                    events.push(LlmEvent::ToolCall {
                        id: format!("call_{i}"),
                        name: tc.function.name.clone(),
                        args: coerce_arguments(tc.function.arguments.clone()),
                    });
                }
            } else {
                if !chunk.message.thinking.is_empty() {
                    events.push(LlmEvent::ThinkingToken(chunk.message.thinking.clone()));
                }
                if !chunk.message.content.is_empty() {
                    events.push(LlmEvent::Token {
                        text: chunk.message.content.clone(),
                        phase: if *emitted_tool_intent {
                            AssistantPhase::Provisional
                        } else {
                            AssistantPhase::Unknown
                        },
                    });
                }
            }
            if chunk.done {
                if chunk.prompt_eval_count.is_some() || chunk.eval_count.is_some() {
                    events.push(LlmEvent::Usage(UsageStats {
                        input_tokens: chunk.prompt_eval_count,
                        output_tokens: chunk.eval_count,
                        total_tokens: match (chunk.prompt_eval_count, chunk.eval_count) {
                            (Some(i), Some(o)) => Some(i.saturating_add(o)),
                            _ => None,
                        },
                    }));
                }
                events.push(LlmEvent::Done);
                return true;
            }
        }
        Err(e) => {
            events.push(LlmEvent::Error(ProviderError::other(
                "Ollama",
                format!("Parse error: {e}"),
            )));
            return true;
        }
    }
    false
}

// ── Provider implementation ───────────────────────────────────────────────────

impl LlmProvider for OllamaProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        let url = format!("{}/api/chat", self.base_url);
        let model = self.model.clone();
        let client = self.client.clone();
        let api_key = self.api_key.clone();

        Box::pin(async_stream::stream! {
            let body = ChatRequest {
                model,
                messages: to_ollama_wire(&messages),
                stream: true,
            };

            if let Ok(json) = serde_json::to_string_pretty(&body) {
                log::debug!("[TAU_DEBUG] → ollama request:\n{json}");
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            let mut line_num = 0usize;
            let mut emitted_tool_intent = false;
            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield LlmEvent::Error(ProviderError::network("Ollama", e.to_string())); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if !line.is_empty() {
                        log::debug!("[TAU_DEBUG] ← ollama chunk {line_num}: {line}");
                        line_num += 1;
                    }
                    let mut events = Vec::new();
                    let done = parse_ndjson_line(&line, &mut events, &mut emitted_tool_intent);
                    for ev in events { yield ev; }
                    if done { return; }
                }
            }
            yield LlmEvent::Done;
        })
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        let url = format!("{}/api/chat", self.base_url);
        let model = self.model.clone();
        let client = self.client.clone();
        let api_key = self.api_key.clone();

        Box::pin(async_stream::stream! {
            let ollama_tools: Vec<OllamaToolDef> = tools
                .iter()
                .map(|t| OllamaToolDef {
                    r#type: "function",
                    function: OllamaFunctionDef {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect();

            let body = ChatRequestWithTools {
                model,
                messages: to_ollama_wire(&messages),
                tools: ollama_tools,
                stream: true,
            };

            if let Ok(json) = serde_json::to_string_pretty(&body) {
                log::debug!("[TAU_DEBUG] → ollama request:\n{json}");
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            let mut line_num = 0usize;
            let mut emitted_tool_intent = false;
            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield LlmEvent::Error(ProviderError::network("Ollama", e.to_string())); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if !line.is_empty() {
                        log::debug!("[TAU_DEBUG] ← chunk {line_num}: {line}");
                        line_num += 1;
                    }
                    let mut events = Vec::new();
                    let done = parse_ndjson_line(&line, &mut events, &mut emitted_tool_intent);
                    for ev in events { yield ev; }
                    if done { return; }
                }
            }
            yield LlmEvent::Done;
        })
    }

    fn list_models(&self) -> ModelListFuture {
        let tags_url = format!("{}/api/tags", self.base_url);
        let show_url = format!("{}/api/show", self.base_url);
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        Box::pin(async move {
            let models = super::common::fetch_model_list::<TagsResponse, _>(
                &client,
                &tags_url,
                "Ollama",
                api_key.as_deref(),
                &[],
                |r| r.models.into_iter().map(|m| m.name).collect(),
            )
            .await?;

            // For each model, fetch /api/show to get its context window size.
            // We do this best-effort: failures are logged and skipped.
            for model_name in &models {
                let mut req = client
                    .post(&show_url)
                    .json(&serde_json::json!({ "model": model_name, "verbose": false }));
                if let Some(key) = &api_key {
                    req = req.bearer_auth(key);
                }
                match req.send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<ShowResponse>().await {
                            Ok(show) => {
                                // The context length is stored under the
                                // architecture-prefixed key, e.g.
                                // "llama.context_length", "gemma3.context_length", etc.
                                // We scan for any key ending with ".context_length".
                                let ctx = show
                                    .model_info
                                    .iter()
                                    .find(|(k, _)| k.ends_with(".context_length"))
                                    .and_then(|(_, v)| v.as_u64())
                                    .map(|n| n as usize);
                                if let Some(ctx_len) = ctx {
                                    log::debug!(
                                        "ollama model {model_name} context_length={ctx_len}"
                                    );
                                    if let Ok(mut map) = context_cache().write() {
                                        map.insert(model_name.clone(), ctx_len);
                                    }
                                }
                            }
                            Err(e) => {
                                log::debug!("ollama /api/show parse error for {model_name}: {e}");
                            }
                        }
                    }
                    Ok(resp) => {
                        log::debug!(
                            "ollama /api/show returned {} for {model_name}",
                            resp.status()
                        );
                    }
                    Err(e) => {
                        log::debug!("ollama /api/show request failed for {model_name}: {e}");
                    }
                }
            }

            Ok(models)
        })
    }
}
