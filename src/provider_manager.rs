//! Provider state management.
//!
//! `ProviderManager` groups the fields that track which provider/model/thinking
//! configuration is currently active, the snapshot of configured instances used
//! for completions and selection menus, and the transient setup-flow state.
//!
//! Methods that only read/write provider state live here; methods that also need
//! `textarea`, `selection`, or session state remain on `App` as thin wrappers.
//!
//! The setup-flow *methods* remain on `App` because they need access to the
//! textarea and selection widgets.  `ProviderManager` is a pure data holder.

use crate::provider_instance::ProviderInstance;
use crate::provider_instance::{ApiType, AuthMode, BackendPreset};
use crate::thinking::ThinkingLevel;

/// Tracks which step of the provider setup input flow is currently active.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum ProviderSetupStep {
    #[default]
    Idle,
    /// User is typing an endpoint URL (covers Ollama, Open WebUI, generic endpoints).
    Endpoint,
    /// User is typing an API key / token. Holds the URL captured in a prior `Endpoint` step.
    ApiKey { pending_url: Option<String> },
    /// User is typing a provider instance name.
    Name,
}

#[derive(Debug, Clone)]
pub struct PendingProviderSetup {
    pub(crate) original_id: String,
    pub(crate) id: String,
    pub(crate) backend_preset: Option<BackendPreset>,
    pub(crate) api_type: Option<ApiType>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) editing_existing: bool,
}

#[derive(Debug, Clone)]
pub struct PendingProviderRemoval {
    pub(crate) id: String,
}

impl PendingProviderSetup {
    /// Create a new pending setup with the given id.  Used in tests.
    #[allow(dead_code)]
    pub(crate) fn new(id: String) -> Self {
        Self {
            original_id: id.clone(),
            id,
            backend_preset: None,
            api_type: None,
            base_url: None,
            api_key: None,
            model: None,
            editing_existing: false,
        }
    }

    pub(crate) fn from_instance(instance: &ProviderInstance) -> Self {
        Self {
            original_id: instance.id.clone(),
            id: instance.id.clone(),
            backend_preset: Some(instance.backend_preset.clone()),
            api_type: Some(instance.api_type.clone()),
            base_url: instance.base_url.clone(),
            api_key: instance.api_key.clone(),
            model: instance.model.clone(),
            editing_existing: true,
        }
    }
}

/// All provider-related state owned by the application.
pub(crate) struct ProviderManager {
    /// Snapshot of configured provider instances (for completions / selection).
    /// Updated whenever the provider list changes.
    pub instances: Vec<ProviderInstance>,

    /// The currently active provider instance.
    pub current_instance: ProviderInstance,

    /// Currently active model name.
    pub current_model: String,

    /// Currently active thinking / reasoning level.
    pub current_thinking: ThinkingLevel,

    /// Whether the current provider+model combination supports thinking.
    pub thinking_supported: bool,

    /// Whether the user has explicitly selected a provider (via `/provider` or
    /// CLI flag).  False on fresh install until the user picks one.
    pub provider_selected: bool,

    /// Which step of the provider setup input flow is currently active.
    pub setup_step: ProviderSetupStep,

    /// Pending provider instance being configured through the add-provider flow.
    pub pending_setup: Option<PendingProviderSetup>,

    /// Pending custom provider instance being confirmed for removal.
    pub pending_removal: Option<PendingProviderRemoval>,
}

impl ProviderManager {
    pub(crate) fn new(
        initial_instance: ProviderInstance,
        initial_model: String,
        initial_thinking: ThinkingLevel,
    ) -> Self {
        Self {
            instances: Vec::new(),
            current_instance: initial_instance,
            current_model: initial_model,
            current_thinking: initial_thinking,
            thinking_supported: false,
            provider_selected: false,
            setup_step: ProviderSetupStep::Idle,
            pending_setup: None,
            pending_removal: None,
        }
    }

    // ── Pure query helpers ────────────────────────────────────────────────────

    pub fn pending_setup_is_edit(&self) -> bool {
        self.pending_setup
            .as_ref()
            .map(|setup| setup.editing_existing)
            .unwrap_or(false)
    }

    pub fn pending_original_id(&self) -> Option<&str> {
        self.pending_setup
            .as_ref()
            .and_then(|setup| setup.editing_existing.then_some(setup.original_id.as_str()))
    }

    pub fn pending_instance(&self) -> Option<ProviderInstance> {
        let setup = self.pending_setup.as_ref()?;
        let backend_preset = setup.backend_preset.clone()?;
        let api_type = setup
            .api_type
            .clone()
            .unwrap_or_else(|| backend_preset.def().default_api.clone());
        let id = if setup.id.is_empty() {
            self.suggested_id()?
        } else {
            setup.id.clone()
        };
        let mut instance = ProviderInstance::new(id, backend_preset);
        instance.api_type = api_type;
        instance.base_url = setup.base_url.clone();
        instance.api_key = setup.api_key.clone();
        instance.model = setup.model.clone();
        Some(instance)
    }

