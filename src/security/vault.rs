//! Passes: Phoenix's locally encrypted store for logins, cards, API keys,
//! tokens, verification codes, and other secrets.
//!
//! # Design (store version 3)
//!
//! * One X25519 keypair per Phoenix home. The **public key** is stored in
//!   plaintext, so *saving* a pass (by the user, by an agent's request popup, or
//!   by `credential_generate`) never needs the master password.
//! * Every pass's secret material is **sealed to the public key**: an ephemeral
//!   X25519 key agreement, HKDF-SHA256, and XChaCha20-Poly1305. The record's id,
//!   scope, site, and kind are bound as associated data, so moving a sealed
//!   secret to another record or re-pointing it to a different site fails
//!   authentication.
//! * The **private key** is encrypted under a random 32-byte unlock key `K`.
//!   `K` is wrapped twice: by a key derived from the master password with
//!   Argon2id (salt and cost parameters stored), and by a high-entropy recovery
//!   key shown once. Changing the password re-wraps only `K`.
//! * Viewing or using a secret requires `K` in memory. It is unlocked once per
//!   gateway process lifetime and wiped on drop (or explicit lock / full quit).
//! * Before a master password exists ("unprotected" setup), `K` lives in a
//!   private `setup.key` file so nothing a user saves is ever lost. Setting the
//!   master password wraps `K` and deletes that file.
//! * Pass metadata (title, site, username, card brand/last 4, timestamps) is
//!   intentionally plaintext so agents and the Passes list work while locked.
//!   Secret values never are.
//!
//! Version-2 vaults (AES-GCM records under a password-wrapped data key, plus a
//! device-local "agent runtime" key on disk) migrate automatically: the old
//! data key becomes `K`, so the existing password and recovery key keep
//! working, every record is re-sealed, the old files are kept as encrypted
//! backups, and the plaintext agent-runtime key is deleted.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const STORE_VERSION: u32 = 3;
const LEGACY_VERSION: u32 = 2;
const RECOVERY_PREFIX: &str = "PHX1-";
const MAX_RECORDS: usize = 16_384;
const MAX_SECRET_BYTES: usize = 1024 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_SITE_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 512;
const MAX_FIELDS: usize = 32;
const KDF_OUTPUT_BYTES: usize = 32;
const SEAL_ALGORITHM: &str = "x25519-hkdf-sha256-xchacha20poly1305";
const PASSWORD_WRAP_AAD: &[u8] = b"phoenix-vault-password-wrap-v1";
const RECOVERY_WRAP_AAD: &[u8] = b"phoenix-vault-recovery-wrap-v1";
const LEGACY_AGENT_RUNTIME_WRAP_AAD: &[u8] = b"phoenix-vault-agent-runtime-wrap-v1";

/// The unlock key `K`, held only in gateway memory. Zeroized on drop.
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

/// Non-secret description of a pass. Safe for agents, logs of ids, and the
/// locked Passes list. `metadata_json` carries public descriptors such as a
/// card's `brand`/`last4`, an API key's `service`/`base_url`, or which secret
/// `fields` the pass holds — never a secret value.
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

/// Sealed plaintext: the primary secret plus named secondary secret fields
/// (card CVC/expiry/name/zip, a login's TOTP seed, ...).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SecretPayload {
    secret: String,
    #[serde(default)]
    fields: BTreeMap<String, String>,
}

impl Drop for SecretPayload {
    fn drop(&mut self) {
        self.secret.zeroize();
        for value in self.fields.values_mut() {
            value.zeroize();
        }
    }
}

/// A decrypted credential returned to the one caller that requested it. The
/// secret buffers are wiped on drop and intentionally omitted from Debug.
pub struct RevealedCredential {
    pub metadata: CredentialMetadata,
    secret: Zeroizing<String>,
    fields: BTreeMap<String, Zeroizing<String>>,
}

