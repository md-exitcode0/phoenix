//! Durable authority records for importing browser cookies into coworker profiles.
//!
//! The grant contains no cookie values. It says which source browser may be
//! consulted for one site and which company scope may receive those cookies.
//! Cookie values remain in the source browser and each authorized Chrome
//! profile; device-bound domains are filtered by the existing import engine.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::security::vault::CredentialScope;

const STORE_VERSION: u32 = 1;
const MAX_GRANTS: usize = 8_192;
const MAX_STORE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieGrant {
    pub grant_id: String,
    pub source: String,
    pub site: String,
    /// Explicit onboarding authority to refresh every portable site from the
    /// chosen source. Ordinary in-task imports remain exact-site grants.
    #[serde(default)]
    pub all_portable: bool,
    pub scope: CredentialScope,
    pub created_by_agent_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantStore {
    version: u32,
    generation: u64,
    grants: Vec<CookieGrant>,
}

impl Default for GrantStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            generation: 0,
            grants: Vec::new(),
        }
    }
}

pub fn normalize_site(site: &str) -> Result<String> {
    let site = site.trim();
    anyhow::ensure!(
        !site.is_empty() && site.len() <= 512,
        "invalid cookie-import site"
    );
    let url = if site.contains("://") {
        url::Url::parse(site).context("cookie-import site is not a valid URL")?
    } else {
        url::Url::parse(&format!("https://{site}"))
            .context("cookie-import site is not a valid domain")?
    };
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "cookie-import URL must not contain credentials"
    );
    url.host_str()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .context("cookie-import site has no host")
}

pub fn domain_matches_site(domain: &str, site: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == site || domain.ends_with(&format!(".{site}")) || site.ends_with(&format!(".{domain}"))
}

pub fn add(
    source: &str,
    site: &str,
    scope: CredentialScope,
    created_by_agent_id: &str,
) -> Result<CookieGrant> {
    anyhow::ensure!(
        crate::tools::browser_cookies::is_supported_source(source),
        "unsupported cookie source `{source}`"
    );
    validate_id(created_by_agent_id, "agent id")?;
    validate_scope(&scope)?;
    let site = normalize_site(site)?;
    let path = store_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        if let Some(existing) = store.grants.iter().find(|grant| {
            grant.source.eq_ignore_ascii_case(source) && grant.site == site && grant.scope == scope
        }) {
            return Ok((existing.clone(), serde_json::to_vec_pretty(&store)?));
        }
        anyhow::ensure!(
            store.grants.len() < MAX_GRANTS,
            "cookie grant store is full"
        );
        let grant = CookieGrant {
            grant_id: uuid::Uuid::new_v4().to_string(),
            source: source.to_ascii_lowercase(),
            site,
            all_portable: false,
            scope,
            created_by_agent_id: created_by_agent_id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store.generation = store.generation.saturating_add(1);
        store.grants.push(grant.clone());
        Ok((grant, serde_json::to_vec_pretty(&store)?))
    })
}

pub fn remove(grant_id: &str) -> Result<bool> {
    validate_id(grant_id, "grant id")?;
    let path = store_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        let before = store.grants.len();
        store.grants.retain(|grant| grant.grant_id != grant_id);
        let removed = store.grants.len() != before;
        if removed {
            store.generation = store.generation.saturating_add(1);
        }
        Ok((removed, serde_json::to_vec_pretty(&store)?))
    })
}

/// Remove every grant owned by a private agent/group scope after its 30-day
/// lifecycle window. Company grants are never touched by private-owner purge.
pub fn remove_scope(scope: &CredentialScope) -> Result<usize> {
    remove_scope_at(&crate::config::phoenix_home(), scope)
}

/// Root-explicit lifecycle variant. Company stores and tests must purge the
/// state tree that owns their directory, never whatever PHOENIX_HOME happens
/// to be set for the current process.
pub fn remove_scope_at(phoenix_home: &std::path::Path, scope: &CredentialScope) -> Result<usize> {
    validate_scope(scope)?;
    let path = store_path_at(phoenix_home);
    if !path.exists() {
        return Ok(0);
    }
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = parse_store(current)?;
        let before = store.grants.len();
        store.grants.retain(|grant| &grant.scope != scope);
        let removed = before - store.grants.len();
        if removed > 0 {
            store.generation = store.generation.saturating_add(1);
        }
        Ok((removed, serde_json::to_vec_pretty(&store)?))
    })
}

