use std::sync::Arc;

use crate::{
    auth::AuthStore,
    config::TauConfig,
    llm::{
        LlmProvider,
        codex::{CodexProvider, DEFAULT_BASE_URL as CODEX_DEFAULT_BASE_URL},
        copilot::CopilotProvider,
        gemini::{
            DEFAULT_BASE_URL as GEMINI_DEFAULT_BASE_URL, GeminiProvider, GeminiThinkingLevel,
        },
        ollama::OllamaProvider,
        openai::OpenAiProvider,
        test_provider::TestProvider,
    },
    thinking::ThinkingLevel,
};

/// All supported back-end providers, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    Copilot,
    OpenAi,
    Codex,
    Gemini,
    Ollama,
    OpenWebUi,
    /// Hidden test provider — exercises the UI without a real API connection.
    /// Never appears in the provider selection menu.
    Test,
}

impl ProviderKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::OpenAi => "openai",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
            Self::OpenWebUi => "open-webui",
            Self::Test => "test",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Copilot => "GitHub Copilot",
            Self::OpenAi => "OpenAI API",
            Self::Codex => "OpenAI Codex (chatgpt.com)",
            Self::Gemini => "Google Gemini CLI (Cloud Code Assist)",
            Self::Ollama => "Ollama (local)",
            Self::OpenWebUi => "Open WebUI",
            Self::Test => "Test (UI exercise)",
        }
    }

    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::Copilot,
            Self::OpenAi,
            Self::Codex,
            Self::Gemini,
            Self::Ollama,
            Self::OpenWebUi,
            // Test is intentionally omitted — it is hidden from the menu.
        ]
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "copilot" | "github-copilot" => Some(Self::Copilot),
            "openai" => Some(Self::OpenAi),
            "codex" => Some(Self::Codex),
            "gemini" | "google-gemini" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            "open-webui" | "openwebui" => Some(Self::OpenWebUi),
            "test" => Some(Self::Test),
            _ => None,
        }
    }

    /// Sensible default model for this provider.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Copilot => "gpt-4o",
            Self::OpenAi => "gpt-4o",
            Self::Codex => "gpt-5.4",
            Self::Gemini => "gemini-2.5-pro",
            Self::Ollama => "llama3.1",
            Self::OpenWebUi => "llama3.1",
            Self::Test => "test",
        }
    }
}

/// Return the context-window size (in tokens) for a known model name.
///
/// Checks the Copilot model metadata cache first (populated by
/// `CopilotProvider::list_models()`), then falls back to a hard-coded table
/// for other providers and for Copilot models used before `list_models()` has
/// been called.
///
/// Returns `None` for unrecognised models.
pub fn context_window_for_model(model: &str) -> Option<usize> {
    // Primary: live metadata from the Copilot /models API.
    if let Some(cw) = CopilotProvider::cached_context_window(model) {
        return Some(cw);
    }

    // Secondary: live metadata fetched by the Ollama /api/show endpoint
    // (covers both Ollama and Open WebUI providers).
    if let Some(cw) = OllamaProvider::cached_context_window(model) {
        return Some(cw);
    }

    // Fallback: hard-coded table for all other providers and cold-start.
    // Normalise to lowercase for matching.
    let m = model.to_ascii_lowercase();
    // Check prefixes / substrings for common model families.
    if m.starts_with("o3-mini") {
        return Some(200_000);
    }
    if m.starts_with("o3") {
        return Some(200_000);
    }
    if m.starts_with("o1-mini") {
        return Some(128_000);
    }
    if m.starts_with("o1") {
        return Some(200_000);
    }
    if m.starts_with("gpt-4o") {
        return Some(128_000);
    }
    if m.starts_with("gpt-4-turbo") {
        return Some(128_000);
    }
    if m.starts_with("gpt-4") {
        return Some(8_192);
    }
    if m.starts_with("gpt-3.5-turbo") {
        return Some(16_385);
    }
    if m.starts_with("gpt-5") {
        return Some(200_000);
    }
    if m.contains("gemini") {
        return Some(1_000_000);
    }
    if m.contains("claude-3-5") || m.contains("claude-3.5") {
        return Some(200_000);
    }
    if m.contains("claude-3") {
        return Some(200_000);
    }
    if m.contains("claude-2") {
        return Some(100_000);
    }
    // Claude 4+ models have a 1M context window.
    // (e.g. claude-sonnet-4.5, claude-sonnet-4-6, claude-opus-4, …).
    if m.contains("claude-") {
        return Some(1_000_000);
    }
    if m.contains("llama3") {
        return Some(128_000);
    }
    if m.contains("llama2") {
        return Some(4_096);
    }
    if m.contains("mistral") {
        return Some(32_000);
    }
    if m.contains("gemma") {
        return Some(8_192);
    }
    None
}