impl std::fmt::Debug for RevealedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevealedCredential")
            .field("metadata", &self.metadata)
            .field("secret", &"[REDACTED]")
            .field("fields", &self.fields.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RevealedCredential {
    /// The primary secret: a login's password, a card's number, an API key.
    pub fn secret(&self) -> &str {
        self.secret.as_str()
    }

    /// A named secondary secret field (`cvc`, `expiry`, `totp`, ...). The
    /// primary secret is also reachable under its kind's primary field name.
    pub fn field(&self, name: &str) -> Option<&str> {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || name == "secret" || name == primary_field(&self.metadata.kind) {
            return Some(self.secret());
        }
        self.fields.get(&name).map(|value| value.as_str())
    }

    pub fn field_names(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }

    /// Every secret value (primary first) for output scrubbing.
    pub fn secret_values(&self) -> Vec<&str> {
        std::iter::once(self.secret())
            .chain(self.fields.values().map(|value| value.as_str()))
            .filter(|value| !value.is_empty())
            .collect()
    }

    pub fn fields_map(&self) -> BTreeMap<String, String> {
        self.fields
            .iter()
            .map(|(key, value)| (key.clone(), value.to_string()))
            .collect()
    }
}

/// Site recorded for passes that belong to no single website (a payment
/// card, a one-time code, a free-form secret). It is bound into the sealed
/// box like any site, so it cannot be forged onto a login.
pub const UNBOUND_SITE: &str = "phoenix.local";

/// Kinds that may be filled on any site when saved with [`UNBOUND_SITE`].
/// Logins, API keys, and tokens are always bound to their own site.
pub fn fillable_anywhere(metadata: &CredentialMetadata) -> bool {
    metadata.site == UNBOUND_SITE && matches!(metadata.kind.as_str(), "card" | "verification_code" | "secret")
}

/// The primary secret field name of a pass kind.
pub fn primary_field(kind: &str) -> &'static str {
    match kind {
        "password" | "login" => "password",
        "card" => "number",
        "api_key" => "key",
        "verification_code" => "code",
        "recovery_code" => "code",
        _ => "value",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WrappedKey {
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KdfConfig {
    algorithm: String,
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

/// Current (v3) config. `password_wrapped_key == None` means the user has not
/// set a master password yet; `K` then lives in `setup.key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PassesConfig {
    version: u32,
    public_key: String,
    private_key_wrapped: WrappedKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password_salt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password_wrapped_key: Option<WrappedKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_wrapped_key: Option<WrappedKey>,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migrated_from_version: Option<u32>,
}

impl PassesConfig {
    fn protected(&self) -> bool {
        self.password_wrapped_key.is_some()
    }
}

/// Legacy (v2) config, read only for migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyConfig {
    version: u32,
    password_salt: String,
    password_wrapped_key: WrappedKey,
    recovery_wrapped_key: WrappedKey,
    #[serde(default)]
    agent_runtime_wrapped_key: Option<WrappedKey>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyStore {
    version: u32,
    records: Vec<LegacyRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyRecord {
    credential_id: String,
    scope: CredentialScope,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Zeroize)]
#[zeroize(drop)]
struct LegacySecretRecord {
    #[zeroize(skip)]
    metadata: CredentialMetadata,
    secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedBox {
    alg: String,
    ephemeral_public: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PassRecord {
    metadata: CredentialMetadata,
    sealed: SealedBox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PassStore {
    version: u32,
    records: Vec<PassRecord>,
}

impl Default for PassStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    /// Nothing saved yet and no master password.
    Uninitialized,
    /// Passes exist and are sealed, but no master password protects the
    /// private key yet. Usable without unlocking; the UI asks to set one.
    Unprotected,
    Locked,
    Unlocked,
}

impl VaultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Unprotected => "unprotected",
            Self::Locked => "locked",
            Self::Unlocked => "unlocked",
        }
    }

    /// Secrets can be decrypted right now without asking the user.
    pub fn can_reveal(self) -> bool {
        matches!(self, Self::Unprotected | Self::Unlocked)
    }
}

enum LoadedConfig {
    Missing,
    /// A v2 vault that could not be migrated without the master password.
    Legacy(LegacyConfig),
    Current(PassesConfig),
}

/// Input for a new pass. Borrowed secrets stay owned (and wiped) by the
/// caller; the sealed payload copy is zeroized on drop.
#[derive(Default)]
pub struct NewPass<'a> {
    pub scope: Option<CredentialScope>,
    pub site: &'a str,
    pub label: &'a str,
    pub username: Option<String>,
    pub kind: &'a str,
    pub metadata_json: &'a str,
    pub secret: &'a str,
    pub fields: BTreeMap<String, String>,
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
        match self.load_config() {
            Ok(LoadedConfig::Missing) => VaultStatus::Uninitialized,
            Ok(LoadedConfig::Legacy(_)) => VaultStatus::Locked,
            Ok(LoadedConfig::Current(config)) if !config.protected() => VaultStatus::Unprotected,
            Ok(LoadedConfig::Current(_)) => {
                if self.unlocked_key(false).is_some() {
                    VaultStatus::Unlocked
                } else {
                    VaultStatus::Locked
                }
            }
            // An unreadable config must never be presented as empty.
            Err(_) => VaultStatus::Locked,
        }
    }

    /// True when a master password exists (the vault can be locked).
    pub fn has_master_password(&self) -> bool {
        matches!(
            self.load_config(),
            Ok(LoadedConfig::Legacy(_)) | Ok(LoadedConfig::Current(PassesConfig { password_wrapped_key: Some(_), .. }))
        )
    }

    /// Set the master password and return the one-time recovery key. Works on
    /// a brand-new home and on an unprotected one (passes saved before a
    /// password existed are kept: their unlock key is wrapped, never rotated).
    pub fn initialize(&self, master_password: &str) -> Result<Zeroizing<String>> {
        validate_master_password(master_password)?;
        crate::config::private_io::with_private_lock(&self.root.join("initialize"), || {
            let config = match self.load_config()? {
                LoadedConfig::Missing => {
                    self.create_unprotected_locked()?;
                    self.read_current_config()?
                }
                LoadedConfig::Current(config) if !config.protected() => config,
                _ => anyhow::bail!("credential vault is already initialized"),
            };
            let unlock_key = self.read_setup_key()?;
            // Prove the setup key really unwraps this keypair before wrapping.
            open_private_key(&unlock_key, &config)?;
            let mut salt = [0u8; 16];
            OsRng.fill_bytes(&mut salt);
            let kdf = default_kdf();
            let password_key = derive_password_key(master_password, &salt, &kdf)?;
            let recovery_key_bytes = random_key();
            let recovery_text = Zeroizing::new(format!(
                "{RECOVERY_PREFIX}{}",
                URL_SAFE_NO_PAD.encode(recovery_key_bytes.as_ref())
            ));
            let mut protected = config.clone();
            protected.password_salt = Some(URL_SAFE_NO_PAD.encode(salt));
            protected.kdf = Some(kdf);
            protected.password_wrapped_key =
                Some(encrypt_key(&password_key, &unlock_key, PASSWORD_WRAP_AAD)?);
            protected.recovery_wrapped_key = Some(encrypt_key(
                &recovery_key_bytes,
                &unlock_key,
                RECOVERY_WRAP_AAD,
            )?);
            crate::config::private_io::atomic_write_private(
                &self.config_path(),
                &serde_json::to_vec_pretty(&protected)?,
            )?;
            // The config is the commit point; only then drop the setup key.
            crate::config::private_io::remove_private_file(&self.setup_key_path())?;
            self.remember_key(&unlock_key);
            Ok(recovery_text)
        })
    }

    pub fn unlock_with_password(&self, master_password: &str) -> Result<()> {
        match self.load_config()? {
            LoadedConfig::Missing => anyhow::bail!("credential vault is not initialized"),
            LoadedConfig::Legacy(legacy) => {
                let salt = decode_exact::<16>(&legacy.password_salt, "vault password salt")?;
                let password_key = derive_password_key(master_password, &salt, &legacy_kdf())?;
                let key = decrypt_key(&password_key, &legacy.password_wrapped_key, PASSWORD_WRAP_AAD)
                    .context("master password is incorrect or vault metadata was changed")?;
                self.migrate_legacy(&legacy, &key)?;
                self.remember_key(&key);
                Ok(())
            }
            LoadedConfig::Current(config) => {
                let wrapped = config
                    .password_wrapped_key
                    .as_ref()
                    .context("no master password is set yet; create one first")?;
                let salt = decode_exact::<16>(
                    config.password_salt.as_deref().context("vault password salt is missing")?,
                    "vault password salt",
                )?;
                let kdf = config.kdf.clone().unwrap_or_else(legacy_kdf);
                let password_key = derive_password_key(master_password, &salt, &kdf)?;
                let key = decrypt_key(&password_key, wrapped, PASSWORD_WRAP_AAD)
                    .context("master password is incorrect or vault metadata was changed")?;
                open_private_key(&key, &config)
                    .context("vault data cannot be authenticated")?;
                self.remember_key(&key);
                Ok(())
            }
        }
    }

    pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<()> {
        let recovery = parse_recovery_key(recovery_key)?;
        match self.load_config()? {
            LoadedConfig::Missing => anyhow::bail!("credential vault is not initialized"),
            LoadedConfig::Legacy(legacy) => {
                let key = decrypt_key(&recovery, &legacy.recovery_wrapped_key, RECOVERY_WRAP_AAD)
                    .context("recovery key is incorrect or vault metadata was changed")?;
                self.migrate_legacy(&legacy, &key)?;
                self.remember_key(&key);
                Ok(())
            }
            LoadedConfig::Current(config) => {
                let wrapped = config
                    .recovery_wrapped_key
                    .as_ref()
                    .context("no recovery key exists yet; create a master password first")?;
                let key = decrypt_key(&recovery, wrapped, RECOVERY_WRAP_AAD)
                    .context("recovery key is incorrect or vault metadata was changed")?;
                open_private_key(&key, &config)
                    .context("vault data cannot be authenticated")?;
                self.remember_key(&key);
                Ok(())
            }
        }
    }

    /// Drop the in-memory unlock key. Saving keeps working while locked.
    pub fn lock(&self) {
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        keys.remove(&self.root);
    }

    /// Drop every unlocked key in this process (full quit / shutdown).
    pub fn lock_all() {
        unlocked_keys()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Re-wrap the unchanged unlock key under a new master password. The
    /// recovery key remains valid, and pass ciphertext is untouched.
    pub fn change_master_password(&self, current_password: &str, new_password: &str) -> Result<()> {
        validate_master_password(new_password)?;
        // Upgrades a legacy vault first, and proves the current password.
        self.unlock_with_password(current_password)
            .context("current master password is incorrect")?;
        let key = self.require_key()?;
        crate::config::private_io::read_modify_write_private(&self.config_path(), |current| {
            let current = current.context("credential vault is not initialized")?;
            let mut config: PassesConfig =
                serde_json::from_slice(current).context("invalid vault config")?;
            anyhow::ensure!(config.version == STORE_VERSION, "unsupported vault version");
            let mut salt = [0u8; 16];
            OsRng.fill_bytes(&mut salt);
            let kdf = default_kdf();
            let password_key = derive_password_key(new_password, &salt, &kdf)?;
            config.password_salt = Some(URL_SAFE_NO_PAD.encode(salt));
            config.kdf = Some(kdf);
            config.password_wrapped_key = Some(encrypt_key(&password_key, &key, PASSWORD_WRAP_AAD)?);
            Ok(((), serde_json::to_vec_pretty(&config)?))
        })
    }

    /// Invalidate the previous recovery key and return a one-time replacement.
    pub fn rotate_recovery_key(&self) -> Result<Zeroizing<String>> {
        let key = self.require_key()?;
        let recovery_key = random_key();
        let recovery_text = Zeroizing::new(format!(
            "{RECOVERY_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(recovery_key.as_ref())
        ));
        crate::config::private_io::read_modify_write_private(&self.config_path(), |current| {
            let current = current.context("credential vault is not initialized")?;
            let mut config: PassesConfig =
                serde_json::from_slice(current).context("invalid vault config")?;
            anyhow::ensure!(config.protected(), "create a master password first");
            config.recovery_wrapped_key =
                Some(encrypt_key(&recovery_key, &key, RECOVERY_WRAP_AAD)?);
            Ok(((), serde_json::to_vec_pretty(&config)?))
        })?;
        Ok(recovery_text)
    }

    /// Save a single-secret pass. Never needs the master password.
    #[allow(clippy::too_many_arguments)]
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
        self.put_pass(NewPass {
            scope: Some(scope),
            site,
            label,
            username,
            kind,
            metadata_json,
            secret,
            fields: BTreeMap::new(),
        })
    }

    /// Kept for callers that store agent-generated secrets. Identical to
    /// [`Vault::put`]: saving only needs the public key.
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
        self.put(scope, site, label, username, kind, metadata_json, secret)
    }

    /// Save a pass with secondary secret fields (card CVC, login TOTP, ...).
    pub fn put_pass(&self, pass: NewPass<'_>) -> Result<CredentialMetadata> {
        let scope = pass.scope.clone().context("pass scope is required")?;
        scope.validate()?;
        let site = normalize_site(pass.site)?;
        validate_text(pass.label, MAX_LABEL_BYTES, "pass title")?;
        validate_kind(pass.kind)?;
        validate_secret(pass.secret)?;
        validate_fields(&pass.fields)?;
        let username = normalize_username(pass.username.clone())?;
        let metadata_json = public_descriptors(pass.kind, pass.metadata_json, pass.secret, &pass.fields)?;
        let config = self.ensure_writable_config()?;
        let public = decode_public_key(&config)?;
        let now = chrono::Utc::now().to_rfc3339();
        let metadata = CredentialMetadata {
            credential_id: uuid::Uuid::new_v4().to_string(),
            scope,
            site,
            label: pass.label.trim().to_string(),
            username,
            kind: pass.kind.to_string(),
            metadata_json,
            created_at: now.clone(),
            updated_at: now,
        };
        let payload = SecretPayload {
            secret: pass.secret.to_string(),
            fields: pass.fields.clone(),
        };
        let sealed = seal(&public, &payload, &record_aad(&metadata)?)?;
        crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
            let mut store = parse_store(current)?;
            anyhow::ensure!(store.records.len() < MAX_RECORDS, "Passes is full");
            store.records.push(PassRecord {
                metadata: metadata.clone(),
                sealed,
            });
            Ok((metadata.clone(), serde_json::to_vec_pretty(&store)?))
        })
    }

    /// Metadata only. Works while locked.
    pub fn list(&self, visible_scopes: &[CredentialScope]) -> Result<Vec<CredentialMetadata>> {
        for scope in visible_scopes {
            scope.validate()?;
        }
        self.ensure_current_if_possible()?;
        let store = self.read_store()?;
        Ok(store
            .records
            .into_iter()
            .filter(|record| visible_scopes.contains(&record.metadata.scope))
            .map(|record| record.metadata)
            .collect())
    }

    pub fn list_for_agent(&self, visible_scopes: &[CredentialScope]) -> Result<Vec<CredentialMetadata>> {
        self.list(visible_scopes)
    }

    /// Decrypt one pass. Requires the vault to be unlocked (or unprotected).
    pub fn reveal(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<RevealedCredential> {
        validate_id(credential_id, "credential id")?;
        let secret_key = self.private_key()?;
        let store = self.read_store()?;
        let record = store
            .records
            .iter()
            .find(|record| {
                record.metadata.credential_id == credential_id
                    && visible_scopes.contains(&record.metadata.scope)
            })
            .context("credential does not exist in the caller's visible scopes")?;
        let mut payload = open(&secret_key, &record.sealed, &record_aad(&record.metadata)?)?;
        Ok(RevealedCredential {
            metadata: record.metadata.clone(),
            secret: Zeroizing::new(std::mem::take(&mut payload.secret)),
            fields: std::mem::take(&mut payload.fields)
                .into_iter()
                .map(|(key, value)| (key, Zeroizing::new(value)))
                .collect(),
        })
    }

    /// Native agent action (browser fill, HTTP header). Same unlock rule as a
    /// human reveal: the secret never enters model context either way.
    pub fn reveal_for_agent(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<RevealedCredential> {
        self.reveal(credential_id, visible_scopes)
    }

    pub fn delete(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<bool> {
        validate_id(credential_id, "credential id")?;
        self.ensure_current_if_possible()?;
        if !self.store_path().exists() {
            return Ok(false);
        }
        crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
            let mut store = parse_store(current)?;
            let before = store.records.len();
            store.records.retain(|record| {
                record.metadata.credential_id != credential_id
                    || !visible_scopes.contains(&record.metadata.scope)
            });
            let removed = store.records.len() != before;
            Ok((removed, serde_json::to_vec_pretty(&store)?))
        })
    }

    pub(crate) fn delete_for_agent(&self, credential_id: &str, visible_scopes: &[CredentialScope]) -> Result<bool> {
        self.delete(credential_id, visible_scopes)
    }

    /// Replace only the primary secret, keeping secondary fields. Needs no
    /// unlock when the pass has no secondary fields.
    pub(crate) fn replace_secret_for_agent(&self, credential_id: &str, visible_scopes: &[CredentialScope], secret: &str) -> Result<()> {
        validate_id(credential_id, "credential id")?;
        validate_secret(secret)?;
        let config = self.ensure_writable_config()?;
        let public = decode_public_key(&config)?;
        let secret_key = self.private_key().ok();
        crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
            let mut store = parse_store(current)?;
            let record = store
                .records
                .iter_mut()
                .find(|record| {
                    record.metadata.credential_id == credential_id
                        && visible_scopes.contains(&record.metadata.scope)
                })
                .context("credential does not exist in the caller's visible scopes")?;
            let fields = if field_names(&record.metadata).is_empty() {
                BTreeMap::new()
            } else {
                let key = secret_key
                    .as_ref()
                    .context("Passes is locked; unlock once to update this pass")?;
                let mut old = open(key, &record.sealed, &record_aad(&record.metadata)?)?;
                std::mem::take(&mut old.fields)
            };
            record.metadata.updated_at = chrono::Utc::now().to_rfc3339();
            let payload = SecretPayload {
                secret: secret.to_string(),
                fields,
            };
            record.sealed = seal(&public, &payload, &record_aad(&record.metadata)?)?;
            Ok(((), serde_json::to_vec_pretty(&store)?))
        })
    }

    /// Update metadata, and optionally replace the whole secret payload.
    /// Changing scope/site/kind without a replacement secret re-seals the
    /// existing secret, which requires the vault to be unlocked.
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
        self.update_pass(
            credential_id,
            visible_scopes,
            NewPass {
                scope: Some(scope),
                site,
                label,
                username,
                kind,
                metadata_json,
                secret: replacement_secret.unwrap_or(""),
                fields: BTreeMap::new(),
            },
            replacement_secret.is_some(),
        )
    }

    /// Update a pass. With `replace_secret`, `pass.secret`/`pass.fields` become
    /// the complete new secret payload (sealed; no unlock needed).
    pub fn update_pass(
        &self,
        credential_id: &str,
        visible_scopes: &[CredentialScope],
        pass: NewPass<'_>,
        replace_secret: bool,
    ) -> Result<CredentialMetadata> {
        validate_id(credential_id, "credential id")?;
        let scope = pass.scope.clone().context("pass scope is required")?;
        scope.validate()?;
        let site = normalize_site(pass.site)?;
        validate_text(pass.label, MAX_LABEL_BYTES, "pass title")?;
        validate_kind(pass.kind)?;
        let username = normalize_username(pass.username.clone())?;
        if replace_secret {
            validate_secret(pass.secret)?;
            validate_fields(&pass.fields)?;
        }
        let config = self.ensure_writable_config()?;
        let public = decode_public_key(&config)?;
        let secret_key = self.private_key().ok();
        crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
            let mut store = parse_store(current)?;
            let record = store
                .records
                .iter_mut()
                .find(|record| {
                    record.metadata.credential_id == credential_id
                        && visible_scopes.contains(&record.metadata.scope)
                })
                .context("credential does not exist in the caller's visible scopes")?;
            let old_aad = record_aad(&record.metadata)?;
            let mut next = record.metadata.clone();
            next.scope = scope.clone();
            next.site = site.clone();
            next.label = pass.label.trim().to_string();
            next.username = username.clone();
            next.kind = pass.kind.to_string();
            next.updated_at = chrono::Utc::now().to_rfc3339();
            if replace_secret {
                next.metadata_json =
                    public_descriptors(pass.kind, pass.metadata_json, pass.secret, &pass.fields)?;
                let payload = SecretPayload {
                    secret: pass.secret.to_string(),
                    fields: pass.fields.clone(),
                };
                record.sealed = seal(&public, &payload, &record_aad(&next)?)?;
            } else {
                // Keep computed descriptors (brand/last4/fields) authoritative.
                next.metadata_json = merge_public_metadata(pass.metadata_json, &record.metadata.metadata_json)?;
                if record_aad(&next)? != old_aad {
                    let key = secret_key
                        .as_ref()
                        .context("Passes is locked; unlock once to move this pass to another site, type, or owner")?;
                    let payload = open(key, &record.sealed, &old_aad)?;
                    record.sealed = seal(&public, &payload, &record_aad(&next)?)?;
                }
            }
            record.metadata = next.clone();
            Ok((next, serde_json::to_vec_pretty(&store)?))
        })
    }

    /// Physically remove all ciphertext owned by one private lifecycle scope.
    /// Works while locked.
    pub fn purge_scope(&self, scope: &CredentialScope) -> Result<usize> {
        scope.validate()?;
        let _ = self.ensure_current_if_possible();
        let mut removed = 0;
        if self.store_path().exists() {
            removed += crate::config::private_io::read_modify_write_private(&self.store_path(), |current| {
                let mut store = parse_store(current)?;
                let before = store.records.len();
                store.records.retain(|record| &record.metadata.scope != scope);
                Ok((before - store.records.len(), serde_json::to_vec_pretty(&store)?))
            })?;
        }
        // A not-yet-migrated v2 store keeps its scope envelopes in plaintext.
        if self.legacy_store_path().exists() {
            removed += crate::config::private_io::read_modify_write_private(&self.legacy_store_path(), |current| {
                let mut store: LegacyStore = serde_json::from_slice(current.context("legacy store missing")?)
                    .context("invalid legacy vault store")?;
                let before = store.records.len();
                store.records.retain(|record| &record.scope != scope);
                Ok((before - store.records.len(), serde_json::to_vec_pretty(&store)?))
            })?;
        }
        Ok(removed)
    }

    // ---------------------------------------------------------------- state

    fn load_config(&self) -> Result<LoadedConfig> {
        let Some(bytes) = crate::config::private_io::read_private_file(&self.config_path())? else {
            return Ok(LoadedConfig::Missing);
        };
        let probe: serde_json::Value = serde_json::from_slice(&bytes).context("invalid vault config")?;
        match probe.get("version").and_then(|value| value.as_u64()) {
            Some(version) if version == u64::from(STORE_VERSION) => Ok(LoadedConfig::Current(
                serde_json::from_slice(&bytes).context("invalid vault config")?,
            )),
            Some(version) if version == u64::from(LEGACY_VERSION) => {
                let legacy: LegacyConfig = serde_json::from_slice(&bytes).context("invalid legacy vault config")?;
                // A v2 vault that already provisioned device-local agent
                // access can be upgraded right away; doing so deletes that
                // plaintext key file, which is the point of the upgrade.
                if let Some(key) = self.legacy_runtime_key(&legacy) {
                    self.migrate_legacy(&legacy, &key)?;
                    return Ok(LoadedConfig::Current(self.read_current_config()?));
                }
                Ok(LoadedConfig::Legacy(legacy))
            }
            other => anyhow::bail!("unsupported credential vault version {other:?}"),
        }
    }

    fn read_current_config(&self) -> Result<PassesConfig> {
        match self.load_config()? {
            LoadedConfig::Current(config) => Ok(config),
            LoadedConfig::Missing => anyhow::bail!("credential vault is not initialized"),
            LoadedConfig::Legacy(_) => anyhow::bail!(
                "Passes needs one unlock with your master password to finish upgrading this vault"
            ),
        }
    }

    fn ensure_current_if_possible(&self) -> Result<()> {
        match self.load_config()? {
            LoadedConfig::Legacy(_) => anyhow::bail!(
                "Passes needs one unlock with your master password to finish upgrading this vault"
            ),
            _ => Ok(()),
        }
    }

    /// Config that can accept a new sealed pass: creates an unprotected
    /// keypair on first save so saving never requires a password.
    fn ensure_writable_config(&self) -> Result<PassesConfig> {
        if let LoadedConfig::Current(config) = self.load_config()? {
            return Ok(config);
        }
        crate::config::private_io::with_private_lock(&self.root.join("initialize"), || {
            match self.load_config()? {
                LoadedConfig::Current(config) => Ok(config),
                LoadedConfig::Missing => {
                    self.create_unprotected_locked()?;
                    self.read_current_config()
                }
                LoadedConfig::Legacy(_) => anyhow::bail!(
                    "Passes needs one unlock with your master password to finish upgrading this vault before it can save"
                ),
            }
        })
    }

    /// Create the keypair and an unprotected config. Caller holds the
    /// initialize lock and has verified no config exists.
    fn create_unprotected_locked(&self) -> Result<()> {
        let unlock_key = random_key();
        let secret = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        let public = x25519_dalek::PublicKey::from(&secret);
        let public_b64 = URL_SAFE_NO_PAD.encode(public.as_bytes());
        let secret_bytes = Zeroizing::new(secret.to_bytes());
        let private_key_wrapped = encrypt_key(&unlock_key, &secret_bytes, &private_wrap_aad(&public_b64))?;
        let config = PassesConfig {
            version: STORE_VERSION,
            public_key: public_b64,
            private_key_wrapped,
            password_salt: None,
            kdf: None,
            password_wrapped_key: None,
            recovery_wrapped_key: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            migrated_from_version: None,
        };
        // A store without config can only be an interrupted first creation
        // (nothing sealed to a published key), so replace that orphan.
        if self.store_path().exists() {
            crate::config::private_io::remove_private_file(&self.store_path())?;
        }
        crate::config::private_io::atomic_write_private(&self.setup_key_path(), unlock_key.as_ref())?;
        crate::config::private_io::atomic_write_private(
            &self.store_path(),
            &serde_json::to_vec_pretty(&PassStore::default())?,
        )?;
        crate::config::private_io::atomic_write_private(&self.config_path(), &serde_json::to_vec_pretty(&config)?)
    }

    fn read_setup_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        let bytes = Zeroizing::new(
            crate::config::private_io::read_private_file(&self.setup_key_path())?
                .context("Passes setup key is missing")?,
        );
        anyhow::ensure!(bytes.len() == 32, "Passes setup key is invalid");
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }

    fn legacy_runtime_key(&self, legacy: &LegacyConfig) -> Option<Zeroizing<[u8; 32]>> {
        let wrapped = legacy.agent_runtime_wrapped_key.as_ref()?;
        let bytes = Zeroizing::new(
            crate::config::private_io::read_private_file(&self.legacy_agent_key_path()).ok()??,
        );
        if bytes.len() != 32 {
            return None;
        }
        let mut runtime = Zeroizing::new([0u8; 32]);
        runtime.copy_from_slice(&bytes);
        decrypt_key(&runtime, wrapped, LEGACY_AGENT_RUNTIME_WRAP_AAD).ok()
    }

    /// v2 → v3. The legacy data key becomes `K`, so the password and recovery
    /// key keep working. Crash-safe: the new config is the commit point and a
    /// rerun simply repeats the upgrade from the untouched v2 files.
    fn migrate_legacy(&self, legacy: &LegacyConfig, key: &[u8; 32]) -> Result<()> {
        crate::config::private_io::with_private_lock(&self.root.join("migrate"), || {
            // Another caller may have finished while we waited for the lock.
            if let Some(bytes) = crate::config::private_io::read_private_file(&self.config_path())? {
                let probe: serde_json::Value = serde_json::from_slice(&bytes)?;
                if probe.get("version").and_then(|v| v.as_u64()) == Some(u64::from(STORE_VERSION)) {
                    return Ok(());
                }
            }
            let legacy_store: LegacyStore = match crate::config::private_io::read_private_file(&self.legacy_store_path())? {
                Some(bytes) => serde_json::from_slice(&bytes).context("invalid legacy vault store")?,
                None => LegacyStore { version: LEGACY_VERSION, records: Vec::new() },
            };
            let secret = x25519_dalek::StaticSecret::random_from_rng(OsRng);
            let public = x25519_dalek::PublicKey::from(&secret);
            let public_b64 = URL_SAFE_NO_PAD.encode(public.as_bytes());
            let mut store = PassStore::default();
            for encrypted in &legacy_store.records {
                let mut record = decrypt_legacy_record(key, encrypted)
                    .context("vault data cannot be authenticated")?;
                let mut metadata = record.metadata.clone();
                if metadata.metadata_json.trim().is_empty() {
                    metadata.metadata_json = "{}".to_string();
                }
                let payload = SecretPayload {
                    secret: std::mem::take(&mut record.secret),
                    fields: BTreeMap::new(),
                };
                metadata.metadata_json = public_descriptors(&metadata.kind, &metadata.metadata_json, &payload.secret, &payload.fields)
                    .unwrap_or_else(|_| metadata.metadata_json.clone());
                let sealed = seal(&public, &payload, &record_aad(&metadata)?)?;
                store.records.push(PassRecord { metadata, sealed });
            }
            let secret_bytes = Zeroizing::new(secret.to_bytes());
            let config = PassesConfig {
                version: STORE_VERSION,
                private_key_wrapped: encrypt_key(key, &secret_bytes, &private_wrap_aad(&public_b64))?,
                public_key: public_b64,
                password_salt: Some(legacy.password_salt.clone()),
                kdf: Some(legacy_kdf()),
                password_wrapped_key: Some(legacy.password_wrapped_key.clone()),
                recovery_wrapped_key: Some(legacy.recovery_wrapped_key.clone()),
                created_at: legacy.created_at.clone(),
                migrated_from_version: Some(LEGACY_VERSION),
            };
            crate::config::private_io::atomic_write_private(&self.store_path(), &serde_json::to_vec_pretty(&store)?)?;
            // Keep encrypted v2 backups: still openable with the same password.
            if let Some(bytes) = crate::config::private_io::read_private_file(&self.config_path())? {
                crate::config::private_io::atomic_write_private(&self.root.join("config.v2.json.bak"), &bytes)?;
            }
            if let Some(bytes) = crate::config::private_io::read_private_file(&self.legacy_store_path())? {
                crate::config::private_io::atomic_write_private(&self.root.join("credentials.v2.enc.bak"), &bytes)?;
            }
            crate::config::private_io::atomic_write_private(&self.config_path(), &serde_json::to_vec_pretty(&config)?)?;
            crate::config::private_io::remove_private_file(&self.legacy_store_path())?;
            crate::config::private_io::remove_private_file(&self.legacy_agent_key_path())?;
            tracing::info!(records = store.records.len(), "Passes: upgraded v2 vault to sealed v3 store");
            Ok(())
        })
    }

    fn read_store(&self) -> Result<PassStore> {
        match crate::config::private_io::read_private_file(&self.store_path())? {
            Some(bytes) => parse_store(Some(&bytes)),
            None => Ok(PassStore::default()),
        }
    }

    /// Unlock key for reveal/use: memory when protected, setup file otherwise.
    fn private_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        let config = self.read_current_config()?;
        let key = if config.protected() {
            self.require_key()?
        } else {
            self.read_setup_key()?
        };
        open_private_key(&key, &config)
    }

    fn require_key(&self) -> Result<Zeroizing<[u8; 32]>> {
        self.unlocked_key(true)
            .context("Passes is locked; ask the user to unlock it once")
    }

    fn unlocked_key(&self, touch: bool) -> Option<Zeroizing<[u8; 32]>> {
        let now = Instant::now();
        let timeout = vault_auto_lock_duration();
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        let expired = keys.get(&self.root).is_some_and(|entry| {
            timeout.is_some_and(|timeout| now.saturating_duration_since(entry.last_used) >= timeout)
        });
        if expired {
            keys.remove(&self.root);
            return None;
        }
        let entry = keys.get_mut(&self.root)?;
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
                self.root.clone(),
                UnlockedVaultKey {
                    key: Zeroizing::new(*key),
                    last_used: Instant::now(),
                },
            );
    }

    #[cfg(test)]
    fn age_unlocked_key(&self, age: Duration) {
        let mut keys = unlocked_keys().lock().unwrap_or_else(|p| p.into_inner());
        let entry = keys.get_mut(&self.root).expect("vault key must be unlocked before aging it");
        entry.last_used = Instant::now().checked_sub(age).expect("test age must fit in Instant");
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    fn store_path(&self) -> PathBuf {
        self.root.join("passes.json")
    }

    fn setup_key_path(&self) -> PathBuf {
        self.root.join("setup.key")
    }

    fn legacy_store_path(&self) -> PathBuf {
        self.root.join("credentials.enc")
    }

    fn legacy_agent_key_path(&self) -> PathBuf {
        self.root.join("agent-runtime.key")
    }
}

