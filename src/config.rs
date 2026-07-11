use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::Context;

use crate::hooks::{HookConfig, HookPoint};
use crate::provider_instance::{BackendClass, BackendPreset, ProviderInstance};

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct HookIpcConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl HookIpcConfig {
    pub fn default_endpoint() -> &'static str {
        #[cfg(windows)]
        {
            r#"\\.\pipe\xi-hook-events"#
        }

        #[cfg(unix)]
        {
            "/tmp/xi-hook-events.sock"
        }
    }

    pub fn effective_endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| Self::default_endpoint().to_string())
    }
}

/// Display thresholds — presentation choices that control how much content
/// is shown in the UI. These do not affect how much is sent to the model.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DisplayConfig {
    /// Maximum lines of a shell command shown in the live turn view.
    pub max_shell_command_lines: usize,
    /// Characters before a command label switches to multi-line display.
    pub max_one_line_chars: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_shell_command_lines: 5,
            max_one_line_chars: 120,
        }
    }
}

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct XiConfig {
    /// Path to the theme file. Overridden by the `--theme` CLI flag.
    pub theme: Option<PathBuf>,
    /// UI display thresholds.
    #[serde(default)]
    pub display: DisplayConfig,

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

    /// Best-effort IPC publisher for hook events.
    #[serde(default)]
    pub hook_ipc: HookIpcConfig,

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
    ///
    /// For built-in hosted providers, auto-creates a config entry when none
    /// exists yet (so that model/API-key changes can be persisted).
    pub fn find_provider_mut(&mut self, id: &str) -> Option<&mut ProviderInstance> {
        if !self.providers.iter().any(|p| p.id == id)
            && let Some(preset) = BackendPreset::from_id(id)
            && preset.def().backend_class == BackendClass::BuiltInHosted
        {
            self.providers.push(ProviderInstance::new(id, preset));
        }
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

    /// Return all available providers: built-in catalog defaults merged with
    /// user config overrides, plus user-created instances.
    ///
    /// Built-in hosted providers always appear (from catalog or config override).
    /// User-supplied providers appear only when present in config.
    /// Sorted: built-ins before user-created, alphabetical within each group.
    pub fn resolve_effective_providers(&self) -> Vec<ProviderInstance> {
        let mut result = Vec::new();

        // Built-in hosted providers: catalog defaults, overridden by config.
        for preset in BackendPreset::built_in_hosted() {
            let id = preset.id().to_string();
            if let Some(cfg) = self.find_provider(&id) {
                result.push(cfg.clone());
            } else {
                result.push(ProviderInstance::new(id, preset.clone()));
            }
        }

        // User-supplied providers: config only.
        for provider in &self.providers {
            if provider.backend_preset.def().backend_class == BackendClass::UserSuppliedService {
                result.push(provider.clone());
            }
        }

        // Sort: built-ins before user-created, alphabetical within each group.
        result.sort_by(|a, b| {
            let a_builtin = BackendPreset::built_in_hosted()
                .iter()
                .any(|p| p.id() == a.id);
            let b_builtin = BackendPreset::built_in_hosted()
                .iter()
                .any(|p| p.id() == b.id);
            match (a_builtin, b_builtin) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.id.cmp(&b.id),
            }
        });

        result
    }

    /// Resolve a provider instance by id, falling back to the built-in catalog
    /// when no config entry exists.
    pub fn resolve_provider(&self, id: &str) -> Option<ProviderInstance> {
        if let Some(inst) = self.find_provider(id) {
            return Some(inst.clone());
        }
        BackendPreset::from_id(id).map(|preset| ProviderInstance::new(id, preset))
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
        save_config(&path, self)
    }
}

fn save_config(path: &std::path::Path, config: &XiConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let body = toml::to_string_pretty(config)?;
    crate::atomic_file::save_atomic(path, &body)
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    Ok(crate::dirs::project_dirs()?
        .config_dir()
        .join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::{HookIpcConfig, XiConfig, save_config};
    use crate::provider_instance::{ApiType, BackendPreset, ProviderInstance};

    // ── Instance-format config tests ─────────────────────────────────────────

    #[test]
    fn save_config_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let cfg = XiConfig::default();

        save_config(&path, &cfg).unwrap();

        assert!(path.exists());
    }

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
    fn resolve_effective_providers_includes_builtins_on_empty_config() {
        let cfg = XiConfig::default();
        let effective = cfg.resolve_effective_providers();
        // All built-in hosted presets are present.
        for preset in BackendPreset::built_in_hosted() {
            assert!(
                effective.iter().any(|p| p.id == preset.id()),
                "missing built-in: {}",
                preset.id()
            );
        }
        // No user-supplied providers on empty config.
        let builtin_ids: Vec<&str> = BackendPreset::built_in_hosted()
            .iter()
            .map(|p| p.id())
            .collect();
        for p in &effective {
            if !builtin_ids.contains(&p.id.as_str()) {
                panic!("unexpected non-builtin provider: {}", p.id);
            }
        }
    }

    #[test]
    fn resolve_effective_providers_includes_user_providers() {
        let mut cfg = XiConfig::default();
        cfg.upsert_provider(ProviderInstance::new("my-ollama", BackendPreset::Ollama));
        let effective = cfg.resolve_effective_providers();
        assert!(effective.iter().any(|p| p.id == "my-ollama"));
        // Built-ins still present.
        assert!(effective.iter().any(|p| p.id == "copilot"));
    }

    #[test]
    fn resolve_effective_providers_prefers_config_override_for_builtin() {
        let mut cfg = XiConfig::default();
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("override-model".to_string());
        cfg.upsert_provider(inst);
        let effective = cfg.resolve_effective_providers();
        let copilot = effective.iter().find(|p| p.id == "copilot").unwrap();
        assert_eq!(copilot.model.as_deref(), Some("override-model"));
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

    #[test]
    fn hook_ipc_section_parses() {
        let raw = r#"
[hook_ipc]
enabled = true
"#;
        let cfg = XiConfig::from_toml_str(raw).expect("config with hook_ipc parses");
        assert!(cfg.hook_ipc.enabled);
        assert_eq!(
            cfg.hook_ipc.effective_endpoint(),
            HookIpcConfig::default_endpoint()
        );
    }

    #[test]
    fn hook_ipc_endpoint_override_parses() {
        let raw = r#"
[hook_ipc]
enabled = true
endpoint = "custom-endpoint"
"#;
        let cfg = XiConfig::from_toml_str(raw).expect("config with hook_ipc endpoint parses");
        assert!(cfg.hook_ipc.enabled);
        assert_eq!(cfg.hook_ipc.endpoint.as_deref(), Some("custom-endpoint"));
        assert_eq!(cfg.hook_ipc.effective_endpoint(), "custom-endpoint");
    }

    #[test]
    fn hook_ipc_defaults_disabled() {
        let cfg = XiConfig::default();
        assert!(!cfg.hook_ipc.enabled);
        assert_eq!(cfg.hook_ipc.endpoint, None);
        assert_eq!(
            cfg.hook_ipc.effective_endpoint(),
            HookIpcConfig::default_endpoint()
        );
    }
}
