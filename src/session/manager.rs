//! Session lifecycle management

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::providers::contracts::{
    NativeCompactionCapability, NativeCompactionReplay, NativeCompactionRoute,
};

/// Session ids cross authenticated IPC and become filenames. Keep the syntax
/// deliberately narrower than a generic path component so absolute paths,
/// separators, dot components, Unicode lookalikes, and control bytes are all
/// rejected by one rule everywhere.
pub const MAX_SESSION_ID_BYTES: usize = 192;
const MAX_SESSION_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SESSION_FILE_COUNT: usize = 2_048;
const MAX_SESSION_AGGREGATE_BYTES: u64 = 256 * 1024 * 1024;
const CAS_VERSION: u32 = 1;
const CAS_ENTRY_SCAN_CAP: usize = 16_384;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TranscriptCasObject {
    version: u32,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TranscriptCasManifest {
    version: u32,
    session_id: String,
    transcript_revision: u64,
    metadata: String,
    messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TranscriptCasRef {
    version: u32,
    session_id: String,
    transcript_revision: u64,
    manifest: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn transcript_cas_root(session_root: &Path) -> PathBuf {
    if session_root.file_name().and_then(|name| name.to_str()) == Some("sessions") {
        session_root
            .parent()
            .unwrap_or(session_root)
            .join("cas/transcripts")
    } else {
        session_root.join(".cas/transcripts")
    }
}

fn transcript_cas_namespace(session_root: &Path, session_id: &str) -> PathBuf {
    transcript_cas_root(session_root).join(sha256_hex(session_id.as_bytes()))
}

fn transcript_cas_digest_path(base: &Path, digest: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_sha256_hex(digest), "invalid transcript CAS digest");
    Ok(base
        .join("sha256")
        .join(&digest[..2])
        .join(format!("{digest}.json")))
}

fn write_transcript_cas_blob(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let expected = path
        .file_stem()
        .and_then(|name| name.to_str())
        .context("transcript CAS path has no digest")?;
    anyhow::ensure!(
        sha256_hex(bytes) == expected,
        "transcript CAS digest mismatch"
    );
    if !crate::config::private_io::atomic_write_private_if_missing(path, bytes)? {
        let existing = crate::config::private_io::read_private_file_limited(
            path,
            usize::try_from(MAX_SESSION_FILE_BYTES).unwrap_or(usize::MAX),
        )?
        .context("transcript CAS object disappeared after create collision")?;
        anyhow::ensure!(
            existing == bytes,
            "existing transcript CAS object does not match its digest"
        );
    }
    Ok(())
}

fn validate_session_id_value(session_id: &str) -> anyhow::Result<()> {
    if session_id.is_empty() {
        anyhow::bail!("session id is empty");
    }
    if session_id.len() > MAX_SESSION_ID_BYTES {
        anyhow::bail!(
            "session id is {} bytes; maximum is {MAX_SESSION_ID_BYTES}",
            session_id.len()
        );
    }
    if !session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        anyhow::bail!("session id may contain only ASCII letters, digits, '_' and '-'");
    }
    Ok(())
}

/// A session is either the main orchestrator or a sub-agent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Main,
    SubAgent(SubAgentType),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "SubAgentTypeRepr", into = "SubAgentTypeRepr")]
pub enum SubAgentType {
    Coder,
    Researcher,
    Browser,
    Frontend,
    Database,
    Hacker,
    Presentation,
    Finance,
    /// Historical execution-only lane. New companies do not expose this as a
    /// coworker; computer-use tools are universal infrastructure.
    ComputerUse,
    Critic,
    Tester,
    Planner,
    Scribe,
    Sales,
    Marketing,
    PersonalLogistics,
    /// A runtime-defined agent from `~/.phoenix/agents/<role>/` (plan 019).
    /// The u16 indexes the process-wide intern table (`custom_agent_label`);
    /// names are leaked once so every `&'static str` signature keeps working.
    Custom(u16),
}

/// Serde shape: built-ins keep the derived unit-variant strings ("Coder") so
/// every existing session file loads unchanged; Custom round-trips through
/// its role name and re-interns on load.
#[derive(Serialize, Deserialize)]
enum SubAgentTypeRepr {
    Coder,
    Researcher,
    Browser,
    Frontend,
    Database,
    Hacker,
    Presentation,
    Finance,
    ComputerUse,
    Critic,
    Tester,
    Planner,
    Scribe,
    Sales,
    Marketing,
    PersonalLogistics,
    Custom(String),
}

impl From<SubAgentTypeRepr> for SubAgentType {
    fn from(repr: SubAgentTypeRepr) -> Self {
        match repr {
            SubAgentTypeRepr::Coder => SubAgentType::Coder,
            SubAgentTypeRepr::Researcher => SubAgentType::Researcher,
            SubAgentTypeRepr::Browser => SubAgentType::Browser,
            SubAgentTypeRepr::Frontend => SubAgentType::Frontend,
            SubAgentTypeRepr::Database => SubAgentType::Database,
            SubAgentTypeRepr::Hacker => SubAgentType::Hacker,
            SubAgentTypeRepr::Presentation => SubAgentType::Presentation,
            SubAgentTypeRepr::Finance => SubAgentType::Finance,
            SubAgentTypeRepr::ComputerUse => SubAgentType::ComputerUse,
            SubAgentTypeRepr::Critic => SubAgentType::Critic,
            SubAgentTypeRepr::Tester => SubAgentType::Tester,
            SubAgentTypeRepr::Planner => SubAgentType::Planner,
            SubAgentTypeRepr::Scribe => SubAgentType::Scribe,
            SubAgentTypeRepr::Sales => SubAgentType::Sales,
            SubAgentTypeRepr::Marketing => SubAgentType::Marketing,
            SubAgentTypeRepr::PersonalLogistics => SubAgentType::PersonalLogistics,
            SubAgentTypeRepr::Custom(name) => SubAgentType::custom(&name),
        }
    }
}

impl From<SubAgentType> for SubAgentTypeRepr {
    fn from(agent: SubAgentType) -> Self {
        match agent {
            SubAgentType::Coder => SubAgentTypeRepr::Coder,
            SubAgentType::Researcher => SubAgentTypeRepr::Researcher,
            SubAgentType::Browser => SubAgentTypeRepr::Browser,
            SubAgentType::Frontend => SubAgentTypeRepr::Frontend,
            SubAgentType::Database => SubAgentTypeRepr::Database,
            SubAgentType::Hacker => SubAgentTypeRepr::Hacker,
            SubAgentType::Presentation => SubAgentTypeRepr::Presentation,
            SubAgentType::Finance => SubAgentTypeRepr::Finance,
            SubAgentType::ComputerUse => SubAgentTypeRepr::ComputerUse,
            SubAgentType::Critic => SubAgentTypeRepr::Critic,
            SubAgentType::Tester => SubAgentTypeRepr::Tester,
            SubAgentType::Planner => SubAgentTypeRepr::Planner,
            SubAgentType::Scribe => SubAgentTypeRepr::Scribe,
            SubAgentType::Sales => SubAgentTypeRepr::Sales,
            SubAgentType::Marketing => SubAgentTypeRepr::Marketing,
            SubAgentType::PersonalLogistics => SubAgentTypeRepr::PersonalLogistics,
            SubAgentType::Custom(id) => {
                SubAgentTypeRepr::Custom(custom_agent_label(id).to_string())
            }
        }
    }
}

/// Process-wide intern table for custom-agent role names. Names are leaked
/// once (custom agents are few and live for the process lifetime), which lets
/// `specialist_label` keep returning `&'static str` for every agent kind.
static CUSTOM_AGENT_NAMES: std::sync::OnceLock<std::sync::RwLock<Vec<&'static str>>> =
    std::sync::OnceLock::new();

fn custom_names() -> &'static std::sync::RwLock<Vec<&'static str>> {
    CUSTOM_AGENT_NAMES.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}

/// The interned role name for a custom agent id.
pub fn custom_agent_label(id: u16) -> &'static str {
    custom_names()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(id as usize)
        .copied()
        .unwrap_or("custom")
}

/// Every interned custom role name, in intern order.
pub fn custom_agent_labels() -> Vec<&'static str> {
    custom_names()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

impl SubAgentType {
    /// Intern (or find) a custom agent by role name. Normalizes to lowercase
    /// snake_case so talk names, session files, and dir names agree.
    pub fn custom(name: &str) -> Self {
        let normalized = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        {
            let table = custom_names().read().unwrap_or_else(|p| p.into_inner());
            if let Some(index) = table.iter().position(|n| *n == normalized) {
                return SubAgentType::Custom(index as u16);
            }
        }
        let mut table = custom_names().write().unwrap_or_else(|p| p.into_inner());
        // Re-check under the write lock (another thread may have interned it).
        if let Some(index) = table.iter().position(|n| *n == normalized) {
            return SubAgentType::Custom(index as u16);
        }
        let leaked: &'static str = Box::leak(normalized.into_boxed_str());
        table.push(leaked);
        SubAgentType::Custom((table.len() - 1) as u16)
    }

