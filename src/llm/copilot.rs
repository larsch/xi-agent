use super::anthropic::AnthropicProvider;
use super::codex::CodexProvider;
use super::openai::OpenAiProvider;
use super::{LlmProvider, LlmStream, Message, ModelListFuture, ToolDefinition};

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
    /// An `OpenAiProvider` pointed at the Copilot base URL, used exclusively
    /// for `list_models()` calls.
    models_provider: OpenAiProvider,
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

        let m = model.to_ascii_lowercase();
        let inner = if m.starts_with("claude") {
            log::debug!(
                "copilot transport resolved: api=anthropic-messages base_url={} endpoint=/v1/messages",
                resolved_base_url
            );
            CopilotInner::Anthropic(AnthropicProvider::new_with_headers(
                resolved_base_url.clone(),
                model,
                access_token,
                true, // bearer_auth
                copilot_extra_headers(),
            ))
        } else if m.contains("codex") || m.starts_with("gpt-5") {
            let responses_url = format!("{}/v1/responses", resolved_base_url.trim_end_matches('/'));
            log::debug!(
                "copilot transport resolved: api=openai-responses base_url={} endpoint={}",
                resolved_base_url,
                responses_url
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
                "copilot transport resolved: api=openai-chat-completions base_url={} endpoint=/chat/completions",
                resolved_base_url
            );
            CopilotInner::OpenAi(OpenAiProvider::new_with_headers(
                resolved_base_url.clone(),
                model,
                access_token,
                copilot_extra_headers(),
            ))
        };

        // The models provider always uses the OpenAI `/models` endpoint,
        // regardless of the current model's API route.
        let models_provider = OpenAiProvider::new_with_headers(
            resolved_base_url,
            model,
            access_token,
            copilot_extra_headers(),
        );

        Self {
            inner,
            models_provider,
        }
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

    /// Always fetches the full model list from the Copilot `/models` endpoint,
    /// regardless of which model is currently active.
    fn list_models(&self) -> ModelListFuture {
        self.models_provider.list_models()
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
