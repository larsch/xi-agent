use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::Context;

use crate::provider_instance::{BackendPreset, ProviderInstance};

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct TauConfig {
    /// The id of the currently active provider instance.
    pub provider: Option<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub thinking_by_model: HashMap<String, String>,

    /// Named provider instances (new format).
    /// When empty on load, `migrate_legacy_providers` synthesises instances
    /// from the legacy per-kind sections below.
    #[serde(default)]
    pub providers: Vec<ProviderInstance>,

    // ── Legacy per-kind sections (read-only after migration) ─────────────────
    #[serde(default)]
    pub openai: OpenAiConfig,
    #[serde(default)]
    pub copilot: CopilotConfig,
    #[serde(default)]
    pub ollama: OllamaConfig,
    #[serde(default)]
    pub codex: CodexConfig,
    #[serde(default)]
    pub gemini: GeminiConfig,
    #[serde(default)]
    pub open_webui: OpenWebUiConfig,
}

impl TauConfig {
    /// Return a reference to the provider instance with the given id, if any.
    pub fn find_provider(&self, id: &str) -> Option<&ProviderInstance> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Return a mutable reference to the provider instance with the given id.
    pub fn find_provider_mut(&mut self, id: &str) -> Option<&mut ProviderInstance> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// Add or replace a provider instance (keyed by id).
    pub fn upsert_provider(&mut self, instance: ProviderInstance) {
        if let Some(existing) = self.providers.iter_mut().find(|p| p.id == instance.id) {
            *existing = instance;
        } else {
            self.providers.push(instance);
        }
    }

    /// Remove a provider instance by id. Returns `true` if it was present.
    pub fn remove_provider(&mut self, id: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        self.providers.len() < before
    }

    /// Ensure singleton built-in hosted providers exist in `providers`.
    fn ensure_builtin_hosted_providers(&mut self) {
        if self.find_provider("openrouter").is_none() {
            self.providers.push(ProviderInstance::new(
                "openrouter",
                BackendPreset::OpenRouter,
            ));
        }
    }

    /// Synthesise provider instances from legacy per-kind config sections.
    ///
    /// Called automatically by `from_toml_str`. When `providers` is empty, it
    /// synthesises instances from the legacy per-kind sections below. It also
    /// ensures singleton built-in hosted providers are present for newer
    /// instance-based configs.
    /// Idempotent — safe to call multiple times.
    fn migrate_legacy_providers(&mut self) {
        if self.providers.is_empty() {
            // Copilot — always present as a built-in default.
            let mut copilot = ProviderInstance::new("copilot", BackendPreset::Copilot);
            copilot.model = self.copilot.model.clone();
            self.providers.push(copilot);

            // OpenAI — only if an api_key is configured.
            if self.openai.api_key.is_some() {
                let mut openai = ProviderInstance::new("openai", BackendPreset::OpenAi);
                openai.base_url = self.openai.base_url.clone();
                openai.api_key = self.openai.api_key.clone();
                openai.model = self.openai.model.clone();
                self.providers.push(openai);
            }

            // Codex — always a built-in (auth handled separately via AuthStore).
            let mut codex = ProviderInstance::new("codex", BackendPreset::Codex);
            codex.base_url = self.codex.base_url.clone();
            codex.model = self.codex.model.clone();
            self.providers.push(codex);

            // Gemini — always a built-in (auth handled separately via AuthStore).
            let mut gemini = ProviderInstance::new("gemini", BackendPreset::Gemini);
            gemini.base_url = self.gemini.base_url.clone();
            gemini.model = self.gemini.model.clone();
            self.providers.push(gemini);

            // Ollama — only if a base_url is configured; otherwise skip (user
            // hasn't set it up yet).
            if self.ollama.base_url.is_some() {
                let mut ollama = ProviderInstance::new("ollama", BackendPreset::Ollama);
                ollama.base_url = self.ollama.base_url.clone();
                ollama.model = self.ollama.model.clone();
                self.providers.push(ollama);
            }

            // Open WebUI — only if a base_url is configured.
            if self.open_webui.base_url.is_some() {
                let mut open_webui = ProviderInstance::new("open-webui", BackendPreset::OpenWebUi);
                open_webui.base_url = self.open_webui.base_url.clone();
                open_webui.api_key = self.open_webui.api_key.clone();
                open_webui.model = self.open_webui.model.clone();
                self.providers.push(open_webui);
            }

            // Migrate the `provider` field: map legacy provider names to instance ids.
            if let Some(ref name) = self.provider.clone() {
                let mapped_id = match name.as_str() {
                    "copilot" | "github-copilot" => Some("copilot"),
                    "openai" => Some("openai"),
                    "openrouter" => Some("openrouter"),
                    "codex" => Some("codex"),
                    "gemini" | "google-gemini" => Some("gemini"),
                    "ollama" => Some("ollama"),
                    "open-webui" | "openwebui" => Some("open-webui"),
                    _ => None,
                };
                if let Some(id) = mapped_id {
                    // Only keep the mapped id if we actually synthesised that instance.
                    if self.providers.iter().any(|p| p.id == id) {
                        self.provider = Some(id.to_string());
                    }
                }
            }
        }

        self.ensure_builtin_hosted_providers();
    }
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct OpenAiConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct CopilotConfig {
    pub model: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct OllamaConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Recently used Ollama endpoints, most-recent first.  Capped at
    /// [`OllamaConfig::MAX_RECENT_ENDPOINTS`] entries.
    #[serde(default)]
    pub recent_endpoints: Vec<String>,
}

impl OllamaConfig {
    /// Maximum number of recent endpoints to remember.
    pub const MAX_RECENT_ENDPOINTS: usize = 5;