fn unlocked_keys() -> &'static Mutex<HashMap<PathBuf, UnlockedVaultKey>> {
    UNLOCKED_KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Default 0: stay unlocked until Phoenix fully quits. A positive value is an
/// opt-in inactivity lock for people who want one.
fn vault_auto_lock_duration() -> Option<Duration> {
    let minutes = crate::settings::effective_u64(
        "security.vault_auto_lock_minutes",
        &crate::settings::SettingsScope::Global,
    )
    .unwrap_or(0);
    (minutes > 0).then(|| Duration::from_secs(minutes.saturating_mul(60)))
}

// ------------------------------------------------------------------ crypto

fn private_wrap_aad(public_b64: &str) -> Vec<u8> {
    format!("phoenix-passes-private-key-v3:{public_b64}").into_bytes()
}

fn decode_public_key(config: &PassesConfig) -> Result<x25519_dalek::PublicKey> {
    Ok(x25519_dalek::PublicKey::from(decode_exact::<32>(&config.public_key, "Passes public key")?))
}

/// Unwrap the X25519 private key with `K` and verify it matches the stored
/// public key (detects a swapped public key that would capture new saves).
fn open_private_key(key: &[u8; 32], config: &PassesConfig) -> Result<Zeroizing<[u8; 32]>> {
    let secret = decrypt_key(key, &config.private_key_wrapped, &private_wrap_aad(&config.public_key))?;
    let derived = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret));
    anyhow::ensure!(
        URL_SAFE_NO_PAD.encode(derived.as_bytes()) == config.public_key,
        "Passes public key does not match its private key"
    );
    Ok(secret)
}

