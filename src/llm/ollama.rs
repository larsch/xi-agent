use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmRequestContext, LlmStream, Message, ModelListFuture,
    ProviderError, ToolDefinition, UsageStats,
    common::{StreamControl, build_http_client, send_streaming_request, stream_ndjson_lines},
    provider_format::to_ollama_wire,
};

// ── Model context-window cache ────────────────────────────────────────────────

/// Process-global cache mapping Ollama model names to their runtime
/// context-window size (in tokens), populated by querying `/api/ps`.
///
/// This is the ground truth — `/api/ps` reports the actual `num_ctx` the
/// server is using.  It only covers models currently loaded in memory.
static OLLAMA_RUNTIME_CONTEXT_CACHE: OnceLock<RwLock<HashMap<String, usize>>> = OnceLock::new();

/// Process-global set of model names known to be available from the Ollama
/// server (populated by `/api/tags`).  Names are normalised (no `:latest`).
static OLLAMA_KNOWN_MODELS: OnceLock<RwLock<HashSet<String>>> = OnceLock::new();

fn runtime_context_cache() -> &'static RwLock<HashMap<String, usize>> {
    OLLAMA_RUNTIME_CONTEXT_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn known_models() -> &'static RwLock<HashSet<String>> {
    OLLAMA_KNOWN_MODELS.get_or_init(|| RwLock::new(HashSet::new()))
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

    /// Look up the context-window size for `model` from the runtime cache
    /// populated by [`fetch_and_cache_running_contexts`] via `/api/ps`.
    ///
    /// Normalises the model name by stripping a trailing `:latest` tag so that
    /// `qwen3-coder` and `qwen3-coder:latest` hit the same cache entry.
    ///
    /// Returns `None` when the model is not currently loaded (we cannot
    /// trust the GGUF `context_length` from `/api/show` because the server
    /// may override it with `num_ctx`).
    pub fn cached_context_window(model: &str) -> Option<usize> {
        let map = runtime_context_cache().read().ok()?;
        let normalized = model.strip_suffix(":latest").unwrap_or(model);
        map.get(normalized).or_else(|| map.get(model)).copied()
    }

    /// Returns `true` if `model` is known to be available from the Ollama
    /// server (was listed by `/api/tags` during a `list_models` call).
    pub fn is_known_model(model: &str) -> bool {
        let set = known_models().read().ok();
        let normalized = model.strip_suffix(":latest").unwrap_or(model);
        set.map(|s| s.contains(normalized) || s.contains(model))
            .unwrap_or(false)
    }
}

/// Attempt to populate the global context-window cache for `model_name`
/// by calling `{base_url}/api/show`.  Failures are logged at debug level
/// and do not disturb the caller.
///
/// Query `{base_url}/api/ps` and populate the runtime context-window cache
/// with the actual `context_length` for each currently loaded model.
///
/// This is the canonical context-window discovery routine, shared by
/// [`OllamaProvider::list_models`] and by providers that proxy through
/// an Ollama-compatible backend (e.g. Open WebUI).
///
/// Failures are logged at debug level and do not disturb the caller.
pub async fn fetch_and_cache_running_contexts(base_url: &str, api_key: Option<&str>) {
    let ps_url = format!("{}/api/ps", base_url.trim_end_matches('/'));
    let client = build_http_client();

    let mut req = client.get(&ps_url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<PsResponse>().await {
            Ok(ps) => {
                if let Ok(mut map) = runtime_context_cache().write() {
                    for m in &ps.models {
                        let ctx = m.context_length;
                        log::debug!("ollama running model {} context_length={ctx}", m.name);
                        map.insert(m.name.clone(), ctx);
                        if let Some(normalized) = m.name.strip_suffix(":latest")
                            && normalized != m.name
                        {
                            map.insert(normalized.to_string(), ctx);
                        }
                    }
                }
            }
            Err(e) => {
                log::debug!("ollama /api/ps parse error: {e}");
            }
        },
        Ok(resp) => {
            log::debug!("ollama /api/ps returned {}", resp.status());
        }
        Err(e) => {
            log::debug!("ollama /api/ps request failed: {e}");
        }
    }
}

