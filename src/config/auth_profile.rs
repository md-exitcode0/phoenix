use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{LLMAuthConfig, LLMProfile};
use crate::debug_session; // binary + lib crate root
use crate::providers::contracts::ProviderAuthIdentity;
use crate::providers::providers_data::ProviderModels;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthProfileState {
    #[serde(default)]
    pub order: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub last_good: HashMap<String, String>,
    #[serde(default)]
    pub cooldown_until: HashMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AuthProfileCredential {
    #[serde(rename = "api_key")]
    ApiKey {
        provider: String,
        key: String,
        display_name: Option<String>,
    },
    #[serde(rename = "token")]
    Token {
        provider: String,
        token: String,
        expires: Option<i64>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        provider: String,
        access: String,
        refresh: Option<String>,
        expires: i64,
        email: Option<String>,
    },
}

/// One lane's pick inside a profile: which model this account runs there and
/// (optionally) at which reasoning effort.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleAssignment {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfileStore {
    pub version: u32,
    #[serde(default)]
    pub profiles: HashMap<String, AuthProfileCredential>,
    /// Preferred model per profile id (optional). A fallback link uses this
    /// model when set; otherwise the role's configured model (same provider)
    /// or the provider's recommended model (different provider).
    /// Legacy single-model pin — `assignments` wins when it has the lane.
    #[serde(default)]
    pub models: HashMap<String, String>,
    /// User-facing account names, independent of credential identity.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    /// Per-lane picks per profile: profile id → lane (role or agent name) →
    /// model + effort. Written by the profile wizard; consulted by fallback
    /// links before the legacy `models` pin.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub assignments: HashMap<String, HashMap<String, RoleAssignment>>,
    #[serde(default)]
    pub state: AuthProfileState,
    /// Monotonic identity generation per auth-profile id. Entries survive
    /// profile deletion as tombstones, so removing and later reusing the same
    /// id can never revive opaque provider state created by the old account.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profile_auth_epochs: HashMap<String, u64>,
}

impl Default for AuthProfileStore {
    fn default() -> Self {
        Self {
            version: 1,
            profiles: HashMap::new(),
            models: HashMap::new(),
            labels: HashMap::new(),
            assignments: HashMap::new(),
            state: AuthProfileState::default(),
            profile_auth_epochs: HashMap::new(),
        }
    }
}

impl AuthProfileStore {
    /// Legacy stores predate explicit account generations. Epoch 1 is their
    /// stable baseline; the centralized writer persists it on the next update.
    pub fn auth_epoch_for_profile(&self, profile_id: &str) -> u64 {
        self.profile_auth_epochs
            .get(profile_id)
            .copied()
            .filter(|epoch| *epoch > 0)
            .unwrap_or(1)
    }

    /// The (model, effort) a profile should run for `lane`: exact lane →
    /// "specialist" umbrella (for agent lanes) → legacy single-model pin.
    pub fn assignment_for(&self, profile_id: &str, lane: &str) -> Option<RoleAssignment> {
        let lanes = self.assignments.get(profile_id);
        if let Some(lanes) = lanes {
            if let Some(hit) = lanes.get(lane) {
                return Some(hit.clone());
            }
            // Agent lanes ("coder") fall back to this profile's shared
            // specialist pick — the wizard writes one when "All specialists"
            // was selected.
            if let Some(shared) = lanes.get("specialist") {
                return Some(shared.clone());
            }
        }
        self.models.get(profile_id).map(|model| RoleAssignment {
            model: model.clone(),
            effort: None,
        })
    }

    /// Every stored profile id belonging to `provider_id`, sorted. One
    /// provider holds as MANY independent accounts as the user has logins —
    /// the store is keyed by profile id, never by provider, so `nvidia:default`,
    /// `nvidia:2` and `nvidia:work` each keep their own credential. This is
    /// what makes a pool of free NVIDIA NIM accounts add up to real capacity.
    pub fn profiles_for_provider(&self, provider_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .profiles
            .iter()
            .filter(|(_, credential)| profile_provider_id(credential) == provider_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// The next unused profile id for a provider: `<provider>:default`, then
/// `<provider>:2`, `:3`, … A user adding five NVIDIA NIM accounts (or five
/// SuperGrok subscriptions) gets five distinct slots instead of five writes to
/// the same key. Pure — the store is passed in, so both the interactive wizard
/// and the headless CLI share one definition and it is unit-testable without
/// touching the user's real `~/.phoenix/auth-profiles.json`.
pub fn next_free_profile_id_in(store: &AuthProfileStore, provider_id: &str) -> String {
    let base = format!("{provider_id}:default");
    if !store.profiles.contains_key(&base) {
        return base;
    }
    for n in 2..1000 {
        let candidate = format!("{provider_id}:{n}");
        if !store.profiles.contains_key(&candidate) {
            return candidate;
        }
    }
    format!("{provider_id}:{}", chrono::Utc::now().timestamp_millis())
}

/// The provider id a stored credential belongs to.
pub fn profile_provider_id(credential: &AuthProfileCredential) -> &str {
    credential_provider(credential)
}

/// True while `profile_id` is in a persisted cooldown window (an account the
/// rotation layer benched after a quota/auth failure).
pub fn profile_cooled_down(state: &AuthProfileState, profile_id: &str, now_ms: i64) -> bool {
    state
        .cooldown_until
        .get(profile_id)
        .is_some_and(|until| *until > now_ms)
}

/// Persist a cooldown for an exhausted account and remember the survivor that
/// answered for this role. Best-effort: rotation must keep working even if the
/// store write fails.
pub fn record_rotation(role: &str, exhausted: Option<(&str, i64)>, active: Option<&str>) {
    if crate::config::test_isolated_from_live_home() {
        return; // durable tests opt in only through an isolated PHOENIX_HOME
    }
    let _ = update_auth_profile_store(|store| {
        if let Some((profile_id, until_ms)) = exhausted {
            store
                .state
                .cooldown_until
                .insert(profile_id.to_string(), until_ms);
        }
        if let Some(profile_id) = active {
            store
                .state
                .last_good
                .insert(role.to_string(), profile_id.to_string());
        }
        Ok(())
    });
}

pub fn auth_profiles_path() -> Result<PathBuf> {
    Ok(auth_profiles_path_in(&crate::config::phoenix_home()))
}

fn auth_profiles_path_in(phoenix_home: &std::path::Path) -> PathBuf {
    phoenix_home.join("auth-profiles.json")
}

pub fn load_auth_profile_store() -> Result<AuthProfileStore> {
    // A test build with no PHOENIX_HOME must not read the user's real
    // credentials. Without this, connecting a provider for real silently
    // changed unit-test outcomes: `reports_missing_openrouter_auth` started
    // failing the moment an openrouter key landed in the live store, because
    // the test stubs the ENV lookup but the factory still fell through to
    // ~/.phoenix/auth-profiles.json. Tests that want a real store point
    // PHOENIX_HOME at a tempdir.
    if crate::config::test_isolated_from_live_home() {
        return Ok(AuthProfileStore::default());
    }
    let path = auth_profiles_path()?;
    load_auth_profile_store_from_path(&path)
}

fn load_auth_profile_store_from_path(path: &std::path::Path) -> Result<AuthProfileStore> {
    match crate::config::private_io::read_private_file(path)? {
        Some(raw) => {
            serde_json::from_slice(&raw).with_context(|| format!("Invalid {}", path.display()))
        }
        None => Ok(AuthProfileStore::default()),
    }
}

#[cfg(test)]
fn save_auth_profile_store_to(path: &std::path::Path, store: &AuthProfileStore) -> Result<PathBuf> {
    let raw = serde_json::to_string_pretty(store)?;
    // Atomic replace: this file holds EVERY credential and is written from
    // several processes (gateway rotation benching, the CLI, the wizard). A
    // plain write killed mid-flight leaves invalid JSON on disk permanently —
    // every load then fails and every authenticated lane dies until the file
    // is repaired by hand.
    crate::config::private_io::atomic_write_private(path, format!("{raw}\n").as_bytes())?;
    Ok(path.to_path_buf())
}

fn update_auth_profile_store_at<T>(
    path: &std::path::Path,
    update: impl FnOnce(&mut AuthProfileStore) -> Result<T>,
) -> Result<T> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let mut store = match current {
            Some(raw) => serde_json::from_slice(raw)
                .with_context(|| format!("Invalid {}", path.display()))?,
            None => AuthProfileStore::default(),
        };
        let previous_profiles = store.profiles.clone();
        let result = update(&mut store)?;
        reconcile_profile_auth_epochs(&previous_profiles, &mut store)?;
        let raw = format!("{}\n", serde_json::to_string_pretty(&store)?).into_bytes();
        Ok((result, raw))
    })
}

