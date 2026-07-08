use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::auth::paths::auth_file_path;
use crate::auth::types::{
    AuthFile, CodexCredentials, CopilotCredentials, GeminiCredentials, ProviderCredentials,
};

pub struct AuthStore {
    path: PathBuf,
    data: AuthFile,
}

impl AuthStore {
    pub fn load_default() -> anyhow::Result<Self> {
        let path = auth_file_path()?;
        Self::load(path)
    }

    /// Threshold for detecting `expires_at` values stored in milliseconds
    /// instead of seconds. Any value above this is assumed to be millisecond-
    /// epoch and gets divided by 1000.
    const MS_THRESHOLD: i64 = 999_999_999_999;

    pub fn load(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let mut data = if path.exists() {
            let text = fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", path.display(), e))?;
            toml::from_str::<AuthFile>(&text)
                .map_err(|e| anyhow::anyhow!("Cannot parse {}: {}", path.display(), e))?
        } else {
            AuthFile::default()
        };

        // Migration: pre-v2 auth files stored `expires_at` in milliseconds.
        // Detect and fix any values that still use the old unit.
        for creds in data.providers.values_mut() {
            creds.migrate_expires_at_ms_to_secs(Self::MS_THRESHOLD);
        }

        Ok(Self { path, data })
    }

    pub fn get_copilot(&self) -> Option<CopilotCredentials> {
        match self.data.providers.get("copilot") {
            Some(ProviderCredentials::Copilot {
                access_token,
                refresh_token,
                expires_at,
                base_url,
            }) => Some(CopilotCredentials {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                expires_at: *expires_at,
                base_url: base_url.clone(),
            }),
            _ => None,
        }
    }

    pub fn get_codex(&self) -> Option<CodexCredentials> {
        match self.data.providers.get("codex") {
            Some(ProviderCredentials::Codex {
                access_token,
                refresh_token,
                expires_at,
                account_id,
            }) => Some(CodexCredentials {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                expires_at: *expires_at,
                account_id: account_id.clone(),
            }),
            _ => None,
        }
    }

    pub fn get_gemini(&self) -> Option<GeminiCredentials> {
        match self.data.providers.get("gemini") {
            Some(ProviderCredentials::Gemini {
                access_token,
                refresh_token,
                expires_at,
                project_id,
            }) => Some(GeminiCredentials {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                expires_at: *expires_at,
                project_id: project_id.clone(),
            }),
            _ => None,
        }
    }

    pub fn set_copilot(&mut self, creds: CopilotCredentials) {
        self.data.providers.insert(
            "copilot".to_string(),
            ProviderCredentials::Copilot {
                access_token: creds.access_token,
                refresh_token: creds.refresh_token,
                expires_at: creds.expires_at,
                base_url: creds.base_url,
            },
        );
    }

    pub fn set_codex(&mut self, creds: CodexCredentials) {
        self.data.providers.insert(
            "codex".to_string(),
            ProviderCredentials::Codex {
                access_token: creds.access_token,
                refresh_token: creds.refresh_token,
                expires_at: creds.expires_at,
                account_id: creds.account_id,
            },
        );
    }

    pub fn set_gemini(&mut self, creds: GeminiCredentials) {
        self.data.providers.insert(
            "gemini".to_string(),
            ProviderCredentials::Gemini {
                access_token: creds.access_token,
                refresh_token: creds.refresh_token,
                expires_at: creds.expires_at,
                project_id: creds.project_id,
            },
        );
    }

    /// Store credentials from a [`ProviderCredentials`] value returned by a
    /// backend, routing to the correct typed setter by variant.
    pub fn set_from_credentials(&mut self, creds: ProviderCredentials) {
        match creds {
            ProviderCredentials::Copilot {
                access_token,
                refresh_token,
                expires_at,
                base_url,
            } => self.set_copilot(CopilotCredentials {
                access_token,
                refresh_token,
                expires_at,
                base_url,
            }),
            ProviderCredentials::Codex {
                access_token,
                refresh_token,
                expires_at,
                account_id,
            } => self.set_codex(CodexCredentials {
                access_token,
                refresh_token,
                expires_at,
                account_id,
            }),
            ProviderCredentials::Gemini {
                access_token,
                refresh_token,
                expires_at,
                project_id,
            } => self.set_gemini(GeminiCredentials {
                access_token,
                refresh_token,
                expires_at,
                project_id,
            }),
        }
    }

