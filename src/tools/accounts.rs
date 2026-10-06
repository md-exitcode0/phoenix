//! Durable, secret-free account lifecycle records.
//!
//! Passwords and recovery material stay in the encrypted vault. This registry
//! records only enough operational state for coworkers to coordinate account
//! creation and verification without repeating work or losing ownership.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::security::vault::CredentialScope;

use super::ToolOutput;

const STORE_VERSION: u32 = 1;
const MAX_ACCOUNTS: usize = 8_192;
const MAX_TEXT_BYTES: usize = 1_000;
const APPROVAL_TTL_SECONDS: i64 = 15 * 60;
const MAX_APPROVALS: usize = 4_096;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Creating,
    AwaitingVerification,
    Ready,
    Blocked,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountRecord {
    pub account_id: String,
    pub scope: CredentialScope,
    pub site: String,
    pub username: Option<String>,
    pub purpose: String,
    pub credential_id: Option<String>,
    pub status: AccountStatus,
    pub verification_kind: Option<String>,
    pub status_note: Option<String>,
    pub created_by_agent_id: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AccountStore {
    version: u32,
    records: Vec<AccountRecord>,
}

impl Default for AccountStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AccountManageInput {
    List {
        #[serde(default)]
        site: Option<String>,
        #[serde(default)]
        include_archived: bool,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    Begin {
        site: String,
        purpose: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        credential_id: Option<String>,
        #[serde(default = "default_scope")]
        scope: String,
        /// One-use receipt returned by ask_for_login when account creation is
        /// configured to ask. Omitted only when Settings allows free accounts.
        #[serde(default)]
        approval_id: Option<String>,
    },
    SetStatus {
        account_id: String,
        status: AccountStatus,
        #[serde(default)]
        verification_kind: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
}

fn default_scope() -> String {
    "agent".to_string()
}

fn default_limit() -> usize {
    50
}

pub fn execute(
    input: AccountManageInput,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    match input {
        AccountManageInput::List {
            site,
            include_archived,
            limit,
        } => list(site.as_deref(), include_archived, limit, agent_id, group_id),
        AccountManageInput::Begin {
            site,
            purpose,
            username,
            credential_id,
            scope,
            approval_id,
        } => begin(
            &site,
            &purpose,
            username,
            credential_id,
            &scope,
            approval_id.as_deref(),
            agent_id,
            group_id,
        ),
        AccountManageInput::SetStatus {
            account_id,
            status,
            verification_kind,
            note,
        } => set_status(
            &account_id,
            status,
            verification_kind,
            note,
            agent_id,
            group_id,
        ),
    }
}

fn list(
    site: Option<&str>,
    include_archived: bool,
    limit: usize,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    anyhow::ensure!((1..=200).contains(&limit), "limit must be 1..=200");
    let scopes =
        crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
    let site = site
        .map(crate::tools::browser_cookie_grants::normalize_site)
        .transpose()?;
    let mut records = read_store()?.records;
    records.retain(|record| {
        scopes.contains(&record.scope)
            && (include_archived || record.status != AccountStatus::Archived)
            && site.as_ref().is_none_or(|site| &record.site == site)
    });
    records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    let omitted = records.len().saturating_sub(limit);
    records.truncate(limit);
    Ok(ToolOutput {
        summary: format!(
            "{} visible account record{}{}",
            records.len(),
            if records.len() == 1 { "" } else { "s" },
            if omitted > 0 {
                format!(" ({omitted} more omitted)")
            } else {
                String::new()
            }
        ),
        content: serde_json::to_string_pretty(&records)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn begin(
    site: &str,
    purpose: &str,
    username: Option<String>,
    credential_id: Option<String>,
    requested_scope: &str,
    approval_id: Option<&str>,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    validate_id(agent_id, "agent id")?;
    let site = crate::tools::browser_cookie_grants::normalize_site(site)?;
    let purpose = validate_text(purpose, "account purpose")?;
    let username = username
        .map(|value| validate_text(&value, "account username"))
        .transpose()?;
    let scope =
        crate::tools::browser_cookie_grants::authorized_scope(requested_scope, agent_id, group_id)?;
    let settings_scope = group_id
        .map(|id| crate::settings::SettingsScope::Group { id: id.to_string() })
        .unwrap_or_else(|| crate::settings::SettingsScope::Agent {
            id: agent_id.to_string(),
        });
    anyhow::ensure!(
        crate::settings::effective_bool("browser.account_creation", &settings_scope)
            .unwrap_or(true),
        "agent-created accounts are disabled in Settings → Browser & Accounts"
    );
    match crate::settings::effective_string("permissions.account_creation", &settings_scope)
        .as_deref()
        .unwrap_or("ask")
    {
        "deny" => anyhow::bail!("account creation is denied in Settings → Permissions"),
        "allow_free" => {}
        _ => consume_creation_approval(
            approval_id
                .context("account creation needs the approval_id returned by ask_for_login")?,
            &site,
            &scope,
            agent_id,
        )?,
    }
    if let Some(credential_id) = credential_id.as_deref() {
        validate_id(credential_id, "credential id")?;
        let visible =
            crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
        let metadata = crate::security::vault::Vault::open_default()
            .list_for_agent(&visible)?
            .into_iter()
            .find(|credential| credential.credential_id == credential_id)
            .context("credential does not exist in the current account scope")?;
        anyhow::ensure!(
            crate::tools::browser_cookie_grants::domain_matches_site(&metadata.site, &site),
            "credential is bound to {} rather than {}",
            metadata.site,
            site
        );
        anyhow::ensure!(
            metadata.scope == scope,
            "credential and account must use the same scope"
        );
    }

    let path = store_path();
    let record = crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        anyhow::ensure!(
            store.records.len() < MAX_ACCOUNTS,
            "account registry is full"
        );
        anyhow::ensure!(
            !store.records.iter().any(|record| {
                record.site == site
                    && record.username == username
                    && record.scope == scope
                    && record.status != AccountStatus::Archived
            }),
            "an active account record already exists for this site, username, and scope"
        );
        let now = chrono::Utc::now().to_rfc3339();
        let record = AccountRecord {
            account_id: uuid::Uuid::new_v4().to_string(),
            scope: scope.clone(),
            site: site.clone(),
            username: username.clone(),
            purpose: purpose.clone(),
            credential_id: credential_id.clone(),
            status: AccountStatus::Creating,
            verification_kind: None,
            status_note: None,
            created_by_agent_id: agent_id.to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        store.records.push(record.clone());
        Ok((record, serde_json::to_vec_pretty(&store)?))
    })?;
    Ok(ToolOutput {
        summary: format!("account creation {} started", record.account_id),
        content: format!(
            "Started account record `{}` for {} with {:?} scope. Create the free account in this coworker's private browser. If email verification is needed, set awaiting_verification and ask an email-capable coworker for the code; never store the code in this record. Mark ready only after a signed-in page is verified.",
            record.account_id, record.site, record.scope
        ),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreationApproval {
    approval_id: String,
    site: String,
    scope: CredentialScope,
    agent_id: String,
    expires_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CreationApprovalStore {
    #[serde(default)]
    approvals: Vec<CreationApproval>,
}

fn approvals_path() -> std::path::PathBuf {
    crate::config::phoenix_home().join("account-creation-approvals.json")
}

/// Mint a short-lived, one-use account-creation receipt after the user chose
/// that exact action in the structured login block. No password, code, or
/// account secret is stored here.
pub fn grant_creation_approval(
    site: &str,
    scope: CredentialScope,
    agent_id: &str,
) -> Result<String> {
    validate_id(agent_id, "agent id")?;
    let site = crate::tools::browser_cookie_grants::normalize_site(site)?;
    let now = chrono::Utc::now().timestamp();
    let path = approvals_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store: CreationApprovalStore = current
            .map(serde_json::from_slice)
            .transpose()
            .context("account approval store is invalid")?
            .unwrap_or_default();
        store.approvals.retain(|approval| approval.expires_at > now);
        anyhow::ensure!(
            store.approvals.len() < MAX_APPROVALS,
            "too many pending account approvals"
        );
        let approval_id = uuid::Uuid::new_v4().to_string();
        store.approvals.push(CreationApproval {
            approval_id: approval_id.clone(),
            site,
            scope,
            agent_id: agent_id.to_string(),
            expires_at: now + APPROVAL_TTL_SECONDS,
        });
        Ok((approval_id, serde_json::to_vec_pretty(&store)?))
    })
}

fn consume_creation_approval(
    approval_id: &str,
    site: &str,
    scope: &CredentialScope,
    agent_id: &str,
) -> Result<()> {
    validate_id(approval_id, "approval id")?;
    let path = approvals_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store: CreationApprovalStore = current
            .map(serde_json::from_slice)
            .transpose()
            .context("account approval store is invalid")?
            .unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        let matched = store.approvals.iter().any(|approval| {
            approval.approval_id == approval_id
                && approval.site == site
                && &approval.scope == scope
                && approval.agent_id == agent_id
                && approval.expires_at > now
        });
        anyhow::ensure!(
            matched,
            "account creation approval is missing, expired, or for another site/scope"
        );
        store
            .approvals
            .retain(|approval| approval.expires_at > now && approval.approval_id != approval_id);
        Ok(((), serde_json::to_vec_pretty(&store)?))
    })
}

fn set_status(
    account_id: &str,
    status: AccountStatus,
    verification_kind: Option<String>,
    note: Option<String>,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    validate_id(account_id, "account id")?;
    let scopes =
        crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
    let verification_kind = verification_kind
        .map(|value| validate_text(&value, "verification kind"))
        .transpose()?;
    let note = note
        .map(|value| validate_text(&value, "account status note"))
        .transpose()?;
    anyhow::ensure!(
        status == AccountStatus::AwaitingVerification || verification_kind.is_none(),
        "verification_kind is valid only for awaiting_verification"
    );
    anyhow::ensure!(
        status != AccountStatus::AwaitingVerification || verification_kind.is_some(),
        "awaiting_verification needs verification_kind (email, sms, totp, or user_action)"
    );

    let path = store_path();
    let record = crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        let record = store
            .records
            .iter_mut()
            .find(|record| record.account_id == account_id && scopes.contains(&record.scope))
            .context("account does not exist in the current conversation scope")?;
        anyhow::ensure!(
            transition_allowed(record.status, status),
            "invalid account transition from {:?} to {:?}",
            record.status,
            status
        );
        record.status = status;
        record.verification_kind = verification_kind.clone();
        record.status_note = note.clone();
        record.updated_at = chrono::Utc::now().to_rfc3339();
        let updated = record.clone();
        Ok((updated, serde_json::to_vec_pretty(&store)?))
    })?;
    let mut content = serde_json::to_value(&record)?;
    if record.status == AccountStatus::AwaitingVerification {
        let settings_scope = group_id
            .map(|id| crate::settings::SettingsScope::Group { id: id.to_string() })
            .unwrap_or_else(|| crate::settings::SettingsScope::Agent {
                id: agent_id.to_string(),
            });
        let automatic_handoff =
            crate::settings::effective_bool("browser.automatic_2fa_handoff", &settings_scope)
                .unwrap_or(true);
        content["next_step"] = serde_json::json!(if automatic_handoff {
            "Request the one-time code from the relevant email-capable coworker with talk; keep the code out of this record and enter it only into the browser."
        } else {
            "Automatic coworker handoff is disabled. Ask the user to complete the verification step in the embedded login window."
        });
    }
    Ok(ToolOutput {
        summary: format!("account {} is now {:?}", record.account_id, record.status),
        content: serde_json::to_string_pretty(&content)?,
    })
}

fn transition_allowed(from: AccountStatus, to: AccountStatus) -> bool {
    from == to
        || matches!(
            (from, to),
            (AccountStatus::Creating, AccountStatus::AwaitingVerification)
                | (AccountStatus::Creating, AccountStatus::Ready)
                | (AccountStatus::Creating, AccountStatus::Blocked)
                | (AccountStatus::Creating, AccountStatus::Archived)
                | (AccountStatus::AwaitingVerification, AccountStatus::Ready)
                | (AccountStatus::AwaitingVerification, AccountStatus::Blocked)
                | (AccountStatus::AwaitingVerification, AccountStatus::Archived)
                | (AccountStatus::Blocked, AccountStatus::Creating)
                | (AccountStatus::Blocked, AccountStatus::AwaitingVerification)
                | (AccountStatus::Blocked, AccountStatus::Ready)
                | (AccountStatus::Blocked, AccountStatus::Archived)
                | (AccountStatus::Ready, AccountStatus::Blocked)
                | (AccountStatus::Ready, AccountStatus::Archived)
        )
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')),
        "invalid {label}"
    );
    Ok(())
}

fn validate_text(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    anyhow::ensure!(
        !value.is_empty() && value.len() <= MAX_TEXT_BYTES && !value.contains('\0'),
        "{label} must be 1..={MAX_TEXT_BYTES} bytes"
    );
    Ok(value.to_string())
}

fn store_path() -> std::path::PathBuf {
    store_path_at(&crate::config::phoenix_home())
}

fn store_path_at(home: &std::path::Path) -> std::path::PathBuf {
    home.join("accounts/registry.json")
}

fn read_store() -> Result<AccountStore> {
    let bytes = crate::config::private_io::read_private_file(&store_path())?;
    parse_store(bytes.as_deref())
}

fn parse_store(bytes: Option<&[u8]>) -> Result<AccountStore> {
    let store = match bytes {
        None | Some([]) => AccountStore::default(),
        Some(bytes) => serde_json::from_slice(bytes).context("invalid account registry")?,
    };
    anyhow::ensure!(
        store.version == STORE_VERSION,
        "unsupported account registry version"
    );
    anyhow::ensure!(
        store.records.len() <= MAX_ACCOUNTS,
        "account registry is oversized"
    );
    Ok(store)
}

pub fn remove_scope_at(home: &std::path::Path, scope: &CredentialScope) -> Result<usize> {
    let path = store_path_at(home);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        let before = store.records.len();
        store.records.retain(|record| &record.scope != scope);
        let removed = before - store.records.len();
        Ok((removed, serde_json::to_vec_pretty(&store)?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_lifecycle_is_scoped_durable_and_contains_no_verification_code() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let approval =
            grant_creation_approval("example.com", CredentialScope::agent("phoenix"), "phoenix")
                .unwrap();
        let started = begin(
            "https://example.com/signup",
            "manage the team calendar",
            Some("ops@example.com".into()),
            None,
            "agent",
            Some(&approval),
            "phoenix",
            None,
        )
        .unwrap();
        let account_id = read_store().unwrap().records[0].account_id.clone();
        assert!(started.content.contains(&account_id));
        set_status(
            &account_id,
            AccountStatus::AwaitingVerification,
            Some("email".into()),
            Some("asked the email coworker".into()),
            "phoenix",
            None,
        )
        .unwrap();
        set_status(
            &account_id,
            AccountStatus::Ready,
            None,
            Some("signed-in dashboard verified".into()),
            "phoenix",
            None,
        )
        .unwrap();
        let listed = list(Some("example.com"), false, 50, "phoenix", None).unwrap();
        assert!(listed.content.contains("ready"));
        assert!(!listed.content.contains("verification_code"));
        assert_eq!(
            remove_scope_at(root.path(), &CredentialScope::agent("phoenix")).unwrap(),
            1
        );
        assert!(read_store().unwrap().records.is_empty());
    }

    #[test]
    fn account_transitions_reject_reopening_archived_records() {
        assert!(!transition_allowed(
            AccountStatus::Archived,
            AccountStatus::Creating
        ));
        assert!(transition_allowed(
            AccountStatus::Ready,
            AccountStatus::Blocked
        ));
    }
}