fn seal_key(shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(recipient);
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), shared);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(b"phoenix-passes-v3/seal", okm.as_mut())
        .map_err(|_| anyhow::anyhow!("seal key derivation failed"))?;
    Ok(okm)
}

fn seal(recipient: &x25519_dalek::PublicKey, payload: &SecretPayload, aad: &[u8]) -> Result<SealedBox> {
    let ephemeral = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(recipient);
    anyhow::ensure!(shared.was_contributory(), "Passes public key is invalid");
    let key = seal_key(shared.as_bytes(), ephemeral_public.as_bytes(), recipient.as_bytes())?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("fixed key size");
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let plain = Zeroizing::new(serde_json::to_vec(payload)?);
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain.as_ref(), aad })
        .map_err(|_| anyhow::anyhow!("Passes encryption failed"))?;
    Ok(SealedBox {
        alg: SEAL_ALGORITHM.to_string(),
        ephemeral_public: URL_SAFE_NO_PAD.encode(ephemeral_public.as_bytes()),
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

fn open(secret_key: &[u8; 32], sealed: &SealedBox, aad: &[u8]) -> Result<SecretPayload> {
    anyhow::ensure!(sealed.alg == SEAL_ALGORITHM, "unsupported pass encryption {}", sealed.alg);
    let secret = x25519_dalek::StaticSecret::from(*secret_key);
    let recipient = x25519_dalek::PublicKey::from(&secret);
    let ephemeral = x25519_dalek::PublicKey::from(decode_exact::<32>(&sealed.ephemeral_public, "pass ephemeral key")?);
    let shared = secret.diffie_hellman(&ephemeral);
    anyhow::ensure!(shared.was_contributory(), "pass ephemeral key is invalid");
    let key = seal_key(shared.as_bytes(), ephemeral.as_bytes(), recipient.as_bytes())?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("fixed key size");
    let nonce = decode_exact::<24>(&sealed.nonce, "pass nonce")?;
    let ciphertext = URL_SAFE_NO_PAD.decode(&sealed.ciphertext).context("pass ciphertext is not valid base64")?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ciphertext, aad })
            .map_err(|_| anyhow::anyhow!("pass authentication failed"))?,
    );
    serde_json::from_slice(plain.as_ref()).context("decrypted pass is invalid")
}