fn same_profile_account(previous: &AuthProfileCredential, current: &AuthProfileCredential) -> bool {
    match (previous, current) {
        (
            AuthProfileCredential::ApiKey {
                provider: previous_provider,
                key: previous_key,
                ..
            },
            AuthProfileCredential::ApiKey {
                provider: current_provider,
                key: current_key,
                ..
            },
        ) => previous_provider == current_provider && previous_key == current_key,
        (
            AuthProfileCredential::Token {
                provider: previous_provider,
                token: previous_token,
                ..
            },
            AuthProfileCredential::Token {
                provider: current_provider,
                token: current_token,
                ..
            },
        ) => previous_provider == current_provider && previous_token == current_token,
        (
            AuthProfileCredential::OAuth {
                provider: previous_provider,
                access: previous_access,
                refresh: previous_refresh,
                ..
            },
            AuthProfileCredential::OAuth {
                provider: current_provider,
                access: current_access,
                refresh: current_refresh,
                ..
            },
        ) => {
            previous_provider == current_provider
                && (previous_access == current_access
                    || (previous_refresh.is_some() && previous_refresh == current_refresh)
                    || (previous_provider == "openai-codex"
                        && crate::providers::openai_codex::codex_oauth_identity(previous_access).is_some()
                        && crate::providers::openai_codex::codex_oauth_identity(previous_access)
                            == crate::providers::openai_codex::codex_oauth_identity(current_access)))
        }
        _ => false,
    }
}

fn advance_profile_auth_epoch(store: &mut AuthProfileStore, profile_id: &str) -> Result<()> {
    let current = store
        .profile_auth_epochs
        .get(profile_id)
        .copied()
        .unwrap_or(0);
    let next = current
        .checked_add(1)
        .context("auth profile epoch overflow")?
        .max(1);
    store
        .profile_auth_epochs
        .insert(profile_id.to_string(), next);
    Ok(())
}

fn ensure_profile_auth_epoch(store: &mut AuthProfileStore, profile_id: &str) {
    let epoch = store
        .profile_auth_epochs
        .entry(profile_id.to_string())
        .or_insert(1);
    if *epoch == 0 {
        *epoch = 1;
    }
}

/// Reconcile account generations around every credential-store mutation.
/// Runtime state writes (cooldowns, last-good, assignments) leave epochs
/// unchanged. Credential replacement advances exactly once under the same
/// cross-process lock as the credential write.
fn reconcile_profile_auth_epochs(
    previous_profiles: &HashMap<String, AuthProfileCredential>,
    store: &mut AuthProfileStore,
) -> Result<()> {
    let mut profile_ids: Vec<String> = previous_profiles
        .keys()
        .chain(store.profiles.keys())
        .cloned()
        .collect();
    profile_ids.sort();
    profile_ids.dedup();

    for profile_id in profile_ids {
        // Clone only the current credential so the match never holds an
        // immutable borrow into `store` while advancing its epoch map.
        let current = store.profiles.get(&profile_id).cloned();
        if previous_profiles.get(&profile_id) != current.as_ref() {
            // A newly connected or successfully refreshed credential gets a
            // fresh attempt immediately, even if the old token was benched.
            store.state.cooldown_until.remove(&profile_id);
        }
        match (previous_profiles.get(&profile_id), current.as_ref()) {
            (None, Some(_)) => {
                // A retained entry is a deletion tombstone. Normalize an old
                // or manually edited zero before advancing so profile reuse
                // cannot return to the legacy epoch-1 identity.
                if store.profile_auth_epochs.contains_key(&profile_id) {
                    ensure_profile_auth_epoch(store, &profile_id);
                }
                advance_profile_auth_epoch(store, &profile_id)?;
            }
            (Some(previous), Some(current)) if !same_profile_account(previous, current) => {
                // A legacy profile with no stored epoch starts at 1; replacing
                // it must therefore land at 2, not reuse the legacy epoch.
                ensure_profile_auth_epoch(store, &profile_id);
                advance_profile_auth_epoch(store, &profile_id)?;
            }
            (Some(_), Some(_)) | (Some(_), None) => {
                ensure_profile_auth_epoch(store, &profile_id);
            }
            (None, None) => unreachable!("profile id came from neither profile map"),
        }
    }
    Ok(())
}

/// Mutate the credential store while holding its cross-process lock from the
/// read through the durable rename. Callers provide a delta, not a stale
/// whole-store snapshot, so concurrent profile additions and cooldown updates
/// cannot erase one another.
pub fn update_auth_profile_store<T>(
    update: impl FnOnce(&mut AuthProfileStore) -> Result<T>,
) -> Result<(PathBuf, T)> {
    if crate::config::test_isolated_from_live_home() {
        anyhow::bail!("tests must set PHOENIX_HOME before mutating auth profiles");
    }
    let path = auth_profiles_path()?;
    let result = update_auth_profile_store_at(&path, update)?;
    Ok((path, result))
}

#[derive(Debug, Clone)]
pub struct ResolvedAuth {
    pub method: String,
    pub source: String,
    pub env_var: Option<String>,
    pub profile_id: Option<String>,
    pub credential: Option<String>,
    pub auth_identity: ProviderAuthIdentity,
}

impl ResolvedAuth {
    pub fn source_summary(&self) -> String {
        match self.source.as_str() {
            "env" => self
                .env_var
                .as_ref()
                .map(|env_var| format!("env:{env_var}"))
                .unwrap_or_else(|| "env".to_string()),
            "profile" => self
                .profile_id
                .as_ref()
                .map(|profile| format!("profile:{profile}"))
                .unwrap_or_else(|| "profile".to_string()),
            other => other.to_string(),
        }
    }
}

/// Env-backed credentials have no durable profile generation to advance when
/// the environment changes. Give every provider built by this process one
/// stable random epoch; a gateway restart then invalidates persisted opaque
/// replay and safely falls back to the portable transcript.
fn process_auth_epoch() -> u64 {
    static PROCESS_AUTH_EPOCH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *PROCESS_AUTH_EPOCH.get_or_init(|| {
        let bytes = uuid::Uuid::new_v4();
        let mut epoch_bytes = [0_u8; 8];
        epoch_bytes.copy_from_slice(&bytes.as_bytes()[..8]);
        u64::from_le_bytes(epoch_bytes).max(1)
    })
}