    /// Return the stored refresh token for `provider`, or `None` if absent.
    pub fn get_refresh_token(&self, provider: &str) -> Option<String> {
        match provider {
            "copilot" => self.get_copilot().map(|c| c.refresh_token),
            "codex" => self.get_codex().map(|c| c.refresh_token),
            "gemini" => self.get_gemini().map(|c| c.refresh_token),
            _ => None,
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        save_auth(&self.path, &self.data)
    }
}

fn save_auth(path: &Path, data: &AuthFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(data)
        .map_err(|e| anyhow::anyhow!("Cannot serialize auth file: {}", e))?;
    crate::atomic_file::save_atomic(path, &serialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn copilot_creds() -> CopilotCredentials {
        CopilotCredentials {
            access_token: "at_copilot".to_string(),
            refresh_token: "rt_copilot".to_string(),
            expires_at: 9_999_999_999,
            base_url: Some("https://api.example.com".to_string()),
        }
    }

    fn codex_creds() -> CodexCredentials {
        CodexCredentials {
            access_token: "at_codex".to_string(),
            refresh_token: "rt_codex".to_string(),
            expires_at: 8_888_888_888,
            account_id: "acct_123".to_string(),
        }
    }

    fn gemini_creds() -> GeminiCredentials {
        GeminiCredentials {
            access_token: "at_gemini".to_string(),
            refresh_token: "rt_gemini".to_string(),
            expires_at: 7_777_777_777,
            project_id: "project-123".to_string(),
        }
    }

    #[test]
    fn load_missing_path_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let store = AuthStore::load(&path).unwrap();
        assert!(store.get_copilot().is_none());
        assert!(store.get_codex().is_none());
        assert!(store.get_gemini().is_none());
    }

    #[test]
    fn round_trip_copilot() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_copilot(copilot_creds());
        store.save().unwrap();

        let store2 = AuthStore::load(&path).unwrap();
        let got = store2
            .get_copilot()
            .expect("copilot creds should be present");
        assert_eq!(got.access_token, "at_copilot");
        assert_eq!(got.refresh_token, "rt_copilot");
        assert_eq!(got.expires_at, 9_999_999_999);
        assert_eq!(got.base_url.as_deref(), Some("https://api.example.com"));
    }

    #[test]
    fn round_trip_codex() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_codex(codex_creds());
        store.save().unwrap();

        let store2 = AuthStore::load(&path).unwrap();
        let got = store2.get_codex().expect("codex creds should be present");
        assert_eq!(got.access_token, "at_codex");
        assert_eq!(got.refresh_token, "rt_codex");
        assert_eq!(got.expires_at, 8_888_888_888);
        assert_eq!(got.account_id, "acct_123");
    }

    #[test]
    fn round_trip_gemini() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_gemini(gemini_creds());
        store.save().unwrap();

        let store2 = AuthStore::load(&path).unwrap();
        let got = store2.get_gemini().expect("gemini creds should be present");
        assert_eq!(got.access_token, "at_gemini");
        assert_eq!(got.refresh_token, "rt_gemini");
        assert_eq!(got.expires_at, 7_777_777_777);
        assert_eq!(got.project_id, "project-123");
    }

    #[test]
    fn set_copilot_preserves_codex() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_copilot(copilot_creds());
        store.set_codex(codex_creds());
        store.set_gemini(gemini_creds());
        store.save().unwrap();

        let store2 = AuthStore::load(&path).unwrap();
        assert!(store2.get_copilot().is_some(), "copilot should survive");
        assert!(store2.get_codex().is_some(), "codex should survive");
        assert!(store2.get_gemini().is_some(), "gemini should survive");
    }

    #[test]
    fn atomic_save_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_copilot(copilot_creds());
        store.save().unwrap();

        assert!(path.exists(), "auth.toml should exist after save");
    }

    #[test]
    #[cfg(unix)]
    fn atomic_save_perms_0o600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");

        let mut store = AuthStore::load(&path).unwrap();
        store.set_copilot(copilot_creds());
        store.save().unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "file should be owner-read/write only");
    }

    // ── Atomic save failure injection ─────────────────────────────────────────

    use crate::atomic_file;

    #[test]
    fn save_atomic_fails_when_parent_is_a_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("not-a-dir");
        // Create a file where a directory would be expected.
        std::fs::write(&file_path, "content").unwrap();
        let path = file_path.join("auth.toml");

        // `create_dir_all` will fail because the parent is a file.
        let result = atomic_file::save_atomic(&path, "test content");
        assert!(
            result.is_err(),
            "save_atomic should fail when parent is a file"
        );
    }

    #[test]
    fn save_atomic_writes_and_reads_back() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.toml");

        assert!(atomic_file::save_atomic(&path, "hello world").is_ok());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn load_parse_error_does_not_clobber_existing_file_on_save_attempt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.toml");
        let original = "not valid toml = [";
        std::fs::write(&path, original).unwrap();

        let result = AuthStore::load(&path);
        assert!(result.is_err(), "load should fail for invalid TOML");

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "invalid auth file must remain untouched");
    }
}
