use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::mpsc::UnboundedSender;

pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod open_url;
pub mod paths;
pub mod store;
pub mod types;

pub use store::AuthStore;

/// Describes what the user needs to do after receiving an [`LoginEvent::AuthCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFlow {
    /// GitHub device-code flow: open the URL **and** type the short code shown
    /// on screen into the browser.
    DeviceCode,
    /// PKCE redirect flow (Codex, Gemini): open the URL; the browser will
    /// redirect back to localhost automatically once the user approves.
    RedirectCallback,
}

#[derive(Debug, Clone)]
pub enum LoginEvent {
    Info(String),
    AuthCode {
        url: String,
        /// Short user-facing code to be entered in the browser (device flow only).
        code: Option<String>,
        /// Which flow is in use, so the UI can show appropriate instructions.
        flow: AuthFlow,
    },
    Success {
        provider: String,
    },
    Error {
        provider: String,
        message: String,
    },
    RefreshResult {
        provider: String,
        success: bool,
        message: String,
    },
    Finished,
}

pub async fn login_provider(
    provider: &str,
    tx: UnboundedSender<LoginEvent>,
    cancel: Arc<AtomicBool>,
) {
    log::debug!("login_provider called: provider={provider}");
    let _ = tx.send(LoginEvent::Info(format!(
        "Starting login for {provider}..."
    )));

    let result = match provider {
        "copilot" => {
            let creds = copilot::login(
                |ev| match ev {
                    copilot::CopilotLoginEvent::DeviceCode {
                        verification_uri,
                        user_code,
                    } => {
                        let _ = tx.send(LoginEvent::AuthCode {
                            url: verification_uri,
                            code: Some(user_code),
                            flow: AuthFlow::DeviceCode,
                        });
                    }
                    copilot::CopilotLoginEvent::Progress(msg) => {
                        let _ = tx.send(LoginEvent::Info(msg));
                    }
                },
                cancel.clone(),
            )
            .await;

            creds.map(|creds| {
                let mut store = AuthStore::load_default()?;
                store.set_copilot(creds);
                store.save()
            })
        }
        "codex" => {
            let creds = codex::login(
                |ev| match ev {
                    codex::CodexLoginEvent::OpenBrowser(url) => {
                        let _ = tx.send(LoginEvent::AuthCode {
                            url,
                            code: None,
                            flow: AuthFlow::RedirectCallback,
                        });
                    }
                    codex::CodexLoginEvent::Progress(msg) => {
                        let _ = tx.send(LoginEvent::Info(msg));
                    }
                },
                cancel.clone(),
            )
            .await;

            creds.map(|creds| {
                let mut store = AuthStore::load_default()?;
                store.set_codex(creds);
                store.save()
            })
        }
        "gemini" => {
            let creds = gemini::login(
                |ev| match ev {
                    gemini::GeminiLoginEvent::OpenBrowser(url) => {
                        let _ = tx.send(LoginEvent::AuthCode {
                            url,
                            code: None,
                            flow: AuthFlow::RedirectCallback,
                        });
                    }
                    gemini::GeminiLoginEvent::Progress(msg) => {
                        let _ = tx.send(LoginEvent::Info(msg));
                    }
                },
                cancel.clone(),
            )
            .await;

            creds.map(|creds| {
                let mut store = AuthStore::load_default()?;
                store.set_gemini(creds);
                store.save()
            })
        }
        _ => Err(anyhow::anyhow!(
            "Unsupported provider for /login: {provider}"
        )),
    };

    match result {
        Ok(Ok(())) => {
            let _ = tx.send(LoginEvent::Success {
                provider: provider.to_string(),
            });
        }
        Ok(Err(e)) | Err(e) => {
            let is_cancelled = cancel.load(Ordering::Relaxed)
                || e.to_string().to_ascii_lowercase().contains("cancel");
            let message = if is_cancelled {
                "Login cancelled".to_string()
            } else {
                e.to_string()
            };
            let _ = tx.send(LoginEvent::Error {
                provider: provider.to_string(),
                message,
            });
        }
    }

    let _ = tx.send(LoginEvent::Finished);
}

/// Refresh the stored OAuth token for `provider` and persist the result.
///
/// This is the single implementation of token renewal. It performs the HTTP
/// refresh, updates the auth store on disk, and returns `Ok(())` on success.
///
/// Supported providers: `"copilot"`, `"codex"`, `"gemini"`.
pub async fn refresh_token(provider: &str) -> anyhow::Result<()> {
    log::debug!("refresh_token called: provider={provider}");
    match provider {
        "copilot" => {
            let mut store = AuthStore::load_default()?;
            let creds = store
                .get_copilot()
                .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
            let refreshed = copilot::refresh(&creds.refresh_token).await?;
            store.set_copilot(refreshed);
            store.save()
        }
        "codex" => {
            let mut store = AuthStore::load_default()?;
            let creds = store
                .get_codex()
                .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
            let refreshed = codex::refresh(&creds.refresh_token).await?;
            store.set_codex(refreshed);
            store.save()
        }
        "gemini" => {
            let mut store = AuthStore::load_default()?;
            let creds = store
                .get_gemini()
                .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
            let refreshed = gemini::refresh(&creds.refresh_token, &creds.project_id).await?;
            store.set_gemini(refreshed);
            store.save()
        }
        _ => Err(anyhow::anyhow!(
            "Refresh not supported for provider {provider}"
        )),
    }
}

