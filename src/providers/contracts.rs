//! Provider contracts used by Phoenix runtime components.

use async_trait::async_trait;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Durable schema for provider-native compaction replay state.
///
/// The logical [`crate::session::Message`] transcript remains the portable
/// source of truth. This schema stores only an optional, provider-scoped replay
/// accelerator beside it.
pub const NATIVE_COMPACTION_SCHEMA_VERSION: u32 = 1;

/// Provider replies are untrusted input. Bound opaque replay state well below
/// the session file ceiling so portable history always retains room to save.
pub const MAX_NATIVE_COMPACTION_REPLAY_ITEMS: usize = 4_096;
pub const MAX_NATIVE_COMPACTION_REPLAY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_COMPACTION_REPLAY_ITEM_BYTES: usize = 4 * 1024 * 1024;

/// Bounds for one native compaction attempt. These are deliberately generous
/// enough for Phoenix's late-fold policy while preventing an adapter from
/// accidentally constructing an unbounded second model request.
pub const MAX_NATIVE_COMPACTION_INPUT_MESSAGES: usize = 4_096;
pub const MAX_NATIVE_COMPACTION_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NATIVE_COMPACTION_INSTRUCTIONS_BYTES: usize = 1024 * 1024;
pub const MAX_NATIVE_COMPACTION_TOOLS: usize = 512;
pub const MAX_NATIVE_COMPACTION_PORTABLE_SUMMARY_BYTES: usize = 512 * 1024;

const MAX_NATIVE_PROVIDER_ID_BYTES: usize = 128;
const MAX_NATIVE_MODEL_ID_BYTES: usize = 512;
const MAX_NATIVE_ACCOUNT_SCOPE_BYTES: usize = 256;
const MAX_NATIVE_BASE_ROUTE_BYTES: usize = 2_048;
const MAX_NATIVE_SESSION_ID_BYTES: usize = 192;

/// Stable, non-secret identity of the account backing one provider client.
///
/// A scope names where authentication came from (`profile:<id>`, `env:<VAR>`,
/// or `none`). The epoch is deliberately separate from credential material:
/// stored-profile epochs advance when an account is replaced, while env-backed
/// providers use one random process epoch and therefore fall back to portable
/// history after a gateway restart. Neither field is a token fingerprint.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct ProviderAuthIdentity {
    account_scope: String,
    auth_epoch: u64,
}

impl ProviderAuthIdentity {
    pub fn try_new(account_scope: impl Into<String>, auth_epoch: u64) -> anyhow::Result<Self> {
        let account_scope = account_scope.into();
        validate_route_label(
            "account scope",
            &account_scope,
            MAX_NATIVE_ACCOUNT_SCOPE_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@'),
        )?;
        anyhow::ensure!(auth_epoch > 0, "provider auth epoch must be positive");
        Ok(Self {
            account_scope,
            auth_epoch,
        })
    }

    pub fn account_scope(&self) -> &str {
        &self.account_scope
    }

    pub fn auth_epoch(&self) -> u64 {
        self.auth_epoch
    }
}

impl fmt::Debug for ProviderAuthIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAuthIdentity")
            .field("account_scope", &"<redacted-account-scope>")
            .field("auth_epoch", &self.auth_epoch)
            .finish()
    }
}

/// The provider wire primitive available for a concrete provider/model route.
///
/// `ResponsesCompact` covers OpenAI-compatible `/responses/compact` endpoints,
/// whose result contains opaque encrypted replay items. `AnthropicCompact`
/// covers the Messages API's `compact_20260112` context-management edit, whose
/// result is a replayable assistant compaction block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompactionCapability {
    #[default]
    Unsupported,
    ResponsesCompact,
    AnthropicCompact,
}

impl NativeCompactionCapability {
    pub fn is_supported(self) -> bool {
        self != Self::Unsupported
    }
}

/// Non-secret identity of one concrete native-compaction wire route.
///
/// This is intentionally constructed once at the provider boundary and then
/// carried through the request/result validators. In particular, an adapter
/// cannot accept replay merely because the model string happens to match.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct NativeCompactionRoute {
    capability: NativeCompactionCapability,
    provider: String,
    base_route: String,
    model: String,
    account_scope: String,
    auth_epoch: u64,
}

impl NativeCompactionRoute {
    pub fn try_new(
        capability: NativeCompactionCapability,
        provider: impl Into<String>,
        base_route: impl Into<String>,
        model: impl Into<String>,
        account_scope: impl Into<String>,
        auth_epoch: u64,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            capability.is_supported(),
            "unsupported providers cannot construct a native compaction route"
        );
        let provider = provider.into();
        let model = model.into();
        let account_scope = account_scope.into();
        validate_route_label(
            "provider id",
            &provider,
            MAX_NATIVE_PROVIDER_ID_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'),
        )?;
        validate_text_id("model id", &model, MAX_NATIVE_MODEL_ID_BYTES)?;
        validate_route_label(
            "account scope",
            &account_scope,
            MAX_NATIVE_ACCOUNT_SCOPE_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@'),
        )?;
        anyhow::ensure!(
            auth_epoch > 0,
            "native compaction auth epoch must be positive"
        );
        let base_route = normalize_base_route(&base_route.into())?;
        Ok(Self {
            capability,
            provider,
            base_route,
            model,
            account_scope,
            auth_epoch,
        })
    }

    pub fn try_for_identity(
        capability: NativeCompactionCapability,
        provider: impl Into<String>,
        base_route: impl Into<String>,
        model: impl Into<String>,
        identity: &ProviderAuthIdentity,
    ) -> anyhow::Result<Self> {
        Self::try_new(
            capability,
            provider,
            base_route,
            model,
            identity.account_scope(),
            identity.auth_epoch(),
        )
    }

    pub fn capability(&self) -> NativeCompactionCapability {
        self.capability
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn base_route(&self) -> &str {
        &self.base_route
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn account_scope(&self) -> &str {
        &self.account_scope
    }

    pub fn auth_epoch(&self) -> u64 {
        self.auth_epoch
    }
}

impl fmt::Debug for NativeCompactionRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionRoute")
            .field("capability", &self.capability)
            .field("provider", &self.provider)
            .field("base_route", &"<redacted-route>")
            .field("model", &self.model)
            .field("account_scope", &"<redacted-account-scope>")
            .field("auth_epoch", &self.auth_epoch)
            .finish()
    }
}