/// Stable for repeated resolutions of the same environment credential, but
/// advances when that variable's value changes inside a long-lived process.
/// Only a SHA-256 digest is retained; neither the credential nor its digest is
/// serialized, logged, or used as the public account scope.
fn env_auth_epoch(env_var: &str, credential: &str) -> u64 {
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static ENV_AUTH_EPOCHS: OnceLock<Mutex<HashMap<String, ([u8; 32], u64)>>> = OnceLock::new();
    let digest: [u8; 32] = Sha256::digest(credential.as_bytes()).into();
    let epochs = ENV_AUTH_EPOCHS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut epochs = epochs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = epochs
        .entry(env_var.to_string())
        .or_insert_with(|| (digest, process_auth_epoch()));
    if entry.0 != digest {
        entry.0 = digest;
        entry.1 = entry.1.checked_add(1).unwrap_or_else(process_auth_epoch);
    }
    entry.1.max(1)
}

fn provider_auth_identity(scope: impl Into<String>, epoch: u64) -> Result<ProviderAuthIdentity> {
    ProviderAuthIdentity::try_new(scope, epoch)
}

fn scoped_provider_auth_identity(
    source: &str,
    source_label: &str,
    epoch: u64,
) -> Result<ProviderAuthIdentity> {
    let readable_scope = format!("{source}:{source_label}");
    match ProviderAuthIdentity::try_new(readable_scope, epoch) {
        Ok(identity) => Ok(identity),
        Err(_) => {
            // Profile names are user-entered and historically had no character
            // or length restriction. Keep normal ids readable, but map an odd
            // label to a fixed non-secret identifier instead of making an
            // otherwise valid provider login fail route construction.
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(source_label.as_bytes());
            ProviderAuthIdentity::try_new(format!("{source}:sha256:{digest:x}"), epoch)
        }
    }
}

pub(crate) fn profile_auth_identity(profile_id: &str, epoch: u64) -> Result<ProviderAuthIdentity> {
    scoped_provider_auth_identity("profile", profile_id, epoch)
}

fn log_resolved_auth(provider_id: &str, auth: &ResolvedAuth) {
    // #region agent log
    debug_session::log(
        "A",
        "auth_profile.rs:resolve_llm_auth",
        "auth resolved",
        serde_json::json!({
            "provider_id": provider_id,
            "source": auth.source,
            "method": auth.method,
            "env_var": auth.env_var,
            "profile_id": auth.profile_id,
            "has_credential": auth.credential.as_ref().is_some_and(|c| !c.is_empty()),
        }),
    );
    // #endregion
}

pub fn resolve_llm_auth(
    llm: &LLMProfile,
    provider: &ProviderModels,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedAuth> {
    let resolved = resolve_llm_auth_inner(llm, provider, env_lookup)?;
    log_resolved_auth(provider.id, &resolved);
    Ok(resolved)
}

fn resolve_llm_auth_inner(
    llm: &LLMProfile,
    provider: &ProviderModels,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedAuth> {
    if let Some(auth) = &llm.auth {
        return resolve_declared_auth(auth, provider, env_lookup);
    }

    if let Some((env_var, value)) = resolve_env_candidate(
        provider
            .env_vars
            .iter()
            .map(|env_var| (*env_var).to_string()),
        &env_lookup,
    ) {
        return Ok(ResolvedAuth {
            method: "api".to_string(),
            source: "env".to_string(),
            auth_identity: scoped_provider_auth_identity(
                "env",
                &env_var,
                env_auth_epoch(&env_var, &value),
            )?,
            env_var: Some(env_var),
            profile_id: None,
            credential: Some(value),
        });
    }

    if let Some((profile_id, credential, auth_epoch)) = resolve_legacy_profile(provider)? {
        return Ok(ResolvedAuth {
            method: legacy_profile_method(&credential).to_string(),
            source: "profile".to_string(),
            env_var: None,
            profile_id: Some(profile_id.clone()),
            credential: Some(extract_profile_secret(&credential)?),
            auth_identity: profile_auth_identity(&profile_id, auth_epoch)?,
        });
    }

    Ok(ResolvedAuth {
        method: "none".to_string(),
        source: "none".to_string(),
        env_var: None,
        profile_id: None,
        credential: None,
        auth_identity: provider_auth_identity("none", process_auth_epoch())?,
    })
}

fn resolve_declared_auth(
    auth: &LLMAuthConfig,
    provider: &ProviderModels,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedAuth> {
    let method = auth
        .method
        .clone()
        .unwrap_or_else(|| default_method_for_provider(provider).to_string());
    let source = auth.source.clone().unwrap_or_else(|| "env".to_string());
    let declared_method = provider.find_auth_method(&method).with_context(|| {
        format!(
            "Provider {} does not support auth method '{}' in the current Phoenix catalog",
            provider.id, method
        )
    })?;
    if !declared_method.llm_supported {
        anyhow::bail!(
            "Auth method '{}' is disabled for provider {}: {}",
            declared_method.label,
            provider.id,
            declared_method
                .unsupported_reason
                .unwrap_or("not supported by Phoenix runtime for LLM requests")
        );
    }

    match source.as_str() {
        "env" => {
            let env_candidates = env_candidates_for_method(provider, auth, &method);
            let (env_var, value) = resolve_env_candidate(env_candidates.into_iter(), env_lookup)
                .with_context(|| {
                    format!(
                        "Missing required environment variable for provider {} auth method {}",
                        provider.id, method
                    )
                })?;
            Ok(ResolvedAuth {
                method,
                source,
                auth_identity: scoped_provider_auth_identity(
                    "env",
                    &env_var,
                    env_auth_epoch(&env_var, &value),
                )?,
                env_var: Some(env_var),
                profile_id: None,
                credential: Some(value),
            })
        }
        "profile" => {
            let profile_id = match auth.profile.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
                Some(id) => id.to_string(),
                None => resolve_legacy_profile(provider)?
                    .map(|(id, _, _)| id)
                    .with_context(|| format!("No connected account for provider {}", provider.id))?,
            };
            let store = load_auth_profile_store()?;
            let credential = store.profiles.get(&profile_id).with_context(|| {
                format!(
                    "Auth profile '{}' was not found in ~/.phoenix/auth-profiles.json",
                    profile_id
                )
            })?;
            validate_profile_provider(provider, &profile_id, credential)?;
            let auth_epoch = store.auth_epoch_for_profile(&profile_id);
            Ok(ResolvedAuth {
                method,
                source,
                env_var: None,
                profile_id: Some(profile_id.clone()),
                credential: Some(extract_profile_secret(credential)?),
                auth_identity: profile_auth_identity(&profile_id, auth_epoch)?,
            })
        }
        "none" => Ok(ResolvedAuth {
            method,
            source,
            env_var: None,
            profile_id: None,
            credential: None,
            auth_identity: provider_auth_identity("none", process_auth_epoch())?,
        }),
        other => anyhow::bail!(
            "Unsupported auth source '{}' for provider {}. Expected env, profile, or none.",
            other,
            provider.id
        ),
    }
}

fn resolve_env_candidate(
    candidates: impl IntoIterator<Item = String>,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    for env_var in candidates {
        if let Some(value) = env_lookup(&env_var) {
            return Some((env_var, value));
        }
    }
    None
}

fn env_candidates_for_method(
    provider: &ProviderModels,
    auth: &LLMAuthConfig,
    method: &str,
) -> Vec<String> {
    let mut candidates = Vec::new();

    if let Some(env_var) = auth.env_var.as_deref() {
        candidates.push(env_var.to_string());
    }

    if let Some(auth_method) = provider.auth_methods.iter().find(|m| {
        m.method_type.eq_ignore_ascii_case(method)
            || (method.eq_ignore_ascii_case("api") && m.method_type == "api")
    }) {
        if let Some(env_var) = auth_method.env_var {
            candidates.push(env_var.to_string());
        }
    }

    candidates.extend(
        provider
            .env_vars
            .iter()
            .map(|env_var| (*env_var).to_string()),
    );
    dedupe_strs(candidates)
}

fn resolve_legacy_profile(
    provider: &ProviderModels,
) -> Result<Option<(String, AuthProfileCredential, u64)>> {
    let store = load_auth_profile_store()?;
    let mut candidates: Vec<_> = store
        .profiles
        .iter()
        .filter(|(_, credential)| auth_compatible(credential_provider(credential), provider.id))
        .map(|(id, credential)| {
            (
                id.clone(),
                credential.clone(),
                store.auth_epoch_for_profile(id),
            )
        })
        .collect();

    // An EXACT provider match wins over a compatible-alias match, so grok-cli
    // prefers a grok-cli:* profile but still works off an xai:* one.
    candidates.sort_by(|(a, ca, _), (b, cb, _)| {
        let exact = |c: &AuthProfileCredential| credential_provider(c) == provider.id;
        exact(cb).cmp(&exact(ca))
            .then_with(|| profile_cooled_down(&store.state, a, chrono::Utc::now().timestamp_millis())
                .cmp(&profile_cooled_down(&store.state, b, chrono::Utc::now().timestamp_millis())))
            .then_with(|| {
                let expired = |credential: &AuthProfileCredential| match credential {
                    AuthProfileCredential::OAuth { expires, .. } => *expires <= chrono::Utc::now().timestamp_millis(),
                    AuthProfileCredential::Token { expires, .. } => expires.is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp_millis()),
                    AuthProfileCredential::ApiKey { .. } => false,
                };
                expired(ca).cmp(&expired(cb))
            })
            .then_with(|| a.cmp(b))
    });
    let mut last_error = None;
    for candidate in candidates {
        match extract_profile_secret(&candidate.1) {
            Ok(_) => return Ok(Some(candidate)),
            Err(error) => last_error = Some(error),
        }
    }
    if let Some(error) = last_error { return Err(error.context("no usable stored provider account")); }
    Ok(None)
}

/// Two provider ids that share the SAME credential (same OIDC login), so a
/// profile stored under one can auth the other. `xai` and `grok-cli` are the
/// same SuperGrok/X login — the console API (`api.x.ai`) vs the CLI proxy
/// (`cli-chat-proxy.grok.com`) — so an `xai:*` profile authorizes the grok-cli
/// lane and vice versa. This is why adding a SuperGrok account under the xAI
/// card now powers the grok-cli main lane.
fn auth_compatible(stored: &str, wanted: &str) -> bool {
    if stored == wanted {
        return true;
    }
    matches!((stored, wanted), ("xai", "grok-cli") | ("grok-cli", "xai"))
}

fn legacy_profile_method(credential: &AuthProfileCredential) -> &'static str {
    match credential {
        AuthProfileCredential::ApiKey { .. } => "api",
        AuthProfileCredential::Token { .. } => "oauth",
        AuthProfileCredential::OAuth { .. } => "oauth",
    }
}

