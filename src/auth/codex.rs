use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::auth::types::CodexCredentials;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

#[derive(Debug, Clone)]
pub enum CodexLoginEvent {
    OpenBrowser(String),
    Progress(String),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

fn cancelled(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

pub async fn login(
    on_event: impl Fn(CodexLoginEvent),
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<CodexCredentials> {
    let (verifier, challenge) = generate_pkce_pair();
    let state = random_urlsafe(16);

    let mut url = reqwest::Url::parse(AUTHORIZE_URL)?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("response_type", "code");
        qp.append_pair("client_id", CLIENT_ID);
        qp.append_pair("redirect_uri", REDIRECT_URI);
        qp.append_pair("scope", SCOPE);
        qp.append_pair("code_challenge", &challenge);
        qp.append_pair("code_challenge_method", "S256");
        qp.append_pair("state", &state);
        qp.append_pair("id_token_add_organizations", "true");
        qp.append_pair("codex_cli_simplified_flow", "true");
        qp.append_pair("originator", "tau");
    }

    on_event(CodexLoginEvent::OpenBrowser(url.to_string()));
    log::debug!("codex oauth open browser: {}", url);

    on_event(CodexLoginEvent::Progress(
        "Waiting for browser callback…".to_string(),
    ));
    let code = wait_for_callback(&state, cancel.clone()).await?;

    let client = reqwest::Client::new();
    let token_body = format!(
        "grant_type=authorization_code&client_id={}&code={}&code_verifier={}&redirect_uri={}",
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(&code),
        urlencoding::encode(&verifier),
        urlencoding::encode(REDIRECT_URI)
    );
    log::debug!("→ POST {TOKEN_URL} (authorization_code)");
    let token: TokenResponse = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    log::debug!("codex token exchange succeeded");

    let account_id = extract_account_id(&token.access_token)
        .ok_or_else(|| anyhow::anyhow!("Failed to extract account id from token"))?;

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    Ok(CodexCredentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: now_ms + token.expires_in * 1000,
        account_id,
    })
}

pub async fn refresh(refresh_token: &str) -> anyhow::Result<CodexCredentials> {
    let client = reqwest::Client::new();
    let token_body = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(refresh_token)
    );
    log::debug!("→ POST {TOKEN_URL} (refresh_token)");
    let token: TokenResponse = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    log::debug!("codex token refresh succeeded");

    let account_id = extract_account_id(&token.access_token)
        .ok_or_else(|| anyhow::anyhow!("Failed to extract account id from token"))?;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;

    Ok(CodexCredentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: now_ms + token.expires_in * 1000,
        account_id,
    })
}

async fn wait_for_callback(state: &str, cancel: Arc<AtomicBool>) -> anyhow::Result<String> {
    let addr: SocketAddr = "127.0.0.1:1455".parse()?;
    let listener = TcpListener::bind(addr).await.map_err(|e| {
        log::debug!("codex oauth callback bind failed: {}", e);
        anyhow::anyhow!(e)
    })?;
    log::debug!("codex oauth callback listener bound on 127.0.0.1:1455");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);

    loop {
        if cancelled(&cancel) {
            anyhow::bail!("Login cancelled");
        }
        if tokio::time::Instant::now() >= deadline {
            log::debug!("codex oauth callback timed out");
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
            log::debug!("codex oauth callback state mismatch");
            ("400 Bad Request", "State mismatch")
        } else if code.is_none() {
            log::debug!("codex oauth callback missing authorization code");
            ("400 Bad Request", "Missing authorization code")
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
            log::debug!("codex oauth callback received authorization code");
            return Ok(c);
        }
    }
}

fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).expect("entropy unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_pkce_pair() -> (String, String) {
    let verifier = random_urlsafe(32);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn extract_account_id(access_token: &str) -> Option<String> {
    let payload_b64 = access_token.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload
        .get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::{extract_account_id, generate_pkce_pair, random_urlsafe};

    fn jwt_with_payload(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn extract_account_id_from_valid_token_payload() {
        let token = jwt_with_payload(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}"#,
        );

        let account = extract_account_id(&token);
        assert_eq!(account, Some("acct_123".to_string()));
    }

    #[test]
    fn extract_account_id_returns_none_for_missing_claim_or_invalid_token() {
        let missing_claim = jwt_with_payload(r#"{"sub":"u1"}"#);
        assert_eq!(extract_account_id(&missing_claim), None);

        assert_eq!(extract_account_id("not-a-jwt"), None);
    }

    #[test]
    fn random_urlsafe_returns_requested_length_and_charset() {
        let value = random_urlsafe(24);
        assert_eq!(value.len(), 32);
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
    }

    #[test]
    fn generate_pkce_pair_produces_urlsafe_values() {
        let (verifier, challenge) = generate_pkce_pair();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert_ne!(verifier, challenge);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
        assert!(
            challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
    }
}
