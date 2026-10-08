//! Agent-facing Passes tools.
//!
//! * `ask_for_pass` — ask the user, through a typed inline popup, for exactly
//!   the credential the task needs (login, card, API key, token, verification
//!   code, or a free-form secret). The user's answer is saved straight into
//!   Passes by the UI→gateway Vault path; the model only ever receives the new
//!   pass's id and public metadata.
//! * `pass_use` — use a saved pass by id. Phoenix fills it into the current
//!   browser field or into an HTTP request header itself; the secret never
//!   enters model context, and response bodies are scrubbed of it.
//!
//! `credential_list` (in `credentials.rs`) remains the metadata listing tool.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use zeroize::Zeroizing;

use super::ToolOutput;
use crate::security::vault::{CredentialMetadata, RevealedCredential, Vault};
use crate::tools::ask_user::{ApprovalRequest, AskUserInput, AskUserQuestion};

pub const PASS_REQUEST_ACTION: &str = "pass_request";
pub const SAVE_OPTION: &str = "Save";
pub const CANCEL_OPTION: &str = "Not now";
/// Marker every receipt line carries so the runtime can parse it.
const RECEIPT_PREFIX: &str = "Saved pass ";

/// Kinds the popup can render, with their default fields.
pub const KINDS: &[&str] = &["login", "card", "api_key", "token", "verification_code", "secret"];

/// Map the request kind onto the stored pass kind (`login` keeps the
/// long-standing `password` kind so existing login tooling finds it).
pub fn stored_kind(kind: &str) -> &'static str {
    match kind {
        "login" | "password" => "password",
        "card" => "card",
        "api_key" => "api_key",
        "token" => "token",
        "verification_code" => "verification_code",
        _ => "secret",
    }
}

fn default_fields(kind: &str) -> Vec<&'static str> {
    match kind {
        "login" => vec!["username", "password"],
        "card" => vec!["number", "name", "expiry", "cvc", "billing_zip"],
        "api_key" => vec!["key"],
        "token" => vec!["value"],
        "verification_code" => vec!["code"],
        _ => vec!["value"],
    }
}

