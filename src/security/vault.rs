//! Locally encrypted, scope-aware credentials for Phoenix coworkers.
//!
//! Chrome profiles retain cookies and browser storage. Passwords, recovery
//! codes, API tokens, and account metadata do not belong in Chrome's password
//! database or plaintext JSON: they live here, encrypted under a random data
//! key. The data key is wrapped independently by the user's master password
//! and a high-entropy recovery key, so changing either unlock method never
//! requires re-encrypting every credential.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const VAULT_VERSION: u32 = 2;
const RECOVERY_PREFIX: &str = "PHX1-";
const MAX_RECORDS: usize = 16_384;
const MAX_SECRET_BYTES: usize = 1024 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_SITE_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 512;
const KDF_OUTPUT_BYTES: usize = 32;
const AGENT_RUNTIME_WRAP_AAD: &[u8] = b"phoenix-vault-agent-runtime-wrap-v1";

struct UnlockedVaultKey {
    key: Zeroizing<[u8; 32]>,
    last_used: Instant,
}

static UNLOCKED_KEYS: OnceLock<Mutex<HashMap<PathBuf, UnlockedVaultKey>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum CredentialScope {
    Agent { agent_id: String },
    Group { group_id: String },
    Company,
}

impl CredentialScope {
    pub fn agent(agent_id: impl Into<String>) -> Self {
        Self::Agent {
            agent_id: agent_id.into(),
        }
    }