    pub fn finish_setup(&mut self) -> Option<ProviderInstance> {
        let instance = self.pending_instance()?;
        self.pending_setup = None;
        self.pending_removal = None;
        Some(instance)
    }

    pub fn clear_setup(&mut self) {
        self.pending_setup = None;
        self.pending_removal = None;
    }

    pub fn clear_removal(&mut self) {
        self.pending_removal = None;
    }

    // Used by tests in app.rs via set_pending_provider_backend_preset.
    #[allow(dead_code)]
    pub fn set_pending_backend_preset(&mut self, backend_preset: BackendPreset) {
        if let Some(setup) = self.pending_setup.as_mut() {
            setup.backend_preset = Some(backend_preset);
            setup.api_type = None;
        }
    }

    pub fn set_pending_api_type(&mut self, api_type: ApiType) {
        if let Some(setup) = self.pending_setup.as_mut() {
            setup.api_type = Some(api_type);
        }
    }

    // ── ID generation helpers ─────────────────────────────────────────────────

    pub fn normalize_id(raw: &str) -> Option<String> {
        let mut out = String::new();
        let mut prev_sep = false;
        for ch in raw.trim().chars() {
            let mapped = match ch {
                'a'..='z' | '0'..='9' | '.' => Some(ch),
                'A'..='Z' => Some(ch.to_ascii_lowercase()),
                _ => None,
            };
            if let Some(c) = mapped {
                out.push(c);
                prev_sep = false;
            } else if !out.is_empty() && !prev_sep {
                out.push('-');
                prev_sep = true;
            }
        }
        while out.ends_with(['-', '.']) {
            out.pop();
        }
        if out.is_empty() { None } else { Some(out) }
    }