/// Compute a scaled token budget using a square-root curve, capped at `window / 3`.
///
/// `f(w) = min(w/3, floor + scale * sqrt(w / 200_000))`
///
/// This gives good utilisation of large context windows while keeping small
/// models usable (the `w/3` cap kicks in below ~200k).
///
/// Intended uses:
/// - `max_tokens` (output budget):   floor = 8_000, scale = 8_000
/// - `reserve_tokens` (compaction):  floor = 16_000, scale = 16_000
pub fn scaled_token_budget(context_window: usize, floor: usize, scale: usize) -> usize {
    let ratio = context_window as f64 / 200_000.0_f64;
    let value = floor as f64 + scale as f64 * ratio.sqrt();
    let cap = context_window / 3;
    (value as usize).min(cap).max(1)
}

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

pub fn thinking_support_for(kind: &ProviderKind, model: &str) -> ThinkingSupport {
    match kind {
        ProviderKind::Copilot => match classify_copilot_route(model) {
            CopilotApiRoute::OpenAiResponses => ThinkingSupport::Applied,
            CopilotApiRoute::AnthropicMessages => {
                ThinkingSupport::Ignored("copilot anthropic route has no thinking mapping yet")
            }
            CopilotApiRoute::OpenAiChatCompletions => ThinkingSupport::Ignored(
                "copilot chat-completions route does not expose reasoning.effort",
            ),
        },
        ProviderKind::Codex => ThinkingSupport::Applied,
        ProviderKind::Gemini => ThinkingSupport::Applied,
        ProviderKind::OpenAi => {
            ThinkingSupport::Ignored("openai chat-completions provider does not map thinking yet")
        }
        ProviderKind::Ollama => {
            ThinkingSupport::Ignored("ollama provider does not support mapped thinking levels")
        }
        ProviderKind::OpenWebUi => {
            ThinkingSupport::Ignored("open-webui provider does not support mapped thinking levels")
        }
        ProviderKind::Test => ThinkingSupport::Ignored("test provider does not support thinking"),
    }
}