/// Wire dialect that produced a provider response.
///
/// This is deliberately coarser than a provider id but finer than a generic
/// HTTP route. Native replay created by one protocol family must never be
/// retained after a response arrived through another one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderResponseDialect {
    #[default]
    Generic,
    OpenAiResponses,
    AnthropicMessages,
}

impl ProviderResponseDialect {
    pub fn for_native_capability(capability: NativeCompactionCapability) -> Option<Self> {
        match capability {
            NativeCompactionCapability::Unsupported => None,
            NativeCompactionCapability::ResponsesCompact => Some(Self::OpenAiResponses),
            NativeCompactionCapability::AnthropicCompact => Some(Self::AnthropicMessages),
        }
    }
}

/// Non-secret receipt for the concrete account route that returned a response.
///
/// Unlike [`NativeCompactionRoute`], this can describe providers that do not
/// support native compaction. It contains no credential, opaque replay item, or
/// credential fingerprint. The factory identity wrapper is the trust boundary
/// that constructs it after an actual provider call succeeds.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct ProviderResponseRoute {
    dialect: ProviderResponseDialect,
    provider: String,
    base_route: String,
    model: String,
    account_scope: String,
    auth_epoch: u64,
}

impl ProviderResponseRoute {
    pub fn try_for_identity(
        dialect: ProviderResponseDialect,
        provider: impl Into<String>,
        base_route: impl Into<String>,
        model: impl Into<String>,
        identity: &ProviderAuthIdentity,
    ) -> anyhow::Result<Self> {
        let provider = provider.into();
        let model = model.into();
        validate_route_label(
            "provider id",
            &provider,
            MAX_NATIVE_PROVIDER_ID_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'),
        )?;
        validate_text_id("model id", &model, MAX_NATIVE_MODEL_ID_BYTES)?;
        let base_route = normalize_base_route(&base_route.into())?;
        Ok(Self {
            dialect,
            provider,
            base_route,
            model,
            account_scope: identity.account_scope().to_string(),
            auth_epoch: identity.auth_epoch(),
        })
    }

    pub fn dialect(&self) -> ProviderResponseDialect {
        self.dialect
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn base_route(&self) -> &str {
        &self.base_route
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn account_scope(&self) -> &str {
        &self.account_scope
    }

    pub fn auth_epoch(&self) -> u64 {
        self.auth_epoch
    }

    /// Exact compatibility check against the native replay route used for the
    /// request. Same provider/model is insufficient: a different account,
    /// endpoint, dialect, or credential generation invalidates opaque replay.
    pub fn matches_native_route(&self, route: &NativeCompactionRoute) -> bool {
        ProviderResponseDialect::for_native_capability(route.capability()).is_some_and(|dialect| {
            self.dialect == dialect
                && self.provider == route.provider()
                && self.base_route == route.base_route()
                && self.model == route.model()
                && self.account_scope == route.account_scope()
                && self.auth_epoch == route.auth_epoch()
        })
    }
}

impl fmt::Debug for ProviderResponseRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponseRoute")
            .field("dialect", &self.dialect)
            .field("provider", &self.provider)
            .field("base_route", &"<redacted-route>")
            .field("model", &self.model)
            .field("account_scope", &"<redacted-account-scope>")
            .field("auth_epoch", &self.auth_epoch)
            .finish()
    }
}

/// Non-secret identity of the exact route that created opaque replay state.
///
/// `account_scope` is a Phoenix auth-profile id (or another stable, non-secret
/// account label), never a key/token or a hash of credential material.
/// `auth_epoch` changes when that profile is replaced by a different account.
/// `compaction_generation` increments for each fold in the session.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct NativeCompactionProvenance {
    schema_version: u32,
    session_id: String,
    provider: String,
    base_route: String,
    model: String,
    account_scope: String,
    auth_epoch: u64,
    compaction_generation: u64,
    transcript_revision: u64,
}

