//! Bot credentials use the existing encrypted vault. Connection files contain
//! only an opaque reference bound to one agent, destination and workspace.
use super::{ChannelConfig, Platform};
use crate::config::private_io;
use crate::security::vault::{CredentialScope, RevealedCredential, Vault};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const KIND: &str = "channel_bot_token";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    credential_id: String,
    binding: String,
}

fn path(home: &Path, config: &ChannelConfig) -> PathBuf {
    home.join("channels")
        .join(&config.id)
        .join("token-reference.json")
}
fn reference(home: &Path, config: &ChannelConfig) -> Result<Option<Reference>> {
    config.validate()?;
    let Some(bytes) = private_io::read_private_file_limited(&path(home, config), 4096)? else {
        return Ok(None);
    };
    let value: Reference =
        serde_json::from_slice(&bytes).context("Saved bot login reference is unreadable")?;
    anyhow::ensure!(
        value.binding == config.binding(),
        "Saved bot login belongs to a different connection"
    );
    Ok(Some(value))
}
fn reveal(
    vault: &Vault,
    config: &ChannelConfig,
    reference: &Reference,
) -> Result<RevealedCredential> {
    let value = vault
        .reveal_for_agent(
            &reference.credential_id,
            &[CredentialScope::agent(&config.agent_id)],
        )
        .context(
            "Saved bot login is unavailable. Unlock the vault in Phoenix or replace this login",
        )?;
    let expected = serde_json::json!({"channel_id":config.id,"binding":config.binding()});
    let actual: serde_json::Value = serde_json::from_str(&value.metadata.metadata_json)?;
    anyhow::ensure!(
        value.metadata.kind == KIND && actual == expected,
        "Saved credential does not belong to this channel"
    );
    Ok(value)
}
/// Ownership/binding check from public metadata only, so saving or forgetting
/// a bot login never needs Passes unlocked.
fn verify_binding(vault: &Vault, config: &ChannelConfig, reference: &Reference) -> Result<bool> {
    let Some(metadata) = vault
        .list_for_agent(&[CredentialScope::agent(&config.agent_id)])?
        .into_iter()
        .find(|meta| meta.credential_id == reference.credential_id)
    else {
        return Ok(false);
    };
    let expected = serde_json::json!({"channel_id":config.id,"binding":config.binding()});
    let actual: serde_json::Value = serde_json::from_str(&metadata.metadata_json)?;
    anyhow::ensure!(
        metadata.kind == KIND && actual == expected,
        "Saved credential does not belong to this channel"
    );
    Ok(true)
}
pub fn saved(home: &Path, config: &ChannelConfig) -> Result<bool> {
    Ok(reference(home, config)?.is_some())
}
pub fn load(home: &Path, config: &ChannelConfig) -> Result<Option<Zeroizing<String>>> {
    let Some(reference) = reference(home, config)? else {
        return Ok(None);
    };
    let value = reveal(&Vault::at(home), config, &reference)?;
    Ok(Some(Zeroizing::new(value.secret().to_string())))
}
pub fn save(home: &Path, config: &ChannelConfig, token: &str) -> Result<()> {
    config.validate()?;
    anyhow::ensure!(
        !token.trim().is_empty() && token.len() <= 8192,
        "Enter a valid bot token"
    );
    let vault = Vault::at(home);
    private_io::with_private_lock(&path(home, config).with_extension("operation"), || {
        if let Some(reference) = reference(home, config)?
            .filter(|reference| verify_binding(&vault, config, reference).unwrap_or(false))
        {
            return vault.replace_secret_for_agent(
                &reference.credential_id,
                &[CredentialScope::agent(&config.agent_id)],
                token.trim(),
            );
        }
        let created = vault
            .put_for_agent(
                CredentialScope::agent(&config.agent_id),
                match config.platform {
                    Platform::Telegram => "api.telegram.org",
                    Platform::Discord => "discord.com",
                },
                &format!("{} bot login", config.name),
                None,
                KIND,
                &serde_json::json!({"channel_id":config.id,"binding":config.binding()}).to_string(),
                token.trim(),
            )
            .context("Set up or unlock the vault in Phoenix to remember this bot login")?;
        let reference = Reference {
            credential_id: created.credential_id.clone(),
            binding: config.binding(),
        };
        if let Err(error) =
            private_io::atomic_write_private(&path(home, config), &serde_json::to_vec(&reference)?)
        {
            let _ = vault.delete_for_agent(
                &created.credential_id,
                &[CredentialScope::agent(&config.agent_id)],
            );
            return Err(error);
        }
        Ok(())
    })
}
pub fn forget(home: &Path, config: &ChannelConfig) -> Result<()> {
    config.validate()?;
    private_io::with_private_lock(&path(home, config).with_extension("operation"), || {
        if let Some(reference) = reference(home, config)? {
            let vault = Vault::at(home);
            // Verify ownership before removing a referenced credential. A stale
            // or altered reference must not delete another saved login.
            let scope = CredentialScope::agent(&config.agent_id);
            let _ = scope;
            verify_binding(&vault, config, &reference)?;
            vault.delete_for_agent(
                &reference.credential_id,
                &[CredentialScope::agent(&config.agent_id)],
            )?;
            private_io::remove_private_file(&path(home, config))?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_vault_login_survives_lock_rotates_and_stays_bound_to_its_owner() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let mut config = ChannelConfig {
            id: "personal".into(),
            name: "Avery chat".into(),
            platform: Platform::Telegram,
            conversation_id: "123".into(),
            allowed_user_ids: vec!["123".into()],
            agent_id: "avery".into(),
            session_id: "agent-avery".into(),
            workspace: home.path().to_path_buf(),
            token_env: "PHOENIX_TEST_CHANNEL_SECRET_UNUSED".into(),
            enabled: true, group_id:None 
        };
        assert!(!saved(home.path(), &config).unwrap());
        // Saving never needs a master password.
        save(home.path(), &config, "token-before-setup").unwrap();
        assert_eq!(load(home.path(), &config).unwrap().unwrap().as_str(), "token-before-setup");
        let vault = Vault::at(home.path());
        vault
            .initialize("a sufficiently long test master password")
            .unwrap();
        save(home.path(), &config, "first-bot-secret").unwrap();
        let first = reference(home.path(), &config)
            .unwrap()
            .unwrap()
            .credential_id;
        assert_eq!(
            load(home.path(), &config).unwrap().unwrap().as_str(),
            "first-bot-secret"
        );
        vault.lock();
        // Locked: the token can be replaced but not read back.
        assert!(load(home.path(), &config).is_err());
        save(home.path(), &config, "replacement-bot-secret").unwrap();
        assert_eq!(vault.status(), crate::security::vault::VaultStatus::Locked);
        vault
            .unlock_with_password("a sufficiently long test master password")
            .unwrap();
        assert_eq!(
            reference(home.path(), &config)
                .unwrap()
                .unwrap()
                .credential_id,
            first
        );
        assert_eq!(
            load(home.path(), &config).unwrap().unwrap().as_str(),
            "replacement-bot-secret"
        );
        for p in [
            home.path().join("vault/passes.json"),
            path(home.path(), &config),
        ] {
            let bytes = std::fs::read(p).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("bot-secret"));
        }
        config.name = "Renamed chat".into();
        assert!(load(home.path(), &config).unwrap().is_some());
        let mut other = config.clone();
        other.conversation_id = "456".into();
        assert!(load(home.path(), &other).is_err());
        assert!(forget(home.path(), &other).is_err());
        other = config.clone();
        other.agent_id = "phoenix".into();
        assert!(load(home.path(), &other).is_err());
        assert!(vault
            .replace_secret_for_agent(&first, &[CredentialScope::agent("phoenix")], "forbidden")
            .is_err());
        assert!(!vault
            .delete_for_agent(&first, &[CredentialScope::agent("phoenix")])
            .unwrap());
        forget(home.path(), &config).unwrap();
        assert!(!saved(home.path(), &config).unwrap());
        assert!(load(home.path(), &config).unwrap().is_none());
        forget(home.path(), &config).unwrap();
        assert!(vault
            .list_for_agent(&[CredentialScope::agent("avery")])
            .unwrap()
            .is_empty());
        // Removing a login manually must not strand its channel reference.
        save(home.path(), &config, "manually-removed-secret").unwrap();
        let reference = reference(home.path(), &config).unwrap().unwrap();
        vault
            .delete_for_agent(&reference.credential_id, &[CredentialScope::agent("avery")])
            .unwrap();
        forget(home.path(), &config).unwrap();
        assert!(!saved(home.path(), &config).unwrap());
    }
}