/// Response from `GET /api/ps`.
#[derive(Deserialize)]
struct PsResponse {
    models: Vec<PsModel>,
}

#[derive(Deserialize)]
struct PsModel {
    name: String,
    context_length: usize,
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

// ── History serialisation ─────────────────────────────────────────────────────

// ── NDJSON helper ─────────────────────────────────────────────────────────────
//
// Parses an Ollama NDJSON chunk and emits the corresponding LlmEvents.
// Returns `true` when the stream is finished (done=true or error).
fn parse_ndjson_line(line: &str, events: &mut Vec<LlmEvent>) -> bool {
    if line.is_empty() {
        return false;
    }
    match serde_json::from_str::<ChatChunk>(line) {
        Ok(chunk) => {
            if !chunk.message.tool_calls.is_empty() {
                for (i, tc) in chunk.message.tool_calls.iter().enumerate() {
                    let id = format!("call_{i}");
                    // Ollama delivers complete tool calls — emit Start + Call together.
                    events.push(LlmEvent::ToolCallStart {
                        id: id.clone(),
                        name: tc.function.name.clone(),
                    });
                    events.push(LlmEvent::ToolCall {
                        id,
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
                        phase: AssistantPhase::Unknown,
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
                        cached_tokens: None,
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
    fn stream_chat(&self, messages: Vec<Message>, _context: LlmRequestContext) -> LlmStream {
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

            if let Ok(payload) = serde_json::to_value(&body) {
                crate::debug_log::log_structured(
                    log::Level::Debug,
                    "xi::llm::ollama",
                    serde_json::json!({
                        "event": "llm_request",
                        "provider": "ollama",
                        "payload": payload,
                    }),
                );
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut stream = stream_ndjson_lines("Ollama", response, move |line, events| {
                if parse_ndjson_line(line, events) {
                    StreamControl::Done
                } else {
                    StreamControl::Continue
                }
            });

            use futures_util::StreamExt as _;
            while let Some(ev) = stream.next().await {
                yield ev;
            }
            yield LlmEvent::Done;
        })
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        _context: LlmRequestContext,
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

            if let Ok(payload) = serde_json::to_value(&body) {
                crate::debug_log::log_structured(
                    log::Level::Debug,
                    "xi::llm::ollama",
                    serde_json::json!({
                        "event": "llm_request",
                        "provider": "ollama",
                        "payload": payload,
                    }),
                );
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut stream = stream_ndjson_lines("Ollama", response, move |line, events| {
                if parse_ndjson_line(line, events) {
                    StreamControl::Done
                } else {
                    StreamControl::Continue
                }
            });

            use futures_util::StreamExt as _;
            while let Some(ev) = stream.next().await {
                yield ev;
            }
            yield LlmEvent::Done;
        })
    }

    fn list_models(&self) -> ModelListFuture {
        let tags_url = format!("{}/api/tags", self.base_url);
        let base_url = self.base_url.clone();
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

            // Populate the known-models set so that context_window_for_model
            // can tell that these are Ollama models (and skip the hard-coded
            // fallback table when /api/ps doesn't have a runtime context).
            if let Ok(mut set) = known_models().write() {
                for name in &models {
                    set.insert(name.clone());
                    if let Some(normalized) = name.strip_suffix(":latest")
                        && normalized != name
                    {
                        set.insert(normalized.to_string());
                    }
                }
            }

            // Query /api/ps to cache the actual runtime context window
            // for currently loaded models.  We do this best-effort.
            fetch_and_cache_running_contexts(&base_url, api_key.as_deref()).await;

            Ok(models)
        })
    }
}
