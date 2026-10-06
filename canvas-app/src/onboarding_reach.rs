//! Read-only Phoenix contracts for first-run onboarding and Agent-Reach.
//!
//! The desktop crate intentionally does not link the agent runtime.  These
//! projections therefore read the same private state that the gateway reads,
//! but never resolve credentials, start an MCP process, open a browser, call a
//! remote service, or execute a desktop probe.  A configured capability is
//! consequently reported as `configured_unprobed` until a runtime-owned,
//! side-effect-free observation supplies stronger evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

const ONBOARDING_CONTRACT_VERSION: &str = "phoenix.onboarding.v1";
const REACH_CONTRACT_VERSION: &str = "phoenix.agent-reach.v1";
const CONFIG_MAX_BYTES: usize = 8 * 1024 * 1024;
const AUTH_PROFILES_MAX_BYTES: usize = 8 * 1024 * 1024;
const COMPOSIO_REGISTRY_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 256;
const MAX_PROFILES: usize = 2_048;
const MAX_MCP_SERVERS: usize = 256;
const MAX_TOOLKITS: usize = 1_024;
const MAX_PROFILE_ID_CHARS: usize = 128;

const REACH_STATUS_VOCABULARY: &[&str] = &[
    "available",
    "configured_unprobed",
    "unavailable",
    "blocked_user",
    "unknown_error",
    "stale",
    "configured_but_not_routed",
];

/// Providers with a native sign-in flow exposed by Phoenix. The serialized
/// field keeps its older `oauth_*` spelling for contract compatibility, but
/// now also covers secure device-code sign-in.
const OAUTH_LOGIN_PROVIDERS: &[&str] = &[
    "xai",
    "grok-cli",
    "openai-codex",
    "google-gemini-cli",
    "chutes",
    "github-copilot",
    "minimax-portal",
];