pub fn list() -> Result<Vec<CookieGrant>> {
    let path = store_path();
    let bytes = crate::config::private_io::read_private_file_limited(&path, MAX_STORE_BYTES)?;
    Ok(parse_store(bytes.as_deref())?.grants)
}

pub fn applicable_to(agent_id: &str) -> Result<Vec<CookieGrant>> {
    validate_id(agent_id, "agent id")?;
    let snapshot = crate::runtime::company::global()
        .and_then(|company| company.directory_snapshot())
        .ok();
    let mut grants = list()?
        .into_iter()
        .filter(|grant| match &grant.scope {
            CredentialScope::Company => true,
            CredentialScope::Agent {
                agent_id: recipient,
            } => recipient == agent_id,
            CredentialScope::Group { group_id } => snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.groups.iter().any(|group| {
                    group.profile.group_id == *group_id
                        && group.profile.lifecycle
                            == crate::runtime::company_directory::LifecycleState::Active
                }) && snapshot
                    .members
                    .iter()
                    .any(|member| member.group_id == *group_id && member.agent_id == agent_id)
            }),
        })
        .collect::<Vec<_>>();
    if let Some(grant) = crate::onboarding::cookie_grant_for_agent(agent_id) {
        grants.push(grant);
    }
    Ok(grants)
}

pub fn agent_id_for_profile(profile_id: &str) -> Result<String> {
    if let Ok(company) = crate::runtime::company::global() {
        if let Ok(snapshot) = company.directory_snapshot() {
            if let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.browser_profile_id == profile_id)
            {
                return Ok(agent.profile.agent_id.clone());
            }
        }
    }
    let fallback = profile_id.strip_prefix("agent-").unwrap_or(profile_id);
    validate_id(fallback, "browser profile agent id")?;
    Ok(fallback.to_string())
}

pub fn authorized_scope(
    requested: &str,
    current_agent_id: &str,
    current_group_id: Option<&str>,
) -> Result<CredentialScope> {
    match requested.trim().to_ascii_lowercase().as_str() {
        "agent" | "private" | "local" => Ok(CredentialScope::agent(current_agent_id)),
        "company" | "global" => Ok(CredentialScope::Company),
        "group" => {
            let group_id = current_group_id
                .context("group cookie scope is available only inside that group's conversation")?;
            let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
            anyhow::ensure!(
                snapshot.members.iter().any(|member| {
                    member.group_id == group_id && member.agent_id == current_agent_id
                }),
                "current coworker is not a member of group `{group_id}`"
            );
            Ok(CredentialScope::group(group_id))
        }
        other => anyhow::bail!("unknown cookie scope `{other}`; use agent, group, or company"),
    }
}

/// Credential scopes visible to one coworker in the current conversation.
/// Direct chats see private + company credentials; a group thread additionally
/// sees that active group's credentials, but only for current members.
pub fn visible_credential_scopes(
    current_agent_id: &str,
    current_group_id: Option<&str>,
) -> Result<Vec<CredentialScope>> {
    validate_id(current_agent_id, "agent id")?;
    let mut scopes = vec![
        CredentialScope::agent(current_agent_id),
        CredentialScope::Company,
    ];
    if let Some(group_id) = current_group_id {
        let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
        let active = snapshot.groups.iter().any(|group| {
            group.profile.group_id == group_id
                && group.profile.lifecycle
                    == crate::runtime::company_directory::LifecycleState::Active
        });
        let member = snapshot
            .members
            .iter()
            .any(|member| member.group_id == group_id && member.agent_id == current_agent_id);
        anyhow::ensure!(
            active && member,
            "current coworker cannot access group credentials"
        );
        scopes.push(CredentialScope::group(group_id));
    }
    Ok(scopes)
}

fn store_path() -> std::path::PathBuf {
    store_path_at(&crate::config::phoenix_home())
}

