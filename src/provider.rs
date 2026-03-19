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
}

impl ProviderKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::OpenAi => "openai",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Copilot => "GitHub Copilot",
            Self::OpenAi => "OpenAI API",
            Self::Codex => "OpenAI Codex (chatgpt.com)",
            Self::Gemini => "Google Gemini CLI (Cloud Code Assist)",
            Self::Ollama => "Ollama (local)",
        }
    }

    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::Copilot,
            Self::OpenAi,
            Self::Codex,
            Self::Gemini,
            Self::Ollama,
        ]
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "copilot" | "github-copilot" => Some(Self::Copilot),
            "openai" => Some(Self::OpenAi),
            "codex" => Some(Self::Codex),
            "gemini" | "google-gemini" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
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
        }
    }
}

/// Return the context-window size (in tokens) for a known model name.
/// Returns `None` for unrecognised models.
pub fn context_window_for_model(model: &str) -> Option<usize> {
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
/// This mirrors pi-mono's model metadata behavior:
/// - Claude models -> Anthropic Messages API.
/// - Codex and GPT-5 family models -> OpenAI Responses API.
/// - Everything else -> OpenAI Chat Completions API.
fn classify_copilot_route(model: &str) -> CopilotApiRoute {
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
    }
}

#[cfg(test)]
mod tests {
    use super::{CopilotApiRoute, ThinkingSupport, classify_copilot_route, thinking_support_for};
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
}