/// Bind identity, owner, site, and kind into every sealed box.
fn record_aad(metadata: &CredentialMetadata) -> Result<Vec<u8>> {
    Ok(format!(
        "phoenix-passes-record-v3\0{}\0{}\0{}\0{}",
        metadata.credential_id,
        serde_json::to_string(&metadata.scope)?,
        metadata.site,
        metadata.kind
    )
    .into_bytes())
}

fn parse_store(bytes: Option<&[u8]>) -> Result<PassStore> {
    let Some(bytes) = bytes else {
        return Ok(PassStore::default());
    };
    let store: PassStore = serde_json::from_slice(bytes).context("invalid Passes store")?;
    anyhow::ensure!(store.version == STORE_VERSION, "unsupported Passes store version {}", store.version);
    anyhow::ensure!(store.records.len() <= MAX_RECORDS, "Passes store is oversized");
    for record in &store.records {
        validate_id(&record.metadata.credential_id, "credential id")?;
        record.metadata.scope.validate()?;
        anyhow::ensure!(
            record.sealed.ciphertext.len() <= MAX_SECRET_BYTES * 3,
            "sealed pass is oversized"
        );
    }
    Ok(store)
}

fn decrypt_legacy_record(key: &[u8; 32], encrypted: &LegacyRecord) -> Result<LegacySecretRecord> {
    let aad = format!(
        "phoenix-vault-record-v2:{}:{}",
        encrypted.credential_id,
        serde_json::to_string(&encrypted.scope)?
    )
    .into_bytes();
    let plain = Zeroizing::new(decrypt_bytes(key, &encrypted.nonce, &encrypted.ciphertext, &aad)?);
    let record: LegacySecretRecord =
        serde_json::from_slice(plain.as_ref()).context("decrypted credential is invalid")?;
    anyhow::ensure!(
        record.metadata.credential_id == encrypted.credential_id && record.metadata.scope == encrypted.scope,
        "credential envelope does not match encrypted payload"
    );
    Ok(record)
}

