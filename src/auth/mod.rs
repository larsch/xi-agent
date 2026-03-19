use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::mpsc::UnboundedSender;

pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod paths;
pub mod store;
pub mod types;

pub use store::AuthStore;

#[derive(Debug, Clone)]
pub enum LoginEvent {
    Info(String),
    AuthCode {
        url: String,
        code: Option<String>,
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
                        let _ = tx.send(LoginEvent::AuthCode { url, code: None });
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
                        let _ = tx.send(LoginEvent::AuthCode { url, code: None });
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

pub async fn refresh_provider(provider: &str, tx: UnboundedSender<LoginEvent>) {
    log::debug!("refresh_provider called: provider={provider}");
    let result: anyhow::Result<()> = match provider {
        "copilot" => {
            async {
                let mut store = AuthStore::load_default()?;
                let creds = store
                    .get_copilot()
                    .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
                let refreshed = copilot::refresh(&creds.refresh_token).await?;
                store.set_copilot(refreshed);
                store.save()
            }
            .await
        }
        "codex" => {
            async {
                let mut store = AuthStore::load_default()?;
                let creds = store
                    .get_codex()
                    .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
                let refreshed = codex::refresh(&creds.refresh_token).await?;
                store.set_codex(refreshed);
                store.save()
            }
            .await
        }
        "gemini" => {
            async {
                let mut store = AuthStore::load_default()?;
                let creds = store
                    .get_gemini()
                    .ok_or_else(|| anyhow::anyhow!("No stored credentials"))?;
                let refreshed = gemini::refresh(&creds.refresh_token, &creds.project_id).await?;
                store.set_gemini(refreshed);
                store.save()
            }
            .await
        }
        _ => Err(anyhow::anyhow!(
            "Refresh not supported for provider {provider}"
        )),
    };

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