    /// Find an already-interned custom agent by role name (never interns —
    /// use for resolving talk targets, where unknown names must stay unknown).
    pub fn find_custom(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        let table = custom_names().read().unwrap_or_else(|p| p.into_inner());
        table
            .iter()
            .position(|n| *n == normalized)
            .map(|index| SubAgentType::Custom(index as u16))
    }
}

impl std::fmt::Display for SubAgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubAgentType::Coder => write!(f, "Coder"),
            SubAgentType::Researcher => write!(f, "Researcher"),
            SubAgentType::Browser => write!(f, "Browser"),
            SubAgentType::Frontend => write!(f, "Frontend"),
            SubAgentType::Database => write!(f, "Database"),
            SubAgentType::Hacker => write!(f, "Hacker"),
            SubAgentType::Presentation => write!(f, "Presentation"),
            SubAgentType::Finance => write!(f, "Finance"),
            SubAgentType::ComputerUse => write!(f, "ComputerUse"),
            SubAgentType::Critic => write!(f, "Critic"),
            SubAgentType::Tester => write!(f, "Tester"),
            SubAgentType::Planner => write!(f, "Planner"),
            SubAgentType::Scribe => write!(f, "Scribe"),
            SubAgentType::Sales => write!(f, "Sales"),
            SubAgentType::Marketing => write!(f, "Marketing"),
            SubAgentType::PersonalLogistics => write!(f, "PersonalLogistics"),
            SubAgentType::Custom(id) => {
                // "trader" → "Trader" for display parity with built-ins.
                let label = custom_agent_label(*id);
                let mut chars = label.chars();
                match chars.next() {
                    Some(first) => {
                        write!(f, "{}{}", first.to_ascii_uppercase(), chars.as_str())
                    }
                    None => write!(f, "Custom"),
                }
            }
        }
    }
}

/// A memory file the librarian pinned into the session: its content is rendered
/// into the prompt every turn (durable — immune to windowing/compaction because
/// it is NOT a transcript message), so the agent carries it forward instead of
/// the librarian re-reading the same file each turn. `hash` is of the source
/// file's content, so an edit on disk can be detected and the copy refreshed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedMemory {
    pub path: String,
    pub hash: String,
    pub content: String,
}

/// A reference library the agent loaded via `design_reference`/`skill`.
/// Pinned = re-materialized fresh from its SOURCE (embedded library / skills
/// dir) into every prompt assembly (`runtime/prompt.rs`), so the content
/// survives tool-result pruning, mid-turn aging, and compaction folds.
/// Reference material is a standing constraint, not tool exhaust — the
/// transcript copy may be shredded freely once the name is registered here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinnedRef {
    /// Tool that loaded it: `design_reference` or `skill`.
    pub tool: String,
    /// Identity: canonical library path (design_reference) or skill name (skill).
    pub key: String,
}

/// The pin identity for a reference-class tool result, if it is one.
/// design_reference: the alias-collapsed library path; skill: the skill name
/// (manifest loads only — a `file:` sub-read is browsing, not adoption).
/// Index/list calls pin nothing.
pub fn reference_pin_key(tool_name: &str, input_json: &str) -> Option<String> {
    let input: serde_json::Value = serde_json::from_str(input_json).ok()?;
    match tool_name {
        "design_reference" => {
            let path = input.get("path")?.as_str()?.trim();
            if path.is_empty() || path == "index" {
                return None;
            }
            Some(crate::tools::design_refs::canonical_path(path))
        }
        "skill" => {
            let name = input.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            match input.get("file").and_then(|f| f.as_str()).map(str::trim) {
                None | Some("") | Some("SKILL.md") => Some(name.to_string()),
                Some(_) => None,
            }
        }
        _ => None,
    }
}

/// A session (either main or sub-agent)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub kind: SessionKind,
    pub model: String,
    pub system_prompt: String,
    pub messages: Vec<super::Message>,
    /// Recent durable company-message receipts already written into this
    /// transcript. This closes the crash window between the atomic session
    /// save and settling the matching SQLite delivery receipt: on restart the
    /// ledger can safely retry without making the coworker act twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub company_message_receipts: Vec<String>,
    /// Nested upstream owners for a suspended mesh assignment. Kept in the
    /// same atomic file as its transcript so a resumed handoff cannot forget
    /// who receives the result. Labels are runtime identities, not prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_owners: Vec<ReplyOwnerFrame>,
    /// Human-readable name, auto-derived from the first user message, so the
    /// global session list reads as `reddit-fable5-research` not `main-<uuid>`.
    /// `#[serde(default)]` → old session files (no title) still load.
    #[serde(default)]
    pub title: Option<String>,
    /// Librarian-pinned memory files, carried across turns (see [`PinnedMemory`]).
    #[serde(default)]
    pub pinned_memory: Vec<PinnedMemory>,
    /// Project folder (cwd) this session runs in. Groups every thread of a
    /// project — across separate mesh conversations — so the project brain can
    /// span them (every coder thread sees all coder threads, etc.). Set at turn
    /// start from the workspace root; `#[serde(default)]` → old files still load.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Reference libraries/skills loaded this session, newest LAST (see
    /// [`PinnedRef`]). Fed by [`Session::push_message`]; rendered by the
    /// prompt assembler. `#[serde(default)]` → old session files still load.
    #[serde(default)]
    pub pinned_refs: Vec<PinnedRef>,
    /// Monotonic logical-history watermark. Unlike `messages.len()`, this does
    /// not move backward when compaction replaces many messages with one.
    /// Legacy sessions start at zero and advance on their next mutation.
    #[serde(default)]
    pub transcript_revision: u64,
    /// Optional provider-native replay accelerator. The portable `messages`
    /// transcript remains authoritative and complete enough to use whenever
    /// this state is absent, invalid, stale, or belongs to another route.
    ///
    /// Invalid/unknown inner schemas deserialize as `None`, so a future or
    /// corrupted opaque blob can never make the portable session unloadable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_native_compaction"
    )]
    pub provider_compaction: Option<NativeCompactionReplay>,
}

fn deserialize_optional_native_compaction<'de, D>(
    deserializer: D,
) -> Result<Option<NativeCompactionReplay>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    // Deliberately discard, do not log, malformed opaque state. The portable
    // transcript is the recovery path and logging the serde error could expose
    // fragments of encrypted provider payloads.
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyOwnerFrame {
    pub owner: String,
    pub handoff_id: String,
}

impl Session {
    pub fn new_main(model: &str, system_prompt: &str) -> Self {
        Self::new_main_with_id(uuid::Uuid::new_v4().to_string(), model, system_prompt)
    }

    pub fn new_main_with_id(id: impl Into<String>, model: &str, system_prompt: &str) -> Self {
        Self {
            id: id.into(),
            kind: SessionKind::Main,
            model: model.to_string(),
            system_prompt: system_prompt.to_string(),
            messages: vec![],
            company_message_receipts: vec![],
            reply_owners: vec![],
            title: None,
            pinned_memory: vec![],
            pinned_refs: vec![],
            workspace: None,
            transcript_revision: 0,
            provider_compaction: None,
        }
    }

    pub fn new_sub_agent(agent_type: SubAgentType, model: &str, system_prompt: &str) -> Self {
        Self::new_sub_agent_with_id(
            uuid::Uuid::new_v4().to_string(),
            agent_type,
            model,
            system_prompt,
        )
    }

    pub fn new_sub_agent_with_id(
        id: impl Into<String>,
        agent_type: SubAgentType,
        model: &str,
        system_prompt: &str,
    ) -> Self {
        Self {
            id: id.into(),
            kind: SessionKind::SubAgent(agent_type),
            model: model.to_string(),
            system_prompt: system_prompt.to_string(),
            messages: vec![],
            company_message_receipts: vec![],
            reply_owners: vec![],
            title: None,
            pinned_memory: vec![],
            pinned_refs: vec![],
            workspace: None,
            transcript_revision: 0,
            provider_compaction: None,
        }
    }

    pub fn push_message(&mut self, message: super::Message) {
        // Reference-class loads register on the pin registry the moment they
        // land — re-loading moves a library to newest (it wins budget order).
        if let super::Message::ToolResult {
            tool_name,
            input,
            success: true,
            ..
        } = &message
        {
            if let Some(key) = reference_pin_key(tool_name, input) {
                self.pinned_refs
                    .retain(|p| !(p.tool == *tool_name && p.key == key));
                self.pinned_refs.push(PinnedRef {
                    tool: tool_name.clone(),
                    key,
                });
            }
        }
        self.messages.push(message);
        self.advance_transcript_revision();
    }

    pub fn has_company_message_receipt(&self, message_id: &str) -> bool {
        !message_id.is_empty()
            && self
                .company_message_receipts
                .iter()
                .any(|receipt| receipt == message_id)
    }

    /// Record a delivery receipt in the same session snapshot as its Talk
    /// message. Only a bounded retry window is needed: once SQLite reaches
    /// `injected`, the message is never rehydrated again.
    pub fn record_company_message_receipt(&mut self, message_id: &str) {
        const MAX_RECENT_COMPANY_RECEIPTS: usize = 256;
        if message_id.is_empty() || self.has_company_message_receipt(message_id) {
            return;
        }
        self.company_message_receipts.push(message_id.to_string());
        if self.company_message_receipts.len() > MAX_RECENT_COMPANY_RECEIPTS {
            let excess = self.company_message_receipts.len() - MAX_RECENT_COMPANY_RECEIPTS;
            self.company_message_receipts.drain(..excess);
        }
    }