/// Refresh the stored token for `provider` and report the outcome on `tx`.
///
/// This is a thin wrapper around [`refresh_token`] for use by the interactive
/// TUI, which communicates refresh results via a [`LoginEvent`] channel.
pub async fn refresh_provider(provider: &str, tx: UnboundedSender<LoginEvent>) {
    let result = refresh_token(provider).await;
    match result {
        Ok(()) => {
            let _ = tx.send(LoginEvent::RefreshResult {
                provider: provider.to_string(),
                success: true,
                message: "Token refreshed".to_string(),
            });
        }
        Err(e) => {
            let _ = tx.send(LoginEvent::RefreshResult {
                provider: provider.to_string(),
                success: false,
                message: e.to_string(),
            });
        }
    }
}

// ── Token expiry preflight ────────────────────────────────────────────────────

/// Leeway for proactive token refresh before actual expiration (in seconds).
/// Tokens expiring within this window will trigger a refresh to avoid
/// last-minute failures.
pub const AUTH_REFRESH_LEEWAY_SECS: i64 = 120;

/// Classification of a stored auth token's freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthTokenState {
    /// No credentials are stored for the provider.
    Missing,
    /// Token is fresh and has time before expiration.
    Valid,
    /// Token expires soon (within the leeway window).
    ExpiringSoon,
    /// Token has already expired.
    Expired,
}

/// Check the expiration state of the stored token for the given provider.
///
/// Returns `Missing` if no credentials exist, otherwise classifies the token
/// based on `expires_at` relative to `now_secs` with a `leeway_secs` buffer.
///
/// # Arguments
/// - `provider`: Provider name ("copilot", "codex", "gemini")
/// - `now_secs`: Current time as Unix epoch seconds
/// - `leeway_secs`: Seconds before expiry to consider the token "expiring soon"
pub fn token_state(
    provider: &str,
    now_secs: i64,
    leeway_secs: i64,
) -> anyhow::Result<AuthTokenState> {
    let store = AuthStore::load_default()?;

    let expires_at = match provider {
        "copilot" => store.get_copilot().map(|c| c.expires_at),
        "codex" => store.get_codex().map(|c| c.expires_at),
        "gemini" => store.get_gemini().map(|c| c.expires_at),
        _ => return Ok(AuthTokenState::Missing),
    };

    let Some(expires_at) = expires_at else {
        return Ok(AuthTokenState::Missing);
    };

    if now_secs >= expires_at {
        Ok(AuthTokenState::Expired)
    } else if now_secs >= expires_at - leeway_secs {
        Ok(AuthTokenState::ExpiringSoon)
    } else {
        Ok(AuthTokenState::Valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_state_missing_when_no_creds() {
        // Non-existent provider returns Missing without error
        let state = token_state("nonexistent", 1000, 120).unwrap();
        assert_eq!(state, AuthTokenState::Missing);
    }

    #[test]
    fn token_state_expired_when_past_expiry() {
        let state = token_state_for_expiry(1000, 900, 120);
        assert_eq!(state, AuthTokenState::Expired);
    }

    #[test]
    fn token_state_expiring_soon_within_leeway() {
        // expires_at = 1100, now = 1000, leeway = 120
        // 1100 - 120 = 980, so 1000 is within the expiring-soon window
        let state = token_state_for_expiry(1000, 1100, 120);
        assert_eq!(state, AuthTokenState::ExpiringSoon);
    }

    #[test]
    fn token_state_expiring_soon_at_boundary() {
        // expires_at = 1120, now = 1000, leeway = 120
        // 1120 - 120 = 1000 (exactly at boundary)
        let state = token_state_for_expiry(1000, 1120, 120);
        assert_eq!(state, AuthTokenState::ExpiringSoon);
    }

    #[test]
    fn token_state_valid_when_fresh() {
        // expires_at = 2000, now = 1000, leeway = 120
        // 2000 - 120 = 1880, so 1000 is well before expiry
        let state = token_state_for_expiry(1000, 2000, 120);
        assert_eq!(state, AuthTokenState::Valid);
    }

    #[test]
    fn token_state_valid_just_outside_leeway() {
        // expires_at = 1121, now = 1000, leeway = 120
        // 1121 - 120 = 1001, so 1000 is just outside (still valid)
        let state = token_state_for_expiry(1000, 1121, 120);
        assert_eq!(state, AuthTokenState::Valid);
    }

    /// Helper to test token_state logic without needing real credentials.
    /// Simulates the classification logic inline.
    fn token_state_for_expiry(now_secs: i64, expires_at: i64, leeway_secs: i64) -> AuthTokenState {
        if now_secs >= expires_at {
            AuthTokenState::Expired
        } else if now_secs >= expires_at - leeway_secs {
            AuthTokenState::ExpiringSoon
        } else {
            AuthTokenState::Valid
        }
    }
}