impl NativeCompactionProvenance {
    pub fn try_new(
        session_id: impl Into<String>,
        provider: impl Into<String>,
        base_route: impl Into<String>,
        model: impl Into<String>,
        account_scope: impl Into<String>,
        auth_epoch: u64,
        compaction_generation: u64,
        transcript_revision: u64,
    ) -> anyhow::Result<Self> {
        let session_id = session_id.into();
        let provider = provider.into();
        let model = model.into();
        let account_scope = account_scope.into();
        validate_session_id(&session_id)?;
        validate_route_label(
            "provider id",
            &provider,
            MAX_NATIVE_PROVIDER_ID_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'),
        )?;
        validate_text_id("model id", &model, MAX_NATIVE_MODEL_ID_BYTES)?;
        validate_route_label(
            "account scope",
            &account_scope,
            MAX_NATIVE_ACCOUNT_SCOPE_BYTES,
            |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@'),
        )?;
        anyhow::ensure!(
            auth_epoch > 0,
            "native compaction auth epoch must be positive"
        );
        anyhow::ensure!(
            compaction_generation > 0,
            "native compaction generation must be positive"
        );
        let base_route = normalize_base_route(&base_route.into())?;
        Ok(Self {
            schema_version: NATIVE_COMPACTION_SCHEMA_VERSION,
            session_id,
            provider,
            base_route,
            model,
            account_scope,
            auth_epoch,
            compaction_generation,
            transcript_revision,
        })
    }

    pub fn try_for_route(
        session_id: impl Into<String>,
        route: &NativeCompactionRoute,
        compaction_generation: u64,
        transcript_revision: u64,
    ) -> anyhow::Result<Self> {
        Self::try_new(
            session_id,
            route.provider(),
            route.base_route(),
            route.model(),
            route.account_scope(),
            route.auth_epoch(),
            compaction_generation,
            transcript_revision,
        )
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn base_route(&self) -> &str {
        &self.base_route
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn account_scope(&self) -> &str {
        &self.account_scope
    }

    pub fn auth_epoch(&self) -> u64 {
        self.auth_epoch
    }

    pub fn compaction_generation(&self) -> u64 {
        self.compaction_generation
    }

    pub fn transcript_revision(&self) -> u64 {
        self.transcript_revision
    }

    /// Strict replay guard. A native item is never portable across a provider,
    /// endpoint family, model, or replaced account merely because its JSON
    /// shape happens to be accepted there.
    pub fn matches_route(
        &self,
        session_id: &str,
        provider: &str,
        base_route: &str,
        model: &str,
        account_scope: &str,
        auth_epoch: u64,
    ) -> bool {
        normalize_base_route(base_route).is_ok_and(|route| {
            self.session_id == session_id
                && self.provider == provider
                && self.base_route == route
                && self.model == model
                && self.account_scope == account_scope
                && self.auth_epoch == auth_epoch
        })
    }
}

impl fmt::Debug for NativeCompactionProvenance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionProvenance")
            .field("schema_version", &self.schema_version)
            .field("session_id", &self.session_id)
            .field("provider", &self.provider)
            .field("base_route", &"<redacted-route>")
            .field("model", &self.model)
            .field("account_scope", &"<redacted-account-scope>")
            .field("auth_epoch", &self.auth_epoch)
            .field("compaction_generation", &self.compaction_generation)
            .field("transcript_revision", &self.transcript_revision)
            .finish()
    }
}

#[derive(Deserialize)]
struct NativeCompactionProvenanceWire {
    schema_version: u32,
    session_id: String,
    provider: String,
    base_route: String,
    model: String,
    account_scope: String,
    auth_epoch: u64,
    compaction_generation: u64,
    transcript_revision: u64,
}

impl<'de> Deserialize<'de> for NativeCompactionProvenance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = NativeCompactionProvenanceWire::deserialize(deserializer)?;
        if wire.schema_version != NATIVE_COMPACTION_SCHEMA_VERSION {
            return Err(D::Error::custom(format!(
                "unsupported native compaction schema version {}",
                wire.schema_version
            )));
        }
        Self::try_new(
            wire.session_id,
            wire.provider,
            wire.base_route,
            wire.model,
            wire.account_scope,
            wire.auth_epoch,
            wire.compaction_generation,
            wire.transcript_revision,
        )
        .map_err(D::Error::custom)
    }
}

/// Bounded provider-native replay prefix persisted beside portable history.
///
/// The JSON items are intentionally opaque. Access is named explicitly for
/// wire serialization, and `Debug` exposes only counts/bytes so encrypted
/// content cannot leak through ordinary request/session logging.
#[derive(Clone, Serialize, PartialEq)]
pub struct NativeCompactionReplay {
    provenance: NativeCompactionProvenance,
    capability: NativeCompactionCapability,
    portable_suffix_start: usize,
    items: Vec<serde_json::Value>,
}

impl NativeCompactionReplay {
    pub fn try_new(
        provenance: NativeCompactionProvenance,
        capability: NativeCompactionCapability,
        portable_suffix_start: usize,
        items: Vec<serde_json::Value>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            portable_suffix_start > 0
                && portable_suffix_start <= MAX_NATIVE_COMPACTION_INPUT_MESSAGES,
            "native compaction portable suffix boundary must be 1..={MAX_NATIVE_COMPACTION_INPUT_MESSAGES}"
        );
        validate_replay_items(capability, &items)?;
        Ok(Self {
            provenance,
            capability,
            portable_suffix_start,
            items,
        })
    }

    pub fn provenance(&self) -> &NativeCompactionProvenance {
        &self.provenance
    }

    pub fn capability(&self) -> NativeCompactionCapability {
        self.capability
    }

    /// Index in the post-compaction portable session transcript where messages
    /// not covered by the opaque replay begin. The portable prefix before this
    /// boundary is the provider-neutral recovery summary and must not be sent
    /// alongside native replay, or the model would see the same history twice.
    pub fn portable_suffix_start(&self) -> usize {
        self.portable_suffix_start
    }

    /// Opaque provider payload for a matching adapter to copy verbatim onto the
    /// wire. Never parse, edit, reorder, merge, or log these items.
    pub fn opaque_items_for_wire(&self) -> &[serde_json::Value] {
        &self.items
    }

    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn payload_bytes(&self) -> usize {
        serde_json::to_vec(&self.items).map_or(MAX_NATIVE_COMPACTION_REPLAY_BYTES, |v| v.len())
    }

    pub fn matches_route(
        &self,
        capability: NativeCompactionCapability,
        session_id: &str,
        provider: &str,
        base_route: &str,
        model: &str,
        account_scope: &str,
        auth_epoch: u64,
    ) -> bool {
        self.capability == capability
            && self.provenance.matches_route(
                session_id,
                provider,
                base_route,
                model,
                account_scope,
                auth_epoch,
            )
    }
}

