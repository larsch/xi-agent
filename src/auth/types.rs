use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub providers: HashMap<String, ProviderCredentials>,
}

impl Default for AuthFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            providers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProviderCredentials {
    #[serde(rename = "copilot")]
    Copilot {
        access_token: String,
        refresh_token: String,
        expires_at: i64,
        #[serde(default)]
        base_url: Option<String>,
    },
    #[serde(rename = "codex")]
    Codex {
        access_token: String,
        refresh_token: String,
        expires_at: i64,
        account_id: String,
    },
    #[serde(rename = "gemini")]
    Gemini {
        access_token: String,
        refresh_token: String,
        expires_at: i64,
        project_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct CopilotCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub account_id: String,
}

#[derive(Debug, Clone)]
pub struct GeminiCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub project_id: String,
}

#[cfg(test)]
mod tests {
    use super::{AuthFile, ProviderCredentials};

    #[test]
    fn auth_file_defaults_when_fields_missing() {
        let parsed: AuthFile = serde_json::from_str("{}").expect("parse auth file");
        assert_eq!(parsed.version, 1);
        assert!(parsed.providers.is_empty());
    }

    #[test]
    fn copilot_credentials_deserialize_without_base_url() {
        let raw = r#"
        {
          "version": 1,
          "providers": {
            "copilot": {
              "kind": "copilot",
              "access_token": "a",
              "refresh_token": "r",
              "expires_at": 123
            }
          }
        }
        "#;

        let parsed: AuthFile = serde_json::from_str(raw).expect("parse with copilot credentials");
        match parsed.providers.get("copilot").expect("copilot entry") {
            ProviderCredentials::Copilot {
                access_token,
                refresh_token,
                expires_at,
                base_url,
            } => {
                assert_eq!(access_token, "a");
                assert_eq!(refresh_token, "r");
                assert_eq!(*expires_at, 123);
                assert_eq!(base_url, &None);
            }
            _ => panic!("expected copilot credentials"),
        }
    }

    #[test]
    fn provider_credentials_round_trip_json() {
        let mut auth = AuthFile::default();
        auth.providers.insert(
            "copilot".to_string(),
            ProviderCredentials::Copilot {
                access_token: "cop_tok".to_string(),
                refresh_token: "cop_ref".to_string(),
                expires_at: 111,
                base_url: Some("https://api.example".to_string()),
            },
        );
        auth.providers.insert(
            "codex".to_string(),
            ProviderCredentials::Codex {
                access_token: "cod_tok".to_string(),
                refresh_token: "cod_ref".to_string(),
                expires_at: 222,
                account_id: "acct_123".to_string(),
            },
        );
        auth.providers.insert(
            "gemini".to_string(),
            ProviderCredentials::Gemini {
                access_token: "gem_tok".to_string(),
                refresh_token: "gem_ref".to_string(),
                expires_at: 333,
                project_id: "proj-123".to_string(),
            },
        );

        let json = serde_json::to_string(&auth).expect("serialize");
        let round_trip: AuthFile = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(round_trip.version, 1);
        assert_eq!(round_trip.providers.len(), 3);
        assert!(matches!(
            round_trip.providers.get("codex"),
            Some(ProviderCredentials::Codex { account_id, .. }) if account_id == "acct_123"
        ));
        assert!(matches!(
            round_trip.providers.get("gemini"),
            Some(ProviderCredentials::Gemini { project_id, .. }) if project_id == "proj-123"
        ));
    }
}
