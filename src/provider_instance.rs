// Items in this module form the public provider-instance API; not all are
// used at every call site yet, and that is expected.
#![allow(dead_code)]

/// API protocol/transport types that tau knows how to speak.
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

/// Recognisable software / cloud services tau supports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceType {
    /// GitHub Copilot (cloud, managed routing)
    Copilot,
    /// OpenAI API (cloud)
    #[serde(rename = "openai")]
    OpenAi,
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
    /// Generic OpenAI-compatible endpoint (e.g. OpenRouter)
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Internal test provider — never shown to users.
    Test,
}

/// Metadata tau keeps about a service type.
pub struct ServiceDef {
    /// Machine-readable id (matches `ServiceType` serialisation).
    pub id: &'static str,
    /// Human-readable display label.
    pub label: &'static str,
    /// API types that this service supports, in preference order.
    pub allowed_apis: &'static [ApiType],
    /// The recommended / default API type.
    pub default_api: ApiType,
    /// Whether the user should be allowed to choose the API type.
    /// `false` means tau picks internally (e.g. Copilot).
    pub user_selects_api: bool,
    /// Whether multiple instances of this service make sense.
    pub multi_instance: bool,
    /// Whether a custom base URL can be set by the user.
    pub custom_base_url: bool,
}

/// Static catalog of all supported service types.
pub const SERVICE_CATALOG: &[ServiceDef] = &[
    ServiceDef {
        id: "copilot",
        label: "GitHub Copilot",
        allowed_apis: &[
            ApiType::OpenAiCompatible,
            ApiType::OpenAiResponses,
            ApiType::AnthropicCompatible,
        ],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
    ServiceDef {
        id: "openai",
        label: "OpenAI API",
        allowed_apis: &[ApiType::OpenAiCompatible],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
    ServiceDef {
        id: "codex",
        label: "OpenAI Codex (chatgpt.com)",
        allowed_apis: &[ApiType::OpenAiResponses],
        default_api: ApiType::OpenAiResponses,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
    ServiceDef {
        id: "gemini",
        label: "Google Gemini CLI (Cloud Code Assist)",
        allowed_apis: &[ApiType::GeminiNative],
        default_api: ApiType::GeminiNative,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
    ServiceDef {
        id: "ollama",
        label: "Ollama",
        allowed_apis: &[
            ApiType::OllamaChatApi,
            ApiType::OpenAiCompatible,
            ApiType::AnthropicCompatible,
        ],
        default_api: ApiType::OllamaChatApi,
        user_selects_api: true,
        multi_instance: true,
        custom_base_url: true,
    },
    ServiceDef {
        id: "ollama-com",
        label: "ollama.com",
        allowed_apis: &[ApiType::OllamaChatApi],
        default_api: ApiType::OllamaChatApi,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
    ServiceDef {
        id: "open-webui",
        label: "Open WebUI",
        allowed_apis: &[ApiType::OpenAiCompatible, ApiType::OllamaChatApi],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: true,
        multi_instance: true,
        custom_base_url: true,
    },
    ServiceDef {
        id: "openai-compatible",
        label: "OpenAI-compatible endpoint",
        allowed_apis: &[ApiType::OpenAiCompatible],
        default_api: ApiType::OpenAiCompatible,
        user_selects_api: false,
        multi_instance: true,
        custom_base_url: true,
    },
    ServiceDef {
        id: "test",
        label: "Test (UI exercise)",
        allowed_apis: &[ApiType::Test],
        default_api: ApiType::Test,
        user_selects_api: false,
        multi_instance: false,
        custom_base_url: false,
    },
];

impl ServiceType {
    /// Look up this service type's static definition.
    pub fn def(&self) -> &'static ServiceDef {
        let id = self.id();
        SERVICE_CATALOG
            .iter()
            .find(|d| d.id == id)
            .expect("every ServiceType has a catalog entry")
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::OpenAi => "openai",
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

    /// All service types visible in the "add provider" menu.
    pub fn user_visible() -> &'static [ServiceType] {
        &[
            Self::Copilot,
            Self::OpenAi,
            Self::Codex,
            Self::Gemini,
            Self::Ollama,
            Self::OllamaCom,
            Self::OpenWebUi,
            Self::OpenAiCompatible,
        ]
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "copilot" => Some(Self::Copilot),
            "openai" => Some(Self::OpenAi),
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
/// This is the primary unit tau uses for provider selection and dispatch.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProviderInstance {
    /// Stable identifier and user-visible name (e.g. "work-webui", "gpu-box").
    /// Used as the key in config and selection state.
    pub id: String,
    /// The recognisable software / service this instance connects to.
    pub service_type: ServiceType,
    /// The API protocol tau uses to talk to this instance.
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
    /// service type.
    pub fn new(id: impl Into<String>, service_type: ServiceType) -> Self {
        let api_type = service_type.def().default_api.clone();
        Self {
            id: id.into(),
            service_type,
            api_type,
            base_url: None,
            api_key: None,
            model: None,
        }
    }

    /// Display label shown in provider selection lists.
    pub fn label(&self) -> String {
        format!("{} ({})", self.id, self.service_type.label())
    }

    /// Effective model: last-selected model, or the service default.
    pub fn effective_model(&self) -> &str {
        self.model
            .as_deref()
            .unwrap_or_else(|| self.service_type.default_model())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_type_has_catalog_entry() {
        let types = [
            ServiceType::Copilot,
            ServiceType::OpenAi,
            ServiceType::Codex,
            ServiceType::Gemini,
            ServiceType::Ollama,
            ServiceType::OllamaCom,
            ServiceType::OpenWebUi,
            ServiceType::OpenAiCompatible,
            ServiceType::Test,
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
    fn service_type_round_trips_through_id() {
        let types = [
            ServiceType::Copilot,
            ServiceType::OpenAi,
            ServiceType::Codex,
            ServiceType::Gemini,
            ServiceType::Ollama,
            ServiceType::OllamaCom,
            ServiceType::OpenWebUi,
            ServiceType::OpenAiCompatible,
            ServiceType::Test,
        ];
        for st in &types {
            let id = st.id();
            let roundtripped = ServiceType::from_id(id).unwrap_or_else(|| {
                panic!("from_id failed for id={id}");
            });
            assert_eq!(st.id(), roundtripped.id());
        }
    }

    #[test]
    fn provider_instance_new_uses_default_api() {
        let inst = ProviderInstance::new("my-ollama", ServiceType::Ollama);
        assert_eq!(inst.api_type, ApiType::OllamaChatApi);
        assert_eq!(inst.effective_model(), "llama3.1");
    }

    #[test]
    fn provider_instance_effective_model_falls_back_to_default() {
        let inst = ProviderInstance::new("copilot", ServiceType::Copilot);
        assert_eq!(inst.effective_model(), "gpt-4o");
    }

    #[test]
    fn provider_instance_effective_model_uses_override() {
        let mut inst = ProviderInstance::new("copilot", ServiceType::Copilot);
        inst.model = Some("claude-sonnet-4.5".to_string());
        assert_eq!(inst.effective_model(), "claude-sonnet-4.5");
    }
}