fn default_kdf() -> KdfConfig {
    #[cfg(test)]
    let m_cost_kib = 8 * 1024;
    #[cfg(not(test))]
    let m_cost_kib = 64 * 1024;
    KdfConfig {
        algorithm: "argon2id".to_string(),
        m_cost_kib,
        t_cost: 3,
        p_cost: 1,
    }
}

/// The fixed parameters every v2 vault used.
fn legacy_kdf() -> KdfConfig {
    default_kdf()
}

fn derive_password_key(password: &str, salt: &[u8; 16], kdf: &KdfConfig) -> Result<Zeroizing<[u8; 32]>> {
    anyhow::ensure!(kdf.algorithm == "argon2id", "unsupported KDF {}", kdf.algorithm);
    anyhow::ensure!(
        (8 * 1024..=4 * 1024 * 1024).contains(&kdf.m_cost_kib) && (1..=64).contains(&kdf.t_cost) && (1..=16).contains(&kdf.p_cost),
        "vault KDF parameters are out of range"
    );
    let params = Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, Some(KDF_OUTPUT_BYTES))
        .map_err(|error| anyhow::anyhow!("invalid vault KDF parameters: {error}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
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
    let plain = Zeroizing::new(decrypt_bytes(key, &wrapped.nonce, &wrapped.ciphertext, aad)?);
    anyhow::ensure!(plain.len() == 32, "wrapped vault key has the wrong size");
    let mut output = Zeroizing::new([0u8; 32]);
    output.copy_from_slice(&plain);
    Ok(output)
}

fn encrypt_bytes(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Result<(String, String)> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key has fixed size");
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| anyhow::anyhow!("vault encryption failed"))?;
    Ok((URL_SAFE_NO_PAD.encode(nonce), URL_SAFE_NO_PAD.encode(ciphertext)))
}

fn decrypt_bytes(key: &[u8; 32], nonce: &str, ciphertext: &str, aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key has fixed size");
    let nonce = decode_exact::<12>(nonce, "vault nonce")?;
    let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext).context("vault ciphertext is not valid base64")?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ciphertext, aad })
        .map_err(|_| anyhow::anyhow!("vault authentication failed"))
}

fn decode_exact<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N]> {
    let decoded = URL_SAFE_NO_PAD.decode(encoded).with_context(|| format!("{label} is not valid base64"))?;
    decoded.try_into().map_err(|_| anyhow::anyhow!("{label} has the wrong size"))
}

fn parse_recovery_key(recovery_key: &str) -> Result<Zeroizing<[u8; 32]>> {
    let encoded = recovery_key
        .trim()
        .strip_prefix(RECOVERY_PREFIX)
        .context("recovery key has an invalid prefix")?;
    Ok(Zeroizing::new(decode_exact::<32>(encoded, "recovery key")?))
}

// -------------------------------------------------------------- validation

fn validate_master_password(password: &str) -> Result<()> {
    anyhow::ensure!(password.chars().count() >= 12, "master password must contain at least 12 characters");
    anyhow::ensure!(password.len() <= 1024, "master password is too long");
    Ok(())
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{label} cannot be empty");
    anyhow::ensure!(value.len() <= 128, "{label} is too long");
    anyhow::ensure!(
        value.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "{label} contains unsupported characters"
    );
    Ok(())
}

fn validate_text(value: &str, max: usize, label: &str) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{label} cannot be empty");
    anyhow::ensure!(value.len() <= max, "{label} exceeds {max} bytes");
    Ok(())
}

fn validate_kind(kind: &str) -> Result<()> {
    validate_text(kind, 64, "pass kind")?;
    anyhow::ensure!(
        kind.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
        "pass kind must be a lower_snake_case word"
    );
    Ok(())
}

fn validate_secret(secret: &str) -> Result<()> {
    anyhow::ensure!(!secret.is_empty(), "credential secret cannot be empty");
    anyhow::ensure!(secret.len() <= MAX_SECRET_BYTES, "credential secret exceeds {MAX_SECRET_BYTES} bytes");
    Ok(())
}

fn validate_fields(fields: &BTreeMap<String, String>) -> Result<()> {
    anyhow::ensure!(fields.len() <= MAX_FIELDS, "a pass can hold at most {MAX_FIELDS} secret fields");
    let total: usize = fields.iter().map(|(key, value)| key.len() + value.len()).sum();
    anyhow::ensure!(total <= MAX_SECRET_BYTES, "pass fields are too large");
    for key in fields.keys() {
        validate_kind(key).context("pass field names must be lower_snake_case")?;
    }
    Ok(())
}