fn allowed_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "login" => &["site", "username", "password", "totp"],
        "card" => &["number", "name", "expiry", "cvc", "billing_zip"],
        "api_key" => &["service", "key", "base_url"],
        "token" => &["service", "value"],
        "verification_code" => &["code"],
        _ => &["value"],
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PassRequestInput {
    pub kind: String,
    pub reason: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Website or service domain the pass belongs to (`gmail.com`,
    /// `api.openai.com`). Required for login and api_key.
    #[serde(default, alias = "service")]
    pub site: Option<String>,
    /// Which fields to show. Defaults by kind.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Optional custom labels, e.g. {"key": "Secret key (sk-…)"}.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub username_hint: Option<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Ask even when a matching pass already exists.
    #[serde(default)]
    pub force_new: bool,
}

fn default_scope() -> String {
    "agent".to_string()
}

impl PassRequestInput {
    pub fn validate_and_normalize(mut self) -> Result<Self> {
        self.kind = self.kind.trim().to_ascii_lowercase();
        if self.kind == "password" {
            self.kind = "login".into();
        }
        anyhow::ensure!(KINDS.contains(&self.kind.as_str()), "kind must be one of {}", KINDS.join(", "));
        self.reason = self.reason.trim().to_string();
        anyhow::ensure!(!self.reason.is_empty() && self.reason.len() <= 600, "reason must be 1..=600 bytes");
        self.title = self.title.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if let Some(title) = &self.title {
            anyhow::ensure!(title.len() <= 120, "title must be <=120 bytes");
        }
        self.site = match self.site.take().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(site) => Some(crate::security::vault::normalize_site(&site)?),
            None => None,
        };
        if matches!(self.kind.as_str(), "login" | "api_key") {
            anyhow::ensure!(self.site.is_some(), "{} requests need the site or service domain", self.kind);
        }
        let allowed = allowed_fields(&self.kind);
        let mut fields = Vec::new();
        for field in self.fields.drain(..) {
            let field = field.trim().to_ascii_lowercase();
            anyhow::ensure!(allowed.contains(&field.as_str()), "field `{field}` is not valid for {} (use {})", self.kind, allowed.join(", "));
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
        if fields.is_empty() {
            fields = default_fields(&self.kind).into_iter().map(str::to_string).collect();
        }
        // The primary secret is always collected.
        let primary = crate::security::vault::primary_field(stored_kind(&self.kind)).to_string();
        if !fields.contains(&primary) {
            fields.push(primary);
        }
        self.fields = fields;
        anyhow::ensure!(self.labels.len() <= 12, "too many labels");
        self.labels.retain(|key, value| {
            *value = value.trim().chars().take(80).collect();
            allowed.contains(&key.as_str()) && !value.is_empty()
        });
        self.username_hint = self.username_hint.take().map(|u| u.trim().to_string()).filter(|u| !u.is_empty() && u.len() <= 320);
        anyhow::ensure!(matches!(self.scope.as_str(), "agent" | "group" | "company"), "scope must be agent, group, or company");
        Ok(self)
    }

    pub fn display_title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        let site = self.site.clone().unwrap_or_default();
        match self.kind.as_str() {
            "login" => format!("{} login", site_name(&site)),
            "card" => "Payment card".into(),
            "api_key" => format!("{} API key", site_name(&site)),
            "token" => if site.is_empty() { "Access token".into() } else { format!("{} token", site_name(&site)) },
            "verification_code" => "Verification code".into(),
            _ => "Secret".into(),
        }
    }

    /// The typed card. The runtime-bound owner and scope live in `details`;
    /// the UI never chooses who receives the pass.
    pub fn to_ask(&self, agent_id: &str, group_id: Option<&str>, scope: &crate::security::vault::CredentialScope) -> AskUserInput {
        let agent_name = crate::runtime::delegation::agent_display_name(agent_id).replace('_', " ");
        let title = self.display_title();
        let mut details = BTreeMap::new();
        details.insert("kind".into(), self.kind.clone());
        details.insert("title".into(), title.clone());
        details.insert("reason".into(), self.reason.clone());
        details.insert("fields".into(), serde_json::to_string(&self.fields).unwrap_or_else(|_| "[]".into()));
        details.insert("labels".into(), serde_json::to_string(&self.labels).unwrap_or_else(|_| "{}".into()));
        details.insert("agent_id".into(), agent_id.to_string());
        details.insert("scope".into(), self.scope.clone());
        details.insert("credential_scope".into(), serde_json::to_string(scope).unwrap_or_default());
        details.insert("credential_capture".into(), "passes_only".into());
        if let Some(site) = &self.site {
            details.insert("site".into(), site.clone());
        }
        if let Some(group_id) = group_id {
            details.insert("group_id".into(), group_id.to_string());
        }
        if let Some(username) = &self.username_hint {
            details.insert("username_hint".into(), username.clone());
        }
        AskUserInput {
            questions: vec![AskUserQuestion {
                header: Some(title.clone()),
                question: format!("{agent_name} needs your {} to {}.", title_for_sentence(&title), self.reason.trim_end_matches('.')),
                options: vec![SAVE_OPTION.into(), CANCEL_OPTION.into()],
                multi_select: false,
            }],
            approval: Some(ApprovalRequest {
                action: PASS_REQUEST_ACTION.into(),
                subject: title,
                approved_option: SAVE_OPTION.into(),
                details,
            }),
        }
    }
}

fn site_name(site: &str) -> String {
    let base = site.split('.').rev().nth(1).unwrap_or(site);
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Account".into(),
    }
}

fn title_for_sentence(title: &str) -> String {
    // Keep "Gmail login" / "OpenAI API key"; lower a generic noun phrase.
    if KIND_TITLES.contains(&title) {
        let mut chars = title.chars();
        if let Some(first) = chars.next() {
            return first.to_lowercase().collect::<String>() + chars.as_str();
        }
    }
    title.to_string()
}

const KIND_TITLES: &[&str] = &["Payment card", "Access token", "Verification code", "Secret"];

/// The answer text the gateway hands back for a fulfilled request: metadata
/// only. It is the ONLY thing that reaches the asking agent.
pub fn fulfilled_answer(metadata: &CredentialMetadata) -> String {
    let public = public_summary(metadata);
    format!(
        "A: {SAVE_OPTION}\n{RECEIPT_PREFIX}credential_id={} kind={} title=\"{}\" site={}{}{}. The secret stays in Passes; use pass_use with this credential_id.",
        metadata.credential_id,
        metadata.kind,
        metadata.label.replace('"', "'"),
        metadata.site,
        metadata.username.as_deref().map(|u| format!(" username={u}")).unwrap_or_default(),
        if public.is_empty() { String::new() } else { format!(" ({public})") },
    )
}

