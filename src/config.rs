use std::{fs, path::PathBuf};

use anyhow::Context;

#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct TauConfig {
    pub provider: Option<String>,
    pub thinking: Option<String>,

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
    use super::TauConfig;

    #[test]
    fn parses_full_config_toml() {
        let raw = r#"
provider = "openai"
thinking = "low"

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
"#;

        let cfg = TauConfig::from_toml_str(raw).expect("config parses");

        assert_eq!(cfg.provider.as_deref(), Some("openai"));
        assert_eq!(cfg.thinking.as_deref(), Some("low"));
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
    }
}