    /// Replace logical history through the portable path. Any native replay
    /// state described the old history and is invalidated before the new
    /// transcript becomes visible.
    pub fn replace_messages(&mut self, messages: Vec<super::Message>) {
        self.messages = messages;
        self.advance_transcript_revision();
        self.provider_compaction = None;
    }

    /// Atomically install the portable replacement history and its optional
    /// provider-native replay accelerator in this in-memory session. The store's
    /// existing `save_one` then persists both in one atomic JSON rename.
    ///
    /// Canonical sessions are single-writer at the mesh lane. `save_one` is
    /// atomic but not compare-and-swap safe, so callers must continue holding
    /// that ownership while installing and saving a compaction generation.
    pub fn install_provider_compaction(
        &mut self,
        messages: Vec<super::Message>,
        replay: NativeCompactionReplay,
        expected_route: &NativeCompactionRoute,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !messages.is_empty(),
            "provider compaction must retain portable replacement history"
        );
        anyhow::ensure!(
            replay.portable_suffix_start() <= messages.len(),
            "native compaction portable suffix boundary {} exceeds replacement history length {}",
            replay.portable_suffix_start(),
            messages.len()
        );
        let expected_revision = self
            .transcript_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("session transcript revision overflow"))?;
        let provenance = replay.provenance();
        anyhow::ensure!(
            provenance.session_id() == self.id,
            "native compaction replay belongs to a different session"
        );
        anyhow::ensure!(
            replay.matches_route(
                expected_route.capability(),
                &self.id,
                expected_route.provider(),
                expected_route.base_route(),
                expected_route.model(),
                expected_route.account_scope(),
                expected_route.auth_epoch(),
            ),
            "native compaction replay does not match the effective provider route"
        );
        anyhow::ensure!(
            provenance.transcript_revision() == expected_revision,
            "native compaction replay targets transcript revision {}, expected {}",
            provenance.transcript_revision(),
            expected_revision
        );
        let same_route_previous = self.provider_compaction.as_ref().filter(|previous| {
            previous.matches_route(
                replay.capability(),
                &self.id,
                provenance.provider(),
                provenance.base_route(),
                provenance.model(),
                provenance.account_scope(),
                provenance.auth_epoch(),
            )
        });
        if let Some(previous) = same_route_previous {
            let expected_generation = previous
                .provenance()
                .compaction_generation()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("session compaction generation overflow"))?;
            anyhow::ensure!(
                provenance.compaction_generation() == expected_generation,
                "native compaction replay generation is {}, expected {}",
                provenance.compaction_generation(),
                expected_generation
            );
        } else {
            anyhow::ensure!(
                provenance.compaction_generation() == 1,
                "first native compaction replay generation must be 1"
            );
        }

        // Validate persistence bounds before mutating the live session. If the
        // opaque accelerator alone would exceed the session-file ceiling,
        // commit the portable fold without it; callers can report the `false`
        // result, but the durable recovery path still advances coherently.
        let mut candidate = self.clone();
        candidate.messages = messages;
        candidate.transcript_revision = expected_revision;
        candidate.provider_compaction = None;
        let portable_bytes = serde_json::to_vec_pretty(&candidate)?.len() as u64;
        anyhow::ensure!(
            portable_bytes <= MAX_SESSION_FILE_BYTES,
            "portable compacted session is {portable_bytes} bytes; maximum is {MAX_SESSION_FILE_BYTES}"
        );
        candidate.provider_compaction = Some(replay);
        let with_replay_bytes = serde_json::to_vec_pretty(&candidate)?.len() as u64;
        let installed_replay = with_replay_bytes <= MAX_SESSION_FILE_BYTES;
        if !installed_replay {
            candidate.provider_compaction = None;
        }
        *self = candidate;
        Ok(installed_replay)
    }

    /// Drop only the provider-specific accelerator. Portable history remains
    /// untouched and is what the next provider request must use.
    pub fn clear_provider_compaction(&mut self) {
        self.provider_compaction = None;
    }

    /// Return replay state only when every non-secret route discriminator
    /// matches. The second tuple element reports that stale persisted state was
    /// cleared during this lookup; the owning single-writer lane must save the
    /// Session before issuing the request so a crash cannot resurrect it.
    /// Adapters must repeat the route check at the final wire boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn provider_compaction_for_route(
        &mut self,
        capability: NativeCompactionCapability,
        provider: &str,
        base_route: &str,
        model: &str,
        account_scope: &str,
        auth_epoch: u64,
    ) -> (Option<&NativeCompactionReplay>, bool) {
        let Some(replay) = self.provider_compaction.as_ref() else {
            return (None, false);
        };
        let valid = replay.provenance().transcript_revision() <= self.transcript_revision
            && replay.portable_suffix_start() <= self.messages.len()
            && replay.matches_route(
                capability,
                &self.id,
                provider,
                base_route,
                model,
                account_scope,
                auth_epoch,
            );
        if !valid {
            // Route/model/account-generation switches are invalidations, not a
            // temporary filter: stale replay must not resurrect after a later
            // switch back to an older account or model.
            self.provider_compaction = None;
            return (None, true);
        }
        (self.provider_compaction.as_ref(), false)
    }

    fn advance_transcript_revision(&mut self) {
        match self.transcript_revision.checked_add(1) {
            Some(next) => self.transcript_revision = next,
            None => {
                // There is no safe provenance after wraparound. Preserve the
                // transcript and permanently stop trusting its replay prefix.
                self.provider_compaction = None;
            }
        }
    }

    fn sanitize_provider_compaction(&mut self) {
        // `Session::model` is the requested lane model. A fallback link may
        // legitimately compact with its own model override, so persistence can
        // validate only route-independent session structure here. The complete
        // capability/provider/base/model/account/epoch binding is enforced by
        // `provider_compaction_for_route` before replay is ever exposed.
        let valid = self.provider_compaction.as_ref().map_or(true, |replay| {
            replay.provenance().session_id() == self.id
                && replay.provenance().transcript_revision() <= self.transcript_revision
                && replay.portable_suffix_start() <= self.messages.len()
        });
        if !valid {
            self.provider_compaction = None;
        }
    }

    /// Set the human-readable title from the first user message, once. No-op if
    /// already titled. Keeps the global session list legible (`/sessions`).
    pub fn ensure_title(&mut self, from_user_message: &str) {
        if self.title.is_none() {
            let title = derive_session_title(from_user_message);
            if !title.is_empty() {
                self.title = Some(title);
            }
        }
    }

    /// Best display name: the title if set, else the id.
    pub fn display_name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.id)
    }

    /// Pin (or refresh) a memory file's content by path. Returns true if this
    /// changed the pinned set (new entry or an edited file), false if the same
    /// content was already pinned. The runtime persists the librarian's loaded
    /// memories here once, so later turns reuse them without a re-read.
    pub fn pin_memory(&mut self, path: &str, hash: &str, content: &str) -> bool {
        if let Some(existing) = self.pinned_memory.iter_mut().find(|m| m.path == path) {
            if existing.hash == hash {
                return false;
            }
            existing.hash = hash.to_string();
            existing.content = content.to_string();
            return true;
        }
        self.pinned_memory.push(PinnedMemory {
            path: path.to_string(),
            hash: hash.to_string(),
            content: content.to_string(),
        });
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub kind: SessionKind,
    pub model: String,
    pub message_count: usize,
}

/// Canonical persisted session store for step two.
pub struct SessionStore {
    root: PathBuf,
    sessions: HashMap<String, Session>,
}

pub type SessionManager = SessionStore;

impl SessionStore {
    /// Populate only an explicitly selected history. An unreadable existing
    /// history is an error, never permission to create an empty replacement.
    pub fn load_one_if_absent(&mut self, session_id: &str) -> anyhow::Result<()> {
        Self::validate_session_id(session_id)?;
        if !self.sessions.contains_key(session_id) {
            if let Some(session) = Self::read_one_from_disk(&self.root, session_id)? {
                self.sessions.insert(session_id.to_string(), session);
            }
        }
        Ok(())
    }

    /// Read one exact canonical session without scanning and deserializing the
    /// entire session directory. Sidebar/status refreshes call this for a
    /// bounded roster, so their cost scales with visible conversations rather
    /// than every historical source Phoenix has ever linked.
    pub fn read_one_from_disk(root: &Path, session_id: &str) -> anyhow::Result<Option<Session>> {
        Self::validate_session_id(session_id)?;
        let path = root.join(format!("{session_id}.json"));
        let Some(bytes) = crate::config::private_io::read_private_file_limited(
            &path,
            usize::try_from(MAX_SESSION_FILE_BYTES).unwrap_or(usize::MAX),
        )?
        else {
            return Self::read_one_from_cas(root, session_id);
        };
        let session: Session = match serde_json::from_slice(&bytes) {
            Ok(session) => session,
            Err(error) => {
                if let Some(recovered) = Self::read_one_from_cas(root, session_id)? {
                    tracing::warn!(
                        "sessions: recovered invalid canonical session {} from verified CAS ({error})",
                        path.display()
                    );
                    return Ok(Some(recovered));
                }
                return Err(error)
                    .with_context(|| format!("invalid canonical session {}", path.display()));
            }
        };
        Self::validate_session_id(&session.id)?;
        anyhow::ensure!(
            session.id == session_id,
            "canonical session id does not match its filename"
        );
        Ok(Some(session))
    }