pub(crate) fn extract_profile_secret(credential: &AuthProfileCredential) -> Result<String> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    match credential {
        AuthProfileCredential::ApiKey { key, .. } => Ok(key.clone()),
        AuthProfileCredential::Token { token, expires, .. } => {
            if expires.is_some_and(|expiry| expiry <= now_ms) {
                anyhow::bail!("Stored auth token is expired");
            }
            Ok(token.clone())
        }
        AuthProfileCredential::OAuth {
            provider,
            access,
            refresh,
            expires,
            ..
        } => {
            // Near-expiry tokens refresh IN PLACE when the provider lane
            // knows how (xai/SuperGrok): the refresh token is the durable
            // credential and the access token rotates hourly — bailing like
            // the codex lane does would demand a re-login every hour.
            if *expires <= now_ms + OAUTH_REFRESH_MARGIN_MS {
                if let Some(rt) = refresh.as_deref() {
                    match refresh_oauth_access(provider, access, rt) {
                        Ok(Some(fresh)) => return Ok(fresh),
                        Err(error) if *expires <= now_ms => return Err(error.context("OAuth access expired and automatic renewal failed")),
                        // A temporary early refresh failure must not discard
                        // an access token that is still valid.
                        _ => {}
                    }
                }
            }
            if *expires <= now_ms {
                anyhow::bail!("Stored OAuth token is expired");
            }
            Ok(access.clone())
        }
    }
}

/// Re-read a bound OAuth account for every request. A new login to a different
/// account must never inherit an old route's opaque conversation state.
pub(crate) fn resolve_bound_profile_secret(profile_id: &str, epoch: u64, force_refresh: bool) -> Result<String> {
    let store = load_auth_profile_store()?;
    anyhow::ensure!(store.auth_epoch_for_profile(profile_id) == epoch,
        "OAuth account changed; rebuild the provider route before retrying");
    let credential = store.profiles.get(profile_id).context("OAuth account was removed")?;
    if force_refresh {
        if let AuthProfileCredential::OAuth { provider, access, refresh: Some(refresh), .. } = credential {
            if let Some(fresh) = refresh_oauth_access(provider, access, refresh)? { return Ok(fresh); }
        }
    }
    extract_profile_secret(credential)
}

/// Refresh access tokens this long before they actually expire, so a token
/// never dies mid-turn.
const OAUTH_REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// Providers whose OAuth access tokens Phoenix can rotate itself. The token
/// endpoint is re-derived from OIDC discovery with a static fallback.
struct OAuthRefreshEndpoints {
    discovery_url: &'static str,
    fallback_token_endpoint: &'static str,
    token_host: &'static str,
    client_id: &'static str,
}

/// The registered public Codex OAuth client used by both setup login flows and
/// access-token rotation. Keeping it here prevents login and refresh from
/// silently drifting onto different OAuth clients.
pub(crate) const OPENAI_CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// The registered public grok-cli OAuth client (shared with the setup login
/// flow) — SuperGrok / X Premium subscription auth.
pub(crate) const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

const XAI_OAUTH_REFRESH: OAuthRefreshEndpoints = OAuthRefreshEndpoints {
    discovery_url: "https://auth.x.ai/.well-known/openid-configuration",
    fallback_token_endpoint: "https://auth.x.ai/oauth/token",
    token_host: "auth.x.ai",
    client_id: XAI_OAUTH_CLIENT_ID,
};

const OPENAI_CODEX_OAUTH_REFRESH: OAuthRefreshEndpoints = OAuthRefreshEndpoints {
    discovery_url: "https://auth.openai.com/.well-known/openid-configuration",
    fallback_token_endpoint: "https://auth.openai.com/oauth/token",
    token_host: "auth.openai.com",
    client_id: OPENAI_CODEX_OAUTH_CLIENT_ID,
};

fn oauth_refresh_endpoints(provider: &str) -> Option<&'static OAuthRefreshEndpoints> {
    match provider {
        "openai-codex" => Some(&OPENAI_CODEX_OAUTH_REFRESH),
        // Both xAI lanes share the one OIDC client — the SuperGrok CLI proxy
        // lane (grok-cli) rotates its token exactly like the console lane (xai).
        "xai" | "grok-cli" => Some(&XAI_OAUTH_REFRESH),
        _ => None,
    }
}