impl fmt::Debug for NativeCompactionReplay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionReplay")
            .field("provenance", &self.provenance)
            .field("capability", &self.capability)
            .field("portable_suffix_start", &self.portable_suffix_start)
            .field("item_count", &self.items.len())
            .field("payload", &"<opaque>")
            .finish()
    }
}

#[derive(Deserialize)]
struct NativeCompactionReplayWire {
    provenance: NativeCompactionProvenance,
    capability: NativeCompactionCapability,
    portable_suffix_start: usize,
    items: Vec<serde_json::Value>,
}

impl<'de> Deserialize<'de> for NativeCompactionReplay {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = NativeCompactionReplayWire::deserialize(deserializer)?;
        Self::try_new(
            wire.provenance,
            wire.capability,
            wire.portable_suffix_start,
            wire.items,
        )
        .map_err(D::Error::custom)
    }
}

/// Dual-view envelope for one normal completion after native compaction.
///
/// `CompletionRequest.messages` always retains the complete provider-neutral
/// prompt for fallback. `native_messages` is a separately prebuilt prompt view
/// whose durable-session portion starts at the replay's
/// `portable_suffix_start()`. A matching native adapter must use this view as a
/// whole; it must never prepend replay to the full portable messages or merge
/// the two views, either of which would duplicate compacted history.
#[derive(Clone, Serialize)]
pub struct NativeCompactionReplayInput {
    replay: NativeCompactionReplay,
    native_messages: Vec<ChatMessage>,
}

impl NativeCompactionReplayInput {
    pub fn try_new(
        replay: NativeCompactionReplay,
        native_messages: Vec<ChatMessage>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !native_messages.is_empty(),
            "native compaction replay input is empty"
        );
        anyhow::ensure!(
            native_messages.len() <= MAX_NATIVE_COMPACTION_INPUT_MESSAGES,
            "native compaction replay input has {} messages; maximum is {}",
            native_messages.len(),
            MAX_NATIVE_COMPACTION_INPUT_MESSAGES
        );
        let input_bytes = serialized_bytes(&(&replay, &native_messages))?;
        anyhow::ensure!(
            input_bytes <= MAX_NATIVE_COMPACTION_INPUT_BYTES,
            "native compaction replay input is {input_bytes} bytes; maximum is {MAX_NATIVE_COMPACTION_INPUT_BYTES}"
        );
        Ok(Self {
            replay,
            native_messages,
        })
    }

    pub fn replay(&self) -> &NativeCompactionReplay {
        &self.replay
    }

    /// Fully assembled provider message view to use with the opaque prefix.
    /// Do not append or merge `CompletionRequest.messages` into this slice.
    pub fn native_messages_for_wire(&self) -> &[ChatMessage] {
        &self.native_messages
    }

    pub fn matches_route(
        &self,
        capability: NativeCompactionCapability,
        session_id: &str,
        provider: &str,
        base_route: &str,
        model: &str,
        account_scope: &str,
        auth_epoch: u64,
    ) -> bool {
        self.replay.matches_route(
            capability,
            session_id,
            provider,
            base_route,
            model,
            account_scope,
            auth_epoch,
        )
    }

    pub fn into_parts(self) -> (NativeCompactionReplay, Vec<ChatMessage>) {
        (self.replay, self.native_messages)
    }
}

impl fmt::Debug for NativeCompactionReplayInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionReplayInput")
            .field("replay", &self.replay)
            .field("native_message_count", &self.native_messages.len())
            .field("native_messages", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct NativeCompactionReplayInputWire {
    replay: NativeCompactionReplay,
    native_messages: Vec<ChatMessage>,
}

impl<'de> Deserialize<'de> for NativeCompactionReplayInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = NativeCompactionReplayInputWire::deserialize(deserializer)?;
        Self::try_new(wire.replay, wire.native_messages).map_err(D::Error::custom)
    }
}

/// One bounded native compaction attempt over the logical prefix selected by
/// the runtime. Fields are private so every adapter receives a validated input.
#[derive(Clone)]
pub struct NativeCompactionRequest {
    route: NativeCompactionRoute,
    instructions: String,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    prior_replay: Option<NativeCompactionReplay>,
    session_id: String,
    source_transcript_revision: u64,
    trigger_tokens: u64,
    next_compaction_generation: u64,
}