    /// Push `url` to the front of `recent_endpoints`, dedup, and cap the list.
    /// Also sets `base_url` to `url`.
    pub fn record_endpoint(&mut self, url: String) {
        self.base_url = Some(url.clone());
        self.recent_endpoints.retain(|e| e != &url);
        self.recent_endpoints.insert(0, url);
        self.recent_endpoints.truncate(Self::MAX_RECENT_ENDPOINTS);
    }
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct CodexConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct GeminiConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct OpenWebUiConfig {
    /// Base URL of the Open WebUI instance (e.g. `https://my-webui.example.com`).
    pub base_url: Option<String>,
    /// API token used as `Authorization: Bearer <api_key>`.
    pub api_key: Option<String>,
    /// Last-selected model.
    pub model: Option<String>,
    /// Recently used Open WebUI endpoints, most-recent first.
    #[serde(default)]
    pub recent_endpoints: Vec<String>,
}

impl OpenWebUiConfig {
    pub const MAX_RECENT_ENDPOINTS: usize = 5;

    /// Push `url` to the front of `recent_endpoints`, dedup, and cap the list.
    /// Also sets `base_url` to `url`.
    pub fn record_endpoint(&mut self, url: String) {
        self.base_url = Some(url.clone());
        self.recent_endpoints.retain(|e| e != &url);
        self.recent_endpoints.insert(0, url);
        self.recent_endpoints.truncate(Self::MAX_RECENT_ENDPOINTS);
    }
}

impl TauConfig {
    /// Load from $XDG_CONFIG_HOME/tau/config.toml (or ~/.config/tau/config.toml).
    /// Missing file is not an error and returns `Default`.
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        Self::from_toml_str(&raw)
            .with_context(|| format!("Failed to parse TOML config file: {}", path.display()))
    }