/// Build a boxed `LlmProvider` for `kind` with the given model name.
///
/// Returns an error if the required credentials or configuration are missing.
pub fn build_provider(
    kind: &ProviderKind,
    model: &str,
    thinking: ThinkingLevel,
    config: &TauConfig,
) -> anyhow::Result<Arc<dyn LlmProvider + Send + Sync>> {
    match kind {
        ProviderKind::Copilot => {
            let store = AuthStore::load_default()?;
            let creds = store.get_copilot().ok_or_else(|| {
                anyhow::anyhow!("Not authenticated for copilot. Run /login copilot.")
            })?;
            log::debug!(
                "provider route selected: provider=copilot model={} base_url={}",
                model,
                creds.base_url.as_deref().unwrap_or("<from-token>")
            );
            let p = CopilotProvider::new(
                &creds.access_token,
                model,
                creds.base_url.as_deref(),
                thinking.to_reasoning_effort().map(ToString::to_string),
            );
            Ok(Arc::new(p))
        }
        ProviderKind::OpenAi => {
            let base_url = config
                .openai
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

            let api_key = config.openai.api_key.clone().ok_or_else(|| {
                anyhow::anyhow!("Missing API key. Configure [openai].api_key in config.toml.")
            })?;

            let p = OpenAiProvider::new(base_url, model, api_key);
            Ok(Arc::new(p))
        }
        ProviderKind::Codex => {
            let store = AuthStore::load_default()?;
            let creds = store
                .get_codex()
                .ok_or_else(|| anyhow::anyhow!("Not authenticated for codex. Run /login codex."))?;
            let base_url = config
                .codex
                .base_url
                .clone()
                .unwrap_or_else(|| CODEX_DEFAULT_BASE_URL.to_string());
            let p = CodexProvider::new(base_url, model, creds.access_token, creds.account_id)
                .with_reasoning_effort(thinking.to_reasoning_effort().map(ToString::to_string));
            Ok(Arc::new(p))
        }
        ProviderKind::Gemini => {
            let store = AuthStore::load_default()?;
            let creds = store.get_gemini().ok_or_else(|| {
                anyhow::anyhow!("Not authenticated for gemini. Run /login gemini.")
            })?;
            let base_url = config
                .gemini
                .base_url
                .clone()
                .unwrap_or_else(|| GEMINI_DEFAULT_BASE_URL.to_string());
            let mapped_thinking = match thinking {
                ThinkingLevel::Off => None,
                ThinkingLevel::Minimal => Some(GeminiThinkingLevel::Minimal),
                ThinkingLevel::Low => Some(GeminiThinkingLevel::Low),
                ThinkingLevel::Medium => Some(GeminiThinkingLevel::Medium),
                ThinkingLevel::High | ThinkingLevel::XHigh => Some(GeminiThinkingLevel::High),
            };
            let p = GeminiProvider::new(base_url, model, creds.access_token, creds.project_id)
                .with_thinking_level(mapped_thinking);
            Ok(Arc::new(p))
        }
        ProviderKind::Ollama => {
            let base = config
                .ollama
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            Ok(Arc::new(OllamaProvider::new(base, model.to_string())))
        }
        ProviderKind::OpenWebUi => {
            let base = config.open_webui.base_url.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "No Open WebUI URL configured. Run /provider open-webui to set it up."
                )
            })?;
            // Use Open WebUI's OpenAI-compatible API rather than the Ollama
            // proxy path (/ollama/api/chat).  The proxy intercepts tool-call
            // requests and runs its own internal agentic loop, returning only
            // a bare done-chunk with empty content instead of streaming the
            // model's tokens back to the client.  The /api endpoint streams
            // correctly and supports tools via the standard OpenAI format.
            let api_base = format!("{}/api", base.trim_end_matches('/'));
            let api_key = config
                .open_webui
                .api_key
                .clone()
                .unwrap_or_default();
            Ok(Arc::new(OpenAiProvider::new(api_base, model, api_key)))
        }
        ProviderKind::Test => Ok(Arc::new(TestProvider::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopilotApiRoute, ThinkingSupport, classify_copilot_route, context_window_for_model,
        scaled_token_budget, thinking_support_for,
    };
    use crate::llm::copilot::test_helpers;
    use crate::provider::ProviderKind;

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
    fn thinking_support_applies_for_copilot_responses() {
        assert_eq!(
            thinking_support_for(&ProviderKind::Copilot, "gpt-5.3-codex"),
            ThinkingSupport::Applied
        );
    }

    #[test]
    fn thinking_support_ignored_for_copilot_chat() {
        assert!(matches!(
            thinking_support_for(&ProviderKind::Copilot, "gpt-4o"),
            ThinkingSupport::Ignored(_)
        ));
    }

    #[test]
    fn thinking_support_applies_for_gemini() {
        assert_eq!(
            thinking_support_for(&ProviderKind::Gemini, "gemini-2.5-pro"),
            ThinkingSupport::Applied
        );
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

    // ── Cache-driven context window ──────────────────────────────────────────

    #[test]
    fn context_window_prefers_cache_over_hard_coded_table() {
        // Seed cache with a non-standard size to prove it wins over the table.
        test_helpers::insert_cache("__gpt_4o_cache_test__", "Azure OpenAI", Some(999_999));
        assert_eq!(
            context_window_for_model("__gpt_4o_cache_test__"),
            Some(999_999)
        );
    }

    #[test]
    fn context_window_falls_back_to_table_when_cache_miss() {
        // gpt-4o is not seeded with this exact key — falls back to hard-coded table.
        assert_eq!(context_window_for_model("gpt-4o"), Some(128_000));
    }

    #[test]
    fn context_window_falls_back_to_table_when_cache_has_no_limit() {
        test_helpers::insert_cache("__no_limit_model__", "OpenAI", None);
        // Cache entry exists but carries no context-window value; table has no
        // match either → None.
        assert_eq!(context_window_for_model("__no_limit_model__"), None);
    }

    #[test]
    fn context_window_claude4_returns_1m() {
        assert_eq!(
            context_window_for_model("claude-sonnet-4-6"),
            Some(1_000_000)
        );
        assert_eq!(
            context_window_for_model("claude-sonnet-4.5"),
            Some(1_000_000)
        );
        assert_eq!(context_window_for_model("claude-opus-4"), Some(1_000_000));
    }

    #[test]
    fn context_window_claude3_unchanged() {
        assert_eq!(context_window_for_model("claude-3-5-sonnet"), Some(200_000));
        assert_eq!(context_window_for_model("claude-3-opus"), Some(200_000));
    }

    // ── scaled_token_budget ──────────────────────────────────────────────────

    #[test]
    fn scaled_token_budget_at_reference_window() {
        // At 200k: floor + scale * sqrt(1.0) = floor + scale
        assert_eq!(scaled_token_budget(200_000, 8_000, 8_000), 16_000);
        assert_eq!(scaled_token_budget(200_000, 16_000, 16_000), 32_000);
    }

    #[test]
    fn scaled_token_budget_at_1m() {
        // sqrt(1_000_000 / 200_000) = sqrt(5) ≈ 2.236
        // max_tokens: 8000 + 8000 * 2.236 = ~25_889, cap = 333_333 → ~25_889
        let mt = scaled_token_budget(1_000_000, 8_000, 8_000);
        assert!(mt > 24_000 && mt < 27_000, "max_tokens at 1M: {mt}");
        // reserve: 16000 + 16000 * 2.236 = ~51_778, cap = 333_333 → ~51_778
        let rv = scaled_token_budget(1_000_000, 16_000, 16_000);
        assert!(rv > 50_000 && rv < 53_000, "reserve at 1M: {rv}");
    }

    #[test]
    fn scaled_token_budget_cap_applies_on_small_window() {
        // 8k window: formula gives floor=8000 which exceeds 8192/3=2730 → cap wins
        let mt = scaled_token_budget(8_192, 8_000, 8_000);
        assert_eq!(mt, 8_192 / 3);
    }

    #[test]
    fn scaled_token_budget_at_128k() {
        // sqrt(128_000 / 200_000) = sqrt(0.64) = 0.8
        // max_tokens: 8000 + 8000 * 0.8 = 14_400, cap = 42_666 → 14_400
        let mt = scaled_token_budget(128_000, 8_000, 8_000);
        assert!(mt > 13_500 && mt < 15_000, "max_tokens at 128k: {mt}");
    }
}