#[derive(Debug)]
struct RotatedOAuthToken {
    access: String,
    refresh: Option<String>,
    expires_in_seconds: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct OAuthRefreshReceipt {
    version: u32,
    access: String,
    expires_at_ms: i64,
}

fn oauth_refresh_identity(provider: &str, old_access: &str, refresh_token: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update(provider.as_bytes());
    digest.update([0]);
    digest.update(old_access.as_bytes());
    digest.update([0]);
    digest.update(refresh_token.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn parse_refresh_json(
    response: reqwest::blocking::Response,
    label: &str,
) -> Result<serde_json::Value> {
    use std::io::Read;

    const MAX_BYTES: u64 = 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES)
    {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        anyhow::bail!("{label} exceeds the 1 MiB response limit");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label}"))
}

fn valid_refresh_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64 * 1024
        && !value
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
}

// Never include raw token-endpoint responses: they can contain secrets.
fn oauth_refresh_failure(status: u16, body: Option<&serde_json::Value>) -> String {
    let code = body.and_then(|body| body.get("error"))
        .and_then(|error| error.as_str().or_else(|| error.get("code").and_then(|code| code.as_str())));
    let reason = match code {
        Some("refresh_token_reused") => "the refresh token was already used; sign in again",
        Some("refresh_token_expired") => "the refresh token expired; sign in again",
        Some("refresh_token_invalidated") => "the provider revoked this login; sign in again",
        Some("invalid_grant") => "the provider rejected the saved login; sign in again",
        Some("invalid_client") => "the provider rejected Phoenix's OAuth client",
        _ => "automatic sign-in renewal failed",
    };
    format!("OAuth token refresh returned HTTP {status}: {reason}")
}

/// Only known terminal login failures require reauthentication. A generic
/// token-endpoint 400/503 or network failure must not bench a healthy login.
pub(crate) fn is_oauth_login_rejected(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("oauth token refresh returned http ")
        && [
            "the refresh token was already used; sign in again",
            "the refresh token expired; sign in again",
            "the provider revoked this login; sign in again",
            "the provider rejected the saved login; sign in again",
        ].iter().any(|reason| lower.contains(reason))
}

fn rotate_oauth_over_network(
    endpoints: &'static OAuthRefreshEndpoints,
    refresh_token: &str,
) -> Result<RotatedOAuthToken> {
    if !valid_refresh_secret(refresh_token) {
        anyhow::bail!("stored OAuth refresh token is empty, oversized, or malformed");
    }
    let refresh_owned = refresh_token.to_string();
    let client_id = endpoints.client_id;
    let discovery = endpoints.discovery_url;
    let fallback = endpoints.fallback_token_endpoint;
    std::thread::spawn(move || -> Result<RotatedOAuthToken> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build OAuth refresh client")?;
        let token_endpoint = client
            .get(discovery)
            .send()
            .ok()
            .filter(|response| response.status().is_success())
            .and_then(|response| parse_refresh_json(response, "OAuth discovery response").ok())
            .and_then(|value| {
                value
                    .get("token_endpoint")
                    .and_then(|endpoint| endpoint.as_str())
                    .filter(|endpoint| endpoint.len() <= 8 * 1024)
                    .and_then(|endpoint| url::Url::parse(endpoint).ok())
                    // Discovery is untrusted network input. Accept only HTTPS
                    // on the exact provider auth host associated with this
                    // credential; otherwise use the compile-time fallback.
                    .filter(|endpoint| {
                        endpoint.scheme() == "https"
                            && endpoint.host_str() == Some(endpoints.token_host)
                    })
                    .map(|endpoint| endpoint.to_string())
            })
            .unwrap_or_else(|| fallback.to_string());
        let response = client
            .post(&token_endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_owned.as_str()),
            ])
            .send()
            .context("OAuth token refresh request failed")?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = parse_refresh_json(response, "OAuth token refresh error").ok();
            anyhow::bail!("{}", oauth_refresh_failure(status, body.as_ref()));
        }
        let value = parse_refresh_json(response, "OAuth token refresh response")?;
        let access = value
            .get("access_token")
            .and_then(|access| access.as_str())
            .context("OAuth refresh response omitted access_token")?
            .to_string();
        if !valid_refresh_secret(&access) {
            anyhow::bail!("OAuth refresh response returned an invalid access token");
        }
        let refresh = value
            .get("refresh_token")
            .and_then(|refresh| refresh.as_str())
            .map(str::to_string);
        if refresh
            .as_deref()
            .is_some_and(|value| !valid_refresh_secret(value))
        {
            anyhow::bail!("OAuth refresh response returned an invalid refresh token");
        }
        let expires_in_seconds = value
            .get("expires_in")
            .and_then(|expires| expires.as_i64())
            .unwrap_or(3600)
            .clamp(30, 30 * 24 * 60 * 60);
        Ok(RotatedOAuthToken {
            access,
            refresh,
            expires_in_seconds,
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("OAuth refresh worker panicked"))?
}

fn refresh_oauth_access_at(
    home: &std::path::Path,
    provider: &str,
    old_access: &str,
    refresh_token: &str,
    now_ms: i64,
    rotate: impl FnOnce(&'static OAuthRefreshEndpoints, &str) -> Result<RotatedOAuthToken>,
) -> Result<Option<String>> {
    let Some(endpoints) = oauth_refresh_endpoints(provider) else {
        return Ok(None);
    };
    let identity = oauth_refresh_identity(provider, old_access, refresh_token);
    let refresh_dir = home.join("oauth-refresh");
    let guard_target = refresh_dir.join(format!("{identity}.guard"));
    let receipt_path = refresh_dir.join(format!("{identity}.json"));
    let store_path = auth_profiles_path_in(home);

    crate::config::private_io::with_private_lock(&guard_target, || {
        // A concurrent process may have completed this exact credential
        // rotation while we waited. The private receipt lets every waiter use
        // that result without replaying a one-time refresh token.
        if let Some(raw) = crate::config::private_io::read_private_file(&receipt_path)? {
            let receipt: OAuthRefreshReceipt = serde_json::from_slice(&raw)
                .with_context(|| format!("Invalid {}", receipt_path.display()))?;
            if receipt.version == 1 && receipt.expires_at_ms > now_ms {
                let store = load_auth_profile_store_from_path(&store_path)?;
                let persisted = store.profiles.values().any(|credential| {
                    matches!(
                        credential,
                        AuthProfileCredential::OAuth {
                            provider: stored_provider,
                            access,
                            expires,
                            ..
                        } if stored_provider.as_str() == provider
                            && access == &receipt.access
                            && *expires == receipt.expires_at_ms
                    )
                });
                if persisted {
                    return Ok(Some(receipt.access));
                }
            }
        }

        // Reload only after taking the credential-scoped lock. If this exact
        // old credential is gone and there is no receipt, never guess another
        // account belonging to the same provider and never refresh twice.
        let store = load_auth_profile_store_from_path(&store_path)?;
        let still_current = store.profiles.values().any(|credential| {
            matches!(
                credential,
                AuthProfileCredential::OAuth {
                    provider: stored_provider,
                    access,
                    refresh: Some(stored_refresh),
                    ..
                } if stored_provider.as_str() == provider
                    && access.as_str() == old_access
                    && stored_refresh.as_str() == refresh_token
            )
        });
        if !still_current {
            return Ok(None);
        }

        let rotated = rotate(endpoints, refresh_token)?;
        let expires_at_ms = now_ms.saturating_add(rotated.expires_in_seconds.saturating_mul(1000));
        let updated = update_auth_profile_store_at(&store_path, |store| {
            let mut updated = 0usize;
            for credential in store.profiles.values_mut() {
                if let AuthProfileCredential::OAuth {
                    provider: stored_provider,
                    access,
                    refresh,
                    expires,
                    ..
                } = credential
                {
                    if stored_provider.as_str() == provider
                        && access.as_str() == old_access
                        && refresh.as_deref() == Some(refresh_token)
                    {
                        *access = rotated.access.clone();
                        if let Some(new_refresh) = rotated.refresh.as_ref() {
                            *refresh = Some(new_refresh.clone());
                        }
                        *expires = expires_at_ms;
                        updated += 1;
                    }
                }
            }
            if updated == 0 {
                anyhow::bail!(
                    "OAuth credential changed before the refreshed token could be persisted"
                );
            }
            Ok(updated)
        })?;
        debug_assert!(updated > 0);

        let receipt = OAuthRefreshReceipt {
            version: 1,
            access: rotated.access.clone(),
            expires_at_ms,
        };
        let raw = format!("{}\n", serde_json::to_string(&receipt)?);
        crate::config::private_io::atomic_write_private(&receipt_path, raw.as_bytes())?;
        Ok(Some(rotated.access))
    })
}

/// Rotate a near-expiry OAuth access token and persist the result. The
/// blocking HTTP runs on a dedicated thread so callers inside a tokio
/// runtime never trip the blocking-in-async panic. Returns the fresh access
/// token, or None (caller falls through to normal expiry handling).
fn refresh_oauth_access(provider: &str, old_access: &str, refresh_token: &str) -> Result<Option<String>> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    match refresh_oauth_access_at(
        &crate::config::phoenix_home(),
        provider,
        old_access,
        refresh_token,
        now_ms,
        rotate_oauth_over_network,
    ) {
        Ok(Some(access)) => {
            crate::runtime::gwlog(&format!("auth: refreshed {provider} OAuth access token"));
            Ok(Some(access))
        }
        Ok(None) => Ok(None),
        Err(error) => {
            crate::runtime::gwlog(&format!(
                "auth: OAuth refresh for {provider} was not applied: {error:#}"
            ));
            Err(error)
        }
    }
}

