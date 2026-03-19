use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::Deserialize;

use crate::auth::types::CopilotCredentials;

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

#[derive(Debug, Clone)]
pub enum CopilotLoginEvent {
    DeviceCode {
        verification_uri: String,
        user_code: String,
    },
    Progress(String),
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: i64,
}

fn cancelled(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

pub async fn login(
    on_event: impl Fn(CopilotLoginEvent),
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<CopilotCredentials> {
    let client = reqwest::Client::new();

    let device_body = format!("client_id={CLIENT_ID}&scope=read%3Auser");
    log::debug!("→ POST https://github.com/login/device/code");
    let device: DeviceCodeResponse = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(device_body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    on_event(CopilotLoginEvent::DeviceCode {
        verification_uri: device.verification_uri.clone(),
        user_code: device.user_code.clone(),
    });
    log::debug!("copilot device flow: opening browser at {}", device.verification_uri);
    let _ = open_url(&device.verification_uri);
    log::debug!(
        "copilot device flow started: uri={} interval={} expires_in={}",
        device.verification_uri,
        device.interval,
        device.expires_in
    );

    let mut interval = device.interval.max(2);
    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);

    let github_access = loop {
        if cancelled(&cancel) {
            anyhow::bail!("Login cancelled");
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("GitHub device login timed out");
        }

        tokio::time::sleep(Duration::from_secs(interval)).await;

        let token_body = format!(
            "client_id={CLIENT_ID}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            urlencoding::encode(&device.device_code)
        );
        log::debug!("→ POST https://github.com/login/oauth/access_token");
        let token_resp: DeviceTokenResponse = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(token_body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if let Some(access_token) = token_resp.access_token {
            break access_token;
        }

        match token_resp.error.as_deref() {
            Some("authorization_pending") => {
                on_event(CopilotLoginEvent::Progress(
                    "Waiting for authorization…".to_string(),
                ));
            }
            Some("slow_down") => {
                interval = token_resp
                    .interval
                    .unwrap_or(interval + 5)
                    .max(interval + 1);
                on_event(CopilotLoginEvent::Progress(
                    "GitHub asked to slow down polling…".to_string(),
                ));
            }
            Some(err) => {
                log::debug!("copilot device flow failed: {}", err);
                anyhow::bail!("GitHub device flow failed: {err}")
            }
            None => anyhow::bail!("GitHub device flow failed without an error code"),
        }
    };

    log::debug!("→ GET https://api.github.com/copilot_internal/v2/token");
    let copilot: CopilotTokenResponse = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {github_access}"))
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let base_url = extract_base_url(&copilot.token);

    log::debug!("copilot login token exchange succeeded");
    Ok(CopilotCredentials {
        access_token: copilot.token,
        refresh_token: github_access,
        expires_at: copilot.expires_at * 1000,
        base_url,
    })
}

pub async fn refresh(refresh_token: &str) -> anyhow::Result<CopilotCredentials> {
    let client = reqwest::Client::new();
    log::debug!("→ GET https://api.github.com/copilot_internal/v2/token (refresh)");
    let copilot: CopilotTokenResponse = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {refresh_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    log::debug!("copilot token refresh succeeded");
    Ok(CopilotCredentials {
        base_url: extract_base_url(&copilot.token),
        access_token: copilot.token,
        refresh_token: refresh_token.to_string(),
        expires_at: copilot.expires_at * 1000,
    })
}

fn extract_base_url(token: &str) -> Option<String> {
    let domain = token
        .split(';')
        .find_map(|segment| segment.strip_prefix("proxy-ep="))?;

    let api_domain = domain
        .strip_prefix("proxy.")
        .map(|rest| format!("api.{rest}"))
        .unwrap_or_else(|| domain.to_string());
    Some(format!("https://{api_domain}"))
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
    use super::extract_base_url;

    #[test]
    fn extract_base_url_maps_proxy_prefix_to_api_subdomain() {
        let token = "abc;proxy-ep=proxy.business.githubcopilot.com;xyz";
        let base = extract_base_url(token);
        assert_eq!(
            base,
            Some("https://api.business.githubcopilot.com".to_string())
        );
    }

    #[test]
    fn extract_base_url_uses_domain_as_is_when_not_proxy_prefixed() {
        let token = "k=v;proxy-ep=enterprise.githubcopilot.com";
        let base = extract_base_url(token);
        assert_eq!(
            base,
            Some("https://enterprise.githubcopilot.com".to_string())
        );
    }

    #[test]
    fn extract_base_url_returns_none_when_segment_missing() {
        assert_eq!(extract_base_url("foo=bar;baz=qux"), None);
    }
}
