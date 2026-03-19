use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::auth::types::GeminiCredentials;

const CLIENT_ID: &str = "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REDIRECT_URI: &str = "http://localhost:8085/oauth2callback";
const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];

#[derive(Debug, Clone)]
pub enum GeminiLoginEvent {
    OpenBrowser(String),
    Progress(String),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
    #[serde(rename = "currentTier")]
    current_tier: Option<TierInfo>,
    #[serde(rename = "allowedTiers")]
    allowed_tiers: Option<Vec<TierInfo>>,
}

#[derive(Debug, Deserialize)]
struct TierInfo {
    id: Option<String>,
    #[serde(default, rename = "isDefault")]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
struct LongRunningOperation {
    name: Option<String>,
    done: Option<bool>,
    response: Option<OnboardOperationResponse>,
}

#[derive(Debug, Deserialize)]
struct OnboardOperationResponse {
    #[serde(rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<ProjectId>,
}

#[derive(Debug, Deserialize)]
struct ProjectId {
    id: Option<String>,
}

fn cancelled(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

pub async fn login(
    on_event: impl Fn(GeminiLoginEvent),
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<GeminiCredentials> {
    let verifier = random_urlsafe(32);
    let challenge = pkce_challenge(&verifier);
    let state = random_urlsafe(16);

    let mut url = reqwest::Url::parse(AUTH_URL)?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("client_id", CLIENT_ID);
        qp.append_pair("response_type", "code");
        qp.append_pair("redirect_uri", REDIRECT_URI);
        qp.append_pair("scope", &SCOPES.join(" "));
        qp.append_pair("code_challenge", &challenge);
        qp.append_pair("code_challenge_method", "S256");
        qp.append_pair("state", &state);
        qp.append_pair("access_type", "offline");
        qp.append_pair("prompt", "consent");
    }

    on_event(GeminiLoginEvent::OpenBrowser(url.to_string()));
    let _ = open_url(url.as_str());

    on_event(GeminiLoginEvent::Progress(
        "Waiting for browser callback…".to_string(),
    ));
    let code = wait_for_callback(&state, cancel.clone()).await?;

    on_event(GeminiLoginEvent::Progress(
        "Exchanging authorization code…".to_string(),
    ));
    let token = exchange_authorization_code(&code, &verifier).await?;
    let refresh_token = token
        .refresh_token
        .ok_or_else(|| anyhow::anyhow!("No refresh token received from Google OAuth"))?;

    on_event(GeminiLoginEvent::Progress(
        "Discovering Cloud Code Assist project…".to_string(),
    ));
    let project_id = discover_project(&token.access_token, cancel, &on_event).await?;

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    Ok(GeminiCredentials {
        access_token: token.access_token,
        refresh_token,
        // Match pi-mono behavior: subtract a 5-minute safety margin.
        expires_at: now_ms + token.expires_in * 1000 - 5 * 60 * 1000,
        project_id,
    })
}

pub async fn refresh(refresh_token: &str, project_id: &str) -> anyhow::Result<GeminiCredentials> {
    let client = reqwest::Client::new();
    let token_body = format!(
        "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(CLIENT_SECRET),
        urlencoding::encode(refresh_token)
    );

    let token: TokenResponse = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    Ok(GeminiCredentials {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at: now_ms + token.expires_in * 1000 - 5 * 60 * 1000,
        project_id: project_id.to_string(),
    })
}

async fn exchange_authorization_code(code: &str, verifier: &str) -> anyhow::Result<TokenResponse> {
    let client = reqwest::Client::new();
    let token_body = format!(
        "client_id={}&client_secret={}&code={}&grant_type=authorization_code&redirect_uri={}&code_verifier={}",
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(CLIENT_SECRET),
        urlencoding::encode(code),
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode(verifier),
    );

    Ok(client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn wait_for_callback(state: &str, cancel: Arc<AtomicBool>) -> anyhow::Result<String> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let listener = TcpListener::bind("127.0.0.1:8085").await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);

    loop {
        if cancelled(&cancel) {
            anyhow::bail!("Login cancelled");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for OAuth callback");
        }

        let accept = tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
        let (mut socket, _) = match accept {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => return Err(anyhow::anyhow!(e)),
            Err(_) => continue,
        };

        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            continue;
        }
        let req = String::from_utf8_lossy(&buf[..n]);
        let first_line = req.lines().next().unwrap_or_default();
        let path = first_line
            .strip_prefix("GET ")
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("");

        let parsed = reqwest::Url::parse(&format!("http://localhost{path}"));
        let mut code: Option<String> = None;
        let mut state_ok = false;
        if let Ok(url) = parsed {
            let got_state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.to_string());
            let got_code = url
                .query_pairs()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.to_string());
            state_ok = got_state.as_deref() == Some(state);
            code = got_code;
        }