    fn read_one_from_cas(root: &Path, session_id: &str) -> anyhow::Result<Option<Session>> {
        Self::validate_session_id(session_id)?;
        let namespace = transcript_cas_namespace(root, session_id);
        let ref_path = namespace.join("current.json");
        let Some(ref_bytes) =
            crate::config::private_io::read_private_file_limited(&ref_path, 64 * 1024)?
        else {
            return Ok(None);
        };
        let current: TranscriptCasRef =
            serde_json::from_slice(&ref_bytes).context("invalid transcript CAS current pointer")?;
        anyhow::ensure!(
            current.version == CAS_VERSION && current.session_id == session_id,
            "transcript CAS current pointer identity/version mismatch"
        );
        let manifest_path =
            transcript_cas_digest_path(&namespace.join("manifests"), &current.manifest)?;
        let manifest_bytes = crate::config::private_io::read_private_file_limited(
            &manifest_path,
            usize::try_from(MAX_SESSION_FILE_BYTES).unwrap_or(usize::MAX),
        )?
        .context("transcript CAS manifest is missing")?;
        anyhow::ensure!(
            sha256_hex(&manifest_bytes) == current.manifest,
            "transcript CAS manifest digest mismatch"
        );
        let manifest: TranscriptCasManifest =
            serde_json::from_slice(&manifest_bytes).context("invalid transcript CAS manifest")?;
        anyhow::ensure!(
            manifest.version == CAS_VERSION
                && manifest.session_id == session_id
                && manifest.transcript_revision == current.transcript_revision,
            "transcript CAS manifest identity/revision mismatch"
        );
        anyhow::ensure!(
            manifest.messages.len() <= CAS_ENTRY_SCAN_CAP,
            "transcript CAS manifest has too many messages"
        );

        let read_object =
            |digest: &str, expected_kind: &str| -> anyhow::Result<serde_json::Value> {
                let path = transcript_cas_digest_path(&namespace.join("objects"), digest)?;
                let bytes = crate::config::private_io::read_private_file_limited(
                    &path,
                    usize::try_from(MAX_SESSION_FILE_BYTES).unwrap_or(usize::MAX),
                )?
                .with_context(|| format!("transcript CAS object {digest} is missing"))?;
                anyhow::ensure!(
                    sha256_hex(&bytes) == digest,
                    "transcript CAS object digest mismatch"
                );
                let object: TranscriptCasObject =
                    serde_json::from_slice(&bytes).context("invalid transcript CAS object")?;
                anyhow::ensure!(
                    object.version == CAS_VERSION && object.kind == expected_kind,
                    "transcript CAS object kind/version mismatch"
                );
                Ok(object.payload)
            };

        let mut session: Session =
            serde_json::from_value(read_object(&manifest.metadata, "session_metadata")?)
                .context("invalid transcript CAS session metadata")?;
        anyhow::ensure!(
            session.id == session_id
                && session.transcript_revision == manifest.transcript_revision
                && session.messages.is_empty(),
            "transcript CAS metadata identity/revision mismatch"
        );
        session.messages = manifest
            .messages
            .iter()
            .map(|digest| {
                serde_json::from_value(read_object(digest, "message")?)
                    .context("invalid transcript CAS message")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        session.sanitize_provider_compaction();
        Ok(Some(session))
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sessions: HashMap::new(),
        }
    }

    pub fn with_default_root() -> anyhow::Result<Self> {
        Ok(Self::new(crate::config::phoenix_home().join("sessions")))
    }

    /// Canonical validation used by IPC boundaries and every persisted path.
    pub fn validate_session_id(session_id: &str) -> anyhow::Result<()> {
        validate_session_id_value(session_id)
    }

    pub fn create_main(&mut self, model: &str, system_prompt: &str) -> Session {
        let session = Session::new_main(model, system_prompt);
        self.sessions.insert(session.id.clone(), session.clone());
        session
    }

    pub fn load_or_create_main(
        &mut self,
        session_id: &str,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<Session> {
        Self::validate_session_id(session_id)?;
        if let Some(mut session) = self.sessions.get(session_id).cloned() {
            anyhow::ensure!(
                session.kind == SessionKind::Main,
                "session `{session_id}` is owned by {:?}; refusing to reinterpret it as Phoenix",
                session.kind
            );
            // Always refresh to current model and system prompt — stale
            // session files must not pin agents to old prompt versions.
            if session.model != model || session.system_prompt != system_prompt {
                session.clear_provider_compaction();
            }
            session.model = model.to_string();
            session.system_prompt = system_prompt.to_string();
            self.sessions.insert(session.id.clone(), session.clone());
            return Ok(session);
        }

        let session = Session::new_main_with_id(session_id.to_string(), model, system_prompt);
        self.sessions.insert(session.id.clone(), session.clone());
        Ok(session)
    }

    pub fn create_sub_agent(
        &mut self,
        agent_type: SubAgentType,
        model: &str,
        system_prompt: &str,
    ) -> Session {
        let session = Session::new_sub_agent(agent_type, model, system_prompt);
        self.sessions.insert(session.id.clone(), session.clone());
        session
    }

    pub fn load_or_create_specialist(
        &mut self,
        main_session_id: &str,
        agent_type: SubAgentType,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<Session> {
        self.load_or_create_specialist_scoped(
            main_session_id,
            agent_type,
            None,
            model,
            system_prompt,
        )
    }

    /// Load a specialist whose id is itself the first-class conversation id.
    /// Used by direct coworker chats so the sidebar's canonical thread and the
    /// transcript the coworker actually reads are the same durable object.
    pub fn load_or_create_specialist_with_id(
        &mut self,
        session_id: &str,
        agent_type: SubAgentType,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<Session> {
        Self::validate_session_id(session_id)?;
        if let Some(mut session) = self.sessions.get(session_id).cloned() {
            let expected = SessionKind::SubAgent(agent_type);
            // Older hires wrote their welcome message into a Main-kind file.
            // A thread holding nothing but that greeting has no other owner,
            // so it becomes the coworker's own conversation instead of
            // failing every handoff.
            if session.kind == SessionKind::Main && is_only_hire_greeting(&session) {
                session.kind = expected.clone();
            }
            anyhow::ensure!(
                session.kind == expected,
                "session `{session_id}` is owned by {:?}; refusing to reinterpret it as {:?}",
                session.kind,
                expected
            );
            if session.model != model || session.system_prompt != system_prompt {
                session.clear_provider_compaction();
            }
            session.model = model.to_string();
            session.system_prompt = system_prompt.to_string();
            self.sessions.insert(session.id.clone(), session.clone());
            return Ok(session);
        }
        let session = Session::new_sub_agent_with_id(session_id, agent_type, model, system_prompt);
        self.sessions.insert(session.id.clone(), session.clone());
        Ok(session)
    }

    /// Scoped variant for parallel specialist instances: a `scope` suffixes the
    /// session id (`<main>__<agent>--<scope>`) so a second concurrent instance
    /// gets its own fresh single-writer session file.
    pub fn load_or_create_specialist_scoped(
        &mut self,
        main_session_id: &str,
        agent_type: SubAgentType,
        scope: Option<&str>,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<Session> {
        let session_id = specialist_session_id_scoped(main_session_id, agent_type, scope);
        Self::validate_session_id(&session_id)?;
        if let Some(mut session) = self.sessions.get(&session_id).cloned() {
            let expected = SessionKind::SubAgent(agent_type);
            anyhow::ensure!(
                session.kind == expected,
                "session `{session_id}` is owned by {:?}; refusing to reinterpret it as {:?}",
                session.kind,
                expected
            );
            // Always refresh to current model and system prompt — stale
            // session files must not pin specialists to old prompt versions.
            if session.model != model || session.system_prompt != system_prompt {
                session.clear_provider_compaction();
            }
            session.model = model.to_string();
            session.system_prompt = system_prompt.to_string();
            self.sessions.insert(session.id.clone(), session.clone());
            return Ok(session);
        }

        let session = Session::new_sub_agent_with_id(session_id, agent_type, model, system_prompt);
        self.sessions.insert(session.id.clone(), session.clone());
        Ok(session)
    }

    pub fn upsert(&mut self, mut session: Session) {
        if let Err(error) = Self::validate_session_id(&session.id) {
            tracing::warn!(
                "sessions: refusing to cache invalid session id {:?} ({error})",
                session.id
            );
            return;
        }
        session.sanitize_provider_compaction();
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn get_mut(&mut self, session_id: &str) -> Option<&mut Session> {
        self.sessions.get_mut(session_id)
    }

    /// Iterate every loaded session — used by the project brain to gather the
    /// sibling threads that share a mesh/project key.
    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// The on-disk root, so callers can stat a session file's mtime (recency).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.sessions
            .values()
            .map(|session| SessionSummary {
                id: session.id.clone(),
                kind: session.kind.clone(),
                model: session.model.clone(),
                message_count: session.messages.len(),
            })
            .collect()
    }

    /// Atomic per-file write (tmp + rename): sessions now run turns
    /// CONCURRENTLY, and whole-dir readers (resume list, digest pass) must
    /// never observe a torn half-written session file.
    fn write_session_file(&self, session: &Session) -> anyhow::Result<()> {
        let path = self.checked_path_for(&session.id)?;
        crate::config::private_io::with_private_lock(&path, || self.write_session_file_under_lock(session))
    }

    /// Read and publish one current session under the same cross-process lock.
    /// Unlike saving a caller's old snapshot, this preserves concurrent appends.
    pub(crate) fn update_one<R>(
        root: &Path,
        session_id: &str,
        update: impl FnOnce(Option<Session>) -> anyhow::Result<(Session, R)>,
    ) -> anyhow::Result<R> {
        let store = Self::new(root.to_path_buf());
        let path = store.checked_path_for(session_id)?;
        crate::config::private_io::with_private_lock(&path, || {
            let (session, result) = update(Self::read_one_from_disk(root, session_id)?)?;
            anyhow::ensure!(session.id == session_id, "session update cannot change its identity");
            store.write_session_file_under_lock(&session)?;
            Ok(result)
        })
    }

    /// Caller holds the canonical JSON lock through CAS publication and GC.
    fn write_session_file_under_lock(&self, session: &Session) -> anyhow::Result<()> {
        let path = self.checked_path_for(&session.id)?;
        anyhow::ensure!(
            session.messages.len() <= CAS_ENTRY_SCAN_CAP,
            "session {} has too many messages for the transcript CAS manifest",
            session.id
        );
        let json = serde_json::to_vec_pretty(session)?;
        if json.len() as u64 > MAX_SESSION_FILE_BYTES {
            anyhow::bail!(
                "session {} serializes to {} bytes; refusing to write more than {} bytes",
                session.id,
                json.len(),
                MAX_SESSION_FILE_BYTES
            );
        }
        let namespace = transcript_cas_namespace(&self.root, &session.id);
        let objects_root = namespace.join("objects");
        let mut metadata = session.clone();
        metadata.messages.clear();
        let metadata_object = TranscriptCasObject {
            version: CAS_VERSION,
            kind: "session_metadata".to_string(),
            payload: serde_json::to_value(&metadata)?,
        };
        let metadata_bytes = serde_json::to_vec(&metadata_object)?;
        let metadata_digest = sha256_hex(&metadata_bytes);
        write_transcript_cas_blob(
            &transcript_cas_digest_path(&objects_root, &metadata_digest)?,
            &metadata_bytes,
        )?;

        let mut message_digests = Vec::with_capacity(session.messages.len());
        for message in &session.messages {
            let object = TranscriptCasObject {
                version: CAS_VERSION,
                kind: "message".to_string(),
                payload: serde_json::to_value(message)?,
            };
            let bytes = serde_json::to_vec(&object)?;
            let digest = sha256_hex(&bytes);
            write_transcript_cas_blob(
                &transcript_cas_digest_path(&objects_root, &digest)?,
                &bytes,
            )?;
            message_digests.push(digest);
        }
        let manifest = TranscriptCasManifest {
            version: CAS_VERSION,
            session_id: session.id.clone(),
            transcript_revision: session.transcript_revision,
            metadata: metadata_digest.clone(),
            messages: message_digests.clone(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let manifest_digest = sha256_hex(&manifest_bytes);
        write_transcript_cas_blob(
            &transcript_cas_digest_path(&namespace.join("manifests"), &manifest_digest)?,
            &manifest_bytes,
        )?;

        // The compatibility snapshot remains the hot path. Publish it only
        // after every object needed for CAS recovery is durable.
        crate::config::private_io::atomic_write_private_under_lock(&path, &json)?;
        let current = TranscriptCasRef {
            version: CAS_VERSION,
            session_id: session.id.clone(),
            transcript_revision: session.transcript_revision,
            manifest: manifest_digest.clone(),
        };
        crate::config::private_io::atomic_write_private(
            &namespace.join("current.json"),
            &serde_json::to_vec_pretty(&current)?,
        )?;
        self.collect_transcript_cas_garbage(
            &namespace,
            &metadata_digest,
            &message_digests,
            &manifest_digest,
        )
    }

    fn collect_transcript_cas_garbage(
        &self,
        namespace: &Path,
        metadata: &str,
        messages: &[String],
        manifest: &str,
    ) -> anyhow::Result<()> {
        let retained_objects = messages
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(metadata))
            .collect::<std::collections::HashSet<_>>();
        let retained_manifests =
            std::iter::once(manifest).collect::<std::collections::HashSet<_>>();
        for (root, retained) in [
            (namespace.join("objects/sha256"), &retained_objects),
            (namespace.join("manifests/sha256"), &retained_manifests),
        ] {
            let prefixes = match std::fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut scanned = 0usize;
            for prefix in prefixes {
                let prefix = prefix?;
                let prefix_name = prefix.file_name();
                let prefix_name = prefix_name.to_string_lossy();
                if prefix_name.len() != 2
                    || !prefix_name.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(prefix.path())?;
                anyhow::ensure!(
                    !metadata.file_type().is_symlink() && metadata.is_dir(),
                    "unsafe transcript CAS prefix {}",
                    prefix.path().display()
                );
                for entry in std::fs::read_dir(prefix.path())? {
                    let entry = entry?;
                    let entry_name = entry.file_name();
                    let entry_name = entry_name.to_string_lossy();
                    // Private-I/O advisory locks intentionally live beside
                    // their targets. They are not CAS objects and must never
                    // be recursively "collected" through another lock file.
                    if entry_name.starts_with('.') || !entry_name.ends_with(".json") {
                        continue;
                    }
                    scanned += 1;
                    anyhow::ensure!(
                        scanned <= CAS_ENTRY_SCAN_CAP,
                        "transcript CAS namespace exceeds garbage-collection scan cap"
                    );
                    let path = entry.path();
                    let file_metadata = std::fs::symlink_metadata(&path)?;
                    anyhow::ensure!(
                        !file_metadata.file_type().is_symlink() && file_metadata.is_file(),
                        "unsafe transcript CAS entry {}",
                        path.display()
                    );
                    let digest = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("");
                    anyhow::ensure!(
                        valid_sha256_hex(digest),
                        "invalid transcript CAS filename {}",
                        path.display()
                    );
                    if !retained.contains(digest) {
                        crate::config::private_io::remove_private_file(&path)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn save_to_disk(&self) -> anyhow::Result<()> {
        crate::config::private_io::prepare_private_parent(&self.root.join(".session-store"))?;
        for session in self.sessions.values() {
            self.write_session_file(session)?;
        }
        Ok(())
    }

    /// Persist ONE session's file, leaving every other file on disk untouched.
    /// Concurrent agent turns each own a different session — a whole-store
    /// `save_to_disk` from one turn would rewrite the others' files with the
    /// stale copies this store loaded at turn start.
    pub fn save_one(&self, session_id: &str) -> anyhow::Result<()> {
        Self::validate_session_id(session_id)?;
        let Some(session) = self.sessions.get(session_id) else {
            return Ok(());
        };
        self.write_session_file(session)?;
        Ok(())
    }

    pub fn load_from_disk(&mut self) -> anyhow::Result<()> {
        match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                tracing::warn!(
                    "sessions: refusing non-directory or symlinked store root {}; starting without persisted sessions",
                    self.root.display()
                );
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    "sessions: cannot inspect store {} ({error}); starting without persisted sessions",
                    self.root.display()
                );
                return Ok(());
            }
        }

        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(
                    "sessions: cannot read store {} ({error}); starting without persisted sessions",
                    self.root.display()
                );
                return Ok(());
            }
        };
        let mut entry_count = 0usize;
        let mut aggregate_bytes = 0u64;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!("sessions: skipping unreadable directory entry ({error})");
                    continue;
                }
            };
            entry_count += 1;
            if entry_count > MAX_SESSION_FILE_COUNT {
                tracing::warn!(
                    "sessions: reached directory-entry limit {MAX_SESSION_FILE_COUNT}; remaining files are ignored"
                );
                break;
            }
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!("sessions: skipping unreadable {} ({error})", path.display());
                    continue;
                }
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            // Sidecar files (TUI feed snapshots from binaries that wrote them
            // here) are not sessions — skip by suffix without parsing.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".feed.json"))
            {
                continue;
            }
            let Some(file_stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                tracing::warn!("sessions: skipping non-UTF-8 filename {}", path.display());
                continue;
            };
            if let Err(error) = Self::validate_session_id(file_stem) {
                tracing::warn!(
                    "sessions: skipping invalid filename {} ({error})",
                    path.display()
                );
                continue;
            }
            if metadata.len() > MAX_SESSION_FILE_BYTES {
                tracing::warn!(
                    "sessions: skipping oversized {} ({} bytes; max {})",
                    path.display(),
                    metadata.len(),
                    MAX_SESSION_FILE_BYTES
                );
                continue;
            }
            if aggregate_bytes.saturating_add(metadata.len()) > MAX_SESSION_AGGREGATE_BYTES {
                tracing::warn!(
                    "sessions: reached aggregate load limit {} bytes; remaining files are ignored",
                    MAX_SESSION_AGGREGATE_BYTES
                );
                break;
            }
            let bytes = match read_session_file_bounded(&path, MAX_SESSION_FILE_BYTES) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(
                        "sessions: skipping unreadable {} ({error:#})",
                        path.display()
                    );
                    continue;
                }
            };
            if aggregate_bytes.saturating_add(bytes.len() as u64) > MAX_SESSION_AGGREGATE_BYTES {
                tracing::warn!(
                    "sessions: reached aggregate load limit {} bytes; remaining files are ignored",
                    MAX_SESSION_AGGREGATE_BYTES
                );
                break;
            }
            aggregate_bytes += bytes.len() as u64;
            let content = match std::str::from_utf8(&bytes) {
                Ok(content) => content,
                Err(error) => {
                    tracing::warn!(
                        "sessions: skipping invalid UTF-8 {} ({error})",
                        path.display()
                    );
                    continue;
                }
            };
            // One unparseable file must not take down every turn of every
            // session forever (2026-07-08: a non-session .json here failed
            // the whole store, and with it the entire gateway). Skip loudly.
            match serde_json::from_str::<Session>(content) {
                Ok(mut session) => {
                    if let Err(error) = Self::validate_session_id(&session.id) {
                        tracing::warn!(
                            "sessions: skipping {} with invalid embedded id {:?} ({error})",
                            path.display(),
                            session.id
                        );
                        continue;
                    }
                    if session.id != file_stem {
                        tracing::warn!(
                            "sessions: skipping {} because embedded id {:?} does not match its filename",
                            path.display(),
                            session.id
                        );
                        continue;
                    }
                    session.sanitize_provider_compaction();
                    self.sessions.insert(session.id.clone(), session);
                }
                Err(error) => {
                    match Self::read_one_from_cas(&self.root, file_stem) {
                        Ok(Some(session)) => {
                            tracing::warn!(
                                "sessions: recovered unparseable {} from verified CAS ({error})",
                                path.display()
                            );
                            self.sessions.insert(session.id.clone(), session);
                        }
                        Ok(None) => tracing::warn!(
                            "sessions: skipping unparseable {} with no CAS recovery ({error})",
                            path.display()
                        ),
                        Err(cas_error) => tracing::warn!(
                            "sessions: skipping unparseable {} because CAS recovery also failed ({error}; {cas_error:#})",
                            path.display()
                        ),
                    }
                }
            }
        }

        Ok(())
    }

