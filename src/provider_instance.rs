// Items in this module form the public provider-instance API.

/// API protocol/transport types that xi-agent knows how to speak.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiType {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    AnthropicCompatible,
    GeminiNative,
    OllamaChatApi,
    /// Internal only — used by the test provider. Never shown to users.
    Test,
}

impl ApiType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicCompatible => "Anthropic-compatible",
            Self::GeminiNative => "Gemini native",
            Self::OllamaChatApi => "Ollama chat API",
            Self::Test => "Test",
        }
    }
}

/// Recognisable software / cloud services xi-agent supports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendPreset {
    /// GitHub Copilot (cloud, managed routing)
    Copilot,
    /// OpenAI API (cloud)
    #[serde(rename = "openai")]
    OpenAi,
    /// OpenRouter API (cloud)
    OpenRouter,
    /// OpenAI Codex / chatgpt.com (cloud)
    Codex,
    /// Google Gemini CLI / Cloud Code Assist (cloud)
    Gemini,
    /// Self-hosted Ollama server
    Ollama,
    /// ollama.com cloud service
    #[serde(rename = "ollama-com")]
    OllamaCom,
    /// Open WebUI instance (self-hosted)
    #[serde(rename = "open-webui")]
    OpenWebUi,
    /// Generic OpenAI-compatible endpoint
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Internal test provider — never shown to users.
    Test,
}

/// Whether a preset represents a built-in hosted provider or a user-supplied service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendClass {
    BuiltInHosted,
    UserSuppliedService,
    Internal,
}

/// How the user authenticates a provider instance for a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    OAuthLogin,
    ApiKey,
    None,
}

/// Whether the endpoint is predetermined or supplied by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointBehavior {
    Predetermined,
    UserSupplied,
    Internal,
}

/// URL normalization parameters for a backend preset that accepts a user-supplied URL.
pub struct UrlNormalization {
    /// Default scheme to prepend when none is present (e.g. `"http"` or `"https"`).
    pub default_scheme: &'static str,
    /// Default port to apply when none is present (e.g. `Some(11434)` for Ollama).
    pub default_port: Option<u16>,
    /// Example URL hint shown to the user in the textarea.
    pub endpoint_hint: &'static str,
    /// Input label shown next to the textarea (e.g. `"ollama URL: "`).
    pub endpoint_label: &'static str,
}

impl UrlNormalization {
    /// Normalize a raw user-entered URL string using this preset's parameters.
    ///
    /// Returns `None` for empty/blank input or URLs that are still invalid after
    /// normalization.
    pub fn normalize(&self, raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("{}://{}", self.default_scheme, trimmed)
        };
        let mut url = reqwest::Url::parse(&with_scheme).ok()?;
        match url.scheme() {
            "http" | "https" => {}
            _ => return None,
        }
        url.host_str()?;
        if let Some(port) = self.default_port.filter(|_| url.port().is_none()) {
            url.set_port(Some(port)).ok()?;
        }
        Some(url.to_string().trim_end_matches('/').to_string())
    }
}

/// Metadata xi-agent keeps about a backend preset.
pub struct BackendPresetDef {
    /// Machine-readable id (matches `BackendPreset` serialisation).
    pub id: &'static str,
    /// Human-readable display label.
    pub label: &'static str,
    /// Which class of backend this preset belongs to.
    pub backend_class: BackendClass,
    /// API types that this service supports, in preference order.
    pub allowed_apis: &'static [ApiType],
    /// The recommended / default API type.
    pub default_api: ApiType,
    /// Whether the user should be allowed to choose the API type.
    /// `false` means xi-agent picks internally (e.g. Copilot).
    pub user_selects_api: bool,
    /// Whether the endpoint is predetermined, user-supplied, or overrideable.
    pub endpoint_behavior: EndpointBehavior,
    /// Which authentication mode this preset requires.
    pub auth_mode: AuthMode,
    /// URL normalization parameters for presets that accept a user-supplied URL.
    /// `None` for presets with predetermined or internal endpoints.
    pub url_normalization: Option<UrlNormalization>,
}