        let (status, body) = if !state_ok {
            (
                "400 Bad Request",
                "Authentication failed: state mismatch. You can close this window.",
            )
        } else if code.is_none() {
            (
                "400 Bad Request",
                "Authentication failed: missing authorization code. You can close this window.",
            )
        } else {
            ("200 OK", "Authentication successful. Return to tau.")
        };

        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;

        if state_ok && let Some(c) = code {
            return Ok(c);
        }
    }
}

async fn discover_project(
    access_token: &str,
    cancel: Arc<AtomicBool>,
    on_event: &impl Fn(GeminiLoginEvent),
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let env_project_id = std::env::var("GOOGLE_CLOUD_PROJECT")
        .ok()
        .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT_ID").ok());

    let load_body = serde_json::json!({
        "cloudaicompanionProject": env_project_id,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
            "duetProject": env_project_id,
        }
    });

    let load: LoadCodeAssistResponse = client
        .post(format!("{CODE_ASSIST_ENDPOINT}/v1internal:loadCodeAssist"))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", "google-api-nodejs-client/9.15.1")
        .header("X-Goog-Api-Client", "gl-node/22.17.0")
        .json(&load_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if let Some(project) = load.cloudaicompanion_project
        && !project.trim().is_empty()
    {
        return Ok(project);
    }

    on_event(GeminiLoginEvent::Progress(
        "No existing Cloud Code project, onboarding…".to_string(),
    ));

    let chosen_tier = load
        .current_tier
        .as_ref()
        .and_then(|t| t.id.clone())
        .or_else(|| {
            load.allowed_tiers.as_ref().and_then(|tiers| {
                tiers
                    .iter()
                    .find(|t| t.is_default)
                    .and_then(|t| t.id.clone())
                    .or_else(|| tiers.first().and_then(|t| t.id.clone()))
            })
        })
        .unwrap_or_else(|| "legacy-tier".to_string());

    let onboard_body = serde_json::json!({
        "tierId": chosen_tier,
        "cloudaicompanionProject": env_project_id,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
            "duetProject": env_project_id,
        },
    });

    let onboard: LongRunningOperation = client
        .post(format!("{CODE_ASSIST_ENDPOINT}/v1internal:onboardUser"))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", "google-api-nodejs-client/9.15.1")
        .header("X-Goog-Api-Client", "gl-node/22.17.0")
        .json(&onboard_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if onboard.done.unwrap_or(false)
        && let Some(id) = onboard
            .response
            .and_then(|r| r.cloudaicompanion_project)
            .and_then(|p| p.id)
        && !id.trim().is_empty()
    {
        return Ok(id);
    }

    let name = onboard
        .name
        .ok_or_else(|| anyhow::anyhow!("onboardUser returned no operation name"))?;

    for attempt in 0..60u32 {
        if cancelled(&cancel) {
            anyhow::bail!("Login cancelled");
        }
        if attempt > 0 {
            on_event(GeminiLoginEvent::Progress(format!(
                "Waiting for project provisioning (attempt {})…",
                attempt + 1
            )));
        }

        let op: LongRunningOperation = client
            .get(format!("{CODE_ASSIST_ENDPOINT}/v1internal/{name}"))
            .bearer_auth(access_token)
            .header("Content-Type", "application/json")
            .header("User-Agent", "google-api-nodejs-client/9.15.1")
            .header("X-Goog-Api-Client", "gl-node/22.17.0")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if op.done.unwrap_or(false) {
            if let Some(id) = op
                .response
                .and_then(|r| r.cloudaicompanion_project)
                .and_then(|p| p.id)
                && !id.trim().is_empty()
            {
                return Ok(id);
            }
            break;
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    anyhow::bail!("Unable to discover/provision Cloud Code Assist project")
}

fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).expect("entropy unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{pkce_challenge, random_urlsafe};

    #[test]
    fn random_urlsafe_returns_urlsafe_text() {
        let value = random_urlsafe(24);
        assert_eq!(value.len(), 32);
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
    }

    #[test]
    fn pkce_challenge_is_urlsafe() {
        let challenge = pkce_challenge("hello-verifier");
        assert!(!challenge.is_empty());
        assert!(
            challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
    }
}
