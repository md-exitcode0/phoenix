use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::thread;
use std::time::{Duration, Instant};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_AUTH_VALUE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct DeviceCodeConfig {
    pub client_id: String,
    pub user_code_url: String,
    pub token_poll_url: String,
    pub token_exchange_url: String,
    pub verification_url: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeTokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at_epoch_ms: i64,
}

#[derive(Deserialize)]
struct UserCodeResp {
    device_auth_id: Option<String>,
    user_code: Option<String>,
    usercode: Option<String>,
    interval: Option<i64>,
}

#[derive(Deserialize)]
struct PollResp {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

fn parse_json_bounded<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    label: &str,
) -> Result<T> {
    use std::io::Read;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label}"))
}

fn validate_value(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_AUTH_VALUE_BYTES
        || value
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        anyhow::bail!("{label} is empty, oversized, or contains control delimiters");
    }
    Ok(())
}

fn validate_config(cfg: &DeviceCodeConfig) -> Result<()> {
    validate_value(&cfg.client_id, "device-code client id")?;
    for (label, value) in [
        ("user-code URL", &cfg.user_code_url),
        ("token-poll URL", &cfg.token_poll_url),
        ("token-exchange URL", &cfg.token_exchange_url),
        ("verification URL", &cfg.verification_url),
        ("redirect URI", &cfg.redirect_uri),
    ] {
        if value.len() > 8 * 1024 {
            anyhow::bail!("{label} is oversized");
        }
        let parsed = url::Url::parse(value).with_context(|| format!("invalid {label}"))?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            anyhow::bail!("{label} must be an HTTPS URL");
        }
    }
    Ok(())
}

pub fn login_device_code(
    cfg: &DeviceCodeConfig,
    on_code: impl Fn(&str, &str),
    on_wait: impl Fn(),
) -> Result<DeviceCodeTokens> {
    validate_config(cfg)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build bounded device-code client")?;
    let user_resp = client
        .post(&cfg.user_code_url)
        .json(&serde_json::json!({ "client_id": cfg.client_id }))
        .send()
        .context("Device-code start failed")?;
    if !user_resp.status().is_success() {
        anyhow::bail!("Device-code start failed: HTTP {}", user_resp.status());
    }
    let u: UserCodeResp = parse_json_bounded(user_resp, "device-code start response")?;
    let device_auth_id = u
        .device_auth_id
        .filter(|s| !s.trim().is_empty())
        .context("Missing device_auth_id")?;
    let user_code = u
        .user_code
        .or(u.usercode)
        .filter(|s| !s.trim().is_empty())
        .context("Missing user_code")?;
    validate_value(&device_auth_id, "device authorization id")?;
    validate_value(&user_code, "device user code")?;
    let interval_secs = u64::try_from(u.interval.unwrap_or(5))
        .unwrap_or(5)
        .clamp(1, 30);
    on_code(&cfg.verification_url, &user_code);
    on_wait();

    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let mut auth_code = None;
    let mut code_verifier = None;
    while Instant::now() < deadline {
        let poll = client
            .post(&cfg.token_poll_url)
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .context("Device-code poll failed")?;
        if poll.status().is_success() {
            let p: PollResp = parse_json_bounded(poll, "device-code poll response")?;
            auth_code = p.authorization_code;
            code_verifier = p.code_verifier;
            if auth_code.is_some() && code_verifier.is_some() {
                break;
            }
        }
        thread::sleep(Duration::from_secs(interval_secs));
    }
    let auth_code = auth_code.context("Authorization timed out")?;
    let code_verifier = code_verifier.context("Missing code_verifier from device flow")?;
    validate_value(&auth_code, "device authorization code")?;
    validate_value(&code_verifier, "device code verifier")?;

    let ex = client
        .post(&cfg.token_exchange_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", auth_code.as_str()),
            ("redirect_uri", cfg.redirect_uri.as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("code_verifier", code_verifier.as_str()),
        ])
        .send()
        .context("Token exchange failed")?;
    if !ex.status().is_success() {
        anyhow::bail!("Token exchange failed: HTTP {}", ex.status());
    }
    let t: TokenResp = parse_json_bounded(ex, "device token exchange response")?;
    let access = t.access_token.context("Missing access_token")?;
    validate_value(&access, "device access token")?;
    if let Some(refresh) = t.refresh_token.as_deref() {
        validate_value(refresh, "device refresh token")?;
    }
    let expires_sec = t.expires_in.unwrap_or(3600).clamp(30, 30 * 24 * 60 * 60);
    let now = chrono::Utc::now().timestamp_millis();
    Ok(DeviceCodeTokens {
        access,
        refresh: t.refresh_token,
        expires_at_epoch_ms: now.saturating_add(expires_sec.saturating_mul(1000)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_config_requires_bounded_https_endpoints() {
        let mut config = DeviceCodeConfig {
            client_id: "client".into(),
            user_code_url: "https://auth.example/user".into(),
            token_poll_url: "https://auth.example/poll".into(),
            token_exchange_url: "https://auth.example/token".into(),
            verification_url: "https://auth.example/device".into(),
            redirect_uri: "https://auth.example/callback".into(),
        };
        validate_config(&config).unwrap();
        config.token_poll_url = "http://auth.example/poll".into();
        assert!(validate_config(&config).is_err());
        assert!(validate_value(&"x".repeat(MAX_AUTH_VALUE_BYTES + 1), "token").is_err());
        assert!(validate_value("line\nbreak", "token").is_err());
    }
}