fn normalize_username(username: Option<String>) -> Result<Option<String>> {
    match username.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
        Some(value) => {
            validate_text(&value, MAX_LABEL_BYTES, "credential username")?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn parse_metadata_object(value: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    let value = if value.trim().is_empty() { "{}" } else { value };
    anyhow::ensure!(value.len() <= MAX_METADATA_BYTES, "credential metadata exceeds {MAX_METADATA_BYTES} bytes");
    match serde_json::from_str::<serde_json::Value>(value).context("metadata_json is invalid")? {
        serde_json::Value::Object(map) => Ok(map),
        _ => anyhow::bail!("metadata_json must be a JSON object"),
    }
}

/// Public keys computed by Phoenix; a caller can never forge or clear them.
const COMPUTED_KEYS: &[&str] = &["brand", "last4", "fields"];

/// Compute public descriptors from the secret at save time: card brand and
/// last four digits, an API key's last four, and the names of secondary
/// fields. Caller-supplied values for those keys are ignored.
fn public_descriptors(kind: &str, metadata_json: &str, secret: &str, fields: &BTreeMap<String, String>) -> Result<String> {
    let mut map = parse_metadata_object(metadata_json)?;
    for key in COMPUTED_KEYS {
        map.remove(*key);
    }
    match kind {
        "card" => {
            let digits: String = secret.chars().filter(|ch| ch.is_ascii_digit()).collect();
            map.insert("brand".into(), serde_json::json!(card_brand(&digits)));
            if digits.len() >= 4 {
                map.insert("last4".into(), serde_json::json!(&digits[digits.len() - 4..]));
            }
        }
        "api_key" | "token" => {
            let trimmed = secret.trim();
            if trimmed.chars().count() >= 12 {
                let tail: String = trimmed.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                map.insert("last4".into(), serde_json::json!(tail));
            }
        }
        _ => {}
    }
    if !fields.is_empty() {
        map.insert("fields".into(), serde_json::json!(fields.keys().collect::<Vec<_>>()));
    }
    let text = serde_json::Value::Object(map).to_string();
    anyhow::ensure!(text.len() <= MAX_METADATA_BYTES, "credential metadata exceeds {MAX_METADATA_BYTES} bytes");
    Ok(text)
}

fn merge_public_metadata(requested: &str, existing: &str) -> Result<String> {
    let mut map = parse_metadata_object(requested)?;
    let existing = parse_metadata_object(existing).unwrap_or_default();
    for key in COMPUTED_KEYS {
        map.remove(*key);
        if let Some(value) = existing.get(*key) {
            map.insert((*key).to_string(), value.clone());
        }
    }
    Ok(serde_json::Value::Object(map).to_string())
}

fn field_names(metadata: &CredentialMetadata) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(&metadata.metadata_json)
        .ok()
        .and_then(|value| value.get("fields").cloned())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Card network from the leading digits (IIN ranges).
pub fn card_brand(digits: &str) -> &'static str {
    let prefix = |n: usize| digits.get(..n).and_then(|value| value.parse::<u32>().ok()).unwrap_or(0);
    if digits.starts_with('4') {
        "visa"
    } else if (51..=55).contains(&prefix(2)) || (2221..=2720).contains(&prefix(4)) {
        "mastercard"
    } else if matches!(prefix(2), 34 | 37) {
        "amex"
    } else if prefix(4) == 6011 || prefix(2) == 65 || (644..=649).contains(&prefix(3)) {
        "discover"
    } else if matches!(prefix(2), 36 | 38 | 39) || (300..=305).contains(&prefix(3)) {
        "diners"
    } else if (3528..=3589).contains(&prefix(4)) {
        "jcb"
    } else if prefix(2) == 62 {
        "unionpay"
    } else {
        "card"
    }
}

/// Luhn checksum for card numbers.
pub fn luhn_valid(digits: &str) -> bool {
    if digits.len() < 12 || digits.len() > 19 || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = digits
        .chars()
        .rev()
        .enumerate()
        .map(|(index, ch)| {
            let mut digit = ch.to_digit(10).unwrap_or(0);
            if index % 2 == 1 {
                digit *= 2;
                if digit > 9 {
                    digit -= 9;
                }
            }
            digit
        })
        .sum();
    sum % 10 == 0
}

pub(crate) fn normalize_site(site: &str) -> Result<String> {
    let site = site.trim();
    anyhow::ensure!(!site.is_empty(), "credential site cannot be empty");
    anyhow::ensure!(site.len() <= MAX_SITE_BYTES, "credential site is too long");
    let candidate = if site.contains("://") {
        url::Url::parse(site).context("credential site is not a valid URL")?
    } else {
        url::Url::parse(&format!("https://{site}")).context("credential site is not a valid domain")?
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

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct horse battery staple";

    fn vault() -> (tempfile::TempDir, crate::config::test_env::PhoenixHomeGuard, Vault) {
        let root = tempfile::tempdir().unwrap();
        let guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let vault = Vault::at(root.path());
        (root, guard, vault)
    }

    fn company() -> Vec<CredentialScope> {
        vec![CredentialScope::Company]
    }

    fn put_login(vault: &Vault, secret: &str) -> CredentialMetadata {
        vault
            .put(CredentialScope::Company, "https://www.Example.com/login", "Example", Some("me@example.com".into()), "password", "{}", secret)
            .unwrap()
    }

    #[test]
    fn crypto_round_trip_seals_to_public_key_and_binds_aad() {
        let secret = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        let public = x25519_dalek::PublicKey::from(&secret);
        let payload = SecretPayload {
            secret: "hunter2-hunter2".into(),
            fields: BTreeMap::from([("cvc".to_string(), "123".to_string())]),
        };
        let sealed = seal(&public, &payload, b"aad-one").unwrap();
        assert!(!sealed.ciphertext.contains("hunter2"));
        let opened = open(&Zeroizing::new(secret.to_bytes()), &sealed, b"aad-one").unwrap();
        assert_eq!(opened.secret, "hunter2-hunter2");
        assert_eq!(opened.fields.get("cvc").map(String::as_str), Some("123"));
        assert!(open(&Zeroizing::new(secret.to_bytes()), &sealed, b"aad-two").is_err());
        let other = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        assert!(open(&Zeroizing::new(other.to_bytes()), &sealed, b"aad-one").is_err());
        // Two seals of the same payload never share ciphertext.
        assert_ne!(seal(&public, &payload, b"aad-one").unwrap().ciphertext, sealed.ciphertext);
    }

    #[test]
    fn saving_never_needs_a_password_and_setting_one_keeps_existing_passes() {
        let (root, _guard, vault) = vault();
        assert_eq!(vault.status(), VaultStatus::Uninitialized);
        let saved = put_login(&vault, "first-secret-value");
        assert_eq!(saved.site, "example.com");
        assert_eq!(vault.status(), VaultStatus::Unprotected);
        // Unprotected passes are usable (nothing to unlock with yet).
        assert_eq!(vault.reveal(&saved.credential_id, &company()).unwrap().secret(), "first-secret-value");
        let recovery = vault.initialize(PASSWORD).unwrap();
        assert!(recovery.starts_with(RECOVERY_PREFIX));
        assert!(!root.path().join("vault/setup.key").exists());
        assert_eq!(vault.status(), VaultStatus::Unlocked);
        vault.lock();
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.reveal(&saved.credential_id, &company()).is_err());
        // Save while locked, list while locked.
        let second = put_login(&vault, "second-secret-value");
        assert_eq!(vault.list(&company()).unwrap().len(), 2);
        vault.unlock_with_password(PASSWORD).unwrap();
        assert_eq!(vault.reveal(&second.credential_id, &company()).unwrap().secret(), "second-secret-value");
        assert_eq!(vault.reveal(&saved.credential_id, &company()).unwrap().secret(), "first-secret-value");
        // Nothing secret is on disk in plaintext.
        for entry in std::fs::read_dir(root.path().join("vault")).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("first-secret-value") && !text.contains("second-secret-value"));
        }
    }

    #[test]
    fn wrong_password_fails_and_recovery_key_unlocks_the_same_store() {
        let (_root, _guard, vault) = vault();
        let recovery = vault.initialize(PASSWORD).unwrap();
        let saved = put_login(&vault, "recoverable-secret");
        vault.lock();
        assert!(vault.unlock_with_password("not the right password").is_err());
        assert_eq!(vault.status(), VaultStatus::Locked);
        vault.unlock_with_recovery_key(&recovery).unwrap();
        assert_eq!(vault.reveal(&saved.credential_id, &company()).unwrap().secret(), "recoverable-secret");
        vault.lock();
        vault.change_master_password(PASSWORD, "a brand new master password").unwrap();
        vault.lock();
        assert!(vault.unlock_with_password(PASSWORD).is_err());
        vault.unlock_with_password("a brand new master password").unwrap();
        let next_recovery = vault.rotate_recovery_key().unwrap();
        vault.lock();
        assert!(vault.unlock_with_recovery_key(&recovery).is_err());
        vault.unlock_with_recovery_key(&next_recovery).unwrap();
    }

    #[test]
    fn unlock_persists_for_process_lifetime_by_default() {
        let (_root, _guard, vault) = vault();
        vault.initialize(PASSWORD).unwrap();
        vault.lock();
        vault.unlock_with_password(PASSWORD).unwrap();
        vault.age_unlocked_key(Duration::from_secs(24 * 60 * 60));
        assert_eq!(vault.status(), VaultStatus::Unlocked);
        Vault::lock_all();
        assert_eq!(vault.status(), VaultStatus::Locked);
    }

    #[test]
    fn cards_store_brand_and_last4_publicly_and_everything_else_sealed() {
        let (_root, _guard, vault) = vault();
        vault.initialize(PASSWORD).unwrap();
        let card = vault
            .put_pass(NewPass {
                scope: Some(CredentialScope::Company),
                site: "pay.example.com",
                label: "Personal Visa",
                kind: "card",
                metadata_json: r#"{"brand":"forged","last4":"0000"}"#,
                secret: "4242 4242 4242 4242",
                fields: BTreeMap::from([
                    ("cvc".to_string(), "314".to_string()),
                    ("expiry".to_string(), "12/29".to_string()),
                    ("name".to_string(), "Mikael Example".to_string()),
                    ("billing_zip".to_string(), "T2P 1J9".to_string()),
                ]),
                ..Default::default()
            })
            .unwrap();
        let public: serde_json::Value = serde_json::from_str(&card.metadata_json).unwrap();
        assert_eq!(public["brand"], "visa");
        assert_eq!(public["last4"], "4242");
        assert!(!card.metadata_json.contains("314") && !card.metadata_json.contains("Mikael"));
        vault.lock();
        assert!(vault.reveal(&card.credential_id, &company()).is_err());
        vault.unlock_with_password(PASSWORD).unwrap();
        let revealed = vault.reveal(&card.credential_id, &company()).unwrap();
        assert_eq!(revealed.secret(), "4242 4242 4242 4242");
        assert_eq!(revealed.field("number"), Some("4242 4242 4242 4242"));
        assert_eq!(revealed.field("cvc"), Some("314"));
        assert!(!format!("{revealed:?}").contains("314"));
        assert!(luhn_valid("4242424242424242") && !luhn_valid("4242424242424241"));
        assert_eq!(card_brand("378282246310005"), "amex");
        assert_eq!(card_brand("5555555555554444"), "mastercard");
    }

    #[test]
    fn sealed_secret_is_bound_to_its_site_so_tampered_metadata_fails() {
        let (root, _guard, vault) = vault();
        vault.initialize(PASSWORD).unwrap();
        let saved = put_login(&vault, "bound-secret");
        let path = root.path().join("vault/passes.json");
        let tampered = std::fs::read_to_string(&path).unwrap().replace("\"example.com\"", "\"evil.example\"");
        std::fs::write(&path, tampered).unwrap();
        assert!(vault.reveal(&saved.credential_id, &company()).is_err());
    }

    #[test]
    fn scopes_are_enforced_and_metadata_updates_preserve_the_secret() {
        let (_root, _guard, vault) = vault();
        vault.initialize(PASSWORD).unwrap();
        let mine = vault
            .put(CredentialScope::agent("avery"), "example.com", "Avery", None, "password", "{}", "avery-secret")
            .unwrap();
        assert!(vault.list(&[CredentialScope::agent("nico")]).unwrap().is_empty());
        assert!(vault.reveal(&mine.credential_id, &[CredentialScope::agent("nico")]).is_err());
        vault.lock();
        // Relabel while locked (no re-seal needed).
        let renamed = vault
            .update(&mine.credential_id, &[CredentialScope::agent("avery")], CredentialScope::agent("avery"), "example.com", "Avery main", Some("a@example.com".into()), "password", "{}", None)
            .unwrap();
        assert_eq!(renamed.label, "Avery main");
        // Moving to another site re-seals and needs the unlock.
        assert!(vault
            .update(&mine.credential_id, &[CredentialScope::agent("avery")], CredentialScope::agent("avery"), "other.com", "Avery main", None, "password", "{}", None)
            .is_err());
        vault.unlock_with_password(PASSWORD).unwrap();
        vault
            .update(&mine.credential_id, &[CredentialScope::agent("avery")], CredentialScope::Company, "other.com", "Avery main", None, "password", "{}", None)
            .unwrap();
        assert_eq!(vault.reveal(&mine.credential_id, &company()).unwrap().secret(), "avery-secret");
        assert!(vault.delete(&mine.credential_id, &company()).unwrap());
    }

    #[test]
    fn locked_scope_purge_removes_only_owned_ciphertext() {
        let (_root, _guard, vault) = vault();
        vault.initialize(PASSWORD).unwrap();
        vault.put(CredentialScope::agent("avery"), "a.com", "A", None, "password", "{}", "s1-secret").unwrap();
        let keep = vault.put(CredentialScope::Company, "b.com", "B", None, "password", "{}", "s2-secret").unwrap();
        vault.lock();
        assert_eq!(vault.purge_scope(&CredentialScope::agent("avery")).unwrap(), 1);
        let left = vault.list(&[CredentialScope::agent("avery"), CredentialScope::Company]).unwrap();
        assert_eq!(left, vec![keep]);
    }

    /// Build a genuine v2 vault on disk exactly as the previous release did.
    fn write_legacy_vault(root: &std::path::Path, with_runtime_key: bool) -> String {
        let dir = root.join("vault");
        std::fs::create_dir_all(&dir).unwrap();
        let data_key = random_key();
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let password_key = derive_password_key(PASSWORD, &salt, &legacy_kdf()).unwrap();
        let recovery = random_key();
        let runtime = random_key();
        let config = LegacyConfig {
            version: 2,
            password_salt: URL_SAFE_NO_PAD.encode(salt),
            password_wrapped_key: encrypt_key(&password_key, &data_key, PASSWORD_WRAP_AAD).unwrap(),
            recovery_wrapped_key: encrypt_key(&recovery, &data_key, RECOVERY_WRAP_AAD).unwrap(),
            agent_runtime_wrapped_key: with_runtime_key
                .then(|| encrypt_key(&runtime, &data_key, LEGACY_AGENT_RUNTIME_WRAP_AAD).unwrap()),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let metadata = CredentialMetadata {
            credential_id: "legacy-1".into(),
            scope: CredentialScope::Company,
            site: "legacy.example".into(),
            label: "Legacy account".into(),
            username: Some("old@example.com".into()),
            kind: "password".into(),
            metadata_json: "{}".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        let record = LegacySecretRecord { metadata, secret: "legacy-secret-value".into() };
        let aad = b"phoenix-vault-record-v2:legacy-1:{\"scope\":\"company\"}";
        let (nonce, ciphertext) = encrypt_bytes(&data_key, &serde_json::to_vec(&record).unwrap(), aad).unwrap();
        let store = LegacyStore {
            version: 2,
            records: vec![LegacyRecord { credential_id: "legacy-1".into(), scope: CredentialScope::Company, nonce, ciphertext }],
        };
        crate::config::private_io::atomic_write_private(&dir.join("config.json"), &serde_json::to_vec(&config).unwrap()).unwrap();
        crate::config::private_io::atomic_write_private(&dir.join("credentials.enc"), &serde_json::to_vec(&store).unwrap()).unwrap();
        if with_runtime_key {
            crate::config::private_io::atomic_write_private(&dir.join("agent-runtime.key"), runtime.as_ref()).unwrap();
        }
        format!("{RECOVERY_PREFIX}{}", URL_SAFE_NO_PAD.encode(recovery.as_ref()))
    }

    #[test]
    fn legacy_vault_migrates_on_first_unlock_without_losing_data() {
        let (root, _guard, vault) = vault();
        let recovery = write_legacy_vault(root.path(), false);
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.list(&company()).is_err(), "pre-upgrade list must not pretend to be empty");
        vault.unlock_with_password(PASSWORD).unwrap();
        let listed = vault.list(&company()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Legacy account");
        assert_eq!(vault.reveal("legacy-1", &company()).unwrap().secret(), "legacy-secret-value");
        assert!(root.path().join("vault/credentials.v2.enc.bak").exists());
        assert!(!root.path().join("vault/credentials.enc").exists());
        // The old password and recovery key both still work.
        vault.lock();
        vault.unlock_with_recovery_key(&recovery).unwrap();
        vault.lock();
        vault.unlock_with_password(PASSWORD).unwrap();
    }

    #[test]
    fn legacy_vault_with_plaintext_runtime_key_upgrades_immediately_and_deletes_it() {
        let (root, _guard, vault) = vault();
        write_legacy_vault(root.path(), true);
        // Listing works with no password; the runtime key file is gone.
        assert_eq!(vault.list(&company()).unwrap().len(), 1);
        assert!(!root.path().join("vault/agent-runtime.key").exists());
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.reveal("legacy-1", &company()).is_err());
        vault.unlock_with_password(PASSWORD).unwrap();
        assert_eq!(vault.reveal("legacy-1", &company()).unwrap().secret(), "legacy-secret-value");
    }

    #[test]
    fn init_rejects_short_passwords_and_double_initialization() {
        let (_root, _guard, vault) = vault();
        assert!(vault.initialize("short").is_err());
        vault.initialize(PASSWORD).unwrap();
        assert!(vault.initialize("another long password").is_err());
    }
}