impl NativeCompactionRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        route: NativeCompactionRoute,
        instructions: impl Into<String>,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        prior_replay: Option<NativeCompactionReplay>,
        session_id: impl Into<String>,
        source_transcript_revision: u64,
        trigger_tokens: u64,
        next_compaction_generation: u64,
    ) -> anyhow::Result<Self> {
        let instructions = instructions.into();
        let session_id = session_id.into();
        validate_session_id(&session_id)?;
        anyhow::ensure!(
            instructions.len() <= MAX_NATIVE_COMPACTION_INSTRUCTIONS_BYTES,
            "native compaction instructions are {} bytes; maximum is {}",
            instructions.len(),
            MAX_NATIVE_COMPACTION_INSTRUCTIONS_BYTES
        );
        anyhow::ensure!(
            messages.len() <= MAX_NATIVE_COMPACTION_INPUT_MESSAGES,
            "native compaction input has {} messages; maximum is {}",
            messages.len(),
            MAX_NATIVE_COMPACTION_INPUT_MESSAGES
        );
        anyhow::ensure!(
            tools.len() <= MAX_NATIVE_COMPACTION_TOOLS,
            "native compaction input has {} tools; maximum is {}",
            tools.len(),
            MAX_NATIVE_COMPACTION_TOOLS
        );
        anyhow::ensure!(!messages.is_empty(), "native compaction input is empty");
        anyhow::ensure!(
            trigger_tokens > 0,
            "native compaction trigger must be positive"
        );
        anyhow::ensure!(
            next_compaction_generation > 0,
            "native compaction generation must be positive"
        );
        source_transcript_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("native compaction transcript revision overflow"))?;
        if let Some(previous) = &prior_replay {
            anyhow::ensure!(
                previous.matches_route(
                    route.capability(),
                    &session_id,
                    route.provider(),
                    route.base_route(),
                    route.model(),
                    route.account_scope(),
                    route.auth_epoch(),
                ),
                "native compaction replay does not match the requested route"
            );
            anyhow::ensure!(
                previous.provenance().transcript_revision() <= source_transcript_revision,
                "native compaction replay is newer than the source transcript"
            );
            let expected = previous
                .provenance()
                .compaction_generation()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("native compaction generation overflow"))?;
            anyhow::ensure!(
                next_compaction_generation == expected,
                "native compaction generation is {}, expected {}",
                next_compaction_generation,
                expected
            );
        } else {
            anyhow::ensure!(
                next_compaction_generation == 1,
                "first native compaction request generation must be 1"
            );
        }
        let input_bytes = serialized_bytes(&(
            &route,
            &instructions,
            &messages,
            &tools,
            &prior_replay,
            &session_id,
        ))?;
        anyhow::ensure!(
            input_bytes <= MAX_NATIVE_COMPACTION_INPUT_BYTES,
            "native compaction input is {input_bytes} bytes; maximum is {MAX_NATIVE_COMPACTION_INPUT_BYTES}"
        );
        Ok(Self {
            route,
            instructions,
            messages,
            tools,
            prior_replay,
            session_id,
            source_transcript_revision,
            trigger_tokens,
            next_compaction_generation,
        })
    }

    pub fn model(&self) -> &str {
        self.route.model()
    }

    pub fn route(&self) -> &NativeCompactionRoute {
        &self.route
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn prior_replay(&self) -> Option<&NativeCompactionReplay> {
        self.prior_replay.as_ref()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn source_transcript_revision(&self) -> u64 {
        self.source_transcript_revision
    }

    pub fn target_transcript_revision(&self) -> u64 {
        // Checked by `try_new`.
        self.source_transcript_revision + 1
    }

    pub fn trigger_tokens(&self) -> u64 {
        self.trigger_tokens
    }

    pub fn next_compaction_generation(&self) -> u64 {
        self.next_compaction_generation
    }
}

impl fmt::Debug for NativeCompactionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionRequest")
            .field("route", &self.route)
            .field("instructions_bytes", &self.instructions.len())
            .field("message_count", &self.messages.len())
            .field("tool_count", &self.tools.len())
            .field("has_prior_replay", &self.prior_replay.is_some())
            .field("session_id", &self.session_id)
            .field(
                "source_transcript_revision",
                &self.source_transcript_revision,
            )
            .field("trigger_tokens", &self.trigger_tokens)
            .field(
                "next_compaction_generation",
                &self.next_compaction_generation,
            )
            .finish()
    }
}

/// Validated result returned by a provider-native compaction adapter.
#[derive(Clone)]
pub struct NativeCompactionResult {
    replay: NativeCompactionReplay,
    portable_summary: Option<String>,
    usage: TokenUsage,
    folded_input_messages: usize,
}

impl NativeCompactionResult {
    pub fn try_new(
        request: &NativeCompactionRequest,
        replay: NativeCompactionReplay,
        portable_summary: Option<String>,
        usage: TokenUsage,
        folded_input_messages: usize,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            folded_input_messages > 0 && folded_input_messages <= request.messages().len(),
            "native compaction folded-message count must be 1..={} (got {})",
            request.messages().len(),
            folded_input_messages
        );
        anyhow::ensure!(
            replay.matches_route(
                request.route().capability(),
                request.session_id(),
                request.route().provider(),
                request.route().base_route(),
                request.route().model(),
                request.route().account_scope(),
                request.route().auth_epoch(),
            ),
            "native compaction result does not match the requested route"
        );
        anyhow::ensure!(
            replay.provenance().compaction_generation() == request.next_compaction_generation(),
            "native compaction result generation does not match the request"
        );
        anyhow::ensure!(
            replay.provenance().transcript_revision() == request.target_transcript_revision(),
            "native compaction result transcript revision does not match the request"
        );
        if let Some(summary) = &portable_summary {
            anyhow::ensure!(
                !summary.trim().is_empty(),
                "native compaction portable summary is empty"
            );
            anyhow::ensure!(
                summary.len() <= MAX_NATIVE_COMPACTION_PORTABLE_SUMMARY_BYTES,
                "native compaction portable summary is {} bytes; maximum is {}",
                summary.len(),
                MAX_NATIVE_COMPACTION_PORTABLE_SUMMARY_BYTES
            );
        }
        Ok(Self {
            replay,
            portable_summary,
            usage,
            folded_input_messages,
        })
    }

    pub fn replay(&self) -> &NativeCompactionReplay {
        &self.replay
    }

    pub fn into_replay(self) -> NativeCompactionReplay {
        self.replay
    }

    pub fn into_parts(self) -> (NativeCompactionReplay, Option<String>, TokenUsage, usize) {
        (
            self.replay,
            self.portable_summary,
            self.usage,
            self.folded_input_messages,
        )
    }

    pub fn portable_summary(&self) -> Option<&str> {
        self.portable_summary.as_deref()
    }

    pub fn usage(&self) -> &TokenUsage {
        &self.usage
    }

    pub fn folded_input_messages(&self) -> usize {
        self.folded_input_messages
    }
}