/// Human-safe public hint: "visa •••• 4242", "key …a1b2".
pub fn public_summary(metadata: &CredentialMetadata) -> String {
    let value: serde_json::Value = serde_json::from_str(&metadata.metadata_json).unwrap_or_default();
    let last4 = value.get("last4").and_then(|v| v.as_str());
    match (metadata.kind.as_str(), value.get("brand").and_then(|v| v.as_str()), last4) {
        ("card", Some(brand), Some(last4)) => format!("{brand} ending {last4}"),
        (_, _, Some(last4)) => format!("ends {last4}"),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassDecision {
    Saved { credential_id: String },
    Existing { credential_id: String },
    Cancelled,
    Pending,
}

pub fn decision_from_answer(answer: &str) -> PassDecision {
    for line in answer.lines() {
        if let Some(rest) = line.trim().strip_prefix(RECEIPT_PREFIX) {
            if let Some(id) = rest.strip_prefix("credential_id=").and_then(|r| r.split_whitespace().next()) {
                return PassDecision::Saved { credential_id: id.to_string() };
            }
        }
    }
    PassDecision::Cancelled
}

pub fn decision_result(input: &PassRequestInput, decision: &PassDecision, metadata: Option<&CredentialMetadata>) -> serde_json::Value {
    let (name, next) = match decision {
        PassDecision::Saved { .. } => ("saved", "The user saved the pass. Use it with pass_use (browser field or HTTP header) by credential_id. Never ask for the secret in chat."),
        PassDecision::Existing { .. } => ("existing", "A matching saved pass already exists; use it with pass_use. Ask again with force_new only if the site rejects it."),
        PassDecision::Cancelled => ("cancel", "The user declined. Do not ask again for this pass in this turn; continue with genuine alternatives or report what is blocked."),
        PassDecision::Pending => ("pending_user", "The request card is still open. Continue independent work; do not post another request. The user's answer resumes this conversation with the new credential_id."),
    };
    let mut result = serde_json::json!({
        "decision": name,
        "kind": input.kind,
        "site": input.site,
        "next_step": next,
        "secret_in_context": false,
    });
    if let PassDecision::Saved { credential_id } | PassDecision::Existing { credential_id } = decision {
        result["credential_id"] = serde_json::json!(credential_id);
    }
    if let Some(metadata) = metadata {
        result["title"] = serde_json::json!(metadata.label);
        result["username"] = serde_json::json!(metadata.username);
        result["public"] = serde_json::json!(public_summary(metadata));
    }
    result
}

/// An existing visible pass that already satisfies the request.
pub fn existing_for_request(input: &PassRequestInput, agent_id: &str, group_id: Option<&str>) -> Option<CredentialMetadata> {
    if input.force_new || input.kind == "verification_code" {
        return None;
    }
    let scopes = crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id).ok()?;
    let mut matches = Vault::open_default().list(&scopes).ok()?;
    let kind = stored_kind(&input.kind);
    matches.retain(|meta| {
        meta.kind == kind
            && match input.site.as_deref() {
                Some(site) => crate::tools::browser_cookie_grants::domain_matches_site(&meta.site, site),
                None => input.kind != "secret",
            }
    });
    matches.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    matches.into_iter().next()
}

/// The user answered with a receipt only — look up its public metadata.
pub fn saved_metadata(credential_id: &str, agent_id: &str, group_id: Option<&str>) -> Option<CredentialMetadata> {
    let scopes = crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id).ok()?;
    Vault::open_default().list(&scopes).ok()?.into_iter().find(|meta| meta.credential_id == credential_id)
}

// ------------------------------------------------------------------ use