    pub fn group(group_id: impl Into<String>) -> Self {
        Self::Group {
            group_id: group_id.into(),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Agent { agent_id } => validate_id(agent_id, "agent id"),
            Self::Group { group_id } => validate_id(group_id, "group id"),
            Self::Company => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialMetadata {
    pub credential_id: String,
    pub scope: CredentialScope,
    /// Canonical lower-case site/domain (`github.com`), never a URL containing
    /// query parameters or embedded credentials.
    pub site: String,
    pub label: String,
    pub username: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub metadata_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Zeroize)]
#[zeroize(drop)]
struct SecretRecord {
    #[zeroize(skip)]
    metadata: CredentialMetadata,
    secret: String,
}

/// A decrypted credential returned to the one caller that requested it. The
/// secret buffer is wiped on drop and is intentionally omitted from Debug.
pub struct RevealedCredential {
    pub metadata: CredentialMetadata,
    secret: Zeroizing<String>,
}

impl std::fmt::Debug for RevealedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevealedCredential")
            .field("metadata", &self.metadata)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl RevealedCredential {
    pub fn secret(&self) -> &str {
        self.secret.as_str()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WrappedKey {
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultConfig {
    version: u32,
    password_salt: String,
    password_wrapped_key: WrappedKey,
    recovery_wrapped_key: WrappedKey,
    /// Device-local wrapping lets the Phoenix gateway use an approved secret
    /// without making the human management surface unlocked. Older vaults
    /// acquire this field the next time the owner unlocks them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_runtime_wrapped_key: Option<WrappedKey>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedStore {
    version: u32,
    records: Vec<EncryptedRecord>,
}

impl Default for EncryptedStore {
    fn default() -> Self {
        Self {
            version: VAULT_VERSION,
            records: Vec::new(),
        }
    }
}

/// Scope and opaque id stay outside the ciphertext so a 30-day lifecycle
/// purge can physically remove the exact encrypted records while the vault is
/// locked. Labels, usernames, metadata, and secrets remain encrypted.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedRecord {
    credential_id: String,
    scope: CredentialScope,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    Uninitialized,
    Locked,
    Unlocked,
}

#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    pub fn open_default() -> Self {
        Self::at(crate::config::phoenix_home())
    }

    pub fn at(phoenix_home: impl Into<PathBuf>) -> Self {
        Self {
            root: phoenix_home.into().join("vault"),
        }
    }

    pub fn status(&self) -> VaultStatus {
        if !self.config_path().is_file() {
            VaultStatus::Uninitialized
        } else if self.unlocked_key(false).is_some() {
            VaultStatus::Unlocked
        } else {
            VaultStatus::Locked
        }
    }

    /// Create a new empty vault and return its one-time recovery key. The
    /// caller must show that key to the user and require them to save it; it is
    /// never written back to the Phoenix home in plaintext.
    pub fn initialize(&self, master_password: &str) -> Result<Zeroizing<String>> {
        validate_master_password(master_password)?;
        let config_path = self.config_path();
        let store_path = self.store_path();
        // A distinct initialization lock coordinates the two-file creation;
        // locking config.json itself and then calling its atomic writer would
        // recursively acquire the same advisory lock.
        crate::config::private_io::with_private_lock(&self.root.join("initialize"), || {
            anyhow::ensure!(
                !config_path.exists(),
                "credential vault is already initialized"
            );
            // The config is the commit marker. A data file without it can only
            // be an interrupted first initialization and has no recoverable
            // wrapping key, so replace that orphan instead of wedging setup.
            if store_path.exists() {
                crate::config::private_io::remove_private_file(&store_path)?;
            }

            let mut data_key = Zeroizing::new([0u8; 32]);
            OsRng.fill_bytes(data_key.as_mut());
            let mut salt = [0u8; 16];
            OsRng.fill_bytes(&mut salt);
            let password_key = derive_password_key(master_password, &salt)?;
            let recovery_key_bytes = random_key();
            let agent_runtime_key = random_key();
            let recovery_text = Zeroizing::new(format!(
                "{RECOVERY_PREFIX}{}",
                URL_SAFE_NO_PAD.encode(recovery_key_bytes.as_ref())
            ));
            let config = VaultConfig {
                version: VAULT_VERSION,
                password_salt: URL_SAFE_NO_PAD.encode(salt),
                password_wrapped_key: encrypt_key(
                    &*password_key,
                    &*data_key,
                    b"phoenix-vault-password-wrap-v1",
                )?,
                recovery_wrapped_key: encrypt_key(
                    &*recovery_key_bytes,
                    &*data_key,
                    b"phoenix-vault-recovery-wrap-v1",
                )?,
                agent_runtime_wrapped_key: Some(encrypt_key(
                    &*agent_runtime_key,
                    &*data_key,
                    AGENT_RUNTIME_WRAP_AAD,
                )?),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let store = EncryptedStore::default();
            crate::config::private_io::atomic_write_private(
                &store_path,
                &serde_json::to_vec_pretty(&store)?,
            )?;
            if let Err(error) = crate::config::private_io::atomic_write_private(
                &self.agent_runtime_key_path(),
                agent_runtime_key.as_ref(),
            ) {
                let _ = crate::config::private_io::remove_private_file(&store_path);
                return Err(error);
            }
            if let Err(error) = crate::config::private_io::atomic_write_private(
                &config_path,
                &serde_json::to_vec_pretty(&config)?,
            ) {
                let _ = crate::config::private_io::remove_private_file(&store_path);
                let _ =
                    crate::config::private_io::remove_private_file(&self.agent_runtime_key_path());
                return Err(error);
            }
            self.remember_key(&*data_key);
            Ok(recovery_text)
        })
    }

    pub fn unlock_with_password(&self, master_password: &str) -> Result<()> {
        let config = self.read_config()?;
        let salt = decode_exact::<16>(&config.password_salt, "vault password salt")?;
        let password_key = derive_password_key(master_password, &salt)?;
        let data_key = decrypt_key(
            &*password_key,
            &config.password_wrapped_key,
            b"phoenix-vault-password-wrap-v1",
        )
        .context("master password is incorrect or vault metadata was changed")?;
        self.verify_and_remember(data_key)
    }

    pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<()> {
        let config = self.read_config()?;
        let recovery_key = parse_recovery_key(recovery_key)?;
        let data_key = decrypt_key(
            &*recovery_key,
            &config.recovery_wrapped_key,
            b"phoenix-vault-recovery-wrap-v1",
        )
        .context("recovery key is incorrect or vault metadata was changed")?;
        self.verify_and_remember(data_key)
    }

    pub fn lock(&self) {
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        keys.remove(&self.root_key());
    }

    /// Re-wrap the unchanged data key under a new master password. The
    /// recovery key remains valid, and credential ciphertext is untouched.
    pub fn change_master_password(&self, current_password: &str, new_password: &str) -> Result<()> {
        validate_master_password(new_password)?;
        let config_path = self.config_path();
        crate::config::private_io::read_modify_write_private(&config_path, |current| {
            let current = current.context("credential vault is not initialized")?;
            let mut config: VaultConfig =
                serde_json::from_slice(current).context("invalid vault config")?;
            anyhow::ensure!(config.version == VAULT_VERSION, "unsupported vault version");
            let old_salt = decode_exact::<16>(&config.password_salt, "vault password salt")?;
            let old_key = derive_password_key(current_password, &old_salt)?;
            let data_key = decrypt_key(
                &*old_key,
                &config.password_wrapped_key,
                b"phoenix-vault-password-wrap-v1",
            )
            .context("current master password is incorrect")?;
            let mut new_salt = [0u8; 16];
            OsRng.fill_bytes(&mut new_salt);
            let new_key = derive_password_key(new_password, &new_salt)?;
            config.password_salt = URL_SAFE_NO_PAD.encode(new_salt);
            config.password_wrapped_key =
                encrypt_key(&*new_key, &*data_key, b"phoenix-vault-password-wrap-v1")?;
            Ok(((), serde_json::to_vec_pretty(&config)?))
        })?;
        self.unlock_with_password(new_password)
    }

    /// Invalidate the previous recovery key and return a one-time replacement.
    /// The vault must already be unlocked through either existing method.
    pub fn rotate_recovery_key(&self) -> Result<Zeroizing<String>> {
        let data_key = self.require_key()?;
        let recovery_key = random_key();
        let recovery_text = Zeroizing::new(format!(
            "{RECOVERY_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(recovery_key.as_ref())
        ));
        let config_path = self.config_path();
        crate::config::private_io::read_modify_write_private(&config_path, |current| {
            let current = current.context("credential vault is not initialized")?;
            let mut config: VaultConfig =
                serde_json::from_slice(current).context("invalid vault config")?;
            config.recovery_wrapped_key = encrypt_key(
                &*recovery_key,
                &*data_key,
                b"phoenix-vault-recovery-wrap-v1",
            )?;
            Ok(((), serde_json::to_vec_pretty(&config)?))
        })?;
        Ok(recovery_text)
    }

    pub fn put(
        &self,
        scope: CredentialScope,
        site: &str,
        label: &str,
        username: Option<String>,
        kind: &str,
        metadata_json: &str,
        secret: &str,
    ) -> Result<CredentialMetadata> {
        let key = self.require_key()?;
        self.put_with_key(
            scope,
            site,
            label,
            username,
            kind,
            metadata_json,
            secret,
            &key,
        )
    }

    /// Store a secret generated during an approved agent workflow without
    /// unlocking the human management surface or exposing the value.
    #[allow(clippy::too_many_arguments)]
    pub fn put_for_agent(
        &self,
        scope: CredentialScope,
        site: &str,
        label: &str,
        username: Option<String>,
        kind: &str,
        metadata_json: &str,
        secret: &str,
    ) -> Result<CredentialMetadata> {
        let key = self.require_agent_key()?;
        self.put_with_key(
            scope,
            site,
            label,
            username,
            kind,
            metadata_json,
            secret,
            &key,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn put_with_key(
        &self,
        scope: CredentialScope,
        site: &str,
        label: &str,
        username: Option<String>,
        kind: &str,
        metadata_json: &str,
        secret: &str,
        key: &[u8; 32],
    ) -> Result<CredentialMetadata> {
        scope.validate()?;
        let site = normalize_site(site)?;
        validate_text(label, MAX_LABEL_BYTES, "credential label")?;
        validate_text(kind, MAX_LABEL_BYTES, "credential kind")?;
        validate_metadata(metadata_json)?;
        anyhow::ensure!(!secret.is_empty(), "credential secret cannot be empty");
        anyhow::ensure!(
            secret.len() <= MAX_SECRET_BYTES,
            "credential secret exceeds {MAX_SECRET_BYTES} bytes"
        );
        if let Some(username) = username.as_deref() {
            validate_text(username, MAX_LABEL_BYTES, "credential username")?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let metadata = CredentialMetadata {
            credential_id: uuid::Uuid::new_v4().to_string(),
            scope,
            site,
            label: label.to_string(),
            username,
            kind: kind.to_string(),
            metadata_json: metadata_json.to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        let store_path = self.store_path();
        crate::config::private_io::read_modify_write_private(&store_path, |current| {
            let mut store = parse_encrypted_store(current)?;
            anyhow::ensure!(
                store.records.len() < MAX_RECORDS,
                "credential vault is full"
            );
            let record = SecretRecord {
                metadata: metadata.clone(),
                secret: secret.to_string(),
            };
            store.records.push(encrypt_record(key, &record)?);
            Ok((metadata.clone(), serde_json::to_vec_pretty(&store)?))
        })
    }

    pub fn list(&self, visible_scopes: &[CredentialScope]) -> Result<Vec<CredentialMetadata>> {
        let key = self.require_key()?;
        self.list_with_key(visible_scopes, &key)
    }

    /// Return only metadata to an agent. Human-visible lock state is not
    /// changed or touched.
    pub fn list_for_agent(
        &self,
        visible_scopes: &[CredentialScope],
    ) -> Result<Vec<CredentialMetadata>> {
        let key = self.require_agent_key()?;
        self.list_with_key(visible_scopes, &key)
    }

    fn list_with_key(
        &self,
        visible_scopes: &[CredentialScope],
        key: &[u8; 32],
    ) -> Result<Vec<CredentialMetadata>> {
        for scope in visible_scopes {
            scope.validate()?;
        }
        let bytes = crate::config::private_io::read_private_file(&self.store_path())?
            .context("credential vault store is missing")?;
        let store = parse_encrypted_store(Some(&bytes))?;
        let mut visible = Vec::new();
        for encrypted in store
            .records
            .iter()
            .filter(|record| visible_scopes.contains(&record.scope))
        {
            visible.push(decrypt_record(key, encrypted)?.metadata.clone());
        }
        Ok(visible)
    }

    pub fn reveal(
        &self,
        credential_id: &str,
        visible_scopes: &[CredentialScope],
    ) -> Result<RevealedCredential> {
        let key = self.require_key()?;
        self.reveal_with_key(credential_id, visible_scopes, &key)
    }

    /// Decrypt one already-authorized secret for a bounded native agent action.
    /// The secret remains zeroizing and never enters model output.
    pub fn reveal_for_agent(
        &self,
        credential_id: &str,
        visible_scopes: &[CredentialScope],
    ) -> Result<RevealedCredential> {
        let key = self.require_agent_key()?;
        self.reveal_with_key(credential_id, visible_scopes, &key)
    }

    fn reveal_with_key(
        &self,
        credential_id: &str,
        visible_scopes: &[CredentialScope],
        key: &[u8; 32],
    ) -> Result<RevealedCredential> {
        validate_id(credential_id, "credential id")?;
        let bytes = crate::config::private_io::read_private_file(&self.store_path())?
            .context("credential vault store is missing")?;
        let store = parse_encrypted_store(Some(&bytes))?;
        let encrypted = store
            .records
            .iter()
            .find(|record| {
                record.credential_id == credential_id && visible_scopes.contains(&record.scope)
            })
            .context("credential does not exist in the caller's visible scopes")?;
        let mut record = decrypt_record(key, encrypted)?;
        Ok(RevealedCredential {
            metadata: record.metadata.clone(),
            secret: Zeroizing::new(std::mem::take(&mut record.secret)),
        })
    }

    pub fn delete(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<bool> {
        validate_id(credential_id, "credential id")?;
        let _key = self.require_key()?;
        self.delete_scoped(credential_id, visible_scopes)
    }

    /// Remove a credential through an already-authorized native workflow.
    /// This never unlocks the human management surface.
    pub(crate) fn delete_for_agent(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<bool> {
        validate_id(credential_id, "credential id")?;
        let _key = self.require_agent_key()?;
        self.delete_scoped(credential_id, visible_scopes)
    }

    fn delete_scoped(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<bool> {
        let store_path = self.store_path();
        crate::config::private_io::read_modify_write_private(&store_path, |current| {
            let mut store = parse_encrypted_store(current)?;
            let before = store.records.len();
            store.records.retain(|record| {
                record.credential_id != credential_id || !visible_scopes.contains(&record.scope)
            });
            let removed = store.records.len() != before;
            Ok((removed, serde_json::to_vec_pretty(&store)?))
        })
    }

    /// Rotate only the secret of an authorized native credential. Ownership
    /// and metadata cannot change through this narrower runtime operation.
    pub(crate) fn replace_secret_for_agent(&self, credential_id: &str, visible_scopes: &[CredentialScope], secret: &str) -> Result<()> {
        validate_id(credential_id, "credential id")?;
        anyhow::ensure!(!secret.is_empty() && secret.len() <= MAX_SECRET_BYTES, "Invalid replacement secret size");
        let key = self.require_agent_key()?;
        crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
            let mut store = parse_encrypted_store(current)?;
            let index = store.records.iter().position(|record| record.credential_id == credential_id && visible_scopes.contains(&record.scope))
                .context("credential does not exist in the caller's visible scopes")?;
            let mut record = decrypt_record(&*key, &store.records[index])?;
            record.secret.zeroize();
            record.secret.push_str(secret);
            record.metadata.updated_at = chrono::Utc::now().to_rfc3339();
            store.records[index] = encrypt_record(&*key, &record)?;
            Ok(((), serde_json::to_vec_pretty(&store)?))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        credential_id: &str,
        visible_scopes: &[CredentialScope],
        scope: CredentialScope,
        site: &str,
        label: &str,
        username: Option<String>,
        kind: &str,
        metadata_json: &str,
        replacement_secret: Option<&str>,
    ) -> Result<CredentialMetadata> {
        validate_id(credential_id, "credential id")?;
        scope.validate()?;
        let site = normalize_site(site)?;
        validate_text(label, MAX_LABEL_BYTES, "credential label")?;
        validate_text(kind, MAX_LABEL_BYTES, "credential kind")?;
        validate_metadata(metadata_json)?;
        if let Some(username) = username.as_deref() {
            validate_text(username, MAX_LABEL_BYTES, "credential username")?;
        }
        if let Some(secret) = replacement_secret {
            anyhow::ensure!(!secret.is_empty(), "credential secret cannot be empty");
            anyhow::ensure!(
                secret.len() <= MAX_SECRET_BYTES,
                "credential secret exceeds {MAX_SECRET_BYTES} bytes"
            );
        }

        let key = self.require_key()?;
        let store_path = self.store_path();
        crate::config::private_io::read_modify_write_private(&store_path, |current| {
            let mut store = parse_encrypted_store(current)?;
            let index = store
                .records
                .iter()
                .position(|record| {
                    record.credential_id == credential_id && visible_scopes.contains(&record.scope)
                })
                .context("credential does not exist in the caller's visible scopes")?;
            let mut record = decrypt_record(&*key, &store.records[index])?;
            record.metadata.scope = scope.clone();
            record.metadata.site = site.clone();
            record.metadata.label = label.to_string();
            record.metadata.username = username.clone();
            record.metadata.kind = kind.to_string();
            record.metadata.metadata_json = metadata_json.to_string();
            record.metadata.updated_at = chrono::Utc::now().to_rfc3339();
            if let Some(secret) = replacement_secret {
                record.secret.zeroize();
                record.secret.push_str(secret);
            }
            let metadata = record.metadata.clone();
            store.records[index] = encrypt_record(&*key, &record)?;
            Ok((metadata, serde_json::to_vec_pretty(&store)?))
        })
    }

    /// Physically remove all ciphertext owned by one private lifecycle scope.
    /// This deliberately works while locked: public scope envelopes exist for
    /// exactly this secure-erasure boundary, while all human metadata remains
    /// encrypted inside each record.
    pub fn purge_scope(&self, scope: &CredentialScope) -> Result<usize> {
        scope.validate()?;
        let store_path = self.store_path();
        if !store_path.exists() {
            return Ok(0);
        }
        crate::config::private_io::read_modify_write_private(&store_path, |current| {
            let mut store = parse_encrypted_store(current)?;
            let before = store.records.len();
            store.records.retain(|record| &record.scope != scope);
            let removed = before - store.records.len();
            Ok((removed, serde_json::to_vec_pretty(&store)?))
        })
    }

    fn read_config(&self) -> Result<VaultConfig> {
        let bytes = crate::config::private_io::read_private_file(&self.config_path())?
            .context("credential vault is not initialized")?;
        let config: VaultConfig = serde_json::from_slice(&bytes).context("invalid vault config")?;
        anyhow::ensure!(
            config.version == VAULT_VERSION,
            "unsupported credential vault version {}",
            config.version
        );
        Ok(config)
    }

    fn verify_and_remember(&self, data_key: Zeroizing<[u8; 32]>) -> Result<()> {
        let bytes = crate::config::private_io::read_private_file(&self.store_path())?
            .context("credential vault store is missing")?;
        let store = parse_encrypted_store(Some(&bytes))?;
        for record in &store.records {
            let _ =
                decrypt_record(&*data_key, record).context("vault data cannot be authenticated")?;
        }
        self.ensure_agent_runtime_access(&data_key)?;
        self.remember_key(&*data_key);
        Ok(())
    }

    /// Provision or repair the device-local agent wrapper while the owner has
    /// supplied a valid unlock factor. This is the only migration path for a
    /// pre-feature vault; afterward agent use survives UI locks and restarts.
    fn ensure_agent_runtime_access(&self, data_key: &[u8; 32]) -> Result<()> {
        let existing = self.read_config()?;
        if let Some(wrapped) = existing.agent_runtime_wrapped_key.as_ref() {
            if let Ok(runtime_key) = self.read_agent_runtime_key() {
                if decrypt_key(&*runtime_key, wrapped, AGENT_RUNTIME_WRAP_AAD)
                    .is_ok_and(|candidate| candidate.as_ref() == data_key)
                {
                    return Ok(());
                }
            }
        }

        let runtime_key = random_key();
        let wrapped = encrypt_key(&*runtime_key, data_key, AGENT_RUNTIME_WRAP_AAD)?;
        crate::config::private_io::atomic_write_private(
            &self.agent_runtime_key_path(),
            runtime_key.as_ref(),
        )?;
        let result =
            crate::config::private_io::read_modify_write_private(&self.config_path(), |current| {
                let current = current.context("credential vault is not initialized")?;
                let mut config: VaultConfig =
                    serde_json::from_slice(current).context("invalid vault config")?;
                config.agent_runtime_wrapped_key = Some(wrapped.clone());
                Ok(((), serde_json::to_vec_pretty(&config)?))
            });
        if result.is_err() {
            let _ = crate::config::private_io::remove_private_file(&self.agent_runtime_key_path());
        }
        result
    }

    fn read_agent_runtime_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        let bytes = Zeroizing::new(
            crate::config::private_io::read_private_file(&self.agent_runtime_key_path())?
                .context("agent vault access is not provisioned; unlock the vault once")?,
        );
        anyhow::ensure!(bytes.len() == 32, "agent vault access key is invalid");
        let mut key = [0_u8; 32];
        key.copy_from_slice(bytes.as_slice());
        Ok(Zeroizing::new(key))
    }

    fn require_agent_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        let config = self.read_config()?;
        let wrapped = config
            .agent_runtime_wrapped_key
            .as_ref()
            .context("agent vault access is not provisioned; unlock the vault once")?;
        let runtime_key = self.read_agent_runtime_key()?;
        decrypt_key(&*runtime_key, wrapped, AGENT_RUNTIME_WRAP_AAD)
            .context("agent vault access could not authenticate the local vault")
    }

    fn require_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        self.unlocked_key(true)
            .context("credential vault is locked; ask the user to unlock it")
    }

    /// Return the human-management data key only while it is inside the
    /// configured inactivity window. Agent use has a separate device-local
    /// wrapper and therefore never changes this UI-visible lock state.
    fn unlocked_key(&self, touch: bool) -> Option<Zeroizing<[u8; 32]>> {
        let root = self.root_key();
        let now = Instant::now();
        let timeout = vault_auto_lock_duration();
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        let expired = keys.get(&root).is_some_and(|entry| {
            timeout.is_some_and(|timeout| now.saturating_duration_since(entry.last_used) >= timeout)
        });
        if expired {
            keys.remove(&root);
            return None;
        }
        let entry = keys.get_mut(&root)?;
        if touch {
            entry.last_used = now;
        }
        Some(Zeroizing::new(*entry.key))
    }

    fn remember_key(&self, key: &[u8; 32]) {
        unlocked_keys()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                self.root_key(),
                UnlockedVaultKey {
                    key: Zeroizing::new(*key),
                    last_used: Instant::now(),
                },
            );
    }

    #[cfg(test)]
    fn age_unlocked_key(&self, age: Duration) {
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        let entry = keys
            .get_mut(&self.root_key())
            .expect("vault key must be unlocked before aging it");
        entry.last_used = Instant::now()
            .checked_sub(age)
            .expect("test age must fit in Instant");
    }

    fn root_key(&self) -> PathBuf {
        self.root.clone()
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    fn store_path(&self) -> PathBuf {
        self.root.join("credentials.enc")
    }

    fn agent_runtime_key_path(&self) -> PathBuf {
        self.root.join("agent-runtime.key")
    }
}

fn unlocked_keys() -> &'static Mutex<HashMap<PathBuf, UnlockedVaultKey>> {
    UNLOCKED_KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn vault_auto_lock_duration() -> Option<Duration> {
    let minutes = crate::settings::effective_u64(
        "security.vault_auto_lock_minutes",
        &crate::settings::SettingsScope::Global,
    )
    .unwrap_or(30);
    (minutes > 0).then(|| Duration::from_secs(minutes.saturating_mul(60)))
}

fn validate_master_password(password: &str) -> Result<()> {
    anyhow::ensure!(
        password.chars().count() >= 12,
        "master password must contain at least 12 characters"
    );
    anyhow::ensure!(password.len() <= 1024, "master password is too long");
    Ok(())
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{label} cannot be empty");
    anyhow::ensure!(value.len() <= 128, "{label} is too long");
    anyhow::ensure!(
        value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "{label} contains unsupported characters"
    );
    Ok(())
}

fn validate_text(value: &str, max: usize, label: &str) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{label} cannot be empty");
    anyhow::ensure!(value.len() <= max, "{label} exceeds {max} bytes");
    Ok(())
}

fn validate_metadata(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() <= MAX_METADATA_BYTES,
        "credential metadata exceeds {MAX_METADATA_BYTES} bytes"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(value).context("metadata_json is invalid")?;
    anyhow::ensure!(parsed.is_object(), "metadata_json must be a JSON object");
    Ok(())
}

fn normalize_site(site: &str) -> Result<String> {
    let site = site.trim();
    anyhow::ensure!(!site.is_empty(), "credential site cannot be empty");
    anyhow::ensure!(site.len() <= MAX_SITE_BYTES, "credential site is too long");
    let candidate = if site.contains("://") {
        url::Url::parse(site).context("credential site is not a valid URL")?
    } else {
        url::Url::parse(&format!("https://{site}"))
            .context("credential site is not a valid domain")?
    };
    anyhow::ensure!(
        candidate.username().is_empty() && candidate.password().is_none(),
        "credential site URL must not contain credentials"
    );
    candidate
        .host_str()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .context("credential site has no host")
}

fn kdf_params() -> Result<Params> {
    #[cfg(test)]
    let memory_kib = 8 * 1024;
    #[cfg(not(test))]
    let memory_kib = 64 * 1024;
    Params::new(memory_kib, 3, 1, Some(KDF_OUTPUT_BYTES))
        .map_err(|error| anyhow::anyhow!("invalid vault KDF parameters: {error}"))
}

fn derive_password_key(password: &str, salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, kdf_params()?)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|error| anyhow::anyhow!("could not derive vault key: {error}"))?;
    Ok(key)
}

fn random_key() -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(key.as_mut());
    key
}

fn encrypt_key(key: &[u8; 32], data_key: &[u8; 32], aad: &[u8]) -> Result<WrappedKey> {
    let (nonce, ciphertext) = encrypt_bytes(key, data_key, aad)?;
    Ok(WrappedKey { nonce, ciphertext })
}

fn decrypt_key(key: &[u8; 32], wrapped: &WrappedKey, aad: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut plain = Zeroizing::new(decrypt_bytes(
        key,
        &wrapped.nonce,
        &wrapped.ciphertext,
        aad,
    )?);
    anyhow::ensure!(plain.len() == 32, "wrapped vault key has the wrong size");
    let mut output = Zeroizing::new([0u8; 32]);
    output.copy_from_slice(&plain);
    plain.zeroize();
    Ok(output)
}

fn parse_encrypted_store(bytes: Option<&[u8]>) -> Result<EncryptedStore> {
    let bytes = bytes.context("credential vault store is missing")?;
    let encrypted: EncryptedStore = serde_json::from_slice(bytes).context("invalid vault store")?;
    anyhow::ensure!(
        encrypted.version == VAULT_VERSION,
        "unsupported credential store version {}",
        encrypted.version
    );
    anyhow::ensure!(
        encrypted.records.len() <= MAX_RECORDS,
        "credential vault is oversized"
    );
    for record in &encrypted.records {
        validate_id(&record.credential_id, "credential id")?;
        record.scope.validate()?;
        let _ = decode_exact::<12>(&record.nonce, "credential nonce")?;
        anyhow::ensure!(
            record.ciphertext.len() <= (MAX_SECRET_BYTES + MAX_METADATA_BYTES) * 2,
            "encrypted credential is oversized"
        );
    }
    Ok(encrypted)
}

fn record_aad(credential_id: &str, scope: &CredentialScope) -> Result<Vec<u8>> {
    Ok(format!(
        "phoenix-vault-record-v2:{credential_id}:{}",
        serde_json::to_string(scope)?
    )
    .into_bytes())
}

fn encrypt_record(key: &[u8; 32], record: &SecretRecord) -> Result<EncryptedRecord> {
    let aad = record_aad(&record.metadata.credential_id, &record.metadata.scope)?;
    let plain = Zeroizing::new(serde_json::to_vec(record)?);
    let (nonce, ciphertext) = encrypt_bytes(key, plain.as_ref(), &aad)?;
    Ok(EncryptedRecord {
        credential_id: record.metadata.credential_id.clone(),
        scope: record.metadata.scope.clone(),
        nonce,
        ciphertext,
    })
}

fn decrypt_record(key: &[u8; 32], encrypted: &EncryptedRecord) -> Result<SecretRecord> {
    let aad = record_aad(&encrypted.credential_id, &encrypted.scope)?;
    let plain = Zeroizing::new(decrypt_bytes(
        key,
        &encrypted.nonce,
        &encrypted.ciphertext,
        &aad,
    )?);
    let record: SecretRecord =
        serde_json::from_slice(plain.as_ref()).context("decrypted credential is invalid")?;
    anyhow::ensure!(
        record.metadata.credential_id == encrypted.credential_id
            && record.metadata.scope == encrypted.scope,
        "credential envelope does not match encrypted payload"
    );
    Ok(record)
}

fn encrypt_bytes(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Result<(String, String)> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key has fixed size");
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| anyhow::anyhow!("vault encryption failed"))?;
    Ok((
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext),
    ))
}

fn decrypt_bytes(key: &[u8; 32], nonce: &str, ciphertext: &str, aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key has fixed size");
    let nonce = decode_exact::<12>(nonce, "vault nonce")?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(ciphertext)
        .context("vault ciphertext is not valid base64")?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("vault authentication failed"))
}