    pub fn type_suffix(backend_preset: &BackendPreset) -> &'static str {
        match backend_preset {
            BackendPreset::Ollama => "ollama",
            BackendPreset::OpenWebUi => "open-webui",
            BackendPreset::OpenAiCompatible => "openai-compatible",
            BackendPreset::Copilot => "copilot",
            BackendPreset::OpenAi => "openai",
            BackendPreset::OpenRouter => "openrouter",
            BackendPreset::Codex => "codex",
            BackendPreset::Gemini => "gemini",
            BackendPreset::OllamaCom => "ollama-com",
            BackendPreset::Test => "test",
        }
    }

    pub fn suggested_id(&self) -> Option<String> {
        let setup = self.pending_setup.as_ref()?;
        if setup.editing_existing {
            return Some(setup.id.clone());
        }
        let backend_preset = setup.backend_preset.as_ref()?;
        let host = setup
            .base_url
            .as_deref()
            .and_then(|base| reqwest::Url::parse(base).ok())
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| backend_preset.id().to_string());
        let raw = match backend_preset {
            BackendPreset::Ollama => format!("ollama-{host}"),
            _ => format!("{}-{}", host, Self::type_suffix(backend_preset)),
        };
        Self::normalize_id(&raw)
    }

    // ── Submit helpers (called from App with pre-extracted input strings) ─────

    /// Validate and store the provider name input.
    ///
    /// `raw` is the raw text from the textarea; `existing_instances` is used to
    /// reject duplicates.  Returns the normalized id on success, `None` on
    /// invalid input or duplicate.
    pub fn submit_name_input(
        &mut self,
        raw: &str,
        existing_instances: &[ProviderInstance],
    ) -> Option<String> {
        let id = Self::normalize_id(raw)?;
        let setup = self.pending_setup.as_mut()?;
        if existing_instances
            .iter()
            .any(|p| p.id == id && (!setup.editing_existing || p.id != setup.original_id))
        {
            return None;
        }
        self.setup_step = ProviderSetupStep::Idle;
        setup.id = id.clone();
        Some(id)
    }

    /// Validate and store the provider base URL.
    ///
    /// `raw` is the raw text from the textarea.  Returns the normalized URL on
    /// success, `None` when there is no pending instance or the URL is invalid.
    pub fn submit_base_url(&mut self, raw: &str) -> Option<String> {
        let instance = self.pending_instance()?;
        let norm = instance.backend_preset.def().url_normalization.as_ref()?;
        let url = norm.normalize(raw)?;
        self.setup_step = ProviderSetupStep::Idle;
        if let Some(setup) = self.pending_setup.as_mut() {
            setup.base_url = Some(url.clone());
        }
        Some(url)
    }

    /// Validate and store the provider API key.
    ///
    /// `raw` is the raw text from the textarea.  Returns the stored token on
    /// success (may be the pre-existing token when editing and left blank).
    pub fn submit_api_key(&mut self, raw: &str) -> Option<String> {
        let token = raw.trim().to_string();
        let existing_token = self
            .pending_setup
            .as_ref()
            .and_then(|setup| setup.api_key.clone());
        let keep_existing = token.is_empty() && self.pending_setup_is_edit();
        if token.is_empty() && !keep_existing {
            return None;
        }
        self.setup_step = ProviderSetupStep::Idle;
        if let Some(setup) = self.pending_setup.as_mut()
            && !keep_existing
        {
            setup.api_key = Some(token.clone());
        }
        if keep_existing {
            existing_token
        } else {
            Some(token)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupInputKind {
    Name,
    BaseUrl,
    ApiKey,
}

impl SetupInputKind {
    pub fn prompt_label(self, instance: Option<&ProviderInstance>) -> String {
        match self {
            Self::Name => "provider instance name: ".to_string(),
            Self::BaseUrl => match instance {
                Some(p) => p
                    .backend_preset
                    .def()
                    .url_normalization
                    .as_ref()
                    .map(|n| n.endpoint_label.to_string())
                    .unwrap_or_else(|| "URL: ".to_string()),
                None => "URL: ".to_string(),
            },
            Self::ApiKey => match instance {
                Some(p) => match p.backend_preset.def().auth_mode {
                    AuthMode::ApiKey => match p.backend_preset {
                        BackendPreset::OpenRouter => "OpenRouter API key: ".to_string(),
                        BackendPreset::OpenWebUi => "open-webui token: ".to_string(),
                        _ if p.base_url.is_some() => {
                            "API key (leave empty to keep current): ".to_string()
                        }
                        _ => "API key: ".to_string(),
                    },
                    _ => "token: ".to_string(),
                },
                None => "API key: ".to_string(),
            },
        }
    }

    pub fn prompt_hint(self, instance: Option<&ProviderInstance>) -> String {
        match self {
            Self::Name => "work-webui   Enter confirm   Esc cancel".to_string(),
            Self::BaseUrl => match instance {
                Some(p) => p
                    .backend_preset
                    .def()
                    .url_normalization
                    .as_ref()
                    .map(|n| n.endpoint_hint.to_string())
                    .unwrap_or_else(|| {
                        "https://example.com   Enter confirm   Esc cancel".to_string()
                    }),
                None => "https://example.com   Enter confirm   Esc cancel".to_string(),
            },
            Self::ApiKey => match instance {
                Some(p) if p.api_key.is_some() => "Enter keep current   Esc cancel".to_string(),
                Some(p) => match p.backend_preset {
                    BackendPreset::OpenRouter => "sk-or-…   Enter confirm   Esc cancel".to_string(),
                    BackendPreset::OpenWebUi => "sk-…   Enter confirm   Esc cancel".to_string(),
                    BackendPreset::OpenAiCompatible | BackendPreset::OpenAi => {
                        "sk-…   Enter confirm   Esc cancel".to_string()
                    }
                    _ => "token   Enter confirm   Esc cancel".to_string(),
                },
                None => "token   Enter confirm   Esc cancel".to_string(),
            },
        }
    }
}

/// Format a provider error into a user-friendly English sentence.
pub(crate) fn format_provider_error_for_display(
    provider_label: &str,
    err: &crate::llm::ProviderError,
) -> String {
    use crate::llm::ProviderErrorKind;

    let subject = if provider_label.trim().is_empty() {
        "The provider"
    } else {
        provider_label
    };

    let summary = match (err.kind, err.status_code) {
        (ProviderErrorKind::Unauthorized, Some(code)) => {
            format!("{subject} rejected the request because authentication expired ({code}).")
        }
        (ProviderErrorKind::Unauthorized, None) => {
            format!("{subject} rejected the request because authentication expired.")
        }
        (ProviderErrorKind::Forbidden, Some(code)) => {
            format!("{subject} rejected the request because access was denied ({code}).")
        }
        (ProviderErrorKind::Forbidden, None) => {
            format!("{subject} rejected the request because access was denied.")
        }
        (ProviderErrorKind::RateLimited, Some(code)) => {
            format!("{subject} is rate limiting requests ({code}).")
        }
        (ProviderErrorKind::RateLimited, None) => {
            format!("{subject} is rate limiting requests.")
        }
        (ProviderErrorKind::ServerError, Some(524)) => {
            format!("{subject} timed out on the backend (524).")
        }
        (ProviderErrorKind::ServerError, Some(code)) => {
            format!("{subject} reported a backend error ({code}).")
        }
        (ProviderErrorKind::ServerError, None) => {
            format!("{subject} reported a backend error.")
        }
        (ProviderErrorKind::Network, _) => {
            format!("Could not reach {subject}.")
        }
        (ProviderErrorKind::Other, Some(code)) => {
            format!("{subject} could not process the request ({code}).")
        }
        (ProviderErrorKind::Other, None) => {
            format!("{subject} could not process the request.")
        }
    };

    let message = err.message.trim();
    if message.is_empty() {
        summary
    } else {
        format!("{summary}\nProvider message: {message}")
    }
}

/// Return the display label for the currently active provider.
pub(crate) fn active_provider_display_name(
    current_provider: &str,
    provider_instances: &[ProviderInstance],
) -> String {
    provider_instances
        .iter()
        .find(|instance| instance.id == current_provider)
        .map(|instance| instance.backend_preset.label().to_string())
        .unwrap_or_else(|| current_provider.to_string())
}
