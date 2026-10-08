//! Durable, resumable first-run control plane for the desktop app.
//!
//! Onboarding never owns provider or vault secrets: it reports whether those
//! existing subsystems are ready, while their dedicated secure commands do the
//! actual setup. This file records only product choices and auditable cookie
//! sharing authority.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::runtime::company::CompanyStore;
use crate::runtime::company_directory::{AgentRecord, LifecycleState};

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_COOKIE_SITES_IN_REPLY: usize = 500;
const PROVIDER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub const SUPPORTED_COOKIE_SOURCES: [&str; 11] = [
    "zen",
    "firefox",
    "librewolf",
    "floorp",
    "waterfox",
    "chrome",
    "chromium",
    "brave",
    "edge",
    "vivaldi",
    "opera",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompanyChoice {
    PhoenixOnly,
    FoundingCompany,
    /// Existing Phoenix history was linked into the founding company without
    /// deleting or rewriting any transcript, memory, account, or preference.
    MigratedExisting,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "choice", rename_all = "snake_case")]
pub enum CookieImportChoice {
    Skipped {
        decided_at: String,
    },
    AllPortable {
        source: String,
        agent_ids: Vec<String>,
        /// Automatic one-click authority follows the company, so coworkers
        /// created after onboarding receive the same portable-session grant.
        #[serde(default)]
        company_wide: bool,
        portable_cookie_count: usize,
        device_bound_cookie_count: usize,
        granted_at: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingState {
    pub version: u32,
    pub company_choice: Option<CompanyChoice>,
    pub default_account_email: Option<String>,
    pub cookie_import: Option<CookieImportChoice>,
    #[serde(default)]
    pub provider_verification: Option<ProviderVerification>,
    #[serde(default)]
    pub account_email_skipped: bool,
    /// Receipt that the user saw and saved the practical company defaults.
    /// Existing completed onboarding remains complete when this older field is
    /// absent; incomplete/new onboarding must review it before finishing.
    #[serde(default)]
    pub company_defaults_reviewed_at: Option<String>,
    /// Receipt that the optional powers studio was shown. The user may connect
    /// Composio/MCP there or deliberately continue without either; onboarding
    /// records the review, never a fabricated connection state.
    #[serde(default)]
    pub powers_setup_reviewed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderVerification {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_epoch: Option<u64>,
    pub verified_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingAgent {
    pub agent_id: String,
    pub display_name: String,
    pub role_title: String,
    pub color: String,
    pub icon_seed: String,
    pub lifecycle: LifecycleState,
}

impl From<&AgentRecord> for OnboardingAgent {
    fn from(agent: &AgentRecord) -> Self {
        Self {
            agent_id: agent.profile.agent_id.clone(),
            display_name: agent.profile.display_name.clone(),
            role_title: agent.profile.role_title.clone(),
            color: agent.profile.color.clone(),
            icon_seed: agent.profile.icon_seed.clone(),
            lifecycle: agent.profile.lifecycle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieSourceInspection {
    pub source: String,
    pub portable_cookie_count: usize,
    pub device_bound_cookie_count: usize,
    pub portable_sites: Vec<String>,
    pub omitted_site_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingSnapshot {
    pub state: OnboardingState,
    pub complete: bool,
    pub provider_configured: bool,
    pub provider_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_model: Option<String>,
    pub vault_status: String,
    pub agents: Vec<OnboardingAgent>,
    pub supported_cookie_sources: Vec<String>,
    pub required_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OnboardingCommand {
    Status,
    ChooseCompany {
        choice: CompanyChoice,
    },
    SetDefaultAccountEmail {
        email: String,
    },
    InspectCookieSource {
        source: String,
    },
    VerifyProvider,
    SetCookieImport {
        source: String,
        agent_ids: Vec<String>,
    },
    SetCookieImportAutomatic,
    SkipCookieImport,
    AcceptDetectedProvider,
    SkipDefaultAccountEmail,
    ReviewCompanyDefaults,
    ReviewPowersSetup,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum OnboardingReply {
    Snapshot(OnboardingSnapshot),
    CookieSource(CookieSourceInspection),
}

pub fn execute(command: OnboardingCommand) -> Result<OnboardingReply> {
    let store = crate::runtime::company::global()?;
    execute_with(&store, &crate::config::phoenix_home(), command)
}

/// Async entry point for desktop/gateway callers. Only the live provider
/// probe uses the async runtime; disk-heavy commands retain the blocking
/// worker path so they cannot stall unrelated conversations.
pub async fn execute_async(command: OnboardingCommand) -> Result<OnboardingReply> {
    if matches!(command, OnboardingCommand::VerifyProvider) {
        return verify_provider().await;
    }
    tokio::task::spawn_blocking(move || execute(command))
        .await
        .context("onboarding worker stopped")?
}

fn execute_with(
    store: &CompanyStore,
    home: &std::path::Path,
    command: OnboardingCommand,
) -> Result<OnboardingReply> {
    match command {
        OnboardingCommand::Status => Ok(OnboardingReply::Snapshot(snapshot(store, home)?)),
        OnboardingCommand::ChooseCompany { choice } => {
            choose_company(store, home, choice)?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::SetDefaultAccountEmail { email } => {
            let email = validate_email(&email)?;
            update_state(store, home, |state| {
                state.default_account_email = Some(email);
                Ok(())
            })?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::InspectCookieSource { source } => Ok(OnboardingReply::CookieSource(
            inspect_cookie_source(&source)?,
        )),
        OnboardingCommand::VerifyProvider => {
            anyhow::bail!("provider verification must run through the async gateway")
        }
        OnboardingCommand::SetCookieImport { source, agent_ids } => {
            set_cookie_import(store, home, &source, agent_ids, false)?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::SetCookieImportAutomatic => {
            set_cookie_import_automatic(store, home)?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::SkipCookieImport => {
            update_state(store, home, |state| {
                state.cookie_import = Some(CookieImportChoice::Skipped {
                    decided_at: chrono::Utc::now().to_rfc3339(),
                });
                state.completed_at = None;
                Ok(())
            })?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::AcceptDetectedProvider => {
            accept_detected_provider(store, home)?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::SkipDefaultAccountEmail => {
            update_state(store, home, |state| {
                state.account_email_skipped = true;
                state.completed_at = None;
                Ok(())
            })?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::ReviewCompanyDefaults => {
            update_state(store, home, |state| {
                state.company_defaults_reviewed_at = Some(chrono::Utc::now().to_rfc3339());
                state.completed_at = None;
                Ok(())
            })?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::ReviewPowersSetup => {
            update_state(store, home, |state| {
                state.powers_setup_reviewed_at = Some(chrono::Utc::now().to_rfc3339());
                state.completed_at = None;
                Ok(())
            })?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
        OnboardingCommand::Complete => {
            complete(store, home)?;
            Ok(OnboardingReply::Snapshot(snapshot(store, home)?))
        }
    }
}

fn choose_company(
    store: &CompanyStore,
    home: &std::path::Path,
    choice: CompanyChoice,
) -> Result<()> {
    anyhow::ensure!(
        choice != CompanyChoice::MigratedExisting,
        "migrated_existing is detected by Phoenix and cannot be selected manually"
    );
    let directory = store.directory_snapshot()?;
    if choice == CompanyChoice::PhoenixOnly {
        anyhow::ensure!(
            directory.agents.iter().all(|agent| agent.profile.agent_id == "phoenix"),
            "this company already has coworkers; Phoenix will not archive or delete them through onboarding"
        );
        store.ensure_founding_team(false)?;
    } else {
        store.ensure_founding_team(true)?;
    }
    update_state(store, home, |state| {
        state.company_choice = Some(choice);
        state.completed_at = None;
        Ok(())
    })?;
    Ok(())
}

fn set_cookie_import(
    store: &CompanyStore,
    home: &std::path::Path,
    source: &str,
    mut agent_ids: Vec<String>,
    company_wide: bool,
) -> Result<()> {
    anyhow::ensure!(!agent_ids.is_empty(), "pick at least one cookie recipient");
    anyhow::ensure!(agent_ids.len() <= 64, "too many cookie recipients");
    agent_ids.sort();
    anyhow::ensure!(
        agent_ids.windows(2).all(|window| window[0] != window[1]),
        "duplicate cookie recipient"
    );
    let directory = store.directory_snapshot()?;
    for agent_id in &agent_ids {
        let agent = directory
            .agents
            .iter()
            .find(|agent| &agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown coworker `{agent_id}`"))?;
        anyhow::ensure!(
            matches!(
                agent.profile.lifecycle,
                LifecycleState::Active | LifecycleState::Dormant
            ),
            "cannot give browser cookies to archived coworker `{agent_id}`"
        );
    }
    let inspection = inspect_cookie_source(source)?;
    anyhow::ensure!(
        inspection.portable_cookie_count > 0,
        "{} has no portable cookies to import",
        inspection.source
    );
    update_state(store, home, |state| {
        state.cookie_import = Some(CookieImportChoice::AllPortable {
            source: inspection.source,
            agent_ids,
            company_wide,
            portable_cookie_count: inspection.portable_cookie_count,
            device_bound_cookie_count: inspection.device_bound_cookie_count,
            granted_at: chrono::Utc::now().to_rfc3339(),
        });
        state.completed_at = None;
        Ok(())
    })?;
    Ok(())
}

/// One-click setup: select the browser with the freshest cookie store and give
/// every active/dormant coworker a renewable portable-cookie grant. The copy
/// still lands in separate private profiles and device-bound sessions remain
/// excluded.
fn set_cookie_import_automatic(store: &CompanyStore, home: &std::path::Path) -> Result<()> {
    let source = crate::tools::browser_cookies::detect_active_source()?;
    let directory = store.directory_snapshot()?;
    let agent_ids = directory
        .agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.profile.lifecycle,
                LifecycleState::Active | LifecycleState::Dormant
            )
        })
        .map(|agent| agent.profile.agent_id.clone())
        .collect::<Vec<_>>();
    set_cookie_import(store, home, &source, agent_ids, true)
}

pub fn inspect_cookie_source(source: &str) -> Result<CookieSourceInspection> {
    let source = source.trim().to_ascii_lowercase();
    anyhow::ensure!(
        SUPPORTED_COOKIE_SOURCES.contains(&source.as_str()),
        "unsupported cookie source `{source}`"
    );
    let cookies = crate::tools::browser_cookies::read_source_cookies(&source)?;
    let mut portable_cookie_count = 0usize;
    let mut device_bound_cookie_count = 0usize;
    let mut sites = std::collections::BTreeSet::new();
    for cookie in cookies {
        if crate::tools::browser_cookies::is_session_bound_domain(&cookie.domain) {
            device_bound_cookie_count = device_bound_cookie_count.saturating_add(1);
            continue;
        }
        portable_cookie_count = portable_cookie_count.saturating_add(1);
        let domain = cookie
            .domain
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !domain.is_empty() && domain.len() <= 253 {
            sites.insert(domain);
        }
    }
    let omitted_site_count = sites.len().saturating_sub(MAX_COOKIE_SITES_IN_REPLY);
    let portable_sites = sites.into_iter().take(MAX_COOKIE_SITES_IN_REPLY).collect();
    Ok(CookieSourceInspection {
        source,
        portable_cookie_count,
        device_bound_cookie_count,
        portable_sites,
        omitted_site_count,
    })
}

fn complete(store: &CompanyStore, home: &std::path::Path) -> Result<()> {
    let current = read_state(store, home)?;
    anyhow::ensure!(
        current.company_choice.is_some(),
        "choose a company setup first"
    );
    anyhow::ensure!(
        current.default_account_email.is_some() || current.account_email_skipped,
        "set the default account email first"
    );
    anyhow::ensure!(
        current.cookie_import.is_some(),
        "choose or skip browser-cookie import first"
    );
    anyhow::ensure!(
        current.company_defaults_reviewed_at.is_some(),
        "review and save the company defaults first"
    );
    anyhow::ensure!(
        current.powers_setup_reviewed_at.is_some(),
        "review or skip the optional powers setup first"
    );
    anyhow::ensure!(
        provider_verified(&current),
        "configure and verify an AI provider first"
    );
    anyhow::ensure!(
        crate::security::vault::Vault::at(home).has_master_password(),
        "create the Passes master password and save its recovery key first"
    );
    update_state(store, home, |state| {
        state.completed_at = Some(chrono::Utc::now().to_rfc3339());
        Ok(())
    })?;
    Ok(())
}

pub fn snapshot(store: &CompanyStore, home: &std::path::Path) -> Result<OnboardingSnapshot> {
    let state = read_state(store, home)?;
    let directory = store.directory_snapshot()?;
    let vault = crate::security::vault::Vault::at(home);
    let vault_status = vault.status().as_str().to_string();
    let vault_protected = vault.has_master_password();
    let provider_configured = provider_configured();
    let provider_ready = provider_verified(&state);
    let configured_identity = current_provider_identity()
        .ok()
        .map(|(identity, _)| identity);
    let mut required_actions = Vec::new();
    if state.company_choice.is_none() {
        required_actions.push("choose_company".to_string());
    }
    if !provider_configured {
        required_actions.push("configure_provider".to_string());
    } else if !provider_ready {
        required_actions.push("verify_provider".to_string());
    }
    if !vault_protected {
        required_actions.push("initialize_vault".to_string());
    }
    if state.default_account_email.is_none() && !state.account_email_skipped {
        required_actions.push("set_default_account_email".to_string());
    }
    if state.company_defaults_reviewed_at.is_none() && state.completed_at.is_none() {
        required_actions.push("review_company_defaults".to_string());
    }
    if state.cookie_import.is_none() {
        required_actions.push("choose_cookie_import".to_string());
    }
    if state.powers_setup_reviewed_at.is_none() && state.completed_at.is_none() {
        required_actions.push("review_powers_setup".to_string());
    }
    // `completed_at` is the durable first-run boundary. Provider/model,
    // browser, vault, and company settings remain independently configurable
    // after launch; changing one of them must not throw an established
    // company back into a full-page onboarding flow. We still expose any
    // newly-unhealthy dependency through `required_actions` so Settings and
    // an explicit onboarding replay can explain it without hijacking boot.
    let complete = state.completed_at.is_some();
    Ok(OnboardingSnapshot {
        state,
        complete,
        provider_configured,
        provider_ready,
        configured_provider: configured_identity
            .as_ref()
            .map(|identity| identity.provider.clone()),
        configured_model: configured_identity.map(|identity| identity.model),
        vault_status,
        agents: directory.agents.iter().map(OnboardingAgent::from).collect(),
        supported_cookie_sources: SUPPORTED_COOKIE_SOURCES
            .iter()
            .map(|source| (*source).to_string())
            .collect(),
        required_actions,
    })
}

fn provider_configured() -> bool {
    crate::config::PhoenixConfig::load().is_ok_and(|config| {
        !config.profile.llm.provider.trim().is_empty()
            && !config.profile.llm.model.trim().is_empty()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderIdentity {
    provider: String,
    model: String,
    auth_profile_id: Option<String>,
    auth_epoch: Option<u64>,
}

fn current_provider_identity() -> Result<(ProviderIdentity, crate::config::types::LLMProfile)> {
    let config = crate::config::PhoenixConfig::load().context("AI provider is not configured")?;
    let provider = config.profile.llm.provider.trim().to_string();
    let model = config.profile.llm.orchestrator().trim().to_string();
    anyhow::ensure!(!provider.is_empty(), "AI provider is empty");
    anyhow::ensure!(!model.is_empty(), "AI model is empty");
    let probe_profile = config.profile.llm.with_lane_pin("orchestrator", &provider);
    let auth_profile_id = probe_profile
        .auth
        .as_ref()
        .and_then(|auth| auth.profile.as_deref())
        .map(str::to_string);
    let auth_epoch = match auth_profile_id.as_deref() {
        Some(profile_id) => {
            let store = crate::config::auth_profile::load_auth_profile_store()
                .context("stored provider accounts are unavailable")?;
            anyhow::ensure!(
                store.profiles.contains_key(profile_id),
                "configured provider account `{profile_id}` does not exist"
            );
            Some(store.auth_epoch_for_profile(profile_id))
        }
        None => None,
    };
    Ok((
        ProviderIdentity {
            provider,
            model,
            auth_profile_id,
            auth_epoch,
        },
        probe_profile,
    ))
}

fn provider_verified(state: &OnboardingState) -> bool {
    let Some(receipt) = state.provider_verification.as_ref() else {
        return false;
    };
    current_provider_identity().is_ok_and(|(identity, _)| {
        receipt.provider == identity.provider
            && receipt.model == identity.model
            && receipt.auth_profile_id == identity.auth_profile_id
            && receipt.auth_epoch == identity.auth_epoch
    })
}

async fn verify_provider() -> Result<OnboardingReply> {
    use crate::providers::{ChatMessage, CompletionRequest, MessageRole, ProviderFactory};

    let store = crate::runtime::company::global()?;
    let home = crate::config::phoenix_home();
    let (identity, profile) = current_provider_identity()?;
    let provider = ProviderFactory::new()
        .build_llm_provider(&profile)
        .context("configured provider could not be opened")?;
    let request = CompletionRequest {
        max_tokens: Some(8),
        ..CompletionRequest::new(
            identity.model.clone(),
            vec![ChatMessage {
                role: MessageRole::User,
                content: "Reply with exactly: OK".to_string(),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
                images: Vec::new(),
                provider_replay: None,
            }],
        )
    };
    tokio::time::timeout(PROVIDER_PROBE_TIMEOUT, provider.complete(request))
        .await
        .context("provider verification timed out after 25 seconds")?
        .context("provider verification request failed")?;

    // Configuration may have changed while the network request was in flight.
    // Never bless a different provider/account with the old probe's success.
    let (current, _) = current_provider_identity()?;
    anyhow::ensure!(
        current == identity,
        "provider configuration changed during verification; verify it again"
    );
    update_state(&store, &home, |state| {
        state.provider_verification = Some(ProviderVerification {
            provider: identity.provider,
            model: identity.model,
            auth_profile_id: identity.auth_profile_id,
            auth_epoch: identity.auth_epoch,
            verified_at: chrono::Utc::now().to_rfc3339(),
        });
        state.completed_at = None;
        Ok(())
    })?;
    Ok(OnboardingReply::Snapshot(snapshot(&store, &home)?))
}

fn read_state(store: &CompanyStore, home: &std::path::Path) -> Result<OnboardingState> {
    let path = state_path(home);
    let bytes = crate::config::private_io::read_private_file_limited(&path, MAX_STATE_BYTES)?;
    match bytes {
        Some(bytes) => parse_state(&bytes),
        None => initial_state(store),
    }
}

fn initial_state(store: &CompanyStore) -> Result<OnboardingState> {
    let now = chrono::Utc::now().to_rfc3339();
    let directory = store.directory_snapshot()?;
    let company_choice = (directory.agents.len() > 1).then_some(CompanyChoice::MigratedExisting);
    Ok(OnboardingState {
        version: STATE_VERSION,
        company_choice,
        default_account_email: None,
        cookie_import: None,
        provider_verification: None,
        account_email_skipped: false,
        company_defaults_reviewed_at: None,
        powers_setup_reviewed_at: None,
        created_at: now.clone(),
        updated_at: now,
        completed_at: None,
    })
}

fn accept_detected_provider(store: &CompanyStore, home: &std::path::Path) -> Result<()> {
    let (identity, _) = current_provider_identity()
        .context("no configured provider to accept — set one up first")?;
    update_state(store, home, |state| {
        state.provider_verification = Some(ProviderVerification {
            provider: identity.provider,
            model: identity.model,
            auth_profile_id: identity.auth_profile_id,
            auth_epoch: identity.auth_epoch,
            verified_at: chrono::Utc::now().to_rfc3339(),
        });
        state.completed_at = None;
        Ok(())
    })?;
    Ok(())
}

fn update_state(
    store: &CompanyStore,
    home: &std::path::Path,
    update: impl FnOnce(&mut OnboardingState) -> Result<()>,
) -> Result<OnboardingState> {
    let path = state_path(home);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut state = match current {
            Some(bytes) => parse_state(bytes)?,
            None => initial_state(store)?,
        };
        update(&mut state)?;
        state.updated_at = chrono::Utc::now().to_rfc3339();
        validate_state(&state)?;
        Ok((state.clone(), serde_json::to_vec_pretty(&state)?))
    })
}

fn parse_state(bytes: &[u8]) -> Result<OnboardingState> {
    anyhow::ensure!(
        bytes.len() <= MAX_STATE_BYTES,
        "onboarding state is oversized"
    );
    let state: OnboardingState =
        serde_json::from_slice(bytes).context("invalid onboarding state")?;
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &OnboardingState) -> Result<()> {
    anyhow::ensure!(
        state.version == STATE_VERSION,
        "unsupported onboarding state version"
    );
    if let Some(email) = &state.default_account_email {
        anyhow::ensure!(
            validate_email(email)? == *email,
            "non-canonical account email"
        );
    }
    if let Some(CookieImportChoice::AllPortable {
        source, agent_ids, ..
    }) = &state.cookie_import
    {
        anyhow::ensure!(
            SUPPORTED_COOKIE_SOURCES.contains(&source.as_str()),
            "invalid onboarding cookie source"
        );
        anyhow::ensure!(
            !agent_ids.is_empty() && agent_ids.len() <= 64,
            "invalid cookie recipients"
        );
        for agent_id in agent_ids {
            validate_id(agent_id, "cookie recipient")?;
        }
        anyhow::ensure!(
            agent_ids.windows(2).all(|window| window[0] < window[1]),
            "cookie recipients must be sorted and unique"
        );
    }
    if let Some(receipt) = &state.provider_verification {
        validate_bounded_text(&receipt.provider, "verified provider", 128)?;
        validate_bounded_text(&receipt.model, "verified model", 512)?;
        if let Some(profile_id) = receipt.auth_profile_id.as_deref() {
            validate_auth_profile_id(profile_id)?;
            anyhow::ensure!(
                receipt.auth_epoch.is_some(),
                "verified auth epoch is missing"
            );
        } else {
            anyhow::ensure!(
                receipt.auth_epoch.is_none(),
                "verified auth epoch has no profile"
            );
        }
        anyhow::ensure!(
            receipt.auth_epoch.is_none_or(|epoch| epoch > 0),
            "verified auth epoch is invalid"
        );
        chrono::DateTime::parse_from_rfc3339(&receipt.verified_at)
            .context("verified provider timestamp is invalid")?;
    }
    if let Some(reviewed_at) = &state.company_defaults_reviewed_at {
        chrono::DateTime::parse_from_rfc3339(reviewed_at)
            .context("company defaults timestamp is invalid")?;
    }
    if let Some(reviewed_at) = &state.powers_setup_reviewed_at {
        chrono::DateTime::parse_from_rfc3339(reviewed_at)
            .context("powers setup timestamp is invalid")?;
    }
    Ok(())
}

fn validate_email(email: &str) -> Result<String> {
    let email = email.trim();
    anyhow::ensure!(
        !email.is_empty()
            && email.len() <= 320
            && !email.chars().any(char::is_whitespace)
            && !email.contains('\0'),
        "default account email is invalid"
    );
    let (local, domain) = email
        .split_once('@')
        .context("default account email needs one @")?;
    anyhow::ensure!(
        !local.is_empty()
            && !domain.is_empty()
            && !domain.contains('@')
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.'),
        "default account email is invalid"
    );
    Ok(format!("{local}@{}", domain.to_ascii_lowercase()))
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty() && value.len() <= 128, "invalid {label}");
    anyhow::ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "invalid {label}"
    );
    Ok(())
}

fn validate_bounded_text(value: &str, label: &str, max_bytes: usize) -> Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.len() <= max_bytes
            && !value.chars().any(char::is_control),
        "invalid {label}"
    );
    Ok(())
}

fn validate_auth_profile_id(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.')
            }),
        "invalid verified auth profile"
    );
    Ok(())
}

fn state_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("onboarding/state.json")
}

/// Shared company account identity for runtime prompts and account tools.
/// Missing/incomplete onboarding simply yields no identity.
pub fn default_account_email() -> Option<String> {
    let home = crate::config::phoenix_home();
    let bytes =
        crate::config::private_io::read_private_file_limited(&state_path(&home), MAX_STATE_BYTES)
            .ok()??;
    parse_state(&bytes).ok()?.default_account_email
}

/// Derive the one blanket cookie grant selected during onboarding. The
/// onboarding state is the single authority, avoiding a crash-prone duplicate
/// record in the ordinary exact-site grant store.
pub fn cookie_grant_for_agent(
    agent_id: &str,
) -> Option<crate::tools::browser_cookie_grants::CookieGrant> {
    cookie_grant_for_agent_at(&crate::config::phoenix_home(), agent_id)
}

fn cookie_grant_for_agent_at(
    home: &std::path::Path,
    agent_id: &str,
) -> Option<crate::tools::browser_cookie_grants::CookieGrant> {
    validate_id(agent_id, "agent id").ok()?;
    let bytes =
        crate::config::private_io::read_private_file_limited(&state_path(home), MAX_STATE_BYTES)
            .ok()??;
    let state = parse_state(&bytes).ok()?;
    let CookieImportChoice::AllPortable {
        source,
        agent_ids,
        company_wide,
        granted_at,
        ..
    } = state.cookie_import?
    else {
        return None;
    };
    (company_wide || agent_ids.iter().any(|recipient| recipient == agent_id)).then(|| {
        crate::tools::browser_cookie_grants::CookieGrant {
            grant_id: format!("onboarding-all-{agent_id}"),
            source,
            site: "*".to_string(),
            all_portable: true,
            scope: crate::security::vault::CredentialScope::agent(agent_id),
            created_by_agent_id: "onboarding".to_string(),
            created_at: granted_at,
        }
    })
}

/// Remove a purged coworker from onboarding's browser authority. This is
/// root-explicit so lifecycle erasure never follows a process-global test or
/// recovery home by accident.
pub fn remove_agent_at(home: &std::path::Path, agent_id: &str) -> Result<bool> {
    let path = state_path(home);
    if !path.exists() {
        return Ok(false);
    }
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let bytes = current.context("onboarding state disappeared during coworker purge")?;
        let mut state = parse_state(bytes)?;
        let mut removed = false;
        if let Some(CookieImportChoice::AllPortable {
            agent_ids,
            company_wide,
            ..
        }) = state.cookie_import.as_mut()
        {
            let before = agent_ids.len();
            agent_ids.retain(|recipient| recipient != agent_id);
            removed = before != agent_ids.len();
            if removed && agent_ids.is_empty() && !*company_wide {
                state.cookie_import = Some(CookieImportChoice::Skipped {
                    decided_at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        if removed {
            state.updated_at = chrono::Utc::now().to_rfc3339();
        }
        Ok((removed, serde_json::to_vec_pretty(&state)?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &std::path::Path) -> CompanyStore {
        CompanyStore::open(root.join("company/company.sqlite")).unwrap()
    }

    #[test]
    fn automatic_cookie_import_command_is_a_stable_desktop_contract() {
        let command: OnboardingCommand = serde_json::from_value(serde_json::json!({
            "action": "set_cookie_import_automatic"
        }))
        .unwrap();
        assert!(matches!(
            command,
            OnboardingCommand::SetCookieImportAutomatic
        ));
        assert_eq!(
            SUPPORTED_COOKIE_SOURCES.as_slice(),
            crate::tools::browser_cookies::AUTO_COOKIE_SOURCES.as_slice()
        );
    }

    #[test]
    fn optional_powers_review_is_a_stable_desktop_contract() {
        let command: OnboardingCommand = serde_json::from_value(serde_json::json!({
            "action": "review_powers_setup"
        }))
        .unwrap();
        assert!(matches!(command, OnboardingCommand::ReviewPowersSetup));
    }

    #[test]
    fn fresh_onboarding_is_resumable_and_never_creates_hidden_coworkers() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        let initial = snapshot(&store, root.path()).unwrap();
        assert_eq!(initial.agents.len(), 1);
        assert!(initial
            .required_actions
            .contains(&"choose_company".to_string()));

        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::PhoenixOnly,
            },
        )
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::SetDefaultAccountEmail {
                email: " Owner@Example.com ".to_string(),
            },
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::SkipCookieImport).unwrap();
        let resumed = snapshot(&store, root.path()).unwrap();
        assert_eq!(
            resumed.state.company_choice,
            Some(CompanyChoice::PhoenixOnly)
        );
        assert_eq!(
            resumed.state.default_account_email.as_deref(),
            Some("Owner@example.com")
        );
        assert_eq!(resumed.agents.len(), 1);
        assert_eq!(
            default_account_email().as_deref(),
            Some("Owner@example.com")
        );
        let identity = crate::runtime::company_directory::runtime_identity_block(
            &store.directory_snapshot().unwrap(),
            "phoenix",
        )
        .unwrap();
        assert!(identity.contains("`Owner@example.com`"));
        assert!(identity.contains("one-time codes stay in the vault/runtime broker"));
    }

    fn write_blank_onboarding_state(home: &std::path::Path) {
        let now = chrono::Utc::now().to_rfc3339();
        let state = OnboardingState {
            version: STATE_VERSION,
            company_choice: None,
            default_account_email: None,
            cookie_import: None,
            provider_verification: None,
            account_email_skipped: false,
            company_defaults_reviewed_at: None,
            powers_setup_reviewed_at: None,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        };
        crate::config::private_io::atomic_write_private(
            &state_path(home),
            &serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
    }

    fn seed_verified_local_provider(home: &std::path::Path, store: &CompanyStore) {
        crate::config::private_io::atomic_write_private(
            &home.join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"local-test\"\n",
        )
        .unwrap();
        let _recovery = crate::security::vault::Vault::at(home)
            .initialize("a sufficiently long master password")
            .unwrap();
        update_state(store, home, |state| {
            state.provider_verification = Some(ProviderVerification {
                provider: "ollama".to_string(),
                model: "local-test".to_string(),
                auth_profile_id: None,
                auth_epoch: None,
                verified_at: chrono::Utc::now().to_rfc3339(),
            });
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn existing_company_is_detected_without_rewriting_or_allowing_destructive_scratch() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let initial = snapshot(&store, root.path()).unwrap();
        assert_eq!(
            initial.state.company_choice,
            Some(CompanyChoice::MigratedExisting)
        );
        assert_eq!(initial.agents.len(), 11);
        assert!(execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::PhoenixOnly,
            },
        )
        .is_err());
        assert_eq!(store.directory_snapshot().unwrap().agents.len(), 11);
        assert!(!initial
            .required_actions
            .contains(&"choose_company".to_string()));
    }

    #[test]
    fn written_null_company_choice_reopens_the_first_step_without_deleting_coworkers() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        write_blank_onboarding_state(root.path());

        let snap = snapshot(&store, root.path()).unwrap();
        assert_eq!(snap.state.company_choice, None);
        assert!(snap
            .required_actions
            .contains(&"choose_company".to_string()));
        assert_eq!(store.directory_snapshot().unwrap().agents.len(), 11);

        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::FoundingCompany,
            },
        )
        .unwrap();
        assert_eq!(store.directory_snapshot().unwrap().agents.len(), 11);
        assert_eq!(
            read_state(&store, root.path()).unwrap().company_choice,
            Some(CompanyChoice::FoundingCompany)
        );
    }

    #[test]
    fn existing_founding_company_can_finish_onboarding_without_deleting_home_data() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        write_blank_onboarding_state(root.path());

        let precious = root.path().join("auth-profiles.json");
        crate::config::private_io::atomic_write_private(&precious, br#"{"profiles":{}}"#).unwrap();

        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::FoundingCompany,
            },
        )
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::SetDefaultAccountEmail {
                email: "owner@example.com".to_string(),
            },
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::SkipCookieImport).unwrap();
        seed_verified_local_provider(root.path(), &store);
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ReviewCompanyDefaults,
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::ReviewPowersSetup).unwrap();
        execute_with(&store, root.path(), OnboardingCommand::Complete).unwrap();

        let complete = snapshot(&store, root.path()).unwrap();
        assert!(complete.complete);
        assert!(complete.required_actions.is_empty());
        assert_eq!(
            complete.state.company_choice,
            Some(CompanyChoice::FoundingCompany)
        );
        assert_eq!(store.directory_snapshot().unwrap().agents.len(), 11);
        assert_eq!(std::fs::read(&precious).unwrap(), br#"{"profiles":{}}"#);
        assert!(root.path().join("config.toml").is_file());
    }

    #[test]
    fn skip_email_and_accept_detected_provider_finish_existing_company() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        write_blank_onboarding_state(root.path());
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::FoundingCompany,
            },
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::SkipCookieImport).unwrap();
        crate::config::private_io::atomic_write_private(
            &root.path().join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"local-test\"\n",
        )
        .unwrap();
        let _recovery = crate::security::vault::Vault::at(root.path())
            .initialize("a sufficiently long master password")
            .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::AcceptDetectedProvider,
        )
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::SkipDefaultAccountEmail,
        )
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ReviewCompanyDefaults,
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::ReviewPowersSetup).unwrap();
        execute_with(&store, root.path(), OnboardingCommand::Complete).unwrap();
        let complete = snapshot(&store, root.path()).unwrap();
        assert!(complete.complete);
        assert!(complete.state.account_email_skipped);
        assert!(complete.provider_ready);
        assert_eq!(store.directory_snapshot().unwrap().agents.len(), 11);
    }