    pub fn from_toml_str(raw: &str) -> anyhow::Result<Self> {
        let mut cfg: Self = toml::from_str(raw)?;
        cfg.migrate_legacy_providers();
        Ok(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }

        let body = toml::to_string_pretty(self)?;
        fs::write(&path, body)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;
        Ok(())
    }
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    Ok(crate::dirs::project_dirs()?
        .config_dir()
        .join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::{OllamaConfig, TauConfig};
    use crate::provider_instance::{ApiType, BackendPreset};

    #[test]
    fn parses_full_config_toml() {
        let raw = r#"
provider = "openai"
thinking = "low"

[thinking_by_model]
gpt-4o-mini = "minimal"
gpt-5 = "high"

[openai]
api_key = "sk-test"
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"

[copilot]
model = "gpt-4o"

[codex]
base_url = "https://chatgpt.com/backend-api/codex"
model = "gpt-5"

[gemini]
base_url = "https://cloudcode-pa.googleapis.com"
model = "gemini-2.5-pro"

[ollama]
base_url = "http://localhost:11434"
model = "llama3.1"
recent_endpoints = ["http://localhost:11434", "http://gpu-box:11434"]
"#;

        let cfg = TauConfig::from_toml_str(raw).expect("config parses");

        assert_eq!(cfg.provider.as_deref(), Some("openai"));
        assert_eq!(cfg.thinking.as_deref(), Some("low"));
        assert_eq!(
            cfg.thinking_by_model.get("gpt-4o-mini").map(String::as_str),
            Some("minimal")
        );
        assert_eq!(
            cfg.thinking_by_model.get("gpt-5").map(String::as_str),
            Some("high")
        );
        assert_eq!(cfg.openai.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            cfg.openai.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(cfg.openai.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(cfg.copilot.model.as_deref(), Some("gpt-4o"));
        assert_eq!(
            cfg.codex.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert_eq!(cfg.codex.model.as_deref(), Some("gpt-5"));
        assert_eq!(
            cfg.gemini.base_url.as_deref(),
            Some("https://cloudcode-pa.googleapis.com")
        );
        assert_eq!(cfg.gemini.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(
            cfg.ollama.base_url.as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(cfg.ollama.model.as_deref(), Some("llama3.1"));
        assert_eq!(
            cfg.ollama.recent_endpoints,
            vec!["http://localhost:11434", "http://gpu-box:11434"]
        );
    }

    // ── Migration tests ───────────────────────────────────────────────────────

    #[test]
    fn migration_synthesises_copilot_from_legacy_config() {
        let raw = r#"
provider = "copilot"
[copilot]
model = "gpt-4o"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        let inst = cfg
            .find_provider("copilot")
            .expect("copilot instance present");
        assert_eq!(inst.backend_preset, BackendPreset::Copilot);
        assert_eq!(inst.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cfg.provider.as_deref(), Some("copilot"));
    }

    #[test]
    fn migration_synthesises_openrouter_as_builtin_hosted_provider() {
        let raw = r#"
provider = "copilot"
[copilot]
model = "gpt-4o"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        let inst = cfg
            .find_provider("openrouter")
            .expect("openrouter instance present");
        assert_eq!(inst.backend_preset, BackendPreset::OpenRouter);
        assert_eq!(inst.id, "openrouter");
    }

    #[test]
    fn migration_adds_openrouter_to_existing_providers_without_duplication() {
        let raw = r#"
[[providers]]
id = "copilot"
backend_preset = "copilot"
api_type = "openai-compatible"
model = "claude-sonnet-4.5"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        assert!(cfg.find_provider("openrouter").is_some());
        assert_eq!(
            cfg.providers
                .iter()
                .filter(|p| p.id == "openrouter")
                .count(),
            1
        );
    }

    #[test]
    fn migration_synthesises_openai_when_api_key_present() {
        let raw = r#"
[openai]
api_key = "sk-test"
model = "gpt-4o-mini"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        let inst = cfg
            .find_provider("openai")
            .expect("openai instance present");
        assert_eq!(inst.backend_preset, BackendPreset::OpenAi);
        assert_eq!(inst.api_key.as_deref(), Some("sk-test"));
        assert_eq!(inst.model.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn migration_skips_openai_without_api_key() {
        let raw = r#"
[openai]
model = "gpt-4o-mini"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        assert!(cfg.find_provider("openai").is_none());
    }

    #[test]
    fn migration_synthesises_ollama_when_base_url_present() {
        let raw = r#"
[ollama]
base_url = "http://gpu-box:11434"
model = "llama3.1"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        let inst = cfg
            .find_provider("ollama")
            .expect("ollama instance present");
        assert_eq!(inst.backend_preset, BackendPreset::Ollama);
        assert_eq!(inst.api_type, ApiType::OllamaChatApi);
        assert_eq!(inst.base_url.as_deref(), Some("http://gpu-box:11434"));
    }

    #[test]
    fn migration_skips_ollama_without_base_url() {
        let raw = "[ollama]\nmodel = \"llama3.1\"\n";
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        assert!(cfg.find_provider("ollama").is_none());
    }

    #[test]
    fn migration_synthesises_open_webui_when_base_url_present() {
        let raw = r#"
[open_webui]
base_url = "https://my-webui.example.com"
api_key = "token123"
model = "llama3.1"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        let inst = cfg
            .find_provider("open-webui")
            .expect("open-webui instance present");
        assert_eq!(inst.backend_preset, BackendPreset::OpenWebUi);
        assert_eq!(inst.api_key.as_deref(), Some("token123"));
    }

    #[test]
    fn migration_is_idempotent_when_providers_already_present() {
        let raw = r#"
[copilot]
model = "gpt-4o"

[[providers]]
id = "copilot"
backend_preset = "copilot"
api_type = "openai-compatible"
model = "claude-sonnet-4.5"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        // Explicit providers remain intact, and builtin openrouter is added once.
        assert_eq!(cfg.providers.len(), 2);
        let inst = cfg.find_provider("copilot").unwrap();
        // The explicit [[providers]] entry wins, not the legacy [copilot] section.
        assert_eq!(inst.model.as_deref(), Some("claude-sonnet-4.5"));
        let openrouter = cfg.find_provider("openrouter").unwrap();
        assert_eq!(openrouter.backend_preset, BackendPreset::OpenRouter);
    }

    #[test]
    fn new_providers_format_parses_directly() {
        let raw = r#"
provider = "work-webui"

[[providers]]
id = "work-webui"
backend_preset = "open-webui"
api_type = "openai-compatible"
base_url = "https://work.example.com"
api_key = "tok"
model = "llama3.1"

[[providers]]
id = "gpu-box"
backend_preset = "ollama"
api_type = "ollama-chat-api"
base_url = "http://gpu-box:11434"
"#;
        let cfg = TauConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.providers.len(), 3);

        let webui = cfg.find_provider("work-webui").unwrap();
        assert_eq!(webui.backend_preset, BackendPreset::OpenWebUi);
        assert_eq!(webui.base_url.as_deref(), Some("https://work.example.com"));

        let gpu = cfg.find_provider("gpu-box").unwrap();
        assert_eq!(gpu.backend_preset, BackendPreset::Ollama);
        assert_eq!(gpu.api_type, ApiType::OllamaChatApi);

        let openrouter = cfg.find_provider("openrouter").unwrap();
        assert_eq!(openrouter.backend_preset, BackendPreset::OpenRouter);

        assert_eq!(cfg.provider.as_deref(), Some("work-webui"));
    }

    #[test]
    fn upsert_and_remove_provider() {
        let mut cfg = TauConfig::default();
        use crate::provider_instance::ProviderInstance;
        let inst = ProviderInstance::new("my-ollama", BackendPreset::Ollama);
        cfg.upsert_provider(inst);
        assert!(cfg.find_provider("my-ollama").is_some());

        // Upsert again with model set
        let mut inst2 = ProviderInstance::new("my-ollama", BackendPreset::Ollama);
        inst2.model = Some("mistral".into());
        cfg.upsert_provider(inst2);
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(
            cfg.find_provider("my-ollama").unwrap().model.as_deref(),
            Some("mistral")
        );

        assert!(cfg.remove_provider("my-ollama"));
        assert!(cfg.find_provider("my-ollama").is_none());
        assert!(!cfg.remove_provider("my-ollama")); // idempotent
    }

    #[test]
    fn upsert_provider_replaces_existing_provider_after_rename_when_old_id_removed() {
        let mut cfg = TauConfig::default();
        let mut original =
            crate::provider_instance::ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        original.base_url = Some("http://gpu-box:11434".to_string());
        cfg.upsert_provider(original);

        let mut renamed =
            crate::provider_instance::ProviderInstance::new("renamed-box", BackendPreset::Ollama);
        renamed.base_url = Some("http://gpu-box:11434".to_string());

        assert!(cfg.remove_provider("gpu-box"));
        cfg.upsert_provider(renamed.clone());

        assert!(cfg.find_provider("gpu-box").is_none());
        let inst = cfg
            .find_provider("renamed-box")
            .expect("renamed provider present");
        assert_eq!(inst.base_url.as_deref(), Some("http://gpu-box:11434"));
        assert_eq!(cfg.providers.len(), 1);
    }

    // ── Legacy tests (kept for backward compatibility) ────────────────────────

    #[test]
    fn ollama_record_endpoint_prepends_and_deduplicates() {
        let mut cfg = OllamaConfig::default();
        cfg.record_endpoint("http://localhost:11434".into());
        cfg.record_endpoint("http://gpu-box:11434".into());
        cfg.record_endpoint("http://localhost:11434".into()); // duplicate → moved to front
        assert_eq!(
            cfg.recent_endpoints,
            vec!["http://localhost:11434", "http://gpu-box:11434"]
        );
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn ollama_record_endpoint_caps_at_max() {
        let mut cfg = OllamaConfig::default();
        for i in 0..=OllamaConfig::MAX_RECENT_ENDPOINTS {
            cfg.record_endpoint(format!("http://host-{i}:11434"));
        }
        assert_eq!(
            cfg.recent_endpoints.len(),
            OllamaConfig::MAX_RECENT_ENDPOINTS
        );
    }

    #[test]
    fn ollama_record_endpoint_sets_base_url() {
        let mut cfg = super::OllamaConfig::default();
        cfg.record_endpoint("http://localhost:11434".to_string());
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434"));
    }
}