/// Static catalog of all supported backend presets.
pub const BACKEND_PRESET_CATALOG: &[BackendPresetDef] = &[
    BackendPresetDef {
        id: "copilot",
        label: "GitHub Copilot",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[
            ApiType::OpenAiCompatible,
            ApiType::OpenAiResponses,
            ApiType::AnthropicCompatible,
        ],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::OAuthLogin,
        url_normalization: None,
    },
    BackendPresetDef {
        id: "openai",
        label: "OpenAI API",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[ApiType::OpenAiCompatible],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::ApiKey,
        url_normalization: None,
    },
    BackendPresetDef {
        id: "openrouter",
        label: "OpenRouter",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[ApiType::OpenAiCompatible],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::ApiKey,
        url_normalization: Some(UrlNormalization {
            default_scheme: "https",
            default_port: None,
            endpoint_label: "URL: ",
            endpoint_hint: "https://openrouter.ai/api/v1   Enter confirm   Esc cancel",
        }),
    },
    BackendPresetDef {
        id: "codex",
        label: "OpenAI Codex (chatgpt.com)",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[ApiType::OpenAiResponses],
        default_api: ApiType::OpenAiResponses,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::OAuthLogin,
        url_normalization: None,
    },
    BackendPresetDef {
        id: "gemini",
        label: "Google Gemini CLI (Cloud Code Assist)",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[ApiType::GeminiNative],
        default_api: ApiType::GeminiNative,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::OAuthLogin,
        url_normalization: None,
    },
    BackendPresetDef {
        id: "ollama",
        label: "Ollama",
        backend_class: BackendClass::UserSuppliedService,
        allowed_apis: &[
            ApiType::OllamaChatApi,
            ApiType::OpenAiCompatible,
            ApiType::AnthropicCompatible,
        ],
        default_api: ApiType::OllamaChatApi,
        user_selects_api: true,
        endpoint_behavior: EndpointBehavior::UserSupplied,
        auth_mode: AuthMode::None,
        url_normalization: Some(UrlNormalization {
            default_scheme: "http",
            default_port: Some(11434),
            endpoint_label: "ollama URL: ",
            endpoint_hint: "http://host:11434   Enter confirm   Esc cancel",
        }),
    },
    BackendPresetDef {
        id: "ollama-com",
        label: "ollama.com",
        backend_class: BackendClass::BuiltInHosted,
        allowed_apis: &[ApiType::OllamaChatApi],
        default_api: ApiType::OllamaChatApi,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Predetermined,
        auth_mode: AuthMode::ApiKey,
        url_normalization: None,
    },
    BackendPresetDef {
        id: "open-webui",
        label: "Open WebUI",
        backend_class: BackendClass::UserSuppliedService,
        allowed_apis: &[ApiType::OpenAiCompatible, ApiType::OllamaChatApi],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: true,
        endpoint_behavior: EndpointBehavior::UserSupplied,
        auth_mode: AuthMode::ApiKey,
        url_normalization: Some(UrlNormalization {
            default_scheme: "https",
            default_port: None,
            endpoint_label: "open-webui URL: ",
            endpoint_hint: "https://my-webui.example.com   Enter confirm   Esc cancel",
        }),
    },
    BackendPresetDef {
        id: "openai-compatible",
        label: "OpenAI-compatible endpoint",
        backend_class: BackendClass::UserSuppliedService,
        allowed_apis: &[ApiType::OpenAiCompatible],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::UserSupplied,
        auth_mode: AuthMode::ApiKey,
        url_normalization: Some(UrlNormalization {
            default_scheme: "https",
            default_port: None,
            endpoint_label: "URL: ",
            endpoint_hint: "https://my-endpoint.example.com/v1   Enter confirm   Esc cancel",
        }),
    },
    BackendPresetDef {
        id: "test",
        label: "Test (UI exercise)",
        backend_class: BackendClass::Internal,
        allowed_apis: &[ApiType::Test],
        default_api: ApiType::Test,
        user_selects_api: false,
        endpoint_behavior: EndpointBehavior::Internal,
        auth_mode: AuthMode::None,
        url_normalization: None,
    },
];