/// Provider environment variables used by Phoenix's current LLM catalog.
/// Values are never returned; only non-empty/present is observed.
const PROVIDER_ENV_VARS: &[(&str, &[&str])] = &[
    ("anthropic", &["ANTHROPIC_API_KEY", "ANTHROPIC_OAUTH_TOKEN"]),
    ("openai", &["OPENAI_API_KEY", "OPENAI_OAUTH_TOKEN"]),
    ("openai-codex", &["OPENAI_OAUTH_TOKEN"]),
    ("openrouter", &["OPENROUTER_API_KEY"]),
    ("tokenrouter", &["TOKENROUTER_API_KEY"]),
    ("opencode", &["OPENCODE_API_KEY"]),
    ("google", &["GOOGLE_API_KEY", "GEMINI_API_KEY"]),
    ("google-gemini-cli", &["GEMINI_CLI_OAUTH_TOKEN"]),
    ("deepseek", &["DEEPSEEK_API_KEY"]),
    ("groq", &["GROQ_API_KEY"]),
    ("minimax-portal", &["MINIMAX_OAUTH_TOKEN"]),
    ("mistral", &["MISTRAL_API_KEY"]),
    ("together", &["TOGETHER_API_KEY"]),
    ("fireworks", &["FIREWORKS_API_KEY"]),
    ("deepinfra", &["DEEPINFRA_API_KEY"]),
    ("moonshot", &["MOONSHOT_API_KEY"]),
    ("kimi-coding", &["KIMI_API_KEY"]),
    ("zai", &["ZAI_API_KEY"]),
    ("xai", &["XAI_API_KEY", "XAI_OAUTH_TOKEN"]),
    ("grok-cli", &["XAI_OAUTH_TOKEN"]),
    ("github-copilot", &["COPILOT_GITHUB_TOKEN"]),
    ("cerebras", &["CEREBRAS_API_KEY"]),
    ("venice", &["VENICE_API_KEY"]),
    ("kilocode", &["KILOCODE_API_KEY"]),
    ("nvidia", &["NVIDIA_API_KEY"]),
    ("ollama-cloud", &["OLLAMA_API_KEY"]),
    ("volcengine", &["VOLCENGINE_API_KEY"]),
    ("byteplus", &["BYTEPLUS_API_KEY"]),
    ("stepfun", &["STEPFUN_API_KEY"]),
    ("qianfan", &["QIANFAN_API_KEY"]),
    ("tencent", &["TENCENT_API_KEY"]),
    ("xiaomi", &["XIAOMI_API_KEY"]),
    ("chutes", &["CHUTES_API_KEY", "CHUTES_OAUTH_TOKEN"]),
    ("huggingface", &["HF_TOKEN", "HUGGINGFACE_API_KEY"]),
];

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingFileStatus {
    pub path: String,
    pub present: bool,
    pub readable: bool,
    pub parseable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingProviderStatus {
    pub id: String,
    pub configured: bool,
    pub active: bool,
    pub profile_count: usize,
    pub profile_ids: Vec<String>,
    pub oauth_supported: bool,
    pub env_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingAuthProfileStatus {
    pub id: String,
    pub provider: String,
    pub method: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingAuthStatus {
    pub state: String,
    pub source: String,
    pub method: String,
    pub provider_configured: bool,
    pub profile_configured: bool,
    pub environment_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingStatus {
    pub contract_version: &'static str,
    pub config: OnboardingFileStatus,
    pub auth_profiles_file: OnboardingFileStatus,
    pub provider: Option<String>,
    pub model_configured: bool,
    pub profile: Option<String>,
    pub next_profile_id: Option<String>,
    pub supported_oauth: Vec<String>,
    pub providers: Vec<OnboardingProviderStatus>,
    pub auth_profiles: Vec<OnboardingAuthProfileStatus>,
    pub auth: OnboardingAuthStatus,
}

/// A single reachability row.  `schema` is deliberately separate from
/// `status`: a schema/configuration observation is not evidence that the
/// capability can be reached, and neither is account metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ReachRow {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub schema: String,
    pub account: String,
    pub route_allowed: Option<bool>,
    pub safe_probe: String,
    pub probe_performed: bool,
    pub observed_at: Option<String>,
    pub side_effect: String,
    pub stale: bool,
    pub ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentReachSnapshot {
    pub contract_version: &'static str,
    /// Unix seconds; numeric so a UI never has to parse a locale-dependent
    /// timestamp and so this crate does not need a second date dependency.
    pub generated_at: i64,
    pub probe_policy: &'static str,
    /// Stable vocabulary for UI filters. A snapshot may contain only the
    /// states supported by the evidence currently on disk.
    pub status_vocabulary: Vec<String>,
    pub rows: Vec<ReachRow>,
}

#[derive(Debug, Clone)]
struct ConfigSnapshot {
    status: OnboardingFileStatus,
    value: Option<toml::Value>,
}

fn error_text(error: impl std::fmt::Display) -> String {
    let mut text = error.to_string();
    if text.chars().count() > MAX_ERROR_CHARS {
        text = text.chars().take(MAX_ERROR_CHARS).collect();
        text.push('…');
    }
    text
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

fn config_path() -> PathBuf {
    super::phoenix_home().join("config.toml")
}

fn auth_profiles_path() -> PathBuf {
    super::phoenix_home().join("auth-profiles.json")
}

fn composio_registry_path() -> PathBuf {
    super::phoenix_home().join("composio-connections.json")
}

fn read_file(path: &Path, max_bytes: usize, label: &str) -> Result<Option<String>, String> {
    super::read_private_text(path, max_bytes, label)
}

fn config_snapshot() -> ConfigSnapshot {
    let path = config_path();
    match read_file(&path, CONFIG_MAX_BYTES, "config.toml") {
        Ok(Some(raw)) => match raw.parse::<toml::Value>() {
            Ok(value) => ConfigSnapshot {
                status: OnboardingFileStatus {
                    path: path.to_string_lossy().to_string(),
                    present: true,
                    readable: true,
                    parseable: true,
                    error: None,
                },
                value: Some(value),
            },
            Err(_error) => ConfigSnapshot {
                status: OnboardingFileStatus {
                    path: path.to_string_lossy().to_string(),
                    present: true,
                    readable: true,
                    parseable: false,
                    // Parser diagnostics can echo a malformed line. Keep the
                    // onboarding projection secret-safe even when a user
                    // accidentally pasted a credential into broken TOML.
                    error: Some("config.toml could not be parsed".to_string()),
                },
                value: None,
            },
        },
        Ok(None) => ConfigSnapshot {
            status: OnboardingFileStatus {
                path: path.to_string_lossy().to_string(),
                present: false,
                readable: false,
                parseable: false,
                error: None,
            },
            value: None,
        },
        Err(error) => ConfigSnapshot {
            status: OnboardingFileStatus {
                path: path.to_string_lossy().to_string(),
                present: true,
                readable: false,
                parseable: false,
                error: Some(error_text(error)),
            },
            value: None,
        },
    }
}

fn value_string<'a>(root: &'a toml::Value, path: &[&str]) -> Option<&'a str> {
    let mut value = root;
    for segment in path {
        value = value.get(*segment)?;
    }
    value.as_str()
}

fn profile_provider(value: &Value) -> Option<&str> {
    value.get("provider").and_then(Value::as_str)
}

fn profile_method(value: &Value) -> String {
    match value.get("type").and_then(Value::as_str) {
        Some("api_key") => "api".to_string(),
        Some(method) => method.to_string(),
        None => "unknown".to_string(),
    }
}

fn profile_expiry(value: &Value) -> Option<i64> {
    value.get("expires").and_then(|expires| {
        expires
            .as_i64()
            .or_else(|| expires.as_u64().and_then(|value| i64::try_from(value).ok()))
    })
}

fn profile_state(value: &Value) -> String {
    match profile_expiry(value) {
        Some(expires) if expires <= unix_now() => "expired".to_string(),
        Some(_) => "configured".to_string(),
        None => match value.get("type").and_then(Value::as_str) {
            Some("api_key") | Some("token") | Some("oauth") => "configured".to_string(),
            _ => "unknown".to_string(),
        },
    }
}

fn profile_ids_from_json(raw: &str) -> (Vec<OnboardingAuthProfileStatus>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (
            Vec::new(),
            Some("auth profile store is not valid JSON".to_string()),
        );
    };
    let Some(profiles) = value.get("profiles").and_then(Value::as_object) else {
        return (
            Vec::new(),
            Some("auth profile store has no profiles object".to_string()),
        );
    };
    let mut rows = Vec::new();
    for (id, credential) in profiles.iter().take(MAX_PROFILES) {
        if id.is_empty() || id.chars().count() > MAX_PROFILE_ID_CHARS {
            continue;
        }
        rows.push(OnboardingAuthProfileStatus {
            id: id.clone(),
            provider: profile_provider(credential)
                .unwrap_or("unknown")
                .to_string(),
            method: profile_method(credential),
            state: profile_state(credential),
            expires_at: profile_expiry(credential),
        });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let limit_error = (profiles.len() > MAX_PROFILES)
        .then(|| format!("auth profile store has more than {MAX_PROFILES} profiles"));
    (rows, limit_error)
}

fn profile_rows_by_provider(rows: &[OnboardingAuthProfileStatus]) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for row in rows {
        grouped
            .entry(row.provider.clone())
            .or_default()
            .push(row.id.clone());
    }
    grouped
}

fn env_vars_for(provider: &str) -> &'static [&'static str] {
    PROVIDER_ENV_VARS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, vars)| *vars)
        .unwrap_or(&[])
}