#[derive(Debug, Clone, Deserialize)]
pub struct PassUseInput {
    pub credential_id: String,
    /// `browser_field` (default) or `http_header`.
    #[serde(default = "default_target")]
    pub target: String,
    /// Which secret to use: password, username, totp, number, cvc, expiry,
    /// exp_month, exp_year, name, billing_zip, key, value, code.
    #[serde(default)]
    pub field: Option<String>,
    /// browser_field: element [index] from current browser state.
    #[serde(default)]
    pub index: Option<i64>,
    /// http_header: the https URL to call (must be on the pass's site).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_header")]
    pub header: String,
    /// Header value template; `{secret}` is replaced by the runtime.
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

fn default_target() -> String {
    "browser_field".into()
}
fn default_method() -> String {
    "GET".into()
}
fn default_header() -> String {
    "Authorization".into()
}
fn default_format() -> String {
    "Bearer {secret}".into()
}

/// How long a turn waits inline on a pass request or unlock card before
/// leaving it open (a late answer still wakes the asking coworker).
pub const REQUEST_WAIT_SECONDS: u64 = 300;

/// The typed one-time unlock card (rendered as the secure inline unlock form).
pub fn unlock_ask(agent_name: &str) -> AskUserInput {
    let reason = format!("{agent_name} needs to use a saved pass. Unlock Passes once — it stays unlocked until Phoenix quits.");
    AskUserInput {
        questions: vec![AskUserQuestion {
            header: Some("Unlock Passes".into()),
            question: reason.clone(),
            options: vec!["Unlock here".into(), CANCEL_OPTION.into()],
            multi_select: false,
        }],
        approval: Some(ApprovalRequest {
            action: "vault_unlock".into(),
            subject: "Passes".into(),
            approved_option: "Unlock here".into(),
            details: BTreeMap::from([("reason".to_string(), reason)]),
        }),
    }
}

/// True when using `credential_id` would need the master password now.
pub fn needs_unlock() -> bool {
    !Vault::open_default().status().can_reveal() && Vault::open_default().has_master_password()
}

/// Resolve which secret value to type/send. TOTP seeds become the current
/// 6-digit code; card expiry splits into month/year on request.
pub fn resolve_field(credential: &RevealedCredential, field: Option<&str>) -> Result<Zeroizing<String>> {
    let field = field.map(|f| f.trim().to_ascii_lowercase()).filter(|f| !f.is_empty());
    match field.as_deref() {
        None => Ok(Zeroizing::new(credential.secret().to_string())),
        Some("username") | Some("email") => credential
            .metadata
            .username
            .clone()
            .map(Zeroizing::new)
            .context("this pass has no username"),
        Some("totp") | Some("otp") | Some("2fa") => {
            let seed = credential.field("totp").context("this pass has no TOTP seed")?;
            totp_now(seed)
        }
        Some(name @ ("exp_month" | "exp_year" | "exp_year_short")) => {
            let expiry = credential.field("expiry").context("this card has no expiry")?;
            let (month, year) = split_expiry(expiry).context("card expiry is not MM/YY")?;
            Ok(Zeroizing::new(match name {
                "exp_month" => month,
                "exp_year" => year,
                _ => year[year.len().saturating_sub(2)..].to_string(),
            }))
        }
        Some(other) => credential
            .field(other)
            .map(|value| Zeroizing::new(value.to_string()))
            .with_context(|| format!("this pass has no `{other}` field (available: {})", available_fields(credential).join(", "))),
    }
}

fn available_fields(credential: &RevealedCredential) -> Vec<String> {
    let mut names = vec![crate::security::vault::primary_field(&credential.metadata.kind).to_string()];
    if credential.metadata.username.is_some() {
        names.push("username".into());
    }
    names.extend(credential.field_names());
    if credential.field("expiry").is_some() {
        names.extend(["exp_month".into(), "exp_year".into()]);
    }
    names
}

fn split_expiry(expiry: &str) -> Option<(String, String)> {
    let digits: Vec<&str> = expiry.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).collect();
    let (month, year) = match digits.as_slice() {
        [m, y] => (m.to_string(), y.to_string()),
        [all] if all.len() == 4 => (all[..2].to_string(), all[2..].to_string()),
        _ => return None,
    };
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let year = if year.len() == 2 { format!("20{year}") } else { year };
    Some((format!("{month:02}"), year))
}

/// RFC 6238 TOTP (SHA-1, 30 s, 6 digits). Accepts a base32 seed or an
/// `otpauth://` URI.
pub fn totp_now(seed: &str) -> Result<Zeroizing<String>> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    totp_at(seed, now)
}

pub fn totp_at(seed: &str, unix_seconds: u64) -> Result<Zeroizing<String>> {
    use hmac::{Hmac, Mac};
    let seed = seed.trim();
    let (secret, digits, period) = if seed.starts_with("otpauth://") {
        let url = url::Url::parse(seed).context("invalid otpauth URI")?;
        let query: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())).collect();
        (
            query.get("secret").cloned().context("otpauth URI has no secret")?,
            query.get("digits").and_then(|d| d.parse().ok()).unwrap_or(6u32),
            query.get("period").and_then(|p| p.parse().ok()).unwrap_or(30u64),
        )
    } else {
        (seed.to_string(), 6, 30)
    };
    anyhow::ensure!((6..=8).contains(&digits) && (15..=120).contains(&period), "unsupported TOTP parameters");
    let key = Zeroizing::new(base32_decode(&secret).context("TOTP seed is not valid base32")?);
    let counter = (unix_seconds / period).to_be_bytes();
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&key).map_err(|_| anyhow::anyhow!("invalid TOTP key"))?;
    mac.update(&counter);
    let hash = mac.finalize().into_bytes();
    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let binary = ((u32::from(hash[offset]) & 0x7f) << 24)
        | (u32::from(hash[offset + 1]) << 16)
        | (u32::from(hash[offset + 2]) << 8)
        | u32::from(hash[offset + 3]);
    let code = binary % 10u32.pow(digits);
    Ok(Zeroizing::new(format!("{code:0width$}", width = digits as usize)))
}

fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut count = 0u32;
    let mut out = Vec::new();
    for ch in input.chars().filter(|c| !c.is_whitespace() && *c != '=' && *c != '-') {
        let value = match ch.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        bits = (bits << 5) | value;
        count += 5;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Replace every secret value with a placeholder before text leaves the
/// runtime.
pub fn scrub(text: &str, credential: &RevealedCredential, extra: &[&str]) -> String {
    let mut output = text.to_string();
    let mut values: Vec<&str> = credential.secret_values();
    values.extend(extra.iter().copied());
    values.retain(|value| value.len() >= 4);
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        output = output.replace(value, "[PASS]");
    }
    output
}

/// Run `pass_use`. Browser fills go through the native browser action;
/// HTTP calls are made here with the secret in one header.
pub fn use_pass(
    input: PassUseInput,
    agent_id: &str,
    group_id: Option<&str>,
    browser_instance: Option<&str>,
    session_id: Option<&str>,
) -> Result<ToolOutput> {
    match input.target.as_str() {
        "browser_field" | "browser" => {
            let index = input.index.context("pass_use browser_field needs the field [index] from browser_state")?;
            let mut payload = serde_json::json!({"credential_id": input.credential_id, "index": index});
            if let Some(field) = &input.field {
                payload["field"] = serde_json::json!(field);
            }
            let mut output = crate::tools::browser_native::execute(
                "browser_input_credential",
                payload,
                browser_instance,
                session_id,
                Some(agent_id),
                group_id,
            )?;
            // One-time verification codes are consumed by their first fill.
            let scopes = crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
            let vault = Vault::open_default();
            let one_time = vault
                .list(&scopes)?
                .into_iter()
                .any(|meta| meta.credential_id == input.credential_id && meta.kind == "verification_code");
            if one_time && vault.delete(&input.credential_id, &scopes).unwrap_or(false) {
                output.content.push_str("\nThe one-time verification code was used and removed from Passes.");
            }
            Ok(output)
        }
        "http_header" | "http" => http_with_pass(input, agent_id, group_id),
        other => anyhow::bail!("unknown pass_use target `{other}` (use browser_field or http_header)"),
    }
}

