use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::Context;

use crate::hooks::{HookConfig, HookPoint};
use crate::provider_instance::ProviderInstance;

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct XiConfig {
    /// The id of the currently active provider instance.
    pub provider: Option<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub thinking_by_model: HashMap<String, String>,

    /// Named provider instances.
    #[serde(default)]
    pub providers: Vec<ProviderInstance>,

    /// Agent-level hooks — user-defined commands that run at specific points
    /// in the agent loop (e.g. pre_tool, post_tool, pre_turn, post_turn, on_error).
    /// Each hook point supports an array of hook configurations.
    #[serde(default)]
    pub hooks: HashMap<HookPoint, Vec<HookConfig>>,

    // Provider-specific persisted settings (legacy per-preset config; kept for
    // backward-compatible TOML parsing and any UI convenience state that still
    // reads from them).
    #[serde(default)]
    pub openai: OpenAiConfig,
    #[serde(default)]
    pub copilot: CopilotConfig,
    #[serde(default)]
    pub codex: CodexConfig,
    #[serde(default)]
    pub gemini: GeminiConfig,
}

impl XiConfig {
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
pub struct CodexConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct GeminiConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl XiConfig {
    /// Load from $XDG_CONFIG_HOME/xi/config.toml (or ~/.config/xi/config.toml).
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
        Ok(toml::from_str(raw)?)
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
    use super::XiConfig;
    use crate::provider_instance::{ApiType, BackendPreset};

    // ── Instance-format config tests ─────────────────────────────────────────

    #[test]
    fn provider_sections_parse_without_synthesising_instances() {
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

        let cfg = XiConfig::from_toml_str(raw).expect("config parses");

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
        // Legacy [ollama] section is silently ignored — no provider instance synthesised.
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn legacy_provider_sections_do_not_synthesise_instances() {
        let raw = r#"
provider = "copilot"

[openai]
api_key = "sk-test"
model = "gpt-4o-mini"

[copilot]
model = "gpt-4o"

[codex]
model = "gpt-5"

[gemini]
model = "gemini-2.5-pro"

[ollama]
base_url = "http://gpu-box:11434"
model = "llama3.1"

[open_webui]
base_url = "https://my-webui.example.com"
api_key = "token123"
model = "llama3.1"
"#;
        let cfg = XiConfig::from_toml_str(raw).unwrap();
        assert!(cfg.providers.is_empty());
        assert!(cfg.find_provider("copilot").is_none());
        assert!(cfg.find_provider("openai").is_none());
        assert!(cfg.find_provider("codex").is_none());
        assert!(cfg.find_provider("gemini").is_none());
        assert!(cfg.find_provider("ollama").is_none());
        assert!(cfg.find_provider("open-webui").is_none());
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
        let cfg = XiConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.providers.len(), 2);

        let webui = cfg.find_provider("work-webui").unwrap();
        assert_eq!(webui.backend_preset, BackendPreset::OpenWebUi);
        assert_eq!(webui.base_url.as_deref(), Some("https://work.example.com"));

        let gpu = cfg.find_provider("gpu-box").unwrap();
        assert_eq!(gpu.backend_preset, BackendPreset::Ollama);
        assert_eq!(gpu.api_type, ApiType::OllamaChatApi);

        assert!(cfg.find_provider("openrouter").is_none());

        assert_eq!(cfg.provider.as_deref(), Some("work-webui"));
    }

    #[test]
    fn upsert_and_remove_provider() {
        let mut cfg = XiConfig::default();
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
        let mut cfg = XiConfig::default();
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

    // ── Hooks config tests ─────────────────────────────────────────────────

    #[test]
    fn hooks_section_parses() {
        let raw = r#"
[[hooks.pre_tool]]
command = "/home/user/bin/notify"
timeout = 5

[[hooks.post_tool]]
command = "/home/user/bin/log-tool"

[[hooks.on_error]]
command = "/home/user/bin/alert"
timeout = 10
"#;
        let cfg = XiConfig::from_toml_str(raw).expect("config with hooks parses");

        let pre = cfg
            .hooks
            .get(&crate::hooks::HookPoint::PreTool)
            .and_then(|v| v.first())
            .unwrap();
        assert_eq!(pre.command.as_deref(), Some("/home/user/bin/notify"));
        assert_eq!(pre.timeout, 5);

        let post = cfg
            .hooks
            .get(&crate::hooks::HookPoint::PostTool)
            .and_then(|v| v.first())
            .unwrap();
        assert_eq!(post.command.as_deref(), Some("/home/user/bin/log-tool"));
        assert_eq!(post.timeout, 30); // default

        let err = cfg
            .hooks
            .get(&crate::hooks::HookPoint::OnError)
            .and_then(|v| v.first())
            .unwrap();
        assert_eq!(err.command.as_deref(), Some("/home/user/bin/alert"));
        assert_eq!(err.timeout, 10);

        assert!(!cfg.hooks.contains_key(&crate::hooks::HookPoint::PreTurn));
        assert!(!cfg.hooks.contains_key(&crate::hooks::HookPoint::PostTurn));
    }

    #[test]
    fn hooks_round_trip() {
        let raw = r#"
provider = "test"

[[hooks.pre_tool]]
bash = "echo hello"
timeout = 5
"#;
        let cfg = XiConfig::from_toml_str(raw).expect("parse");
        let serialized = toml::to_string_pretty(&cfg).expect("serialize");
        let cfg2 = XiConfig::from_toml_str(&serialized).expect("re-parse");
        let pre = cfg2
            .hooks
            .get(&crate::hooks::HookPoint::PreTool)
            .and_then(|v| v.first())
            .expect("pre_tool hook preserved");
        assert_eq!(pre.bash.as_deref(), Some("echo hello"));
        assert_eq!(pre.timeout, 5);
    }
}