impl fmt::Debug for NativeCompactionResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompactionResult")
            .field("replay", &self.replay)
            .field(
                "portable_summary_bytes",
                &self.portable_summary.as_ref().map(String::len),
            )
            .field("usage", &self.usage)
            .field("folded_input_messages", &self.folded_input_messages)
            .finish()
    }
}

fn validate_replay_items(
    capability: NativeCompactionCapability,
    items: &[serde_json::Value],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        capability.is_supported(),
        "unsupported providers cannot create native compaction replay state"
    );
    anyhow::ensure!(!items.is_empty(), "native compaction replay is empty");
    anyhow::ensure!(
        items.len() <= MAX_NATIVE_COMPACTION_REPLAY_ITEMS,
        "native compaction replay has {} items; maximum is {}",
        items.len(),
        MAX_NATIVE_COMPACTION_REPLAY_ITEMS
    );
    let mut saw_compaction = false;
    for item in items {
        anyhow::ensure!(
            item.is_object(),
            "native compaction replay items must be JSON objects"
        );
        let item_type = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        anyhow::ensure!(
            !item_type.is_empty(),
            "native compaction replay item is missing a non-empty type"
        );
        let item_bytes = serialized_bytes(item)?;
        anyhow::ensure!(
            item_bytes <= MAX_NATIVE_COMPACTION_REPLAY_ITEM_BYTES,
            "native compaction replay item is {item_bytes} bytes; maximum is {MAX_NATIVE_COMPACTION_REPLAY_ITEM_BYTES}"
        );
        // The public Responses documentation calls this a `compaction` item,
        // while the first-party ChatGPT Codex route currently returns the
        // same opaque replay payload as `compaction_summary`.  Both are
        // canonical provider output and must be retained byte-for-byte.
        let is_compaction_item = item_type == "compaction"
            || (capability == NativeCompactionCapability::ResponsesCompact
                && item_type == "compaction_summary");
        if is_compaction_item {
            saw_compaction = true;
            match capability {
                NativeCompactionCapability::ResponsesCompact => {
                    let encrypted = item
                        .get("encrypted_content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    anyhow::ensure!(
                        !encrypted.is_empty(),
                        "Responses compaction replay is missing encrypted_content"
                    );
                }
                NativeCompactionCapability::AnthropicCompact => {
                    let content = item
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    anyhow::ensure!(
                        !content.trim().is_empty(),
                        "Anthropic compaction replay is missing summary content"
                    );
                }
                NativeCompactionCapability::Unsupported => unreachable!(),
            }
        }
    }
    anyhow::ensure!(
        saw_compaction,
        "native compaction replay has no compaction item"
    );
    if capability == NativeCompactionCapability::AnthropicCompact {
        anyhow::ensure!(
            items.len() == 1,
            "paused Anthropic compaction replay must contain exactly one compaction block"
        );
    }
    let total_bytes = serialized_bytes(items)?;
    anyhow::ensure!(
        total_bytes <= MAX_NATIVE_COMPACTION_REPLAY_BYTES,
        "native compaction replay is {total_bytes} bytes; maximum is {MAX_NATIVE_COMPACTION_REPLAY_BYTES}"
    );
    Ok(())
}

fn serialized_bytes<T: Serialize + ?Sized>(value: &T) -> anyhow::Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(Into::into)
}

fn validate_text_id(label: &str, value: &str, max_bytes: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= max_bytes,
        "{label} must be 1..={max_bytes} bytes"
    );
    anyhow::ensure!(
        value.trim() == value && !value.chars().any(char::is_control),
        "{label} contains surrounding whitespace or control characters"
    );
    Ok(())
}

fn validate_session_id(session_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !session_id.is_empty() && session_id.len() <= MAX_NATIVE_SESSION_ID_BYTES,
        "native compaction session id must be 1..={MAX_NATIVE_SESSION_ID_BYTES} bytes"
    );
    anyhow::ensure!(
        session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "native compaction session id contains unsupported characters"
    );
    Ok(())
}

fn validate_route_label(
    label: &str,
    value: &str,
    max_bytes: usize,
    valid_byte: impl Fn(u8) -> bool,
) -> anyhow::Result<()> {
    validate_text_id(label, value, max_bytes)?;
    anyhow::ensure!(
        value.bytes().all(valid_byte),
        "{label} contains unsupported characters"
    );
    Ok(())
}