fn env_configured(provider: &str, requested: Option<&str>) -> bool {
    let mut candidates = requested
        .into_iter()
        .chain(env_vars_for(provider).iter().copied());
    candidates.any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn next_profile_id(provider: Option<&str>, rows: &[OnboardingAuthProfileStatus]) -> Option<String> {
    let provider = provider?.trim();
    if provider.is_empty() || provider.chars().count() > MAX_PROFILE_ID_CHARS {
        return None;
    }
    let existing: BTreeSet<&str> = rows
        .iter()
        .filter(|row| row.provider == provider)
        .map(|row| row.id.as_str())
        .collect();
    let base = format!("{provider}:default");
    if !existing.contains(base.as_str()) {
        return Some(base);
    }
    for slot in 2..10_000 {
        let candidate = format!("{provider}:{slot}");
        if !existing.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }
    None
}

fn declared_auth(
    config: Option<&toml::Value>,
    provider: &str,
    rows: &[OnboardingAuthProfileStatus],
) -> OnboardingAuthStatus {
    let llm = config
        .and_then(|root| root.get("profile"))
        .and_then(|profile| profile.get("llm"));
    let auth = config
        .and_then(|root| root.get("profile"))
        .and_then(|profile| profile.get("llm"))
        .and_then(|llm| llm.get("auth"));
    let method = auth
        .and_then(|value| value.get("method"))
        .and_then(toml::Value::as_str)
        .unwrap_or("auto")
        .to_string();
    let lane_profile_id = llm
        .and_then(|value| value.get("auth_by_lane"))
        .and_then(|value| value.get("orchestrator"))
        .and_then(toml::Value::as_str);
    let source = auth
        .and_then(|value| value.get("source"))
        .and_then(toml::Value::as_str)
        .unwrap_or(if lane_profile_id.is_some() {
            "profile"
        } else {
            "auto"
        })
        .to_string();
    let requested_env = auth
        .and_then(|value| value.get("env_var"))
        .and_then(toml::Value::as_str);
    let profile_id = auth
        .and_then(|value| value.get("profile"))
        .and_then(toml::Value::as_str)
        .or(lane_profile_id)
        .map(str::to_string);
    let profile_configured = profile_id.as_deref().is_some_and(|id| {
        rows.iter()
            .any(|row| row.id == id && row.provider == provider)
    });
    let environment_configured = env_configured(provider, requested_env);
    let state = match source.as_str() {
        "profile" => {
            if profile_id.is_none() {
                "missing_profile_id"
            } else if profile_configured {
                "configured"
            } else {
                "missing_profile"
            }
        }
        "env" => {
            if environment_configured {
                "configured"
            } else {
                "missing_environment_credential"
            }
        }
        "none" => "not_required",
        _ if profile_configured || environment_configured => "configured",
        _ => "unconfigured",
    };
    OnboardingAuthStatus {
        state: state.to_string(),
        source,
        method,
        provider_configured: !provider.is_empty(),
        profile_configured,
        environment_configured,
        profile_id,
        detail: None,
    }
}

/// Return the secret-safe first-run state used by the Phoenix
/// onboarding card. This command is entirely read-only and does not call a
/// provider or try to refresh OAuth.
#[tauri::command]
pub fn onboarding_status() -> Result<OnboardingStatus, String> {
    let config = config_snapshot();
    let provider = config
        .value
        .as_ref()
        .and_then(|root| value_string(root, &["profile", "llm", "provider"]))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let model_configured = config
        .value
        .as_ref()
        .and_then(|root| value_string(root, &["profile", "llm", "model"]))
        .is_some_and(|value| !value.trim().is_empty());

    let auth_path = auth_profiles_path();
    let (mut auth_status, auth_raw) =
        match read_file(&auth_path, AUTH_PROFILES_MAX_BYTES, "auth-profiles.json") {
            Ok(Some(raw)) => {
                let parsed = serde_json::from_str::<Value>(&raw).is_ok();
                let status = OnboardingFileStatus {
                    path: auth_path.to_string_lossy().to_string(),
                    present: true,
                    readable: true,
                    parseable: parsed,
                    error: None,
                };
                (status, Some(raw))
            }
            Ok(None) => (
                OnboardingFileStatus {
                    path: auth_path.to_string_lossy().to_string(),
                    present: false,
                    readable: false,
                    parseable: false,
                    error: None,
                },
                None,
            ),
            Err(error) => (
                OnboardingFileStatus {
                    path: auth_path.to_string_lossy().to_string(),
                    present: true,
                    readable: false,
                    parseable: false,
                    error: Some(error_text(error)),
                },
                None,
            ),
        };
    let (profile_rows, profile_error) = auth_raw
        .as_deref()
        .map(profile_ids_from_json)
        .unwrap_or_default();
    if auth_status.error.is_none() {
        auth_status.error = profile_error;
    }
    let grouped = profile_rows_by_provider(&profile_rows);
    let mut provider_ids: BTreeSet<String> = grouped.keys().cloned().collect();
    if let Some(provider) = &provider {
        provider_ids.insert(provider.clone());
    }
    let mut providers = provider_ids
        .into_iter()
        .map(|id| {
            let profile_ids = grouped.get(&id).cloned().unwrap_or_default();
            OnboardingProviderStatus {
                active: provider.as_deref() == Some(id.as_str()),
                configured: !profile_ids.is_empty() || env_configured(&id, None),
                profile_count: profile_ids.len(),
                profile_ids,
                oauth_supported: OAUTH_LOGIN_PROVIDERS.contains(&id.as_str()),
                env_configured: env_configured(&id, None),
                id,
            }
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| (!a.active, a.id.as_str()).cmp(&(!b.active, b.id.as_str())));

    let mut auth = provider
        .as_deref()
        .map(|provider| declared_auth(config.value.as_ref(), provider, &profile_rows))
        .unwrap_or(OnboardingAuthStatus {
            state: "missing_provider".to_string(),
            source: "none".to_string(),
            method: "unknown".to_string(),
            provider_configured: false,
            profile_configured: false,
            environment_configured: false,
            profile_id: None,
            detail: Some("config.toml has no profile.llm.provider".to_string()),
        });
    if !config.status.readable || !config.status.parseable {
        auth.state = "config_unreadable".to_string();
        auth.detail = config.status.error.clone();
    }

    Ok(OnboardingStatus {
        contract_version: ONBOARDING_CONTRACT_VERSION,
        config: config.status,
        auth_profiles_file: auth_status,
        provider: provider.clone(),
        model_configured,
        profile: auth.profile_id.clone(),
        next_profile_id: next_profile_id(provider.as_deref(), &profile_rows),
        supported_oauth: OAUTH_LOGIN_PROVIDERS
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        providers,
        auth_profiles: profile_rows,
        auth,
    })
}

fn reach_row(
    id: impl Into<String>,
    kind: impl Into<String>,
    label: impl Into<String>,
    status: &str,
    schema: &str,
    account: &str,
    side_effect: &str,
) -> ReachRow {
    ReachRow {
        id: id.into(),
        kind: kind.into(),
        label: label.into(),
        status: status.to_string(),
        schema: schema.to_string(),
        account: account.to_string(),
        route_allowed: None,
        safe_probe: "not_run".to_string(),
        probe_performed: false,
        observed_at: None,
        side_effect: side_effect.to_string(),
        stale: false,
        ttl_seconds: None,
        error: None,
        route: None,
        metadata: None,
    }
}

fn local_mcp_rows(config: Option<&toml::Value>) -> Vec<ReachRow> {
    let Some(servers) = config
        .and_then(|root| root.get("profile"))
        .and_then(|profile| profile.get("mcp_server"))
        .and_then(toml::Value::as_array)
    else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for server in servers.iter().take(MAX_MCP_SERVERS) {
        let Some(table) = server.as_table() else {
            continue;
        };
        let Some(name) = table.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let enabled = table
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        let route = table
            .get("route")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty());
        let remote = table
            .get("url")
            .and_then(toml::Value::as_str)
            .is_some_and(|url| !url.trim().is_empty());
        let configured = table
            .get("command")
            .and_then(toml::Value::as_str)
            .is_some_and(|command| !command.trim().is_empty())
            || remote;
        let mut row = reach_row(
            format!("mcp:{name}"),
            "mcp",
            name,
            if !enabled {
                "unavailable"
            } else if configured {
                if route.is_some() {
                    "configured_but_not_routed"
                } else {
                    "configured_unprobed"
                }
            } else {
                "unavailable"
            },
            "unknown",
            "not_applicable",
            "unknown",
        );
        row.route = route.clone();
        // This command has no agent-lane identity. Shared servers are known
        // to be routeable; an agent-specific server is intentionally kept
        // unknown to Phoenix rather than guessed as allowed or denied.
        row.route_allowed = Some(route.is_none());
        row.error = (!configured).then(|| "server has neither command nor url".to_string());
        row.metadata = Some(serde_json::json!({
            "transport": if remote { "remote_http" } else { "local_stdio" },
            "enabled": enabled,
            "schema_source": "not_probed",
        }));
        rows.push(row);
    }
    rows
}

fn composio_rows() -> Vec<ReachRow> {
    let key_path = super::phoenix_home().join("composio-mcp.key");
    let file_key = read_file(&key_path, super::PRIVATE_SECRET_MAX_BYTES, "Composio key")
        .ok()
        .flatten()
        .is_some_and(|key| !key.trim().is_empty());
    let env_key = std::env::var("COMPOSIO_MCP_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty());
    let configured = file_key || env_key;
    let registry_path = composio_registry_path();
    let registry = read_file(
        &registry_path,
        COMPOSIO_REGISTRY_MAX_BYTES,
        "Composio connection registry",
    );
    let Ok(Some(raw)) = registry else {
        let mut row = reach_row(
            "composio",
            "composio",
            "Composio For You",
            if configured {
                "configured_unprobed"
            } else {
                "unavailable"
            },
            "unknown",
            if configured { "configured" } else { "missing" },
            "external_account",
        );
        row.error = if configured {
            None
        } else {
            Some("Composio consumer key is not configured".to_string())
        };
        row.metadata = Some(serde_json::json!({
            "credential_source": if file_key { "file" } else if env_key { "env" } else { "none" },
            "schema_source": "not_probed",
            "registry": "missing_or_unreadable",
        }));
        return vec![row];
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        let mut row = reach_row(
            "composio",
            "composio",
            "Composio For You",
            "unknown_error",
            "unknown",
            if configured { "configured" } else { "missing" },
            "external_account",
        );
        row.error = Some("Composio connection registry is not valid JSON".to_string());
        return vec![row];
    };
    let Some(object) = value.as_object() else {
        let mut row = reach_row(
            "composio",
            "composio",
            "Composio For You",
            "unknown_error",
            "unknown",
            if configured { "configured" } else { "missing" },
            "external_account",
        );
        row.error = Some("Composio connection registry is not an object".to_string());
        return vec![row];
    };
    if object.is_empty() {
        let mut row = reach_row(
            "composio",
            "composio",
            "Composio For You",
            if configured {
                "configured_unprobed"
            } else {
                "unavailable"
            },
            "unknown",
            if configured { "configured" } else { "missing" },
            "external_account",
        );
        row.metadata = Some(serde_json::json!({
            "credential_source": if file_key { "file" } else if env_key { "env" } else { "none" },
            "registry_entries": 0,
            "schema_source": "not_probed",
        }));
        return vec![row];
    }
    let mut rows = Vec::new();
    for (toolkit, state) in object.iter().take(MAX_TOOLKITS) {
        let state_text = state.as_str().unwrap_or("unknown").to_ascii_lowercase();
        let mut row = reach_row(
            format!("composio:{toolkit}"),
            "composio_toolkit",
            toolkit,
            if !configured {
                "unavailable"
            } else if state_text == "failed" {
                "unavailable"
            } else {
                // The registry records account state, not a current network
                // reachability proof. Keep this honest until a runtime-owned
                // probe records an observation with a timestamp.
                "configured_unprobed"
            },
            "unknown",
            if configured { "configured" } else { "missing" },
            "external_account",
        );
        row.error = (state_text == "failed")
            .then(|| "last recorded Composio connection state is failed".to_string());
        row.metadata = Some(serde_json::json!({
            "connection_state": state_text,
            "schema_source": "not_probed",
            "credential_source": if file_key { "file" } else if env_key { "env" } else { "none" },
        }));
        rows.push(row);
    }
    rows
}

fn browser_row(config: Option<&toml::Value>) -> ReachRow {
    let browser = config
        .and_then(|root| root.get("profile"))
        .and_then(|profile| profile.get("browser"));
    let configured = browser.is_some()
        || std::env::var("PHOENIX_BROWSER_ATTACH")
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
        || std::env::var("PHOENIX_BROWSER_SOURCE")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
    let mut row = reach_row(
        "browser",
        "browser",
        "Phoenix browser",
        if configured {
            "configured_unprobed"
        } else {
            // Runtime can launch its default browser even without an explicit
            // profile. This is an unobserved capability, not an outage.
            "configured_unprobed"
        },
        "unknown",
        "unknown",
        "browser_session",
    );
    row.safe_probe = "browser_status_read_only".to_string();
    row.metadata = Some(serde_json::json!({
        "configured_profile": configured,
        "status_source": "not_probed_in_phoenix",
        "schema_source": "not_probed",
    }));
    row
}

fn computer_row() -> ReachRow {
    let display = std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let wayland = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let configured = display.is_some() || wayland.is_some();
    let mut row = reach_row(
        "computer",
        "computer",
        "Desktop control",
        if configured {
            "configured_unprobed"
        } else {
            "unavailable"
        },
        "unknown",
        "not_applicable",
        "desktop_side_effect",
    );
    row.safe_probe = "computer_status_read_only".to_string();
    if !configured {
        row.error = Some(
            "DISPLAY/WAYLAND_DISPLAY is not present; desktop control is not observed".to_string(),
        );
    }
    row.metadata = Some(serde_json::json!({
        "display_present": display.is_some(),
        "wayland_present": wayland.is_some(),
        "status_source": "environment_only",
    }));
    row
}

/// Build a read-only capability/account/reachability matrix. No schema or
/// account endpoint is called here. Runtime observations can later be merged
/// into this shape by adding a timestamped row with `probe_performed=true`;
/// until then every configured external capability remains
/// `configured_unprobed`.
#[tauri::command]
pub fn agent_reach_snapshot() -> Result<AgentReachSnapshot, String> {
    let config = config_snapshot();
    let mut rows = if config.status.readable && config.status.parseable {
        local_mcp_rows(config.value.as_ref())
    } else {
        let mut row = reach_row(
            "mcp:configuration",
            "mcp",
            "MCP configuration",
            "unknown_error",
            "unknown",
            "unknown",
            "unknown",
        );
        row.error = config
            .status
            .error
            .clone()
            .or_else(|| Some("config.toml is unavailable; MCP reach is unknown".to_string()));
        row.metadata = Some(serde_json::json!({"schema_source": "not_probed"}));
        vec![row]
    };
    rows.extend(composio_rows());
    rows.push(browser_row(config.value.as_ref()));
    rows.push(computer_row());
    Ok(AgentReachSnapshot {
        contract_version: REACH_CONTRACT_VERSION,
        generated_at: unix_now(),
        probe_policy: "read_only_no_network_or_process_probe",
        status_vocabulary: REACH_STATUS_VOCABULARY
            .iter()
            .map(|status| (*status).to_string())
            .collect(),
        rows,
    })
}
