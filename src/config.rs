use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::Context;

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct TauConfig {
    pub provider: Option<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub thinking_by_model: HashMap<String, String>,

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

    /// Default endpoint used when nothing else is configured.
    pub const DEFAULT_ENDPOINT: &'static str = "http://localhost:11434";

    /// Push `url` to the front of `recent_endpoints`, dedup, and cap the list.
    /// Also sets `base_url` to `url`.
    pub fn record_endpoint(&mut self, url: String) {
        self.base_url = Some(url.clone());
        self.recent_endpoints.retain(|e| e != &url);
        self.recent_endpoints.insert(0, url);
        self.recent_endpoints.truncate(Self::MAX_RECENT_ENDPOINTS);
    }

    /// Return the list of recent endpoints, always including the default as a
    /// fallback at the end when it isn't already present.
    pub fn effective_recent_endpoints(&self) -> Vec<String> {
        let mut list = self.recent_endpoints.clone();
        let default = Self::DEFAULT_ENDPOINT.to_string();
        if !list.contains(&default) {
            list.push(default);
        }
        list
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
        Ok(toml::from_str::<Self>(raw)?)
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
    fn ollama_effective_recent_endpoints_always_includes_default() {
        let cfg = OllamaConfig::default();
        let list = cfg.effective_recent_endpoints();
        assert!(list.contains(&OllamaConfig::DEFAULT_ENDPOINT.to_string()));
    }

    #[test]
    fn ollama_effective_recent_endpoints_does_not_duplicate_default() {
        let mut cfg = OllamaConfig::default();
        cfg.record_endpoint(OllamaConfig::DEFAULT_ENDPOINT.into());
        let list = cfg.effective_recent_endpoints();
        assert_eq!(
            list.iter()
                .filter(|e| e.as_str() == OllamaConfig::DEFAULT_ENDPOINT)
                .count(),
            1
        );
    }
}