    #[test]
    fn account_email_rejects_ambiguous_or_multiline_values() {
        for bad in [
            "",
            "missing-at.example",
            "a@@example.com",
            "a@example",
            "a@example.com\nBcc:x",
        ] {
            assert!(validate_email(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn changing_account_email_after_onboarding_preserves_completion() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        update_state(&store, root.path(), |state| {
            state.company_choice = Some(CompanyChoice::PhoenixOnly);
            state.default_account_email = Some("old@example.com".to_string());
            state.cookie_import = Some(CookieImportChoice::Skipped {
                decided_at: chrono::Utc::now().to_rfc3339(),
            });
            state.completed_at = Some(chrono::Utc::now().to_rfc3339());
            Ok(())
        })
        .unwrap();

        execute_with(
            &store,
            root.path(),
            OnboardingCommand::SetDefaultAccountEmail {
                email: "new@example.com".to_string(),
            },
        )
        .unwrap();
        let updated = read_state(&store, root.path()).unwrap();
        assert_eq!(
            updated.default_account_email.as_deref(),
            Some("new@example.com")
        );
        assert!(updated.completed_at.is_some());
    }

    #[test]
    fn completion_requires_every_operational_dependency_and_persists() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::PhoenixOnly,
            },
        )
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::SetDefaultAccountEmail {
                email: "owner@example.com".to_string(),
            },
        )
        .unwrap();
        execute_with(&store, root.path(), OnboardingCommand::SkipCookieImport).unwrap();
        assert!(execute_with(&store, root.path(), OnboardingCommand::Complete).is_err());

