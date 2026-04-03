use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;

use super::anthropic::AnthropicProvider;
use super::codex::CodexProvider;
use super::common::build_http_client;
use super::openai::OpenAiProvider;
use super::{LlmProvider, LlmStream, Message, ModelListFuture, ToolDefinition};

// ── Model metadata cache ──────────────────────────────────────────────────────

/// Metadata fetched from the Copilot `/models` endpoint for a single model.
#[derive(Debug, Clone)]
pub struct CopilotModelMeta {
    /// Provider vendor string as returned by the API, e.g. `"Anthropic"` or
    /// `"Azure OpenAI"`.  Empty string when not present in the response.
    pub vendor: String,
    /// Maximum context-window size in tokens, when reported by the API.
    pub max_context_window_tokens: Option<usize>,
}

/// Process-global cache populated by [`CopilotProvider::list_models`].
/// Keyed by the exact model `id` returned by the API (case-preserved).
static COPILOT_MODEL_CACHE: OnceLock<RwLock<HashMap<String, CopilotModelMeta>>> = OnceLock::new();

fn cache() -> &'static RwLock<HashMap<String, CopilotModelMeta>> {
    COPILOT_MODEL_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

// ── Deserialization types (private) ───────────────────────────────────────────

#[derive(Deserialize)]
struct ApiModelsResponse {
    data: Vec<ApiModelEntry>,
}

#[derive(Deserialize)]
struct ApiModelEntry {
    id: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    capabilities: ApiModelCapabilities,
}

#[derive(Deserialize, Default)]
struct ApiModelCapabilities {
    #[serde(default)]
    limits: ApiModelLimits,
}

#[derive(Deserialize, Default)]
struct ApiModelLimits {
    max_context_window_tokens: Option<usize>,
}

// ── Shared Copilot headers ────────────────────────────────────────────────────

fn copilot_extra_headers() -> Vec<(String, String)> {
    vec![
        (
            "User-Agent".to_string(),
            "GitHubCopilotChat/0.35.0".to_string(),
        ),
        ("Editor-Version".to_string(), "vscode/1.107.0".to_string()),
        (
            "Editor-Plugin-Version".to_string(),
            "copilot-chat/0.35.0".to_string(),
        ),
        (
            "Copilot-Integration-Id".to_string(),
            "vscode-chat".to_string(),
        ),
        ("X-Initiator".to_string(), "user".to_string()),
        (
            "Openai-Intent".to_string(),
            "conversation-edits".to_string(),
        ),
    ]
}

// ── Async model fetch ─────────────────────────────────────────────────────────

/// Fetches the Copilot `/models` endpoint, populates the metadata cache,
/// and returns a sorted list of model IDs.
async fn fetch_and_cache_models(
    base_url: String,
    access_token: String,
    extra_headers: Vec<(String, String)>,
) -> Result<Vec<String>, super::ProviderError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = build_http_client();

    super::common::fetch_model_list::<ApiModelsResponse, _>(
        &client,
        &url,
        "Copilot",
        Some(&access_token),
        &extra_headers,
        |parsed| {
            // Populate the metadata cache as a side-effect of parsing.
            if let Ok(mut map) = cache().write() {
                for entry in &parsed.data {
                    map.insert(
                        entry.id.clone(),
                        CopilotModelMeta {
                            vendor: entry.vendor.clone(),
                            max_context_window_tokens: entry
                                .capabilities
                                .limits
                                .max_context_window_tokens,
                        },
                    );
                }
                log::debug!("copilot model cache populated: {} entries", map.len());
            }
            parsed.data.into_iter().map(|e| e.id).collect()
        },
    )
    .await
}

// ── Route enum ────────────────────────────────────────────────────────────────

/// Which inner provider handles a given Copilot model.
enum CopilotInner {
    OpenAi(OpenAiProvider),
    Anthropic(AnthropicProvider),
    Codex(CodexProvider),
}

// ── CopilotProvider ───────────────────────────────────────────────────────────

/// A unified Copilot provider that routes chat requests to the correct
/// underlying API (OpenAI Chat Completions, Anthropic Messages, or OpenAI
/// Responses) while always fetching the model list from the Copilot `/models`
/// endpoint — regardless of which model is currently active.
pub struct CopilotProvider {
    /// The inner provider selected based on the current model.
    inner: CopilotInner,
    /// Resolved API base URL, stored for use in `list_models()`.
    base_url: String,
    /// Bearer token, stored for use in `list_models()`.
    access_token: String,
}

impl CopilotProvider {
    pub fn new(
        access_token: &str,
        model: &str,
        base_url: Option<&str>,
        reasoning_effort: Option<String>,
    ) -> Self {
        let resolved_base_url = base_url
            .map(|s| s.to_string())
            .unwrap_or_else(|| extract_base_url(access_token));

        let inner = build_inner(model, access_token, &resolved_base_url, reasoning_effort);

        Self {
            inner,
            base_url: resolved_base_url,
            access_token: access_token.to_string(),
        }
    }

    /// Look up the vendor string for `model` from the cache.
    ///
    /// Returns `None` when the cache is unpopulated or the model is absent.
    pub fn cached_vendor(model: &str) -> Option<String> {
        let map = cache().read().ok()?;
        map.get(model).map(|m| m.vendor.clone())
    }

    /// Look up the context-window token limit for `model` from the cache.
    ///
    /// Returns `None` when the cache is unpopulated or the model is absent.
    pub fn cached_context_window(model: &str) -> Option<usize> {
        let map = cache().read().ok()?;
        map.get(model)?.max_context_window_tokens
    }
}