impl BackendPreset {
    /// Look up this backend preset's static definition.
    pub fn def(&self) -> &'static BackendPresetDef {
        let id = self.id();
        BACKEND_PRESET_CATALOG
            .iter()
            .find(|d| d.id == id)
            .expect("every BackendPreset has a catalog entry")
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
            Self::OllamaCom => "ollama-com",
            Self::OpenWebUi => "open-webui",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Test => "test",
        }
    }

    pub fn label(&self) -> &'static str {
        self.def().label
    }

    /// Built-in hosted providers that always appear in the provider picker.
    ///
    /// These are unconditional singletons — the user never creates, names, or
    /// deletes them.  Credentials are collected on first selection.
    pub fn built_in_hosted() -> &'static [BackendPreset] {
        &[
            Self::Copilot,
            Self::Codex,
            Self::Gemini,
            Self::OpenAi,
            Self::OpenRouter,
            Self::OllamaCom,
        ]
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "copilot" => Some(Self::Copilot),
            "openai" => Some(Self::OpenAi),
            "openrouter" => Some(Self::OpenRouter),
            "codex" => Some(Self::Codex),
            "gemini" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            "ollama-com" => Some(Self::OllamaCom),
            "open-webui" => Some(Self::OpenWebUi),
            "openai-compatible" => Some(Self::OpenAiCompatible),
            "test" => Some(Self::Test),
            _ => None,
        }
    }

    /// Sensible default model name for first-time use.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Copilot => "gpt-4o",
            Self::OpenAi => "gpt-4o",
            Self::OpenRouter => "openai/gpt-4o",
            Self::Codex => "gpt-5.4",
            Self::Gemini => "gemini-2.5-pro",
            Self::Ollama => "llama3.1",
            Self::OllamaCom => "llama3.1",
            Self::OpenWebUi => "llama3.1",
            Self::OpenAiCompatible => "gpt-4o",
            Self::Test => "test",
        }
    }
}

/// A named, user-configured provider instance.
///
/// This is the primary unit xi-agent uses for provider selection and dispatch.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProviderInstance {
    /// Stable identifier and user-visible name (e.g. "work-webui", "gpu-box").
    /// Used as the key in config and selection state.
    pub id: String,
    /// The backend preset this instance connects to.
    #[serde(rename = "service_type", alias = "backend_preset")]
    pub backend_preset: BackendPreset,
    /// The API protocol xi-agent uses to talk to this instance.
    pub api_type: ApiType,
    /// Base URL (required for self-hosted; absent for cloud services that have
    /// a fixed endpoint).
    pub base_url: Option<String>,
    /// API key or bearer token, if needed by this service/API.
    pub api_key: Option<String>,
    /// Last-selected model for this instance.
    pub model: Option<String>,
}

impl ProviderInstance {
    /// Construct a new instance with the recommended defaults for the given
    /// backend preset.
    pub fn new(id: impl Into<String>, backend_preset: BackendPreset) -> Self {
        let api_type = backend_preset.def().default_api.clone();
        Self {
            id: id.into(),
            backend_preset,
            api_type,
            base_url: None,
            api_key: None,
            model: None,
        }
    }

    /// Display label shown in provider selection lists.
    pub fn label(&self) -> String {
        format!("{} ({})", self.id, self.backend_preset.label())
    }

