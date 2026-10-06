//! Secret-safe credential tools for coworkers.
//!
//! Agents may inspect metadata and generate a strong stored password, but no
//! operation returns secret material to the model. Browser form filling is a
//! separate native action that reads the vault and types directly into Chrome.

use anyhow::{Context, Result};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::ToolOutput;
use crate::security::vault::Vault;

#[derive(Debug, Deserialize)]
pub struct CredentialListInput {
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct CredentialGenerateInput {
    pub site: String,
    pub label: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_password_length")]
    pub length: usize,
}

fn default_limit() -> usize {
    50
}

fn default_scope() -> String {
    "agent".to_string()
}

fn default_password_length() -> usize {
    24
}

pub fn list(
    input: CredentialListInput,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    anyhow::ensure!((1..=200).contains(&input.limit), "limit must be 1..=200");
    let scopes =
        crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
    let site = input
        .site
        .as_deref()
        .map(crate::tools::browser_cookie_grants::normalize_site)
        .transpose()?;
    let mut credentials = Vault::open_default().list_for_agent(&scopes)?;
    if let Some(site) = site.as_deref() {
        credentials.retain(|credential| credential.site == site);
    }
    credentials.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    let omitted = credentials.len().saturating_sub(input.limit);
    credentials.truncate(input.limit);
    let content = serde_json::to_string_pretty(&credentials)?;
    Ok(ToolOutput {
        summary: format!(
            "{} visible credential{}{}",
            credentials.len(),
            if credentials.len() == 1 { "" } else { "s" },
            if omitted > 0 {
                format!(" ({omitted} more omitted)")
            } else {
                String::new()
            }
        ),
        content,
    })
}

pub fn generate(
    input: CredentialGenerateInput,
    agent_id: &str,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    anyhow::ensure!(
        (16..=128).contains(&input.length),
        "generated password length must be 16..=128"
    );
    let scope =
        crate::tools::browser_cookie_grants::authorized_scope(&input.scope, agent_id, group_id)?;
    let password = generate_password(input.length);
    let metadata_json = serde_json::json!({
        "generated_by": "phoenix",
        "password_length": input.length
    })
    .to_string();
    let credential = Vault::open_default()
        .put_for_agent(
            scope,
            &input.site,
            &input.label,
            input.username,
            "password",
            &metadata_json,
            password.as_str(),
        )
        .context("could not store generated credential")?;
    Ok(ToolOutput {
        summary: format!("generated and stored credential {}", credential.credential_id),
        content: format!(
            "Created a strong password for {} and stored it as credential `{}` with {:?} scope. The password was not exposed to the model. Use browser_input_credential with this id to fill it.",
            credential.site, credential.credential_id, credential.scope
        ),
    })
}

/// Rejection-sampled cryptographic password generation with a balanced,
/// site-friendly alphabet. The temporary plaintext is wiped on drop.
fn generate_password(length: usize) -> Zeroizing<String> {
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const DIGIT: &[u8] = b"23456789";
    const SYMBOL: &[u8] = b"!@#$%^&*-_=+";
    const ALL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^&*-_=+";

    let mut bytes = Zeroizing::new(Vec::with_capacity(length));
    bytes.push(random_from(UPPER));
    bytes.push(random_from(LOWER));
    bytes.push(random_from(DIGIT));
    bytes.push(random_from(SYMBOL));
    while bytes.len() < length {
        bytes.push(random_from(ALL));
    }
    // Fisher-Yates so the guaranteed character classes do not occupy fixed
    // positions. OsRng supplies every draw.
    for index in (1..bytes.len()).rev() {
        let swap = random_below(index + 1);
        bytes.swap(index, swap);
    }
    Zeroizing::new(String::from_utf8(bytes.to_vec()).expect("password alphabet is ASCII"))
}

fn random_from(alphabet: &[u8]) -> u8 {
    alphabet[random_below(alphabet.len())]
}

fn random_below(bound: usize) -> usize {
    debug_assert!(bound > 0);
    let zone = u64::MAX - (u64::MAX % bound as u64);
    loop {
        let mut bytes = [0_u8; 8];
        OsRng.fill_bytes(&mut bytes);
        let value = u64::from_le_bytes(bytes);
        if value < zone {
            return (value % bound as u64) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_password_has_every_required_class_and_length() {
        let password = generate_password(40);
        assert_eq!(password.len(), 40);
        assert!(password.bytes().any(|byte| byte.is_ascii_uppercase()));
        assert!(password.bytes().any(|byte| byte.is_ascii_lowercase()));
        assert!(password.bytes().any(|byte| byte.is_ascii_digit()));
        assert!(password.bytes().any(|byte| b"!@#$%^&*-_=+".contains(&byte)));
    }

    #[test]
    fn generated_tool_never_returns_the_password() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        Vault::open_default()
            .initialize("a sufficiently long master password")
            .unwrap();
        let output = generate(
            CredentialGenerateInput {
                site: "example.com".into(),
                label: "Example".into(),
                username: Some("owner@example.com".into()),
                scope: "agent".into(),
                length: 24,
            },
            "phoenix",
            None,
        )
        .unwrap();
        assert!(output.content.contains("not exposed to the model"));
        let metadata = Vault::open_default()
            .list(&[crate::security::vault::CredentialScope::agent("phoenix")])
            .unwrap();
        assert_eq!(metadata.len(), 1);
        let revealed = Vault::open_default()
            .reveal(
                &metadata[0].credential_id,
                &[crate::security::vault::CredentialScope::agent("phoenix")],
            )
            .unwrap();
        assert!(!output.content.contains(revealed.secret()));
        assert!(!output.summary.contains(revealed.secret()));
    }
}
