//! Provider pickers and every auth flow: API key, device-code (GitHub/MiniMax),
//! OAuth (Codex/Gemini/Chutes), stored-auth reuse, imported tokens.

use super::*;

const AUTH_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const AUTH_HTTP_BODY_MAX_BYTES: u64 = 1024 * 1024;
const AUTH_SECRET_MAX_BYTES: usize = 64 * 1024;

pub(super) fn auth_http_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(AUTH_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build bounded authentication client")
}

pub(super) fn parse_auth_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
    label: &str,
) -> Result<T> {
    use std::io::Read;

    if response
        .content_length()
        .is_some_and(|length| length > AUTH_HTTP_BODY_MAX_BYTES)
    {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    let mut body = Vec::new();
    response
        .take(AUTH_HTTP_BODY_MAX_BYTES + 1)
        .read_to_end(&mut body)
        .with_context(|| format!("failed to read {label}"))?;
    if body.len() as u64 > AUTH_HTTP_BODY_MAX_BYTES {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    serde_json::from_slice(&body).with_context(|| format!("invalid {label}"))
}

fn validate_auth_secret(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > AUTH_SECRET_MAX_BYTES
        || value
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        anyhow::bail!("{label} is empty, oversized, or contains control delimiters");
    }
    Ok(())
}

fn auth_secret_from_env(name: &str, allow_reuse: bool) -> Result<Option<String>> {
    if !allow_reuse {
        return Ok(None);
    }
    match std::env::var(name) {
        Ok(value) => {
            validate_auth_secret(&value, &format!("environment variable {name}"))?;
            Ok(Some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("environment variable {name} is not valid UTF-8")
        }
    }
}

fn oauth_expiry_ms(expires_in: Option<i64>) -> i64 {
    let seconds = expires_in.unwrap_or(3600).clamp(30, 30 * 24 * 60 * 60);
    chrono::Utc::now()
        .timestamp_millis()
        .saturating_add(seconds.saturating_mul(1000))
}

pub(super) fn pick_provider_flat(theme: &ColorfulTheme) -> Result<ProviderModels> {
    let mut all = providers_data::all_providers();
    all.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    // Ollama ships as two providers behind the scenes — Local (this machine,
    // no auth) and Cloud (the user's Ollama Max plan, API key). They share one
    // name a user cares about ("Ollama"), so collapse them into a single flat
    // entry and branch into Local vs Cloud on selection. The user never has to
    // know the `ollama-cloud` provider ID exists.
    let entries: Vec<&ProviderModels> = all.iter().filter(|p| p.id != "ollama-cloud").collect();
    let names: Vec<&str> = entries.iter().map(|p| p.name).collect();
    let idx = FuzzySelect::with_theme(theme)
        .with_prompt("  Provider (type to filter)")
        .items(&names)
        .default(0)
        .interact()
        .context("Provider selection cancelled")?;
    let picked = entries[idx];
    if picked.id == "ollama" {
        return pick_ollama_variant(theme);
    }
    providers_data::get_provider(picked.id).context("Invalid provider selection")
}

/// The Ollama branch: Local (no key) vs Cloud (paste the Ollama Max API key
/// once, reuse it after). Both feed into the same downstream auth + model
/// pickers; this only decides which provider ID runs.
fn pick_ollama_variant(theme: &ColorfulTheme) -> Result<ProviderModels> {
    let variants = [
        "Ollama Local — runs on this machine, no API key",
        "Ollama Cloud — your Ollama Max plan, paste API key",
        "← Back to provider list",
    ];
    let idx = Select::with_theme(theme)
        .with_prompt("  Which Ollama?")
        .items(&variants)
        .default(0)
        .interact()
        .context("Ollama variant selection cancelled")?;
    match idx {
        0 => providers_data::get_provider("ollama").context("Missing Ollama local provider"),
        1 => providers_data::get_provider("ollama-cloud").context("Missing Ollama cloud provider"),
        _ => pick_provider_flat(theme),
    }
}

// Returns the chosen provider together with its auth method. The provider is
// returned (not just the method) because the "← Back to provider list" path lets
// the user switch providers mid-flow; the caller MUST use this provider, not the
// one it passed in, or provider/method desync (e.g. Anthropic provider + OpenAI
// Codex OAuth method → misrouted auth flow, wrong models written).
pub(super) fn pick_provider_detail_and_auth(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    auth_override: Option<&str>,
) -> Result<(ProviderModels, AuthMethod)> {
    let supported_methods = provider.supported_auth_methods();
    if supported_methods.is_empty() {
        anyhow::bail!(
            "Provider {} has no LLM auth methods currently supported by Phoenix runtime.",
            provider.id
        );
    }

    if let Some(wanted) = auth_override {
        let normalized = wanted.trim().to_ascii_lowercase();
        let method = provider.find_auth_method(&normalized).with_context(|| {
            format!("Auth method '{}' not supported for {}", wanted, provider.id)
        })?;
        if !method.llm_supported {
            anyhow::bail!(
                "Auth method '{}' is currently disabled for provider {}: {}",
                method.label,
                provider.id,
                method
                    .unsupported_reason
                    .unwrap_or("not supported by Phoenix runtime for LLM requests")
            );
        }
        let method = method.clone();
        return Ok((provider.clone(), method));
    }

    println!();
    println!("  ── {} ──", provider.name);
    println!("  {}", provider.base_url);
    println!();
    let mut items = vec!["← Back to provider list".to_string()];
    items.extend(supported_methods.iter().map(|m| {
        let env = m.env_var.map(|v| format!(" ({v})")).unwrap_or_default();
        format!("{}{}", m.label, env)
    }));
    let idx = Select::with_theme(theme)
        .with_prompt("  Auth methods")
        .items(&items)
        .default(1.min(items.len().saturating_sub(1)))
        .interact()
        .context("Auth selection cancelled")?;
    if idx == 0 {
        let p = pick_provider_flat(theme)?;
        return pick_provider_detail_and_auth(theme, &p, auth_override);
    }
    Ok((provider.clone(), supported_methods[idx - 1].clone()))
}

/// Any usable stored credential for this provider (profiles are keyed
/// `provider:variant` — codex uses `:default`, MiniMax `:global`/`:cn` — so
/// match on the prefix, never assume `:default`).
pub(super) fn try_stored_auth(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
) -> Result<Option<SetupAuthSelection>> {
    let store = load_auth_profile_store().context("failed to load stored authorization")?;
    let prefix = format!("{}:", provider.id);
    let mut usable: Vec<(&String, &AuthProfileCredential)> = store
        .profiles
        .iter()
        .filter(|(id, cred)| id.starts_with(&prefix) && stored_credential_usable(cred))
        .collect();
    usable.sort_by(|a, b| a.0.cmp(b.0));
    let Some((profile_id, credential)) = usable.first() else {
        return Ok(None);
    };
    let reuse = Confirm::with_theme(theme)
        .with_prompt(format!(
            "  {} is already authorized on this machine ({profile_id}) — use the stored login?",
            provider.name
        ))
        .default(true)
        .interact()
        .context("Stored-auth confirmation cancelled")?;
    if !reuse {
        return Ok(None);
    }
    println!("  ✓ Using stored authorization ({profile_id})");
    let method = match credential {
        AuthProfileCredential::OAuth { .. } => "oauth",
        AuthProfileCredential::Token { .. } => "token",
        _ => "api",
    };
    Ok(Some(SetupAuthSelection {
        method: method.to_string(),
        source: "profile".to_string(),
        profile: Some((*profile_id).clone()),
        env_var: None,
    }))
}

pub(super) fn run_auth_flow(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    method: &AuthMethod,
) -> Result<SetupAuthSelection> {
    run_auth_flow_inner(theme, provider, method, true)
}

/// Auth flow for adding an ADDITIONAL account: never offers the stored login
/// or an env key (those ARE the existing account — reusing them here would
/// silently create a "fallback" that is the same account twice, adding zero
/// quota). Always logs in / pastes fresh.
pub(super) fn run_auth_flow_fresh(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    method: &AuthMethod,
) -> Result<SetupAuthSelection> {
    run_auth_flow_inner(theme, provider, method, false)
}

fn run_auth_flow_inner(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    method: &AuthMethod,
    allow_reuse: bool,
) -> Result<SetupAuthSelection> {
    // A valid stored authorization always wins over a fresh login dance —
    // covers OAuth (codex), device-code (minimax/copilot), keys alike.
    if allow_reuse && method.method_type != "none" {
        if let Some(stored) = try_stored_auth(theme, provider)? {
            return Ok(stored);
        }
    }
    match method.method_type {
        "api" => run_api_key_flow(theme, provider, method, allow_reuse),
        method_type if method_type.starts_with("device_code") => {
            run_device_code_flow(theme, provider, method)
        }
        "oauth" => run_oauth_placeholder_flow(provider, method, allow_reuse),
        "none" => Ok(SetupAuthSelection {
            method: "none".to_string(),
            source: "none".to_string(),
            profile: None,
            env_var: None,
        }),
        other => anyhow::bail!(
            "authentication method `{other}` is not implemented for provider {}",
            provider.id
        ),
    }
}

pub(super) fn run_api_key_flow(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    method: &AuthMethod,
    allow_reuse: bool,
) -> Result<SetupAuthSelection> {
    let env = method.env_var.unwrap_or("API_KEY");
    let existing = auth_secret_from_env(env, allow_reuse)?;
    if existing.is_some() {
        println!("  Detected env var {env} (value hidden)");
    }
    let use_existing = existing.is_some()
        && Confirm::with_theme(theme)
            .with_prompt(format!("Use existing {} from environment?", env))
            .default(true)
            .interact()
            .context("Prompt cancelled")?;
    if use_existing {
        return Ok(SetupAuthSelection {
            method: "api".to_string(),
            source: "env".to_string(),
            profile: None,
            env_var: Some(env.to_string()),
        });
    }
    let key = Input::<String>::with_theme(theme)
        .with_prompt(format!("Paste {} value", env))
        .allow_empty(false)
        .interact_text()
        .context("API key input cancelled")?;
    let key = key.trim().to_string();
    validate_auth_secret(&key, "API key")?;

    // Prove the key WORKS before storing it. A silently-stored bad key looks
    // like a clean setup and then kills every turn — Phoenix has shipped that
    // bug twice. See providers/validate.rs for why this is a real completion
    // and not a catalog fetch.
    if !confirm_key_is_live(theme, provider, &key)? {
        anyhow::bail!("API key not stored");
    }

    let (path, profile_id) = update_auth_profile_store(|store| {
        // Adding an ADDITIONAL account must never land on the id the existing
        // one occupies. Choose the slot under the same lock as the insertion.
        let profile_id = if allow_reuse {
            format!("{}:default", provider.id)
        } else {
            crate::config::auth_profile::next_free_profile_id_in(store, provider.id)
        };
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::ApiKey {
                provider: provider.id.to_string(),
                key,
                display_name: Some(format!("{} API key", provider.name)),
            },
        );
        Ok(profile_id)
    })?;
    println!(
        "  Stored API key as `{profile_id}` in {} (plaintext, local machine store)",
        path.display()
    );
    Ok(SetupAuthSelection {
        method: "api".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

/// Live-check a pasted key and report honestly. Returns whether to store it.
///
/// - verified  → store, say so.
/// - REJECTED  → the provider itself refused the credential/account. Storing
///   it would only produce a profile that dies on its first turn, so the
///   default is not to; the user may still override (a provider can be wrong).
/// - unverified→ the check could not run (offline, or a provider with no live
///   check yet). Store it, but never claim it was verified.
fn confirm_key_is_live(
    theme: &ColorfulTheme,
    provider: &ProviderModels,
    key: &str,
) -> Result<bool> {
    use crate::providers::validate::{validate_api_key, KeyCheck};
    println!(
        "  {}",
        style("checking the key against the provider…").dim()
    );
    match validate_api_key(provider, key, None) {
        KeyCheck::Works { model } => {
            println!(
                "  {} key verified — {model} answered a live call",
                style("✔").green()
            );
            Ok(true)
        }
        KeyCheck::Unknown { reason } => {
            println!(
                "  {} could not verify the key ({reason}) — storing it UNVERIFIED. \
                 Run `phoenix auth probe <profile-id>` once you're back online.",
                style("⚠").yellow()
            );
            Ok(true)
        }
        KeyCheck::Rejected { model, reason } => {
            println!(
                "  {} {} rejected this key on {model}:\n    {reason}",
                style("✖").red(),
                provider.name
            );
            Confirm::with_theme(theme)
                .with_prompt("  Store it anyway (it will fail at the first turn)?")
                .default(false)
                .interact()
                .context("Rejected-key confirmation cancelled")
        }
    }
}

/// True when a stored credential can still authenticate: an OAuth grant that
/// has not expired (or carries a refresh token), or any API key/token.
pub(super) fn stored_credential_usable(credential: &AuthProfileCredential) -> bool {
    match credential {
        AuthProfileCredential::OAuth {
            refresh, expires, ..
        } => refresh.is_some() || *expires > chrono::Utc::now().timestamp_millis(),
        AuthProfileCredential::Token { expires, .. } => expires
            .map(|e| e > chrono::Utc::now().timestamp_millis())
            .unwrap_or(true),
        _ => true,
    }
}

pub(super) fn run_device_code_flow(
    _theme: &ColorfulTheme,
    provider: &ProviderModels,
    method: &AuthMethod,
) -> Result<SetupAuthSelection> {
    run_device_code_flow_as(provider, method, None, &device_code_notice)
}

pub(super) fn device_code_notice(url: &str, code: &str) {
    println!(
        "DEVICE_AUTH={}",
        serde_json::json!({"verification_url": url, "user_code": code})
    );
    println!("\n  If the browser doesn't open, visit:\n  {url}\n");
    println!("  Enter the code: {code}\n");
    let _ = open::that(url);
}

pub(super) fn run_device_code_flow_as(
    provider: &ProviderModels,
    method: &AuthMethod,
    profile_id: Option<&str>,
    on_code: &dyn Fn(&str, &str),
) -> Result<SetupAuthSelection> {
    let requested_profile_id = profile_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string());
    match provider.id {
        "openai-codex" => {
            let cfg = DeviceCodeConfig {
                client_id: crate::config::auth_profile::OPENAI_CODEX_OAUTH_CLIENT_ID.to_string(),
                user_code_url: "https://auth.openai.com/api/accounts/deviceauth/usercode"
                    .to_string(),
                token_poll_url: "https://auth.openai.com/api/accounts/deviceauth/token".to_string(),
                token_exchange_url: "https://auth.openai.com/oauth/token".to_string(),
                verification_url: "https://auth.openai.com/codex/device".to_string(),
                redirect_uri: "https://auth.openai.com/deviceauth/callback".to_string(),
            };
            let token =
                login_device_code(&cfg, on_code, || println!("  Waiting for authorization..."))?;
            let (path, profile_id) = update_auth_profile_store(|store| {
                let profile_id = requested_profile_id.unwrap_or_else(|| {
                    crate::config::auth_profile::next_free_profile_id_in(store, "openai-codex")
                });
                store.profiles.insert(
                    profile_id.clone(),
                    AuthProfileCredential::OAuth {
                        provider: "openai-codex".to_string(),
                        access: token.access,
                        refresh: token.refresh,
                        expires: token.expires_at_epoch_ms,
                        email: None,
                    },
                );
                Ok(profile_id)
            })?;
            println!("  ✓ Authorization successful! Saved to {}", path.display());
            Ok(SetupAuthSelection {
                method: "device_code".to_string(),
                source: "profile".to_string(),
                profile: Some(profile_id),
                env_var: None,
            })
        }
        "github-copilot" => {
            let token = run_github_device_flow(on_code)?;
            let (path, profile_id) = update_auth_profile_store(|store| {
                let profile_id = requested_profile_id.unwrap_or_else(|| {
                    crate::config::auth_profile::next_free_profile_id_in(store, "github-copilot")
                });
                store.profiles.insert(
                    profile_id.clone(),
                    AuthProfileCredential::Token {
                        provider: "github-copilot".to_string(),
                        token: token.access,
                        expires: Some(token.expires_at_epoch_ms),
                    },
                );
                Ok(profile_id)
            })?;
            println!("  ✓ Authorization successful! Saved to {}", path.display());
            Ok(SetupAuthSelection {
                method: "device_code".to_string(),
                source: "profile".to_string(),
                profile: Some(profile_id),
                env_var: None,
            })
        }
        "minimax-portal" => {
            let region = if method.method_type == "device_code_cn" {
                "cn"
            } else {
                "global"
            };
            run_minimax_device_flow_as(region, requested_profile_id.as_deref(), on_code)
        }
        _ => anyhow::bail!("Device-code flow is not implemented for {}", provider.id),
    }
}

#[derive(Deserialize)]
pub(super) struct GitHubDeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: i64,
}

#[derive(Deserialize)]
pub(super) struct GitHubTokenResp {
    access_token: Option<String>,
    error: Option<String>,
}

pub(super) fn run_github_device_flow(
    on_code: &dyn Fn(&str, &str),
) -> Result<crate::auth::device_code::DeviceCodeTokens> {
    const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
    let client = auth_http_client()?;
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", CLIENT_ID), ("scope", "read:user")])
        .send()
        .context("GitHub device-code request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub device-code request failed: HTTP {}", resp.status());
    }
    let dc: GitHubDeviceCodeResp = parse_auth_json(resp, "GitHub device response")?;
    validate_auth_secret(&dc.device_code, "GitHub device code")?;
    validate_auth_secret(&dc.user_code, "GitHub user code")?;
    if dc.verification_uri.len() > 8 * 1024 {
        anyhow::bail!("GitHub verification URL is oversized");
    }
    on_code(&dc.verification_uri, &dc.user_code);
    println!("  Waiting for authorization...");

    let lifetime = u64::try_from(dc.expires_in)
        .ok()
        .filter(|seconds| *seconds > 0)
        .context("GitHub device response has an invalid expiry")?
        .min(30 * 60);
    let poll_interval = u64::try_from(dc.interval).unwrap_or(5).clamp(1, 30);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(lifetime);
    while std::time::Instant::now() < deadline {
        let tok = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", dc.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .context("GitHub device poll failed")?;
        if !tok.status().is_success() {
            anyhow::bail!("GitHub device poll failed: HTTP {}", tok.status());
        }
        let payload: GitHubTokenResp = parse_auth_json(tok, "GitHub token response")?;
        if let Some(t) = payload.access_token {
            validate_auth_secret(&t, "GitHub access token")?;
            let now = chrono::Utc::now().timestamp_millis();
            return Ok(crate::auth::device_code::DeviceCodeTokens {
                access: t,
                refresh: None,
                expires_at_epoch_ms: now + (365 * 24 * 60 * 60 * 1000),
            });
        }
        match payload.error.as_deref() {
            Some("authorization_pending") | Some("slow_down") | None => {
                std::thread::sleep(std::time::Duration::from_secs(poll_interval));
            }
            Some("access_denied") => anyhow::bail!("GitHub device login was denied"),
            Some("expired_token") => anyhow::bail!("GitHub device code expired"),
            Some(other) => anyhow::bail!("GitHub device flow error: {other}"),
        }
    }
    anyhow::bail!("GitHub device login timed out")
}

pub(super) fn run_oauth_placeholder_flow(
    provider: &ProviderModels,
    method: &AuthMethod,
    allow_reuse: bool,
) -> Result<SetupAuthSelection> {
    if provider.id == "openai-codex" {
        return run_openai_codex_oauth_flow();
    }
    if provider.id == "google-gemini-cli" {
        return run_google_gemini_cli_oauth_flow();
    }
    if provider.id == "chutes" {
        return run_chutes_oauth_flow();
    }
    if provider.id == "xai" {
        return run_xai_oauth_flow();
    }
    // SuperGrok CLI proxy: identical OIDC login as xai (same client + scopes),
    // but the token is stored under and used by the grok-cli provider, which
    // targets cli-chat-proxy.grok.com where the subscription is honored.
    if provider.id == "grok-cli" {
        return run_grok_oauth_flow("grok-cli");
    }
    run_imported_token_flow(provider, method, allow_reuse)
}

/// xAI Grok OAuth (SuperGrok / X Premium subscription) — PKCE authorization-
/// code flow against accounts.x.ai's OIDC endpoints, bearer used on the same
/// api.x.ai/v1 surface as API keys. Client id + loopback port match the
/// registered public grok-cli client (from the open pi-grok implementation);
/// the id lives beside the runtime refresh in auth_profile.rs.
use crate::config::auth_profile::XAI_OAUTH_CLIENT_ID;
const XAI_OAUTH_SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";

/// xAI publishes endpoints via OIDC discovery; fall back to the standard
/// paths if the discovery document is unreachable.
fn xai_oauth_endpoints(client: &Client) -> (String, String) {
    let fallback_auth = "https://auth.x.ai/authorize".to_string();
    let fallback_token = "https://auth.x.ai/oauth/token".to_string();
    let disc = client
        .get("https://auth.x.ai/.well-known/openid-configuration")
        .send()
        .ok()
        .filter(|r| r.status().is_success())
        .and_then(|r| parse_auth_json::<serde_json::Value>(r, "xAI OIDC discovery").ok());
    let trusted = |value: &serde_json::Value, key: &str, fallback: String| {
        value
            .get(key)
            .and_then(|entry| entry.as_str())
            .filter(|endpoint| endpoint.len() <= 8 * 1024)
            .and_then(|endpoint| Url::parse(endpoint).ok())
            .filter(|endpoint| {
                endpoint.scheme() == "https"
                    && endpoint
                        .host_str()
                        .is_some_and(|host| host == "x.ai" || host.ends_with(".x.ai"))
            })
            .map(|endpoint| endpoint.to_string())
            .unwrap_or(fallback)
    };
    match disc {
        Some(value) => (
            trusted(&value, "authorization_endpoint", fallback_auth),
            trusted(&value, "token_endpoint", fallback_token),
        ),
        None => (fallback_auth, fallback_token),
    }
}

pub(super) fn run_xai_oauth_flow() -> Result<SetupAuthSelection> {
    run_grok_oauth_flow("xai")
}

/// The next free profile id for a provider: `<provider>:default`, then
/// `<provider>:2`, `:3`, … so several subscriptions of the SAME provider (e.g.
/// five SuperGrok accounts) can live side by side instead of overwriting each
/// other. Used whenever a login/key is added without an explicit id.
pub fn next_free_profile_id(provider_id: &str) -> Result<String> {
    let store = load_auth_profile_store()?;
    let base = format!("{provider_id}:default");
    if !store.profiles.contains_key(&base) {
        return Ok(base);
    }
    for n in 2..1000 {
        let candidate = format!("{provider_id}:{n}");
        if !store.profiles.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("too many profiles for '{provider_id}'")
}

/// The shared xAI OIDC PKCE login. `provider_id` selects which lane the token
/// serves: `xai` (console api.x.ai surface) or `grok-cli` (the SuperGrok CLI
/// proxy). The browser flow, client id, scopes, and endpoints are identical —
/// only the stored profile id + provider tag differ, so the runtime builds the
/// matching client and the refresh path (auth_profile.rs) rotates it.
pub(super) fn run_grok_oauth_flow(provider_id: &str) -> Result<SetupAuthSelection> {
    run_grok_oauth_flow_as(provider_id, None)
}

/// Same flow, but the caller may name the profile it lands in — that's what
/// lets a second (third, fourth…) SuperGrok subscription be added instead of
/// clobbering `<provider>:default`.
pub(super) fn run_grok_oauth_flow_as(
    provider_id: &str,
    profile_id: Option<&str>,
) -> Result<SetupAuthSelection> {
    let state = crate::auth::pkce::random_verifier(32);
    let verifier = crate::auth::pkce::random_verifier(64);
    let challenge = crate::auth::pkce::code_challenge_s256(&verifier);
    // The registered loopback redirect for this public client.
    let redirect_uri = "http://127.0.0.1:56121/callback";
    let client = auth_http_client()?;
    let (authorize_endpoint, token_endpoint) = xai_oauth_endpoints(&client);
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        authorize_endpoint,
        urlencoding::encode(XAI_OAUTH_CLIENT_ID),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(XAI_OAUTH_SCOPES),
        urlencoding::encode(&challenge),
        urlencoding::encode(&state)
    );

    println!("\n  Opening browser for xAI authorization (SuperGrok / X Premium account)...\n");
    println!("AUTH_URL={auth_url}"); // machine-readable marker for GUI clients
    println!("  If the browser doesn't open, visit:\n  {auth_url}\n");
    println!("  Listening for callback on {redirect_uri}");
    open_auth_url(&auth_url)?;
    println!("  Waiting for authorization...");

    let (code, returned_state) = match crate::auth::oauth_server::wait_for_oauth_code_on(
        redirect_uri,
        std::time::Duration::from_secs(180),
    ) {
        Ok(pair) => pair,
        Err(_) => {
            let raw = Input::<String>::new()
                .with_prompt("  Paste the authorization code (or full redirect URL)")
                .allow_empty(false)
                .interact_text()
                .context("OAuth input cancelled")?;
            if raw.starts_with("http://") || raw.starts_with("https://") {
                let parsed = Url::parse(&raw).context("Invalid redirect URL")?;
                let code = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "code").then(|| v.to_string()))
                    .context("Missing code in redirect URL")?;
                let st = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "state").then(|| v.to_string()))
                    .unwrap_or_default();
                (code, st)
            } else {
                (raw, state.clone())
            }
        }
    };

    if returned_state != state {
        anyhow::bail!("OAuth state mismatch");
    }
    validate_auth_secret(&code, "xAI authorization code")?;

    let resp = client
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri),
            ("client_id", XAI_OAUTH_CLIENT_ID),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .context("xAI OAuth token exchange failed")?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "xAI OAuth token exchange failed: HTTP {} — xAI gates OAuth API access by plan tier; \
             if this persists on an active SuperGrok sub, use an XAI_API_KEY from console.x.ai instead",
            resp.status()
        );
    }
    let token: OAuthTokenExchangeResp = parse_auth_json(resp, "xAI OAuth token response")?;
    let access = token.access_token.context("Missing access_token")?;
    validate_auth_secret(&access, "xAI access token")?;
    if let Some(refresh) = token.refresh_token.as_deref() {
        validate_auth_secret(refresh, "xAI refresh token")?;
    }
    let expires = oauth_expiry_ms(token.expires_in);

    // An explicit id (from `phoenix login <provider> --profile <id>`) lands the
    // token in ITS OWN slot; without one we take the next free slot, so adding
    // another account of the same provider never overwrites the first.
    let requested_profile_id = profile_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string());
    let (path, profile_id) = update_auth_profile_store(|store| {
        let profile_id = requested_profile_id.unwrap_or_else(|| {
            crate::config::auth_profile::next_free_profile_id_in(store, provider_id)
        });
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::OAuth {
                provider: provider_id.to_string(),
                access,
                refresh: token.refresh_token,
                expires,
                email: None,
            },
        );
        Ok(profile_id)
    })?;
    println!("  ✓ Authorization successful! Saved to {}", path.display());
    Ok(SetupAuthSelection {
        method: "oauth".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

#[derive(Deserialize)]
pub(super) struct OAuthTokenExchangeResp {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

pub(super) fn run_openai_codex_oauth_flow() -> Result<SetupAuthSelection> {
    run_openai_codex_oauth_flow_as(None)
}

/// OpenAI Codex OAuth login into an explicit profile, or the next unused
/// profile when called by the headless Canvas flow. A second login must add an
/// account instead of replacing `openai-codex:default`.
pub(super) fn run_openai_codex_oauth_flow_as(
    profile_id: Option<&str>,
) -> Result<SetupAuthSelection> {
    let state = crate::auth::pkce::random_verifier(32);
    let verifier = crate::auth::pkce::random_verifier(64);
    let challenge = crate::auth::pkce::code_challenge_s256(&verifier);
    let redirect_uri = "http://localhost:1455/auth/callback";
    let auth_url = format!(
        "https://auth.openai.com/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope=openid%20profile%20email%20offline_access&code_challenge={}&code_challenge_method=S256&state={}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=phoenix",
        urlencoding::encode(crate::config::auth_profile::OPENAI_CODEX_OAUTH_CLIENT_ID),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&challenge),
        urlencoding::encode(&state)
    );

    println!("\n  Opening browser for authorization...\n");
    println!("AUTH_URL={auth_url}"); // machine-readable marker for GUI clients
    println!("  If the browser doesn't open, visit:\n  {auth_url}\n");
    println!("  Listening for callback on {redirect_uri}");
    open_auth_url(&auth_url)?;
    println!("  Waiting for authorization...");

    let (code, returned_state) = match crate::auth::oauth_server::wait_for_oauth_code_on(
        redirect_uri,
        std::time::Duration::from_secs(180),
    ) {
        Ok(pair) => pair,
        Err(_) => {
            let raw = Input::<String>::new()
                .with_prompt("  Paste the authorization code (or full redirect URL)")
                .allow_empty(false)
                .interact_text()
                .context("OAuth input cancelled")?;

            if raw.starts_with("http://") || raw.starts_with("https://") {
                let parsed = Url::parse(&raw).context("Invalid redirect URL")?;
                let code = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "code").then(|| v.to_string()))
                    .context("Missing code in redirect URL")?;
                let st = parsed
                    .query_pairs()
                    .find_map(|(k, v)| (k == "state").then(|| v.to_string()))
                    .unwrap_or_default();
                (code, st)
            } else {
                (raw, state.clone())
            }
        }
    };

    if returned_state != state {
        anyhow::bail!("OAuth state mismatch");
    }
    validate_auth_secret(&code, "OpenAI authorization code")?;

    let client = auth_http_client()?;
    let resp = client
        .post("https://auth.openai.com/oauth/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri),
            (
                "client_id",
                crate::config::auth_profile::OPENAI_CODEX_OAUTH_CLIENT_ID,
            ),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .context("OpenAI OAuth token exchange failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("OpenAI OAuth token exchange failed: HTTP {}", resp.status());
    }
    let token: OAuthTokenExchangeResp = parse_auth_json(resp, "OpenAI OAuth token response")?;
    let access = token.access_token.context("Missing access_token")?;
    validate_auth_secret(&access, "OpenAI access token")?;
    if let Some(refresh) = token.refresh_token.as_deref() {
        validate_auth_secret(refresh, "OpenAI refresh token")?;
    }
    let expires = oauth_expiry_ms(token.expires_in);

    let requested_profile_id = profile_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string());
    let (path, profile_id) = update_auth_profile_store(|store| {
        let profile_id = requested_profile_id.unwrap_or_else(|| {
            crate::config::auth_profile::next_free_profile_id_in(store, "openai-codex")
        });
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::OAuth {
                provider: "openai-codex".to_string(),
                access,
                refresh: token.refresh_token,
                expires,
                email: None,
            },
        );
        Ok(profile_id)
    })?;
    println!("  ✓ Authorization successful! Saved to {}", path.display());
    Ok(SetupAuthSelection {
        method: "oauth".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

pub(super) fn run_google_gemini_cli_oauth_flow() -> Result<SetupAuthSelection> {
    run_google_gemini_cli_oauth_flow_as(None)
}

pub(super) fn run_google_gemini_cli_oauth_flow_as(
    profile_id: Option<&str>,
) -> Result<SetupAuthSelection> {
    let client_id = match read_env_first(&[
        "OPENCLAW_GEMINI_OAUTH_CLIENT_ID",
        "GEMINI_CLI_OAUTH_CLIENT_ID",
    ]) {
        Some(value) => value,
        None => Input::<String>::new()
            .with_prompt("  Enter Gemini CLI OAuth client id")
            .allow_empty(false)
            .interact_text()
            .context("Gemini client id input cancelled")?,
    };
    let client_secret = read_env_first(&[
        "OPENCLAW_GEMINI_OAUTH_CLIENT_SECRET",
        "GEMINI_CLI_OAUTH_CLIENT_SECRET",
    ]);
    let redirect_uri = "http://localhost:8085/oauth2callback";
    let state = crate::auth::pkce::random_verifier(32);
    let verifier = crate::auth::pkce::random_verifier(64);
    let challenge = crate::auth::pkce::code_challenge_s256(&verifier);
    let scope = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
    ]
    .join(" ");
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
        urlencoding::encode(&client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&scope),
        urlencoding::encode(&challenge),
        urlencoding::encode(&state),
    );

    println!("\n  Opening browser for Gemini CLI authorization...\n");
    println!("AUTH_URL={auth_url}");
    println!("  If the browser doesn't open, visit:\n  {auth_url}\n");
    println!("  Listening for callback on {redirect_uri}");
    open_auth_url(&auth_url)?;
    println!("  Waiting for authorization...");

    let (code, returned_state) = wait_or_prompt_for_oauth_code(redirect_uri, &state, 300)?;
    if returned_state != state {
        anyhow::bail!("OAuth state mismatch");
    }
    validate_auth_secret(&client_id, "Gemini client id")?;
    if let Some(secret) = client_secret.as_deref() {
        validate_auth_secret(secret, "Gemini client secret")?;
    }
    validate_auth_secret(&code, "Gemini authorization code")?;

    let client = auth_http_client()?;
    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("code", code.as_str()),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(secret) = client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .header("Accept", "*/*")
        .header("User-Agent", "google-api-nodejs-client/9.15.1")
        .form(&form)
        .send()
        .context("Gemini CLI OAuth token exchange failed")?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "Gemini CLI OAuth token exchange failed: HTTP {}",
            resp.status()
        );
    }
    let token: OAuthTokenExchangeResp = parse_auth_json(resp, "Gemini CLI OAuth token response")?;
    let access = token.access_token.context("Missing access_token")?;
    let refresh = token.refresh_token.context("Missing refresh_token")?;
    validate_auth_secret(&access, "Gemini access token")?;
    validate_auth_secret(&refresh, "Gemini refresh token")?;
    let expires = oauth_expiry_ms(token.expires_in);
    let email = fetch_google_email(&access).ok().flatten();

    let requested_profile_id = profile_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string());
    let (path, profile_id) = update_auth_profile_store(|store| {
        let profile_id = requested_profile_id.unwrap_or_else(|| {
            crate::config::auth_profile::next_free_profile_id_in(store, "google-gemini-cli")
        });
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::OAuth {
                provider: "google-gemini-cli".to_string(),
                access,
                refresh: Some(refresh),
                expires,
                email,
            },
        );
        Ok(profile_id)
    })?;
    println!("  ✓ Authorization successful! Saved to {}", path.display());
    Ok(SetupAuthSelection {
        method: "oauth".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

pub(super) fn run_chutes_oauth_flow() -> Result<SetupAuthSelection> {
    run_chutes_oauth_flow_as(None)
}

pub(super) fn run_chutes_oauth_flow_as(profile_id: Option<&str>) -> Result<SetupAuthSelection> {
    let client_id = match std::env::var("CHUTES_CLIENT_ID") {
        Ok(value) => value,
        Err(_) => Input::<String>::new()
            .with_prompt("  Enter Chutes OAuth client id")
            .allow_empty(false)
            .interact_text()
            .context("Chutes client id input cancelled")?,
    };
    let client_secret = std::env::var("CHUTES_CLIENT_SECRET").ok();
    let redirect_uri = std::env::var("CHUTES_OAUTH_REDIRECT_URI")
        .unwrap_or_else(|_| "http://127.0.0.1:1456/oauth-callback".to_string());
    let scopes = std::env::var("CHUTES_OAUTH_SCOPES")
        .unwrap_or_else(|_| "openid profile chutes:invoke".to_string());
    validate_auth_secret(&client_id, "Chutes client id")?;
    if let Some(secret) = client_secret.as_deref() {
        validate_auth_secret(secret, "Chutes client secret")?;
    }
    if redirect_uri.len() > 8 * 1024 || scopes.len() > 8 * 1024 {
        anyhow::bail!("Chutes OAuth redirect/scopes are oversized");
    }
    let state = random_hex(16);
    let verifier = crate::auth::pkce::random_verifier(64);
    let challenge = crate::auth::pkce::code_challenge_s256(&verifier);
    let auth_url = format!(
        "https://api.chutes.ai/idp/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&scopes),
        urlencoding::encode(&state),
        urlencoding::encode(&challenge),
    );

    println!("\n  Opening browser for Chutes authorization...\n");
    println!("AUTH_URL={auth_url}");
    println!("  If the browser doesn't open, visit:\n  {auth_url}\n");
    println!("  Listening for callback on {redirect_uri}");
    open_auth_url(&auth_url)?;

    let (code, returned_state) = wait_or_prompt_for_oauth_code(&redirect_uri, &state, 180)?;
    if returned_state != state {
        anyhow::bail!("OAuth state mismatch");
    }
    validate_auth_secret(&code, "Chutes authorization code")?;

    let client = auth_http_client()?;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id.as_str()),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(secret) = client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let resp = client
        .post("https://api.chutes.ai/idp/token")
        .form(&form)
        .send()
        .context("Chutes OAuth token exchange failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("Chutes OAuth token exchange failed: HTTP {}", resp.status());
    }
    let token: OAuthTokenExchangeResp = parse_auth_json(resp, "Chutes OAuth token response")?;
    let access = token.access_token.context("Missing access_token")?;
    let refresh = token.refresh_token.context("Missing refresh_token")?;
    validate_auth_secret(&access, "Chutes access token")?;
    validate_auth_secret(&refresh, "Chutes refresh token")?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let expires = oauth_expiry_ms(token.expires_in)
        .saturating_sub(300_000)
        .max(now_ms.saturating_add(30_000));

    let requested_profile_id = profile_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string());
    let (path, profile_id) = update_auth_profile_store(|store| {
        let profile_id = requested_profile_id.unwrap_or_else(|| {
            crate::config::auth_profile::next_free_profile_id_in(store, "chutes")
        });
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::OAuth {
                provider: "chutes".to_string(),
                access,
                refresh: Some(refresh),
                expires,
                email: None,
            },
        );
        Ok(profile_id)
    })?;
    println!("  ✓ Authorization successful! Saved to {}", path.display());
    Ok(SetupAuthSelection {
        method: "oauth".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

#[derive(Deserialize)]
pub(super) struct MiniMaxCodeResp {
    user_code: String,
    verification_uri: String,
    expired_in: i64,
    interval: Option<i64>,
    state: String,
}

#[derive(Deserialize)]
pub(super) struct MiniMaxTokenResp {
    status: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expired_in: Option<i64>,
    resource_url: Option<String>,
    notification_message: Option<String>,
    base_resp: Option<MiniMaxBaseResp>,
}

#[derive(Deserialize)]
pub(super) struct MiniMaxBaseResp {
    status_msg: Option<String>,
}

pub(super) fn run_minimax_device_flow(region: &str) -> Result<SetupAuthSelection> {
    run_minimax_device_flow_as(region, None, &device_code_notice)
}

pub(super) fn run_minimax_device_flow_as(
    region: &str,
    profile_id: Option<&str>,
    on_code: &dyn Fn(&str, &str),
) -> Result<SetupAuthSelection> {
    let base_url = if region == "cn" {
        "https://api.minimaxi.com"
    } else {
        "https://api.minimax.io"
    };
    let client_id = "78257093-7e40-4613-99e0-527b14b39113";
    let state = random_hex(16);
    let verifier = crate::auth::pkce::random_verifier(64);
    let challenge = crate::auth::pkce::code_challenge_s256(&verifier);
    let client = auth_http_client()?;
    let resp = client
        .post(format!("{base_url}/oauth/code"))
        .header("Accept", "application/json")
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("scope", "group_id profile model.completion"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ])
        .send()
        .context("MiniMax OAuth authorization failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("MiniMax OAuth authorization failed: HTTP {}", resp.status());
    }
    let code: MiniMaxCodeResp = parse_auth_json(resp, "MiniMax OAuth code response")?;
    validate_auth_secret(&code.user_code, "MiniMax user code")?;
    if code.verification_uri.len() > 8 * 1024 {
        anyhow::bail!("MiniMax verification URL is oversized");
    }
    if code.state != state {
        anyhow::bail!("MiniMax OAuth state mismatch");
    }

    on_code(&code.verification_uri, &code.user_code);
    println!("  Waiting for authorization...");

    let interval_ms = u64::try_from(code.interval.unwrap_or(2000))
        .unwrap_or(2000)
        .clamp(1000, 30_000);
    let now_ms = chrono::Utc::now().timestamp_millis();
    if code.expired_in <= now_ms {
        anyhow::bail!("MiniMax OAuth code is already expired");
    }
    let deadline_ms = code.expired_in.min(now_ms.saturating_add(30 * 60 * 1000));
    while chrono::Utc::now().timestamp_millis() < deadline_ms {
        let tok = client
            .post(format!("{base_url}/oauth/token"))
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:user_code"),
                ("client_id", client_id),
                ("user_code", code.user_code.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .context("MiniMax OAuth token poll failed")?;
        if !tok.status().is_success() {
            anyhow::bail!("MiniMax OAuth token poll failed: HTTP {}", tok.status());
        }
        let payload: MiniMaxTokenResp = parse_auth_json(tok, "MiniMax OAuth token response")?;
        match payload.status.as_deref() {
            Some("success") => {
                let access = payload
                    .access_token
                    .context("Missing MiniMax access_token")?;
                let refresh = payload
                    .refresh_token
                    .context("Missing MiniMax refresh_token")?;
                validate_auth_secret(&access, "MiniMax access token")?;
                validate_auth_secret(&refresh, "MiniMax refresh token")?;
                let now = chrono::Utc::now().timestamp_millis();
                let expires = payload
                    .expired_in
                    .filter(|value| {
                        *value >= now.saturating_add(30_000)
                            && *value <= now.saturating_add(30 * 24 * 60 * 60 * 1000)
                    })
                    .unwrap_or_else(|| now.saturating_add(3600 * 1000));
                let email = payload
                    .notification_message
                    .or(payload.resource_url)
                    .filter(|value| value.len() <= 4 * 1024);
                let requested_profile_id = profile_id
                    .filter(|id| !id.trim().is_empty())
                    .map(|id| id.trim().to_string());
                let (path, profile_id) = update_auth_profile_store(|store| {
                    let profile_id = requested_profile_id.unwrap_or_else(|| {
                        let regional = format!("minimax-portal:{region}");
                        if store.profiles.contains_key(&regional) {
                            crate::config::auth_profile::next_free_profile_id_in(
                                store,
                                "minimax-portal",
                            )
                        } else {
                            regional
                        }
                    });
                    store.profiles.insert(
                        profile_id.clone(),
                        AuthProfileCredential::OAuth {
                            provider: "minimax-portal".to_string(),
                            access,
                            refresh: Some(refresh),
                            expires,
                            email,
                        },
                    );
                    Ok(profile_id)
                })?;
                println!("  ✓ Authorization successful! Saved to {}", path.display());
                return Ok(SetupAuthSelection {
                    method: "device_code".to_string(),
                    source: "profile".to_string(),
                    profile: Some(profile_id),
                    env_var: None,
                });
            }
            Some("error") => {
                let message = payload
                    .base_resp
                    .and_then(|base| base.status_msg)
                    .unwrap_or_else(|| "MiniMax OAuth failed".to_string());
                let preview: String = message.chars().take(8 * 1024).collect();
                anyhow::bail!(preview);
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(interval_ms)),
        }
    }
    anyhow::bail!("MiniMax OAuth timed out before authorization completed")
}

pub(super) fn run_imported_token_flow(
    provider: &ProviderModels,
    method: &AuthMethod,
    allow_reuse: bool,
) -> Result<SetupAuthSelection> {
    let env = method.env_var.unwrap_or("OAUTH_TOKEN");
    let theme = crate::config::setup::phoenix_theme();
    let existing = auth_secret_from_env(env, allow_reuse)?;
    if provider.id == "anthropic" {
        println!(
            "  Anthropic OAuth currently uses token import rather than a Phoenix-owned browser flow."
        );
    } else {
        println!("  {} OAuth currently uses token import.", provider.name);
    }
    if existing.is_some() {
        println!("  Detected env var {env} (value hidden)");
    }
    let use_existing = existing.is_some()
        && Confirm::with_theme(&theme)
            .with_prompt(format!("Use existing {} from environment?", env))
            .default(true)
            .interact()
            .context("Prompt cancelled")?;
    if use_existing {
        return Ok(SetupAuthSelection {
            method: "oauth".to_string(),
            source: "env".to_string(),
            profile: None,
            env_var: Some(env.to_string()),
        });
    }

    let token = Input::<String>::with_theme(&theme)
        .with_prompt(format!("Paste {} token", method.label))
        .allow_empty(false)
        .interact_text()
        .context("OAuth token input cancelled")?;
    validate_auth_secret(&token, "imported OAuth token")?;
    let profile_id = format!("{}:oauth", provider.id);
    let (path, ()) = update_auth_profile_store(|store| {
        store.profiles.insert(
            profile_id.clone(),
            AuthProfileCredential::Token {
                provider: provider.id.to_string(),
                token,
                expires: None,
            },
        );
        Ok(())
    })?;
    println!("  ✓ Authorization saved to {}", path.display());
    Ok(SetupAuthSelection {
        method: "oauth".to_string(),
        source: "profile".to_string(),
        profile: Some(profile_id),
        env_var: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_secrets_are_nonempty_bounded_and_single_line() {
        validate_auth_secret("token-value", "token").unwrap();
        assert!(validate_auth_secret("", "token").is_err());
        assert!(validate_auth_secret("line\nbreak", "token").is_err());
        assert!(validate_auth_secret("nul\0byte", "token").is_err());
        assert!(validate_auth_secret(&"x".repeat(AUTH_SECRET_MAX_BYTES + 1), "token").is_err());
    }

    #[test]
    fn oauth_expiry_is_clamped_to_a_sane_window() {
        let before = chrono::Utc::now().timestamp_millis();
        let short = oauth_expiry_ms(Some(-1));
        let long = oauth_expiry_ms(Some(i64::MAX));
        let after = chrono::Utc::now().timestamp_millis();
        assert!(short >= before.saturating_add(30_000));
        assert!(short <= after.saturating_add(30_000));
        assert!(long >= before.saturating_add(30 * 24 * 60 * 60 * 1000));
        assert!(long <= after.saturating_add(30 * 24 * 60 * 60 * 1000));
    }
}