fn decode_exact<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .with_context(|| format!("{label} is not valid base64"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} has the wrong size"))
}

fn parse_recovery_key(recovery_key: &str) -> Result<Zeroizing<[u8; 32]>> {
    let encoded = recovery_key
        .trim()
        .strip_prefix(RECOVERY_PREFIX)
        .context("recovery key has an invalid prefix")?;
    Ok(Zeroizing::new(decode_exact::<32>(encoded, "recovery key")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (
        tempfile::TempDir,
        crate::config::test_env::PhoenixHomeGuard,
        Vault,
    ) {
        let root = tempfile::tempdir().unwrap();
        let guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let vault = Vault::at(root.path());
        (root, guard, vault)
    }

    #[test]
    fn master_password_and_recovery_key_unlock_the_same_store() {
        let (_root, _guard, vault) = vault();
        let recovery = vault.initialize("correct horse battery staple").unwrap();
        let created = vault
            .put(
                CredentialScope::agent("finance"),
                "https://www.example.com/login?ignored=true",
                "Example billing",
                Some("felix@example.com".into()),
                "password",
                r#"{"account":"billing"}"#,
                "never-plaintext-on-disk",
            )
            .unwrap();
        vault.lock();
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.unlock_with_password("wrong password value").is_err());
        vault
            .unlock_with_password("correct horse battery staple")
            .unwrap();
        let revealed = vault
            .reveal(&created.credential_id, &[CredentialScope::agent("finance")])
            .unwrap();
        assert_eq!(revealed.secret(), "never-plaintext-on-disk");
        vault.lock();
        vault.unlock_with_recovery_key(&recovery).unwrap();
        assert_eq!(
            vault
                .list(&[CredentialScope::agent("finance")])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn scopes_are_enforced_and_serialized_store_never_contains_the_secret() {
        let (root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        let created = vault
            .put(
                CredentialScope::group("operations"),
                "github.com",
                "Deploy bot",
                None,
                "token",
                "{}",
                "ghp_super_secret_value",
            )
            .unwrap();
        assert!(vault
            .reveal(&created.credential_id, &[CredentialScope::agent("coder")])
            .is_err());
        let disk = std::fs::read(root.path().join("vault/credentials.enc")).unwrap();
        assert!(!String::from_utf8_lossy(&disk).contains("ghp_super_secret_value"));
        assert!(vault
            .delete(&created.credential_id, &[CredentialScope::agent("coder")])
            .is_ok_and(|removed| !removed));
        assert!(vault
            .delete(
                &created.credential_id,
                &[CredentialScope::group("operations")]
            )
            .unwrap());
    }

    #[test]
    fn init_is_first_write_wins_and_rejects_short_passwords() {
        let (_root, _guard, vault) = vault();
        assert!(vault.initialize("short").is_err());
        vault.initialize("this one is long enough").unwrap();
        assert!(vault.initialize("a different long password").is_err());
    }

    #[test]
    fn configured_inactivity_locks_human_management_but_not_agent_use() {
        let (_root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        assert_eq!(vault.status(), VaultStatus::Unlocked);

        let created = vault
            .put(
                CredentialScope::Company,
                "example.com",
                "Shared login",
                None,
                "password",
                "{}",
                "agent-can-use-this",
            )
            .unwrap();
        // The default policy is 30 minutes. Age the human key beyond it.
        vault.age_unlocked_key(Duration::from_secs(31 * 60));
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.list(&[CredentialScope::Company]).is_err());
        assert_eq!(
            vault
                .list_for_agent(&[CredentialScope::Company])
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            vault
                .reveal_for_agent(&created.credential_id, &[CredentialScope::Company])
                .unwrap()
                .secret(),
            "agent-can-use-this"
        );
        assert_eq!(vault.status(), VaultStatus::Locked);
    }

    #[test]
    fn manual_lock_still_allows_agent_generated_credentials_without_unlocking_ui() {
        let (_root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        vault.lock();
        let created = vault
            .put_for_agent(
                CredentialScope::agent("phoenix"),
                "new.example",
                "Generated account",
                None,
                "password",
                "{}",
                "generated-secret",
            )
            .unwrap();
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.list(&[CredentialScope::agent("phoenix")]).is_err());
        assert_eq!(
            vault
                .reveal_for_agent(&created.credential_id, &[CredentialScope::agent("phoenix")],)
                .unwrap()
                .secret(),
            "generated-secret"
        );
    }

    #[test]
    fn owner_unlock_migrates_an_existing_vault_to_persistent_agent_access() {
        let (root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        let created = vault
            .put(
                CredentialScope::Company,
                "legacy.example",
                "Legacy login",
                None,
                "password",
                "{}",
                "legacy-secret",
            )
            .unwrap();

        let config_path = root.path().join("vault/config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config
            .as_object_mut()
            .unwrap()
            .remove("agent_runtime_wrapped_key");
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        std::fs::remove_file(root.path().join("vault/agent-runtime.key")).unwrap();
        vault.lock();
        assert!(vault.list_for_agent(&[CredentialScope::Company]).is_err());

        vault
            .unlock_with_password("a sufficiently long master password")
            .unwrap();
        vault.lock();
        assert_eq!(
            vault
                .reveal_for_agent(&created.credential_id, &[CredentialScope::Company])
                .unwrap()
                .secret(),
            "legacy-secret"
        );
        assert_eq!(vault.status(), VaultStatus::Locked);
    }

    #[test]
    fn password_change_preserves_recovery_and_recovery_rotation_revokes_old_key() {
        let (_root, _guard, vault) = vault();
        let old_recovery = vault.initialize("the original master password").unwrap();
        vault
            .change_master_password(
                "the original master password",
                "the replacement master password",
            )
            .unwrap();
        vault.lock();
        assert!(vault
            .unlock_with_password("the original master password")
            .is_err());
        vault
            .unlock_with_password("the replacement master password")
            .unwrap();
        let new_recovery = vault.rotate_recovery_key().unwrap();
        vault.lock();
        assert!(vault.unlock_with_recovery_key(&old_recovery).is_err());
        vault.unlock_with_recovery_key(&new_recovery).unwrap();
    }

    #[test]
    fn locked_scope_purge_removes_only_owned_ciphertext() {
        let (root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        let private = vault
            .put(
                CredentialScope::agent("finance"),
                "bank.example",
                "Private finance login",
                None,
                "password",
                "{}",
                "private-secret",
            )
            .unwrap();
        let company = vault
            .put(
                CredentialScope::Company,
                "shared.example",
                "Shared company login",
                None,
                "password",
                "{}",
                "company-secret",
            )
            .unwrap();

        vault.lock();
        assert_eq!(
            vault
                .purge_scope(&CredentialScope::agent("finance"))
                .unwrap(),
            1
        );
        vault
            .unlock_with_password("a sufficiently long master password")
            .unwrap();
        assert!(vault
            .reveal(&private.credential_id, &[CredentialScope::agent("finance")])
            .is_err());
        assert_eq!(
            vault
                .reveal(&company.credential_id, &[CredentialScope::Company])
                .unwrap()
                .secret(),
            "company-secret"
        );

        let disk = std::fs::read_to_string(root.path().join("vault/credentials.enc")).unwrap();
        assert!(!disk.contains(&private.credential_id));
        assert!(disk.contains(&company.credential_id));
    }

    #[test]
    fn metadata_update_preserves_secret_unless_explicitly_replaced() {
        let (_root, _guard, vault) = vault();
        vault
            .initialize("a sufficiently long master password")
            .unwrap();
        let created = vault
            .put(
                CredentialScope::agent("phoenix"),
                "example.com",
                "Old label",
                None,
                "password",
                "{}",
                "original-secret",
            )
            .unwrap();
        let updated = vault
            .update(
                &created.credential_id,
                &[CredentialScope::agent("phoenix")],
                CredentialScope::Company,
                "www.example.com/login",
                "New label",
                Some("owner@example.com".into()),
                "password",
                r#"{"note":"shared"}"#,
                None,
            )
            .unwrap();
        assert_eq!(updated.credential_id, created.credential_id);
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.scope, CredentialScope::Company);
        assert_eq!(updated.label, "New label");
        assert_eq!(
            vault
                .reveal(&created.credential_id, &[CredentialScope::Company])
                .unwrap()
                .secret(),
            "original-secret"
        );
        vault
            .update(
                &created.credential_id,
                &[CredentialScope::Company],
                CredentialScope::Company,
                "example.com",
                "New label",
                Some("owner@example.com".into()),
                "password",
                "{}",
                Some("replacement-secret"),
            )
            .unwrap();
        assert_eq!(
            vault
                .reveal(&created.credential_id, &[CredentialScope::Company])
                .unwrap()
                .secret(),
            "replacement-secret"
        );
    }
}