fn store_path_at(phoenix_home: &std::path::Path) -> std::path::PathBuf {
    phoenix_home.join("browser/cookie-grants.json")
}

fn parse_store(bytes: Option<&[u8]>) -> Result<GrantStore> {
    let Some(bytes) = bytes else {
        return Ok(GrantStore::default());
    };
    anyhow::ensure!(
        bytes.len() <= MAX_STORE_BYTES,
        "cookie grant store is oversized"
    );
    let store: GrantStore = serde_json::from_slice(bytes).context("invalid cookie grant store")?;
    anyhow::ensure!(
        store.version == STORE_VERSION,
        "unsupported cookie grant version"
    );
    anyhow::ensure!(
        store.grants.len() <= MAX_GRANTS,
        "cookie grant store is oversized"
    );
    for grant in &store.grants {
        validate_id(&grant.grant_id, "grant id")?;
        validate_id(&grant.created_by_agent_id, "agent id")?;
        validate_scope(&grant.scope)?;
        anyhow::ensure!(
            !grant.all_portable,
            "blanket cookie authority may only come from onboarding"
        );
        anyhow::ensure!(
            normalize_site(&grant.site)? == grant.site,
            "non-canonical grant site"
        );
        anyhow::ensure!(
            crate::tools::browser_cookies::is_supported_source(&grant.source),
            "unsupported stored cookie source"
        );
    }
    Ok(store)
}

fn validate_scope(scope: &CredentialScope) -> Result<()> {
    match scope {
        CredentialScope::Agent { agent_id } => validate_id(agent_id, "agent id"),
        CredentialScope::Group { group_id } => validate_id(group_id, "group id"),
        CredentialScope::Company => Ok(()),
    }
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty() && value.len() <= 128, "invalid {label}");
    anyhow::ensure!(
        value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "invalid {label}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_matching_is_bounded_to_the_requested_registrable_tree() {
        assert!(domain_matches_site(".github.com", "github.com"));
        assert!(domain_matches_site("api.github.com", "github.com"));
        assert!(!domain_matches_site("notgithub.com", "github.com"));
        assert!(!domain_matches_site(
            "github.com.attacker.test",
            "github.com"
        ));
    }

    #[test]
    fn grants_are_private_deduplicated_and_scope_checked() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let one = add(
            "firefox",
            "https://www.github.com/login",
            CredentialScope::agent("coder"),
            "coder",
        )
        .unwrap();
        let duplicate = add(
            "firefox",
            "github.com",
            CredentialScope::agent("coder"),
            "coder",
        )
        .unwrap();
        assert_eq!(one.grant_id, duplicate.grant_id);
        assert_eq!(list().unwrap().len(), 1);
        assert_eq!(applicable_to("coder").unwrap().len(), 1);
        assert!(applicable_to("finance").unwrap().is_empty());
        assert!(remove(&one.grant_id).unwrap());
        assert!(list().unwrap().is_empty());
    }

    #[test]
    fn scope_purge_removes_private_grants_without_touching_company() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        add(
            "firefox",
            "github.com",
            CredentialScope::agent("coder"),
            "coder",
        )
        .unwrap();
        add(
            "firefox",
            "example.com",
            CredentialScope::Company,
            "phoenix",
        )
        .unwrap();
        assert_eq!(remove_scope(&CredentialScope::agent("coder")).unwrap(), 1);
        let grants = list().unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].scope, CredentialScope::Company);
    }

    #[test]
    fn persisted_store_cannot_smuggle_blanket_cookie_authority() {
        let raw = serde_json::to_vec(&GrantStore {
            version: STORE_VERSION,
            generation: 1,
            grants: vec![CookieGrant {
                grant_id: "broad".to_string(),
                source: "firefox".to_string(),
                site: "*".to_string(),
                all_portable: true,
                scope: CredentialScope::agent("coder"),
                created_by_agent_id: "onboarding".to_string(),
                created_at: "2026-08-14T00:00:00Z".to_string(),
            }],
        })
        .unwrap();
        assert!(parse_store(Some(&raw)).is_err());
    }
}