/// Build the correct inner provider based on the cached vendor (primary) or
/// model-name heuristics (cold-start fallback).
fn build_inner(
    model: &str,
    access_token: &str,
    base_url: &str,
    reasoning_effort: Option<String>,
) -> CopilotInner {
    // Primary: vendor from the API metadata cache.
    let vendor = CopilotProvider::cached_vendor(model).unwrap_or_default();
    let use_anthropic = vendor.eq_ignore_ascii_case("anthropic")
        // Cold-start fallback: when vendor is unknown, use name heuristic.
        || (vendor.is_empty() && model.to_ascii_lowercase().starts_with("claude"));

    if use_anthropic {
        log::debug!(
            "copilot transport resolved: api=anthropic-messages base_url={base_url} endpoint=/v1/messages"
        );
        return CopilotInner::Anthropic(AnthropicProvider::new_with_headers(
            base_url.to_string(),
            model,
            access_token,
            true, // bearer_auth
            copilot_extra_headers(),
        ));
    }

    // For OpenAI-vendor models the Responses vs. Chat-Completions split is not
    // encoded in the API; use name heuristics.
    let m = model.to_ascii_lowercase();
    if m.contains("codex") || m.starts_with("gpt-5") {
        let responses_url = format!("{}/v1/responses", base_url.trim_end_matches('/'));
        log::debug!(
            "copilot transport resolved: api=openai-responses base_url={base_url} endpoint={responses_url}"
        );
        CopilotInner::Codex(
            CodexProvider::new_with_headers(
                responses_url,
                model,
                access_token,
                copilot_extra_headers(),
            )
            .with_reasoning_effort(reasoning_effort),
        )
    } else {
        log::debug!(
            "copilot transport resolved: api=openai-chat-completions base_url={base_url} endpoint=/chat/completions"
        );
        CopilotInner::OpenAi(OpenAiProvider::new_with_headers(
            base_url.to_string(),
            model,
            access_token,
            copilot_extra_headers(),
        ))
    }
}

impl LlmProvider for CopilotProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        match &self.inner {
            CopilotInner::OpenAi(p) => p.stream_chat(messages),
            CopilotInner::Anthropic(p) => p.stream_chat(messages),
            CopilotInner::Codex(p) => p.stream_chat(messages),
        }
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        match &self.inner {
            CopilotInner::OpenAi(p) => p.stream_chat_with_tools(messages, tools),
            CopilotInner::Anthropic(p) => p.stream_chat_with_tools(messages, tools),
            CopilotInner::Codex(p) => p.stream_chat_with_tools(messages, tools),
        }
    }

    /// Fetches the full model list from the Copilot `/models` endpoint and
    /// populates the metadata cache as a side-effect.
    fn list_models(&self) -> ModelListFuture {
        let base_url = self.base_url.clone();
        let access_token = self.access_token.clone();
        let headers = copilot_extra_headers();
        Box::pin(fetch_and_cache_models(base_url, access_token, headers))
    }
}

// ── URL helpers ───────────────────────────────────────────────────────────────

/// Extract the API base URL from a Copilot access token, falling back to the
/// known default if the `proxy-ep=` field is absent.
fn extract_base_url(token: &str) -> String {
    if let Some(domain) = token
        .split(';')
        .find_map(|seg| seg.strip_prefix("proxy-ep="))
    {
        // "proxy.individual.githubcopilot.com" → "api.individual.githubcopilot.com"
        let api_domain = domain
            .strip_prefix("proxy.")
            .map(|rest| format!("api.{rest}"))
            .unwrap_or_else(|| domain.to_string());
        return format!("https://{api_domain}");
    }
    "https://api.individual.githubcopilot.com".to_string()
}

// ── Test helpers ──────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod test_helpers {
    use super::{CopilotModelMeta, cache};

    /// Insert a synthetic entry into the global cache.
    /// Use unique model names in tests to avoid cross-test pollution.
    pub fn insert_cache(model: &str, vendor: &str, context_window: Option<usize>) {
        cache().write().unwrap().insert(
            model.to_string(),
            CopilotModelMeta {
                vendor: vendor.to_string(),
                max_context_window_tokens: context_window,
            },
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{CopilotProvider, test_helpers};

    #[test]
    fn cached_vendor_returns_none_for_unknown_model() {
        assert!(CopilotProvider::cached_vendor("__unknown_vendor_model__").is_none());
    }

    #[test]
    fn cached_context_window_returns_none_for_unknown_model() {
        assert!(CopilotProvider::cached_context_window("__unknown_cw_model__").is_none());
    }

    #[test]
    fn cache_round_trips_vendor_and_context_window() {
        test_helpers::insert_cache("__test_anthropic_model__", "Anthropic", Some(200_000));
        assert_eq!(
            CopilotProvider::cached_vendor("__test_anthropic_model__").as_deref(),
            Some("Anthropic")
        );
        assert_eq!(
            CopilotProvider::cached_context_window("__test_anthropic_model__"),
            Some(200_000)
        );
    }

    #[test]
    fn cache_round_trips_empty_vendor_and_none_context_window() {
        test_helpers::insert_cache("__test_no_meta_model__", "", None);
        assert_eq!(
            CopilotProvider::cached_vendor("__test_no_meta_model__").as_deref(),
            Some("")
        );
        assert!(CopilotProvider::cached_context_window("__test_no_meta_model__").is_none());
    }
}
