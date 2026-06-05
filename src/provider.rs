use std::sync::Arc;

use crate::{
    auth::AuthStore,
    config::XiConfig,
    llm::{
        LlmProvider,
        codex::{CodexProvider, DEFAULT_BASE_URL as CODEX_DEFAULT_BASE_URL},
        copilot::CopilotProvider,
        gemini::{DEFAULT_BASE_URL as GEMINI_DEFAULT_BASE_URL, GeminiProvider},
        ollama::OllamaProvider,
        openai::OpenAiProvider,
        test_provider::TestProvider,
    },
    provider_instance::{ApiType, BackendPreset, ProviderInstance},
    thinking::ThinkingLevel,
};

const OPENROUTER_DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_REFERER: &str = "https://github.com/larsch/xi-agent";
const OPENROUTER_TITLE: &str = "xi";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopilotApiRoute {
    OpenAiChatCompletions,
    AnthropicMessages,
    OpenAiResponses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingSupport {
    Applied,
    Ignored(&'static str),
}

/// Classify Copilot model routing in a provider-agnostic way.
///
/// Uses the cached `vendor` field from the Copilot `/models` API as the
/// primary signal.  Falls back to model-name heuristics on cold start (before
/// `list_models()` has populated the cache) and for the OpenAI
/// Chat-Completions vs. Responses split (not exposed in the API).
fn classify_copilot_route(model: &str) -> CopilotApiRoute {
    // Primary: vendor from the API metadata cache.
    if let Some(vendor) = CopilotProvider::cached_vendor(model)
        && vendor.eq_ignore_ascii_case("anthropic")
    {
        return CopilotApiRoute::AnthropicMessages;
        // For OpenAI-vendor models fall through to name heuristics below;
        // the Responses vs. Chat-Completions split is not in the API.
    }
    // Fallback / OpenAI sub-routing: name heuristics.
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        CopilotApiRoute::AnthropicMessages
    } else if m.contains("codex") || m.starts_with("gpt-5") {
        CopilotApiRoute::OpenAiResponses
    } else {
        CopilotApiRoute::OpenAiChatCompletions
    }
}

// ── Instance-based API ────────────────────────────────────────────────────────

/// Return the thinking support level for a named provider instance.
pub fn thinking_support_for_instance(instance: &ProviderInstance, model: &str) -> ThinkingSupport {
    match instance.api_type {
        ApiType::OpenAiResponses => {
            if instance.backend_preset == BackendPreset::Copilot {
                match classify_copilot_route(model) {
                    CopilotApiRoute::OpenAiResponses => ThinkingSupport::Applied,
                    CopilotApiRoute::AnthropicMessages => ThinkingSupport::Ignored(
                        "copilot anthropic route has no thinking mapping yet",
                    ),
                    CopilotApiRoute::OpenAiChatCompletions => ThinkingSupport::Ignored(
                        "copilot chat-completions route does not expose reasoning.effort",
                    ),
                }
            } else {
                ThinkingSupport::Applied
            }
        }
        ApiType::GeminiNative => ThinkingSupport::Applied,
        ApiType::OpenAiCompatible => {
            if instance.backend_preset == BackendPreset::Copilot {
                match classify_copilot_route(model) {
                    CopilotApiRoute::OpenAiResponses => ThinkingSupport::Applied,
                    CopilotApiRoute::AnthropicMessages => ThinkingSupport::Ignored(
                        "copilot anthropic route has no thinking mapping yet",
                    ),
                    CopilotApiRoute::OpenAiChatCompletions => ThinkingSupport::Ignored(
                        "copilot chat-completions route does not expose reasoning.effort",
                    ),
                }
            } else if instance.backend_preset == BackendPreset::OpenAi
                || instance.backend_preset == BackendPreset::OpenRouter
            {
                // OpenAI API and OpenRouter support `reasoning_effort` in the
                // chat completions request body (OpenAI o-series convention).
                ThinkingSupport::Applied
            } else {
                // Generic OpenAI-compatible endpoints (e.g. DeepSeek) may or
                // may not support `reasoning_effort`.  Many don't — they still
                // produce `reasoning_content` in responses autonomously, but
                // sending the parameter triggers a 400 error.  Mark thinking
                // as unsupported so the parameter is not sent; the model's
                // autonomous reasoning tokens are still parsed from responses.
                ThinkingSupport::Ignored(
                    "generic openai-compatible: reasoning_effort not reliably supported",
                )
            }
        }
        ApiType::AnthropicCompatible => {
            ThinkingSupport::Ignored("anthropic route has no thinking mapping yet")
        }
        ApiType::OllamaChatApi => {
            ThinkingSupport::Ignored("ollama provider does not support mapped thinking levels")
        }
        ApiType::Test => ThinkingSupport::Ignored("test provider does not support thinking"),
    }
}

/// Build a provider for a named [`ProviderInstance`], dispatching on its
/// [`ApiType`].
/// Build a provider for a named [`ProviderInstance`], dispatching on its
/// [`ApiType`].
///
/// `session_id` — when `Some`, sets the `prompt_cache_key` on OpenAI-compatible
/// backends so that requests for the same session are routed to the same cached-prefix
/// server, improving cache hit rates across the lifetime of the session.
pub fn build_provider_for_instance(
    instance: &ProviderInstance,
    thinking: ThinkingLevel,
    _config: &XiConfig,
    session_id: Option<&str>,
) -> anyhow::Result<Arc<dyn LlmProvider + Send + Sync>> {
    let model = instance.effective_model();

    match instance.backend_preset {
        // ── Cloud services with AuthStore credentials ─────────────────────
        BackendPreset::Copilot => {
            let store = AuthStore::load_default()?;
            let creds = store.get_copilot().ok_or_else(|| {
                anyhow::anyhow!("Not authenticated for copilot. Run /login copilot.")
            })?;
            log::debug!(
                "provider route selected: instance={} model={} base_url={}",
                instance.id,
                model,
                creds.base_url.as_deref().unwrap_or("<from-token>")
            );
            let p = CopilotProvider::new(
                &creds.access_token,
                model,
                creds.base_url.as_deref(),
                thinking.to_reasoning_effort_string(),
            );
            Ok(Arc::new(p))
        }
        BackendPreset::Codex => {
            let store = AuthStore::load_default()?;
            let creds = store
                .get_codex()
                .ok_or_else(|| anyhow::anyhow!("Not authenticated for codex. Run /login codex."))?;
            let base_url = instance
                .base_url
                .clone()
                .unwrap_or_else(|| CODEX_DEFAULT_BASE_URL.to_string());
            let p = CodexProvider::new(base_url, model, creds.access_token, creds.account_id)
                .with_reasoning_effort(thinking.to_reasoning_effort_string());
            Ok(Arc::new(p))
        }
        BackendPreset::Gemini => {
            let store = AuthStore::load_default()?;
            let creds = store.get_gemini().ok_or_else(|| {
                anyhow::anyhow!("Not authenticated for gemini. Run /login gemini.")
            })?;
            let base_url = instance
                .base_url
                .clone()
                .unwrap_or_else(|| GEMINI_DEFAULT_BASE_URL.to_string());
            let p = GeminiProvider::new(base_url, model, creds.access_token, creds.project_id)
                .with_thinking_level(thinking.to_gemini_thinking_level());
            Ok(Arc::new(p))
        }

        // ── OpenAI-compatible cloud services (api_key in instance) ─────────
        BackendPreset::OpenAi | BackendPreset::OpenAiCompatible => {
            let base_url = instance
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let api_key = instance.api_key.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing API key for provider '{}'. Set api_key in config.",
                    instance.id
                )
            })?;
            let mut p = OpenAiProvider::new(base_url, model, api_key);
            log::debug!(
                "provider build: id={} backend={:?} api={:?} thinking={:?} session_id={:?}",
                instance.id,
                instance.backend_preset,
                instance.api_type,
                thinking,
                session_id,
            );
            // Only send reasoning_effort and prompt_cache_key for OpenAI API;
            // generic openai-compatible endpoints (e.g. DeepSeek) may reject them.
            if instance.backend_preset == BackendPreset::OpenAi {
                log::debug!(
                    "provider build: enabling reasoning_effort+prompt_cache_key for OpenAi backend"
                );
                p = p.with_reasoning_effort(thinking.to_reasoning_effort_string());
                if let Some(sid) = session_id {
                    p = p.with_prompt_cache_key(sid);
                }
            } else {
                log::debug!(
                    "provider build: skipping reasoning_effort+prompt_cache_key (backend is not OpenAi)"
                );
            }
            Ok(Arc::new(p))
        }
        BackendPreset::OpenRouter => {
            let base_url = instance
                .base_url
                .clone()
                .unwrap_or_else(|| OPENROUTER_DEFAULT_BASE_URL.to_string());
            let api_key = instance.api_key.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing API key for provider '{}'. Set api_key in config.",
                    instance.id
                )
            })?;
            let mut p = OpenAiProvider::new_with_headers(
                base_url,
                model,
                api_key,
                vec![
                    ("HTTP-Referer".to_string(), OPENROUTER_REFERER.to_string()),
                    ("X-Title".to_string(), OPENROUTER_TITLE.to_string()),
                ],
            );
            p = p.with_reasoning_effort(thinking.to_reasoning_effort_string());
            if let Some(sid) = session_id {
                p = p.with_prompt_cache_key(sid);
            }
            Ok(Arc::new(p))
        }

        // ── Ollama ────────────────────────────────────────────────────────
        BackendPreset::Ollama => {
            let base = instance
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            Ok(Arc::new(OllamaProvider::new(base, model.to_string())))
        }
        BackendPreset::OllamaCom => {
            let base = instance
                .base_url
                .clone()
                .unwrap_or_else(|| "https://ollama.com".to_string());
            Ok(Arc::new(OllamaProvider::new(base, model.to_string())))
        }

        // ── Open WebUI ────────────────────────────────────────────────────
        BackendPreset::OpenWebUi => {
            let base = instance.base_url.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "No base URL for Open WebUI provider '{}'. Configure it first.",
                    instance.id
                )
            })?;
            match instance.api_type {
                ApiType::OllamaChatApi => {
                    let api_base = format!("{}/ollama", base.trim_end_matches('/'));
                    Ok(Arc::new(OllamaProvider::new(api_base, model.to_string())))
                }
                _ => {
                    let api_base = format!("{}/api", base.trim_end_matches('/'));
                    let api_key = instance.api_key.clone().unwrap_or_default();
                    Ok(Arc::new(OpenAiProvider::new(api_base, model, api_key)))
                }
            }
        }

        // ── Test ──────────────────────────────────────────────────────────
        BackendPreset::Test => Ok(Arc::new(TestProvider::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopilotApiRoute, ThinkingSupport, classify_copilot_route, thinking_support_for_instance,
    };
    use crate::llm::copilot::test_helpers;
    use crate::provider_instance::{ApiType, BackendPreset, ProviderInstance};
    use crate::thinking::{GeminiThinkingLevel, ThinkingLevel};

    #[test]
    fn copilot_route_uses_responses_for_codex_models() {
        assert_eq!(
            classify_copilot_route("gpt-5.3-codex"),
            CopilotApiRoute::OpenAiResponses
        );
    }

    #[test]
    fn copilot_route_uses_anthropic_for_claude_models() {
        assert_eq!(
            classify_copilot_route("claude-sonnet-4.5"),
            CopilotApiRoute::AnthropicMessages
        );
    }

    #[test]
    fn copilot_route_uses_chat_completions_for_gpt4o() {
        assert_eq!(
            classify_copilot_route("gpt-4o"),
            CopilotApiRoute::OpenAiChatCompletions
        );
    }

    #[test]
    fn shared_reasoning_effort_mapping_matches_responses_routes() {
        assert_eq!(ThinkingLevel::Off.to_reasoning_effort_string(), None);
        assert_eq!(
            ThinkingLevel::Minimal.to_reasoning_effort_string(),
            Some("minimal".to_string())
        );
        assert_eq!(
            ThinkingLevel::XHigh.to_reasoning_effort_string(),
            Some("xhigh".to_string())
        );
    }

    #[test]
    fn shared_gemini_mapping_preserves_provider_specific_clamp() {
        assert_eq!(ThinkingLevel::Off.to_gemini_thinking_level(), None);
        assert_eq!(
            ThinkingLevel::Medium.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::Medium)
        );
        assert_eq!(
            ThinkingLevel::XHigh.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::High)
        );
    }

    #[test]
    fn instance_thinking_support_for_copilot_depends_on_model_route() {
        let mut instance = ProviderInstance::new("copilot", BackendPreset::Copilot);
        instance.api_type = ApiType::OpenAiCompatible;

        assert_eq!(
            thinking_support_for_instance(&instance, "gpt-5.3-codex"),
            ThinkingSupport::Applied
        );
        assert!(matches!(
            thinking_support_for_instance(&instance, "gpt-4o"),
            ThinkingSupport::Ignored(_)
        ));
        assert!(matches!(
            thinking_support_for_instance(&instance, "claude-sonnet-4.5"),
            ThinkingSupport::Ignored(_)
        ));
    }

    // ── Cache-driven routing ─────────────────────────────────────────────────

    #[test]
    fn classify_copilot_route_uses_cached_vendor_for_anthropic() {
        // A model whose name gives no Anthropic hint, but the API says "Anthropic".
        test_helpers::insert_cache("__future_ai_model__", "Anthropic", Some(200_000));
        assert_eq!(
            classify_copilot_route("__future_ai_model__"),
            CopilotApiRoute::AnthropicMessages
        );
    }

    #[test]
    fn classify_copilot_route_falls_back_to_heuristic_for_openai_vendor() {
        // OpenAI vendor does not disambiguate Chat-Completions vs Responses;
        // name heuristic must still fire.  This model name has no prefix/substring
        // that would trigger the Responses path, so it falls to Chat-Completions.
        test_helpers::insert_cache("__openai_vendor_model__", "Azure OpenAI", None);
        assert_eq!(
            classify_copilot_route("__openai_vendor_model__"),
            CopilotApiRoute::OpenAiChatCompletions
        );
    }
}