    pub fn session_path(&self, session_id: &str) -> PathBuf {
        match self.checked_path_for(session_id) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(
                    "sessions: refusing to construct a path for invalid id {:?} ({error})",
                    session_id
                );
                // Callers use this accessor for metadata/diagnostics. Writes
                // always go through `checked_path_for` and return the error.
                self.root.join(".invalid-session-id")
            }
        }
    }

    fn checked_path_for(&self, session_id: &str) -> anyhow::Result<PathBuf> {
        Self::validate_session_id(session_id)?;
        Ok(self.root.join(format!("{session_id}.json")))
    }
}

fn read_session_file_bounded(path: &Path, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        anyhow::bail!("not a regular file");
    }
    if metadata.len() > max_bytes {
        anyhow::bail!("file grew beyond {max_bytes} bytes before read");
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!("file grew beyond {max_bytes} bytes while reading");
    }
    Ok(bytes)
}

/// Turn the first user message into a short, legible session title:
/// first line, collapsed whitespace, trimmed to ~6 words / 48 chars.
/// `"could you please use my zen browser to go to reddit…"` → `"could you please use my zen browser"`.
pub fn derive_session_title(message: &str) -> String {
    let first_line = message.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let normalized = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let by_words: String = normalized.split(' ').take(6).collect::<Vec<_>>().join(" ");
    let mut title: String = by_words.chars().take(48).collect();
    if title.len() < normalized.len() {
        title = title.trim_end().to_string();
        title.push('…');
    }
    title
}