        crate::config::private_io::atomic_write_private(
            &root.path().join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"local-test\"\n",
        )
        .unwrap();
        let _recovery = crate::security::vault::Vault::at(root.path())
            .initialize("a sufficiently long master password")
            .unwrap();
        update_state(&store, root.path(), |state| {
            state.provider_verification = Some(ProviderVerification {
                provider: "ollama".to_string(),
                model: "local-test".to_string(),
                auth_profile_id: None,
                auth_epoch: None,
                verified_at: chrono::Utc::now().to_rfc3339(),
            });
            Ok(())
        })
        .unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ReviewCompanyDefaults,
        )
        .unwrap();
        assert!(execute_with(&store, root.path(), OnboardingCommand::Complete).is_err());
        execute_with(&store, root.path(), OnboardingCommand::ReviewPowersSetup).unwrap();
        execute_with(&store, root.path(), OnboardingCommand::Complete).unwrap();
        let complete = snapshot(&store, root.path()).unwrap();
        assert!(complete.complete);
        assert!(complete.required_actions.is_empty());
        assert!(complete.state.completed_at.is_some());
        assert_eq!(complete.configured_provider.as_deref(), Some("ollama"));
        assert_eq!(complete.configured_model.as_deref(), Some("local-test"));

        crate::config::private_io::atomic_write_private(
            &root.path().join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"different-model\"\n",
        )
        .unwrap();
        let changed = snapshot(&store, root.path()).unwrap();
        assert!(changed.complete);
        assert!(!changed.provider_ready);
        assert_eq!(changed.configured_provider.as_deref(), Some("ollama"));
        assert_eq!(changed.configured_model.as_deref(), Some("different-model"));
        assert!(changed
            .required_actions
            .contains(&"verify_provider".to_string()));
    }

    #[test]
    fn blanket_cookie_authority_has_one_atomic_source_and_is_erased_with_coworker() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        update_state(&store, root.path(), |state| {
            state.cookie_import = Some(CookieImportChoice::AllPortable {
                source: "firefox".to_string(),
                agent_ids: vec!["coder".to_string(), "phoenix".to_string()],
                company_wide: false,
                portable_cookie_count: 42,
                device_bound_cookie_count: 3,
                granted_at: "2026-08-14T00:00:00Z".to_string(),
            });
            Ok(())
        })
        .unwrap();
        assert!(crate::tools::browser_cookie_grants::list()
            .unwrap()
            .is_empty());
        let grant = cookie_grant_for_agent_at(root.path(), "coder").unwrap();
        assert!(grant.all_portable);
        assert_eq!(grant.site, "*");
        assert_eq!(grant.source, "firefox");
        assert!(remove_agent_at(root.path(), "coder").unwrap());
        assert!(cookie_grant_for_agent_at(root.path(), "coder").is_none());
        assert!(cookie_grant_for_agent_at(root.path(), "phoenix").is_some());

        update_state(&store, root.path(), |state| {
            let Some(CookieImportChoice::AllPortable { company_wide, .. }) =
                state.cookie_import.as_mut()
            else {
                unreachable!()
            };
            *company_wide = true;
            Ok(())
        })
        .unwrap();
        assert!(
            cookie_grant_for_agent_at(root.path(), "future-coworker").is_some(),
            "one-click company authority must follow coworkers created later"
        );
    }

    #[test]
    fn corrupted_state_fails_closed_without_being_replaced() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        let path = state_path(root.path());
        let corrupted = br#"{"version":1,"cookie_import":{"choice":"all_portable","source":"firefox","agent_ids":["../escape"]}}"#;
        crate::config::private_io::atomic_write_private(&path, corrupted).unwrap();

        assert!(execute_with(
            &store,
            root.path(),
            OnboardingCommand::SetDefaultAccountEmail {
                email: "owner@example.com".to_string(),
            },
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupted);
        assert!(cookie_grant_for_agent_at(root.path(), "../escape").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn onboarding_state_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        execute_with(
            &store,
            root.path(),
            OnboardingCommand::ChooseCompany {
                choice: CompanyChoice::PhoenixOnly,
            },
        )
        .unwrap();

        assert_eq!(
            std::fs::metadata(state_path(root.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