    /// Effective model: last-selected model, or the service default.
    pub fn effective_model(&self) -> &str {
        self.model
            .as_deref()
            .unwrap_or_else(|| self.backend_preset.default_model())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_backend_preset_has_catalog_entry() {
        let types = [
            BackendPreset::Copilot,
            BackendPreset::OpenAi,
            BackendPreset::OpenRouter,
            BackendPreset::Codex,
            BackendPreset::Gemini,
            BackendPreset::Ollama,
            BackendPreset::OllamaCom,
            BackendPreset::OpenWebUi,
            BackendPreset::OpenAiCompatible,
            BackendPreset::Test,
        ];
        for st in &types {
            let def = st.def();
            assert!(
                !def.allowed_apis.is_empty(),
                "{} has no allowed APIs",
                st.id()
            );
            assert!(
                def.allowed_apis.contains(&def.default_api),
                "{} default_api not in allowed_apis",
                st.id()
            );
        }
    }

    #[test]
    fn backend_preset_round_trips_through_id() {
        let types = [
            BackendPreset::Copilot,
            BackendPreset::OpenAi,
            BackendPreset::OpenRouter,
            BackendPreset::Codex,
            BackendPreset::Gemini,
            BackendPreset::Ollama,
            BackendPreset::OllamaCom,
            BackendPreset::OpenWebUi,
            BackendPreset::OpenAiCompatible,
            BackendPreset::Test,
        ];
        for st in &types {
            let id = st.id();
            let roundtripped = BackendPreset::from_id(id).unwrap_or_else(|| {
                panic!("from_id failed for id={id}");
            });
            assert_eq!(st.id(), roundtripped.id());
        }
    }

    #[test]
    fn built_in_hosted_includes_all_hosted_services() {
        let built_ins = BackendPreset::built_in_hosted();
        assert_eq!(
            built_ins,
            &[
                BackendPreset::Copilot,
                BackendPreset::Codex,
                BackendPreset::Gemini,
                BackendPreset::OpenAi,
                BackendPreset::OpenRouter,
                BackendPreset::OllamaCom,
            ]
        );
        // Built-in hosted and user-supplied services should be disjoint.
        for bi in built_ins {
            assert!(
                bi.def().backend_class == BackendClass::BuiltInHosted,
                "{:?} should be BuiltInHosted",
                bi
            );
        }
    }

    #[test]
    fn provider_preset_metadata_matches_spec_semantics() {
        let openrouter = BackendPreset::OpenRouter.def();
        assert_eq!(openrouter.backend_class, BackendClass::BuiltInHosted);
        assert_eq!(openrouter.auth_mode, AuthMode::ApiKey);
        assert_eq!(
            openrouter.endpoint_behavior,
            EndpointBehavior::Predetermined
        );

        let copilot = BackendPreset::Copilot.def();
        assert_eq!(copilot.auth_mode, AuthMode::OAuthLogin);
        assert_eq!(copilot.endpoint_behavior, EndpointBehavior::Predetermined);

        let ollama = BackendPreset::Ollama.def();
        assert_eq!(ollama.backend_class, BackendClass::UserSuppliedService);
        assert_eq!(ollama.auth_mode, AuthMode::None);
        assert_eq!(ollama.endpoint_behavior, EndpointBehavior::UserSupplied);

        let webui = BackendPreset::OpenWebUi.def();
        assert_eq!(webui.backend_class, BackendClass::UserSuppliedService);
        assert_eq!(webui.auth_mode, AuthMode::ApiKey);
        assert_eq!(webui.endpoint_behavior, EndpointBehavior::UserSupplied);

        let ollamacom = BackendPreset::OllamaCom.def();
        assert_eq!(ollamacom.backend_class, BackendClass::BuiltInHosted);
        assert_eq!(ollamacom.auth_mode, AuthMode::ApiKey);
        assert_eq!(ollamacom.endpoint_behavior, EndpointBehavior::Predetermined);
    }

    #[test]
    fn provider_instance_new_uses_default_api() {
        let inst = ProviderInstance::new("my-ollama", BackendPreset::Ollama);
        assert_eq!(inst.api_type, ApiType::OllamaChatApi);
        assert_eq!(inst.effective_model(), "llama3.1");
    }

    #[test]
    fn provider_instance_effective_model_falls_back_to_default() {
        let inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        assert_eq!(inst.effective_model(), "gpt-4o");
    }

    #[test]
    fn provider_instance_effective_model_uses_override() {
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("claude-sonnet-4.5".to_string());
        assert_eq!(inst.effective_model(), "claude-sonnet-4.5");
    }
}