pub fn specialist_session_id(main_session_id: &str, agent_type: SubAgentType) -> String {
    specialist_session_id_scoped(main_session_id, agent_type, None)
}

/// Derive a filename-safe specialist id without letting a valid maximum-size
/// main id become unpersistable only after delegation. Normal ids retain the
/// historical `<main>__<agent>--<scope>` spelling; an overlong base is
/// shortened with a stable hash so distinct sessions cannot collide.
pub fn specialist_session_id_scoped(
    main_session_id: &str,
    agent_type: SubAgentType,
    scope: Option<&str>,
) -> String {
    actor_session_id_scoped(main_session_id, &agent_type.to_string().to_lowercase(), scope)
}

pub(crate) fn actor_session_id_scoped(main_session_id: &str, actor: &str, scope: Option<&str>) -> String {
    use sha2::{Digest, Sha256};

    fn hash16(value: &str) -> String {
        Sha256::digest(value.as_bytes())
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn bounded_component(value: &str) -> String {
        const MAX_COMPONENT_BYTES: usize = 64;
        if !value.is_empty()
            && value.len() <= MAX_COMPONENT_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return value.to_string();
        }
        let safe_prefix: String = value
            .bytes()
            .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            .take(MAX_COMPONENT_BYTES - 17)
            .map(char::from)
            .collect();
        let prefix = if safe_prefix.is_empty() {
            "scope".to_string()
        } else {
            safe_prefix
        };
        format!("{prefix}-{}", hash16(value))
    }

    let agent = bounded_component(actor);
    let mut suffix = format!("__{agent}");
    if let Some(scope) = scope.filter(|scope| !scope.is_empty()) {
        suffix.push_str("--");
        suffix.push_str(&bounded_component(scope));
    }
    let direct = format!("{main_session_id}{suffix}");
    if validate_session_id_value(&direct).is_ok() {
        return direct;
    }

    let hash = hash16(main_session_id);
    let marker = format!("-{hash}");
    let prefix_budget = MAX_SESSION_ID_BYTES
        .saturating_sub(suffix.len())
        .saturating_sub(marker.len());
    let prefix: String = main_session_id
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        .take(prefix_budget)
        .map(char::from)
        .collect();
    let prefix = if prefix.is_empty() {
        "session".chars().take(prefix_budget).collect::<String>()
    } else {
        prefix
    };
    let derived = format!("{prefix}{marker}{suffix}");
    debug_assert!(validate_session_id_value(&derived).is_ok());
    derived
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::with_default_root().unwrap_or_else(|_| Self::new(Path::new(".phoenix/sessions")))
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn transactional_session_appends_preserve_parallel_results_and_failed_updates() {
        use crate::session::Message;
        let root = tempfile::tempdir().unwrap();
        // Before: two callers can serialize their final writes yet still lose
        // data if both started from the same earlier snapshot.
        let mut old_store = SessionStore::new(root.path());
        let mut first = Session::new_main_with_id("stale-control", "test", "system");
        let mut second = first.clone();
        first.push_message(Message::User { content: "first result".into() });
        second.push_message(Message::User { content: "second result".into() });
        old_store.upsert(first); old_store.save_one("stale-control").unwrap();
        old_store.upsert(second); old_store.save_one("stale-control").unwrap();
        let control = SessionStore::read_one_from_disk(root.path(), "stale-control").unwrap().unwrap();
        assert_eq!(control.messages.iter().filter(|message| matches!(message, Message::User { .. })).count(), 1,
            "control reproduces lost update from saving stale snapshots");
        let id = "group-parallel-publish";
        SessionStore::update_one(root.path(), id, |_| Ok((Session::new_main_with_id(id, "test", "system"), ()))).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let threads = (0..4).map(|worker| {
            let path = root.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for item in 0..8 {
                    SessionStore::update_one(&path, id, |saved| {
                        let mut session = saved.unwrap();
                        session.push_message(Message::User { content: format!("worker {worker} / item {item} — exact result") });
                        Ok((session, ()))
                    }).unwrap();
                }
            })
        }).collect::<Vec<_>>();
        for thread in threads { thread.join().unwrap(); }
        let saved = SessionStore::read_one_from_disk(root.path(), id).unwrap().unwrap();
        let actual = saved.messages.iter().filter_map(|message| match message {
            Message::User { content } => Some(content.clone()), _ => None,
        }).collect::<std::collections::BTreeSet<_>>();
        let expected = (0..4).flat_map(|worker| (0..8).map(move |item| format!("worker {worker} / item {item} — exact result")))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert_eq!(saved.messages.iter().filter(|message| matches!(message, Message::User { .. })).count(), 32);
        let before = std::fs::read(root.path().join(format!("{id}.json"))).unwrap();
        let failed: anyhow::Result<()> = SessionStore::update_one(root.path(), id, |saved| {
            let mut session = saved.unwrap();
            session.push_message(Message::User { content: "must not publish".into() });
            anyhow::bail!("rejected transaction")
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(root.path().join(format!("{id}.json"))).unwrap(), before);
        let mismatch = SessionStore::update_one(root.path(), id, |saved| {
            let mut session = saved.unwrap(); session.id = "another-session".into(); Ok((session, ()))
        });
        assert!(mismatch.is_err());
        assert!(!root.path().join("another-session.json").exists());
        let recovered = SessionStore::read_one_from_cas(root.path(), id).unwrap().unwrap();
        assert_eq!(serde_json::to_value(recovered).unwrap(), serde_json::to_value(saved).unwrap());
    }

    #[test]
    fn selective_preload_preserves_history_and_does_not_scan_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let mut original = SessionStore::new(&sessions);
        let mut history = Session::new_main_with_id("selected-history", "old-model", "system");
        history.push_message(super::super::Message::User { content: "Already completed lessons 3 and 4".into() });
        original.upsert(history);
        original.save_one("selected-history").unwrap();
        std::fs::write(sessions.join("unrelated-corrupt.json"), b"not JSON").unwrap();
        let mut scoped = SessionStore::new(&sessions);
        scoped.load_one_if_absent("selected-history").unwrap();
        assert_eq!(scoped.sessions.len(), 1);
        assert_eq!(scoped.get("selected-history").unwrap().messages.len(), 1);
        let recovered = scoped.load_or_create_main("selected-history", "new-model", "system").unwrap();
        assert_eq!(recovered.messages.len(), 1);
        assert!(scoped.load_one_if_absent("unrelated-corrupt").is_err());
        assert!(scoped.get("unrelated-corrupt").is_none());
        assert_eq!(std::fs::read(sessions.join("unrelated-corrupt.json")).unwrap(), b"not JSON");
    }

    #[test]
    #[ignore = "explicit read-only local corpus benchmark; requires PHOENIX_SESSION_BENCH_ROOT and PHOENIX_SESSION_BENCH_IDS"]
    fn selective_preload_live_corpus_benchmark() {
        let root = PathBuf::from(std::env::var("PHOENIX_SESSION_BENCH_ROOT").unwrap());
        assert!(root.is_absolute());
        let ids = std::env::var("PHOENIX_SESSION_BENCH_IDS").unwrap();
        let ids: Vec<_> = ids.split(',').collect();
        assert!(!ids.is_empty() && ids.len() <= 3);
        for sample in 0..3 {
            let start = std::time::Instant::now();
            let mut broad = SessionStore::new(&root);
            broad.load_from_disk().unwrap();
            let broad_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            let mut scoped = SessionStore::new(&root);
            for id in &ids { scoped.load_one_if_absent(id).unwrap(); }
            let scoped_ms = start.elapsed().as_secs_f64() * 1000.0;
            for id in &ids {
                assert!(scoped.get(id).is_some(), "benchmark identity is missing");
                assert_eq!(serde_json::to_value(scoped.get(id)).unwrap(), serde_json::to_value(broad.get(id)).unwrap());
            }
            println!("PRELOAD_SAMPLE sample={sample} broad_sessions={} scoped_sessions={} broad_ms={broad_ms:.3} scoped_ms={scoped_ms:.3}", broad.sessions.len(), scoped.sessions.len());
        }
    }

    #[test]
    fn session_cas_recovers_canonical_json_and_collects_unreferenced_content() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let mut store = SessionStore::new(&sessions);
        let mut session = Session::new_main_with_id("main-cas-recovery", "model", "system");
        session.push_message(super::super::Message::User {
            content: "first durable prompt".into(),
        });
        session.push_message(super::super::Message::Assistant {
            content: "first durable answer".into(),
        });
        store.upsert(session.clone());
        store.save_one(&session.id).unwrap();

        let namespace = transcript_cas_namespace(&sessions, &session.id);
        let first_ref: TranscriptCasRef =
            serde_json::from_slice(&std::fs::read(namespace.join("current.json")).unwrap())
                .unwrap();
        let first_manifest_path =
            transcript_cas_digest_path(&namespace.join("manifests"), &first_ref.manifest).unwrap();
        let first_manifest: TranscriptCasManifest =
            serde_json::from_slice(&std::fs::read(&first_manifest_path).unwrap()).unwrap();
        assert_eq!(first_manifest.messages.len(), 2);
        let removed_digest = first_manifest.messages[1].clone();

        // A missing hot snapshot recovers from the verified current manifest.
        std::fs::remove_file(sessions.join("main-cas-recovery.json")).unwrap();
        let recovered = SessionStore::read_one_from_disk(&sessions, &session.id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.messages.len(), 2);
        assert_eq!(recovered.transcript_revision, session.transcript_revision);

        // Replacing history publishes a new immutable manifest and removes
        // objects that no longer belong to the current privacy boundary.
        session.replace_messages(vec![super::super::Message::User {
            content: "first durable prompt".into(),
        }]);
        store.upsert(session.clone());
        store.save_one(&session.id).unwrap();
        assert!(!first_manifest_path.exists());
        let removed_path =
            transcript_cas_digest_path(&namespace.join("objects"), &removed_digest).unwrap();
        assert!(!removed_path.exists());
        let current = SessionStore::read_one_from_disk(&sessions, &session.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.messages.len(), 1);
    }

    #[test]
    fn session_ids_are_one_safe_bounded_path_component() {
        let max_id = "x".repeat(MAX_SESSION_ID_BYTES);
        for valid in ["main-abc", "abc_123", "A", max_id.as_str()] {
            SessionStore::validate_session_id(valid).expect("valid session id");
        }
        let too_long = "x".repeat(MAX_SESSION_ID_BYTES + 1);
        for invalid in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            r"a\b",
            "/absolute",
            "C:\\absolute",
            "has space",
            "unicodé",
            too_long.as_str(),
        ] {
            assert!(
                SessionStore::validate_session_id(invalid).is_err(),
                "must reject {invalid:?}"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let rejected = store.session_path("../../outside");
        assert!(rejected.starts_with(dir.path()));
        assert_eq!(
            rejected.file_name().and_then(|name| name.to_str()),
            Some(".invalid-session-id")
        );
    }

    #[test]
    fn derives_a_short_legible_title() {
        assert_eq!(derive_session_title("browser?"), "browser?");
        assert_eq!(
            derive_session_title(
                "could you please use my zen browser to go to reddit, check my notifs?"
            ),
            "could you please use my zen…"
        );
        // first non-empty line only; whitespace collapsed
        assert_eq!(
            derive_session_title("\n\n  check   my   twitter  \nposts"),
            "check my twitter"
        );
        assert_eq!(derive_session_title("   "), "");
    }

    #[test]
    fn ensure_title_sets_once_then_is_stable() {
        let mut s = Session::new_main("m", "sys");
        assert!(s.title.is_none());
        assert_eq!(s.display_name(), &s.id);
        s.ensure_title("check my twitter posts please now");
        assert_eq!(
            s.title.as_deref(),
            Some("check my twitter posts please now")
        );
        // a later message never renames the session
        s.ensure_title("totally different second message");
        assert_eq!(
            s.title.as_deref(),
            Some("check my twitter posts please now")
        );
        assert_eq!(s.display_name(), "check my twitter posts please now");
        // an all-whitespace first message leaves it untitled (falls back to id)
        let mut blank = Session::new_main("m", "sys");
        blank.ensure_title("   \n  ");
        assert!(blank.title.is_none());
    }

    /// The 2026-07-08 gateway-killer: a TUI feed snapshot (`*.feed.json`) in
    /// the sessions dir — or any unparseable .json — must be skipped, never
    /// fail the whole store (which failed every turn of every session).
    #[test]
    fn load_from_disk_survives_sidecar_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        let session = Session::new_main("model", "sys");
        let real_id = session.id.clone();
        store.upsert(session);
        store.save_to_disk().unwrap();
        // A feed snapshot (no `id` field) and outright garbage, both .json.
        std::fs::write(
            dir.path().join("real-session.feed.json"),
            r#"{"version":1,"main_watermark":0,"items":[{"Notice":"hi"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("corrupt.json"), "{not json").unwrap();

        let mut fresh = SessionStore::new(dir.path());
        fresh
            .load_from_disk()
            .expect("sidecars must not fail the store");
        assert!(fresh.get(&real_id).is_some(), "real session loads");
        assert_eq!(fresh.list().len(), 1, "only the real session");
    }

    #[test]
    fn load_skips_invalid_utf8_oversize_and_mismatched_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut seed = SessionStore::new(dir.path());
        let valid = Session::new_main_with_id("valid-session", "model", "sys");
        seed.upsert(valid.clone());
        seed.save_to_disk().unwrap();

        std::fs::write(dir.path().join("invalid-utf8.json"), [0xff, 0xfe, 0xfd]).unwrap();
        let oversize = std::fs::File::create(dir.path().join("oversized.json")).unwrap();
        oversize.set_len(MAX_SESSION_FILE_BYTES + 1).unwrap();
        let escaped = Session::new_main_with_id("../escape", "model", "sys");
        std::fs::write(
            dir.path().join("safe-name.json"),
            serde_json::to_vec(&escaped).unwrap(),
        )
        .unwrap();

        let mut loaded = SessionStore::new(dir.path());
        loaded
            .load_from_disk()
            .expect("bad neighbors never brick the session store");
        assert!(loaded.get("valid-session").is_some());
        assert_eq!(loaded.list().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn session_writes_are_private_atomic_and_reject_traversal() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        store.upsert(Session::new_main_with_id("private-session", "model", "sys"));
        store.save_one("private-session").unwrap();
        let path = dir.path().join("private-session.json");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        serde_json::from_slice::<Session>(&std::fs::read(path).unwrap())
            .expect("atomic write leaves complete JSON");

        let mut invalid = SessionStore::new(dir.path());
        let outside_name = format!("escape-{}", uuid::Uuid::new_v4());
        let escaped_id = format!("../{outside_name}");
        assert!(invalid
            .load_or_create_main(&escaped_id, "model", "sys")
            .is_err());
        assert!(!dir
            .path()
            .parent()
            .unwrap()
            .join(format!("{outside_name}.json"))
            .exists());
    }

    fn ref_result(tool: &str, input: &str, success: bool) -> super::super::Message {
        super::super::Message::ToolResult {
            tool_name: tool.to_string(),
            input: input.to_string(),
            success,
            output: "content".to_string(),
        }
    }

    #[test]
    fn derived_specialist_ids_remain_valid_at_the_main_id_boundary() {
        let first = format!("{}a", "x".repeat(MAX_SESSION_ID_BYTES - 1));
        let second = format!("{}b", "x".repeat(MAX_SESSION_ID_BYTES - 1));
        let first_id = specialist_session_id_scoped(
            &first,
            SubAgentType::Researcher,
            Some("guild-1234567890"),
        );
        let second_id = specialist_session_id_scoped(
            &second,
            SubAgentType::Researcher,
            Some("guild-1234567890"),
        );

        SessionStore::validate_session_id(&first_id).expect("first derived id is valid");
        SessionStore::validate_session_id(&second_id).expect("second derived id is valid");
        assert!(first_id.len() <= MAX_SESSION_ID_BYTES);
        assert_ne!(first_id, second_id, "truncated bases must retain identity");
        assert_eq!(
            specialist_session_id_scoped("main-abc", SubAgentType::Coder, Some("2")),
            "main-abc__coder--2"
        );
    }

    /// The compaction-gitignore registry: successful reference loads pin by
    /// canonical identity; aliases collapse; re-loads move to newest; index
    /// listings, failures, and skill sub-file reads pin nothing.
    #[test]
    fn reference_loads_pin_on_the_session() {
        let mut s = Session::new_main("m", "sys");
        s.push_message(ref_result(
            "design_reference",
            r#"{"path":"taste/SKILL.md"}"#,
            true,
        ));
        s.push_message(ref_result("design_reference", r#"{"path":"10k"}"#, true));
        s.push_message(ref_result(
            "skill",
            r#"{"name":"kombai-design-playbook"}"#,
            true,
        ));
        assert_eq!(
            s.pinned_refs
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "taste/SKILL.md",
                "immersive/SKILL.md",
                "kombai-design-playbook"
            ],
            "canonical keys, load order"
        );
        // Alias re-load of the same library: dedup + bump to newest.
        s.push_message(ref_result("design_reference", r#"{"path":"taste"}"#, true));
        assert_eq!(
            s.pinned_refs
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "immersive/SKILL.md",
                "kombai-design-playbook",
                "taste/SKILL.md"
            ],
            "re-load moves Taste to newest, no duplicate"
        );
        // Non-pinning shapes.
        s.push_message(ref_result("design_reference", r#"{"path":"index"}"#, true));
        s.push_message(ref_result("design_reference", r#"{}"#, true));
        s.push_message(ref_result(
            "design_reference",
            r#"{"path":"studio"}"#,
            false,
        ));
        s.push_message(ref_result(
            "skill",
            r#"{"name":"x","file":"references/a.md"}"#,
            true,
        ));
        s.push_message(ref_result("bash", r#"{"command":"ls"}"#, true));
        assert_eq!(
            s.pinned_refs.len(),
            3,
            "index/failure/sub-file/other tools never pin"
        );
    }

    #[test]
    fn direct_coworker_session_uses_the_canonical_id_without_a_hidden_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        let mut session = store
            .load_or_create_specialist_with_id(
                "agent-finance",
                SubAgentType::Finance,
                "model-a",
                "prompt-a",
            )
            .unwrap();
        session.push_message(super::super::Message::User {
            content: "reconcile this month".into(),
        });
        store.upsert(session);
        store.save_one("agent-finance").unwrap();

        let mut reopened = SessionStore::new(dir.path());
        reopened.load_from_disk().unwrap();
        let session = reopened
            .load_or_create_specialist_with_id(
                "agent-finance",
                SubAgentType::Finance,
                "model-b",
                "prompt-b",
            )
            .unwrap();
        assert_eq!(session.id, "agent-finance");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.model, "model-b");
        assert!(matches!(
            session.kind,
            SessionKind::SubAgent(SubAgentType::Finance)
        ));
        assert!(!dir.path().join("agent-finance--finance.json").exists());
    }

    #[test]
    fn hire_greeting_only_main_session_becomes_the_coworkers_own() {
        use crate::session::Message;
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        let mut greeting = Session::new_main_with_id("agent-hired", "configured", "Phoenix coworker conversation");
        greeting.push_message(Message::Assistant { content: "<!-- phoenix-initial-agent-message:v1 -->\nHi — I'm Rory.".into() });
        store.upsert(greeting);
        let session = store.load_or_create_specialist_with_id("agent-hired", SubAgentType::Finance, "m", "p").unwrap();
        assert_eq!(session.kind, SessionKind::SubAgent(SubAgentType::Finance));
        // A Main thread with real conversation is still never retyped.
        let mut used = Session::new_main_with_id("agent-used", "configured", "x");
        used.push_message(Message::Assistant { content: "<!-- phoenix-initial-agent-message:v1 -->".into() });
        used.push_message(Message::User { content: "hello".into() });
        store.upsert(used);
        assert!(store.load_or_create_specialist_with_id("agent-used", SubAgentType::Finance, "m", "p").is_err());
    }

    #[test]
    fn existing_sessions_cannot_be_retyped_or_reprompted_by_another_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        store.upsert(Session::new_main_with_id(
            "owned-main",
            "main-model",
            "main-prompt",
        ));
        assert!(store
            .load_or_create_specialist_with_id(
                "owned-main",
                SubAgentType::Finance,
                "finance-model",
                "finance-prompt",
            )
            .is_err());
        let main = store.get("owned-main").unwrap();
        assert_eq!(main.kind, SessionKind::Main);
        assert_eq!(main.model, "main-model");
        assert_eq!(main.system_prompt, "main-prompt");

        store.upsert(Session::new_sub_agent_with_id(
            "owned-finance",
            SubAgentType::Finance,
            "finance-model",
            "finance-prompt",
        ));
        assert!(store
            .load_or_create_main("owned-finance", "main-model", "main-prompt")
            .is_err());
        assert!(store
            .load_or_create_specialist_with_id(
                "owned-finance",
                SubAgentType::Scribe,
                "scribe-model",
                "scribe-prompt",
            )
            .is_err());
        let finance = store.get("owned-finance").unwrap();
        assert_eq!(finance.kind, SessionKind::SubAgent(SubAgentType::Finance));
        assert_eq!(finance.model, "finance-model");
        assert_eq!(finance.system_prompt, "finance-prompt");
    }

    #[test]
    fn scoped_specialist_session_rejects_a_conflicting_persisted_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        let session_id =
            specialist_session_id_scoped("main", SubAgentType::Finance, Some("group-launch"));
        store.upsert(Session::new_main_with_id(
            &session_id,
            "main-model",
            "main-prompt",
        ));
        assert!(store
            .load_or_create_specialist_scoped(
                "main",
                SubAgentType::Finance,
                Some("group-launch"),
                "finance-model",
                "finance-prompt",
            )
            .is_err());
        assert_eq!(store.get(&session_id).unwrap().kind, SessionKind::Main);
    }

    #[test]
    fn company_message_receipts_survive_restart_and_stay_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path());
        let mut session = Session::new_main_with_id("receipt-session", "model", "system");
        for index in 0..300 {
            session.record_company_message_receipt(&format!("message_{index:03}"));
        }
        session.record_company_message_receipt("message_299");
        assert_eq!(session.company_message_receipts.len(), 256);
        assert!(!session.has_company_message_receipt("message_000"));
        assert!(session.has_company_message_receipt("message_299"));
        store.upsert(session);
        store.save_one("receipt-session").unwrap();

        let mut reopened = SessionStore::new(dir.path());
        reopened.load_from_disk().unwrap();
        let session = reopened.get("receipt-session").unwrap();
        assert_eq!(session.company_message_receipts.len(), 256);
        assert!(session.has_company_message_receipt("message_299"));
    }
}

/// True when a conversation contains only the welcome message written when a
/// coworker was hired (no user or tool turns).
fn is_only_hire_greeting(session: &Session) -> bool {
    !session.messages.is_empty()
        && session.messages.iter().all(|message| {
            matches!(message, super::Message::Assistant { content } if content.contains("phoenix-initial-agent-message:"))
        })
}