fn validate_profile_provider(
    provider: &ProviderModels,
    profile_id: &str,
    credential: &AuthProfileCredential,
) -> Result<()> {
    let actual = credential_provider(credential);
    if !auth_compatible(actual, provider.id) {
        anyhow::bail!(
            "Auth profile '{}' belongs to provider '{}' but config expects '{}'",
            profile_id,
            actual,
            provider.id
        );
    }
    Ok(())
}

fn credential_provider(credential: &AuthProfileCredential) -> &str {
    match credential {
        AuthProfileCredential::ApiKey { provider, .. } => provider,
        AuthProfileCredential::Token { provider, .. } => provider,
        AuthProfileCredential::OAuth { provider, .. } => provider,
    }
}

fn default_method_for_provider(provider: &ProviderModels) -> &'static str {
    provider
        .auth_methods
        .first()
        .map(|method| method.method_type)
        .unwrap_or("none")
}

fn dedupe_strs(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_selection_without_account_pin_skips_expired_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let mut store = AuthProfileStore::default();
        for (id, expires) in [("openai-codex:2", Some(1)), ("openai-codex:4", None)] {
            store.profiles.insert(id.into(), AuthProfileCredential::Token {
                provider: "openai-codex".into(), token: "synthetic".into(), expires,
            });
        }
        save_auth_profile_store_to(&dir.path().join("auth-profiles.json"), &store).unwrap();
        let provider = crate::providers::providers_data::get_provider("openai-codex").unwrap();
        let mut auth = LLMAuthConfig { method: Some("oauth".into()), source: Some("profile".into()), profile: None, env_var: None };
        let resolved = resolve_declared_auth(&auth, &provider, |_| None).unwrap();
        assert_eq!(resolved.profile_id.as_deref(), Some("openai-codex:4"));
        auth.profile = Some("openai-codex:2".into());
        assert!(resolve_declared_auth(&auth, &provider, |_| None).unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn reconnect_clears_the_old_credentials_cooldown() {
        let id = "openai-codex:default".to_string();
        let mut store = AuthProfileStore::default();
        store.profiles.insert(id.clone(), AuthProfileCredential::Token {
            provider: "openai-codex".into(), token: "old".into(), expires: None,
        });
        store.state.cooldown_until.insert(id.clone(), i64::MAX);
        let previous = store.profiles.clone();
        store.profiles.insert(id.clone(), AuthProfileCredential::Token {
            provider: "openai-codex".into(), token: "new".into(), expires: None,
        });
        reconcile_profile_auth_epochs(&previous, &mut store).unwrap();
        assert!(!store.state.cooldown_until.contains_key(&id));
        assert_eq!(store.auth_epoch_for_profile(&id), 2);
    }

    #[test]
    fn codex_token_rotation_preserves_only_the_same_subject_and_account() {
        use base64::Engine;
        let token = |sub: &str, account: &str, nonce: i64| {
            let payload = serde_json::json!({"sub": sub, "nonce": nonce, "https://api.openai.com/auth": {"chatgpt_account_id": account}});
            format!("header.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string()))
        };
        let credential = |sub: &str, account: &str, nonce: i64| AuthProfileCredential::OAuth {
            provider: "openai-codex".into(), access: token(sub, account, nonce),
            refresh: Some(format!("rotated-{nonce}")), expires: 9999999999999, email: None,
        };
        let old = credential("user-a", "account-a", 1);
        assert!(same_profile_account(&old, &credential("user-a", "account-a", 2)));
        assert!(!same_profile_account(&old, &credential("user-b", "account-a", 2)));
        assert!(!same_profile_account(&old, &credential("user-a", "account-b", 2)));
    }

    #[test]
    fn account_labels_survive_credential_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-profiles.json");
        update_auth_profile_store_at(&path, |store| {
            store.labels.insert("openai:work".into(), "Work account".into());
            store.profiles.insert("openai:work".into(), AuthProfileCredential::ApiKey {
                provider: "openai".into(), key: "old-key".into(), display_name: None,
            });
            Ok(())
        }).unwrap();
        update_auth_profile_store_at(&path, |store| {
            if let Some(AuthProfileCredential::ApiKey { key, .. }) = store.profiles.get_mut("openai:work") {
                *key = "new-key".into();
            }
            Ok(())
        }).unwrap();
        let store = load_auth_profile_store_from_path(&path).unwrap();
        assert_eq!(store.labels.get("openai:work").map(String::as_str), Some("Work account"));
        assert!(store.profiles.contains_key("openai:work"));
        let legacy: AuthProfileStore = serde_json::from_str(r#"{"version":1,"profiles":{}}"#).unwrap();
        assert!(legacy.labels.is_empty());
    }


    #[test]
    fn auth_store_path_is_rooted_in_the_selected_phoenix_home() {
        let selected = std::path::Path::new("/isolated/phoenix-home");
        assert_eq!(
            auth_profiles_path_in(selected),
            selected.join("auth-profiles.json")
        );
    }

    #[test]
    fn declared_profiles_accept_the_xai_grok_cli_shared_login_both_ways() {
        let grok_cli = crate::providers::providers_data::get_provider("grok-cli").unwrap();
        let xai = crate::providers::providers_data::get_provider("xai").unwrap();
        let xai_login = AuthProfileCredential::Token {
            provider: "xai".to_string(),
            token: "test-token".to_string(),
            expires: None,
        };
        let grok_login = AuthProfileCredential::Token {
            provider: "grok-cli".to_string(),
            token: "test-token".to_string(),
            expires: None,
        };
        validate_profile_provider(&grok_cli, "xai:default", &xai_login).unwrap();
        validate_profile_provider(&xai, "grok-cli:default", &grok_login).unwrap();
        assert!(validate_profile_provider(
            &xai,
            "nvidia:default",
            &AuthProfileCredential::ApiKey {
                provider: "nvidia".to_string(),
                key: "test-key".to_string(),
                display_name: None,
            }
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_auth_store_write_creates_private_temp_and_final_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-profiles.json");
        let temp = path.with_extension("json.tmp");

        crate::config::private_io::write_private_file(&temp, b"stale credential").unwrap();
        assert_eq!(
            std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&path, "old credential").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut store = AuthProfileStore::default();
        store.profiles.insert(
            "nvidia:default".to_string(),
            AuthProfileCredential::ApiKey {
                provider: "nvidia".to_string(),
                key: "test-only-key".to_string(),
                display_name: None,
            },
        );
        save_auth_profile_store_to(&path, &store).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!temp.exists());
        let saved: AuthProfileStore =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.profiles.contains_key("nvidia:default"));
    }

    #[test]
    fn two_lock_scoped_writers_preserve_both_profile_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-profiles.json");
        update_auth_profile_store_at(&path, |store| {
            store
                .state
                .cooldown_until
                .insert("existing:profile".to_string(), 123_456);
            Ok(())
        })
        .unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));

        std::thread::scope(|scope| {
            for (profile_id, key) in [
                ("nvidia:default", "test-key-one"),
                ("nvidia:2", "test-key-two"),
            ] {
                let path = path.clone();
                let start = start.clone();
                scope.spawn(move || {
                    start.wait();
                    update_auth_profile_store_at(&path, |store| {
                        store.profiles.insert(
                            profile_id.to_string(),
                            AuthProfileCredential::ApiKey {
                                provider: "nvidia".to_string(),
                                key: key.to_string(),
                                display_name: None,
                            },
                        );
                        Ok(())
                    })
                    .unwrap();
                });
            }
            start.wait();
        });

        let store = load_auth_profile_store_from_path(&path).unwrap();
        assert!(store.profiles.contains_key("nvidia:default"));
        assert!(store.profiles.contains_key("nvidia:2"));
        assert_eq!(
            store.state.cooldown_until.get("existing:profile"),
            Some(&123_456)
        );
    }

    fn seed_expiring_oauth(path: &std::path::Path) {
        update_auth_profile_store_at(path, |store| {
            store.profiles.insert(
                "xai:default".to_string(),
                AuthProfileCredential::OAuth {
                    provider: "xai".to_string(),
                    access: "access-old".to_string(),
                    refresh: Some("refresh-old".to_string()),
                    expires: 10,
                    email: None,
                },
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn oauth_refresh_failure_preserves_reason_without_response_secrets() {
        let body = serde_json::json!({"error":{"code":"refresh_token_reused","message":"private token abc123"}});
        let message = oauth_refresh_failure(400, Some(&body));
        assert!(message.contains("already used"));
        assert!(!message.contains("abc123"));
        let unknown = serde_json::json!({"error":"secret-value", "access_token":"private"});
        assert_eq!(oauth_refresh_failure(503, Some(&unknown)), "OAuth token refresh returned HTTP 503: automatic sign-in renewal failed");
    }

    #[test]
    fn terminal_oauth_refresh_reasons_activate_account_cooldown_without_leaking_secrets() {
        for code in ["refresh_token_reused", "refresh_token_expired", "refresh_token_invalidated", "invalid_grant"] {
            for body in [serde_json::json!({"error":code}), serde_json::json!({"error":{"code":code,"message":"private-secret"}})] {
                let message = oauth_refresh_failure(400, Some(&body));
                assert!(is_oauth_login_rejected(&message), "{code}");
                assert!(crate::providers::fallback::is_account_exhausted_error(&message), "{code}");
                assert!(!message.contains("private-secret"));
            }
        }
        for (status, code) in [(400,"invalid_request"), (400,"invalid_client"), (503,"temporarily_unavailable")] {
            let message = oauth_refresh_failure(status, Some(&serde_json::json!({"error":code})));
            assert!(!is_oauth_login_rejected(&message), "{message}");
        }
    }

    #[test]
    fn openai_codex_oauth_refresh_uses_the_login_client_and_exact_auth_host() {
        let endpoints = oauth_refresh_endpoints("openai-codex").unwrap();
        assert_eq!(endpoints.client_id, OPENAI_CODEX_OAUTH_CLIENT_ID);
        assert_eq!(endpoints.token_host, "auth.openai.com");
        assert_eq!(
            endpoints.fallback_token_endpoint,
            "https://auth.openai.com/oauth/token"
        );
    }

    #[test]
    fn openai_codex_expired_access_token_rotates_and_persists_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store_path = auth_profiles_path_in(home);
        update_auth_profile_store_at(&store_path, |store| {
            store.profiles.insert(
                "openai-codex:3".to_string(),
                AuthProfileCredential::OAuth {
                    provider: "openai-codex".to_string(),
                    access: "codex-access-old".to_string(),
                    refresh: Some("codex-refresh-old".to_string()),
                    expires: 10,
                    email: None,
                },
            );
            Ok(())
        })
        .unwrap();

        let refreshed = refresh_oauth_access_at(
            home,
            "openai-codex",
            "codex-access-old",
            "codex-refresh-old",
            1_000,
            |endpoints, refresh| {
                assert_eq!(endpoints.token_host, "auth.openai.com");
                assert_eq!(refresh, "codex-refresh-old");
                Ok(RotatedOAuthToken {
                    access: "codex-access-new".to_string(),
                    refresh: Some("codex-refresh-new".to_string()),
                    expires_in_seconds: 3_600,
                })
            },
        )
        .unwrap();

        assert_eq!(refreshed.as_deref(), Some("codex-access-new"));
        match &load_auth_profile_store_from_path(&store_path)
            .unwrap()
            .profiles["openai-codex:3"]
        {
            AuthProfileCredential::OAuth {
                access,
                refresh,
                expires,
                ..
            } => {
                assert_eq!(access, "codex-access-new");
                assert_eq!(refresh.as_deref(), Some("codex-refresh-new"));
                assert_eq!(*expires, 3_601_000);
            }
            other => panic!("unexpected credential: {other:?}"),
        }
    }

    #[test]
    fn concurrent_oauth_refreshes_rotate_once_and_share_persisted_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let store_path = auth_profiles_path_in(&home);
        seed_expiring_oauth(&store_path);

        let rotations = std::sync::Arc::new(AtomicUsize::new(0));
        let entered_network = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release_network = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let first_home = home.clone();
            let first_rotations = rotations.clone();
            let first_entered = entered_network.clone();
            let first_release = release_network.clone();
            let first = scope.spawn(move || {
                refresh_oauth_access_at(
                    &first_home,
                    "xai",
                    "access-old",
                    "refresh-old",
                    1_000,
                    move |_, _| {
                        first_rotations.fetch_add(1, Ordering::SeqCst);
                        first_entered.wait();
                        first_release.wait();
                        Ok(RotatedOAuthToken {
                            access: "access-new".to_string(),
                            refresh: Some("refresh-new".to_string()),
                            expires_in_seconds: 3_600,
                        })
                    },
                )
            });

            entered_network.wait();
            let second_home = home.clone();
            let second_rotations = rotations.clone();
            let second = scope.spawn(move || {
                refresh_oauth_access_at(
                    &second_home,
                    "xai",
                    "access-old",
                    "refresh-old",
                    1_000,
                    move |_, _| {
                        second_rotations.fetch_add(1, Ordering::SeqCst);
                        Ok(RotatedOAuthToken {
                            access: "wrong-second-access".to_string(),
                            refresh: None,
                            expires_in_seconds: 3_600,
                        })
                    },
                )
            });
            release_network.wait();

            assert_eq!(
                first.join().unwrap().unwrap(),
                Some("access-new".to_string())
            );
            assert_eq!(
                second.join().unwrap().unwrap(),
                Some("access-new".to_string())
            );
        });

        assert_eq!(rotations.load(Ordering::SeqCst), 1);
        let store = load_auth_profile_store_from_path(&store_path).unwrap();
        match &store.profiles["xai:default"] {
            AuthProfileCredential::OAuth {
                access, refresh, ..
            } => {
                assert_eq!(access, "access-new");
                assert_eq!(refresh.as_deref(), Some("refresh-new"));
            }
            other => panic!("unexpected credential: {other:?}"),
        }
    }

    #[test]
    fn oauth_refresh_never_reports_success_when_persistence_matches_zero_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let store_path = auth_profiles_path_in(&home);
        seed_expiring_oauth(&store_path);
        let path_for_rotation = store_path.clone();

        let result = refresh_oauth_access_at(
            &home,
            "xai",
            "access-old",
            "refresh-old",
            1_000,
            move |_, _| {
                update_auth_profile_store_at(&path_for_rotation, |store| {
                    store.profiles.remove("xai:default");
                    Ok(())
                })?;
                Ok(RotatedOAuthToken {
                    access: "must-not-escape".to_string(),
                    refresh: Some("must-not-persist".to_string()),
                    expires_in_seconds: 3_600,
                })
            },
        );

        assert!(result.is_err());
        assert!(load_auth_profile_store_from_path(&store_path)
            .unwrap()
            .profiles
            .is_empty());
        let identity = oauth_refresh_identity("xai", "access-old", "refresh-old");
        assert!(!home
            .join("oauth-refresh")
            .join(format!("{identity}.json"))
            .exists());
    }

    fn nim_key(key: &str) -> AuthProfileCredential {
        AuthProfileCredential::ApiKey {
            provider: "nvidia".to_string(),
            key: key.to_string(),
            display_name: Some("NVIDIA NIM API key".to_string()),
        }
    }

    /// The store is keyed by PROFILE id, so N accounts on one provider is the
    /// normal case, not a special one. Adding a second NVIDIA NIM key must not
    /// touch the first — that is the whole premise of pooling free accounts.
    #[test]
    fn a_provider_holds_many_independent_accounts() {
        let mut store = AuthProfileStore::default();
        for (id, key) in [
            ("nvidia:default", "nvapi-account-one"),
            ("nvidia:2", "nvapi-account-two"),
            ("nvidia:work", "nvapi-account-three"),
        ] {
            let slot = next_free_profile_id_in(&store, "nvidia");
            assert!(
                !store.profiles.contains_key(&slot),
                "the suggested slot `{slot}` is already taken"
            );
            store.profiles.insert(id.to_string(), nim_key(key));
        }
        assert_eq!(
            store.profiles.len(),
            3,
            "a later add overwrote an earlier one"
        );
        for (id, want) in [
            ("nvidia:default", "nvapi-account-one"),
            ("nvidia:2", "nvapi-account-two"),
            ("nvidia:work", "nvapi-account-three"),
        ] {
            let secret = extract_profile_secret(&store.profiles[id]).expect("api key");
            assert_eq!(secret, want, "profile `{id}` lost its own credential");
        }
        assert_eq!(
            store.profiles_for_provider("nvidia"),
            vec!["nvidia:2", "nvidia:default", "nvidia:work"],
        );
    }

    #[test]
    fn free_slots_never_collide_with_a_stored_account() {
        let mut store = AuthProfileStore::default();
        assert_eq!(next_free_profile_id_in(&store, "nvidia"), "nvidia:default");
        store
            .profiles
            .insert("nvidia:default".to_string(), nim_key("k1"));
        assert_eq!(next_free_profile_id_in(&store, "nvidia"), "nvidia:2");
        store.profiles.insert("nvidia:2".to_string(), nim_key("k2"));
        store.profiles.insert("nvidia:3".to_string(), nim_key("k3"));
        assert_eq!(next_free_profile_id_in(&store, "nvidia"), "nvidia:4");
        // Another provider's accounts never shift NIM's numbering.
        store
            .profiles
            .insert("openai:default".to_string(), nim_key("unrelated"));
        assert_eq!(next_free_profile_id_in(&store, "nvidia"), "nvidia:4");
    }

    /// Profile ids are stable and human-pickable, and every field survives a
    /// disk round trip — a GUI writing this JSON gets the same store back.
    #[test]
    fn the_store_round_trips_a_multi_account_nim_pool() {
        let mut store = AuthProfileStore::default();
        store
            .profiles
            .insert("nvidia:default".to_string(), nim_key("nvapi-one"));
        store
            .profiles
            .insert("nvidia:2".to_string(), nim_key("nvapi-two"));
        store.assignments.insert(
            "nvidia:2".to_string(),
            HashMap::from([(
                "orchestrator".to_string(),
                RoleAssignment {
                    model: "nvidia/nemotron-3-super-120b-a12b".to_string(),
                    effort: Some("high".to_string()),
                },
            )]),
        );
        store
            .state
            .cooldown_until
            .insert("nvidia:default".to_string(), 1_800_000_000_000);

        let raw = serde_json::to_string_pretty(&store).expect("serialize");
        let back: AuthProfileStore = serde_json::from_str(&raw).expect("deserialize");

        assert_eq!(back.profiles.len(), 2);
        assert_eq!(
            extract_profile_secret(&back.profiles["nvidia:2"]).unwrap(),
            "nvapi-two"
        );
        let assignment = back
            .assignment_for("nvidia:2", "orchestrator")
            .expect("lane assignment survived");
        assert_eq!(assignment.model, "nvidia/nemotron-3-super-120b-a12b");
        assert_eq!(assignment.effort.as_deref(), Some("high"));
        // A persisted bench survives a restart, so a drained account is not
        // re-probed on every gateway start.
        assert!(profile_cooled_down(
            &back.state,
            "nvidia:default",
            1_700_000_000_000
        ));
        assert!(!profile_cooled_down(
            &back.state,
            "nvidia:default",
            1_900_000_000_000
        ));
        assert!(!profile_cooled_down(
            &back.state,
            "nvidia:2",
            1_700_000_000_000
        ));
    }

    /// An agent lane with no exact pick inherits the profile's shared
    /// "specialist" model — so one NIM profile covers every specialist agent.
    #[test]
    fn agent_lanes_inherit_the_profiles_shared_specialist_pick() {
        let mut store = AuthProfileStore::default();
        store
            .profiles
            .insert("nvidia:2".to_string(), nim_key("nvapi-two"));
        store.assignments.insert(
            "nvidia:2".to_string(),
            HashMap::from([(
                "specialist".to_string(),
                RoleAssignment {
                    model: "z-ai/glm-5.2".to_string(),
                    effort: None,
                },
            )]),
        );
        assert_eq!(
            store.assignment_for("nvidia:2", "coder").map(|a| a.model),
            Some("z-ai/glm-5.2".to_string())
        );
        assert_eq!(store.assignment_for("nvidia:default", "coder"), None);
    }
}