fn normalize_base_route(raw: &str) -> anyhow::Result<String> {
    validate_text_id(
        "native compaction base route",
        raw,
        MAX_NATIVE_BASE_ROUTE_BYTES,
    )?;
    let mut parsed = url::Url::parse(raw).map_err(|_| {
        anyhow::anyhow!("native compaction base route must be an absolute HTTP(S) URL")
    })?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "native compaction base route must use HTTP(S)"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "native compaction base route must not contain credentials"
    );
    anyhow::ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "native compaction base route must not contain a query or fragment"
    );
    anyhow::ensure!(
        parsed.host_str().is_some(),
        "native compaction base route must contain a host"
    );
    let trimmed_path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(if trimmed_path.is_empty() {
        "/"
    } else {
        &trimmed_path
    });
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    fn name(&self) -> &str;
    fn display_name(&self) -> &str;
    fn base_url(&self) -> &str;
    fn auth_type(&self) -> AuthType;
    fn env_vars(&self) -> Vec<&str>;
    fn default_headers(&self) -> HashMap<String, String>;

    fn has_model(&self, model: &str) -> bool;
    fn default_model(&self) -> &str;
    fn fallback_models(&self) -> Vec<&str>;

    /// The usable input window for this concrete provider/model route.
    /// Callers use this instead of assuming every account in a fallback chain
    /// has the primary model's window.
    fn context_window(&self, model: &str) -> Option<u64> {
        crate::providers::providers_data::context_window_for(self.name(), model)
    }

    /// Provider-native semantic compaction available for this exact model.
    /// Capability is model-specific: for example, Anthropic's beta is not
    /// available on every Claude model exposed by the provider catalog.
    fn native_compaction_capability(&self, _model: &str) -> NativeCompactionCapability {
        NativeCompactionCapability::Unsupported
    }

    /// Exact provider/account/model route that may create or consume opaque
    /// replay state. Raw adapters return `None`; the factory's identity wrapper
    /// supplies this only when the adapter reports a native capability.
    /// Fallback providers must return a concrete candidate route, never their
    /// first-link `name()`/`base_url()` aliases.
    fn native_compaction_route(&self, _model: &str) -> Option<NativeCompactionRoute> {
        None
    }

    /// Wire dialect used by ordinary completion requests. Identity-free raw
    /// adapters may use the generic default; adapters capable of native replay
    /// override it so response receipts can be compared to replay provenance.
    fn response_dialect(&self, _model: &str) -> ProviderResponseDialect {
        ProviderResponseDialect::Generic
    }

    /// Exact non-secret route receipt this provider would stamp for `model`.
    /// Raw adapters have no account identity and therefore return `None`; the
    /// factory wrapper supplies it and fallback providers forward the concrete
    /// candidate's receipt.
    fn response_route(&self, _model: &str) -> Option<ProviderResponseRoute> {
        None
    }

    /// Replace an older logical prefix with a provider-native replay item.
    ///
    /// Default behavior is explicitly unsupported so adding this contract does
    /// not silently alter any existing provider. Callers must check capability
    /// first and retain portable compaction as the failure path.
    async fn compact_context(
        &self,
        request: NativeCompactionRequest,
    ) -> anyhow::Result<NativeCompactionResult> {
        anyhow::bail!(
            "provider '{}' does not support native compaction for model '{}'",
            self.name(),
            request.model()
        )
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;
    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<StreamingResponse>;

    /// Additive response API carrying the actual successful provider route.
    /// The default preserves every existing provider implementation; identity
    /// wrappers override it with an authoritative receipt.
    async fn complete_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedCompletionResponse> {
        self.complete(request)
            .await
            .map(RoutedCompletionResponse::unrouted)
    }

    /// Streaming counterpart. Phoenix streaming adapters currently return one
    /// terminal aggregate, so the receipt accompanies that terminal result.
    async fn stream_with_route(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<RoutedStreamingResponse> {
        self.stream(request)
            .await
            .map(RoutedStreamingResponse::unrouted)
    }

    async fn embeddings(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>>;
    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>>;
    async fn health_check(&self) -> anyhow::Result<bool>;

    /// Whether this provider serializes `ChatMessage.images` to the wire
    /// (multipart content). False = attached images are silently dropped, so
    /// callers must fall back to text grounding (the caption sidecar).
    fn supports_native_images(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthType {
    ApiKey,
    Bearer,
    XApiKey,
    AwsSdk,
    OAuthExternal,
    OAuthDeviceCode,
    Copilot,
    None,
}

/// Definition of a tool passed to the provider for native tool calling.
///
/// Every agent's allowed tools are converted to this format and included in
/// the `CompletionRequest.tools` list. The provider passes them to the model
/// using the appropriate native format (OpenAI function calling, Anthropic
/// tools, Responses API, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema object for the function parameters.
    pub parameters: serde_json::Value,
}

/// A native tool call requested by the model in a `CompletionResponse`.
///
/// When `finish_reason` is `"tool_calls"` the model has requested one or more
/// tool calls. The runner executes them and injects results as `role: "tool"`
/// messages before the next provider call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeToolCall {
    /// Provider-assigned call ID. Used to correlate tool results back to calls.
    pub id: String,
    /// Name of the tool to invoke.
    pub tool_name: String,
    /// Parsed arguments for the tool call.
    pub arguments: serde_json::Value,
}

/// In-process, request-owned stream delivery. Never serialized into provider
/// wire bodies or persisted prompts; clones (including fallback attempts)
/// retain the same execution destination.
#[derive(Clone)]
pub struct StreamObserver(std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>);

impl std::fmt::Debug for StreamObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamObserver(<execution-owned>)")
    }
}