fn http_with_pass(input: PassUseInput, agent_id: &str, group_id: Option<&str>) -> Result<ToolOutput> {
    let scopes = crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
    let url = url::Url::parse(input.url.as_deref().context("pass_use http_header needs `url`")?.trim()).context("invalid url")?;
    let host = url.host_str().context("url has no host")?.to_ascii_lowercase();
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    anyhow::ensure!(url.scheme() == "https" || (local && url.scheme() == "http"), "pass_use only sends secrets over https");
    anyhow::ensure!(url.username().is_empty() && url.password().is_none(), "url must not embed credentials");
    let header = input.header.trim();
    anyhow::ensure!(
        !header.is_empty() && header.len() <= 64 && header.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "header must be a plain HTTP header name"
    );
    anyhow::ensure!(input.format.contains("{secret}") && input.format.len() <= 200, "format must contain {{secret}}");
    let method = reqwest::Method::from_bytes(input.method.trim().to_ascii_uppercase().as_bytes()).context("invalid HTTP method")?;
    let credential = Vault::open_default()
        .reveal_for_agent(&input.credential_id, &scopes)
        .map_err(|error| anyhow::anyhow!("{error:#}"))
        .context("saved pass is unavailable")?;
    // Bind the secret to its own site (or its declared API base URL): a
    // model can't redirect a key to an arbitrary host.
    let public: serde_json::Value = serde_json::from_str(&credential.metadata.metadata_json).unwrap_or_default();
    let base_host = public
        .get("base_url")
        .and_then(|v| v.as_str())
        .and_then(|base| url::Url::parse(base).ok())
        .and_then(|base| base.host_str().map(str::to_ascii_lowercase));
    let bound = crate::tools::browser_cookie_grants::domain_matches_site(&host, &credential.metadata.site)
        || base_host.as_deref().is_some_and(|base| base == host);
    anyhow::ensure!(
        bound,
        "pass `{}` is bound to {}; refusing to send it to {host}",
        credential.metadata.credential_id,
        credential.metadata.site
    );
    let secret = resolve_field(&credential, input.field.as_deref())?;
    let value = Zeroizing::new(input.format.replace("{secret}", secret.as_str()));
    let mut header_value = reqwest::header::HeaderValue::from_str(value.as_str()).context("secret cannot be sent in an HTTP header")?;
    header_value.set_sensitive(true);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client.request(method.clone(), url.clone()).header(header, header_value);
    if let Some(body) = &input.body {
        anyhow::ensure!(body.len() <= 256 * 1024, "body is too large");
        request = request
            .header(reqwest::header::CONTENT_TYPE, input.content_type.as_deref().unwrap_or("application/json"))
            .body(body.clone());
    }
    let response = request.send().map_err(|error| anyhow::anyhow!("{}", scrub(&error.to_string(), &credential, &[value.as_str()])))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = response.text().unwrap_or_default();
    let mut body: String = text.chars().take(20_000).collect();
    if text.chars().count() > 20_000 {
        body.push_str("\n…[truncated]");
    }
    let body = scrub(&body, &credential, &[value.as_str(), secret.as_str()]);
    Ok(ToolOutput {
        summary: format!("{method} {host} → {status} with pass {}", credential.metadata.credential_id),
        content: format!(
            "HTTP {status} ({content_type}) from {method} {url}\nSent pass `{}` in `{header}` (value hidden).\n\n{body}",
            credential.metadata.credential_id
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_rfc6238_vector() {
        // RFC 6238 SHA-1 seed "12345678901234567890" in base32, 8 digits.
        let seed = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8";
        assert_eq!(totp_at(seed, 59).unwrap().as_str(), "94287082");
        assert_eq!(totp_at(seed, 1111111109).unwrap().as_str(), "07081804");
        assert_eq!(totp_at("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59).unwrap().as_str(), "287082");
    }

    #[test]
    fn request_validation_curates_fields_per_kind() {
        let input = PassRequestInput {
            kind: "card".into(),
            reason: "pay for the approved domain".into(),
            title: None,
            site: None,
            fields: vec!["number".into(), "cvc".into()],
            labels: BTreeMap::from([("cvc".into(), "Security code".into()), ("bogus".into(), "x".into())]),
            username_hint: None,
            scope: "agent".into(),
            force_new: false,
        }
        .validate_and_normalize()
        .unwrap();
        assert_eq!(input.fields, vec!["number", "cvc"]);
        assert!(!input.labels.contains_key("bogus"));
        let bad = PassRequestInput { kind: "login".into(), site: None, ..input.clone() };
        assert!(bad.validate_and_normalize().is_err(), "login needs a site");
        let wrong_field = PassRequestInput { fields: vec!["password".into()], ..input.clone() };
        assert!(wrong_field.validate_and_normalize().is_err());
        let ask = input.to_ask("phoenix", None, &crate::security::vault::CredentialScope::agent("phoenix"));
        let approval = ask.approval.unwrap();
        assert_eq!(approval.action, PASS_REQUEST_ACTION);
        assert_eq!(approval.details.get("kind").map(String::as_str), Some("card"));
        assert!(ask.questions[0].question.contains("payment card"));
    }

    #[test]
    fn receipt_round_trips_without_any_secret() {
        let metadata = CredentialMetadata {
            credential_id: "abc-123".into(),
            scope: crate::security::vault::CredentialScope::Company,
            site: "gmail.com".into(),
            label: "Gmail login".into(),
            username: Some("me@gmail.com".into()),
            kind: "password".into(),
            metadata_json: "{}".into(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let answer = fulfilled_answer(&metadata);
        assert_eq!(decision_from_answer(&answer), PassDecision::Saved { credential_id: "abc-123".into() });
        assert_eq!(decision_from_answer("A: Not now"), PassDecision::Cancelled);
    }

    #[test]
    fn expiry_parsing_and_scrubbing() {
        assert_eq!(split_expiry("4/29"), Some(("04".into(), "2029".into())));
        assert_eq!(split_expiry("12 / 2031"), Some(("12".into(), "2031".into())));
        assert_eq!(split_expiry("13/29"), None);
    }
}
