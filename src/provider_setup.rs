//! Provider setup helpers: resolution, config persistence, thinking, and the
//! unavailable-provider sentinel used when no provider is available.

use crate::app::App;
use crate::config::XiConfig;
use crate::llm::{LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture};
use crate::provider::{ThinkingSupport, thinking_support_for_instance};
use crate::provider_instance::{BackendPreset, ProviderInstance};
use crate::thinking::ThinkingLevel;

// ── Unavailable provider sentinel ─────────────────────────────────────────

pub(crate) struct UnavailableProvider {
    pub(crate) message: String,
}

impl LlmProvider for UnavailableProvider {
    fn stream_chat(
        &self,
        _messages: Vec<Message>,
        _context: crate::llm::LlmRequestContext,
    ) -> LlmStream {
        let msg = self.message.clone();
        Box::pin(async_stream::stream! {
            yield LlmEvent::Error(crate::llm::ProviderError::other("unavailable", msg));
        })
    }

    fn stream_chat_with_tools(
        &self,
        _messages: Vec<Message>,
        _tools: Vec<crate::llm::ToolDefinition>,
        _context: crate::llm::LlmRequestContext,
    ) -> LlmStream {
        self.stream_chat(vec![], crate::llm::LlmRequestContext::default())
    }

    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec![]) })
    }
}

// ── Provider resolution ───────────────────────────────────────────────────

/// Resolve the default active [`ProviderInstance`] from config.
///
/// Resolution order:
/// 1. `config.provider` matched against effective providers
/// 2. First effective provider
/// 3. Synthetic copilot default
pub(crate) fn resolve_default_provider_instance(config: &XiConfig) -> ProviderInstance {
    let effective = config.resolve_effective_providers();

    if let Some(ref id) = config.provider
        && let Some(inst) = effective.iter().find(|p| p.id == *id)
    {
        return inst.clone();
    }

    effective
        .into_iter()
        .next()
        .unwrap_or_else(|| ProviderInstance::new("copilot", BackendPreset::Copilot))
}

pub(crate) fn resolve_provider_instance(
    cli_override: Option<&str>,
    config: &XiConfig,
) -> Result<ProviderInstance, String> {
    if let Some(id) = cli_override {
        if id == "test" {
            return Ok(ProviderInstance::new("test", BackendPreset::Test));
        }
        if let Some(inst) = config.resolve_provider(id) {
            return Ok(inst);
        }

        let effective = config.resolve_effective_providers();
        let mut allowed: Vec<&str> = effective
            .iter()
            .map(|instance| instance.id.as_str())
            .collect();
        allowed.push("test");
        return Err(format!(
            "unknown provider '{id}'. Expected one of: {}",
            allowed.join(", ")
        ));
    }

    Ok(resolve_default_provider_instance(config))
}

/// Resolve the effective model for a provider instance.
pub(crate) fn resolve_model_for_instance(
    cli_override: Option<&str>,
    instance: &ProviderInstance,
) -> String {
    cli_override
        .map(ToString::to_string)
        .or_else(|| instance.model.clone())
        .unwrap_or_else(|| instance.backend_preset.default_model().to_string())
}

pub(crate) fn with_resolved_model(
    cli_override: Option<&str>,
    instance: &ProviderInstance,
) -> ProviderInstance {
    let mut resolved = instance.clone();
    resolved.model = Some(resolve_model_for_instance(cli_override, instance));
    resolved
}

// ── Config persistence ────────────────────────────────────────────────────

/// Instance-based variant of `persist_provider_model_selection`.
///
/// Updates the named instance's model in the providers list and persists config.
pub(crate) fn persist_provider_model_selection_v2(config: &mut XiConfig, app: &mut App) {
    let instance = &app.provider.current_instance;
    let model = &app.provider.current_model;
    let thinking = app.provider.current_thinking;
    // Never persist the test provider.
    if instance.backend_preset == BackendPreset::Test {
        return;
    }
    app.provider.provider_selected = true;
    config.provider = Some(instance.id.clone());
    config.thinking = Some(thinking.as_str().to_string());
    config
        .thinking_by_model
        .insert(model.to_string(), thinking.as_str().to_string());

    // Update the model on the stored instance.
    if let Some(stored) = config.find_provider_mut(&instance.id) {
        stored.model = Some(model.to_string());
    }

    if let Err(e) = config.save() {
        log::debug!("failed to persist provider/model config: {}", e);
        app.push_notice(Message::assistant(format!(
            "[failed to persist config.toml: {e}]"
        )));
    }
}

// ── Thinking helpers ──────────────────────────────────────────────────────

pub(crate) fn resolve_thinking_level_for_model(config: &XiConfig, model: &str) -> ThinkingLevel {
    config
        .thinking_by_model
        .get(model)
        .and_then(|raw| ThinkingLevel::parse(raw))
        .or_else(|| config.thinking.as_deref().and_then(ThinkingLevel::parse))
        .unwrap_or(ThinkingLevel::Off)
}

pub(crate) fn maybe_warn_thinking_unsupported(app: &mut App) {
    let instance = &app.provider.current_instance;
    let model = &app.provider.current_model;
    let thinking = app.provider.current_thinking;
    // Always keep app.provider.thinking_supported in sync regardless of the level.
    app.provider.thinking_supported =
        thinking_support_for_instance(instance, model) == ThinkingSupport::Applied;

    if thinking == ThinkingLevel::Off {
        return;
    }
    if let ThinkingSupport::Ignored(reason) = thinking_support_for_instance(instance, model) {
        log::debug!(
            "thinking '{}' ignored for provider={} model={}: {}",
            thinking.as_str(),
            instance.id,
            model,
            reason
        );
    }
}