impl StreamObserver {
    pub fn new(callback: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(callback))
    }

    pub fn emit(&self, kind: &str, text: &str) {
        (self.0)(kind, text);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: bool,
    #[serde(default)]
    pub extra_body: HashMap<String, serde_json::Value>,
    pub session_id: Option<String>,
    #[serde(skip)]
    pub stream_observer: Option<StreamObserver>,
    /// Tools available for native tool calling. Empty = plain completion, no tools.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Optional provider-scoped replay prefix and its separately assembled
    /// native message view. `messages` always remains the complete portable
    /// fallback view. A matching native adapter uses
    /// `native_messages_for_wire()` instead; it must never merge the two views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_replay: Option<NativeCompactionReplayInput>,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            stream: false,
            extra_body: HashMap::new(),
            session_id: None,
            stream_observer: None,
            tools: Vec::new(),
            native_replay: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    /// Tool calls requested in this message. Non-empty only for `role: assistant`
    /// messages where the model requested tool execution.
    #[serde(default)]
    pub tool_calls: Vec<NativeToolCall>,
    /// Inline images (data URIs) attached to this message. Only providers that
    /// report `supports_native_images` serialize these; everyone else drops
    /// them, so callers must gate on that capability before attaching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Opaque provider items that must precede this assistant message on the
    /// wire (Responses `reasoning` items with `encrypted_content`). Without
    /// them a reasoning model forgets every conclusion it reached in hidden
    /// reasoning between tool rounds and re-derives it — the root of the
    /// "inspect the same image again" loops. Route-scoped: only the exact
    /// provider/model/account that minted them may replay them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_replay: Option<ProviderReplay>,
}

/// Provider-minted items replayable only on the route that produced them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderReplay {
    pub route: String,
    pub items: Vec<serde_json::Value>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
            provider_replay: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
            provider_replay: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
            provider_replay: None,
        }
    }

    pub fn assistant_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<NativeToolCall>,
    ) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls,
            images: Vec::new(),
            provider_replay: None,
        }
    }

    /// The assistant message that records a tool-calling response, carrying
    /// its prose and any replayable provider reasoning.
    pub fn assistant_reply(response: &CompletionResponse) -> Self {
        let mut message = Self::assistant_tool_calls(response.content.clone(), response.tool_calls.clone());
        message.provider_replay = response.provider_replay.clone();
        message
    }

    /// User message with inline images (data URIs). Gate on the provider's
    /// `supports_native_images` before using — others drop the images.
    pub fn user_with_images(content: impl Into<String>, images: Vec<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images,
            provider_replay: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            name: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
            images: Vec::new(),
            provider_replay: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
    Developer,
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageRole::System => write!(f, "system"),
            MessageRole::User => write!(f, "user"),
            MessageRole::Assistant => write!(f, "assistant"),
            MessageRole::Tool => write!(f, "tool"),
            MessageRole::Developer => write!(f, "developer"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Text content from the model. Empty when the model only requested tool calls.
    pub content: String,
    pub model: String,
    pub usage: TokenUsage,
    pub reasoning: Option<String>,
    pub stop_reason: Option<String>,
    /// Native tool calls requested by the model.
    /// Non-empty when `stop_reason` is `"tool_calls"`. Empty on final text responses.
    #[serde(default)]
    pub tool_calls: Vec<NativeToolCall>,
    /// Opaque items to attach to the assistant message built from this
    /// response (see [`ChatMessage::provider_replay`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_replay: Option<ProviderReplay>,
}

#[derive(Debug, Clone)]
pub struct RoutedCompletionResponse {
    response: CompletionResponse,
    actual_route: Option<ProviderResponseRoute>,
}

impl RoutedCompletionResponse {
    pub fn new(response: CompletionResponse, actual_route: Option<ProviderResponseRoute>) -> Self {
        Self {
            response,
            actual_route,
        }
    }

    pub fn unrouted(response: CompletionResponse) -> Self {
        Self::new(response, None)
    }

    pub fn response(&self) -> &CompletionResponse {
        &self.response
    }

    pub fn actual_route(&self) -> Option<&ProviderResponseRoute> {
        self.actual_route.as_ref()
    }

    pub fn into_parts(self) -> (CompletionResponse, Option<ProviderResponseRoute>) {
        (self.response, self.actual_route)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Prompt-cache telemetry (providers that report it): tokens written to
    /// the cache this call vs read back at discount. Over a session, both
    /// nonzero = caching is live; creation-only = the prefix is being
    /// rewritten every call (a silent cache leak).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
}

impl TokenUsage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_creation_tokens: None,
            cache_read_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: Option<u64>,
    pub context_window: Option<u32>,
    pub input_cost_per_token: Option<f32>,
    pub output_cost_per_token: Option<f32>,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct RoutedStreamingResponse {
    response: StreamingResponse,
    actual_route: Option<ProviderResponseRoute>,
}

impl RoutedStreamingResponse {
    pub fn new(response: StreamingResponse, actual_route: Option<ProviderResponseRoute>) -> Self {
        Self {
            response,
            actual_route,
        }
    }

    pub fn unrouted(response: StreamingResponse) -> Self {
        Self::new(response, None)
    }

    pub fn response(&self) -> &StreamingResponse {
        &self.response
    }

    pub fn actual_route(&self) -> Option<&ProviderResponseRoute> {
        self.actual_route.as_ref()
    }

    pub fn into_parts(self) -> (StreamingResponse, Option<ProviderResponseRoute>) {
        (self.response, self.actual_route)
    }
}
