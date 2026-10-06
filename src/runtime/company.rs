//! Phoenix's durable company runtime.
//!
//! Models remain decentralized: they propose, claim, challenge, review, and
//! talk directly.  This module is deliberately boring infrastructure beneath
//! them: one append-only SQLite/WAL event stream and disposable projections.
//! A process restart may kill an executor, but it cannot erase what the team
//! accepted, produced, learned, or still owes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const COMPANY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupReadyContinuation {
    pub canonical_session_id: String,
    pub original_turn_id: String,
    pub turn_id: String,
    pub activation: super::group_conversation::GroupActivationIntent,
    pub predecessor_receipts: Vec<String>,
    #[serde(default)]
    pub original_request: Option<String>,
}
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_EVENT_PAGE: usize = 10_000;
const MAX_SNAPSHOT_ROWS: usize = 50_000;
/// A machine-wide snapshot feeds live company UI, not the durable history
/// browser. Keep it recent and cheap even after years of company activity.
const MAX_GLOBAL_STATUS_JOBS: usize = 512;
const MAX_EVENT_PAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_MIRRORED_READ_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COMPANY_DB_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_JOB_RETURN_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_JOB_RETURN_TEXT_BYTES: usize = 64 * 1024;
const MAX_JOB_RETURNS_PER_SESSION: i64 = 100;

// Historical v6 defaults used only by the monotonic v7 rollback migration.
// Every field that v6 wrote is matched before rollback, so a user refinement
// is authoritative even when only one part of the old payload was changed.
const V6_PERSONAL_LOGISTICS_TITLE: &str = "School & Life Operations";
const V6_PERSONAL_LOGISTICS_DESCRIPTION: &str = "Owns school success, Moodle continuity, three-week-ahead study plans, assignment materials, recurring procedures, and real-life commitments.";
const V6_PERSONAL_LOGISTICS_SCOPE: &str = "Own school success and practical operations through verified completion: learning portals, deadlines, rubrics, three-week-ahead planning, study materials, assignment readiness, recurring procedures, and real-life commitments.";
const V6_PERSONAL_LOGISTICS_CRITERIA: [&str; 3] = [
    "Deadlines, timezone, rubric, source, identity scope, and constraints are explicit",
    "Draft preparation is proactive while submission, paid, or binding actions carry the required approval",
    "Submission, schedule, artifact, cost, and cancellation evidence are preserved",
];
const FOUNDING_APPROVAL_POLICY: &str = r#"{"inherits_company_policy":true}"#;
const FOUNDING_ESCALATION_POLICY: &str = r#"{"ambiguous_owner":"phoenix","blocked":"ask_user"}"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SidebarItemKey {
    Agent(String),
    Group(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMemberInput {
    pub agent_id: String,
    #[serde(default = "default_group_member_role")]
    pub member_role: String,
    #[serde(default = "default_group_history_access")]
    pub history_access: super::company_directory::HistoryAccess,
}

pub(crate) const MIN_GROUP_MEMBERS: usize = 2;
pub(crate) const MAX_GROUP_MEMBERS: usize = 6;

/// Validate a prospective active roster and derive the server-owned fallback
/// name used when the client leaves the group name blank. The result depends
/// only on the ordered stable member ids and their current directory profiles,
/// so retries against the same snapshot produce the same name.
pub(crate) fn generated_group_name(
    snapshot: &super::company_directory::DirectorySnapshot,
    members: &[String],
) -> Result<String> {
    anyhow::ensure!(
        (MIN_GROUP_MEMBERS..=MAX_GROUP_MEMBERS).contains(&members.len()),
        "a group needs {MIN_GROUP_MEMBERS} to {MAX_GROUP_MEMBERS} active coworkers"
    );
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::with_capacity(members.len());
    for agent_id in members {
        anyhow::ensure!(seen.insert(agent_id), "duplicate group member `{agent_id}`");
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == *agent_id)
            .with_context(|| format!("unknown group member `{agent_id}`"))?;
        anyhow::ensure!(
            agent.profile.lifecycle == super::company_directory::LifecycleState::Active,
            "group member `{agent_id}` is not active"
        );
        let display_name = agent
            .profile
            .display_name
            .trim()
            .chars()
            .take(32)
            .collect::<String>();
        names.push(display_name);
    }

    let name = match names.as_slice() {
        [first, second] => format!("{first} & {second}"),
        [first, second, third] => format!("{first}, {second} & {third}"),
        [first, second, rest @ ..] => {
            format!("{first}, {second} & {} more", rest.len())
        }
        _ => unreachable!("the member-count guard requires at least two names"),
    };
    Ok(format!("{name} Room"))
}

fn default_group_member_role() -> String {
    "member".to_string()
}

fn default_group_history_access() -> super::company_directory::HistoryAccess {
    super::company_directory::HistoryAccess::Full
}

fn directory_event(
    actor_agent_id: &str,
    idempotency_key: String,
    change: super::company_directory::DirectoryChange,
) -> NewCompanyEvent {
    NewCompanyEvent {
        run_id: "company-directory".to_string(),
        session_id: "company-directory".to_string(),
        pod_id: None,
        work_node_id: None,
        attempt_id: None,
        agent_identity_id: Some(actor_agent_id.to_string()),
        agent_instance_id: Some(actor_agent_id.to_string()),
        causation_id: None,
        correlation_id: None,
        idempotency_key: Some(idempotency_key),
        event: CompanyEventKind::DirectoryChanged { change },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Proposed,
    Ready,
    Claimed,
    Active,
    ReviewReady,
    Accepted,
    Rejected,
    Superseded,
    WaitingUser,
    Canceled,
}

impl WorkState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Ready => "ready",
            Self::Claimed => "claimed",
            Self::Active => "active",
            Self::ReviewReady => "review_ready",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::WaitingUser => "waiting_user",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Available,
    Forming,
    Queued,
    Starting,
    Reasoning,
    UsingTool,
    WaitingPeer,
    WaitingUser,
    Reviewing,
    Integrating,
    Superseded,
    Failed,
    CompletedVerified,
    CompletedUnverified,
    Stale,
    Unavailable,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Forming => "forming",
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Reasoning => "reasoning",
            Self::UsingTool => "using_tool",
            Self::WaitingPeer => "waiting_peer",
            Self::WaitingUser => "waiting_user",
            Self::Reviewing => "reviewing",
            Self::Integrating => "integrating",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
            Self::CompletedVerified => "completed_verified",
            Self::CompletedUnverified => "completed_unverified",
            Self::Stale => "stale",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationPattern {
    Split,
    Studio,
    Mob,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Status,
    Proposal,
    Question,
    Challenge,
    Decision,
    Handoff,
    Conversation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningKind {
    EpisodicMemory,
    CompanyKnowledge,
    UserPreference,
    SkillPatch,
    RegressionEval,
    RoutingPrior,
    CollaborationLesson,
    ToolIncident,
    RuntimeChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusFrame {
    pub intent: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub collaborators: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    pub next_event: String,
    #[serde(default)]
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompanyEventKind {
    RunOpened {
        mission: String,
    },
    AgentRegistered {
        identity_id: String,
        role: String,
        persona: String,
        guild: String,
    },
    PodFormed {
        pod_id: String,
        name: String,
        pattern: CollaborationPattern,
        members: Vec<String>,
    },
    WorkProposed {
        node_id: String,
        title: String,
        outcome: String,
        acceptance: Vec<String>,
        dependencies: Vec<String>,
        pattern: CollaborationPattern,
    },
    WorkClaimed {
        node_id: String,
        attempt_id: String,
        identity_id: String,
        approach: String,
    },
    WorkStateChanged {
        node_id: String,
        state: WorkState,
        reason: String,
    },
    FocusUpdated {
        frame: FocusFrame,
    },
    MessageAccepted {
        message_id: String,
        /// Producer-owned operation identity. Replaying the same operation is
        /// idempotent; reusing it with different immutable content is rejected.
        #[serde(default)]
        operation_id: String,
        /// Stable lifecycle identity shown by the UI. Detached work may use a
        /// `return_*` identity distinct from the delivery row's message id.
        #[serde(default)]
        handoff_id: String,
        /// Exact handoff this message answers, when it is a return.
        #[serde(default)]
        reply_to: Option<String>,
        /// Durable delivery/event that caused this message.
        #[serde(default)]
        causation_id: Option<String>,
        from: String,
        to: String,
        subject: String,
        body: String,
        message_kind: MessageKind,
        #[serde(default)]
        reply_expected: bool,
    },
    MessageInjected {
        message_id: String,
    },
    JobStarted {
        job_id: String,
        role: String,
        subject: String,
    },
    JobSettled {
        job_id: String,
        ok: bool,
        verified: bool,
        summary: String,
    },
    ArtifactPublished {
        artifact_id: String,
        path: String,
        content_hash: String,
        source_artifacts: Vec<String>,
    },
    ArtifactSuperseded {
        artifact_id: String,
        replacement_id: String,
        reason: String,
    },
    ChallengeRaised {
        challenge_id: String,
        target_id: String,
        claim: String,
        evidence: Vec<String>,
    },
    DecisionRecorded {
        decision_id: String,
        target_id: String,
        accepted_id: String,
        voters: Vec<String>,
        rationale: String,
    },
    ReadRecorded {
        path: String,
        content_hash: String,
        question: String,
        answer_ref: String,
    },
    SkillActivated {
        skill: String,
        version: String,
        required_checkpoints: Vec<String>,
    },
    SkillCheckpoint {
        skill: String,
        checkpoint: String,
    },
    LearningProposed {
        candidate_id: String,
        learning_kind: LearningKind,
        statement: String,
        evidence: Vec<String>,
        producer: String,
    },
    LearningPromoted {
        candidate_id: String,
        reviewers: Vec<String>,
        canary_receipt: String,
    },
    /// Durable workflow contract and scheduler receipts.  These events are
    /// intentionally separate from the older `Work*` collaboration events:
    /// the latter describe a human-readable work map, while these records
    /// carry the fencing, budget, restart, and evidence state needed by a
    /// long-lived executor.
    WorkflowGoalCreated {
        goal_id: String,
        title: String,
        objective: String,
        contract_json: String,
    },
    WorkflowRunOpened {
        workflow_run_id: String,
        goal_id: String,
        budget_json: String,
        #[serde(default = "default_workflow_scope")]
        scope: String,
        #[serde(default = "default_workflow_owner")]
        owner_agent_id: String,
        #[serde(default)]
        group_id: Option<String>,
        restart_state: String,
        next_wake_at: Option<String>,
    },
    WorkflowNodeDefined {
        node_id: String,
        #[serde(default)]
        owner_agent_id: Option<String>,
        title: String,
        outcome: String,
        parent_id: Option<String>,
        phase: String,
        dependencies: Vec<String>,
        budget_json: String,
        evidence_requirements_json: String,
        node_idempotency_key: String,
    },
    WorkflowNodeLeased {
        node_id: String,
        lease_id: String,
        worker_id: String,
        fencing_token: String,
        lease_runtime_epoch: String,
        leased_at: String,
        heartbeat_at: String,
        expires_at: String,
        attempt: u32,
    },
    WorkflowNodeHeartbeat {
        node_id: String,
        lease_id: String,
        worker_id: String,
        fencing_token: String,
        heartbeat_at: String,
        expires_at: String,
    },
    WorkflowNodeStateChanged {
        node_id: String,
        phase: String,
        state: String,
        restart_state: String,
        reason: String,
        next_wake_at: Option<String>,
        result_json: Option<String>,
        usage_json: Option<String>,
        evidence_json: Option<String>,
        lease_id: Option<String>,
        fencing_token: Option<String>,
        clear_lease: bool,
        #[serde(default)]
        wait_json: Option<String>,
    },
    WorkflowEvidenceRecorded {
        node_id: String,
        receipt_json: String,
        lease_id: String,
        worker_id: String,
        fencing_token: String,
    },
    WorkflowRunStateChanged {
        workflow_run_id: String,
        state: String,
        restart_state: String,
        reason: String,
        next_wake_at: Option<String>,
    },
    WorkflowRunOwnershipChanged {
        workflow_run_id: String,
        new_owner_agent_id: String,
        reason: String,
    },
    /// A durable change to the human-facing company directory. The nested
    /// enum remains strongly typed while sharing this event stream's ordering,
    /// idempotency, causation, and recovery guarantees.
    DirectoryChanged {
        change: super::company_directory::DirectoryChange,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCompanyEvent {
    pub run_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub pod_id: Option<String>,
    #[serde(default)]
    pub work_node_id: Option<String>,
    #[serde(default)]
    pub attempt_id: Option<String>,
    #[serde(default)]
    pub agent_identity_id: Option<String>,
    #[serde(default)]
    pub agent_instance_id: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub event: CompanyEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyEvent {
    pub schema_version: u32,
    pub event_id: String,
    pub runtime_epoch: String,
    pub company_seq: i64,
    pub recorded_at: DateTime<Utc>,
    #[serde(flatten)]
    pub envelope: NewCompanyEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProjection {
    pub job_id: String,
    pub run_id: String,
    pub session_id: String,
    pub role: String,
    pub subject: String,
    pub state: AgentState,
    pub ok: Option<bool>,
    pub verified: bool,
    pub summary: Option<String>,
    pub started_at: String,
    pub settled_at: Option<String>,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkProjection {
    pub node_id: String,
    pub run_id: String,
    pub title: String,
    pub outcome: String,
    pub acceptance: Vec<String>,
    pub dependencies: Vec<String>,
    pub pattern: CollaborationPattern,
    pub state: WorkState,
    pub reason: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowGoalProjection {
    pub goal_id: String,
    pub title: String,
    pub objective: String,
    pub contract_json: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunProjection {
    pub run_id: String,
    pub goal_id: String,
    pub budget_json: String,
    pub scope: String,
    pub owner_agent_id: String,
    pub group_id: Option<String>,
    pub state: String,
    pub restart_state: String,
    pub reason: String,
    pub next_wake_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

fn default_workflow_scope() -> String {
    "company".to_string()
}

fn default_workflow_owner() -> String {
    "phoenix".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeProjection {
    pub node_id: String,
    #[serde(default)]
    pub owner_agent_id: Option<String>,
    #[serde(default)]
    pub wait_json: Option<String>,
    pub run_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub outcome: String,
    pub phase: String,
    pub state: String,
    pub dependencies: Vec<String>,
    pub budget_json: String,
    pub evidence_requirements_json: String,
    pub evidence_json: String,
    pub result_json: Option<String>,
    pub usage_json: Option<String>,
    pub restart_state: String,
    pub reason: String,
    pub node_idempotency_key: String,
    pub lease_id: Option<String>,
    pub lease_worker: Option<String>,
    pub fencing_token: Option<String>,
    pub lease_runtime_epoch: Option<String>,
    pub leased_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub attempt: u32,
    pub next_wake_at: Option<String>,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdgeProjection {
    pub run_id: String,
    pub node_id: String,
    pub upstream_id: String,
    pub edge_kind: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowSnapshot {
    pub as_of_seq: i64,
    pub goals: Vec<WorkflowGoalProjection>,
    pub runs: Vec<WorkflowRunProjection>,
    pub nodes: Vec<WorkflowNodeProjection>,
    pub edges: Vec<WorkflowEdgeProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompanySnapshot {
    pub runtime_epoch: String,
    pub as_of_seq: i64,
    pub jobs: Vec<JobProjection>,
    pub work: Vec<WorkProjection>,
    #[serde(default)]
    pub workflow: WorkflowSnapshot,
    pub active_jobs: usize,
    pub stale_jobs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationReadMarker {
    pub item: SidebarItemKey,
    pub last_read_revision: u64,
    pub read_at: String,
}

pub struct CompanyStore {
    path: PathBuf,
    runtime_epoch: String,
    connection: Mutex<Connection>,
    // A different connection/epoch is not evidence of a dead executor.
    // Keep this OS claim until every holder of this store has released it.
    _runtime_claim: crate::config::private_io::PrivateExecutionClaim,
}

fn runtime_claim_path(database: &Path, epoch: &str) -> PathBuf {
    database.with_extension("runtime-claims")
        .join(format!("{:x}.lock", Sha256::digest(epoch.as_bytes())))
}

/// Older Phoenix test binaries mirrored their synthetic mesh/postbox jobs into
/// the selected company database. Those rows remain valid forensic history and
/// must not be deleted during migration, but they are not coworkers or user
/// work and must never pollute the live company view. A session-scoped snapshot
/// deliberately remains raw so diagnostics can still inspect an exact run.
fn legacy_synthetic_session(session_id: &str) -> bool {
    [
        "bench-",
        "mesh-bg-test-",
        "mesh-chain-test-",
        "mesh-conc-test-",
        "mesh-direct-test-",
        "mesh-frontend-test-",
        "mesh-inject-test-",
        "mesh-universal-talk-",
        "postbox-cancel-test-",
        "postbox-live-turn-",
        "postbox-test-",
    ]
    .iter()
    .any(|prefix| session_id.starts_with(prefix))
}

/// Undelivered handoffs older than this are canceled instead of delivered.
const STALE_UNDELIVERED_MESSAGE_HOURS: i64 = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingCompanyMessage {
    pub message_id: String,
    #[serde(default)]
    pub operation_id: String,
    #[serde(default)]
    pub handoff_id: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub reply_expected: bool,
}

/// Result of reserving one producer-owned company handoff operation.
///
/// Only a newly-created reservation may be routed by the caller. Exact
/// retries return the original durable identities with `should_route=false`;
/// accepted rows are rehydrated by `Gateway::with_durable_session` after a
/// restart, while terminal or paused rows must never be made observable again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanyMessageAcceptance {
    pub message_id: String,
    pub handoff_id: String,
    pub state: String,
    pub created: bool,
    pub should_route: bool,
}

/// One acceptance gate for every event producer, including replay/import
/// helpers that bypass `accept_company_message`. Alias/display-name equality
/// is resolved before SQLite is locked so live-directory lookup cannot
/// re-enter the same connection mutex.
fn validate_company_message_route(input: &NewCompanyEvent) -> Result<()> {
    if let CompanyEventKind::MessageAccepted {
        from,
        to,
        message_kind,
        ..
    } = &input.event
    {
        if *message_kind == MessageKind::Conversation && to != "company" {
            anyhow::ensure!(
                !crate::runtime::mailbox::same_agent_identity(from, to),
                "coworker `{from}` cannot accept a conversation message addressed to itself (`{to}`)"
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum DuePrivatePurge {
    Agent {
        agent_id: String,
        internal_role: String,
        canonical_session_id: Option<String>,
        browser_profile_id: String,
    },
    Group {
        group_id: String,
        canonical_session_id: Option<String>,
    },
}

impl CompanyStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        crate::config::private_io::prepare_private_parent(&path)
            .context("preparing company store directory")?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                anyhow::bail!("refusing unsafe company store {}", path.display());
            }
            Ok(metadata) if metadata.len() > MAX_COMPANY_DB_BYTES => {
                anyhow::bail!(
                    "company store {} is too large ({} bytes; max {MAX_COMPANY_DB_BYTES})",
                    path.display(),
                    metadata.len()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        validate_sqlite_sidecars(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("opening company store {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("securing company store {}", path.display()))?;
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        validate_sqlite_sidecars(&path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&connection)?;
        super::company_directory::migrate(&connection)?;
        validate_company_text_bounds(&connection)?;
        let runtime_epoch = format!("epoch_{}", uuid::Uuid::new_v4().simple());
        let runtime_claim = crate::config::private_io::try_execution_claim(
            &runtime_claim_path(&path, &runtime_epoch),
        )?.context("new company runtime identity is already owned")?;
        Ok(Self {
            path,
            runtime_epoch,
            connection: Mutex::new(connection),
            _runtime_claim: runtime_claim,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }

    /// Only the gateway startup boundary owns legacy job recovery. Opening
    /// the database for a helper, inspector or another scheduler is harmless.
    pub fn recover_interrupted_jobs(&self) -> Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.execute(
            "UPDATE company_jobs SET state='stale',settled_at=?1
             WHERE state NOT IN ('completed_verified','completed_unverified','failed','superseded','stale')",
            [Utc::now().to_rfc3339()],
        )?)
    }

    pub(crate) fn workflow_lease_recoverable(
        &self, owner_epoch: Option<&str>, expires_at: Option<&str>, now: &str,
    ) -> Result<bool> {
        if expires_at.is_some_and(|expires| expires <= now) {
            return Ok(true);
        }
        let Some(epoch) = owner_epoch else { return Ok(false) };
        if epoch == self.runtime_epoch() { return Ok(false); }
        anyhow::ensure!(epoch.len() <= 256, "workflow runtime identity exceeds safe bound");
        let claim_path = runtime_claim_path(&self.path, epoch);
        match std::fs::symlink_metadata(&claim_path) {
            // Pre-claim builds have no OS ownership record. Do not invent a
            // dead-owner proof during rolling migration; honor their deadline.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("cannot inspect workflow runtime ownership"),
            Ok(_) => {}
        }
        // Nonblocking and fail closed on filesystem errors. Retain lock inodes:
        // unlinking them could allow two owners of different inodes.
        Ok(crate::config::private_io::try_execution_claim(
            &claim_path,
        )?.is_some())
    }

    /// Append one typed company-directory mutation to the canonical event
    /// stream. UI, CLI, onboarding, and agents all use this same gate so a
    /// rename, archive, group change, or responsibility assignment cannot
    /// bypass audit/idempotency semantics.
    pub fn apply_directory_change(
        &self,
        actor_agent_id: &str,
        idempotency_key: impl Into<String>,
        change: super::company_directory::DirectoryChange,
    ) -> Result<CompanyEvent> {
        self.append(NewCompanyEvent {
            run_id: "company-directory".to_string(),
            session_id: "company-directory".to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(actor_agent_id.to_string()),
            agent_instance_id: Some(actor_agent_id.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(idempotency_key.into()),
            event: CompanyEventKind::DirectoryChanged { change },
        })
    }

    pub fn directory_snapshot(&self) -> Result<super::company_directory::DirectorySnapshot> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        super::company_directory::snapshot(&connection)
    }

    /// Reserve the authoritative execution receipt for one group-authored
    /// turn. The exact prompt hash, roster authority, stable activation set,
    /// and immutable participant snapshots are compared on retry. Reusing a
    /// turn id with any substituted payload fails closed; an exact retry
    /// returns the same record and preserves its current member states.
    pub fn reserve_group_turn(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        prompt: &str,
        intent: &super::group_conversation::GroupActivationIntent,
        participants: &[super::group_conversation::GroupParticipant],
    ) -> Result<super::group_conversation::GroupTurnLedgerReservation> {
        use super::group_conversation::{GroupMemberActivationState, GroupTurnLedgerReservation};

        crate::session::SessionStore::validate_session_id(canonical_session_id)?;
        validate_group_turn_id(turn_id)?;
        anyhow::ensure!(!intent.group_id.trim().is_empty(), "group id is empty");
        anyhow::ensure!(
            !intent.roster_fingerprint.trim().is_empty(),
            "group roster fingerprint is empty"
        );
        let participant_ids = participants
            .iter()
            .map(|participant| participant.agent_id.as_str())
            .collect::<Vec<_>>();
        let active_ids = intent
            .active_agent_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            participant_ids == active_ids,
            "group activation snapshots do not match the authoritative active roster order"
        );
        let mut seen = std::collections::HashSet::new();
        anyhow::ensure!(
            participants
                .iter()
                .all(|participant| seen.insert(participant.agent_id.as_str())),
            "group activation contains duplicate participant snapshots"
        );

        let prompt_hash = group_prompt_hash(prompt);
        let selection = match intent.selection {
            super::group_conversation::GroupActivationSelection::Explicit => "explicit",
            super::group_conversation::GroupActivationSelection::Everyone => "everyone",
        };
        let active_agent_ids_json = serde_json::to_string(&intent.active_agent_ids)?;
        let expected_members = participants
            .iter()
            .map(|participant| {
                (
                    stable_group_activation_id(
                        canonical_session_id,
                        turn_id,
                        &intent.group_id,
                        &participant.agent_id,
                    ),
                    participant,
                )
            })
            .collect::<Vec<_>>();

        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_group_turn_ledger_record(&tx, canonical_session_id, turn_id)? {
            let immutable_matches = existing.prompt_hash == prompt_hash
                && existing.group_id == intent.group_id
                && existing.roster_fingerprint == intent.roster_fingerprint
                && existing.selection == intent.selection
                && existing.active_agent_ids == intent.active_agent_ids
                && existing.activation.as_ref().is_none_or(|stored| stored == intent)
                && existing.members.iter().filter(|member| member.source_receipt_id.is_none()).count() == expected_members.len()
                && existing.members.iter().filter(|member| member.source_receipt_id.is_none()).zip(&expected_members).all(
                    |(stored, (activation_id, participant))| {
                        stored.activation_id == *activation_id
                            && stored.participant == **participant
                    },
                );
            anyhow::ensure!(
                immutable_matches,
                "group turn_id was already used with a different prompt, roster, or activation payload"
            );
            tx.commit()?;
            return Ok(GroupTurnLedgerReservation::Existing(existing));
        }

        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO company_group_turns(
                canonical_session_id,turn_id,prompt_hash,group_id,roster_fingerprint,
                selection,active_agent_ids_json,created_at,updated_at,activation_json,original_request)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,?10)",
            params![
                canonical_session_id,
                turn_id,
                prompt_hash,
                intent.group_id,
                intent.roster_fingerprint,
                selection,
                active_agent_ids_json,
                now,
                serde_json::to_string(intent)?,
                prompt,
            ],
        )?;
        for (ordinal, (activation_id, participant)) in expected_members.iter().enumerate() {
            let participant_json = serde_json::to_string(participant)?;
            tx.execute(
                "INSERT INTO company_group_turn_members(
                    canonical_session_id,turn_id,activation_id,activation_ordinal,
                    agent_id,participant_json,state,status_detail,receipt_id,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,'queued','',NULL,?7)",
                params![
                    canonical_session_id,
                    turn_id,
                    activation_id,
                    i64::try_from(ordinal).context("group activation ordinal overflow")?,
                    participant.agent_id,
                    participant_json,
                    now,
                ],
            )?;
        }
        let record = load_group_turn_ledger_record(&tx, canonical_session_id, turn_id)?
            .context("new group turn receipt disappeared before commit")?;
        debug_assert!(record.members.is_empty() || !record.is_done());
        debug_assert!(record
            .members
            .iter()
            .all(|member| member.state == GroupMemberActivationState::Queued));
        tx.commit()?;
        Ok(GroupTurnLedgerReservation::New(record))
    }

    /// Add one explicitly pinged room member without changing the human's
    /// immutable activation intent. The caller binds this to a saved canonical
    /// contribution first. The member key prevents cycles and producer retries.
    pub(crate) fn reserve_group_ping(
        &self,
        group: &super::group_conversation::GroupTurnContext,
        turn_id: &str,
        source_agent_id: &str,
        source_receipt_id: &str,
        target: &super::group_conversation::GroupParticipant,
    ) -> Result<bool> {
        crate::session::SessionStore::validate_session_id(&group.canonical_session_id)?;
        validate_group_turn_id(turn_id)?;
        anyhow::ensure!(!source_receipt_id.is_empty() && source_receipt_id.len() <= 512,
            "group ping requires its saved source receipt");
        anyhow::ensure!(source_agent_id != target.agent_id, "a room ping cannot wake its author");
        let directory = self.directory_snapshot()?;
        // Recheck current membership and lifecycle: a saved roster is not
        // authority to wake someone removed or disabled while a peer worked.
        for id in [source_agent_id, target.agent_id.as_str()] {
            if !directory.members.iter().any(|member| member.group_id == group.group_id && member.agent_id == id)
                || !directory.agents.iter().any(|agent| agent.profile.agent_id == id
                    && agent.profile.lifecycle == super::company_directory::LifecycleState::Active) {
                return Ok(false);
            }
        }
        anyhow::ensure!(directory.agents.iter().any(|agent|
            agent.profile.agent_id == target.agent_id && agent.profile.internal_role == target.internal_role),
            "group ping target runtime identity changed");
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let turn = load_group_turn_ledger_record(&tx, &group.canonical_session_id, turn_id)?
            .context("group ping has no originating turn")?;
        anyhow::ensure!(turn.group_id == group.group_id, "group ping room does not match its turn");
        anyhow::ensure!(turn.members.iter().any(|member| member.participant.agent_id == source_agent_id),
            "group ping author was not activated in this turn");
        if turn.members.iter().any(|member| member.participant.agent_id == target.agent_id) {
            tx.commit()?;
            return Ok(false);
        }
        let now = Utc::now().to_rfc3339();
        tx.execute("INSERT INTO company_group_turn_members(
            canonical_session_id,turn_id,activation_id,activation_ordinal,agent_id,
            participant_json,state,status_detail,receipt_id,source_receipt_id,updated_at)
            VALUES(?1,?2,?3,?4,?5,?6,'queued','Mentioned by a room member',NULL,?7,?8)",
            params![group.canonical_session_id, turn_id,
                stable_group_activation_id(&group.canonical_session_id, turn_id, &group.group_id, &target.agent_id),
                i64::try_from(turn.members.len())?, target.agent_id, serde_json::to_string(target)?, source_receipt_id, now])?;
        tx.execute("UPDATE company_group_turns SET updated_at=?3 WHERE canonical_session_id=?1 AND turn_id=?2",
            params![group.canonical_session_id, turn_id, now])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn group_turn(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
    ) -> Result<Option<super::group_conversation::GroupTurnLedgerRecord>> {
        crate::session::SessionStore::validate_session_id(canonical_session_id)?;
        validate_group_turn_id(turn_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        load_group_turn_ledger_record(&connection, canonical_session_id, turn_id)
    }

    pub fn mark_group_member_working(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        self.transition_group_member_activation(
            canonical_session_id,
            turn_id,
            agent_id,
            super::group_conversation::GroupMemberActivationState::Working,
            "Activation is executing",
            None,
        ).and_then(|admitted| {
            anyhow::ensure!(admitted, "group assignment has transferred to a successor; obsolete execution refused");
            Ok(())
        })
    }

    pub fn mark_group_member_waiting_user(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
        ask_id: &str,
    ) -> Result<()> {
        anyhow::ensure!(!ask_id.trim().is_empty(), "waiting-user receipt is empty");
        self.transition_group_member_activation(
            canonical_session_id,
            turn_id,
            agent_id,
            super::group_conversation::GroupMemberActivationState::WaitingUser,
            "Waiting for a user answer",
            Some(ask_id),
        ).map(|_| ())
    }

    pub fn mark_group_member_blocked(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.transition_group_member_activation(
            canonical_session_id,
            turn_id,
            agent_id,
            super::group_conversation::GroupMemberActivationState::Blocked,
            reason,
            None,
        ).map(|_| ())
    }

    /// Input admission can race with an answer continuation. Return whether
    /// this activation still owns the transition before publishing its status.
    pub(crate) fn reject_group_member_input(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<bool> {
        self.transition_group_member_activation(
            canonical_session_id, turn_id, agent_id,
            super::group_conversation::GroupMemberActivationState::Blocked,
            reason, None,
        )
    }

    /// `done` requires the id of the group contribution that was already
    /// committed to the canonical transcript. This makes it impossible for a
    /// silent run or a merely closed provider stream to masquerade as done.
    pub fn mark_group_member_done(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
        persisted_message_id: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            !persisted_message_id.trim().is_empty(),
            "done requires a persisted group contribution receipt"
        );
        self.transition_group_member_activation(
            canonical_session_id,
            turn_id,
            agent_id,
            super::group_conversation::GroupMemberActivationState::Done,
            "Contribution persisted in the canonical group transcript",
            Some(persisted_message_id),
        ).map(|_| ())
    }

    /// Bind before publishing a popup: answers can arrive while its producer
    /// is still running, before it emits a waiting contribution.
    pub fn bind_group_ask(&self, session_id: &str, turn_id: &str, agent_id: &str, ask_id: &str) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_group_turn_id(turn_id)?;
        anyhow::ensure!(!ask_id.is_empty() && ask_id.len() <= 256, "invalid group ask id");
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String,String)> = tx.query_row("SELECT turn_id,agent_id FROM company_group_asks WHERE canonical_session_id=?1 AND ask_id=?2",params![session_id,ask_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some(owner) = existing {
            anyhow::ensure!(owner == (turn_id.to_string(),agent_id.to_string()), "group ask owner cannot change");
        } else {
            let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM company_group_turn_members WHERE canonical_session_id=?1 AND turn_id=?2 AND agent_id=?3 AND state IN ('working','waiting_user'))",params![session_id,turn_id,agent_id],|r|r.get(0))?;
            anyhow::ensure!(active, "group ask requires its executing owner");
            tx.execute("INSERT INTO company_group_asks(canonical_session_id,ask_id,turn_id,agent_id) VALUES(?1,?2,?3,?4)",params![session_id,ask_id,turn_id,agent_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomically refuse to finish the obsolete producer after ownership has
    /// moved to an answer continuation. False means retain the successor.
    pub(crate) fn settle_group_member_result(&self, session_id: &str, turn_id: &str, agent_id: &str, message_id: &str, pending_ask: Option<&str>) -> Result<bool> {
        use super::group_conversation::GroupMemberActivationState as State;
        let (state,detail,receipt) = if let Some(ask) = pending_ask {
            (State::WaitingUser,"Waiting for a user answer",ask)
        } else {
            (State::Done,"Contribution persisted in the canonical group transcript",message_id)
        };
        self.transition_group_member_activation(session_id,turn_id,agent_id,state,detail,Some(receipt))
    }

    /// The caller resolved this failure from the exact canonical contribution.
    /// Its durable receipt preserves partial evidence without admitting dependents.
    pub(crate) fn settle_group_member_failure(&self, session_id: &str, turn_id: &str, agent_id: &str, message_id: &str) -> Result<bool> {
        anyhow::ensure!(!message_id.trim().is_empty(), "failed contribution requires a durable receipt");
        self.transition_group_member_activation(session_id, turn_id, agent_id,
            super::group_conversation::GroupMemberActivationState::Blocked,
            "Contribution preserved; task did not finish", Some(message_id))
    }

    pub(crate) fn group_member_ask_ids(&self, session_id: &str, turn_id: &str, agent_id: &str) -> Result<Vec<String>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_group_turn_id(turn_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let mut statement = connection.prepare("SELECT ask_id FROM company_group_asks WHERE canonical_session_id=?1 AND turn_id=?2 AND agent_id=?3 UNION SELECT receipt_id FROM company_group_turn_members WHERE canonical_session_id=?1 AND turn_id=?2 AND agent_id=?3 AND state='waiting_user' AND receipt_id IS NOT NULL")?;
        let rows = statement.query_map(params![session_id,turn_id,agent_id],|row|row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Question files referenced by durable assignments are not disposable
    /// popup history. Retain them until task-aware archival removes the binding.
    pub(crate) fn group_ask_is_bound(&self, session_id: &str, ask_id: &str) -> Result<bool> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_group_asks WHERE canonical_session_id=?1 AND ask_id=?2 UNION ALL SELECT 1 FROM company_group_turn_members WHERE canonical_session_id=?1 AND state='waiting_user' AND receipt_id=?2)",
            params![session_id, ask_id], |row| row.get(0),
        )?)
    }

    /// Find the immutable group-turn receipt that owns a pending popup. This
    /// lets the late-answer path carry forward downstream queued coworkers,
    /// rather than waking only the asker and stranding the rest of the plan.
    pub fn group_turn_waiting_on_ask(
        &self,
        canonical_session_id: &str,
        ask_id: &str,
    ) -> Result<Option<super::group_conversation::GroupTurnLedgerRecord>> {
        crate::session::SessionStore::validate_session_id(canonical_session_id)?;
        anyhow::ensure!(!ask_id.trim().is_empty(), "ask receipt is empty");
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let matches = {
            let mut statement = connection.prepare(
                "SELECT turn_id FROM company_group_asks WHERE canonical_session_id=?1 AND ask_id=?2
                 UNION SELECT turn_id FROM company_group_turn_members
                 WHERE canonical_session_id=?1 AND state='waiting_user' AND receipt_id=?2
                 LIMIT 2",
            )?;
            let rows = statement.query_map(params![canonical_session_id, ask_id], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        anyhow::ensure!(
            matches.len() <= 1,
            "ask receipt `{ask_id}` is linked to multiple group activations"
        );
        let Some(turn_id) = matches.into_iter().next() else {
            return Ok(None);
        };
        load_group_turn_ledger_record(&connection, canonical_session_id, &turn_id)
    }

    /// Link a detached popup answer to the durable continuation that will
    /// resume the work. The original activation stops being `waiting_user`
    /// as soon as that continuation is safely queued, but it is not falsely
    /// marked done; its blocked receipt points at the successor turn.
    pub fn supersede_waiting_group_activation(
        &self,
        canonical_session_id: &str,
        ask_id: &str,
        continuation_turn_id: &str,
    ) -> Result<Option<String>> {
        crate::session::SessionStore::validate_session_id(canonical_session_id)?;
        validate_group_turn_id(continuation_turn_id)?;
        anyhow::ensure!(!ask_id.trim().is_empty(), "ask receipt is empty");
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let matches = {
            let mut statement = tx.prepare(
                "SELECT turn_id,agent_id FROM company_group_asks WHERE canonical_session_id=?1 AND ask_id=?2
                 UNION SELECT turn_id,agent_id FROM company_group_turn_members
                 WHERE canonical_session_id=?1 AND state='waiting_user' AND receipt_id=?2
                 LIMIT 2",
            )?;
            let rows = statement.query_map(params![canonical_session_id, ask_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        anyhow::ensure!(
            matches.len() <= 1,
            "ask receipt `{ask_id}` is linked to multiple group activations"
        );
        let Some((original_turn_id, agent_id)) = matches.into_iter().next() else {
            tx.commit()?;
            return Ok(None);
        };
        let original = load_group_turn_ledger_record(&tx, canonical_session_id, &original_turn_id)?
            .context("waiting group turn disappeared")?;
        if original.members.iter().any(|member| member.participant.agent_id == agent_id && member.receipt_id.as_deref() == Some(continuation_turn_id) && member.status_detail == format!("User answered; work continues in successor turn {continuation_turn_id}")) {
            tx.commit()?;
            return Ok(Some(original_turn_id));
        }
        let frontier = original.answer_frontier(&agent_id);
        let now = Utc::now().to_rfc3339();
        let detail =
            format!("User answered; work continues in successor turn {continuation_turn_id}");
        let changed = tx.execute(
            "UPDATE company_group_turn_members
             SET state='blocked',status_detail=?1,receipt_id=?2,updated_at=?3
             WHERE canonical_session_id=?4 AND turn_id=?5 AND agent_id=?6
               AND ((state='waiting_user' AND receipt_id=?7) OR state='working'
                 OR (state='blocked' AND status_detail='Gateway restarted while this activation was working (stale)'))",
            params![
                detail,
                continuation_turn_id,
                now,
                canonical_session_id,
                original_turn_id,
                agent_id,
                ask_id,
            ],
        )?;
        anyhow::ensure!(
            changed == 1,
            "waiting group activation changed concurrently"
        );
        // Transfer only tasks unlocked by this answer. A sibling's unanswered
        // branch and its joins retain their original ownership and receipts.
        for next_agent_id in frontier.iter().filter(|id| **id != agent_id) {
            tx.execute(
            "UPDATE company_group_turn_members
             SET state='blocked',status_detail=?1,receipt_id=?2,updated_at=?3
             WHERE canonical_session_id=?4 AND turn_id=?5 AND state='queued' AND agent_id=?6",
            params![
                detail,
                continuation_turn_id,
                now,
                canonical_session_id,
                original_turn_id,
                next_agent_id,
            ],
        )?;
        }
        tx.execute(
            "UPDATE company_group_turns SET updated_at=?1
             WHERE canonical_session_id=?2 AND turn_id=?3",
            params![now, canonical_session_id, original_turn_id],
        )?;
        tx.commit()?;
        Ok(Some(original_turn_id))
    }

    /// Mark activations that were executing when the gateway process ended as
    /// blocked/stale. Queued work remains resumable and terminal evidence is
    /// never rewritten. The gateway calls this exactly once at its startup
    /// boundary; read-only processes that merely open CompanyStore do not.
    pub fn recover_stale_group_activations(&self) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let changed = connection.execute(
            "UPDATE company_group_turn_members
             SET state='blocked',
                 status_detail='Gateway restarted while this activation was working (stale)',
                 receipt_id=NULL,updated_at=?1
             WHERE state='working'",
            [&now],
        )?;
        if changed > 0 {
            connection.execute(
                "UPDATE company_group_turns SET updated_at=?1
                 WHERE EXISTS(
                    SELECT 1 FROM company_group_turn_members member
                    WHERE member.canonical_session_id=company_group_turns.canonical_session_id
                      AND member.turn_id=company_group_turns.turn_id
                      AND member.updated_at=?1
                 )",
                [&now],
            )?;
        }
        Ok(changed)
    }

    fn transition_group_member_activation(
        &self,
        canonical_session_id: &str,
        turn_id: &str,
        agent_id: &str,
        next: super::group_conversation::GroupMemberActivationState,
        detail: &str,
        receipt_id: Option<&str>,
    ) -> Result<bool> {
        use super::group_conversation::GroupMemberActivationState as State;

        crate::session::SessionStore::validate_session_id(canonical_session_id)?;
        validate_group_turn_id(turn_id)?;
        anyhow::ensure!(
            !agent_id.trim().is_empty(),
            "group activation agent id is empty"
        );
        let detail = detail.chars().take(2_000).collect::<String>();
        if matches!(next, State::WaitingUser | State::Done) {
            anyhow::ensure!(
                receipt_id.is_some_and(|receipt| !receipt.trim().is_empty()),
                "{} requires a durable receipt",
                next.as_str()
            );
        }
        if let Some(receipt) = receipt_id {
            anyhow::ensure!(receipt.len() <= 256, "group activation receipt is too long");
        }

        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, current_detail, current_receipt): (String, String, Option<String>) = tx
            .query_row(
                "SELECT state,status_detail,receipt_id FROM company_group_turn_members
                 WHERE canonical_session_id=?1 AND turn_id=?2 AND agent_id=?3",
                params![canonical_session_id, turn_id, agent_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .with_context(|| {
                format!("unknown group activation for `{agent_id}` in turn `{turn_id}`")
            })?;
        let current = State::from_str(&current)?;
        if current == State::Blocked && current_receipt.as_deref().is_some_and(|receipt| current_detail == format!("User answered; work continues in successor turn {receipt}") || current_detail == format!("Work continues in successor turn {receipt}")) {
            tx.commit()?;
            return Ok(false);
        }
        if next == State::Working {
            // Admission, not the caller's earlier frontier snapshot, is the
            // authority for execution. Check dependencies under this same
            // write transaction; a waiting peer is not a completed input.
            let record = load_group_turn_ledger_record(&tx, canonical_session_id, turn_id)?
                .context("group admission lost its saved turn")?;
            if let Some(plan) = &record.activation {
                let edges = plan.execution_dependencies.clone().unwrap_or_else(||
                    super::group_conversation::legacy_wave_dependencies(&plan.execution_waves));
                for edge in edges.iter().filter(|edge| edge.dependent == agent_id) {
                    anyhow::ensure!(record.members.iter().any(|member|
                        member.participant.agent_id == edge.prerequisite
                            && member.has_committed_result()),
                        "group assignment `{agent_id}` is waiting for committed input from `{}`", edge.prerequisite);
                }
            }
        }
        // Earlier versions confused a saved error with a successful result.
        // Correct only that same receipt, or an interrupted blocked assignment
        // that has no result yet. Successor ownership was checked above.
        let reconciling_failure = next == State::Blocked
            && detail == "Contribution preserved; task did not finish"
            && receipt_id.is_some()
            && (current_receipt.as_deref() == receipt_id
                || (current == State::Blocked && current_receipt.is_none()));
        if current == next {
            if current_detail == detail && current_receipt.as_deref() == receipt_id {
                tx.commit()?;
                return Ok(true);
            }
            anyhow::ensure!(
                reconciling_failure,
                "group activation retry attempted to substitute the durable {} receipt",
                next.as_str()
            );
        }
        let allowed = matches!(
            (current, next),
            // Recovery may discover a committed contribution before execution
            // starts. A receipt-backed completion must not fake a working phase.
            (State::Queued, State::Working | State::Blocked | State::Done)
                | (
                    State::Working,
                    State::WaitingUser | State::Blocked | State::Done
                )
                | (
                    State::WaitingUser,
                    State::Working | State::Blocked | State::Done
                )
                | (State::Blocked, State::Working | State::Done)
        );
        anyhow::ensure!(
            allowed || reconciling_failure,
            "invalid group activation transition {} -> {}",
            current.as_str(),
            next.as_str()
        );
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE company_group_turn_members
             SET state=?1,status_detail=?2,receipt_id=?3,updated_at=?4
             WHERE canonical_session_id=?5 AND turn_id=?6 AND agent_id=?7",
            params![
                next.as_str(),
                detail,
                receipt_id,
                now,
                canonical_session_id,
                turn_id,
                agent_id,
            ],
        )?;
        anyhow::ensure!(changed == 1, "group activation transition was lost");
        if next == State::Done {
            // A resumed task may itself have asked another question. Reconcile
            // the entire ownership chain in the same transaction as its result,
            // without touching sibling asks or unrelated blocked activations.
            let ancestors = {
                let mut statement = tx.prepare(
                    "WITH RECURSIVE ancestors(turn_id) AS (
                        SELECT ?2
                        UNION
                        SELECT member.turn_id FROM company_group_turn_members member
                        JOIN ancestors ON member.receipt_id=ancestors.turn_id
                        WHERE member.canonical_session_id=?1 AND member.agent_id=?3
                          AND member.state='blocked'
                          AND member.status_detail IN (
                              'User answered; work continues in successor turn ' || ancestors.turn_id,
                              'Work continues in successor turn ' || ancestors.turn_id)
                     ) SELECT turn_id FROM ancestors WHERE turn_id<>?2",
                )?;
                let rows = statement.query_map(
                    params![canonical_session_id, turn_id, agent_id],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for ancestor in ancestors {
                tx.execute(
                    "UPDATE company_group_turn_members
                     SET state='done',status_detail=?1,receipt_id=?2,updated_at=?3
                     WHERE canonical_session_id=?4 AND turn_id=?5 AND agent_id=?6",
                    params![detail, receipt_id, now, canonical_session_id, ancestor, agent_id],
                )?;
                tx.execute(
                    "UPDATE company_group_turns SET updated_at=?1
                     WHERE canonical_session_id=?2 AND turn_id=?3",
                    params![now, canonical_session_id, ancestor],
                )?;
                persist_ready_group_continuation(&tx, canonical_session_id, &ancestor)?;
            }
            // A successor can now finish BEFORE an independent original
            // sibling. Re-evaluate that original join when the sibling later
            // finishes. Only reconciled turns use this handoff: ordinary
            // same-run dependencies remain owned by the live dispatcher.
            let has_reconciled_result: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM company_group_turn_members original
                 JOIN company_group_turn_members successor
                   ON successor.canonical_session_id=original.canonical_session_id
                  AND successor.agent_id=original.agent_id AND successor.receipt_id=original.receipt_id
                  AND successor.turn_id<>original.turn_id AND successor.state='done'
                 WHERE original.canonical_session_id=?1 AND original.turn_id=?2
                   AND original.state='done' AND original.receipt_id IS NOT NULL)",
                params![canonical_session_id, turn_id], |row| row.get(0),
            )?;
            if has_reconciled_result {
                persist_ready_group_continuation(&tx, canonical_session_id, turn_id)?;
            }
        }
        tx.execute(
            "UPDATE company_group_turns SET updated_at=?1
             WHERE canonical_session_id=?2 AND turn_id=?3",
            params![now, canonical_session_id, turn_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Persisted outbox; reads do not claim work. Enqueue by deterministic turn
    /// id, then acknowledge, so interruption between stores is replay-safe.
    pub fn pending_group_continuations(&self) -> Result<Vec<GroupReadyContinuation>> {
        let (ready, failures) = self.scan_group_continuations(None)?;
        anyhow::ensure!(failures.is_empty(), "invalid group continuations: {}", failures.join("; "));
        Ok(ready)
    }

    /// Keep malformed outbox records pending and visible without starving
    /// unrelated ready work. Scope in SQL, before decoding another room's data.
    pub(crate) fn scan_group_continuations(&self, only_session: Option<&str>) -> Result<(Vec<GroupReadyContinuation>, Vec<String>)> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let mut statement = connection.prepare(
            "SELECT canonical_session_id,turn_id,payload_json FROM company_group_continuations
             WHERE dispatched=0 AND (?1 IS NULL OR canonical_session_id=?1) ORDER BY rowid",
        )?;
        let rows = statement.query_map([only_session], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
        let mut ready = Vec::new();
        let mut failures = Vec::new();
        for row in rows {
            let (session_id, turn_id, payload) = row?;
            match serde_json::from_str::<GroupReadyContinuation>(&payload) {
                Ok(item) if item.canonical_session_id == session_id && item.turn_id == turn_id => ready.push(item),
                Ok(_) => failures.push(format!("{session_id}/{turn_id}: outbox identity does not match its payload")),
                Err(error) => failures.push(format!("{session_id}/{turn_id}: invalid outbox payload: {error}")),
            }
        }
        Ok((ready, failures))
    }

    pub fn acknowledge_group_continuation(&self, session_id: &str, turn_id: &str) -> Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let changed = connection.execute(
            "UPDATE company_group_continuations SET dispatched=1 WHERE canonical_session_id=?1 AND turn_id=?2",
            params![session_id, turn_id],
        )?;
        anyhow::ensure!(changed == 1, "unknown group continuation receipt");
        Ok(())
    }

    pub fn freeze_group_continuation_dispatch(&self, session_id: &str, turn_id: &str, payload: &str) -> Result<String> {
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE company_group_continuations SET dispatch_payload_json=COALESCE(dispatch_payload_json,?1)
             WHERE canonical_session_id=?2 AND turn_id=?3",
            params![payload, session_id, turn_id],
        )?;
        let saved = tx.query_row(
            "SELECT dispatch_payload_json FROM company_group_continuations WHERE canonical_session_id=?1 AND turn_id=?2",
            params![session_id, turn_id], |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn group_continuation_dispatch(&self, session_id: &str, turn_id: &str) -> Result<Option<String>> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.query_row(
            "SELECT dispatch_payload_json FROM company_group_continuations WHERE canonical_session_id=?1 AND turn_id=?2",
            params![session_id, turn_id], |row| row.get(0),
        )?)
    }

    /// UI read cursors are durable local state, not model-authored company
    /// events. A stale window may never move a cursor backward.
    pub fn mark_conversation_read(
        &self,
        item: &SidebarItemKey,
        transcript_revision: u64,
    ) -> Result<()> {
        let snapshot = self.directory_snapshot()?;
        let (owner_kind, owner_id, exists) = match item {
            SidebarItemKey::Agent(agent_id) => (
                "agent",
                agent_id.as_str(),
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.profile.agent_id == *agent_id),
            ),
            SidebarItemKey::Group(group_id) => (
                "group",
                group_id.as_str(),
                snapshot
                    .groups
                    .iter()
                    .any(|group| group.profile.group_id == *group_id),
            ),
        };
        anyhow::ensure!(exists, "unknown {owner_kind} `{owner_id}`");
        let revision = i64::try_from(transcript_revision)
            .context("conversation revision is too large to persist")?;
        let now = Utc::now().to_rfc3339();
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        connection.execute(
            "INSERT INTO company_conversation_reads(
                owner_kind,owner_id,last_read_revision,read_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(owner_kind,owner_id) DO UPDATE SET
                read_at=CASE WHEN excluded.last_read_revision >= last_read_revision
                    THEN excluded.read_at ELSE read_at END,
                last_read_revision=MAX(last_read_revision,excluded.last_read_revision)",
            params![owner_kind, owner_id, revision, now],
        )?;
        Ok(())
    }

    pub fn conversation_read_markers(&self) -> Result<Vec<ConversationReadMarker>> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let mut statement = connection.prepare(
            "SELECT owner_kind,owner_id,last_read_revision,read_at
             FROM company_conversation_reads ORDER BY owner_kind,owner_id",
        )?;
        let rows = statement.query_map([], |row| {
            let owner_kind: String = row.get(0)?;
            let owner_id: String = row.get(1)?;
            let revision: i64 = row.get(2)?;
            let item = match owner_kind.as_str() {
                "agent" => SidebarItemKey::Agent(owner_id),
                "group" => SidebarItemKey::Group(owner_id),
                _ => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("invalid conversation owner kind `{owner_kind}`"),
                        )),
                    ))
                }
            };
            let last_read_revision = u64::try_from(revision).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(ConversationReadMarker {
                item,
                last_read_revision,
                read_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn pending_company_messages(&self, session_id: &str) -> Result<Vec<PendingCompanyMessage>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        // A handoff nobody delivered for half a day belongs to work that has
        // moved on. Delivering it anyway revived a cancelled challenge's
        // portfolio job 44 hours later, the moment an unrelated turn began.
        let stale_before = (chrono::Utc::now() - chrono::Duration::hours(STALE_UNDELIVERED_MESSAGE_HOURS)).to_rfc3339();
        connection.execute(
            "UPDATE company_messages SET state='canceled'
              WHERE session_id=?1 AND state='accepted' AND accepted_at<?2",
            params![session_id, stale_before],
        )?;
        let mut statement = connection.prepare(
            "SELECT message_id,operation_id,handoff_id,reply_to,causation_id,
                    from_agent,to_agent,subject,body,reply_expected
             FROM company_messages
             WHERE session_id=?1 AND state='accepted'
             ORDER BY as_of_seq,message_id",
        )?;
        let rows = statement.query_map([session_id], |row| {
            let message_id: String = row.get(0)?;
            let persisted_handoff_id: String = row.get(2)?;
            Ok(PendingCompanyMessage {
                handoff_id: if persisted_handoff_id.is_empty() {
                    message_id.clone()
                } else {
                    persisted_handoff_id
                },
                message_id,
                operation_id: row.get(1)?,
                reply_to: row.get(3)?,
                causation_id: row.get(4)?,
                from: row.get(5)?,
                to: row.get(6)?,
                subject: row.get(7)?,
                body: row.get(8)?,
                reply_expected: row.get::<_, i64>(9)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The user stopped every agent in this conversation: nothing still
    /// waiting to be delivered there may start work on a later turn.
    pub fn cancel_all_pending_messages(&self, session_id: &str) -> Result<usize> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.execute(
            "UPDATE company_messages SET state='canceled' WHERE session_id=?1 AND state IN ('accepted','suspended')",
            params![session_id],
        )?)
    }

    /// After a stopped group execution has actually exited, retire only its
    /// undelivered handoffs. A later user turn must not recover canceled work.
    /// Follow durable lineage even if cancellation beat the delivery claim;
    /// injected receipts and independently running turns remain untouched.
    pub fn cancel_pending_group_turn_messages(&self, session_id: &str, turn_id: &str) -> Result<usize> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_company_message_identity("stopped turn id", turn_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.execute(
            "WITH RECURSIVE owned(message_id,handoff_id) AS (
                SELECT m.message_id,m.handoff_id FROM company_messages m
                 WHERE m.session_id=?1 AND (m.causation_id=?2 OR EXISTS(
                    SELECT 1 FROM company_message_claims c
                     WHERE c.message_id=m.message_id AND c.session_id=?1 AND c.owner_task_id=?2
                 ))
                UNION
                SELECT m.message_id,m.handoff_id FROM company_messages m JOIN owned parent
                  ON m.causation_id IN (parent.message_id,parent.handoff_id)
                    OR m.reply_to IN (parent.message_id,parent.handoff_id)
                 WHERE m.session_id=?1
             )
             UPDATE company_messages SET state='canceled'
              WHERE session_id=?1 AND state IN ('accepted','suspended')
                AND message_id IN (SELECT message_id FROM owned)",
            params![session_id,turn_id],
        )?)
    }

    /// Claim one accepted delivery for a specific task attempt. The OS lock is
    /// authoritative for live ownership; SQLite retains the last claimant for
    /// recovery diagnostics. Never steal on elapsed time or a new connection.
    pub(crate) fn try_claim_company_message(
        &self,
        session_id: &str,
        message_id: &str,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<Option<crate::config::private_io::PrivateExecutionClaim>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        for id in [message_id, task_id, attempt_id] {
            anyhow::ensure!(!id.trim().is_empty() && id.len() <= 256, "invalid company message claim identity");
        }
        let claim_name = format!("{:x}.lock", Sha256::digest(format!("{session_id}\0{message_id}").as_bytes()));
        let claim_path = self.path.with_extension("message-claims").join(claim_name);
        let Some(claim) = crate::config::private_io::try_execution_claim(&claim_path)? else { return Ok(None) };
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let accepted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_messages WHERE session_id=?1 AND message_id=?2 AND state='accepted')",
            params![session_id,message_id], |row| row.get(0),
        )?;
        if !accepted { return Ok(None) }
        tx.execute(
            "INSERT INTO company_message_claims(message_id,session_id,owner_task_id,owner_attempt_id,claimed_at)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(message_id) DO UPDATE SET
             owner_task_id=excluded.owner_task_id,owner_attempt_id=excluded.owner_attempt_id,claimed_at=excluded.claimed_at",
            params![message_id,session_id,task_id,attempt_id,Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(Some(claim))
    }

    /// Permanently stop a legacy/corrupt self-addressed receipt before gateway
    /// rehydration. New self messages are rejected at append time; this is the
    /// recovery gate for rows written by an older runtime.
    pub fn quarantine_self_company_message(
        &self,
        session_id: &str,
        message_id: &str,
        from: &str,
        to: &str,
    ) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        anyhow::ensure!(
            crate::runtime::mailbox::same_agent_identity(from, to),
            "refusing to quarantine non-self company message `{message_id}`"
        );
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let changed = connection.execute(
            "UPDATE company_messages
                SET state='canceled', injected_at=COALESCE(injected_at,?1)
              WHERE message_id=?2 AND session_id=?3 AND from_agent=?4 AND to_agent=?5
                AND state IN ('accepted','suspended')",
            params![Utc::now().to_rfc3339(), message_id, session_id, from, to],
        )?;
        anyhow::ensure!(
            changed == 1,
            "self-addressed company message `{message_id}` was not pending"
        );
        Ok(())
    }

    pub fn accept_company_message(
        &self,
        session_id: &str,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        reply_expected: bool,
    ) -> Result<String> {
        let operation_id = format!("operation_{}", uuid::Uuid::new_v4().simple());
        self.accept_company_message_with_identity(
            session_id,
            &operation_id,
            None,
            None,
            None,
            from,
            to,
            subject,
            body,
            reply_expected,
        )
        .map(|accepted| accepted.message_id)
    }

    /// Accept one producer operation exactly once. The operation identity is
    /// stable across an in-process retry or a gateway crash/replay; its full
    /// immutable payload is compared before returning the original receipt.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_company_message_with_identity(
        &self,
        session_id: &str,
        operation_id: &str,
        handoff_id: Option<&str>,
        reply_to: Option<&str>,
        causation_id: Option<&str>,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        reply_expected: bool,
    ) -> Result<CompanyMessageAcceptance> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_company_message_identity("operation id", operation_id)?;
        if let Some(value) = reply_to {
            validate_company_message_identity("reply-to id", value)?;
        }
        if let Some(value) = causation_id {
            validate_company_message_identity("causation id", value)?;
        }
        let message_id = stable_company_message_id(session_id, operation_id);
        let handoff_id = handoff_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&message_id)
            .to_string();
        validate_company_message_identity("handoff id", &handoff_id)?;
        let reply_to = reply_to.map(str::to_string);
        let causation_id = causation_id.map(str::to_string);
        let input = NewCompanyEvent {
            run_id: session_id.to_string(),
            session_id: session_id.to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(from.to_string()),
            agent_instance_id: Some(from.to_string()),
            causation_id: causation_id.clone(),
            correlation_id: Some(message_id.clone()),
            idempotency_key: Some(format!("message-accepted:{message_id}")),
            event: CompanyEventKind::MessageAccepted {
                message_id: message_id.clone(),
                operation_id: operation_id.to_string(),
                handoff_id: handoff_id.clone(),
                reply_to: reply_to.clone(),
                causation_id: causation_id.clone(),
                from: from.to_string(),
                to: to.to_string(),
                subject: subject.to_string(),
                body: body.to_string(),
                message_kind: MessageKind::Conversation,
                reply_expected,
            },
        };
        validate_company_message_route(&input)?;

        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT message_id,session_id,handoff_id,reply_to,causation_id,from_agent,to_agent,
                        subject,body,message_kind,reply_expected,state
                   FROM company_messages
                  WHERE operation_id=?1",
                params![operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            existing_message_id,
            existing_session_id,
            existing_handoff_id,
            existing_reply_to,
            existing_causation_id,
            existing_from,
            existing_to,
            existing_subject,
            existing_body,
            existing_kind,
            existing_reply_expected,
            existing_state,
        )) = existing
        {
            anyhow::ensure!(
                existing_message_id == message_id
                    && existing_session_id == session_id
                    && existing_handoff_id == handoff_id
                    && existing_reply_to == reply_to
                    && existing_causation_id == causation_id
                    && existing_from == from
                    && existing_to == to
                    && existing_subject == subject
                    && existing_body == body
                    && existing_kind == "conversation"
                    && existing_reply_expected == reply_expected as i64,
                "company message operation `{operation_id}` was reused with a different immutable payload"
            );
            tx.commit()?;
            anyhow::ensure!(
                existing_state != "canceled",
                "company message operation `{operation_id}` was canceled and cannot be replayed"
            );
            anyhow::ensure!(
                matches!(existing_state.as_str(), "accepted" | "suspended" | "injected"),
                "company message operation `{operation_id}` has invalid delivery state `{existing_state}`"
            );
            return Ok(CompanyMessageAcceptance {
                message_id: existing_message_id,
                handoff_id: existing_handoff_id,
                state: existing_state,
                created: false,
                should_route: false,
            });
        }

        self.append_in_transaction(&tx, input)?;
        tx.commit()?;
        Ok(CompanyMessageAcceptance {
            message_id,
            handoff_id,
            state: "accepted".to_string(),
            created: true,
            should_route: true,
        })
    }

    pub fn inject_company_message(
        &self,
        session_id: &str,
        message_id: &str,
        to: &str,
    ) -> Result<()> {
        self.append(NewCompanyEvent {
            run_id: session_id.to_string(),
            session_id: session_id.to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(to.to_string()),
            agent_instance_id: Some(to.to_string()),
            causation_id: Some(message_id.to_string()),
            correlation_id: Some(message_id.to_string()),
            idempotency_key: Some(format!("message-injected:{message_id}")),
            event: CompanyEventKind::MessageInjected {
                message_id: message_id.to_string(),
            },
        })?;
        Ok(())
    }

    pub fn update_agent_profile(
        &self,
        actor_agent_id: &str,
        agent_id: &str,
        display_name: Option<String>,
        role_title: Option<String>,
        description: Option<String>,
        color: Option<String>,
        icon_seed: Option<String>,
    ) -> Result<super::company_directory::AgentRecord> {
        let snapshot = self.directory_snapshot()?;
        let mut profile = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?
            .profile
            .clone();
        anyhow::ensure!(
            profile.lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "restore `{agent_id}` before editing its profile"
        );
        if let Some(value) = display_name {
            profile.display_name = value;
        }
        if let Some(value) = role_title {
            profile.role_title = value;
        }
        if let Some(value) = description {
            profile.description = value;
        }
        if let Some(value) = color {
            profile.color = value;
        }
        if let Some(value) = icon_seed {
            profile.icon_seed = value;
        }
        self.apply_directory_change(
            actor_agent_id,
            format!("agent-profile:{agent_id}:{}", uuid::Uuid::new_v4()),
            super::company_directory::DirectoryChange::AgentUpserted { profile },
        )?;
        self.directory_snapshot()?
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("updated agent `{agent_id}` disappeared"))
    }

    /// Replace an agent's validated metadata object without disturbing its
    /// canonical thread, role identity, browser profile, or lifecycle.
    pub fn update_agent_metadata(
        &self,
        actor_agent_id: &str,
        agent_id: &str,
        metadata_json: String,
    ) -> Result<super::company_directory::AgentRecord> {
        let parsed: serde_json::Value =
            serde_json::from_str(&metadata_json).context("agent metadata must be valid JSON")?;
        anyhow::ensure!(parsed.is_object(), "agent metadata must be a JSON object");
        let snapshot = self.directory_snapshot()?;
        let mut profile = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?
            .profile
            .clone();
        anyhow::ensure!(
            profile.lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "restore `{agent_id}` before editing its profile"
        );
        profile.metadata_json = metadata_json;
        self.apply_directory_change(
            actor_agent_id,
            format!("agent-metadata:{agent_id}:{}", uuid::Uuid::new_v4()),
            super::company_directory::DirectoryChange::AgentUpserted { profile },
        )?;
        self.directory_snapshot()?
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("updated agent `{agent_id}` disappeared"))
    }

    pub fn update_group_profile(
        &self,
        actor_agent_id: &str,
        group_id: &str,
        name: Option<String>,
        description: Option<String>,
        color: Option<String>,
        icon_seed: Option<String>,
        metadata_json: Option<String>,
    ) -> Result<super::company_directory::GroupRecord> {
        let snapshot = self.directory_snapshot()?;
        let mut profile = snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("unknown group `{group_id}`"))?
            .profile
            .clone();
        anyhow::ensure!(
            profile.lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "restore `{group_id}` before editing its profile"
        );
        if let Some(value) = name {
            profile.name = value;
        }
        if let Some(value) = description {
            profile.description = value;
        }
        if let Some(value) = color {
            profile.color = value;
        }
        if let Some(value) = icon_seed {
            profile.icon_seed = value;
        }
        if let Some(value) = metadata_json {
            profile.metadata_json = value;
        }
        self.apply_directory_change(
            actor_agent_id,
            format!("group-profile:{group_id}:{}", uuid::Uuid::new_v4()),
            super::company_directory::DirectoryChange::GroupUpserted { profile },
        )?;
        self.directory_snapshot()?
            .groups
            .into_iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("updated group `{group_id}` disappeared"))
    }

    pub fn set_sidebar_item_pinned(
        &self,
        actor_agent_id: &str,
        item: &SidebarItemKey,
        pinned: bool,
    ) -> Result<()> {
        let snapshot = self.directory_snapshot()?;
        let change = match item {
            SidebarItemKey::Agent(agent_id) => {
                let mut profile = snapshot
                    .agents
                    .iter()
                    .find(|agent| &agent.profile.agent_id == agent_id)
                    .with_context(|| format!("unknown agent `{agent_id}`"))?
                    .profile
                    .clone();
                profile.pinned = pinned;
                super::company_directory::DirectoryChange::AgentUpserted { profile }
            }
            SidebarItemKey::Group(group_id) => {
                let mut profile = snapshot
                    .groups
                    .iter()
                    .find(|group| &group.profile.group_id == group_id)
                    .with_context(|| format!("unknown group `{group_id}`"))?
                    .profile
                    .clone();
                profile.pinned = pinned;
                super::company_directory::DirectoryChange::GroupUpserted { profile }
            }
        };
        self.apply_directory_change(
            actor_agent_id,
            format!("sidebar-pin:{item:?}:{pinned}:{}", uuid::Uuid::new_v4()),
            change,
        )?;
        Ok(())
    }

    /// Apply one mixed agent/group sidebar order atomically. The UI may pass
    /// any visible subset (for example only pinned rows); omitted items keep
    /// their previous order and no lifecycle state is changed.
    pub fn reorder_sidebar_items(
        &self,
        actor_agent_id: &str,
        ordered: Vec<SidebarItemKey>,
    ) -> Result<()> {
        anyhow::ensure!(ordered.len() <= 1_024, "sidebar order is too large");
        let snapshot = self.directory_snapshot()?;
        let mut seen = std::collections::HashSet::new();
        let mut inputs = Vec::with_capacity(ordered.len());
        for (sort_order, item) in ordered.into_iter().enumerate() {
            anyhow::ensure!(
                seen.insert(item.clone()),
                "duplicate sidebar item `{item:?}`"
            );
            let change = match &item {
                SidebarItemKey::Agent(agent_id) => {
                    let mut profile = snapshot
                        .agents
                        .iter()
                        .find(|agent| &agent.profile.agent_id == agent_id)
                        .with_context(|| format!("unknown agent `{agent_id}`"))?
                        .profile
                        .clone();
                    profile.sort_order = sort_order as i64;
                    super::company_directory::DirectoryChange::AgentUpserted { profile }
                }
                SidebarItemKey::Group(group_id) => {
                    let mut profile = snapshot
                        .groups
                        .iter()
                        .find(|group| &group.profile.group_id == group_id)
                        .with_context(|| format!("unknown group `{group_id}`"))?
                        .profile
                        .clone();
                    profile.sort_order = sort_order as i64;
                    super::company_directory::DirectoryChange::GroupUpserted { profile }
                }
            };
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some(actor_agent_id.to_string()),
                agent_instance_id: Some(actor_agent_id.to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!(
                    "sidebar-order:{sort_order}:{item:?}:{}",
                    uuid::Uuid::new_v4()
                )),
                event: CompanyEventKind::DirectoryChanged { change },
            });
        }
        self.append_many(inputs)?;
        Ok(())
    }

    /// Replace a group's manually ordered membership in one event-stream
    /// transaction. Individual canonical conversations remain untouched.
    pub fn set_group_members(
        &self,
        actor_agent_id: &str,
        group_id: &str,
        members: Vec<GroupMemberInput>,
    ) -> Result<()> {
        anyhow::ensure!(
            (MIN_GROUP_MEMBERS..=MAX_GROUP_MEMBERS).contains(&members.len()),
            "a group needs {MIN_GROUP_MEMBERS} to {MAX_GROUP_MEMBERS} active coworkers"
        );
        let snapshot = self.directory_snapshot()?;
        let group = snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("unknown group `{group_id}`"))?;
        anyhow::ensure!(
            group.profile.lifecycle == super::company_directory::LifecycleState::Active,
            "group `{group_id}` is not active"
        );
        let group_session_id = group
            .profile
            .canonical_session_id
            .clone()
            .unwrap_or_else(|| format!("group-{group_id}"));
        let history_message_count = {
            let mut sessions = crate::session::SessionStore::with_default_root()?;
            sessions.load_from_disk()?;
            sessions
                .get(&group_session_id)
                .map(|session| session.messages.len())
                .unwrap_or(0)
        };
        let mut seen = std::collections::HashSet::new();
        for member in &members {
            anyhow::ensure!(
                seen.insert(member.agent_id.as_str()),
                "duplicate group member `{}`",
                member.agent_id
            );
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == member.agent_id)
                .with_context(|| format!("unknown group member `{}`", member.agent_id))?;
            anyhow::ensure!(
                agent.profile.lifecycle == super::company_directory::LifecycleState::Active,
                "group member `{}` is not active",
                member.agent_id
            );
        }
        let mut inputs = Vec::new();
        for existing in snapshot
            .members
            .iter()
            .filter(|membership| membership.group_id == group_id)
        {
            if members
                .iter()
                .any(|member| member.agent_id == existing.agent_id)
            {
                continue;
            }
            inputs.push(directory_event(
                actor_agent_id,
                format!(
                    "group-member-remove:{group_id}:{}:{}",
                    existing.agent_id,
                    uuid::Uuid::new_v4()
                ),
                super::company_directory::DirectoryChange::GroupMemberSet {
                    group_id: group_id.to_string(),
                    agent_id: existing.agent_id.clone(),
                    member_role: existing.member_role.clone(),
                    history_access: existing.history_access,
                    history_start_message_index: existing.history_start_message_index,
                    sort_order: existing.sort_order,
                    present: false,
                },
            ));
        }
        for (sort_order, member) in members.into_iter().enumerate() {
            let existing = snapshot.members.iter().find(|existing| {
                existing.group_id == group_id && existing.agent_id == member.agent_id
            });
            let history_start_message_index = match member.history_access {
                super::company_directory::HistoryAccess::Full => 0,
                super::company_directory::HistoryAccess::FromJoin => existing
                    .filter(|existing| {
                        existing.history_access == super::company_directory::HistoryAccess::FromJoin
                    })
                    .map(|existing| existing.history_start_message_index)
                    .unwrap_or(history_message_count),
            };
            inputs.push(directory_event(
                actor_agent_id,
                format!(
                    "group-member-set:{group_id}:{}:{}",
                    member.agent_id,
                    uuid::Uuid::new_v4()
                ),
                super::company_directory::DirectoryChange::GroupMemberSet {
                    group_id: group_id.to_string(),
                    agent_id: member.agent_id,
                    member_role: member.member_role,
                    history_access: member.history_access,
                    history_start_message_index,
                    sort_order: sort_order as i64,
                    present: true,
                },
            ));
        }
        self.append_many(inputs)?;
        Ok(())
    }

    pub fn outside_call_granted(&self, group_id: &str, agent_id: &str) -> Result<bool> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        super::company_directory::outside_call_granted(&connection, group_id, agent_id)
    }

    pub fn set_agent_lifecycle(
        &self,
        actor_agent_id: &str,
        agent_id: &str,
        lifecycle: super::company_directory::LifecycleState,
    ) -> Result<CompanyEvent> {
        anyhow::ensure!(
            lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "use schedule_agent_deletion so the current name is confirmed"
        );
        let internal_role = self
            .directory_snapshot()?
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .map(|agent| agent.profile.internal_role)
            .with_context(|| format!("unknown agent `{agent_id}`"))?;
        let event = self.apply_directory_change(
            actor_agent_id,
            format!(
                "agent-lifecycle:{agent_id}:{lifecycle:?}:{}",
                uuid::Uuid::new_v4()
            ),
            super::company_directory::DirectoryChange::AgentLifecycleSet {
                agent_id: agent_id.to_string(),
                lifecycle,
                delete_after: None,
            },
        )?;
        if lifecycle != super::company_directory::LifecycleState::Active {
            super::postbox::pause_background_agent_everywhere(&internal_role);
        }
        Ok(event)
    }

    pub fn schedule_agent_deletion(
        &self,
        actor_agent_id: &str,
        agent_id: &str,
        confirmed_name: &str,
    ) -> Result<CompanyEvent> {
        let internal_role = self
            .directory_snapshot()?
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .map(|agent| agent.profile.internal_role)
            .with_context(|| format!("unknown agent `{agent_id}`"))?;
        let event = self.apply_directory_change(
            actor_agent_id,
            format!("agent-delete:{agent_id}:{}", uuid::Uuid::new_v4()),
            super::company_directory::DirectoryChange::AgentDeletionScheduled {
                agent_id: agent_id.to_string(),
                confirmed_name: confirmed_name.to_string(),
            },
        )?;
        super::postbox::pause_background_agent_everywhere(&internal_role);
        Ok(event)
    }

    pub fn set_group_lifecycle(
        &self,
        actor_agent_id: &str,
        group_id: &str,
        lifecycle: super::company_directory::LifecycleState,
    ) -> Result<CompanyEvent> {
        anyhow::ensure!(
            lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "use schedule_group_deletion so the current name is confirmed"
        );
        let canonical_session_id = self
            .directory_snapshot()?
            .groups
            .into_iter()
            .find(|group| group.profile.group_id == group_id)
            .and_then(|group| group.profile.canonical_session_id)
            .with_context(|| format!("group `{group_id}` has no canonical thread"))?;
        let event = self.apply_directory_change(
            actor_agent_id,
            format!(
                "group-lifecycle:{group_id}:{lifecycle:?}:{}",
                uuid::Uuid::new_v4()
            ),
            super::company_directory::DirectoryChange::GroupLifecycleSet {
                group_id: group_id.to_string(),
                lifecycle,
                delete_after: None,
            },
        )?;
        if lifecycle != super::company_directory::LifecycleState::Active {
            super::postbox::pause_background_session(&canonical_session_id);
        }
        Ok(event)
    }

    pub fn schedule_group_deletion(
        &self,
        actor_agent_id: &str,
        group_id: &str,
        confirmed_name: &str,
    ) -> Result<CompanyEvent> {
        let canonical_session_id = self
            .directory_snapshot()?
            .groups
            .into_iter()
            .find(|group| group.profile.group_id == group_id)
            .and_then(|group| group.profile.canonical_session_id)
            .with_context(|| format!("group `{group_id}` has no canonical thread"))?;
        let event = self.apply_directory_change(
            actor_agent_id,
            format!("group-delete:{group_id}:{}", uuid::Uuid::new_v4()),
            super::company_directory::DirectoryChange::GroupDeletionScheduled {
                group_id: group_id.to_string(),
                confirmed_name: confirmed_name.to_string(),
            },
        )?;
        super::postbox::pause_background_session(&canonical_session_id);
        Ok(event)
    }

    /// Permanently remove an expired directory item and its exact owner-private
    /// filesystem state. Company/group-shared knowledge, credentials, skills,
    /// and workflows are preserved; agent-private workflows are removed by the
    /// same audited directory event.
    pub fn purge_directory_items_due(&self) -> Result<Vec<(String, String)>> {
        let snapshot = self.directory_snapshot()?;
        let now = Utc::now();
        let mut due = Vec::new();
        for agent in snapshot.agents {
            if agent.profile.lifecycle == super::company_directory::LifecycleState::PendingDeletion
                && agent.delete_after.as_deref().is_some_and(|deadline| {
                    DateTime::parse_from_rfc3339(deadline)
                        .map(|deadline| deadline <= now)
                        .unwrap_or(false)
                })
            {
                due.push(DuePrivatePurge::Agent {
                    agent_id: agent.profile.agent_id,
                    internal_role: agent.profile.internal_role,
                    canonical_session_id: agent.profile.canonical_session_id,
                    browser_profile_id: agent.profile.browser_profile_id,
                });
            }
        }
        for group in snapshot.groups {
            if group.profile.lifecycle == super::company_directory::LifecycleState::PendingDeletion
                && group.delete_after.as_deref().is_some_and(|deadline| {
                    DateTime::parse_from_rfc3339(deadline)
                        .map(|deadline| deadline <= now)
                        .unwrap_or(false)
                })
            {
                due.push(DuePrivatePurge::Group {
                    group_id: group.profile.group_id,
                    canonical_session_id: group.profile.canonical_session_id,
                });
            }
        }
        let state_root = company_state_root(&self.path)?;
        let mut purged = Vec::with_capacity(due.len());
        for item in due {
            purge_private_state(&state_root, &item)?;
            let cookie_scope = match &item {
                DuePrivatePurge::Agent { agent_id, .. } => {
                    crate::security::vault::CredentialScope::agent(agent_id)
                }
                DuePrivatePurge::Group { group_id, .. } => {
                    crate::security::vault::CredentialScope::group(group_id)
                }
            };
            // Cookie grants hold no cookie values, but they are durable access
            // authority and must disappear with the private owner.
            crate::tools::browser_cookie_grants::remove_scope_at(&state_root, &cookie_scope)?;
            if let DuePrivatePurge::Agent { agent_id, .. } = &item {
                crate::onboarding::remove_agent_at(&state_root, agent_id)?;
            }
            // Secret-free account lifecycle records still reveal site and
            // ownership metadata, so private records follow the same 30-day
            // erasure boundary as their coworker/group.
            crate::tools::accounts::remove_scope_at(&state_root, &cookie_scope)?;
            // Credential envelopes expose only opaque ids and ownership scope,
            // allowing exact ciphertext removal even while the user's vault is
            // locked. Shared company credentials deliberately survive.
            crate::security::vault::Vault::at(&state_root).purge_scope(&cookie_scope)?;
            let (kind, id, change) = match item {
                DuePrivatePurge::Agent { agent_id, .. } => (
                    "agent",
                    agent_id.clone(),
                    super::company_directory::DirectoryChange::AgentPurged { agent_id },
                ),
                DuePrivatePurge::Group { group_id, .. } => (
                    "group",
                    group_id.clone(),
                    super::company_directory::DirectoryChange::GroupPurged { group_id },
                ),
            };
            self.apply_directory_change(
                "phoenix",
                format!("{kind}-purge:{id}:{}", uuid::Uuid::new_v4()),
                change,
            )?;
            purged.push((kind.to_string(), id));
        }
        Ok(purged)
    }

    /// Resolve and durably record one endless canonical thread for a group.
    /// Calling this repeatedly is safe and returns the original thread.
    pub fn ensure_group_canonical_session(&self, group_id: &str) -> Result<String> {
        let snapshot = self.directory_snapshot()?;
        let group = snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("unknown group `{group_id}`"))?;
        if let Some(session_id) = &group.profile.canonical_session_id {
            return Ok(session_id.clone());
        }
        let session_id = format!("group-{group_id}");
        crate::session::SessionStore::validate_session_id(&session_id)?;
        self.apply_directory_change(
            "phoenix",
            format!("group-canonical-session:{group_id}"),
            super::company_directory::DirectoryChange::ConversationSourceLinked {
                session_id: session_id.clone(),
                owner_kind: "group".to_string(),
                owner_id: group_id.to_string(),
                source_kind: "canonical".to_string(),
                canonical: true,
            },
        )?;
        Ok(session_id)
    }

    pub fn ensure_agent_canonical_session(&self, agent_id: &str) -> Result<String> {
        let snapshot = self.directory_snapshot()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?;
        if let Some(session_id) = &agent.profile.canonical_session_id {
            return Ok(session_id.clone());
        }
        let session_id = format!("agent-{agent_id}");
        crate::session::SessionStore::validate_session_id(&session_id)?;
        self.apply_directory_change(
            "phoenix",
            format!("agent-canonical-session:{agent_id}"),
            super::company_directory::DirectoryChange::ConversationSourceLinked {
                session_id: session_id.clone(),
                owner_kind: "agent".to_string(),
                owner_id: agent_id.to_string(),
                source_kind: "canonical".to_string(),
                canonical: true,
            },
        )?;
        Ok(session_id)
    }

    /// Start a coworker with a fresh canonical conversation while retaining
    /// the previous transcript as non-canonical history. Identity, memories,
    /// skills, workflows, credentials, browser profile, and relationships are
    /// deliberately untouched. Phoenix is protected because its continuous
    /// company context must never be reset through this convenience path.
    pub fn rotate_agent_canonical_session(
        &self,
        actor_agent_id: &str,
        agent_id: &str,
    ) -> Result<(Option<String>, String)> {
        anyhow::ensure!(
            agent_id != "phoenix",
            "Phoenix's canonical company context cannot be reset"
        );
        let snapshot = self.directory_snapshot()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?;
        anyhow::ensure!(
            agent.profile.lifecycle != super::company_directory::LifecycleState::PendingDeletion,
            "agent `{agent_id}` is pending deletion"
        );
        let previous = agent.profile.canonical_session_id.clone();
        let session_id = format!("agent-{agent_id}-{}", uuid::Uuid::new_v4().simple());
        crate::session::SessionStore::validate_session_id(&session_id)?;
        self.apply_directory_change(
            actor_agent_id,
            format!("agent-fresh-context:{agent_id}:{session_id}"),
            super::company_directory::DirectoryChange::ConversationSourceLinked {
                session_id: session_id.clone(),
                owner_kind: "agent".to_string(),
                owner_id: agent_id.to_string(),
                source_kind: "fresh_start".to_string(),
                canonical: true,
            },
        )?;
        Ok((previous, session_id))
    }

    /// Create a first-class group and its endless canonical thread in one
    /// transaction. Membership is manually ordered and never alters any
    /// member's individual thread or history.
    pub fn create_group(
        &self,
        actor_agent_id: &str,
        mut profile: super::company_directory::GroupProfile,
        members: Vec<String>,
    ) -> Result<super::company_directory::GroupRecord> {
        let snapshot = self.directory_snapshot()?;
        let fallback_name = generated_group_name(&snapshot, &members)?;
        if profile.name.trim().is_empty() {
            profile.name = fallback_name;
        } else {
            profile.name = profile.name.trim().to_string();
        }
        anyhow::ensure!(
            !snapshot
                .groups
                .iter()
                .any(|group| group.profile.group_id == profile.group_id),
            "group `{}` already exists",
            profile.group_id
        );
        let session_id = profile
            .canonical_session_id
            .clone()
            .unwrap_or_else(|| format!("group-{}", profile.group_id));
        crate::session::SessionStore::validate_session_id(&session_id)?;
        profile.canonical_session_id = None;
        let group_id = profile.group_id.clone();
        let mut inputs = vec![NewCompanyEvent {
            run_id: "company-directory".to_string(),
            session_id: "company-directory".to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(actor_agent_id.to_string()),
            agent_instance_id: Some(actor_agent_id.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(format!("group-create:{group_id}:{}", uuid::Uuid::new_v4())),
            event: CompanyEventKind::DirectoryChanged {
                change: super::company_directory::DirectoryChange::GroupUpserted { profile },
            },
        }];
        inputs.push(NewCompanyEvent {
            run_id: "company-directory".to_string(),
            session_id: "company-directory".to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(actor_agent_id.to_string()),
            agent_instance_id: Some(actor_agent_id.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(format!("group-thread:{group_id}:{session_id}")),
            event: CompanyEventKind::DirectoryChanged {
                change: super::company_directory::DirectoryChange::ConversationSourceLinked {
                    session_id,
                    owner_kind: "group".to_string(),
                    owner_id: group_id.clone(),
                    source_kind: "canonical".to_string(),
                    canonical: true,
                },
            },
        });
        for (sort_order, agent_id) in members.into_iter().enumerate() {
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some(actor_agent_id.to_string()),
                agent_instance_id: Some(actor_agent_id.to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!("group-member:{group_id}:{agent_id}")),
                event: CompanyEventKind::DirectoryChanged {
                    change: super::company_directory::DirectoryChange::GroupMemberSet {
                        group_id: group_id.clone(),
                        agent_id,
                        member_role: "member".to_string(),
                        history_access: super::company_directory::HistoryAccess::Full,
                        history_start_message_index: 0,
                        sort_order: sort_order as i64,
                        present: true,
                    },
                },
            });
        }
        self.append_many(inputs)?;
        self.directory_snapshot()?
            .groups
            .into_iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("created group `{group_id}` was not projected"))
    }

    /// Register a custom coworker while Phoenix is researching and refining
    /// its responsibility/prompt. Dormant provisioning records are visible to
    /// setup surfaces but cannot receive work or join active groups.
    pub fn register_provisioning_agent(
        &self,
        actor_agent_id: &str,
        mut profile: super::company_directory::AgentProfile,
        responsibility_title: String,
        responsibility_scope: String,
    ) -> Result<()> {
        let snapshot = self.directory_snapshot()?;
        anyhow::ensure!(
            !snapshot
                .agents
                .iter()
                .any(|agent| agent.profile.agent_id == profile.agent_id),
            "agent `{}` already exists",
            profile.agent_id
        );
        profile.lifecycle = super::company_directory::LifecycleState::Dormant;
        profile.canonical_session_id = None;
        let agent_id = profile.agent_id.clone();
        let session_id = format!("agent-{agent_id}");
        crate::session::SessionStore::validate_session_id(&session_id)?;
        let envelope = |key: String, change| NewCompanyEvent {
            run_id: "company-directory".to_string(),
            session_id: "company-directory".to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(actor_agent_id.to_string()),
            agent_instance_id: Some(actor_agent_id.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(key),
            event: CompanyEventKind::DirectoryChanged { change },
        };
        self.append_many(vec![
            envelope(
                format!("provisioning-agent:{agent_id}"),
                super::company_directory::DirectoryChange::AgentUpserted { profile },
            ),
            envelope(
                format!("provisioning-responsibility:{agent_id}"),
                super::company_directory::DirectoryChange::ResponsibilityUpserted {
                    responsibility_id: format!("responsibility_{agent_id}"),
                    agent_id: agent_id.clone(),
                    title: responsibility_title,
                    scope: responsibility_scope,
                    success_criteria: vec![
                        "Own and deliver the complete configured outcome".to_string(),
                        "Use teammates when their judgment materially improves the result"
                            .to_string(),
                        "Return verified evidence and preserve durable learnings".to_string(),
                    ],
                    approval_policy_json: r#"{"inherits_company_policy":true}"#.to_string(),
                    escalation_policy_json: r#"{"ambiguous_owner":"phoenix","blocked":"ask_user"}"#
                        .to_string(),
                    status: "provisioning".to_string(),
                },
            ),
            envelope(
                format!("provisioning-thread:{agent_id}"),
                super::company_directory::DirectoryChange::ConversationSourceLinked {
                    session_id,
                    owner_kind: "agent".to_string(),
                    owner_id: agent_id,
                    source_kind: "canonical".to_string(),
                    canonical: true,
                },
            ),
        ])?;
        Ok(())
    }

    pub fn set_provisioned_agent_ready(&self, agent_id: &str, ready: bool) -> Result<()> {
        let snapshot = self.directory_snapshot()?;
        let mut profile = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?
            .profile
            .clone();
        if matches!(
            profile.lifecycle,
            super::company_directory::LifecycleState::Archived
                | super::company_directory::LifecycleState::PendingDeletion
        ) {
            anyhow::ensure!(
                !ready,
                "agent `{agent_id}` is archived or pending deletion and cannot be activated by provisioning"
            );
            return Ok(());
        }
        let source = serde_json::from_str::<serde_json::Value>(&profile.metadata_json)
            .ok()
            .and_then(|metadata| {
                metadata
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        anyhow::ensure!(
            matches!(
                source.as_deref(),
                Some("custom_agent_pipeline" | "phoenix_agent_provisioning")
            ),
            "agent `{agent_id}` is not managed by Phoenix provisioning"
        );
        profile.lifecycle = if ready {
            super::company_directory::LifecycleState::Active
        } else {
            super::company_directory::LifecycleState::Dormant
        };
        let mut metadata: serde_json::Value =
            serde_json::from_str(&profile.metadata_json).unwrap_or_else(|_| serde_json::json!({}));
        if !metadata.is_object() {
            metadata = serde_json::json!({});
        }
        metadata["provisioning"] = serde_json::Value::Bool(!ready);
        metadata["runtime_ready"] = serde_json::Value::Bool(ready);
        profile.metadata_json = serde_json::to_string(&metadata)?;
        let responsibility = snapshot
            .responsibilities
            .iter()
            .find(|responsibility| responsibility.agent_id == agent_id)
            .cloned();
        let mut changes =
            vec![super::company_directory::DirectoryChange::AgentUpserted { profile }];
        if let Some(responsibility) = responsibility {
            changes.push(
                super::company_directory::DirectoryChange::ResponsibilityUpserted {
                    responsibility_id: responsibility.responsibility_id,
                    agent_id: responsibility.agent_id,
                    title: responsibility.title,
                    scope: responsibility.scope,
                    success_criteria: responsibility.success_criteria,
                    approval_policy_json: responsibility.approval_policy_json,
                    escalation_policy_json: responsibility.escalation_policy_json,
                    status: if ready { "active" } else { "provisioning" }.to_string(),
                },
            );
        }
        let inputs = changes
            .into_iter()
            .enumerate()
            .map(|(index, change)| NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!(
                    "provisioning-ready:{agent_id}:{ready}:{index}:{}",
                    uuid::Uuid::new_v4()
                )),
                event: CompanyEventKind::DirectoryChanged { change },
            })
            .collect();
        self.append_many(inputs)?;
        Ok(())
    }

    /// Repair the event-projected directory from the existing private custom
    /// agent registry. This is additive and lifecycle-safe: missing records
    /// are imported, Phoenix-managed dormant records follow receipt-proven
    /// readiness, while archived/deletion-requested records and human-edited
    /// names/colors/descriptions are never resurrected or overwritten.
    pub fn reconcile_custom_agent_registry(&self) -> Result<usize> {
        let inventory = crate::sub_agents::registry::custom_directory_inventory()?;
        let mut changed = 0usize;
        for (manifest, ready) in inventory {
            let provisioning_source =
                crate::sub_agents::registry::agent_dir_for_role(&manifest.role)
                    .ok()
                    .is_some_and(|dir| dir.join("provisioning.json").is_file())
                    .then_some("phoenix_agent_provisioning")
                    .unwrap_or("custom_agent_pipeline");
            let snapshot = self.directory_snapshot()?;
            let existing = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == manifest.role);
            if existing.is_none() {
                let sort_order = snapshot
                    .agents
                    .iter()
                    .map(|agent| agent.profile.sort_order)
                    .max()
                    .unwrap_or(-1)
                    .saturating_add(1);
                let display_name = manifest
                    .persona
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| manifest.role.clone());
                let description = manifest
                    .description
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "Custom responsibility owner".to_string());
                let title = manifest
                    .output_label
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| humanize_directory_role(&manifest.role));
                self.register_provisioning_agent(
                    "phoenix",
                    super::company_directory::AgentProfile {
                        agent_id: manifest.role.clone(),
                        internal_role: manifest.role.clone(),
                        display_name,
                        role_title: title.clone(),
                        description: description.clone(),
                        color: directory_agent_color(&manifest.role),
                        icon_seed: format!("phoenix-flame-{}", manifest.role),
                        kind: super::company_directory::AgentKind::ResponsibilityOwner,
                        lifecycle: super::company_directory::LifecycleState::Dormant,
                        pinned: false,
                        sort_order,
                        canonical_session_id: None,
                        browser_profile_id: format!("agent-{}", manifest.role),
                        metadata_json: serde_json::json!({
                            "provisioning": true,
                            "runtime_ready": false,
                            "source": provisioning_source
                        })
                        .to_string(),
                    },
                    title,
                    description,
                )?;
                self.set_provisioned_agent_ready(&manifest.role, ready)?;
                changed = changed.saturating_add(1);
                continue;
            }

            let existing = existing.expect("checked above");
            if matches!(
                existing.profile.lifecycle,
                super::company_directory::LifecycleState::Archived
                    | super::company_directory::LifecycleState::PendingDeletion
            ) {
                continue;
            }
            let metadata =
                serde_json::from_str::<serde_json::Value>(&existing.profile.metadata_json)
                    .unwrap_or_else(|_| serde_json::json!({}));
            let phoenix_managed = matches!(
                metadata.get("source").and_then(serde_json::Value::as_str),
                Some("custom_agent_pipeline" | "phoenix_agent_provisioning")
            );
            let should_be_active = ready;
            let is_active =
                existing.profile.lifecycle == super::company_directory::LifecycleState::Active;
            if phoenix_managed && should_be_active != is_active {
                self.set_provisioned_agent_ready(&manifest.role, should_be_active)?;
                changed = changed.saturating_add(1);
            }
        }
        Ok(changed)
    }

    /// Materialize either the complete founding team or Phoenix-only scratch
    /// mode. Missing founding coworkers and charters are added idempotently;
    /// existing people and user edits are never overwritten. This also lets a
    /// user begin with Phoenix and choose the complete company later in
    /// onboarding without creating a second Phoenix.
    pub fn ensure_founding_team(&self, full_team: bool) -> Result<usize> {
        self.ensure_team(full_team, false)
    }

    /// Tests that exercise roles outside the default team (planner, finance…)
    /// seed the whole built-in role catalog.
    #[cfg(test)]
    pub fn ensure_full_catalog_team(&self) -> Result<usize> {
        self.ensure_team(true, true)
    }

    fn ensure_team(&self, full_team: bool, catalog: bool) -> Result<usize> {
        use super::company_directory as directory;
        let profiles_for = |active| if catalog { directory::role_catalog_profiles(active) } else { directory::founding_team_profiles(active) };
        let mut snapshot = self.directory_snapshot()?;
        if full_team {
            self.upgrade_legacy_founding_profiles(&snapshot)?;
            snapshot = self.directory_snapshot()?;
        }
        let known_agents = snapshot
            .agents
            .iter()
            .map(|agent| agent.profile.agent_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let known_responsibilities = snapshot
            .responsibilities
            .iter()
            .map(|responsibility| responsibility.responsibility_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let known_relationships = snapshot
            .relationships
            .iter()
            .map(|relationship| {
                (
                    relationship.from_agent_id.as_str(),
                    relationship.to_agent_id.as_str(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let profiles = profiles_for(full_team);
        let mut inputs = profiles
            .into_iter()
            .filter(|profile| !known_agents.contains(profile.agent_id.as_str()))
            .map(|profile| NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!("founding-team-v1:{}", profile.agent_id)),
                event: CompanyEventKind::DirectoryChanged {
                    change: super::company_directory::DirectoryChange::AgentUpserted { profile },
                },
            })
            .collect::<Vec<_>>();
        let count = inputs.len();
        for change in if catalog { directory::catalog_responsibilities(full_team) } else { directory::founding_responsibilities(full_team) } {
            let super::company_directory::DirectoryChange::ResponsibilityUpserted {
                ref responsibility_id,
                ..
            } = change
            else {
                unreachable!("founding responsibility helper returned another change")
            };
            if known_responsibilities.contains(responsibility_id.as_str()) {
                continue;
            }
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!("founding-responsibility-v1:{responsibility_id}")),
                event: CompanyEventKind::DirectoryChanged { change },
            });
        }
        for change in if catalog { directory::catalog_relationships(full_team) } else { directory::founding_relationships(full_team) } {
            let super::company_directory::DirectoryChange::RelationshipUpserted {
                ref from_agent_id,
                ref to_agent_id,
                ..
            } = change
            else {
                unreachable!("founding relationship helper returned another change")
            };
            if known_relationships.contains(&(from_agent_id.as_str(), to_agent_id.as_str())) {
                continue;
            }
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!(
                    "founding-relationship-v1:{from_agent_id}:{to_agent_id}"
                )),
                event: CompanyEventKind::DirectoryChanged { change },
            });
        }
        for profile in profiles_for(full_team) {
            let already_has_canonical = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == profile.agent_id)
                .is_some_and(|agent| agent.profile.canonical_session_id.is_some());
            if already_has_canonical {
                continue;
            }
            let session_id = format!("agent-{}", profile.agent_id);
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!(
                    "founding-canonical-session-v1:{}",
                    profile.agent_id
                )),
                event: CompanyEventKind::DirectoryChanged {
                    change: super::company_directory::DirectoryChange::ConversationSourceLinked {
                        session_id,
                        owner_kind: "agent".to_string(),
                        owner_id: profile.agent_id,
                        source_kind: "canonical".to_string(),
                        canonical: true,
                    },
                },
            });
        }
        if inputs.is_empty() {
            return Ok(0);
        }
        self.append_many(inputs)?;
        Ok(count)
    }

    /// Move an untouched v1 technical roster onto the responsibility-based
    /// company model without rewriting user customizations or deleting any
    /// history. Renames apply only when both the compiled old name and title
    /// still match. Database/security/test records remain readable historical
    /// data, but they are retired and cannot receive new work.
    fn upgrade_legacy_founding_profiles(
        &self,
        snapshot: &super::company_directory::DirectorySnapshot,
    ) -> Result<usize> {
        use super::company_directory::{AgentKind, DirectoryChange};

        let targets = super::company_directory::role_catalog_profiles(true)
            .into_iter()
            .map(|profile| (profile.agent_id.clone(), profile))
            .collect::<std::collections::HashMap<_, _>>();
        let legacy_identity = [
            ("coder", "Nico", "Software & Automation"),
            ("researcher", "Theo", "Research & Intelligence"),
            ("finance", "Felix", "Money & Administration"),
            ("scribe", "Nora", "Inbox & Communications"),
            ("critic", "Vera", "Review & Decisions"),
        ];
        let hidden_expertise = [
            ("database", "Ada", "Data & Systems"),
            ("hacker", "Soren", "Security & Privacy"),
            ("tester", "Quinn", "Quality & Verification"),
        ];
        let mut changed = 0usize;

        for (agent_id, old_name, old_title) in legacy_identity {
            let Some(existing) = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == agent_id)
            else {
                continue;
            };
            if existing.profile.display_name != old_name || existing.profile.role_title != old_title
            {
                continue;
            }
            let Some(target) = targets.get(agent_id) else {
                continue;
            };
            let mut profile = existing.profile.clone();
            profile.display_name = target.display_name.clone();
            profile.role_title = target.role_title.clone();
            profile.description = target.description.clone();
            profile.icon_seed = target.icon_seed.clone();
            profile.kind = AgentKind::ResponsibilityOwner;
            self.apply_directory_change(
                "phoenix",
                format!("founding-company-v2:identity:{agent_id}"),
                DirectoryChange::AgentUpserted { profile },
            )?;
            changed = changed.saturating_add(1);
        }

        // The first responsibility-company preview still used mascot-style
        // names. Upgrade only those exact untouched identities; a user rename
        // is authoritative and must never be replaced by a product migration.
        let preview_identity = [
            ("finance", "Bart", "Money & Administration"),
            ("coder", "Spark", "Engineering"),
            ("researcher", "Scout", "Research & Intelligence"),
            ("sales", "Milo", "Sales & Relationships"),
            ("personal_logistics", "Atlas", "Personal Logistics"),
        ];
        for (agent_id, old_name, old_title) in preview_identity {
            let Some(existing) = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == agent_id)
            else {
                continue;
            };
            if existing.profile.display_name != old_name || existing.profile.role_title != old_title
            {
                continue;
            }
            let Some(target) = targets.get(agent_id) else {
                continue;
            };
            let mut profile = existing.profile.clone();
            profile.display_name = target.display_name.clone();
            profile.role_title = target.role_title.clone();
            profile.description = target.description.clone();
            profile.icon_seed = target.icon_seed.clone();
            profile.kind = AgentKind::ResponsibilityOwner;
            self.apply_directory_change(
                "phoenix",
                format!("founding-company-v3:human-identity:{agent_id}"),
                DirectoryChange::AgentUpserted { profile },
            )?;
            changed = changed.saturating_add(1);
        }

        // The responsibility-company redesign assigns stable human identities
        // to product outcomes. Keep the immutable role ids, canonical sessions,
        // browser profiles, memory, and metadata intact. Only exact untouched
        // v3 defaults move; any user-edited name or title remains authoritative.
        let responsibility_roster_v3 = [
            ("scribe", "Nico", "Communications"),
            ("planner", "Maya", "Projects & Operations"),
            ("finance", "Theo", "Money & Administration"),
            ("frontend", "Iris", "Product & Design"),
            ("researcher", "Elena", "Research & Intelligence"),
            ("presentation", "Cleo", "Documents & Presentations"),
            ("critic", "Vera", "Quality & Review"),
            ("sales", "Owen", "Sales & Relationships"),
            ("marketing", "June", "Marketing & Audience"),
            ("personal_logistics", "Remy", "Personal Logistics"),
        ];
        for (agent_id, old_name, old_title) in responsibility_roster_v3 {
            let Some(existing) = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == agent_id)
            else {
                continue;
            };
            if existing.profile.display_name != old_name || existing.profile.role_title != old_title
            {
                continue;
            }
            let Some(target) = targets.get(agent_id) else {
                continue;
            };
            let mut profile = existing.profile.clone();
            profile.display_name = target.display_name.clone();
            profile.role_title = target.role_title.clone();
            profile.description = target.description.clone();
            profile.icon_seed = target.icon_seed.clone();
            profile.kind = AgentKind::ResponsibilityOwner;
            self.apply_directory_change(
                "phoenix",
                format!("founding-company-v4:responsibility-roster:{agent_id}"),
                DirectoryChange::AgentUpserted { profile },
            )?;
            changed = changed.saturating_add(1);
        }

        // v6 briefly changed Cleo's default company responsibility to one
        // example domain. Roll back only that exact generated payload. Stable
        // identity, canonical history, browser state, metadata, and any user
        // edits remain untouched. The v7 keys make the correction monotonic
        // for installations that already recorded the v6 events.
        if let Some(existing) = snapshot.agents.iter().find(|agent| {
            agent.profile.agent_id == "personal_logistics"
                && agent.profile.internal_role == "personal_logistics"
                && agent.profile.display_name == "Cleo"
                && agent.profile.role_title == V6_PERSONAL_LOGISTICS_TITLE
                && agent.profile.description == V6_PERSONAL_LOGISTICS_DESCRIPTION
                && agent.profile.kind == AgentKind::ResponsibilityOwner
        }) {
            if let Some(target) = targets.get("personal_logistics") {
                let mut profile = existing.profile.clone();
                profile.role_title = target.role_title.clone();
                profile.description = target.description.clone();
                self.apply_directory_change(
                    "phoenix",
                    "founding-company-v7:restore-operations-owner",
                    DirectoryChange::AgentUpserted { profile },
                )?;
                changed = changed.saturating_add(1);
            }
        }

        let v6_criteria = V6_PERSONAL_LOGISTICS_CRITERIA
            .iter()
            .map(|criterion| (*criterion).to_string())
            .collect::<Vec<_>>();
        if snapshot.responsibilities.iter().any(|responsibility| {
            responsibility.responsibility_id == "responsibility_personal_logistics"
                && responsibility.agent_id == "personal_logistics"
                && responsibility.title == V6_PERSONAL_LOGISTICS_TITLE
                && responsibility.scope == V6_PERSONAL_LOGISTICS_SCOPE
                && responsibility.success_criteria == v6_criteria
                && responsibility.approval_policy_json == FOUNDING_APPROVAL_POLICY
                && responsibility.escalation_policy_json == FOUNDING_ESCALATION_POLICY
                && responsibility.status == "active"
        }) {
            let operations_charter = super::company_directory::catalog_responsibilities(true)
                .into_iter()
                .find(|change| {
                    matches!(
                        change,
                        DirectoryChange::ResponsibilityUpserted {
                            responsibility_id,
                            ..
                        } if responsibility_id == "responsibility_personal_logistics"
                    )
                });
            // Operations is no longer a founding role; older companies that
            // still have it keep their charter as it is.
            if let Some(operations_charter) = operations_charter {
                self.apply_directory_change(
                    "phoenix",
                    "founding-company-v7:restore-operations-charter",
                    operations_charter,
                )?;
                changed = changed.saturating_add(1);
            }
        }

        for (agent_id, old_name, old_title) in hidden_expertise {
            let Some(existing) = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == agent_id)
            else {
                continue;
            };
            if existing.profile.display_name != old_name
                || existing.profile.role_title != old_title
                || existing.profile.kind != AgentKind::ResponsibilityOwner
            {
                continue;
            }
            let mut profile = existing.profile.clone();
            profile.kind = AgentKind::CraftSpecialist;
            profile.lifecycle = super::company_directory::LifecycleState::Archived;
            let mut metadata = serde_json::from_str::<serde_json::Value>(&profile.metadata_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = metadata.as_object_mut() {
                object.insert("hidden_expertise".to_string(), serde_json::json!(true));
                object.insert(
                    "migration".to_string(),
                    serde_json::json!("responsibility_company_v2"),
                );
            }
            profile.metadata_json = serde_json::to_string(&metadata)?;
            self.apply_directory_change(
                "phoenix",
                format!("founding-company-v2:expertise:{agent_id}"),
                DirectoryChange::AgentUpserted { profile },
            )?;
            changed = changed.saturating_add(1);
        }

        // Installations that already ran the earlier "hidden expertise"
        // migration need one more monotonic event to retire those records.
        for agent_id in ["database", "hacker", "tester"] {
            let Some(existing) = snapshot.agents.iter().find(|agent| {
                agent.profile.agent_id == agent_id
                    && agent.profile.kind == AgentKind::CraftSpecialist
                    && agent.profile.lifecycle != super::company_directory::LifecycleState::Archived
            }) else {
                continue;
            };
            let mut profile = existing.profile.clone();
            profile.lifecycle = super::company_directory::LifecycleState::Archived;
            self.apply_directory_change(
                "phoenix",
                format!("founding-company-v5:retire-legacy-role:{agent_id}"),
                DirectoryChange::AgentUpserted { profile },
            )?;
            changed = changed.saturating_add(1);
        }
        Ok(changed)
    }

    /// Link the existing session store into the durable coworker directory.
    /// Transcripts are never concatenated or rewritten: one newest suitable
    /// source is marked canonical, while every older thread remains an indexed
    /// context source for librarian recall and advanced history search.
    pub fn reconcile_session_history(&self, sessions_root: &Path) -> Result<usize> {
        let directory = self.directory_snapshot()?;
        let known_agents: std::collections::HashSet<String> = directory
            .agents
            .iter()
            .map(|agent| agent.profile.agent_id.clone())
            .collect();
        // Reconciliation runs at every gateway startup. It may choose a legacy
        // transcript only while an owner still has the untouched, file-less
        // founding placeholder. Once a real canonical conversation (including
        // a deliberately fresh one) exists, filesystem mtimes must never
        // silently replace that user-visible thread.
        let existing_canonical = directory
            .conversation_sources
            .iter()
            .filter(|source| source.owner_kind == "agent" && source.canonical)
            .map(|source| {
                (
                    source.owner_id.clone(),
                    (source.session_id.clone(), source.source_kind.clone()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        // Room transcripts and their private working branches are not legacy
        // personal chats. Exclude them BEFORE choosing each agent's newest
        // candidate; otherwise startup can steal room ownership or promote a
        // room-only checkpoint to a personal canonical conversation.
        let room_ids: std::collections::HashSet<_> = directory.groups.iter()
            .filter_map(|group| group.profile.canonical_session_id.clone())
            .chain(directory.conversation_sources.iter().filter(|source| source.owner_kind == "group").map(|source| source.session_id.clone()))
            .collect();
        let room_actor_ids: Vec<_> = room_ids.iter().flat_map(|room| directory.agents.iter().map(move |agent| {
            let role = if agent.profile.internal_role == "orchestrator" { "phoenix" } else { &agent.profile.internal_role };
            crate::session::actor_session_id_scoped(room, role, None)
        })).collect();
        let discovered = super::company_directory::discover_session_history_excluding(sessions_root, |id| {
            room_ids.contains(id) || room_ids.iter().any(|room| id.starts_with(&format!("{room}__")))
                || room_actor_ids.iter().any(|actor| id == actor || id.starts_with(&format!("{actor}--")))
        })?;
        let mut inputs = Vec::new();
        for source in discovered {
            if !known_agents.contains(&source.owner_agent_id) {
                continue;
            }
            let canonical = match existing_canonical.get(&source.owner_agent_id) {
                Some((session_id, source_kind))
                    if source_kind != "canonical"
                        || sessions_root.join(format!("{session_id}.json")).is_file() =>
                {
                    source.session_id == *session_id
                }
                _ => source.canonical,
            };
            let session_id = source.session_id;
            let owner_id = source.owner_agent_id;
            let source_kind = source.source_kind;
            inputs.push(NewCompanyEvent {
                run_id: "company-directory".to_string(),
                session_id: "company-directory".to_string(),
                pod_id: None,
                work_node_id: None,
                attempt_id: None,
                agent_identity_id: Some("phoenix".to_string()),
                agent_instance_id: Some("phoenix".to_string()),
                causation_id: None,
                correlation_id: None,
                idempotency_key: Some(format!(
                    "session-history-v1:{session_id}:{owner_id}:{source_kind}:{canonical}:{}",
                    source.modified_unix_millis
                )),
                event: CompanyEventKind::DirectoryChanged {
                    change: super::company_directory::DirectoryChange::ConversationSourceLinked {
                        session_id,
                        owner_kind: "agent".to_string(),
                        owner_id,
                        source_kind,
                        canonical,
                    },
                },
            });
        }
        let count = inputs.len();
        for chunk in inputs.chunks(64) {
            self.append_many(chunk.to_vec())?;
        }
        Ok(count)
    }

    pub fn append(&self, input: NewCompanyEvent) -> Result<CompanyEvent> {
        validate_company_message_route(&input)?;
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction()?;
        let event = self.append_in_transaction(&tx, input)?;
        tx.commit()?;
        Ok(event)
    }

    /// Append a small related event group under one SQLite transaction. Work
    /// claims and state+focus changes must never leave a convincing half-event
    /// behind when projection validation rejects the second event.
    pub fn append_many(&self, inputs: Vec<NewCompanyEvent>) -> Result<Vec<CompanyEvent>> {
        self.append_many_checked(inputs, false)
    }

    pub(crate) fn append_many_exact(&self, inputs: Vec<NewCompanyEvent>) -> Result<Vec<CompanyEvent>> {
        self.append_many_checked(inputs, true)
    }

    fn append_many_checked(&self, inputs: Vec<NewCompanyEvent>, exact: bool) -> Result<Vec<CompanyEvent>> {
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction()?;
        let events = self.append_many_in_transaction(&tx, inputs, exact)?;
        tx.commit()?;
        Ok(events)
    }

    /// Compose workflow installation with task reservation under the caller's
    /// transaction. Projection checks and exact retry fencing remain shared
    /// with the standalone append path; this helper never commits on its own.
    fn append_many_in_transaction(&self, tx: &Transaction<'_>, inputs: Vec<NewCompanyEvent>, exact: bool) -> Result<Vec<CompanyEvent>> {
        const MAX_ATOMIC_EVENTS: usize = 64;
        if inputs.is_empty() || inputs.len() > MAX_ATOMIC_EVENTS {
            anyhow::bail!("atomic company append requires 1..={MAX_ATOMIC_EVENTS} events");
        }
        for input in &inputs {
            validate_company_message_route(input)?;
        }
        // The caller may handle this error and commit unrelated work. Fence
        // our own prefix instead of relying on the outer transaction's Drop.
        // The identifier is generated here, never interpolated from input.
        let savepoint = format!("phoenix_append_{}", uuid::Uuid::new_v4().simple());
        tx.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
        let result = (|| -> Result<Vec<CompanyEvent>> {
        let mut events = Vec::with_capacity(inputs.len());
        for input in inputs {
            if exact {
                if let Some(key) = input.idempotency_key.as_deref() {
                    if let Some(existing) = event_by_key(tx, key)? {
                        anyhow::ensure!(serde_json::to_value(&existing.envelope)? == serde_json::to_value(&input)?,
                            "atomic plan retry changed a saved request");
                    }
                }
            }
            events.push(self.append_in_transaction(tx, input)?);
        }
        Ok(events)
        })();
        match result {
            Ok(events) => {
                tx.execute_batch(&format!("RELEASE SAVEPOINT {savepoint}"))?;
                Ok(events)
            }
            Err(error) => {
                tx.execute_batch(&format!("ROLLBACK TO SAVEPOINT {savepoint}; RELEASE SAVEPOINT {savepoint}"))
                    .with_context(|| format!("failed to roll back rejected event batch: {error:#}"))?;
                Err(error)
            }
        }
    }

    fn append_in_transaction(
        &self,
        tx: &Transaction<'_>,
        input: NewCompanyEvent,
    ) -> Result<CompanyEvent> {
        let envelope = serde_json::to_vec(&input)?;
        if envelope.len() > MAX_EVENT_BYTES {
            anyhow::bail!(
                "company event is too large ({} bytes; max {MAX_EVENT_BYTES})",
                envelope.len()
            );
        }
        if let Some(key) = input.idempotency_key.as_deref() {
            if let Some(existing) = event_by_key(tx, key)? {
                return Ok(existing);
            }
        }
        let event_id = format!("evt_{}", uuid::Uuid::new_v4().simple());
        let recorded_at = Utc::now();
        let event_type = event_name(&input.event);
        let payload = serde_json::to_string(&input.event)?;
        tx.execute(
            "INSERT INTO company_events
             (event_id,runtime_epoch,recorded_at,run_id,session_id,pod_id,work_node_id,attempt_id,
              agent_identity_id,agent_instance_id,causation_id,correlation_id,idempotency_key,event_type,payload)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![event_id, self.runtime_epoch, recorded_at.to_rfc3339(), input.run_id,
                input.session_id, input.pod_id, input.work_node_id, input.attempt_id,
                input.agent_identity_id, input.agent_instance_id, input.causation_id,
                input.correlation_id, input.idempotency_key, event_type, payload],
        )?;
        let seq = tx.last_insert_rowid();
        project(tx, seq, &recorded_at, &input)?;
        Ok(CompanyEvent {
            schema_version: COMPANY_SCHEMA_VERSION,
            event_id,
            runtime_epoch: self.runtime_epoch.clone(),
            company_seq: seq,
            recorded_at,
            envelope: input,
        })
    }

    pub fn events_since(&self, after_seq: i64, limit: usize) -> Result<Vec<CompanyEvent>> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = connection.prepare(
            "SELECT company_seq,event_id,runtime_epoch,recorded_at,run_id,session_id,pod_id,
                    work_node_id,attempt_id,agent_identity_id,agent_instance_id,causation_id,
                    correlation_id,idempotency_key,payload
             FROM company_events WHERE company_seq>?1 ORDER BY company_seq LIMIT ?2",
        )?;
        let limit = limit.clamp(1, MAX_EVENT_PAGE);
        let rows = stmt.query_map(params![after_seq, limit as i64], row_to_event)?;
        let mut events = Vec::new();
        let mut total_bytes = 0usize;
        for row in rows {
            let event = row?;
            total_bytes = total_bytes
                .checked_add(serde_json::to_vec(&event)?.len())
                .context("company event page size overflow")?;
            if total_bytes > MAX_EVENT_PAGE_BYTES {
                anyhow::bail!("company event page exceeds {MAX_EVENT_PAGE_BYTES} bytes");
            }
            events.push(event);
        }
        Ok(events)
    }

    /// Return a run-scoped event page with both row and serialized-byte
    /// budgets. This is intended for tool/UI surfaces, which must not turn a
    /// bounded tool result into an unbounded database read first.
    pub fn events_since_bounded_for_run(
        &self,
        run_id: &str,
        after_seq: i64,
        limit: usize,
        max_bytes: usize,
    ) -> Result<(Vec<CompanyEvent>, bool)> {
        let limit = limit.clamp(1, MAX_EVENT_PAGE);
        let max_bytes = max_bytes.clamp(1, MAX_EVENT_PAGE_BYTES);
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = connection.prepare(
            "SELECT company_seq,event_id,runtime_epoch,recorded_at,run_id,session_id,pod_id,
                    work_node_id,attempt_id,agent_identity_id,agent_instance_id,causation_id,
                    correlation_id,idempotency_key,payload
             FROM company_events
             WHERE run_id=?1 AND company_seq>?2
             ORDER BY company_seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![run_id, after_seq, limit.saturating_add(1) as i64],
            row_to_event,
        )?;
        let mut events = Vec::new();
        let mut total_bytes = 0usize;
        let mut partial = false;
        for row in rows {
            if events.len() == limit {
                partial = true;
                break;
            }
            let event = row?;
            let event_bytes = serde_json::to_vec(&event)?.len();
            let next_bytes = total_bytes
                .checked_add(event_bytes)
                .context("bounded company event page size overflow")?;
            if next_bytes > max_bytes {
                partial = true;
                break;
            }
            total_bytes = next_bytes;
            events.push(event);
        }
        Ok((events, partial))
    }

    /// Return the newest projections for one run within caller-provided row
    /// and byte budgets. The boolean reports whether older or oversized rows
    /// were omitted.
    pub fn bounded_snapshot_for_run(
        &self,
        run_id: &str,
        max_rows_per_kind: usize,
        max_bytes: usize,
    ) -> Result<(CompanySnapshot, bool)> {
        let max_rows = max_rows_per_kind.clamp(1, MAX_SNAPSHOT_ROWS);
        let max_bytes = max_bytes.clamp(1, MAX_SNAPSHOT_BYTES);
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let as_of_seq = connection.query_row(
            "SELECT COALESCE(MAX(company_seq),0) FROM company_events WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )?;
        let mut total_bytes = 0usize;
        let mut partial = false;

        let mut jobs = Vec::new();
        let mut jobs_stmt = connection.prepare(
            "SELECT job_id,run_id,session_id,role,subject,state,ok,verified,summary,started_at,settled_at,as_of_seq
             FROM company_jobs WHERE session_id=?1 ORDER BY as_of_seq DESC LIMIT ?2",
        )?;
        let job_rows = jobs_stmt.query_map(
            params![run_id, max_rows.saturating_add(1) as i64],
            row_to_job,
        )?;
        for row in job_rows {
            if jobs.len() == max_rows {
                partial = true;
                break;
            }
            let job = row?;
            let next_bytes = total_bytes
                .checked_add(serde_json::to_vec(&job)?.len())
                .context("bounded company snapshot size overflow")?;
            if next_bytes > max_bytes {
                partial = true;
                break;
            }
            total_bytes = next_bytes;
            jobs.push(job);
        }

        let mut work = Vec::new();
        let mut work_stmt = connection.prepare(
            "SELECT node_id,run_id,title,outcome,acceptance_json,dependencies_json,pattern,state,reason,as_of_seq
             FROM company_work WHERE run_id=?1 ORDER BY as_of_seq DESC LIMIT ?2",
        )?;
        let work_rows = work_stmt.query_map(
            params![run_id, max_rows.saturating_add(1) as i64],
            row_to_work,
        )?;
        for row in work_rows {
            if work.len() == max_rows {
                partial = true;
                break;
            }
            let projection = row?;
            let next_bytes = total_bytes
                .checked_add(serde_json::to_vec(&projection)?.len())
                .context("bounded company snapshot size overflow")?;
            if next_bytes > max_bytes {
                partial = true;
                break;
            }
            total_bytes = next_bytes;
            work.push(projection);
        }

        let active_jobs = jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    AgentState::Queued
                        | AgentState::Starting
                        | AgentState::Reasoning
                        | AgentState::UsingTool
                        | AgentState::WaitingPeer
                        | AgentState::WaitingUser
                        | AgentState::Reviewing
                        | AgentState::Integrating
                )
            })
            .count();
        let stale_jobs = jobs
            .iter()
            .filter(|job| job.state == AgentState::Stale)
            .count();
        Ok((
            CompanySnapshot {
                runtime_epoch: self.runtime_epoch.clone(),
                as_of_seq,
                jobs,
                work,
                workflow: WorkflowSnapshot::default(),
                active_jobs,
                stale_jobs,
            },
            partial,
        ))
    }

    pub fn snapshot(&self, session_id: Option<&str>) -> Result<CompanySnapshot> {
        let workflow = self.workflow_snapshot(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let as_of_seq = connection.query_row(
            "SELECT COALESCE(MAX(company_seq),0) FROM company_events",
            [],
            |r| r.get(0),
        )?;
        let mut jobs = Vec::new();
        let sql = if session_id.is_some() {
            "SELECT job_id,run_id,session_id,role,subject,state,ok,verified,summary,started_at,settled_at,as_of_seq
             FROM company_jobs WHERE session_id=?1 ORDER BY started_at LIMIT ?2"
        } else {
            "SELECT job_id,run_id,session_id,role,subject,state,ok,verified,summary,started_at,settled_at,as_of_seq
             FROM company_jobs ORDER BY as_of_seq DESC LIMIT ?1"
        };
        let mut stmt = connection.prepare(sql)?;
        let mut snapshot_bytes = 0usize;
        if let Some(session_id) = session_id {
            let rows = stmt.query_map(
                params![session_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_job,
            )?;
            for row in rows {
                let job = row?;
                snapshot_bytes = snapshot_bytes
                    .checked_add(serde_json::to_vec(&job)?.len())
                    .context("company snapshot size overflow")?;
                if snapshot_bytes > MAX_SNAPSHOT_BYTES {
                    anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
                }
                jobs.push(job);
            }
        } else {
            let rows = stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_job)?;
            for row in rows {
                let job = row?;
                if legacy_synthetic_session(&job.session_id) {
                    continue;
                }
                snapshot_bytes = snapshot_bytes
                    .checked_add(serde_json::to_vec(&job)?.len())
                    .context("company snapshot size overflow")?;
                if snapshot_bytes > MAX_SNAPSHOT_BYTES {
                    anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
                }
                jobs.push(job);
                if jobs.len() == MAX_GLOBAL_STATUS_JOBS {
                    break;
                }
            }
            // Consumers historically received chronological projections. The
            // query is newest-first only so the bounded view keeps recent work.
            jobs.reverse();
        }
        if snapshot_bytes > MAX_SNAPSHOT_BYTES {
            anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
        }
        if jobs.len() > MAX_SNAPSHOT_ROWS {
            anyhow::bail!("company snapshot has more than {MAX_SNAPSHOT_ROWS} jobs");
        }
        let active_jobs = jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    AgentState::Queued
                        | AgentState::Starting
                        | AgentState::Reasoning
                        | AgentState::UsingTool
                        | AgentState::WaitingPeer
                        | AgentState::WaitingUser
                        | AgentState::Reviewing
                        | AgentState::Integrating
                )
            })
            .count();
        let stale_jobs = jobs.iter().filter(|j| j.state == AgentState::Stale).count();
        let mut work = Vec::new();
        if let Some(session_id) = session_id {
            let mut stmt = connection.prepare(
                "SELECT node_id,run_id,title,outcome,acceptance_json,dependencies_json,pattern,state,reason,as_of_seq
                 FROM company_work WHERE run_id=?1 ORDER BY as_of_seq LIMIT ?2",
            )?;
            for row in stmt.query_map(
                params![session_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_work,
            )? {
                let projection = row?;
                snapshot_bytes = snapshot_bytes
                    .checked_add(serde_json::to_vec(&projection)?.len())
                    .context("company snapshot size overflow")?;
                if snapshot_bytes > MAX_SNAPSHOT_BYTES {
                    anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
                }
                work.push(projection);
            }
        } else {
            let mut stmt = connection.prepare(
                "SELECT node_id,run_id,title,outcome,acceptance_json,dependencies_json,pattern,state,reason,as_of_seq
                 FROM company_work ORDER BY as_of_seq LIMIT ?1",
            )?;
            for row in stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_work)? {
                let projection = row?;
                snapshot_bytes = snapshot_bytes
                    .checked_add(serde_json::to_vec(&projection)?.len())
                    .context("company snapshot size overflow")?;
                if snapshot_bytes > MAX_SNAPSHOT_BYTES {
                    anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
                }
                work.push(projection);
            }
        }
        if snapshot_bytes > MAX_SNAPSHOT_BYTES {
            anyhow::bail!("company snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
        }
        if work.len() > MAX_SNAPSHOT_ROWS {
            anyhow::bail!("company snapshot has more than {MAX_SNAPSHOT_ROWS} work rows");
        }
        Ok(CompanySnapshot {
            runtime_epoch: self.runtime_epoch.clone(),
            as_of_seq,
            jobs,
            work,
            workflow,
            active_jobs,
            stale_jobs,
        })
    }

    pub fn start_job(
        &self,
        run_id: &str,
        session_id: &str,
        role: &str,
        subject: &str,
    ) -> Result<String> {
        let job_id = format!("job_{}", uuid::Uuid::new_v4().simple());
        self.append(NewCompanyEvent {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(role.to_string()),
            agent_instance_id: Some(role.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(format!("job-start:{job_id}")),
            event: CompanyEventKind::JobStarted {
                job_id: job_id.clone(),
                role: role.to_string(),
                subject: subject.to_string(),
            },
        })?;
        Ok(job_id)
    }

    pub fn settle_matching_job(
        &self,
        session_id: &str,
        role: &str,
        subject: &str,
        ok: bool,
        summary: &str,
    ) -> Result<Option<String>> {
        let job_id = {
            let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
            connection.query_row(
                "SELECT job_id FROM company_jobs WHERE session_id=?1 AND role=?2 AND subject=?3
                 AND state NOT IN ('completed_verified','completed_unverified','failed','superseded')
                 ORDER BY started_at DESC LIMIT 1",
                params![session_id, role, subject], |r| r.get::<_, String>(0),
            ).optional()?
        };
        let Some(job_id) = job_id else {
            return Ok(None);
        };
        self.append(NewCompanyEvent {
            run_id: session_id.to_string(),
            session_id: session_id.to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(role.to_string()),
            agent_instance_id: Some(role.to_string()),
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(format!("job-settle:{job_id}")),
            event: CompanyEventKind::JobSettled {
                job_id: job_id.clone(),
                ok,
                verified: false,
                summary: summary.to_string(),
            },
        })?;
        Ok(Some(job_id))
    }

    /// Persist a detached coworker's complete return before the live postbox
    /// announces it. The company event ledger records lifecycle/summary; this
    /// delivery table preserves the full body until the canonical transcript
    /// has durably absorbed it.
    pub fn persist_job_return(
        &self,
        session_id: &str,
        job: &crate::runtime::postbox::CompletedJob,
    ) -> Result<crate::runtime::postbox::CompletedJob> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_job_return(job)?;
        let mut job = job.clone();
        if job.delivery_id.is_empty() {
            job.delivery_id = format!("return_{}", uuid::Uuid::new_v4().simple());
        }
        validate_delivery_id(&job.delivery_id)?;
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT session_id,agent,subject,ok,summary,body,finished_at,causation_id,return_kind
                 FROM company_job_returns WHERE delivery_id=?1",
                [&job.delivery_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            existing_session,
            existing_agent,
            existing_subject,
            existing_ok,
            existing_summary,
            existing_body,
            existing_finished,
            existing_causation,
            existing_kind,
        )) = existing
        {
            anyhow::ensure!(
                existing_session == session_id
                    && existing_agent == job.agent
                    && existing_subject == job.subject
                    && existing_ok == job.ok as i64
                    && existing_summary == job.summary
                    && existing_body == job.body
                    && existing_finished == job.finished.to_rfc3339()
                    && existing_causation == job.causation_id
                    && existing_kind == job.kind.as_str(),
                "coworker return delivery id `{}` was reused with a different payload",
                job.delivery_id
            );
            tx.commit()?;
            return Ok(job);
        }
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM company_job_returns
             WHERE session_id=?1 AND delivery_state IN ('staged','ready','claimed')",
            [session_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            pending < MAX_JOB_RETURNS_PER_SESSION,
            "conversation already has {MAX_JOB_RETURNS_PER_SESSION} undelivered coworker returns"
        );
        tx.execute(
            "INSERT INTO company_job_returns(
                delivery_id,session_id,agent,subject,ok,summary,body,finished_at,causation_id,
                delivery_state,created_at,owner_runtime_epoch,return_kind)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'staged',?10,?11,?12)",
            params![
                job.delivery_id,
                session_id,
                job.agent,
                job.subject,
                job.ok as i64,
                job.summary,
                job.body,
                job.finished.to_rfc3339(),
                job.causation_id,
                Utc::now().to_rfc3339(),
                self.runtime_epoch(),
                job.kind.as_str(),
            ],
        )?;
        tx.commit()?;
        Ok(job)
    }

    /// Publish a durably staged return to the delivery queue. The postbox
    /// performs this transition while holding its per-session lifecycle lock,
    /// so a concurrent claimant cannot emit `Absorbed` before the matching
    /// live `Returned` receipt has been queued.
    pub fn promote_job_return(&self, session_id: &str, delivery_id: &str) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        validate_delivery_id(delivery_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let changed = connection.execute(
            "UPDATE company_job_returns SET delivery_state='ready'
             WHERE delivery_id=?1 AND session_id=?2 AND delivery_state='staged'
               AND (owner_runtime_epoch=?3 OR owner_runtime_epoch IS NULL)",
            params![delivery_id, session_id, self.runtime_epoch()],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let existing = connection
            .query_row(
                "SELECT session_id,delivery_state FROM company_job_returns WHERE delivery_id=?1",
                [delivery_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((existing_session, state)) = existing else {
            anyhow::bail!("coworker return `{delivery_id}` was not durably staged");
        };
        anyhow::ensure!(
            existing_session == session_id,
            "coworker return `{delivery_id}` belongs to another conversation"
        );
        anyhow::ensure!(
            matches!(state.as_str(), "ready" | "claimed"),
            "coworker return `{delivery_id}` is in invalid delivery state `{state}`"
        );
        Ok(())
    }

    /// Atomically claim every ready return for one canonical conversation.
    /// Dead-owner claims become ready at recovery, so a crash before the
    /// transcript save cannot consume the coworker's work. Live owners retain
    /// their claims even if another gateway runs recovery concurrently.
    pub fn claim_job_returns(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::runtime::postbox::CompletedJob>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let jobs = select_job_returns(&tx, session_id, "ready")?;
        for job in &jobs {
            tx.execute(
                "UPDATE company_job_returns SET delivery_state='claimed',owner_runtime_epoch=?3
                 WHERE delivery_id=?1 AND session_id=?2 AND delivery_state='ready'",
                params![job.delivery_id, session_id, self.runtime_epoch()],
            )?;
        }
        tx.commit()?;
        Ok(jobs)
    }

    pub fn peek_job_returns(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::runtime::postbox::CompletedJob>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        select_job_returns(&connection, session_id, "ready")
    }

    pub fn acknowledge_job_returns(&self, session_id: &str, delivery_ids: &[String]) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for delivery_id in delivery_ids {
            validate_delivery_id(delivery_id)?;
            tx.execute(
                "DELETE FROM company_job_returns
                 WHERE delivery_id=?1 AND session_id=?2 AND delivery_state='claimed'
                   AND owner_runtime_epoch=?3",
                params![delivery_id, session_id, self.runtime_epoch()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stop suppresses selected managed terminal receipts in every delivery
    /// phase, without discarding unrelated coworker returns.
    pub fn discard_terminal_returns(&self, session_id: &str, delivery_ids: &[String]) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let mut connection=self.connection.lock().unwrap_or_else(|p|p.into_inner());
        let tx=connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in delivery_ids {
            validate_delivery_id(id)?;
            tx.execute("DELETE FROM company_job_returns WHERE session_id=?1 AND delivery_id=?2 AND return_kind='terminal'",
                params![session_id,id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Permanently discard every undelivered detached-worker return for one
    /// canonical conversation. Transcript deletion uses this before removing
    /// its recovery manifest: otherwise a gateway restart could promote a
    /// previously claimed return back to `ready` and resurrect work from the
    /// deleted turn.
    pub fn purge_job_returns(&self, session_id: &str) -> Result<usize> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        Ok(connection.execute(
            "DELETE FROM company_job_returns WHERE session_id=?1",
            [session_id],
        )?)
    }

    pub fn release_job_returns(&self, session_id: &str, delivery_ids: &[String]) -> Result<()> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        for delivery_id in delivery_ids {
            validate_delivery_id(delivery_id)?;
            connection.execute(
                "UPDATE company_job_returns SET delivery_state='ready'
                 WHERE delivery_id=?1 AND session_id=?2 AND delivery_state='claimed'
                   AND owner_runtime_epoch=?3",
                params![delivery_id, session_id, self.runtime_epoch()],
            )?;
        }
        Ok(())
    }

    pub fn recover_job_returns(&self) -> Result<Vec<String>> {
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Opening a recovery path is not proof that another publisher died.
        // Check OS ownership inside the transaction that promotes the rows.
        let owners = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT owner_runtime_epoch FROM company_job_returns
                 WHERE delivery_state IN ('staged','claimed')",
            )?;
            let owners = statement.query_map([], |row| row.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            owners
        };
        for owner in owners {
            // Null identities retain the legacy gateway-start recovery path.
            if owner.is_none() || self.workflow_lease_recoverable(owner.as_deref(), None, "")? {
                tx.execute(
                    "UPDATE company_job_returns SET delivery_state='ready',owner_runtime_epoch=NULL
                     WHERE delivery_state IN ('staged','claimed') AND owner_runtime_epoch IS ?1",
                    params![owner],
                )?;
            }
        }
        let sessions = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT session_id FROM company_job_returns
                 WHERE delivery_state='ready' ORDER BY session_id",
            )?;
            let sessions = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            sessions
        };
        tx.commit()?;
        Ok(sessions)
    }

    pub(crate) fn workflow_node_run_id(&self, node_id: &str) -> Result<String> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        connection.query_row("SELECT run_id FROM workflow_nodes WHERE node_id=?1", [node_id],
            |row| row.get(0)).optional()?.context("workflow node does not exist")
    }

    /// Select continuity before loading projections: another coworker's large
    /// or corrupt workflow must not become this actor's prompt material.
    pub(crate) fn workflow_context(&self, session_id: &str, actor: &str) -> Result<(String, WorkflowSnapshot)> {
        let (identity, run_ids) = {
            let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
            let mut actors = connection.prepare(
                "SELECT agent_id,internal_role FROM company_agents WHERE lifecycle='active'
                 AND (agent_id=?1 OR internal_role=?1)")?;
            let matches = actors.query_map([actor], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            anyhow::ensure!(matches.len() == 1, "workflow context requires one active immutable actor");
            let (identity, role) = &matches[0];
            let mut rooms = connection.prepare(
                "SELECT group_id,lifecycle FROM company_groups WHERE canonical_session_id=?1
                 OR canonical_session_id || '__' || ?2=?1")?;
            let rooms = rooms.query_map(params![session_id, role], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            anyhow::ensure!(rooms.len() <= 1, "ambiguous workflow room context");
            if rooms.first().is_some_and(|(_, lifecycle)| lifecycle != "active") {
                return Ok((identity.clone(), WorkflowSnapshot::default()));
            }
            let room = rooms.first().map(|(id, _)| id.as_str());
            let mut statement = connection.prepare(
                "SELECT r.run_id FROM workflow_runs r JOIN workflow_goals g ON g.goal_id=r.goal_id
                 WHERE r.state NOT IN ('completed','canceled')
                 AND (json_extract(g.contract_json,'$.metadata.default_goal_session') IS NULL
                      OR json_extract(g.contract_json,'$.metadata.default_goal_session')=?3)
                 AND (r.owner_agent_id=?1 OR EXISTS(SELECT 1 FROM workflow_nodes n
                      WHERE n.run_id=r.run_id AND (n.owner_agent_id=?1 OR n.lease_worker=?1)
                      AND n.state NOT IN ('succeeded','canceled')))
                 AND ((r.scope<>'group' AND ?2 IS NULL) OR (r.scope='group' AND r.group_id=?2
                      AND EXISTS(SELECT 1 FROM company_group_members m WHERE m.group_id=r.group_id AND m.agent_id=?1)))
                 ORDER BY r.updated_at DESC,r.run_id LIMIT 4")?;
            let ids = statement.query_map(params![identity, room, session_id], |r| r.get::<_,String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            (identity.clone(), ids)
        };
        let mut selected = WorkflowSnapshot::default();
        for run_id in run_ids {
            let view = self.workflow_snapshot(Some(&run_id))?;
            selected.as_of_seq = selected.as_of_seq.max(view.as_of_seq);
            for goal in view.goals {
                if !selected.goals.iter().any(|g| g.goal_id == goal.goal_id) { selected.goals.push(goal); }
            }
            selected.runs.extend(view.runs);
            selected.nodes.extend(view.nodes);
            selected.edges.extend(view.edges);
        }
        Ok((identity, selected))
    }

    /// Read the bounded workflow projections rebuilt from the company event
    /// stream.  The scheduler uses this on every tick and after a process
    /// restart; no in-memory queue is authoritative.
    pub fn workflow_snapshot(&self, run_id: Option<&str>) -> Result<WorkflowSnapshot> {
        let connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let as_of_seq = if let Some(run_id) = run_id {
            connection.query_row(
                "SELECT COALESCE(MAX(company_seq),0) FROM company_events WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT COALESCE(MAX(company_seq),0) FROM company_events",
                [],
                |row| row.get(0),
            )?
        };

        let mut goals = Vec::new();
        if let Some(run_id) = run_id {
            let mut stmt = connection.prepare(
                "SELECT goal_id,title,objective,contract_json,state,created_at,updated_at,as_of_seq
                 FROM workflow_goals
                 WHERE goal_id IN (SELECT goal_id FROM workflow_runs WHERE run_id=?1)
                 ORDER BY as_of_seq LIMIT ?2",
            )?;
            for row in stmt.query_map(
                params![run_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_workflow_goal,
            )? {
                goals.push(row?);
            }
        } else {
            let mut stmt = connection.prepare(
                "SELECT goal_id,title,objective,contract_json,state,created_at,updated_at,as_of_seq
                 FROM workflow_goals ORDER BY as_of_seq LIMIT ?1",
            )?;
            for row in stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_workflow_goal)? {
                goals.push(row?);
            }
        }

        let mut runs = Vec::new();
        if let Some(run_id) = run_id {
            let mut stmt = connection.prepare(
                "SELECT run_id,goal_id,budget_json,scope,owner_agent_id,group_id,
                        state,restart_state,reason,next_wake_at,
                        created_at,updated_at,as_of_seq
                 FROM workflow_runs WHERE run_id=?1 ORDER BY as_of_seq LIMIT ?2",
            )?;
            for row in stmt.query_map(
                params![run_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_workflow_run,
            )? {
                runs.push(row?);
            }
        } else {
            let mut stmt = connection.prepare(
                "SELECT run_id,goal_id,budget_json,scope,owner_agent_id,group_id,
                        state,restart_state,reason,next_wake_at,
                        created_at,updated_at,as_of_seq
                 FROM workflow_runs ORDER BY as_of_seq LIMIT ?1",
            )?;
            for row in stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_workflow_run)? {
                runs.push(row?);
            }
        }

        let mut nodes = Vec::new();
        if let Some(run_id) = run_id {
            let mut stmt = connection.prepare(
                "SELECT node_id,run_id,parent_id,title,outcome,phase,state,dependencies_json,
                        budget_json,evidence_requirements_json,evidence_json,result_json,usage_json,
                        restart_state,reason,node_idempotency_key,lease_id,lease_worker,fencing_token,
                        lease_runtime_epoch,leased_at,heartbeat_at,lease_expires_at,attempt,
                        next_wake_at,updated_at,as_of_seq,owner_agent_id,wait_json
                 FROM workflow_nodes WHERE run_id=?1 ORDER BY as_of_seq LIMIT ?2",
            )?;
            for row in stmt.query_map(
                params![run_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_workflow_node,
            )? {
                nodes.push(row?);
            }
        } else {
            let mut stmt = connection.prepare(
                "SELECT node_id,run_id,parent_id,title,outcome,phase,state,dependencies_json,
                        budget_json,evidence_requirements_json,evidence_json,result_json,usage_json,
                        restart_state,reason,node_idempotency_key,lease_id,lease_worker,fencing_token,
                        lease_runtime_epoch,leased_at,heartbeat_at,lease_expires_at,attempt,
                        next_wake_at,updated_at,as_of_seq,owner_agent_id,wait_json
                 FROM workflow_nodes ORDER BY as_of_seq LIMIT ?1",
            )?;
            for row in stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_workflow_node)? {
                nodes.push(row?);
            }
        }

        let mut edges = Vec::new();
        if let Some(run_id) = run_id {
            let mut stmt = connection.prepare(
                "SELECT run_id,node_id,upstream_id,edge_kind,as_of_seq
                 FROM workflow_edges WHERE run_id=?1 ORDER BY as_of_seq LIMIT ?2",
            )?;
            for row in stmt.query_map(
                params![run_id, MAX_SNAPSHOT_ROWS as i64 + 1],
                row_to_workflow_edge,
            )? {
                edges.push(row?);
            }
        } else {
            let mut stmt = connection.prepare(
                "SELECT run_id,node_id,upstream_id,edge_kind,as_of_seq
                 FROM workflow_edges ORDER BY as_of_seq LIMIT ?1",
            )?;
            for row in stmt.query_map([MAX_SNAPSHOT_ROWS as i64 + 1], row_to_workflow_edge)? {
                edges.push(row?);
            }
        }

        if goals.len() > MAX_SNAPSHOT_ROWS
            || runs.len() > MAX_SNAPSHOT_ROWS
            || nodes.len() > MAX_SNAPSHOT_ROWS
            || edges.len() > MAX_SNAPSHOT_ROWS
        {
            anyhow::bail!("workflow snapshot has more than {MAX_SNAPSHOT_ROWS} rows");
        }
        let snapshot = WorkflowSnapshot {
            as_of_seq,
            goals,
            runs,
            nodes,
            edges,
        };
        if serde_json::to_vec(&snapshot)?.len() > MAX_SNAPSHOT_BYTES {
            anyhow::bail!("workflow snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
        }
        Ok(snapshot)
    }

    /// Atomically claim one ready workflow node.  Selection and projection
    /// update happen under the same SQLite write transaction, so multiple
    /// scheduler instances cannot oversubscribe the global or per-run active
    /// worker bound.  `None` means the node was no longer eligible or the
    /// bounded lane was full.
    pub fn claim_workflow_node(
        &self,
        input: NewCompanyEvent,
        expected_revision: i64,
        max_active_workers: usize,
        max_active_per_run: usize,
        now: &str,
    ) -> Result<Option<CompanyEvent>> {
        if max_active_workers == 0 || max_active_per_run == 0 {
            return Ok(None);
        }
        let (node_id, lease_runtime_epoch) = match &input.event {
            CompanyEventKind::WorkflowNodeLeased {
                node_id,
                lease_runtime_epoch,
                ..
            } => (node_id.as_str(), lease_runtime_epoch.as_str()),
            _ => anyhow::bail!("claim_workflow_node requires WorkflowNodeLeased"),
        };
        if lease_runtime_epoch != self.runtime_epoch() {
            anyhow::bail!("workflow lease runtime epoch does not match this process");
        }
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = input.idempotency_key.as_deref() {
            if let Some(existing) = event_by_key(&tx, key)? {
                anyhow::ensure!(serde_json::to_value(&existing.envelope)? == serde_json::to_value(&input)?,
                    "workflow lease idempotency key belongs to a different request");
                let CompanyEventKind::WorkflowNodeLeased {
                    lease_id, worker_id, fencing_token, ..
                } = &input.event else { unreachable!() };
                // A saved receipt is history, not permission to restart an
                // execution whose lease has expired, changed or been released.
                let current: Option<(String, Option<String>)> = tx.query_row(
                    "SELECT n.run_id,n.owner_agent_id FROM workflow_nodes n
                     JOIN workflow_runs r ON r.run_id=n.run_id
                     WHERE n.node_id=?1 AND n.lease_id=?2 AND n.lease_worker=?3
                       AND n.fencing_token=?4 AND n.lease_runtime_epoch=?5
                       AND n.state IN ('leased','running','review') AND r.state='active'
                       AND (r.next_wake_at IS NULL OR r.next_wake_at<=?6)
                       AND n.lease_expires_at>?6
                       AND (n.owner_agent_id IS NULL OR n.owner_agent_id=?3)",
                    params![node_id, lease_id, worker_id, fencing_token, lease_runtime_epoch, now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?;
                let eligible = match current {
                    Some((run, Some(owner))) => workflow_assignment_owner_eligible(&tx, &run, &owner)?,
                    Some((_, None)) => true,
                    None => false,
                };
                let eligible = eligible && workflow_prerequisites_satisfied(&tx, node_id)?;
                tx.commit()?;
                return Ok(eligible.then_some(existing));
            }
        }
        let row = tx
            .query_row(
                "SELECT run_id,state,next_wake_at,lease_expires_at,lease_runtime_epoch,as_of_seq
                 FROM workflow_nodes WHERE node_id=?1",
                [node_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((run_id, state, next_wake_at, lease_expires_at, previous_epoch, revision)) = row else {
            anyhow::bail!("workflow node `{node_id}` does not exist");
        };
        // Readiness alone is insufficient: a correction or resumed wait may
        // return a newer assignment to ready while an old snapshot survives.
        if revision != expected_revision {
            tx.commit()?;
            return Ok(None);
        }
        if !workflow_prerequisites_satisfied(&tx, node_id)? {
            tx.commit()?;
            return Ok(None);
        }
        let assigned_owner: Option<String> = tx.query_row(
            "SELECT owner_agent_id FROM workflow_nodes WHERE node_id=?1", [node_id], |row| row.get(0))?;
        if let Some(owner) = assigned_owner.as_deref() {
            let CompanyEventKind::WorkflowNodeLeased { worker_id, .. } = &input.event else { unreachable!() };
            if owner != worker_id || !workflow_assignment_owner_eligible(&tx, &run_id, owner)? {
                tx.commit()?;
                return Ok(None);
            }
        }
        let (run_state, run_wake): (String, Option<String>) = tx.query_row(
            "SELECT state,next_wake_at FROM workflow_runs WHERE run_id=?1",
            [run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if run_state != "active" || run_wake.as_deref().is_some_and(|wake| wake > now) {
            tx.commit()?;
            return Ok(None);
        }
        if next_wake_at.as_deref().is_some_and(|wake| wake > now) {
            tx.commit()?;
            return Ok(None);
        }
        let expired = matches!(state.as_str(), "leased" | "running" | "review")
            && self.workflow_lease_recoverable(previous_epoch.as_deref(), lease_expires_at.as_deref(), now)?;
        if state != "ready" && !expired {
            tx.commit()?;
            return Ok(None);
        }
        let global_active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workflow_nodes WHERE state IN ('leased','running','review')",
            [],
            |row| row.get(0),
        )?;
        let run_active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workflow_nodes WHERE run_id=?1 AND state IN ('leased','running','review')",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        // Reclaiming an expired lease does not consume another active slot.
        let global_after = global_active.saturating_sub(expired as i64);
        let run_after = run_active.saturating_sub(expired as i64);
        if global_after >= max_active_workers as i64 || run_after >= max_active_per_run as i64 {
            tx.commit()?;
            return Ok(None);
        }
        let event = self.append_in_transaction(&tx, input)?;
        tx.commit()?;
        Ok(Some(event))
    }

    /// Accept one explicitly requested resume/reroute or recover its saved
    /// acknowledgement. The scheduler validates inputs and the caller applies
    /// its normal permissions. A retry returns history, never reprojects it.
    pub(crate) fn resume_workflow_run(
        &self,
        run_id: &str,
        actor_agent_id: &str,
        new_owner_agent_id: Option<&str>,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<String> {
        let key = format!("workflow:ownership:{run_id}:{idempotency_key}");
        // Keep the existing event format. Like atomic plan installation, the
        // correlation field binds the original request, including an omitted
        // target: a later reroute must not change what that omission meant.
        let fingerprint = format!("workflow:ownership:v1:{:x}", Sha256::digest(
            serde_json::to_vec(&(run_id, actor_agent_id, new_owner_agent_id, reason))?
        ));
        let request = |owner: &str, correlation_id: Option<String>| NewCompanyEvent {
            run_id: run_id.to_string(),
            session_id: run_id.to_string(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(actor_agent_id.to_string()),
            agent_instance_id: Some(actor_agent_id.to_string()),
            causation_id: None,
            correlation_id,
            idempotency_key: Some(key.clone()),
            event: CompanyEventKind::WorkflowRunOwnershipChanged {
                workflow_run_id: run_id.to_string(),
                new_owner_agent_id: owner.to_string(),
                reason: reason.to_string(),
            },
        };
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = event_by_key(&tx, &key)? {
            let CompanyEventKind::WorkflowRunOwnershipChanged { new_owner_agent_id: owner, .. } =
                &existing.envelope.event else {
                    anyhow::bail!("workflow ownership idempotency key belongs to another event");
                };
            // Old events recorded only the effective owner, not whether the
            // caller supplied it. Compare all recorded fields for those
            // receipts; new receipts additionally require the raw request hash.
            let correlation = existing.envelope.correlation_id.as_ref().map(|_| fingerprint);
            let expected = request(new_owner_agent_id.unwrap_or(owner), correlation);
            anyhow::ensure!(serde_json::to_value(&existing.envelope)? == serde_json::to_value(&expected)?,
                "workflow ownership idempotency key belongs to a different request");
            let owner = owner.clone();
            tx.commit()?;
            return Ok(owner);
        }
        // Resolve an omitted target under the same transaction as publication.
        // The existing projector enforces paused state, active ownership and
        // group membership. Only a new accepted event reaches that projector.
        let current_owner: String = tx.query_row(
            "SELECT owner_agent_id FROM workflow_runs WHERE run_id=?1",
            [run_id], |row| row.get(0),
        ).optional()?.with_context(|| format!("workflow run `{run_id}` does not exist"))?;
        let owner = new_owner_agent_id.unwrap_or(&current_owner);
        self.append_in_transaction(&tx, request(owner, Some(fingerprint)))?;
        tx.commit()?;
        Ok(owner.to_string())
    }

    /// Commit a validated scheduler decision only while its input projection
    /// is current. Lease fencing alone cannot protect unleased ready/terminal
    /// nodes, or two different decisions made under the same active lease.
    pub(crate) fn transition_workflow_node(
        &self, node: &WorkflowNodeProjection, input: NewCompanyEvent,
    ) -> Result<CompanyEvent> {
        self.transition_workflow_node_observed(node, input, &[])
    }

    pub(crate) fn transition_workflow_node_observed(
        &self, node: &WorkflowNodeProjection, input: NewCompanyEvent,
        prerequisites: &[(&str, Option<i64>)],
    ) -> Result<CompanyEvent> {
        validate_company_message_route(&input)?;
        anyhow::ensure!(matches!(&input.event,
            CompanyEventKind::WorkflowNodeStateChanged { node_id, .. } if node_id == &node.node_id),
            "workflow transition must target the validated node");
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = input.idempotency_key.as_deref() {
            if let Some(existing) = event_by_key(&tx, key)? {
                anyhow::ensure!(serde_json::to_value(&existing.envelope.event)? == serde_json::to_value(&input.event)?,
                    "workflow transition idempotency key belongs to a different decision");
                tx.commit()?;
                return Ok(existing);
            }
        }
        let revision: Option<i64> = tx.query_row(
            "SELECT as_of_seq FROM workflow_nodes WHERE node_id=?1", [&node.node_id],
            |row| row.get(0),
        ).optional()?;
        anyhow::ensure!(revision == Some(node.as_of_seq),
            "workflow transition rejected: node changed since validation; reload before deciding");
        for (upstream, expected) in prerequisites {
            let current: Option<i64> = tx.query_row(
                "SELECT as_of_seq FROM workflow_nodes WHERE node_id=?1 AND run_id=?2",
                params![upstream, node.run_id], |row| row.get(0),
            ).optional()?;
            anyhow::ensure!(current == *expected,
                "workflow transition rejected: prerequisite {upstream} changed since validation; reload before deciding");
        }
        let event = self.append_in_transaction(&tx, input)?;
        tx.commit()?;
        Ok(event)
    }

    /// Recovery uses the snapshot revision as well as the lease fence: a
    /// heartbeat renews the SAME fence and must invalidate an expired snapshot.
    pub(crate) fn recover_workflow_node(
        &self, node: &WorkflowNodeProjection, input: NewCompanyEvent, now: &str,
    ) -> Result<bool> {
        validate_company_message_route(&input)?;
        anyhow::ensure!(matches!(&input.event,
            CompanyEventKind::WorkflowNodeStateChanged { node_id, state, clear_lease: true, .. }
                if node_id == &node.node_id && state == "ready"),
            "workflow recovery requires a ready transition with lease release");
        let mut connection = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = tx.query_row(
            "SELECT n.as_of_seq,n.state,n.lease_runtime_epoch,n.lease_expires_at,r.state,r.next_wake_at
             FROM workflow_nodes n JOIN workflow_runs r ON r.run_id=n.run_id WHERE n.node_id=?1",
            [&node.node_id], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?,
                row.get::<_,Option<String>>(2)?, row.get::<_,Option<String>>(3)?, row.get::<_,String>(4)?, row.get::<_,Option<String>>(5)?)),
        ).optional()?;
        let Some((revision, state, epoch, expires, run_state, run_wake)) = row else { return Ok(false) };
        if revision != node.as_of_seq || !matches!(state.as_str(), "leased" | "running" | "review")
            || run_state != "active"
            || run_wake.as_deref().is_some_and(|wake| wake > now)
            || !self.workflow_lease_recoverable(epoch.as_deref(), expires.as_deref(), now)? {
            return Ok(false);
        }
        self.append_in_transaction(&tx, input)?;
        tx.commit()?;
        Ok(true)
    }
}

fn workflow_prerequisites_satisfied(tx: &Transaction<'_>, node_id: &str) -> Result<bool> {
    let (run_id, parent_id, dependencies): (String, Option<String>, String) = tx.query_row(
        "SELECT run_id,parent_id,dependencies_json FROM workflow_nodes WHERE node_id=?1",
        [node_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let mut prerequisites: Vec<String> = serde_json::from_str(&dependencies)
        .context("workflow dependency projection is invalid")?;
    prerequisites.extend(parent_id);
    prerequisites.sort();
    prerequisites.dedup();
    let mut check = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM workflow_nodes
         WHERE node_id=?1 AND run_id=?2 AND state='succeeded')",
    )?;
    for upstream in prerequisites {
        let succeeded: bool = check.query_row(
            params![upstream, run_id], |row| row.get(0),
        )?;
        if !succeeded { return Ok(false); }
    }
    Ok(true)
}

fn workflow_assignment_owner_eligible(tx: &Transaction<'_>, run_id: &str, owner: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_runs r JOIN company_agents a ON a.agent_id=?2
         WHERE r.run_id=?1 AND a.lifecycle='active' AND
           (r.scope<>'group' OR EXISTS(
             SELECT 1 FROM company_groups g JOIN company_group_members m ON m.group_id=g.group_id
             WHERE g.group_id=r.group_id AND g.lifecycle='active' AND m.agent_id=a.agent_id)))",
        params![run_id, owner], |row| row.get(0),
    )?)
}

fn company_state_root(company_path: &Path) -> Result<PathBuf> {
    let parent = company_path
        .parent()
        .context("company database has no parent directory")?;
    let root = if parent.file_name().is_some_and(|name| name == "company") {
        parent
            .parent()
            .context("company database directory has no Phoenix state root")?
    } else {
        parent
    };
    anyhow::ensure!(
        root != Path::new("/"),
        "refusing to treat filesystem root as Phoenix state"
    );
    Ok(root.to_path_buf())
}

fn purge_private_state(root: &Path, item: &DuePrivatePurge) -> Result<()> {
    let mut targets = Vec::new();
    let mut add_session = |session_id: &str| {
        targets.push(root.join("sessions").join(format!("{session_id}.json")));
        targets.push(
            root.join("sessions")
                .join(format!("{session_id}.archive.jsonl")),
        );
        targets.push(
            root.join("session_cache")
                .join(format!("{session_id}.json")),
        );
        targets.push(root.join("session_cache").join(session_id));
        targets.push(root.join("checkpoints").join(session_id));
        targets.push(
            root.join("canvas-feeds")
                .join(format!("{session_id}.jsonl")),
        );
        targets.push(root.join("feeds").join(format!("{session_id}.jsonl")));
    };
    match item {
        DuePrivatePurge::Agent {
            agent_id,
            internal_role,
            canonical_session_id,
            browser_profile_id,
        } => {
            if let Some(session_id) = canonical_session_id {
                add_session(session_id);
            }
            targets.push(root.join("agents").join(internal_role));
            targets.push(
                root.join("browser")
                    .join("profiles")
                    .join(browser_profile_id),
            );
            targets.push(
                root.join("browser")
                    .join("artifacts")
                    .join(browser_profile_id),
            );
            targets.push(root.join("memory").join("agents").join(agent_id));
            targets.push(root.join("vault").join("agents").join(agent_id));
            targets.push(root.join("credentials").join("agents").join(agent_id));
        }
        DuePrivatePurge::Group {
            group_id,
            canonical_session_id,
        } => {
            if let Some(session_id) = canonical_session_id {
                add_session(session_id);
            }
            targets.push(root.join("memory").join("groups").join(group_id));
            targets.push(root.join("vault").join("groups").join(group_id));
            targets.push(root.join("credentials").join("groups").join(group_id));
        }
    }
    targets.sort();
    targets.dedup();
    for target in targets {
        remove_private_target(root, &target)?;
    }
    Ok(())
}

fn remove_private_target(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .context("private purge target escapes the Phoenix state root")?;
    anyhow::ensure!(
        relative.components().count() >= 2,
        "private purge target is broader than one owned namespace"
    );
    let mut ancestor = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        ancestor.push(component.as_os_str());
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "private purge ancestor is a symlink: {}",
                ancestor.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(target)
            .with_context(|| format!("removing private directory {}", target.display()))?;
    } else {
        std::fs::remove_file(target)
            .with_context(|| format!("removing private file {}", target.display()))?;
    }
    Ok(())
}

fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS company_events(
            company_seq INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL UNIQUE,
            runtime_epoch TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            run_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            pod_id TEXT,
            work_node_id TEXT,
            attempt_id TEXT,
            agent_identity_id TEXT,
            agent_instance_id TEXT,
            causation_id TEXT,
            correlation_id TEXT,
            idempotency_key TEXT UNIQUE,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_events_run_seq ON company_events(run_id,company_seq);
        CREATE INDEX IF NOT EXISTS company_events_work_seq ON company_events(work_node_id,company_seq);
        CREATE TABLE IF NOT EXISTS company_jobs(
            job_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            subject TEXT NOT NULL,
            state TEXT NOT NULL,
            ok INTEGER,
            verified INTEGER NOT NULL DEFAULT 0,
            summary TEXT,
            started_at TEXT NOT NULL,
            settled_at TEXT,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_jobs_session_state ON company_jobs(session_id,state);
        CREATE TABLE IF NOT EXISTS company_job_returns(
            delivery_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            agent TEXT NOT NULL,
            subject TEXT NOT NULL,
            ok INTEGER NOT NULL,
            summary TEXT NOT NULL,
            body TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            causation_id TEXT,
            delivery_state TEXT NOT NULL CHECK(delivery_state IN ('staged','ready','claimed')),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_job_returns_session_state
            ON company_job_returns(session_id,delivery_state,created_at,delivery_id);
        CREATE TABLE IF NOT EXISTS company_work(
            node_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            title TEXT NOT NULL,
            outcome TEXT NOT NULL,
            acceptance_json TEXT NOT NULL,
            dependencies_json TEXT NOT NULL,
            pattern TEXT NOT NULL,
            state TEXT NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            as_of_seq INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS company_artifacts(
            artifact_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            path TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            sources_json TEXT NOT NULL,
            state TEXT NOT NULL,
            replacement_id TEXT,
            as_of_seq INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS company_focus(
            agent_instance_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            work_node_id TEXT,
            frame_json TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS company_messages(
            message_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            operation_id TEXT NOT NULL DEFAULT '',
            handoff_id TEXT NOT NULL DEFAULT '',
            reply_to TEXT,
            causation_id TEXT,
            from_agent TEXT NOT NULL,
            to_agent TEXT NOT NULL,
            subject TEXT NOT NULL,
            body TEXT NOT NULL,
            message_kind TEXT NOT NULL,
            reply_expected INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL CHECK(state IN ('accepted','suspended','injected','canceled')),
            accepted_at TEXT NOT NULL,
            injected_at TEXT,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_messages_session_state
            ON company_messages(session_id,state,accepted_at,message_id);
        CREATE INDEX IF NOT EXISTS company_messages_delivery_order
            ON company_messages(session_id,state,as_of_seq,message_id);
        CREATE TABLE IF NOT EXISTS company_message_claims(
            message_id TEXT PRIMARY KEY REFERENCES company_messages(message_id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            owner_task_id TEXT NOT NULL,
            owner_attempt_id TEXT NOT NULL,
            claimed_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS company_group_turns(
            canonical_session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            prompt_hash TEXT NOT NULL,
            group_id TEXT NOT NULL,
            roster_fingerprint TEXT NOT NULL,
            selection TEXT NOT NULL CHECK(selection IN ('explicit','everyone')),
            active_agent_ids_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(canonical_session_id,turn_id)
        );
        CREATE INDEX IF NOT EXISTS company_group_turns_group_created
            ON company_group_turns(group_id,created_at,turn_id);
        CREATE TABLE IF NOT EXISTS company_group_turn_members(
            canonical_session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            activation_id TEXT NOT NULL UNIQUE,
            activation_ordinal INTEGER NOT NULL,
            agent_id TEXT NOT NULL,
            participant_json TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'queued','working','waiting_user','blocked','done'
            )),
            status_detail TEXT NOT NULL DEFAULT '',
            receipt_id TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(canonical_session_id,turn_id,agent_id),
            FOREIGN KEY(canonical_session_id,turn_id)
                REFERENCES company_group_turns(canonical_session_id,turn_id)
                ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS company_group_turn_members_state
            ON company_group_turn_members(state,updated_at,activation_id);
        CREATE TABLE IF NOT EXISTS company_group_asks(
            canonical_session_id TEXT NOT NULL,
            ask_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            PRIMARY KEY(canonical_session_id,ask_id),
            FOREIGN KEY(canonical_session_id,turn_id,agent_id)
                REFERENCES company_group_turn_members(canonical_session_id,turn_id,agent_id)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS company_reads(
            agent_identity_id TEXT NOT NULL,
            path TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            question TEXT NOT NULL,
            answer_ref TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL,
            PRIMARY KEY(agent_identity_id,path,content_hash,question)
        );
        CREATE TABLE IF NOT EXISTS workflow_goals(
            goal_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            objective TEXT NOT NULL,
            contract_json TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS workflow_runs(
            run_id TEXT PRIMARY KEY,
            goal_id TEXT NOT NULL,
            budget_json TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'company',
            owner_agent_id TEXT NOT NULL DEFAULT 'phoenix',
            group_id TEXT,
            state TEXT NOT NULL,
            restart_state TEXT NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            next_wake_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS workflow_runs_goal_state ON workflow_runs(goal_id,state);
        CREATE TABLE IF NOT EXISTS workflow_nodes(
            node_id TEXT PRIMARY KEY,
            owner_agent_id TEXT,
            wait_json TEXT,
            run_id TEXT NOT NULL,
            parent_id TEXT,
            title TEXT NOT NULL,
            outcome TEXT NOT NULL,
            phase TEXT NOT NULL,
            state TEXT NOT NULL,
            dependencies_json TEXT NOT NULL,
            budget_json TEXT NOT NULL,
            evidence_requirements_json TEXT NOT NULL,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            result_json TEXT,
            usage_json TEXT,
            restart_state TEXT NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            node_idempotency_key TEXT NOT NULL,
            lease_id TEXT,
            lease_worker TEXT,
            fencing_token TEXT,
            lease_runtime_epoch TEXT,
            leased_at TEXT,
            heartbeat_at TEXT,
            lease_expires_at TEXT,
            attempt INTEGER NOT NULL DEFAULT 0,
            next_wake_at TEXT,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS workflow_nodes_run_state ON workflow_nodes(run_id,state);
        CREATE INDEX IF NOT EXISTS workflow_nodes_wake ON workflow_nodes(state,next_wake_at);
        CREATE TABLE IF NOT EXISTS workflow_edges(
            run_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            upstream_id TEXT NOT NULL,
            edge_kind TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL,
            PRIMARY KEY(node_id,upstream_id,edge_kind)
        );"
    )?;
    if !sqlite_column_exists(connection, "company_group_turn_members", "source_receipt_id")? {
        connection.execute("ALTER TABLE company_group_turn_members ADD COLUMN source_receipt_id TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "company_group_turns", "activation_json")? {
        connection.execute("ALTER TABLE company_group_turns ADD COLUMN activation_json TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "company_group_turns", "original_request")? {
        connection.execute("ALTER TABLE company_group_turns ADD COLUMN original_request TEXT", [])?;
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS company_group_continuations (
            canonical_session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            dispatched INTEGER NOT NULL DEFAULT 0,
            dispatch_payload_json TEXT,
            PRIMARY KEY(canonical_session_id,turn_id)
        );",
    )?;
    if !sqlite_column_exists(connection, "workflow_nodes", "owner_agent_id")? {
        connection.execute("ALTER TABLE workflow_nodes ADD COLUMN owner_agent_id TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "workflow_nodes", "wait_json")? {
        connection.execute("ALTER TABLE workflow_nodes ADD COLUMN wait_json TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "workflow_runs", "scope")? {
        connection.execute(
            "ALTER TABLE workflow_runs ADD COLUMN scope TEXT NOT NULL DEFAULT 'company'",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "workflow_runs", "owner_agent_id")? {
        connection.execute(
            "ALTER TABLE workflow_runs ADD COLUMN owner_agent_id TEXT NOT NULL DEFAULT 'phoenix'",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "workflow_runs", "group_id")? {
        connection.execute("ALTER TABLE workflow_runs ADD COLUMN group_id TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "company_job_returns", "causation_id")? {
        connection.execute(
            "ALTER TABLE company_job_returns ADD COLUMN causation_id TEXT",
            [],
        )?;
    }
    // Early durable-return queues published rows immediately as `ready`.
    // Add an invisible `staged` state so the full body can commit before the
    // postbox's lifecycle lock makes either Returned or Absorbed observable.
    // SQLite cannot ALTER a CHECK constraint, so preserve every receipt while
    // rebuilding only legacy two-state tables.
    let job_return_schema: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='company_job_returns'",
        [],
        |row| row.get(0),
    )?;
    if !job_return_schema.contains("'staged'") {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE company_job_returns_v2(
                delivery_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                agent TEXT NOT NULL,
                subject TEXT NOT NULL,
                ok INTEGER NOT NULL,
                summary TEXT NOT NULL,
                body TEXT NOT NULL,
                finished_at TEXT NOT NULL,
                causation_id TEXT,
                delivery_state TEXT NOT NULL CHECK(delivery_state IN ('staged','ready','claimed')),
                created_at TEXT NOT NULL
             );
             INSERT INTO company_job_returns_v2
                SELECT delivery_id,session_id,agent,subject,ok,summary,body,finished_at,
                       causation_id,delivery_state,created_at
                  FROM company_job_returns;
             DROP TABLE company_job_returns;
             ALTER TABLE company_job_returns_v2 RENAME TO company_job_returns;
             CREATE INDEX company_job_returns_session_state
                ON company_job_returns(session_id,delivery_state,created_at,delivery_id);
             COMMIT;",
        )?;
    }
    if !sqlite_column_exists(connection, "company_job_returns", "owner_runtime_epoch")? {
        connection.execute(
            "ALTER TABLE company_job_returns ADD COLUMN owner_runtime_epoch TEXT",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "company_job_returns", "return_kind")? {
        connection.execute(
            "ALTER TABLE company_job_returns ADD COLUMN return_kind TEXT NOT NULL DEFAULT 'specialist'",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "company_messages", "operation_id")? {
        connection.execute(
            "ALTER TABLE company_messages ADD COLUMN operation_id TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "company_messages", "handoff_id")? {
        connection.execute(
            "ALTER TABLE company_messages ADD COLUMN handoff_id TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !sqlite_column_exists(connection, "company_messages", "reply_to")? {
        connection.execute("ALTER TABLE company_messages ADD COLUMN reply_to TEXT", [])?;
    }
    if !sqlite_column_exists(connection, "company_messages", "causation_id")? {
        connection.execute(
            "ALTER TABLE company_messages ADD COLUMN causation_id TEXT",
            [],
        )?;
    }
    // Early rebuild snapshots created `company_messages` with a two-state
    // CHECK constraint. Expand it in-place without losing accepted receipts;
    // SQLite cannot ALTER a CHECK constraint directly.
    let message_schema: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='company_messages'",
        [],
        |row| row.get(0),
    )?;
    if !message_schema.contains("'suspended'") || !message_schema.contains("'canceled'") {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE company_messages_v2(
                message_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                operation_id TEXT NOT NULL DEFAULT '',
                handoff_id TEXT NOT NULL DEFAULT '',
                reply_to TEXT,
                causation_id TEXT,
                from_agent TEXT NOT NULL,
                to_agent TEXT NOT NULL,
                subject TEXT NOT NULL,
                body TEXT NOT NULL,
                message_kind TEXT NOT NULL,
                reply_expected INTEGER NOT NULL DEFAULT 0,
                state TEXT NOT NULL CHECK(state IN ('accepted','suspended','injected','canceled')),
                accepted_at TEXT NOT NULL,
                injected_at TEXT,
                as_of_seq INTEGER NOT NULL
             );
             INSERT INTO company_messages_v2
                SELECT message_id,run_id,session_id,operation_id,handoff_id,reply_to,causation_id,
                       from_agent,to_agent,subject,body,message_kind,reply_expected,state,
                       accepted_at,injected_at,as_of_seq
                  FROM company_messages;
             DROP TABLE company_messages;
             ALTER TABLE company_messages_v2 RENAME TO company_messages;
             CREATE INDEX company_messages_session_state
                ON company_messages(session_id,state,accepted_at,message_id);
             COMMIT;",
        )?;
    }
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS company_messages_operation
            ON company_messages(operation_id)
         WHERE operation_id <> ''",
        [],
    )?;
    Ok(())
}

fn validate_delivery_id(delivery_id: &str) -> Result<()> {
    anyhow::ensure!(
        delivery_id.starts_with("return_")
            && delivery_id.len() <= 80
            && delivery_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid coworker return delivery id"
    );
    Ok(())
}

fn validate_company_message_identity(label: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.len() <= MAX_JOB_RETURN_TEXT_BYTES
            && !value.contains('\0'),
        "company message {label} is empty or too large"
    );
    Ok(())
}

fn stable_company_message_id(session_id: &str, operation_id: &str) -> String {
    let digest = Sha256::digest(format!("{session_id}\0{operation_id}").as_bytes());
    format!("message_{digest:x}")
}

fn validate_job_return(job: &crate::runtime::postbox::CompletedJob) -> Result<()> {
    anyhow::ensure!(
        !job.agent.trim().is_empty() && job.agent.len() <= MAX_JOB_RETURN_TEXT_BYTES,
        "coworker return agent is empty or too large"
    );
    anyhow::ensure!(
        !job.subject.trim().is_empty() && job.subject.len() <= MAX_JOB_RETURN_TEXT_BYTES,
        "coworker return subject is empty or too large"
    );
    anyhow::ensure!(
        job.summary.len() <= MAX_JOB_RETURN_TEXT_BYTES,
        "coworker return summary is too large"
    );
    anyhow::ensure!(
        job.body.len() <= MAX_JOB_RETURN_BODY_BYTES,
        "coworker return body is too large"
    );
    if !job.delivery_id.is_empty() {
        validate_delivery_id(&job.delivery_id)?;
    }
    if let Some(causation_id) = &job.causation_id {
        anyhow::ensure!(
            !causation_id.trim().is_empty() && causation_id.len() <= MAX_JOB_RETURN_TEXT_BYTES,
            "coworker return causation id is empty or too large"
        );
    }
    Ok(())
}

fn select_job_returns(
    connection: &Connection,
    session_id: &str,
    state: &str,
) -> Result<Vec<crate::runtime::postbox::CompletedJob>> {
    anyhow::ensure!(
        matches!(state, "ready" | "claimed"),
        "invalid delivery state"
    );
    let mut statement = connection.prepare(
        "SELECT delivery_id,causation_id,agent,subject,ok,summary,body,finished_at,return_kind
         FROM company_job_returns
         WHERE session_id=?1 AND delivery_state=?2
         ORDER BY created_at,delivery_id LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![session_id, state, MAX_JOB_RETURNS_PER_SESSION],
        |row| {
            let finished_at = row.get::<_, String>(7)?;
            let finished = chrono::DateTime::parse_from_rfc3339(&finished_at)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            Ok(crate::runtime::postbox::CompletedJob {
                kind: crate::runtime::postbox::ReturnKind::from_stored(&row.get::<_, String>(8)?)
                    .map_err(|error|rusqlite::Error::FromSqlConversionFailure(8,rusqlite::types::Type::Text,error.into()))?,
        delivery_id: row.get(0)?,
                causation_id: row.get(1)?,
                agent: row.get(2)?,
                subject: row.get(3)?,
                ok: row.get::<_, i64>(4)? != 0,
                summary: row.get(5)?,
                body: row.get(6)?,
                finished,
            })
        },
    )?;
    let jobs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for job in &jobs {
        validate_job_return(job)?;
    }
    Ok(jobs)
}

fn sqlite_column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    anyhow::ensure!(
        table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid SQLite table identifier"
    );
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_company_text_bounds(connection: &Connection) -> Result<()> {
    let oversized: i64 = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM company_events
             WHERE length(CAST(event_id AS BLOB))>?1
                OR length(CAST(runtime_epoch AS BLOB))>?1
                OR length(CAST(recorded_at AS BLOB))>?1
                OR length(CAST(event_type AS BLOB))>?1
                OR length(CAST(payload AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(session_id AS BLOB))>?1
                OR length(CAST(pod_id AS BLOB))>?1
                OR length(CAST(work_node_id AS BLOB))>?1
                OR length(CAST(attempt_id AS BLOB))>?1
                OR length(CAST(agent_identity_id AS BLOB))>?1
                OR length(CAST(agent_instance_id AS BLOB))>?1
                OR length(CAST(causation_id AS BLOB))>?1
                OR length(CAST(correlation_id AS BLOB))>?1
                OR length(CAST(idempotency_key AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_jobs
             WHERE length(CAST(job_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(session_id AS BLOB))>?1
                OR length(CAST(role AS BLOB))>?1
                OR length(CAST(subject AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(summary AS BLOB))>?1
                OR length(CAST(started_at AS BLOB))>?1
                OR length(CAST(settled_at AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_work
             WHERE length(CAST(node_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(title AS BLOB))>?1
                OR length(CAST(outcome AS BLOB))>?1
                OR length(CAST(acceptance_json AS BLOB))>?1
                OR length(CAST(dependencies_json AS BLOB))>?1
                OR length(CAST(pattern AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(reason AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_artifacts
             WHERE length(CAST(artifact_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(path AS BLOB))>?1
                OR length(CAST(content_hash AS BLOB))>?1
                OR length(CAST(sources_json AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(replacement_id AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_focus
             WHERE length(CAST(agent_instance_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(work_node_id AS BLOB))>?1
                OR length(CAST(frame_json AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_messages
             WHERE length(CAST(message_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(session_id AS BLOB))>?1
                OR length(CAST(operation_id AS BLOB))>?1
                OR length(CAST(handoff_id AS BLOB))>?1
                OR length(CAST(reply_to AS BLOB))>?1
                OR length(CAST(causation_id AS BLOB))>?1
                OR length(CAST(from_agent AS BLOB))>?1
                OR length(CAST(to_agent AS BLOB))>?1
                OR length(CAST(subject AS BLOB))>?1
                OR length(CAST(body AS BLOB))>?1
                OR length(CAST(message_kind AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(accepted_at AS BLOB))>?1
                OR length(CAST(injected_at AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM company_reads
             WHERE length(CAST(agent_identity_id AS BLOB))>?1
                OR length(CAST(path AS BLOB))>?1
                OR length(CAST(content_hash AS BLOB))>?1
                OR length(CAST(question AS BLOB))>?1
                OR length(CAST(answer_ref AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM workflow_goals
             WHERE length(CAST(goal_id AS BLOB))>?1
                OR length(CAST(title AS BLOB))>?1
                OR length(CAST(objective AS BLOB))>?1
                OR length(CAST(contract_json AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(created_at AS BLOB))>?1
                OR length(CAST(updated_at AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM workflow_runs
             WHERE length(CAST(run_id AS BLOB))>?1
                OR length(CAST(goal_id AS BLOB))>?1
                OR length(CAST(budget_json AS BLOB))>?1
                OR length(CAST(scope AS BLOB))>?1
                OR length(CAST(owner_agent_id AS BLOB))>?1
                OR length(CAST(group_id AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(restart_state AS BLOB))>?1
                OR length(CAST(reason AS BLOB))>?1
                OR length(CAST(next_wake_at AS BLOB))>?1
                OR length(CAST(created_at AS BLOB))>?1
                OR length(CAST(updated_at AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM workflow_nodes
             WHERE length(CAST(node_id AS BLOB))>?1
                OR length(CAST(run_id AS BLOB))>?1
                OR length(CAST(parent_id AS BLOB))>?1
                OR length(CAST(title AS BLOB))>?1
                OR length(CAST(outcome AS BLOB))>?1
                OR length(CAST(phase AS BLOB))>?1
                OR length(CAST(state AS BLOB))>?1
                OR length(CAST(dependencies_json AS BLOB))>?1
                OR length(CAST(budget_json AS BLOB))>?1
                OR length(CAST(evidence_requirements_json AS BLOB))>?1
                OR length(CAST(evidence_json AS BLOB))>?1
                OR length(CAST(result_json AS BLOB))>?1
                OR length(CAST(usage_json AS BLOB))>?1
                OR length(CAST(restart_state AS BLOB))>?1
                OR length(CAST(reason AS BLOB))>?1
                OR length(CAST(node_idempotency_key AS BLOB))>?1
                OR length(CAST(lease_id AS BLOB))>?1
                OR length(CAST(lease_worker AS BLOB))>?1
                OR length(CAST(fencing_token AS BLOB))>?1
                OR length(CAST(lease_runtime_epoch AS BLOB))>?1
                OR length(CAST(leased_at AS BLOB))>?1
                OR length(CAST(heartbeat_at AS BLOB))>?1
                OR length(CAST(lease_expires_at AS BLOB))>?1
                OR length(CAST(next_wake_at AS BLOB))>?1
                OR length(CAST(updated_at AS BLOB))>?1
            UNION ALL
            SELECT 1 FROM workflow_edges
             WHERE length(CAST(run_id AS BLOB))>?1
                OR length(CAST(node_id AS BLOB))>?1
                OR length(CAST(upstream_id AS BLOB))>?1
                OR length(CAST(edge_kind AS BLOB))>?1
            LIMIT 1
        )",
        [MAX_EVENT_BYTES as i64],
        |row| row.get(0),
    )?;
    if oversized != 0 {
        anyhow::bail!("company store contains an oversized text field");
    }
    Ok(())
}

fn validate_sqlite_sidecars(path: &Path) -> Result<()> {
    for (suffix, max_bytes) in [
        ("-wal", 1024 * 1024 * 1024u64),
        ("-shm", 64 * 1024 * 1024u64),
    ] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                anyhow::bail!(
                    "refusing unsafe company SQLite sidecar {}",
                    sidecar.display()
                );
            }
            Ok(metadata) if metadata.len() > max_bytes => {
                anyhow::bail!(
                    "company SQLite sidecar {} is too large ({} bytes; max {max_bytes})",
                    sidecar.display(),
                    metadata.len()
                );
            }
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn event_name(event: &CompanyEventKind) -> &'static str {
    match event {
        CompanyEventKind::RunOpened { .. } => "run_opened",
        CompanyEventKind::AgentRegistered { .. } => "agent_registered",
        CompanyEventKind::PodFormed { .. } => "pod_formed",
        CompanyEventKind::WorkProposed { .. } => "work_proposed",
        CompanyEventKind::WorkClaimed { .. } => "work_claimed",
        CompanyEventKind::WorkStateChanged { .. } => "work_state_changed",
        CompanyEventKind::FocusUpdated { .. } => "focus_updated",
        CompanyEventKind::MessageAccepted { .. } => "message_accepted",
        CompanyEventKind::MessageInjected { .. } => "message_injected",
        CompanyEventKind::JobStarted { .. } => "job_started",
        CompanyEventKind::JobSettled { .. } => "job_settled",
        CompanyEventKind::ArtifactPublished { .. } => "artifact_published",
        CompanyEventKind::ArtifactSuperseded { .. } => "artifact_superseded",
        CompanyEventKind::ChallengeRaised { .. } => "challenge_raised",
        CompanyEventKind::DecisionRecorded { .. } => "decision_recorded",
        CompanyEventKind::ReadRecorded { .. } => "read_recorded",
        CompanyEventKind::SkillActivated { .. } => "skill_activated",
        CompanyEventKind::SkillCheckpoint { .. } => "skill_checkpoint",
        CompanyEventKind::LearningProposed { .. } => "learning_proposed",
        CompanyEventKind::LearningPromoted { .. } => "learning_promoted",
        CompanyEventKind::WorkflowGoalCreated { .. } => "workflow_goal_created",
        CompanyEventKind::WorkflowRunOpened { .. } => "workflow_run_opened",
        CompanyEventKind::WorkflowNodeDefined { .. } => "workflow_node_defined",
        CompanyEventKind::WorkflowNodeLeased { .. } => "workflow_node_leased",
        CompanyEventKind::WorkflowNodeHeartbeat { .. } => "workflow_node_heartbeat",
        CompanyEventKind::WorkflowNodeStateChanged { .. } => "workflow_node_state_changed",
        CompanyEventKind::WorkflowEvidenceRecorded { .. } => "workflow_evidence_recorded",
        CompanyEventKind::WorkflowRunStateChanged { .. } => "workflow_run_state_changed",
        CompanyEventKind::WorkflowRunOwnershipChanged { .. } => "workflow_run_ownership_changed",
        CompanyEventKind::DirectoryChanged { .. } => "directory_changed",
    }
}

fn project(
    tx: &Transaction<'_>,
    seq: i64,
    at: &DateTime<Utc>,
    input: &NewCompanyEvent,
) -> Result<()> {
    match &input.event {
        CompanyEventKind::JobStarted {
            job_id,
            role,
            subject,
        } => {
            tx.execute(
                "INSERT OR IGNORE INTO company_jobs(job_id,run_id,session_id,role,subject,state,started_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,'reasoning',?6,?7)",
                params![job_id, input.run_id, input.session_id, role, subject, at.to_rfc3339(), seq],
            )?;
        }
        CompanyEventKind::JobSettled {
            job_id,
            ok,
            verified,
            summary,
        } => {
            let state = if *ok && *verified {
                "completed_verified"
            } else if *ok {
                "completed_unverified"
            } else {
                "failed"
            };
            tx.execute(
                "UPDATE company_jobs SET state=?1,ok=?2,verified=?3,summary=?4,settled_at=?5,as_of_seq=?6 WHERE job_id=?7",
                params![state, *ok as i64, *verified as i64, summary, at.to_rfc3339(), seq, job_id],
            )?;
        }
        CompanyEventKind::WorkProposed {
            node_id,
            title,
            outcome,
            acceptance,
            dependencies,
            pattern,
        } => {
            tx.execute(
                "INSERT OR REPLACE INTO company_work(node_id,run_id,title,outcome,acceptance_json,dependencies_json,pattern,state,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,'proposed',?8)",
                params![node_id, input.run_id, title, outcome, serde_json::to_string(acceptance)?, serde_json::to_string(dependencies)?,
                    serde_json::to_string(pattern)?.trim_matches('"'), seq],
            )?;
        }
        CompanyEventKind::WorkStateChanged {
            node_id,
            state,
            reason,
        } => {
            let changed = tx.execute(
                "UPDATE company_work SET state=?1,reason=?2,as_of_seq=?3
                 WHERE node_id=?4 AND run_id=?5",
                params![state.as_str(), reason, seq, node_id, input.run_id],
            )?;
            if changed != 1 {
                anyhow::bail!(
                    "work node `{node_id}` does not exist in run `{}`",
                    input.run_id
                );
            }
        }
        CompanyEventKind::FocusUpdated { frame } => {
            let instance = input.agent_instance_id.as_deref().unwrap_or("unknown");
            tx.execute(
                "INSERT OR REPLACE INTO company_focus(agent_instance_id,run_id,work_node_id,frame_json,as_of_seq) VALUES(?1,?2,?3,?4,?5)",
                params![instance, input.run_id, input.work_node_id, serde_json::to_string(frame)?, seq],
            )?;
        }
        CompanyEventKind::MessageAccepted {
            message_id,
            operation_id,
            handoff_id,
            reply_to,
            causation_id,
            from,
            to,
            subject,
            body,
            message_kind,
            reply_expected,
        } => {
            if *message_kind == MessageKind::Conversation && to != "company" {
                tx.execute(
                    "INSERT INTO company_messages(
                    message_id,run_id,session_id,operation_id,handoff_id,reply_to,causation_id,
                    from_agent,to_agent,subject,body,message_kind,reply_expected,state,accepted_at,
                    as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'accepted',?14,?15)",
                    params![
                        message_id,
                        input.run_id,
                        input.session_id,
                        operation_id,
                        handoff_id,
                        reply_to,
                        causation_id,
                        from,
                        to,
                        subject,
                        body,
                        serde_json::to_string(message_kind)?.trim_matches('"'),
                        *reply_expected as i64,
                        at.to_rfc3339(),
                        seq
                    ],
                )?;
            }
        }
        CompanyEventKind::MessageInjected { message_id } => {
            let changed = tx.execute(
                "UPDATE company_messages SET state='injected',injected_at=?1,as_of_seq=?2
                 WHERE message_id=?3 AND session_id=?4",
                params![at.to_rfc3339(), seq, message_id, input.session_id],
            )?;
            anyhow::ensure!(changed == 1, "message `{message_id}` was not accepted");
        }
        CompanyEventKind::ArtifactPublished {
            artifact_id,
            path,
            content_hash,
            source_artifacts,
        } => {
            tx.execute(
                "INSERT OR REPLACE INTO company_artifacts(artifact_id,run_id,path,content_hash,sources_json,state,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,'current',?6)",
                params![artifact_id, input.run_id, path, content_hash, serde_json::to_string(source_artifacts)?, seq],
            )?;
        }
        CompanyEventKind::ArtifactSuperseded {
            artifact_id,
            replacement_id,
            ..
        } => {
            tx.execute("UPDATE company_artifacts SET state='superseded',replacement_id=?1,as_of_seq=?2 WHERE artifact_id=?3",
                params![replacement_id, seq, artifact_id])?;
        }
        CompanyEventKind::ReadRecorded {
            path,
            content_hash,
            question,
            answer_ref,
        } => {
            let identity = input.agent_identity_id.as_deref().unwrap_or("unknown");
            tx.execute(
                "INSERT OR REPLACE INTO company_reads(agent_identity_id,path,content_hash,question,answer_ref,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![identity, path, content_hash, question, answer_ref, seq],
            )?;
        }
        CompanyEventKind::WorkflowGoalCreated {
            goal_id,
            title,
            objective,
            contract_json,
        } => {
            tx.execute(
                "INSERT INTO workflow_goals
                 (goal_id,title,objective,contract_json,state,created_at,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,'open',?5,?5,?6)",
                params![
                    goal_id,
                    title,
                    objective,
                    contract_json,
                    at.to_rfc3339(),
                    seq
                ],
            )?;
        }
        CompanyEventKind::WorkflowRunOpened {
            workflow_run_id,
            goal_id,
            budget_json,
            scope,
            owner_agent_id,
            group_id,
            restart_state,
            next_wake_at,
        } => {
            let goal_exists: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_goals WHERE goal_id=?1)",
                [goal_id],
                |row| row.get(0),
            )?;
            if goal_exists == 0 {
                anyhow::bail!("workflow goal `{goal_id}` does not exist");
            }
            tx.execute(
                "INSERT INTO workflow_runs
                 (run_id,goal_id,budget_json,scope,owner_agent_id,group_id,state,restart_state,
                  reason,next_wake_at,created_at,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,'planned',?7,'',?8,?9,?9,?10)",
                params![
                    workflow_run_id,
                    goal_id,
                    budget_json,
                    scope,
                    owner_agent_id,
                    group_id,
                    restart_state,
                    next_wake_at,
                    at.to_rfc3339(),
                    seq
                ],
            )?;
            tx.execute(
                "UPDATE workflow_goals SET state='active',updated_at=?1,as_of_seq=?2 WHERE goal_id=?3",
                params![at.to_rfc3339(), seq, goal_id],
            )?;
        }
        CompanyEventKind::WorkflowNodeDefined {
            node_id,
            owner_agent_id,
            title,
            outcome,
            parent_id,
            phase,
            dependencies,
            budget_json,
            evidence_requirements_json,
            node_idempotency_key,
        } => {
            let run_id = &input.run_id;
            let run_exists: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE run_id=?1)",
                [run_id],
                |row| row.get(0),
            )?;
            if run_exists == 0 {
                anyhow::bail!("workflow run `{run_id}` does not exist");
            }
            let run_state: String = tx.query_row(
                "SELECT state FROM workflow_runs WHERE run_id=?1", [run_id], |row| row.get(0),
            )?;
            anyhow::ensure!(!matches!(run_state.as_str(), "completed" | "failed" | "canceled"),
                "workflow run `{run_id}` is {run_state}; new assignments require a nonterminal run");
            if let Some(owner) = owner_agent_id {
                anyhow::ensure!(workflow_assignment_owner_eligible(tx, run_id, owner)?,
                    "workflow assignment owner must be active and belong to the run's group");
            }
            if parent_id.as_deref() == Some(node_id)
                || dependencies.iter().any(|dependency| dependency == node_id)
            {
                anyhow::bail!("workflow node `{node_id}` cannot depend on itself");
            }
            tx.execute(
                "INSERT INTO workflow_nodes
                 (node_id,run_id,parent_id,title,outcome,phase,state,dependencies_json,
                  budget_json,evidence_requirements_json,evidence_json,restart_state,reason,
                  node_idempotency_key,attempt,updated_at,as_of_seq,owner_agent_id)
                 VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?8,?9,'[]','fresh','',?10,0,?11,?12,?13)",
                params![
                    node_id,
                    run_id,
                    parent_id,
                    title,
                    outcome,
                    phase,
                    serde_json::to_string(dependencies)?,
                    budget_json,
                    evidence_requirements_json,
                    node_idempotency_key,
                    at.to_rfc3339(),
                    seq,
                    owner_agent_id
                ],
            )?;
            if let Some(parent_id) = parent_id {
                tx.execute(
                    "INSERT INTO workflow_edges(run_id,node_id,upstream_id,edge_kind,as_of_seq)
                     VALUES(?1,?2,?3,'parent',?4)",
                    params![run_id, node_id, parent_id, seq],
                )?;
            }
            for dependency in dependencies {
                tx.execute(
                    "INSERT INTO workflow_edges(run_id,node_id,upstream_id,edge_kind,as_of_seq)
                     VALUES(?1,?2,?3,'dependency',?4)",
                    params![run_id, node_id, dependency, seq],
                )?;
            }
        }
        CompanyEventKind::WorkflowNodeLeased {
            node_id,
            lease_id,
            worker_id,
            fencing_token,
            lease_runtime_epoch,
            leased_at,
            heartbeat_at,
            expires_at,
            attempt,
        } => {
            let (run_id, owner): (String, Option<String>) = tx.query_row(
                "SELECT run_id,owner_agent_id FROM workflow_nodes WHERE node_id=?1", [node_id],
                |row| Ok((row.get(0)?, row.get(1)?)))?;
            if let Some(owner) = owner {
                anyhow::ensure!(&owner == worker_id && workflow_assignment_owner_eligible(tx, &run_id, &owner)?,
                    "workflow lease does not match its active assignment owner");
            }
            let changed = tx.execute(
                "UPDATE workflow_nodes SET state='leased',lease_id=?1,lease_worker=?2,
                    fencing_token=?3,lease_runtime_epoch=?4,leased_at=?5,heartbeat_at=?6,
                    lease_expires_at=?7,attempt=?8,restart_state='fresh',reason='',
                    updated_at=?9,as_of_seq=?10
                 WHERE node_id=?11 AND (
                    state='ready' OR
                    (state IN ('leased','running','review') AND
                     (lease_runtime_epoch IS NULL OR lease_runtime_epoch<>?4 OR lease_expires_at<=?5))
                 )",
                params![
                    lease_id,
                    worker_id,
                    fencing_token,
                    lease_runtime_epoch,
                    leased_at,
                    heartbeat_at,
                    expires_at,
                    *attempt as i64,
                    at.to_rfc3339(),
                    seq,
                    node_id
                ],
            )?;
            if changed != 1 {
                anyhow::bail!("workflow node `{node_id}` is not ready for this lease");
            }
        }
        CompanyEventKind::WorkflowNodeHeartbeat {
            node_id,
            lease_id,
            worker_id,
            fencing_token,
            heartbeat_at,
            expires_at,
        } => {
            let changed = tx.execute(
                "UPDATE workflow_nodes SET heartbeat_at=?1,lease_expires_at=?2,
                    updated_at=?3,as_of_seq=?4
                 WHERE node_id=?5 AND lease_id=?6 AND lease_worker=?7 AND fencing_token=?8
                   AND state IN ('leased','running','review') AND lease_expires_at>?1",
                params![
                    heartbeat_at,
                    expires_at,
                    at.to_rfc3339(),
                    seq,
                    node_id,
                    lease_id,
                    worker_id,
                    fencing_token
                ],
            )?;
            if changed != 1 {
                anyhow::bail!("workflow heartbeat rejected by the lease fence for `{node_id}`");
            }
        }
        CompanyEventKind::WorkflowNodeStateChanged {
            node_id,
            phase,
            state,
            restart_state,
            reason,
            next_wake_at,
            result_json,
            usage_json,
            evidence_json,
            lease_id,
            fencing_token,
            clear_lease,
            wait_json,
        } => {
            let current = tx
                .query_row(
                    "SELECT lease_id,fencing_token FROM workflow_nodes WHERE node_id=?1",
                    [node_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()?;
            let current_state: String = tx.query_row(
                "SELECT state FROM workflow_nodes WHERE node_id=?1",
                [node_id],
                |row| row.get(0),
            )?;
            let Some((current_lease, current_fence)) = current else {
                anyhow::bail!("workflow node `{node_id}` does not exist");
            };
            if matches!(current_state.as_str(), "leased" | "running" | "review")
                && lease_id.is_none()
            {
                anyhow::bail!("active workflow state update requires a lease fence");
            }
            if lease_id.is_some() && (lease_id != &current_lease || fencing_token != &current_fence)
            {
                anyhow::bail!("workflow state update rejected by the lease fence for `{node_id}`");
            }
            if let Some(usage_json) = usage_json.as_deref() {
                enforce_workflow_run_budget(tx, node_id, usage_json)?;
            }
            let changed = tx.execute(
                "UPDATE workflow_nodes SET phase=?1,state=?2,restart_state=?3,reason=?4,
                    next_wake_at=?5,result_json=COALESCE(?6,result_json),
                    usage_json=COALESCE(?7,usage_json),evidence_json=COALESCE(?8,evidence_json),
                    lease_id=CASE WHEN ?9 THEN NULL ELSE lease_id END,
                    lease_worker=CASE WHEN ?9 THEN NULL ELSE lease_worker END,
                    fencing_token=CASE WHEN ?9 THEN NULL ELSE fencing_token END,
                    lease_runtime_epoch=CASE WHEN ?9 THEN NULL ELSE lease_runtime_epoch END,
                    leased_at=CASE WHEN ?9 THEN NULL ELSE leased_at END,
                    heartbeat_at=CASE WHEN ?9 THEN NULL ELSE heartbeat_at END,
                    lease_expires_at=CASE WHEN ?9 THEN NULL ELSE lease_expires_at END,
                    updated_at=?10,as_of_seq=?11,wait_json=?13 WHERE node_id=?12",
                params![
                    phase,
                    state,
                    restart_state,
                    reason,
                    next_wake_at,
                    result_json,
                    usage_json,
                    evidence_json,
                    *clear_lease as i64,
                    at.to_rfc3339(),
                    seq,
                    node_id,
                    wait_json
                ],
            )?;
            if changed != 1 {
                anyhow::bail!("workflow node `{node_id}` state update did not apply");
            }
        }
        CompanyEventKind::WorkflowEvidenceRecorded {
            node_id,
            receipt_json,
            lease_id,
            worker_id,
            fencing_token,
        } => {
            let current = tx
                .query_row(
                    "SELECT evidence_json,lease_id,lease_worker,fencing_token,state
                     FROM workflow_nodes WHERE node_id=?1",
                    [node_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?
                .context("workflow node disappeared while recording evidence")?;
            let (existing, current_lease, current_worker, current_fence, state) = current;
            if !matches!(state.as_str(), "leased" | "running" | "review")
                || current_lease.as_deref() != Some(lease_id)
                || current_worker.as_deref() != Some(worker_id)
                || current_fence.as_deref() != Some(fencing_token)
            {
                anyhow::bail!("workflow evidence rejected by the lease fence for node");
            }
            let mut receipts: Vec<serde_json::Value> = serde_json::from_str(&existing)
                .context("workflow evidence projection is not a JSON array")?;
            receipts.push(
                serde_json::from_str(receipt_json).context("invalid workflow evidence receipt")?,
            );
            tx.execute(
                "UPDATE workflow_nodes SET evidence_json=?1,updated_at=?2,as_of_seq=?3 WHERE node_id=?4",
                params![serde_json::to_string(&receipts)?, at.to_rfc3339(), seq, node_id],
            )?;
        }
        CompanyEventKind::WorkflowRunStateChanged {
            workflow_run_id,
            state,
            restart_state,
            reason,
            next_wake_at,
        } => {
            if state == "active" {
                let planned: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE run_id=?1 AND state='planned')",
                    [workflow_run_id], |row| row.get(0),
                )?;
                anyhow::ensure!(planned,
                    "automatic workflow activation requires a planned run; paused work must use explicit resume");
            }
            if state == "completed" {
                // Completion and assignment insertion use this same write
                // transaction boundary; neither ordering may strand work.
                let complete: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workflow_runs r WHERE r.run_id=?1
                     AND r.state IN ('planned','active')
                     AND EXISTS(SELECT 1 FROM workflow_nodes n WHERE n.run_id=r.run_id)
                     AND NOT EXISTS(SELECT 1 FROM workflow_nodes n
                                    WHERE n.run_id=r.run_id AND n.state<>'succeeded'))",
                    [workflow_run_id], |row| row.get(0),
                )?;
                anyhow::ensure!(complete,
                    "workflow completion rejected: run changed or has unfinished assignments; reload before deciding");
            }
            let changed = tx.execute(
                "UPDATE workflow_runs SET state=?1,restart_state=?2,reason=?3,next_wake_at=?4,
                    updated_at=?5,as_of_seq=?6 WHERE run_id=?7",
                params![
                    state,
                    restart_state,
                    reason,
                    next_wake_at,
                    at.to_rfc3339(),
                    seq,
                    workflow_run_id
                ],
            )?;
            if changed != 1 {
                anyhow::bail!("workflow run `{workflow_run_id}` does not exist");
            }
        }
        CompanyEventKind::WorkflowRunOwnershipChanged {
            workflow_run_id,
            new_owner_agent_id,
            reason,
        } => {
            let (scope, group_id, state) = tx
                .query_row(
                    "SELECT scope,group_id,state FROM workflow_runs WHERE run_id=?1",
                    [workflow_run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
                .with_context(|| format!("workflow run `{workflow_run_id}` does not exist"))?;
            anyhow::ensure!(
                state == "paused",
                "only a paused workflow may be resumed or rerouted"
            );
            let active_owner: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM company_agents
                  WHERE agent_id=?1 AND lifecycle='active')",
                [new_owner_agent_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                active_owner == 1,
                "new workflow owner is not an active coworker"
            );
            if scope == "group" {
                let group_id = group_id.context("group workflow has no group_id")?;
                let member: i64 = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM company_group_members
                      WHERE group_id=?1 AND agent_id=?2)",
                    params![group_id, new_owner_agent_id],
                    |row| row.get(0),
                )?;
                anyhow::ensure!(
                    member == 1,
                    "new workflow owner is not a member of its group"
                );
            }
            tx.execute(
                "UPDATE workflow_runs SET owner_agent_id=?1,state='active',restart_state='requeued',
                    reason=?2,next_wake_at=NULL,updated_at=?3,as_of_seq=?4 WHERE run_id=?5",
                params![new_owner_agent_id, reason, at.to_rfc3339(), seq, workflow_run_id],
            )?;
        }
        CompanyEventKind::DirectoryChanged { change } => {
            let lifecycle_effect = directory_lifecycle_effect(tx, change)?;
            super::company_directory::project(tx, seq, at, change)?;
            apply_directory_lifecycle_effect(tx, seq, at, lifecycle_effect)?;
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug)]
enum DirectoryLifecycleEffect {
    Agent {
        agent_id: String,
        internal_role: String,
        terminal: bool,
        active: bool,
    },
    Group {
        group_id: String,
        canonical_session_id: Option<String>,
        terminal: bool,
    },
}

fn directory_lifecycle_effect(
    tx: &Transaction<'_>,
    change: &super::company_directory::DirectoryChange,
) -> Result<Option<DirectoryLifecycleEffect>> {
    use super::company_directory::{DirectoryChange, LifecycleState};
    let agent = |agent_id: &str, terminal: bool, active: bool| -> Result<_> {
        let role = tx
            .query_row(
                "SELECT internal_role FROM company_agents WHERE agent_id=?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(role.map(|internal_role| DirectoryLifecycleEffect::Agent {
            agent_id: agent_id.to_string(),
            internal_role,
            terminal,
            active,
        }))
    };
    let group = |group_id: &str, terminal: bool| -> Result<_> {
        let session = tx
            .query_row(
                "SELECT canonical_session_id FROM company_groups WHERE group_id=?1",
                [group_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(Some(DirectoryLifecycleEffect::Group {
            group_id: group_id.to_string(),
            canonical_session_id: session,
            terminal,
        }))
    };
    match change {
        DirectoryChange::AgentLifecycleSet {
            agent_id,
            lifecycle,
            ..
        } => agent(agent_id, false, *lifecycle == LifecycleState::Active),
        DirectoryChange::AgentDeletionScheduled { agent_id, .. } => agent(agent_id, false, false),
        DirectoryChange::AgentPurged { agent_id } => agent(agent_id, true, false),
        DirectoryChange::GroupLifecycleSet {
            group_id,
            lifecycle,
            ..
        } => {
            if *lifecycle == LifecycleState::Active {
                // Recompute below after the directory projection has restored
                // the group; `terminal=false` is enough to distinguish purge.
            }
            group(group_id, false)
        }
        DirectoryChange::GroupDeletionScheduled { group_id, .. } => group(group_id, false),
        DirectoryChange::GroupPurged { group_id } => group(group_id, true),
        _ => Ok(None),
    }
}

fn apply_directory_lifecycle_effect(
    tx: &Transaction<'_>,
    seq: i64,
    at: &DateTime<Utc>,
    effect: Option<DirectoryLifecycleEffect>,
) -> Result<()> {
    let Some(effect) = effect else {
        return Ok(());
    };
    let timestamp = at.to_rfc3339();
    match effect {
        DirectoryLifecycleEffect::Agent {
            agent_id,
            internal_role,
            terminal,
            active,
        } => {
            if terminal {
                tx.execute(
                    "UPDATE company_messages SET state='canceled',as_of_seq=?1
                     WHERE state IN ('accepted','suspended')
                       AND (to_agent=?2 OR (?2='phoenix' AND to_agent='orchestrator'))",
                    params![seq, internal_role],
                )?;
                tx.execute(
                    "DELETE FROM workflow_edges WHERE run_id IN (
                        SELECT run_id FROM workflow_runs WHERE scope='agent' AND owner_agent_id=?1
                    )",
                    [&agent_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_nodes WHERE run_id IN (
                        SELECT run_id FROM workflow_runs WHERE scope='agent' AND owner_agent_id=?1
                    )",
                    [&agent_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_runs WHERE scope='agent' AND owner_agent_id=?1",
                    [&agent_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_goals WHERE goal_id NOT IN (SELECT goal_id FROM workflow_runs)",
                    [],
                )?;
            }
            if !active {
                let worker_pattern = format!("{internal_role}#%");
                let reason = format!(
                    "coworker `{internal_role}` is unavailable; explicit owner rerouting is required"
                );
                tx.execute(
                    "UPDATE workflow_runs SET state='paused',restart_state='waiting',reason=?1,
                        next_wake_at=NULL,updated_at=?2,as_of_seq=?3
                     WHERE state IN ('planned','active') AND (
                        owner_agent_id=?4 OR run_id IN (
                            SELECT DISTINCT run_id FROM workflow_nodes
                             WHERE lease_worker=?5 OR lease_worker LIKE ?6
                        )
                     )",
                    params![
                        reason,
                        timestamp,
                        seq,
                        agent_id,
                        internal_role,
                        worker_pattern
                    ],
                )?;
                tx.execute(
                    "UPDATE workflow_nodes SET state='ready',restart_state='requeued',reason=?1,
                        lease_id=NULL,lease_worker=NULL,fencing_token=NULL,lease_runtime_epoch=NULL,
                        leased_at=NULL,heartbeat_at=NULL,lease_expires_at=NULL,next_wake_at=NULL,
                        updated_at=?2,as_of_seq=?3
                     WHERE state IN ('leased','running','review')
                       AND (lease_worker=?4 OR lease_worker LIKE ?5)",
                    params![reason, timestamp, seq, internal_role, worker_pattern],
                )?;
                tx.execute(
                    "UPDATE company_jobs SET state='stale',ok=0,verified=0,summary=?1,
                        settled_at=?2,as_of_seq=?3
                     WHERE state NOT IN ('completed_verified','completed_unverified','failed','superseded','stale')
                       AND (role=?4 OR role LIKE ?5)",
                    params![reason, timestamp, seq, internal_role, worker_pattern],
                )?;
            }
        }
        DirectoryLifecycleEffect::Group {
            group_id,
            canonical_session_id,
            terminal,
        } => {
            if terminal {
                if let Some(session_id) = canonical_session_id {
                    tx.execute(
                        "UPDATE company_messages SET state='canceled',as_of_seq=?1
                         WHERE session_id=?2 AND state IN ('accepted','suspended')",
                        params![seq, session_id],
                    )?;
                }
                tx.execute(
                    "DELETE FROM workflow_edges WHERE run_id IN (
                        SELECT run_id FROM workflow_runs WHERE scope='group' AND group_id=?1
                    )",
                    [&group_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_nodes WHERE run_id IN (
                        SELECT run_id FROM workflow_runs WHERE scope='group' AND group_id=?1
                    )",
                    [&group_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_runs WHERE scope='group' AND group_id=?1",
                    [&group_id],
                )?;
                tx.execute(
                    "DELETE FROM workflow_goals WHERE goal_id NOT IN (SELECT goal_id FROM workflow_runs)",
                    [],
                )?;
            }
        }
    }

    // Delivery is enabled only when the recipient coworker is active and, for
    // a canonical group thread, that group is active too. Recomputing instead
    // of blindly toggling prevents restoring an agent from accidentally waking
    // messages that remain paused by an archived group (and vice versa).
    tx.execute(
        "UPDATE company_messages
            SET state=CASE
                WHEN EXISTS(
                    SELECT 1 FROM company_agents a
                     WHERE a.lifecycle='active'
                       AND (a.internal_role=company_messages.to_agent
                            OR (a.internal_role='phoenix' AND company_messages.to_agent='orchestrator'))
                ) AND NOT EXISTS(
                    SELECT 1 FROM company_groups g
                     WHERE g.canonical_session_id=company_messages.session_id
                       AND g.lifecycle!='active'
                ) THEN 'accepted' ELSE 'suspended' END,
                as_of_seq=?1
          WHERE state IN ('accepted','suspended')
            AND EXISTS(
                SELECT 1 FROM company_agents a
                 WHERE a.internal_role=company_messages.to_agent
                    OR (a.internal_role='phoenix' AND company_messages.to_agent='orchestrator')
            )",
        [seq],
    )?;
    Ok(())
}

/// Enforce the run-level aggregate budget inside the same write transaction
/// that records a node usage receipt. Node usage is cumulative for that node;
/// token, cost, and iteration dimensions sum across nodes, while wall time is
/// the maximum observed node wall time.
fn enforce_workflow_run_budget(
    tx: &Transaction<'_>,
    node_id: &str,
    incoming_json: &str,
) -> Result<()> {
    let (run_id, budget_json): (String, String) = tx.query_row(
        "SELECT n.run_id,r.budget_json FROM workflow_nodes n
         JOIN workflow_runs r ON r.run_id=n.run_id WHERE n.node_id=?1",
        [node_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let budget: serde_json::Value =
        serde_json::from_str(&budget_json).context("workflow run budget is not valid JSON")?;
    let incoming: serde_json::Value =
        serde_json::from_str(incoming_json).context("workflow node usage is not valid JSON")?;
    let mut input_tokens = incoming
        .get("input_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut output_tokens = incoming
        .get("output_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut cost_micros = incoming
        .get("cost_micros")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut iterations = incoming
        .get("iterations")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut wall_seconds = incoming
        .get("wall_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut stmt = tx.prepare(
        "SELECT usage_json FROM workflow_nodes
         WHERE run_id=?1 AND node_id<>?2 AND usage_json IS NOT NULL",
    )?;
    for row in stmt.query_map(params![run_id, node_id], |row| row.get::<_, String>(0))? {
        let usage: serde_json::Value =
            serde_json::from_str(&row?).context("workflow node usage projection is invalid")?;
        input_tokens = input_tokens.saturating_add(
            usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        output_tokens = output_tokens.saturating_add(
            usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        cost_micros = cost_micros.saturating_add(
            usage
                .get("cost_micros")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        iterations = iterations.saturating_add(
            usage
                .get("iterations")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        wall_seconds = wall_seconds.max(
            usage
                .get("wall_seconds")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
    }
    let over = |field: &str, value: u64| {
        budget
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|limit| value > limit)
    };
    if over("max_input_tokens", input_tokens)
        || over("max_output_tokens", output_tokens)
        || over(
            "max_total_tokens",
            input_tokens.saturating_add(output_tokens),
        )
        || over("max_cost_micros", cost_micros)
        || over("max_wall_seconds", wall_seconds)
        || over("max_iterations", iterations)
    {
        anyhow::bail!("workflow run budget exceeded");
    }
    Ok(())
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<CompanyEvent> {
    let at: String = row.get(3)?;
    let payload: String = row.get(14)?;
    let event = serde_json::from_str(&payload).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(CompanyEvent {
        schema_version: COMPANY_SCHEMA_VERSION,
        company_seq: row.get(0)?,
        event_id: row.get(1)?,
        runtime_epoch: row.get(2)?,
        recorded_at: DateTime::parse_from_rfc3339(&at)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc),
        envelope: NewCompanyEvent {
            run_id: row.get(4)?,
            session_id: row.get(5)?,
            pod_id: row.get(6)?,
            work_node_id: row.get(7)?,
            attempt_id: row.get(8)?,
            agent_identity_id: row.get(9)?,
            agent_instance_id: row.get(10)?,
            causation_id: row.get(11)?,
            correlation_id: row.get(12)?,
            idempotency_key: row.get(13)?,
            event,
        },
    })
}

fn event_by_key(tx: &Transaction<'_>, key: &str) -> Result<Option<CompanyEvent>> {
    tx.query_row(
        "SELECT company_seq,event_id,runtime_epoch,recorded_at,run_id,session_id,pod_id,
                work_node_id,attempt_id,agent_identity_id,agent_instance_id,causation_id,
                correlation_id,idempotency_key,payload FROM company_events WHERE idempotency_key=?1",
        [key], row_to_event,
    ).optional().map_err(Into::into)
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobProjection> {
    let state: String = row.get(5)?;
    let state = parse_json_column::<AgentState>(&format!("\"{state}\""), 5)?;
    let ok: Option<i64> = row.get(6)?;
    Ok(JobProjection {
        job_id: row.get(0)?,
        run_id: row.get(1)?,
        session_id: row.get(2)?,
        role: row.get(3)?,
        subject: row.get(4)?,
        state,
        ok: ok.map(|v| v != 0),
        verified: row.get::<_, i64>(7)? != 0,
        summary: row.get(8)?,
        started_at: row.get(9)?,
        settled_at: row.get(10)?,
        as_of_seq: row.get(11)?,
    })
}

fn row_to_work(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkProjection> {
    let acceptance: String = row.get(4)?;
    let dependencies: String = row.get(5)?;
    let pattern: String = row.get(6)?;
    let state: String = row.get(7)?;
    Ok(WorkProjection {
        node_id: row.get(0)?,
        run_id: row.get(1)?,
        title: row.get(2)?,
        outcome: row.get(3)?,
        acceptance: parse_json_column(&acceptance, 4)?,
        dependencies: parse_json_column(&dependencies, 5)?,
        pattern: parse_json_column(&format!("\"{pattern}\""), 6)?,
        state: parse_json_column(&format!("\"{state}\""), 7)?,
        reason: row.get(8)?,
        as_of_seq: row.get(9)?,
    })
}

fn row_to_workflow_goal(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowGoalProjection> {
    Ok(WorkflowGoalProjection {
        goal_id: row.get(0)?,
        title: row.get(1)?,
        objective: row.get(2)?,
        contract_json: row.get(3)?,
        state: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        as_of_seq: row.get(7)?,
    })
}

fn row_to_workflow_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRunProjection> {
    Ok(WorkflowRunProjection {
        run_id: row.get(0)?,
        goal_id: row.get(1)?,
        budget_json: row.get(2)?,
        scope: row.get(3)?,
        owner_agent_id: row.get(4)?,
        group_id: row.get(5)?,
        state: row.get(6)?,
        restart_state: row.get(7)?,
        reason: row.get(8)?,
        next_wake_at: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        as_of_seq: row.get(12)?,
    })
}

fn row_to_workflow_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowNodeProjection> {
    let dependencies: String = row.get(7)?;
    Ok(WorkflowNodeProjection {
        node_id: row.get(0)?,
        owner_agent_id: row.get(27)?,
        wait_json: row.get(28)?,
        run_id: row.get(1)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        outcome: row.get(4)?,
        phase: row.get(5)?,
        state: row.get(6)?,
        dependencies: parse_json_column(&dependencies, 7)?,
        budget_json: row.get(8)?,
        evidence_requirements_json: row.get(9)?,
        evidence_json: row.get(10)?,
        result_json: row.get(11)?,
        usage_json: row.get(12)?,
        restart_state: row.get(13)?,
        reason: row.get(14)?,
        node_idempotency_key: row.get(15)?,
        lease_id: row.get(16)?,
        lease_worker: row.get(17)?,
        fencing_token: row.get(18)?,
        lease_runtime_epoch: row.get(19)?,
        leased_at: row.get(20)?,
        heartbeat_at: row.get(21)?,
        lease_expires_at: row.get(22)?,
        attempt: row.get::<_, i64>(23)? as u32,
        next_wake_at: row.get(24)?,
        updated_at: row.get(25)?,
        as_of_seq: row.get(26)?,
    })
}

fn row_to_workflow_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowEdgeProjection> {
    Ok(WorkflowEdgeProjection {
        run_id: row.get(0)?,
        node_id: row.get(1)?,
        upstream_id: row.get(2)?,
        edge_kind: row.get(3)?,
        as_of_seq: row.get(4)?,
    })
}

fn parse_json_column<T: serde::de::DeserializeOwned>(
    value: &str,
    index: usize,
) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn validate_group_turn_id(turn_id: &str) -> Result<()> {
    anyhow::ensure!(
        (8..=128).contains(&turn_id.len())
            && turn_id
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') }),
        "invalid group turn_id"
    );
    Ok(())
}

fn group_prompt_hash(prompt: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"phoenix-group-turn-prompt-v1\0");
    digest.update(prompt.as_bytes());
    format!("{:x}", digest.finalize())
}

fn stable_group_activation_id(
    canonical_session_id: &str,
    turn_id: &str,
    group_id: &str,
    agent_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"phoenix-group-member-activation-v1\0");
    for component in [canonical_session_id, turn_id, group_id, agent_id] {
        digest.update(component.as_bytes());
        digest.update([0]);
    }
    format!("group-activation-{:x}", digest.finalize())
}

fn persist_ready_group_continuation(
    tx: &Transaction<'_>,
    session_id: &str,
    original_turn_id: &str,
) -> Result<()> {
    use super::group_conversation::{dependency_waves, GroupExecutionMode};
    let record = load_group_turn_ledger_record(tx, session_id, original_turn_id)?
        .context("continuation ancestor disappeared")?;
    let ids = record.ready_frontier();
    if ids.is_empty() { return Ok(()); }
    let mut activation = record.activation.context("ready work has no stored plan")?;
    activation.active_agent_ids = ids.clone();
    // A continuation is an explicit subset even if the original user selected
    // everyone; retaining Everyone would fail the current-roster validator.
    activation.selection = super::group_conversation::GroupActivationSelection::Explicit;
    let edges = activation.execution_dependencies.as_mut().context("ready work has no exact dependencies")?;
    let needed_predecessors = edges.iter()
        .filter(|edge| ids.contains(&edge.dependent) && !ids.contains(&edge.prerequisite))
        .map(|edge| edge.prerequisite.clone()).collect::<std::collections::HashSet<_>>();
    edges.retain(|edge| ids.contains(&edge.prerequisite) && ids.contains(&edge.dependent));
    activation.execution_mode = if edges.is_empty() { GroupExecutionMode::Parallel } else { GroupExecutionMode::Ordered };
    activation.execution_waves = dependency_waves(&ids, edges)?;
    let mut digest = Sha256::new();
    digest.update(b"phoenix-ready-group-v1\0");
    digest.update(serde_json::to_vec(&(session_id, original_turn_id, &ids))?);
    let turn_id = format!("group_ready_{:x}", digest.finalize());
    let continuation = GroupReadyContinuation {
        canonical_session_id: session_id.to_string(),
        original_turn_id: original_turn_id.to_string(),
        turn_id: turn_id.clone(),
        activation,
        predecessor_receipts: record.members.iter()
            .filter(|member| member.has_committed_result() && needed_predecessors.contains(&member.participant.agent_id))
            .filter_map(|member| member.receipt_id.clone()).collect(),
        original_request: tx.query_row(
            "SELECT original_request FROM company_group_turns WHERE canonical_session_id=?1 AND turn_id=?2",
            params![session_id, original_turn_id], |row| row.get(0),
        )?,
    };
    tx.execute(
        "INSERT INTO company_group_continuations(canonical_session_id,turn_id,payload_json) VALUES(?1,?2,?3)",
        params![session_id, turn_id, serde_json::to_string(&continuation)?],
    )?;
    for id in ids {
        let changed = tx.execute(
            "UPDATE company_group_turn_members SET state='blocked',status_detail=?1,receipt_id=?2,updated_at=?3
             WHERE canonical_session_id=?4 AND turn_id=?5 AND agent_id=?6 AND state='queued'",
            params![format!("Work continues in successor turn {turn_id}"), turn_id, Utc::now().to_rfc3339(), session_id, original_turn_id, id],
        )?;
        anyhow::ensure!(changed == 1, "ready group task changed during ownership transfer");
    }
    Ok(())
}

fn load_group_turn_ledger_record(
    connection: &Connection,
    canonical_session_id: &str,
    turn_id: &str,
) -> Result<Option<super::group_conversation::GroupTurnLedgerRecord>> {
    use super::group_conversation::{
        GroupActivationSelection, GroupMemberActivationRecord, GroupMemberActivationState,
        GroupTurnLedgerRecord,
    };

    let header = connection
        .query_row(
            "SELECT prompt_hash,group_id,roster_fingerprint,selection,
                    active_agent_ids_json,created_at,updated_at,activation_json
             FROM company_group_turns
             WHERE canonical_session_id=?1 AND turn_id=?2",
            params![canonical_session_id, turn_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        prompt_hash,
        group_id,
        roster_fingerprint,
        selection,
        active_agent_ids_json,
        created_at,
        updated_at,
        activation_json,
    )) = header
    else {
        return Ok(None);
    };
    let selection = match selection.as_str() {
        "explicit" => GroupActivationSelection::Explicit,
        "everyone" => GroupActivationSelection::Everyone,
        _ => anyhow::bail!("invalid group activation selection `{selection}`"),
    };
    let active_agent_ids: Vec<String> = serde_json::from_str(&active_agent_ids_json)
        .context("invalid active-agent ids in group turn ledger")?;
    let mut statement = connection.prepare(
        "SELECT activation_id,participant_json,state,status_detail,receipt_id,updated_at,source_receipt_id
         FROM company_group_turn_members
         WHERE canonical_session_id=?1 AND turn_id=?2
         ORDER BY activation_ordinal,activation_id",
    )?;
    let rows = statement.query_map(params![canonical_session_id, turn_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut members = Vec::new();
    for row in rows {
        let (activation_id, participant_json, state, status_detail, receipt_id, member_updated_at, source_receipt_id) =
            row?;
        let participant = serde_json::from_str(&participant_json)
            .context("invalid participant snapshot in group turn ledger")?;
        let state = GroupMemberActivationState::from_str(&state)?;
        anyhow::ensure!(
            !matches!(
                state,
                GroupMemberActivationState::WaitingUser | GroupMemberActivationState::Done
            ) || receipt_id
                .as_ref()
                .is_some_and(|receipt| !receipt.trim().is_empty()),
            "group activation state `{}` is missing its durable receipt",
            state.as_str()
        );
        members.push(GroupMemberActivationRecord {
            activation_id,
            participant,
            state,
            status_detail,
            receipt_id,
            source_receipt_id,
            updated_at: member_updated_at,
        });
    }
    anyhow::ensure!(
        members
            .iter()
            .filter(|member| member.source_receipt_id.is_none())
            .map(|member| member.participant.agent_id.as_str())
            .eq(active_agent_ids.iter().map(String::as_str)),
        "group turn member rows do not match their immutable activation list"
    );
    Ok(Some(GroupTurnLedgerRecord {
        activation: activation_json.map(|json| serde_json::from_str(&json)).transpose().context("invalid persisted group activation")?,
        canonical_session_id: canonical_session_id.to_string(),
        turn_id: turn_id.to_string(),
        prompt_hash,
        group_id,
        roster_fingerprint,
        selection,
        active_agent_ids,
        members,
        created_at,
        updated_at,
    }))
}

fn global_cell() -> &'static Mutex<Option<Arc<CompanyStore>>> {
    static CELL: OnceLock<Mutex<Option<Arc<CompanyStore>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide company store only when startup has already opened
/// it.  Name/identity resolution uses this non-creating view so a harmless
/// parser cannot unexpectedly initialize or migrate the company database (or
/// recurse while company startup itself is still running).
pub fn global_if_initialized() -> Option<Arc<CompanyStore>> {
    global_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(Arc::clone)
}

/// Open the company database for the currently selected Phoenix home without
/// touching the process-global cache. Setup and migration pipelines use this
/// so isolated homes (tests, import previews, recovery) never cross-contaminate
/// one another while the long-running gateway still shares [`global`].
pub fn open_current_home_store() -> Result<CompanyStore> {
    CompanyStore::open(
        crate::config::phoenix_home()
            .join("company")
            .join("company.sqlite"),
    )
}

fn humanize_directory_role(role: &str) -> String {
    role.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| format!("{}{}", first.to_ascii_uppercase(), chars.as_str()))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn directory_agent_color(role: &str) -> String {
    const PALETTE: [&str; 12] = [
        "#E06C52", "#6E7DE8", "#36A17C", "#D65C9A", "#C68932", "#4E96A8", "#8A67B8", "#437FC7",
        "#B45A62", "#658B4D", "#95664C", "#5A75A6",
    ];
    let index = role.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    }) % PALETTE.len();
    PALETTE[index].to_string()
}

pub fn global() -> Result<Arc<CompanyStore>> {
    let mut cell = global_cell().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(store) = cell.as_ref() {
        return Ok(Arc::clone(store));
    }
    let path = if cfg!(test) {
        std::env::temp_dir().join(format!(
            "phoenix-company-test-{}.sqlite",
            std::process::id()
        ))
    } else {
        crate::config::phoenix_home()
            .join("company")
            .join("company.sqlite")
    };
    let store = Arc::new(CompanyStore::open(path)?);
    if !cfg!(test) {
        let sessions_root = crate::config::paths::phoenix_sessions_root();
        let has_history = std::fs::read_dir(&sessions_root)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".json") && !name.starts_with('.'))
            });
        store.ensure_founding_team(has_history)?;
        store.reconcile_custom_agent_registry()?;
        if has_history {
            store.reconcile_session_history(&sessions_root)?;
        }
    }
    *cell = Some(Arc::clone(&store));
    Ok(store)
}

pub fn mirror_job_started(session_id: &str, role: &str, subject: &str) {
    if let Err(error) = global().and_then(|s| s.start_job(session_id, session_id, role, subject)) {
        tracing::warn!("company event mirror could not record job start: {error:#}");
    }
}

pub fn mirror_job_settled(session_id: &str, role: &str, subject: &str, ok: bool, summary: &str) {
    if let Err(error) =
        global().and_then(|s| s.settle_matching_job(session_id, role, subject, ok, summary))
    {
        tracing::warn!("company event mirror could not record job settlement: {error:#}");
    }
}

pub fn mirror_message_accepted(
    session_id: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> String {
    match global()
        .and_then(|store| store.accept_company_message(session_id, from, to, subject, body, false))
    {
        Ok(message_id) => message_id,
        Err(error) => {
            tracing::warn!("company event mirror could not record accepted message: {error:#}");
            String::new()
        }
    }
}

pub fn mirror_message_injected(session_id: &str, message_id: &str, to: &str) {
    if message_id.is_empty() {
        return;
    }
    if let Err(error) =
        global().and_then(|store| store.inject_company_message(session_id, message_id, to))
    {
        tracing::warn!("company event mirror could not record injected message: {error:#}");
    }
}

pub fn mirror_read(session_id: &str, agent: &str, path: &Path, question: &str, answer_ref: &str) {
    let content_hash = match hash_mirrored_read(path) {
        Ok(hash) => hash,
        Err(error) => {
            tracing::warn!(
                "company event mirror could not hash read {}: {error:#}",
                path.display()
            );
            return;
        }
    };
    let input = NewCompanyEvent {
        run_id: session_id.to_string(),
        session_id: session_id.to_string(),
        pod_id: None,
        work_node_id: None,
        attempt_id: None,
        agent_identity_id: Some(agent.to_string()),
        agent_instance_id: Some(agent.to_string()),
        causation_id: None,
        correlation_id: None,
        idempotency_key: Some(format!(
            "read:{session_id}:{agent}:{}:{content_hash}:{question}",
            path.display()
        )),
        event: CompanyEventKind::ReadRecorded {
            path: path.to_string_lossy().to_string(),
            content_hash,
            question: question.to_string(),
            answer_ref: answer_ref.to_string(),
        },
    };
    if let Err(error) = global().and_then(|store| store.append(input)) {
        tracing::warn!("company event mirror could not record read receipt: {error:#}");
    }
}

fn hash_mirrored_read(path: &Path) -> Result<String> {
    crate::config::private_io::reject_symlink_components(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting mirrored read {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("mirrored read source is not a regular file");
    }
    if metadata.len() > MAX_MIRRORED_READ_BYTES {
        anyhow::bail!(
            "mirrored read source is too large ({} bytes; max {MAX_MIRRORED_READ_BYTES})",
            metadata.len()
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening mirrored read {}", path.display()))?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("mirrored read source changed to a non-regular file");
    }
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading mirrored read {}", path.display()))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_MIRRORED_READ_BYTES {
            anyhow::bail!("mirrored read source grew beyond its size limit while reading");
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn mirror_skill_activated(session_id: &str, agent: &str, skill: &str) {
    let checkpoints = match skill {
        "slides" | "presentation" => vec![
            "template_selected",
            "sources_resolved",
            "rendered",
            "visual_verified",
        ],
        "frontend-design" | "design" => {
            vec!["references_loaded", "candidate_compared", "visual_diff"]
        }
        _ => vec!["instructions_applied", "outcome_verified"],
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    let input = NewCompanyEvent {
        run_id: session_id.to_string(),
        session_id: session_id.to_string(),
        pod_id: None,
        work_node_id: None,
        attempt_id: None,
        agent_identity_id: Some(agent.to_string()),
        agent_instance_id: Some(agent.to_string()),
        causation_id: None,
        correlation_id: None,
        idempotency_key: Some(format!("skill:{session_id}:{agent}:{skill}")),
        event: CompanyEventKind::SkillActivated {
            skill: skill.to_string(),
            version: "session-pin".to_string(),
            required_checkpoints: checkpoints,
        },
    };
    if let Err(error) = global().and_then(|store| store.append(input)) {
        tracing::warn!("company event mirror could not record skill activation: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stopped_group_handoffs_stay_canceled_after_reopen_without_touching_other_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let add = |session: &str, operation: &str, from: &str, to: &str, cause: Option<&str>, reply: Option<&str>| {
            store.accept_company_message_with_identity(session, operation, None, reply, cause,
                from, to, "Fixture", operation, reply.is_none()).unwrap()
        };
        // Stop may arrive before this accepted request ever gets a claim.
        let queued = add("group-stop", "queued", "phoenix", "coder", Some("stopped-turn"), None);
        let started = add("group-stop", "started", "phoenix", "critic", Some("stopped-turn"), None);
        store.inject_company_message("group-stop", &started.message_id, "critic").unwrap();
        let nested = add("group-stop", "nested", "critic", "researcher", Some(&started.message_id), None);
        let returned = add("group-stop", "return", "researcher", "critic", None, Some(&nested.handoff_id));
        store.connection.lock().unwrap().execute("UPDATE company_messages SET state='suspended' WHERE message_id=?1", [&nested.message_id]).unwrap();
        let other = add("group-stop", "other", "phoenix", "coder", Some("other-turn"), None);
        let other_room = add("other-room", "other-room", "phoenix", "coder", Some("stopped-turn"), None);
        // Legacy receipt with no causation still has an authoritative claim.
        let claimed = add("group-stop", "claimed", "phoenix", "planner", None, None);
        let claim = store.try_claim_company_message("group-stop", &claimed.message_id, "stopped-turn", "owned-attempt").unwrap().unwrap();
        drop(claim);
        assert_eq!(store.cancel_pending_group_turn_messages("group-stop", "stopped-turn").unwrap(), 4);
        assert_eq!(store.cancel_pending_group_turn_messages("group-stop", "stopped-turn").unwrap(), 0);
        drop(store);
        let reopened = CompanyStore::open(&path).unwrap();
        let pending = reopened.pending_company_messages("group-stop").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_id, other.message_id);
        assert_eq!(reopened.pending_company_messages("other-room").unwrap()[0].message_id, other_room.message_id);
        let connection = reopened.connection.lock().unwrap();
        for id in [&queued.message_id, &nested.message_id, &returned.message_id, &claimed.message_id] {
            let state: String = connection.query_row("SELECT state FROM company_messages WHERE message_id=?1", [id], |row| row.get(0)).unwrap();
            assert_eq!(state, "canceled");
        }
        let state: String = connection.query_row("SELECT state FROM company_messages WHERE message_id=?1", [&started.message_id], |row| row.get(0)).unwrap();
        assert_eq!(state, "injected", "keep already delivered history intact");
    }
    use super::*;

    fn event(run: &str, key: &str, event: CompanyEventKind) -> NewCompanyEvent {
        NewCompanyEvent {
            run_id: run.into(),
            session_id: run.into(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: None,
            agent_instance_id: None,
            causation_id: None,
            correlation_id: None,
            idempotency_key: Some(key.into()),
            event,
        }
    }

    fn v6_personal_logistics_profile() -> super::super::company_directory::AgentProfile {
        let mut profile = super::super::company_directory::role_catalog_profiles(true)
            .into_iter()
            .find(|profile| profile.agent_id == "personal_logistics")
            .unwrap();
        profile.role_title = V6_PERSONAL_LOGISTICS_TITLE.into();
        profile.description = V6_PERSONAL_LOGISTICS_DESCRIPTION.into();
        profile
    }

    fn v6_personal_logistics_responsibility() -> super::super::company_directory::DirectoryChange {
        super::super::company_directory::DirectoryChange::ResponsibilityUpserted {
            responsibility_id: "responsibility_personal_logistics".into(),
            agent_id: "personal_logistics".into(),
            title: V6_PERSONAL_LOGISTICS_TITLE.into(),
            scope: V6_PERSONAL_LOGISTICS_SCOPE.into(),
            success_criteria: V6_PERSONAL_LOGISTICS_CRITERIA
                .iter()
                .map(|criterion| (*criterion).to_string())
                .collect(),
            approval_policy_json: FOUNDING_APPROVAL_POLICY.into(),
            escalation_policy_json: FOUNDING_ESCALATION_POLICY.into(),
            status: "active".into(),
        }
    }

    #[test]
    fn append_is_idempotent_and_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let input = event(
            "run-a",
            "open-a",
            CompanyEventKind::RunOpened {
                mission: "ship it".into(),
            },
        );
        let first = store.append(input.clone()).unwrap();
        let second = store.append(input).unwrap();
        assert_eq!(first.event_id, second.event_id);
        let replay = store.events_since(0, 10).unwrap();
        assert_eq!(replay.len(), 1);
        assert!(matches!(
            replay[0].envelope.event,
            CompanyEventKind::RunOpened { .. }
        ));
    }

    #[test]
    fn stale_undelivered_handoffs_are_canceled_and_stop_all_drops_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(&dir.path().join("company.sqlite")).unwrap();
        let stale = store.accept_company_message("main-x", "orchestrator", "frontend",
            "Build the five-project portfolio", "Old challenge work.", true).unwrap();
        let fresh = store.accept_company_message("main-x", "orchestrator", "frontend",
            "Open the five SVGs", "Current request.", false).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::hours(13)).to_rfc3339();
        store.connection.lock().unwrap().execute(
            "UPDATE company_messages SET accepted_at=?1 WHERE message_id=?2", params![old, stale]).unwrap();
        let pending = store.pending_company_messages("main-x").unwrap();
        assert_eq!(pending.iter().map(|message| message.message_id.clone()).collect::<Vec<_>>(), vec![fresh]);
        assert_eq!(store.cancel_all_pending_messages("main-x").unwrap(), 1);
        assert!(store.pending_company_messages("main-x").unwrap().is_empty());
    }

    #[test]
    fn accepted_company_handoffs_survive_until_receiver_persists_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let message_id = store
            .accept_company_message(
                "agent-coder",
                "planner",
                "coder",
                "implement the plan",
                "Use the accepted design and verify it.",
                true,
            )
            .unwrap();
        drop(store);

        let reopened = CompanyStore::open(&path).unwrap();
        let pending = reopened.pending_company_messages("agent-coder").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_id, message_id);
        assert!(pending[0].reply_expected);
        reopened
            .inject_company_message("agent-coder", &message_id, "coder")
            .unwrap();
        assert!(reopened
            .pending_company_messages("agent-coder")
            .unwrap()
            .is_empty());
        assert!(reopened
            .inject_company_message("agent-coder", "message_missing", "coder")
            .is_err());
    }

    #[test]
    fn company_message_claim_is_exclusive_across_connections_and_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let first = CompanyStore::open(&path).unwrap();
        let message = first.accept_company_message("claim-session", "planner", "coder", "fixture", "preserve this body", true).unwrap();
        let other = CompanyStore::open(&path).unwrap();
        // The old read-only recovery query exposes the same delivery twice.
        assert_eq!(first.pending_company_messages("claim-session").unwrap().len(), 1);
        assert_eq!(other.pending_company_messages("claim-session").unwrap().len(), 1);
        let claim = first.try_claim_company_message("claim-session", &message, "task-a", "attempt-a").unwrap().unwrap();
        assert!(other.try_claim_company_message("claim-session", &message, "task-b", "attempt-b").unwrap().is_none());
        assert!(other.try_claim_company_message("other-session", &message, "task-b", "attempt-b").unwrap().is_none());
        drop(claim);
        let next = other.try_claim_company_message("claim-session", &message, "task-b", "attempt-b").unwrap().unwrap();
        other.inject_company_message("claim-session", &message, "coder").unwrap();
        drop(next);
        assert!(first.try_claim_company_message("claim-session", &message, "task-c", "attempt-c").unwrap().is_none());
    }

    #[test]
    #[ignore = "child-process fixture, invoked only by the killed-owner test"]
    fn company_message_claim_child() {
        use std::io::{Read, Write};
        let path = std::path::PathBuf::from(std::env::var("PHOENIX_CLAIM_FIXTURE").unwrap());
        let store = CompanyStore::open(path).unwrap();
        let message = store.pending_company_messages("claim-kill-session").unwrap().remove(0);
        let _claim = store.try_claim_company_message("claim-kill-session", &message.message_id, "child-task", "child-attempt").unwrap().unwrap();
        println!("CLAIM_HELD");
        std::io::stdout().flush().unwrap();
        // Keep the claim until the parent kills the fixture process.
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    #[test]
    fn company_message_claim_survives_reopen_and_recovers_after_killed_owner() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        struct FixtureChild(std::process::Child);
        impl Drop for FixtureChild {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let message = store.accept_company_message("claim-kill-session", "planner", "coder", "fixture", "one immutable handoff", true).unwrap();
        let mut child = FixtureChild(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::company::tests::company_message_claim_child", "--ignored", "--nocapture"])
            .env("PHOENIX_CLAIM_FIXTURE", &path)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap());
        let output = child.0.stdout.take().unwrap();
        let mut ready = false;
        for line in std::io::BufReader::new(output).lines() {
            if line.unwrap() == "CLAIM_HELD" { ready = true; break }
        }
        assert!(ready, "child failed before acquiring the fixture claim");
        let reopened = CompanyStore::open(&path).unwrap();
        assert!(reopened.try_claim_company_message("claim-kill-session", &message, "recovery-task", "recovery-attempt").unwrap().is_none());
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        let _recovered = reopened.try_claim_company_message("claim-kill-session", &message, "recovery-task", "recovery-attempt").unwrap().unwrap();
        let pending = reopened.pending_company_messages("claim-kill-session").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_id, message);
        assert_eq!(pending[0].body, "one immutable handoff");
    }

    #[test]
    fn company_handoff_operation_is_exactly_once_and_rejects_substitution() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let accept = |session_id: &str,
                      operation_id: &str,
                      handoff_id: Option<&str>,
                      reply_to: Option<&str>,
                      causation_id: Option<&str>,
                      from: &str,
                      to: &str,
                      subject: &str,
                      body: &str,
                      reply_expected: bool| {
            store.accept_company_message_with_identity(
                session_id,
                operation_id,
                handoff_id,
                reply_to,
                causation_id,
                from,
                to,
                subject,
                body,
                reply_expected,
            )
        };
        let operation = "mesh_turn_parent-a:round-0:call_0";
        let first = accept(
            "agent-coder",
            operation,
            Some("return_exact"),
            Some("handoff_parent"),
            Some("message_parent"),
            "phoenix",
            "coder",
            "implement",
            "Build the accepted slice.",
            true,
        )
        .unwrap();
        assert!(first.created);
        assert!(first.should_route);
        assert_eq!(first.state, "accepted");

        let retry = accept(
            "agent-coder",
            operation,
            Some("return_exact"),
            Some("handoff_parent"),
            Some("message_parent"),
            "phoenix",
            "coder",
            "implement",
            "Build the accepted slice.",
            true,
        )
        .unwrap();
        assert_eq!(retry.message_id, first.message_id);
        assert_eq!(retry.handoff_id, first.handoff_id);
        assert!(!retry.created);
        assert!(!retry.should_route);

        let substitutions = [
            (
                "agent-other",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_other"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_other"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_other"),
                "phoenix",
                "coder",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "researcher",
                "coder",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "tester",
                "implement",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "review",
                "Build the accepted slice.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "implement",
                "Substituted body.",
                true,
            ),
            (
                "agent-coder",
                Some("return_exact"),
                Some("handoff_parent"),
                Some("message_parent"),
                "phoenix",
                "coder",
                "implement",
                "Build the accepted slice.",
                false,
            ),
        ];
        for (
            session_id,
            handoff_id,
            reply_to,
            causation_id,
            from,
            to,
            subject,
            body,
            reply_expected,
        ) in substitutions
        {
            let error = accept(
                session_id,
                operation,
                handoff_id,
                reply_to,
                causation_id,
                from,
                to,
                subject,
                body,
                reply_expected,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("different immutable payload"),
                "unexpected substitution error: {error:#}"
            );
        }

        let connection = store.connection.lock().unwrap();
        let row_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM company_messages WHERE operation_id=?1",
                [operation],
                |row| row.get(0),
            )
            .unwrap();
        let event_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM company_events WHERE idempotency_key=?1",
                [format!("message-accepted:{}", first.message_id)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 1);
        assert_eq!(event_count, 1);
    }

    #[test]
    fn company_handoff_restart_restores_lineage_and_terminal_state_blocks_reroute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let operation = "mesh_turn_restart:round-2:call_0";
        let first = {
            let store = CompanyStore::open(&path).unwrap();
            store
                .accept_company_message_with_identity(
                    "agent-coder",
                    operation,
                    Some("return_restart"),
                    Some("handoff_request"),
                    Some("message_cause"),
                    "phoenix",
                    "coder",
                    "resume",
                    "Continue from the durable boundary.",
                    true,
                )
                .unwrap()
        };

        let reopened = CompanyStore::open(&path).unwrap();
        let pending = reopened.pending_company_messages("agent-coder").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, operation);
        assert_eq!(pending[0].message_id, first.message_id);
        assert_eq!(pending[0].handoff_id, "return_restart");
        assert_eq!(pending[0].reply_to.as_deref(), Some("handoff_request"));
        assert_eq!(pending[0].causation_id.as_deref(), Some("message_cause"));

        reopened
            .inject_company_message("agent-coder", &first.message_id, "coder")
            .unwrap();
        let replay = reopened
            .accept_company_message_with_identity(
                "agent-coder",
                operation,
                Some("return_restart"),
                Some("handoff_request"),
                Some("message_cause"),
                "phoenix",
                "coder",
                "resume",
                "Continue from the durable boundary.",
                true,
            )
            .unwrap();
        assert_eq!(replay.state, "injected");
        assert!(!replay.should_route);
        assert!(reopened
            .pending_company_messages("agent-coder")
            .unwrap()
            .is_empty());

        let suspended = reopened
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_suspended:round-0:call-0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "pause me",
                "This delivery is paused.",
                false,
            )
            .unwrap();
        reopened
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE company_messages SET state='suspended' WHERE message_id=?1",
                [&suspended.message_id],
            )
            .unwrap();
        let suspended_replay = reopened
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_suspended:round-0:call-0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "pause me",
                "This delivery is paused.",
                false,
            )
            .unwrap();
        assert_eq!(suspended_replay.state, "suspended");
        assert!(!suspended_replay.should_route);

        let canceled = reopened
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_canceled:round-0:call_0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "cancel me",
                "This will be quarantined.",
                false,
            )
            .unwrap();
        reopened
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE company_messages SET state='canceled' WHERE message_id=?1",
                [&canceled.message_id],
            )
            .unwrap();
        let error = reopened
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_canceled:round-0:call_0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "cancel me",
                "This will be quarantined.",
                false,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("canceled and cannot be replayed"));
    }

    #[test]
    fn legacy_two_state_company_messages_migrate_with_stable_handoff_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE company_messages(
                        message_id TEXT PRIMARY KEY,
                        run_id TEXT NOT NULL,
                        session_id TEXT NOT NULL,
                        from_agent TEXT NOT NULL,
                        to_agent TEXT NOT NULL,
                        subject TEXT NOT NULL,
                        body TEXT NOT NULL,
                        message_kind TEXT NOT NULL,
                        reply_expected INTEGER NOT NULL DEFAULT 0,
                        state TEXT NOT NULL CHECK(state IN ('accepted','injected')),
                        accepted_at TEXT NOT NULL,
                        injected_at TEXT,
                        as_of_seq INTEGER NOT NULL
                    );
                    INSERT INTO company_messages VALUES(
                        'message_legacy','agent-coder','agent-coder','phoenix','coder',
                        'legacy handoff','Preserve this accepted receipt.','conversation',1,
                        'accepted','{}',NULL,7
                    );", chrono::Utc::now().to_rfc3339()),
                )
                .unwrap();
        }

        let store = CompanyStore::open(&path).unwrap();
        let pending = store.pending_company_messages("agent-coder").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_id, "message_legacy");
        assert_eq!(pending[0].handoff_id, "message_legacy");
        assert!(pending[0].operation_id.is_empty());
        assert!(pending[0].reply_to.is_none());
        assert!(pending[0].causation_id.is_none());
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE company_messages SET state='suspended' WHERE message_id='message_legacy'",
                [],
            )
            .unwrap();
    }

    #[test]
    fn same_provider_call_id_in_distinct_parent_turns_gets_distinct_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let first = store
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_parent-a:round-0:call_0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "implement",
                "Same legitimate payload.",
                true,
            )
            .unwrap();
        let second = store
            .accept_company_message_with_identity(
                "agent-coder",
                "mesh_turn_parent-b:round-0:call_0",
                None,
                None,
                None,
                "phoenix",
                "coder",
                "implement",
                "Same legitimate payload.",
                true,
            )
            .unwrap();
        assert_ne!(first.message_id, second.message_id);
        assert_ne!(first.handoff_id, second.handoff_id);
        assert!(first.should_route && second.should_route);
    }

    #[test]
    fn company_acceptance_rejects_self_handoff_through_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();

        let error = store
            .accept_company_message(
                "agent-coder",
                "coder",
                "Leo",
                "loop",
                "never persist this",
                true,
            )
            .unwrap_err();
        assert!(error.to_string().contains("addressed to itself"));
        assert!(store
            .pending_company_messages("agent-coder")
            .unwrap()
            .is_empty());
        assert!(store.events_since(0, 10).unwrap().is_empty());
    }

    #[test]
    fn generic_message_event_cannot_bypass_self_handoff_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let input = event(
            "agent-coder",
            "self-message-event",
            CompanyEventKind::MessageAccepted {
                message_id: "message_self".into(),
                operation_id: "self-message-event".into(),
                handoff_id: "message_self".into(),
                reply_to: None,
                causation_id: None,
                from: "Leo (coder) #2".into(),
                to: "database".into(),
                subject: "loop".into(),
                body: "never persist this".into(),
                message_kind: MessageKind::Conversation,
                reply_expected: true,
            },
        );

        assert!(store.append(input).is_err());
        assert!(store.events_since(0, 10).unwrap().is_empty());
    }

    #[test]
    fn legacy_pending_self_handoff_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "INSERT INTO company_messages(
                        message_id,run_id,session_id,from_agent,to_agent,subject,body,
                        message_kind,reply_expected,state,accepted_at,as_of_seq)
                     VALUES('message_legacy_self','agent-coder','agent-coder','Leo','coder',
                        'legacy loop','old runtime receipt','conversation',1,'accepted',?1,0)",
                    [Utc::now().to_rfc3339()],
                )
                .unwrap();
        }
        assert_eq!(
            store.pending_company_messages("agent-coder").unwrap().len(),
            1
        );

        store
            .quarantine_self_company_message("agent-coder", "message_legacy_self", "Leo", "coder")
            .unwrap();
        assert!(store
            .pending_company_messages("agent-coder")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn completed_coworker_returns_survive_claim_crash_and_ack_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let session_id = "company-return-restart";
        let persisted = store
            .persist_job_return(
                session_id,
                &crate::runtime::postbox::CompletedJob {
                    kind: crate::runtime::postbox::ReturnKind::Specialist,
                    delivery_id: String::new(),
                    causation_id: Some("message_competitive_scan".to_string()),
                    agent: "researcher".into(),
                    subject: "competitive scan".into(),
                    ok: true,
                    summary: "scan complete".into(),
                    body: "Full evidence that must survive the gateway.".into(),
                    finished: Utc::now(),
                },
            )
            .unwrap();
        assert!(
            store.claim_job_returns(session_id).unwrap().is_empty(),
            "a durable body must remain invisible until postbox publication"
        );
        let inspector = CompanyStore::open(&path).unwrap();
        assert!(store.recover_job_returns().unwrap().is_empty(),
            "same runtime recovery must not publish its in-flight stage");
        assert!(inspector.recover_job_returns().unwrap().is_empty(),
            "another runtime must not publish a live owner's stage");
        store
            .promote_job_return(session_id, &persisted.delivery_id)
            .unwrap();
        let claimed = store.claim_job_returns(session_id).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].body, persisted.body);
        assert!(store.peek_job_returns(session_id).unwrap().is_empty());
        assert!(inspector.recover_job_returns().unwrap().is_empty(),
            "recovery must not steal a live transcript-save claim");
        inspector.release_job_returns(session_id, &[persisted.delivery_id.clone()]).unwrap();
        inspector.acknowledge_job_returns(session_id, &[persisted.delivery_id.clone()]).unwrap();
        assert!(inspector.claim_job_returns(session_id).unwrap().is_empty());
        drop(store);

        let reopened = CompanyStore::open(&path).unwrap();
        assert_eq!(
            reopened.recover_job_returns().unwrap(),
            vec![session_id.to_string()]
        );
        let replay = reopened.claim_job_returns(session_id).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].delivery_id, persisted.delivery_id);
        assert_eq!(
            replay[0].causation_id.as_deref(),
            Some("message_competitive_scan")
        );
        reopened
            .acknowledge_job_returns(session_id, &[persisted.delivery_id])
            .unwrap();
        assert!(reopened.recover_job_returns().unwrap().is_empty());
    }

    #[test]
    #[ignore = "child process used by coworker return recovery acceptance"]
    fn coworker_return_owner_child() {
        use std::io::{Read, Write};
        let store = CompanyStore::open(std::env::var("PHOENIX_RETURN_FIXTURE").unwrap()).unwrap();
        for (session, claimed) in [("stage-child", false), ("claim-child", true)] {
            let row = store.persist_job_return(session, &crate::runtime::postbox::CompletedJob {
                kind: crate::runtime::postbox::ReturnKind::Specialist,
                delivery_id: String::new(), causation_id: None, agent: "researcher".into(),
                subject: "owner death".into(), ok: true, summary: "complete".into(),
                body: format!("Exact evidence for {session}"), finished: Utc::now(),
            }).unwrap();
            if claimed {
                store.promote_job_return(session, &row.delivery_id).unwrap();
                assert_eq!(store.claim_job_returns(session).unwrap().len(), 1);
            }
        }
        println!("RETURNS_HELD");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    #[test]
    fn coworker_returns_recover_after_real_process_death_not_live_reopen() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        struct FixtureChild(std::process::Child);
        impl Drop for FixtureChild {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let recovery = CompanyStore::open(&path).unwrap();
        let mut child = FixtureChild(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::company::tests::coworker_return_owner_child", "--ignored", "--nocapture"])
            .env("PHOENIX_RETURN_FIXTURE", &path)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap());
        let mut ready = false;
        for line in std::io::BufReader::new(child.0.stdout.take().unwrap()).lines() {
            if line.unwrap() == "RETURNS_HELD" { ready = true; break; }
        }
        assert!(ready);
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(recovery.recover_job_returns().unwrap().is_empty());
        for session in ["stage-child", "claim-child"] {
            assert!(recovery.claim_job_returns(session).unwrap().is_empty());
        }
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        assert_eq!(recovery.recover_job_returns().unwrap(), vec!["claim-child", "stage-child"]);
        for session in ["stage-child", "claim-child"] {
            let rows = recovery.claim_job_returns(session).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].body, format!("Exact evidence for {session}"));
            recovery.acknowledge_job_returns(session, &[rows[0].delivery_id.clone()]).unwrap();
        }
        assert!(recovery.recover_job_returns().unwrap().is_empty());
    }

    #[test]
    fn staged_coworker_return_recovers_only_after_owner_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let owner = CompanyStore::open(&path).unwrap();
        let recovery = CompanyStore::open(&path).unwrap();
        let job = owner.persist_job_return("stage-owner", &crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(), causation_id: None, agent: "researcher".into(),
            subject: "stage".into(), ok: true, summary: "done".into(),
            body: "Exact body survives interruption before publication".into(), finished: Utc::now(),
        }).unwrap();
        assert!(recovery.recover_job_returns().unwrap().is_empty());
        drop(owner);
        assert_eq!(recovery.recover_job_returns().unwrap(), vec!["stage-owner"]);
        let rows = recovery.claim_job_returns("stage-owner").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].delivery_id, job.delivery_id);
        assert_eq!(rows[0].body, job.body);
        assert!(recovery.recover_job_returns().unwrap().is_empty());
    }

    #[test]
    fn coworker_return_retry_is_payload_exact_and_precedes_capacity_gate() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let session_id = "company-return-idempotency";
        let job = crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: "return_retry_exact".to_string(),
            causation_id: Some("message_original_request".to_string()),
            agent: "scribe".into(),
            subject: "draft copy".into(),
            ok: true,
            summary: "draft complete".into(),
            body: "Exact persisted draft".into(),
            finished: Utc::now(),
        };
        store.persist_job_return(session_id, &job).unwrap();

        // Fill the undelivered queue to its safety cap. An exact retry is an
        // identity lookup, not a new row, and must remain valid at capacity.
        {
            let connection = store.connection.lock().unwrap();
            for index in 1..MAX_JOB_RETURNS_PER_SESSION {
                connection
                    .execute(
                        "INSERT INTO company_job_returns(
                            delivery_id,session_id,agent,subject,ok,summary,body,finished_at,
                            causation_id,delivery_state,created_at)
                         VALUES(?1,?2,'scribe','other',1,'done','body',?3,NULL,'ready',?3)",
                        params![
                            format!("return_capacity_{index}"),
                            session_id,
                            Utc::now().to_rfc3339(),
                        ],
                    )
                    .unwrap();
            }
        }
        let retried = store.persist_job_return(session_id, &job).unwrap();
        assert_eq!(retried.delivery_id, job.delivery_id);

        let mut substituted = job.clone();
        substituted.body = "different payload under the same id".to_string();
        let error = store
            .persist_job_return(session_id, &substituted)
            .unwrap_err()
            .to_string();
        assert!(error.contains("reused with a different payload"), "{error}");
        store
            .promote_job_return(session_id, &job.delivery_id)
            .unwrap();
        assert_eq!(
            store.claim_job_returns(session_id).unwrap().len() as i64,
            MAX_JOB_RETURNS_PER_SESSION
        );
    }

    #[test]
    fn permanent_transcript_deletion_purges_ready_and_claimed_returns_only_for_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let job = |body: &str| crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(),
            causation_id: None,
            agent: "researcher".into(),
            subject: "scan".into(),
            ok: true,
            summary: "done".into(),
            body: body.into(),
            finished: Utc::now(),
        };
        let ready = store
            .persist_job_return("delete-owner", &job("ready"))
            .unwrap();
        store
            .promote_job_return("delete-owner", &ready.delivery_id)
            .unwrap();
        let claimed = store
            .persist_job_return("delete-owner", &job("claimed"))
            .unwrap();
        store
            .promote_job_return("delete-owner", &claimed.delivery_id)
            .unwrap();
        let keep = store
            .persist_job_return("keep-owner", &job("survives"))
            .unwrap();
        store
            .promote_job_return("keep-owner", &keep.delivery_id)
            .unwrap();
        assert_eq!(store.claim_job_returns("delete-owner").unwrap().len(), 2);

        assert_eq!(store.purge_job_returns("delete-owner").unwrap(), 2);
        assert!(store
            .recover_job_returns()
            .unwrap()
            .contains(&"keep-owner".to_string()));
        assert!(store.claim_job_returns("delete-owner").unwrap().is_empty());
        assert_eq!(store.claim_job_returns("keep-owner").unwrap().len(), 1);
    }

    #[test]
    fn directory_persists_human_coworkers_groups_grants_and_threads() {
        use crate::runtime::company_directory::{
            DirectoryChange, GroupProfile, HistoryAccess, LifecycleState,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        assert_eq!(store.ensure_founding_team(true).unwrap(), 5);
        assert_eq!(store.ensure_founding_team(true).unwrap(), 0);
        let initial = store.directory_snapshot().unwrap();
        assert_eq!(initial.agents.len(), 5);
        assert_eq!(initial.responsibilities.len(), 5);
        assert!(!initial.relationships.is_empty());
        assert!(!initial
            .agents
            .iter()
            .any(|agent| agent.profile.agent_id == "scribe"));
        assert!(!initial
            .agents
            .iter()
            .any(|agent| agent.profile.internal_role == "browser"));

        store
            .apply_directory_change(
                "phoenix",
                "group:product:create",
                DirectoryChange::GroupUpserted {
                    profile: GroupProfile {
                        group_id: "product".into(),
                        name: "Product".into(),
                        description: "The product group".into(),
                        color: "#F26B38".into(),
                        icon_seed: "group-product".into(),
                        lifecycle: LifecycleState::Active,
                        pinned: true,
                        sort_order: 0,
                        canonical_session_id: None,
                        metadata_json: "{}".into(),
                    },
                },
            )
            .unwrap();
        for (agent_id, role) in [("phoenix", "lead"), ("coder", "member")] {
            store
                .apply_directory_change(
                    "phoenix",
                    format!("group:product:member:{agent_id}"),
                    DirectoryChange::GroupMemberSet {
                        group_id: "product".into(),
                        agent_id: agent_id.into(),
                        member_role: role.into(),
                        history_access: HistoryAccess::Full,
                        history_start_message_index: 0,
                        sort_order: 0,
                        present: true,
                    },
                )
                .unwrap();
        }
        store
            .apply_directory_change(
                "phoenix",
                "group:product:grant:researcher",
                DirectoryChange::OutsideCallGrantSet {
                    group_id: "product".into(),
                    agent_id: "researcher".into(),
                    granted: true,
                },
            )
            .unwrap();
        store
            .apply_directory_change(
                "phoenix",
                "thread:product:v1",
                DirectoryChange::ConversationSourceLinked {
                    session_id: "group-product".into(),
                    owner_kind: "group".into(),
                    owner_id: "product".into(),
                    source_kind: "canonical".into(),
                    canonical: true,
                },
            )
            .unwrap();
        assert!(store.outside_call_granted("product", "researcher").unwrap());
        drop(store);

        let reopened = CompanyStore::open(path).unwrap();
        let snapshot = reopened.directory_snapshot().unwrap();
        assert_eq!(snapshot.groups.len(), 1);
        assert_eq!(snapshot.members.len(), 2);
        assert_eq!(snapshot.outside_call_grants.len(), 1);
        assert_eq!(snapshot.outside_call_grants[0].group_id, "product");
        assert_eq!(snapshot.outside_call_grants[0].agent_id, "researcher");
        assert!(snapshot.outside_call_grants[0].granted);
        assert_eq!(
            snapshot.groups[0].profile.canonical_session_id.as_deref(),
            Some("group-product")
        );
        assert!(reopened
            .outside_call_granted("product", "researcher")
            .unwrap());
    }

    #[test]
    fn scratch_company_can_install_the_missing_founding_coworkers_later() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        assert_eq!(store.ensure_founding_team(false).unwrap(), 1);
        let scratch = store.directory_snapshot().unwrap();
        assert_eq!(scratch.agents.len(), 1);
        assert_eq!(scratch.responsibilities.len(), 1);

        assert_eq!(store.ensure_founding_team(true).unwrap(), 4);
        let complete = store.directory_snapshot().unwrap();
        assert_eq!(complete.agents.len(), 5);
        assert_eq!(complete.responsibilities.len(), 5);
        assert!(complete
            .agents
            .iter()
            .any(|agent| agent.profile.display_name == "Theo"));
        assert!(!complete
            .agents
            .iter()
            .any(|agent| ["Maya", "Vera", "Elena", "Owen", "June", "Cleo"].contains(&agent.profile.display_name.as_str())),
            "only the default team is created");
        assert_eq!(store.ensure_founding_team(true).unwrap(), 0);
    }

    #[test]
    fn responsibility_company_migration_preserves_custom_names_and_hides_old_craft_lanes() {
        use crate::runtime::company_directory::{AgentKind, DirectoryChange};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let targets = crate::runtime::company_directory::role_catalog_profiles(true);

        let mut coder = targets
            .iter()
            .find(|profile| profile.agent_id == "coder")
            .unwrap()
            .clone();
        coder.display_name = "Nico".into();
        coder.role_title = "Software & Automation".into();
        store
            .apply_directory_change(
                "legacy",
                "seed-old-coder",
                DirectoryChange::AgentUpserted { profile: coder },
            )
            .unwrap();

        // A user's own (non-founding) inbox coworker must survive migration.
        let mut custom_scribe = targets
            .iter()
            .find(|profile| profile.agent_id == "finance")
            .unwrap()
            .clone();
        custom_scribe.agent_id = "scribe".into();
        custom_scribe.internal_role = "scribe".into();
        custom_scribe.browser_profile_id = "agent-scribe".into();
        custom_scribe.canonical_session_id = Some("agent-scribe".into());
        custom_scribe.display_name = "My Inbox".into();
        custom_scribe.role_title = "Inbox & Communications".into();
        store
            .apply_directory_change(
                "user",
                "seed-custom-scribe",
                DirectoryChange::AgentUpserted {
                    profile: custom_scribe,
                },
            )
            .unwrap();

        let mut v3_finance = targets
            .iter()
            .find(|profile| profile.agent_id == "finance")
            .unwrap()
            .clone();
        v3_finance.display_name = "Theo".into();
        v3_finance.role_title = "Money & Administration".into();
        v3_finance.canonical_session_id = Some("agent-finance-history".into());
        v3_finance.browser_profile_id = "agent-finance-existing".into();
        v3_finance.metadata_json = r#"{"preserve":"me"}"#.into();
        store
            .apply_directory_change(
                "legacy",
                "seed-v3-finance",
                DirectoryChange::AgentUpserted {
                    profile: v3_finance,
                },
            )
            .unwrap();

        let mut database = targets
            .iter()
            .find(|profile| profile.agent_id == "coder")
            .unwrap()
            .clone();
        database.agent_id = "database".into();
        database.internal_role = "database".into();
        database.display_name = "Ada".into();
        database.role_title = "Data & Systems".into();
        database.browser_profile_id = "agent-database".into();
        store
            .apply_directory_change(
                "legacy",
                "seed-old-database",
                DirectoryChange::AgentUpserted { profile: database },
            )
            .unwrap();

        let mut cleo = targets
            .iter()
            .find(|profile| profile.agent_id == "personal_logistics")
            .unwrap()
            .clone();
        cleo.role_title = "Operations".into();
        cleo.description = "Owns practical operations.".into();
        store
            .apply_directory_change(
                "legacy",
                "seed-old-cleo",
                DirectoryChange::AgentUpserted { profile: cleo },
            )
            .unwrap();
        store
            .apply_directory_change(
                "legacy",
                "seed-old-operations-charter",
                DirectoryChange::ResponsibilityUpserted {
                    responsibility_id: "responsibility_personal_logistics".into(),
                    agent_id: "personal_logistics".into(),
                    title: "Operations".into(),
                    scope: "Own practical operations, recurring procedures, travel, appointments, reservations, forms, errands, deliveries, reminders, and real-life commitments through verified completion.".into(),
                    success_criteria: vec![
                        "Dates, timezone, location, identity scope, and constraints are explicit".into(),
                        "Paid or binding actions carry the required approval".into(),
                        "Confirmation, cost, and cancellation evidence are preserved".into(),
                    ],
                    approval_policy_json: r#"{"inherits_company_policy":true}"#.into(),
                    escalation_policy_json: r#"{"ambiguous_owner":"phoenix","blocked":"ask_user"}"#.into(),
                    status: "active".into(),
                },
            )
            .unwrap();

        store.ensure_full_catalog_team().unwrap();
        let snapshot = store.directory_snapshot().unwrap();
        let coder = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "coder")
            .unwrap();
        assert_eq!(coder.profile.display_name, "Leo");
        assert_eq!(coder.profile.role_title, "Engineering");
        let scribe = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "scribe")
            .unwrap();
        assert_eq!(scribe.profile.display_name, "My Inbox");
        let finance = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "finance")
            .unwrap();
        assert_eq!(finance.profile.display_name, "Vera");
        assert_eq!(finance.profile.role_title, "Finance & Purchasing");
        assert_eq!(
            finance.profile.canonical_session_id.as_deref(),
            Some("agent-finance-history")
        );
        assert_eq!(finance.profile.browser_profile_id, "agent-finance-existing");
        assert_eq!(finance.profile.metadata_json, r#"{"preserve":"me"}"#);
        let database = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "database")
            .unwrap();
        assert_eq!(database.profile.kind, AgentKind::CraftSpecialist);
        assert_eq!(
            database.profile.lifecycle,
            crate::runtime::company_directory::LifecycleState::Archived
        );
        assert!(database.profile.metadata_json.contains("hidden_expertise"));
    }

    #[test]
    fn exact_v6_personal_logistics_defaults_roll_back_monotonically() {
        use crate::runtime::company_directory::DirectoryChange;

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let mut profile = v6_personal_logistics_profile();
        profile.canonical_session_id = Some("agent-cleo-existing".into());
        profile.browser_profile_id = "agent-cleo-browser-existing".into();
        profile.metadata_json = r#"{"preserve":"v6-install"}"#.into();
        store
            .apply_directory_change(
                "migration-seed",
                "seed-v6-personal-logistics-profile",
                DirectoryChange::AgentUpserted { profile },
            )
            .unwrap();
        store
            .apply_directory_change(
                "migration-seed",
                "seed-v6-personal-logistics-responsibility",
                v6_personal_logistics_responsibility(),
            )
            .unwrap();

        store.ensure_full_catalog_team().unwrap();
        let migrated = store.directory_snapshot().unwrap();
        let cleo = migrated
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "personal_logistics")
            .unwrap();
        assert_eq!(cleo.profile.role_title, "Operations");
        assert_eq!(
            cleo.profile.description,
            "Owns practical operations, recurring procedures, travel, appointments, reservations, forms, errands, deliveries, reminders, and real-life commitments."
        );
        assert_eq!(
            cleo.profile.canonical_session_id.as_deref(),
            Some("agent-cleo-existing")
        );
        assert_eq!(
            cleo.profile.browser_profile_id,
            "agent-cleo-browser-existing"
        );
        assert_eq!(cleo.profile.metadata_json, r#"{"preserve":"v6-install"}"#);
        let charter = migrated
            .responsibilities
            .iter()
            .find(|responsibility| {
                responsibility.responsibility_id == "responsibility_personal_logistics"
            })
            .unwrap();
        assert_eq!(charter.title, "Operations");
        assert!(charter.scope.starts_with("Own practical operations"));

        let v7_events = store
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event
                    .envelope
                    .idempotency_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("founding-company-v7:"))
            })
            .count();
        assert_eq!(v7_events, 2);
        store.ensure_full_catalog_team().unwrap();
        let after_rerun = store
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event
                    .envelope
                    .idempotency_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("founding-company-v7:"))
            })
            .count();
        assert_eq!(after_rerun, v7_events);
    }

    #[test]
    fn v7_rollback_preserves_any_user_edited_v6_payload() {
        use crate::runtime::company_directory::DirectoryChange;

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let mut custom_profile = v6_personal_logistics_profile();
        custom_profile.description = "My custom responsibility description".into();
        store
            .apply_directory_change(
                "user",
                "seed-user-edited-v6-profile",
                DirectoryChange::AgentUpserted {
                    profile: custom_profile,
                },
            )
            .unwrap();
        let mut custom_responsibility = v6_personal_logistics_responsibility();
        let DirectoryChange::ResponsibilityUpserted { ref mut scope, .. } = custom_responsibility
        else {
            unreachable!()
        };
        *scope = "My custom operating charter".into();
        store
            .apply_directory_change(
                "user",
                "seed-user-edited-v6-responsibility",
                custom_responsibility,
            )
            .unwrap();

        store.ensure_full_catalog_team().unwrap();
        let snapshot = store.directory_snapshot().unwrap();
        let cleo = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "personal_logistics")
            .unwrap();
        assert_eq!(cleo.profile.role_title, V6_PERSONAL_LOGISTICS_TITLE);
        assert_eq!(
            cleo.profile.description,
            "My custom responsibility description"
        );
        let charter = snapshot
            .responsibilities
            .iter()
            .find(|responsibility| {
                responsibility.responsibility_id == "responsibility_personal_logistics"
            })
            .unwrap();
        assert_eq!(charter.title, V6_PERSONAL_LOGISTICS_TITLE);
        assert_eq!(charter.scope, "My custom operating charter");
        assert!(!store.events_since(0, 10_000).unwrap().iter().any(|event| {
            event
                .envelope
                .idempotency_key
                .as_deref()
                .is_some_and(|key| key.starts_with("founding-company-v7:"))
        }));
    }

    #[test]
    fn directory_profile_pin_order_and_group_membership_api_is_atomic() {
        use crate::runtime::company_directory::{GroupProfile, HistoryAccess, LifecycleState};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .create_group(
                "user",
                GroupProfile {
                    group_id: "launch".into(),
                    name: "Launch".into(),
                    description: "Launch group".into(),
                    color: "#E06C52".into(),
                    icon_seed: "launch".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 50,
                    canonical_session_id: None,
                    metadata_json: "{}".into(),
                },
                vec!["planner".into(), "coder".into()],
            )
            .unwrap();
        let updated = store
            .update_agent_profile(
                "user",
                "coder",
                Some("Nick".into()),
                None,
                Some("Owns engineering delivery".into()),
                Some("#123456".into()),
                None,
            )
            .unwrap();
        assert_eq!(updated.profile.display_name, "Nick");
        assert_eq!(updated.profile.internal_role, "coder");
        assert_eq!(
            updated.profile.canonical_session_id.as_deref(),
            Some("agent-coder")
        );
        store
            .set_sidebar_item_pinned("user", &SidebarItemKey::Group("launch".into()), true)
            .unwrap();
        store
            .reorder_sidebar_items(
                "user",
                vec![
                    SidebarItemKey::Group("launch".into()),
                    SidebarItemKey::Agent("coder".into()),
                    SidebarItemKey::Agent("planner".into()),
                ],
            )
            .unwrap();
        store
            .set_group_members(
                "user",
                "launch",
                vec![
                    GroupMemberInput {
                        agent_id: "frontend".into(),
                        member_role: "design lead".into(),
                        history_access: HistoryAccess::Full,
                    },
                    GroupMemberInput {
                        agent_id: "coder".into(),
                        member_role: "engineering lead".into(),
                        history_access: HistoryAccess::FromJoin,
                    },
                ],
            )
            .unwrap();
        let snapshot = store.directory_snapshot().unwrap();
        let launch = snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == "launch")
            .unwrap();
        assert!(launch.profile.pinned);
        assert_eq!(launch.profile.sort_order, 0);
        let members = snapshot
            .members
            .iter()
            .filter(|member| member.group_id == "launch")
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].agent_id, "frontend");
        assert_eq!(members[0].sort_order, 0);
        assert_eq!(members[1].agent_id, "coder");
        assert_eq!(members[1].history_access, HistoryAccess::FromJoin);
        assert!(!members.iter().any(|member| member.agent_id == "planner"));

        let before = store.events_since(0, 10_000).unwrap().len();
        assert!(store
            .set_group_members(
                "user",
                "launch",
                vec![GroupMemberInput {
                    agent_id: "missing".into(),
                    member_role: "member".into(),
                    history_access: HistoryAccess::Full,
                }],
            )
            .is_err());
        assert_eq!(store.events_since(0, 10_000).unwrap().len(), before);
    }

    #[test]
    fn provisioning_cannot_resurrect_an_archived_or_deleting_coworker() {
        use crate::runtime::company_directory::{AgentKind, AgentProfile, LifecycleState};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store
            .register_provisioning_agent(
                "phoenix",
                AgentProfile {
                    agent_id: "custom_ops".into(),
                    internal_role: "custom_ops".into(),
                    display_name: "Riley".into(),
                    role_title: "Operations".into(),
                    description: "Owns a custom operation".into(),
                    color: "#E06C52".into(),
                    icon_seed: "phoenix-flame-custom_ops".into(),
                    kind: AgentKind::ResponsibilityOwner,
                    lifecycle: LifecycleState::Dormant,
                    pinned: false,
                    sort_order: 1,
                    canonical_session_id: None,
                    browser_profile_id: "agent-custom_ops".into(),
                    metadata_json: serde_json::json!({
                        "source": "custom_agent_pipeline"
                    })
                    .to_string(),
                },
                "Operations".into(),
                "Owns a custom operation".into(),
            )
            .unwrap();
        store
            .set_agent_lifecycle("user", "custom_ops", LifecycleState::Archived)
            .unwrap();
        assert!(store
            .set_provisioned_agent_ready("custom_ops", true)
            .is_err());
        assert_eq!(
            store
                .directory_snapshot()
                .unwrap()
                .agents
                .into_iter()
                .find(|agent| agent.profile.agent_id == "custom_ops")
                .unwrap()
                .profile
                .lifecycle,
            LifecycleState::Archived
        );

        store
            .set_agent_lifecycle("user", "custom_ops", LifecycleState::Active)
            .unwrap();
        store
            .schedule_agent_deletion("user", "custom_ops", "Riley")
            .unwrap();
        assert!(store
            .set_provisioned_agent_ready("custom_ops", true)
            .is_err());
    }

    #[test]
    fn from_join_group_history_records_the_exact_canonical_message_boundary() {
        use crate::runtime::company_directory::{GroupProfile, HistoryAccess, LifecycleState};
        use crate::session::Message;

        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let store = CompanyStore::open(home.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .create_group(
                "user",
                GroupProfile {
                    group_id: "history".into(),
                    name: "History".into(),
                    description: "History boundary test".into(),
                    color: "#123456".into(),
                    icon_seed: "history".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 1,
                    canonical_session_id: None,
                    metadata_json: "{}".into(),
                },
                vec!["planner".into(), "researcher".into()],
            )
            .unwrap();
        let mut sessions = crate::session::SessionStore::with_default_root().unwrap();
        let mut session = sessions
            .load_or_create_main("group-history", "model", "prompt")
            .unwrap();
        session.push_message(Message::User {
            content: "before one".into(),
        });
        session.push_message(Message::Assistant {
            content: "before two".into(),
        });
        session.push_message(Message::User {
            content: "before three".into(),
        });
        sessions.upsert(session);
        sessions.save_one("group-history").unwrap();

        store
            .set_group_members(
                "user",
                "history",
                vec![
                    GroupMemberInput {
                        agent_id: "planner".into(),
                        member_role: "existing".into(),
                        history_access: HistoryAccess::Full,
                    },
                    GroupMemberInput {
                        agent_id: "coder".into(),
                        member_role: "new".into(),
                        history_access: HistoryAccess::FromJoin,
                    },
                ],
            )
            .unwrap();
        let snapshot = store.directory_snapshot().unwrap();
        let coder = snapshot
            .members
            .iter()
            .find(|member| member.group_id == "history" && member.agent_id == "coder")
            .unwrap();
        assert_eq!(coder.history_start_message_index, 3);
        let context = crate::runtime::group_conversation::resolve_group_turn(
            &snapshot, "history", "continue",
        )
        .unwrap();
        assert_eq!(
            context
                .participants
                .iter()
                .find(|participant| participant.internal_role == "coder")
                .unwrap()
                .history_start_message_index,
            3
        );
    }

    #[test]
    fn deletion_requires_the_exact_current_name_and_has_a_30_day_recovery_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        assert!(store
            .schedule_agent_deletion("phoenix", "finance", "bart")
            .is_err());
        let before = Utc::now();
        store
            .schedule_agent_deletion("phoenix", "finance", "Vera")
            .unwrap();
        let snapshot = store.directory_snapshot().unwrap();
        let felix = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "finance")
            .unwrap();
        assert_eq!(
            felix.profile.lifecycle,
            crate::runtime::company_directory::LifecycleState::PendingDeletion
        );
        let deadline = DateTime::parse_from_rfc3339(felix.delete_after.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let expected = before + chrono::Duration::days(30);
        assert!(deadline >= expected);
        assert!(deadline <= Utc::now() + chrono::Duration::days(30));
        assert!(store.purge_directory_items_due().unwrap().is_empty());

        store
            .set_agent_lifecycle(
                "phoenix",
                "finance",
                crate::runtime::company_directory::LifecycleState::Active,
            )
            .unwrap();
        let restored = store.directory_snapshot().unwrap();
        let felix = restored
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "finance")
            .unwrap();
        assert_eq!(
            felix.profile.lifecycle,
            crate::runtime::company_directory::LifecycleState::Active
        );
        assert!(felix.delete_after.is_none());
    }

    #[test]
    fn expired_agent_purge_removes_only_exact_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .schedule_agent_deletion("user", "finance", "Vera")
            .unwrap();
        store
            .mark_conversation_read(&SidebarItemKey::Agent("finance".to_string()), 7)
            .unwrap();

        let private_targets = [
            dir.path().join("agents/finance/agent.toml"),
            dir.path().join("sessions/agent-finance.json"),
            dir.path()
                .join("browser/profiles/agent-finance/Default/Cookies"),
            dir.path().join("memory/agents/finance/private.json"),
        ];
        for path in &private_targets {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"private").unwrap();
        }
        let shared = dir.path().join("memory/company/shared.json");
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        std::fs::write(&shared, b"shared").unwrap();
        let onboarding_path = dir.path().join("onboarding/state.json");
        let onboarding_state = crate::onboarding::OnboardingState {
            version: 1,
            company_choice: Some(crate::onboarding::CompanyChoice::FoundingCompany),
            default_account_email: Some("owner@example.com".to_string()),
            cookie_import: Some(crate::onboarding::CookieImportChoice::AllPortable {
                source: "firefox".to_string(),
                agent_ids: vec!["finance".to_string(), "phoenix".to_string()],
                company_wide: false,
                portable_cookie_count: 42,
                device_bound_cookie_count: 3,
                granted_at: "2026-08-14T00:00:00Z".to_string(),
            }),
            provider_verification: None,
            account_email_skipped: false,
            company_defaults_reviewed_at: None,
            powers_setup_reviewed_at: None,
            created_at: "2026-08-14T00:00:00Z".to_string(),
            updated_at: "2026-08-14T00:00:00Z".to_string(),
            completed_at: None,
        };
        crate::config::private_io::atomic_write_private(
            &onboarding_path,
            &serde_json::to_vec_pretty(&onboarding_state).unwrap(),
        )
        .unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE company_agents SET delete_after=?1 WHERE agent_id='finance'",
                    [(Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()],
                )
                .unwrap();
        }

        assert_eq!(
            store.purge_directory_items_due().unwrap(),
            vec![("agent".to_string(), "finance".to_string())]
        );
        assert!(private_targets.iter().all(|path| !path.exists()));
        assert_eq!(std::fs::read_to_string(shared).unwrap(), "shared");
        let onboarding_state: crate::onboarding::OnboardingState =
            serde_json::from_slice(&std::fs::read(onboarding_path).unwrap()).unwrap();
        let crate::onboarding::CookieImportChoice::AllPortable { agent_ids, .. } =
            onboarding_state.cookie_import.unwrap()
        else {
            panic!("shared Phoenix cookie recipient should remain");
        };
        assert_eq!(agent_ids, vec!["phoenix"]);
        assert!(!store
            .directory_snapshot()
            .unwrap()
            .agents
            .iter()
            .any(|agent| agent.profile.agent_id == "finance"));
        assert!(!store
            .conversation_read_markers()
            .unwrap()
            .iter()
            .any(|marker| marker.item == SidebarItemKey::Agent("finance".to_string())));
    }

    #[cfg(unix)]
    #[test]
    fn private_purge_refuses_symlinked_namespace_ancestors() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("company")).unwrap();
        let store = CompanyStore::open(root.path().join("company/company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .schedule_agent_deletion("user", "finance", "Vera")
            .unwrap();
        std::fs::write(outside.path().join("sentinel"), b"keep").unwrap();
        symlink(outside.path(), root.path().join("agents")).unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE company_agents SET delete_after=?1 WHERE agent_id='finance'",
                    [(Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()],
                )
                .unwrap();
        }

        assert!(store.purge_directory_items_due().is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel")).unwrap(),
            "keep"
        );
        assert!(store
            .directory_snapshot()
            .unwrap()
            .agents
            .iter()
            .any(|agent| agent.profile.agent_id == "finance"));
    }

    #[test]
    fn lifecycle_pauses_handoffs_and_group_and_agent_restores_compose_safely() {
        use crate::runtime::company_directory::{GroupProfile, LifecycleState};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .create_group(
                "phoenix",
                GroupProfile {
                    group_id: "ops".into(),
                    name: "Ops".into(),
                    description: "Operations group".into(),
                    color: "#334455".into(),
                    icon_seed: "group-ops".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 0,
                    canonical_session_id: None,
                    metadata_json: "{}".into(),
                },
                vec!["finance".into(), "planner".into()],
            )
            .unwrap();
        let direct = store
            .accept_company_message(
                "agent-finance",
                "planner",
                "finance",
                "direct",
                "reconcile the ledger",
                true,
            )
            .unwrap();
        let grouped = store
            .accept_company_message(
                "group-ops",
                "planner",
                "finance",
                "group",
                "review group expenses",
                true,
            )
            .unwrap();
        assert_eq!(
            store
                .pending_company_messages("agent-finance")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.pending_company_messages("group-ops").unwrap().len(),
            1
        );

        store
            .set_agent_lifecycle("user", "finance", LifecycleState::Archived)
            .unwrap();
        assert!(store
            .pending_company_messages("agent-finance")
            .unwrap()
            .is_empty());
        assert!(store
            .pending_company_messages("group-ops")
            .unwrap()
            .is_empty());
        store
            .set_group_lifecycle("user", "ops", LifecycleState::Archived)
            .unwrap();
        store
            .set_agent_lifecycle("user", "finance", LifecycleState::Active)
            .unwrap();
        assert_eq!(
            store.pending_company_messages("agent-finance").unwrap()[0].message_id,
            direct
        );
        assert!(
            store
                .pending_company_messages("group-ops")
                .unwrap()
                .is_empty(),
            "restoring the agent must not override the archived group"
        );
        store
            .set_group_lifecycle("user", "ops", LifecycleState::Active)
            .unwrap();
        assert_eq!(
            store.pending_company_messages("group-ops").unwrap()[0].message_id,
            grouped
        );
        assert!(store
            .set_agent_lifecycle("user", "phoenix", LifecycleState::Archived)
            .is_err());
        assert!(store
            .schedule_agent_deletion("user", "phoenix", "Phoenix")
            .is_err());
    }

    #[test]
    fn archiving_a_worker_requeues_its_lease_and_pauses_the_workflow() {
        use crate::runtime::company_directory::LifecycleState;
        use crate::runtime::workflow::{
            ConcurrencyPolicy, DurableWorkflowContract, DurableWorkflowScheduler, GoalRequest,
            NodeSpec, RunRequest, WorkflowBudget, WorkflowOwnership, WorkflowPhase, WorkflowScope,
        };

        let dir = tempfile::tempdir().unwrap();
        let store =
            std::sync::Arc::new(CompanyStore::open(dir.path().join("company.sqlite")).unwrap());
        store.ensure_full_catalog_team().unwrap();
        let scheduler =
            DurableWorkflowScheduler::new(store.clone(), ConcurrencyPolicy::default()).unwrap();
        let goal_id = scheduler
            .create_goal(GoalRequest {
                goal_id: Some("goal_lifecycle".into()),
                idempotency_key: Some("goal-lifecycle".into()),
                contract: DurableWorkflowContract {
                    title: "Lifecycle workflow".into(),
                    objective: "Prove leased work pauses safely".into(),
                    budget: WorkflowBudget::default(),
                    evidence_requirements: vec![],
                    concurrency: ConcurrencyPolicy::default(),
                    metadata: Default::default(),
                    ownership: WorkflowOwnership {
                        scope: WorkflowScope::Agent,
                        owner_agent_id: "finance".into(),
                        group_id: None,
                    },
                },
            })
            .unwrap();
        let run_id = scheduler
            .open_run(RunRequest {
                run_id: Some("run_lifecycle".into()),
                idempotency_key: Some("run-lifecycle".into()),
                goal_id,
                budget: WorkflowBudget::default(),
                next_wake_at: None,
            })
            .unwrap();
        scheduler
            .define_node(NodeSpec {
                node_id: Some("node_lifecycle".into()),
                owner_agent_id: None,
                idempotency_key: Some("node-lifecycle".into()),
                run_id: run_id.clone(),
                parent_id: None,
                title: "Reconcile".into(),
                outcome: "Ledger is reconciled".into(),
                phase: WorkflowPhase::Execute,
                dependencies: vec![],
                budget: WorkflowBudget::default(),
                evidence_requirements: vec![],
            })
            .unwrap();
        scheduler.reconcile(&run_id, Utc::now()).unwrap();
        let (_, leased) = scheduler.tick(&run_id, "finance", Utc::now()).unwrap();
        assert_eq!(leased.len(), 1);

        store
            .set_agent_lifecycle("user", "finance", LifecycleState::Archived)
            .unwrap();
        let snapshot = scheduler.projection(Some(&run_id)).unwrap();
        assert_eq!(snapshot.runs[0].state, "paused");
        assert_eq!(snapshot.nodes[0].state, "ready");
        assert!(snapshot.nodes[0].lease_id.is_none());
        let (_, leased) = scheduler.tick(&run_id, "planner", Utc::now()).unwrap();
        assert!(leased.is_empty(), "paused work never silently reroutes");
        assert_eq!(
            scheduler
                .resume_or_reroute_run(
                    &run_id,
                    "phoenix",
                    Some("planner"),
                    "user approved rerouting to planning",
                    "reroute-to-planner",
                )
                .unwrap(),
            "planner"
        );
        let resumed = scheduler.projection(Some(&run_id)).unwrap();
        assert_eq!(resumed.runs[0].state, "active");
        assert_eq!(resumed.runs[0].scope, "agent");
        assert_eq!(resumed.runs[0].owner_agent_id, "planner");
        let (_, leased) = scheduler.tick(&run_id, "planner", Utc::now()).unwrap();
        assert_eq!(
            leased.len(),
            1,
            "explicit reroute makes work claimable again"
        );
    }

    #[test]
    fn group_create_and_membership_edits_require_two_to_six_active_coworkers() {
        use crate::runtime::company_directory::{GroupProfile, HistoryAccess, LifecycleState};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        let active_ids = store
            .directory_snapshot()
            .unwrap()
            .agents
            .into_iter()
            .filter(|agent| agent.profile.lifecycle == LifecycleState::Active)
            .map(|agent| agent.profile.agent_id)
            .collect::<Vec<_>>();
        assert!(active_ids.len() >= 7);
        let profile = |group_id: &str, name: &str| GroupProfile {
            group_id: group_id.into(),
            name: name.into(),
            description: "Roster contract test".into(),
            color: "#334455".into(),
            icon_seed: group_id.into(),
            lifecycle: LifecycleState::Active,
            pinned: false,
            sort_order: 0,
            canonical_session_id: None,
            metadata_json: "{}".into(),
        };

        assert!(store
            .create_group(
                "user",
                profile("too-small", "Too small"),
                active_ids[..1].to_vec(),
            )
            .is_err());
        assert!(store
            .create_group(
                "user",
                profile("too-large", "Too large"),
                active_ids[..7].to_vec(),
            )
            .is_err());
        assert!(store.directory_snapshot().unwrap().groups.is_empty());

        let dormant_id = active_ids
            .iter()
            .find(|agent_id| agent_id.as_str() != "phoenix")
            .unwrap()
            .clone();
        let partner_id = active_ids
            .iter()
            .find(|agent_id| agent_id.as_str() != dormant_id.as_str())
            .unwrap()
            .clone();
        store
            .set_agent_lifecycle("user", &dormant_id, LifecycleState::Dormant)
            .unwrap();
        assert!(store
            .create_group(
                "user",
                profile("dormant-member", "Dormant member"),
                vec![dormant_id.clone(), partner_id],
            )
            .is_err());
        assert!(store.directory_snapshot().unwrap().groups.is_empty());
        store
            .set_agent_lifecycle("user", &dormant_id, LifecycleState::Active)
            .unwrap();

        let expected_name =
            generated_group_name(&store.directory_snapshot().unwrap(), &active_ids[..6]).unwrap();
        let created = store
            .create_group("user", profile("six", "   "), active_ids[..6].to_vec())
            .unwrap();
        assert_eq!(created.profile.name, expected_name);

        let as_inputs = |ids: &[String]| {
            ids.iter()
                .map(|agent_id| GroupMemberInput {
                    agent_id: agent_id.clone(),
                    member_role: "member".into(),
                    history_access: HistoryAccess::Full,
                })
                .collect::<Vec<_>>()
        };
        assert!(store
            .set_group_members("user", "six", as_inputs(&active_ids[..1]))
            .is_err());
        assert!(store
            .set_group_members("user", "six", as_inputs(&active_ids[..7]))
            .is_err());
        store
            .set_agent_lifecycle("user", &active_ids[6], LifecycleState::Dormant)
            .unwrap();
        assert!(store
            .set_group_members(
                "user",
                "six",
                as_inputs(&[active_ids[0].clone(), active_ids[6].clone()]),
            )
            .is_err());
        store
            .set_agent_lifecycle("user", &active_ids[6], LifecycleState::Active)
            .unwrap();
        assert_eq!(
            store
                .directory_snapshot()
                .unwrap()
                .members
                .iter()
                .filter(|member| member.group_id == "six")
                .count(),
            6
        );
    }

    #[test]
    fn group_creation_persists_manual_order_and_an_endless_thread() {
        use crate::runtime::company_directory::{GroupProfile, LifecycleState};

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        let group = store
            .create_group(
                "phoenix",
                GroupProfile {
                    group_id: "release".into(),
                    name: "Release".into(),
                    description: "Ship the release".into(),
                    color: "#334455".into(),
                    icon_seed: "group-release".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: true,
                    sort_order: 0,
                    canonical_session_id: None,
                    metadata_json: "{}".into(),
                },
                vec!["critic".into(), "coder".into(), "frontend".into()],
            )
            .unwrap();
        assert_eq!(
            group.profile.canonical_session_id.as_deref(),
            Some("group-release")
        );
        assert_eq!(
            store.ensure_group_canonical_session("release").unwrap(),
            "group-release"
        );
        let snapshot = store.directory_snapshot().unwrap();
        let members = snapshot
            .members
            .iter()
            .filter(|member| member.group_id == "release")
            .map(|member| member.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(members, vec!["critic", "coder", "frontend"]);
    }

    #[test]
    fn fresh_coworker_context_keeps_old_thread_as_history_and_protects_phoenix() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        let before = store
            .directory_snapshot()
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == "coder")
            .unwrap()
            .profile
            .canonical_session_id
            .unwrap();

        let (previous, fresh) = store
            .rotate_agent_canonical_session("user", "coder")
            .unwrap();
        assert_eq!(previous.as_deref(), Some(before.as_str()));
        assert_ne!(fresh, before);
        let snapshot = store.directory_snapshot().unwrap();
        let coder = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == "coder")
            .unwrap();
        assert_eq!(
            coder.profile.canonical_session_id.as_deref(),
            Some(fresh.as_str())
        );
        assert!(snapshot.conversation_sources.iter().any(|source| {
            source.session_id == before && source.owner_id == "coder" && !source.canonical
        }));
        assert!(snapshot.conversation_sources.iter().any(|source| {
            source.session_id == fresh
                && source.owner_id == "coder"
                && source.canonical
                && source.source_kind == "fresh_start"
        }));
        assert!(store
            .rotate_agent_canonical_session("user", "phoenix")
            .is_err());
    }

    #[test]
    fn startup_history_reconciliation_preserves_an_established_canonical_thread() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_root = dir.path().join("sessions");
        let mut sessions = crate::session::SessionStore::new(&sessions_root);
        sessions.upsert(crate::session::Session::new_main_with_id(
            "main-current",
            "model",
            "prompt",
        ));
        sessions.save_one("main-current").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sessions.upsert(crate::session::Session::new_main_with_id(
            "main-old-but-newer-mtime",
            "model",
            "prompt",
        ));
        sessions.save_one("main-old-but-newer-mtime").unwrap();

        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store
            .apply_directory_change(
                "user",
                "select-main-current",
                super::super::company_directory::DirectoryChange::ConversationSourceLinked {
                    session_id: "main-current".into(),
                    owner_kind: "agent".into(),
                    owner_id: "phoenix".into(),
                    source_kind: "production_canonical".into(),
                    canonical: true,
                },
            )
            .unwrap();

        store.create_group("phoenix", super::super::company_directory::GroupProfile {
            group_id: "history-room".into(), name: "History room".into(), description: "fixture".into(),
            color: "#334455".into(), icon_seed: "history".into(),
            lifecycle: super::super::company_directory::LifecycleState::Active,
            pinned: false, sort_order: 0, canonical_session_id: Some("room-history".into()), metadata_json: "{}".into(),
        }, vec!["phoenix".into(), "coder".into()]).unwrap();
        for id in ["room-history", "room-history__phoenix", "room-history__coder--assignment"] {
            sessions.upsert(crate::session::Session::new_main_with_id(id, "model", "room-private"));
            sessions.save_one(id).unwrap();
        }
        let room_bytes = std::fs::read(sessions_root.join("room-history.json")).unwrap();
        store.reconcile_session_history(&sessions_root).unwrap();
        store.reconcile_session_history(&sessions_root).unwrap();
        assert_eq!(std::fs::read(sessions_root.join("room-history.json")).unwrap(), room_bytes);
        assert!(!store.directory_snapshot().unwrap().conversation_sources.iter().any(|source|
            source.owner_kind == "agent" && source.session_id.starts_with("room-history")));
        let phoenix = store
            .directory_snapshot()
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == "phoenix")
            .unwrap();
        assert_eq!(
            phoenix.profile.canonical_session_id.as_deref(),
            Some("main-current")
        );
    }

    #[test]
    fn authoritative_job_projection_settles_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let job_id = store
            .start_job("run-a", "session-a", "browser", "review deck")
            .unwrap();
        assert!(job_id.starts_with("job_"));
        assert_eq!(store.snapshot(Some("session-a")).unwrap().active_jobs, 1);
        let settled = store
            .settle_matching_job("session-a", "browser", "review deck", true, "done")
            .unwrap();
        assert_eq!(settled.as_deref(), Some(job_id.as_str()));
        assert_eq!(store.snapshot(Some("session-a")).unwrap().active_jobs, 0);
        assert!(store
            .settle_matching_job("session-a", "browser", "review deck", true, "again")
            .unwrap()
            .is_none());
    }

    #[test]
    fn opening_an_observer_preserves_jobs_until_explicit_gateway_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let owner = CompanyStore::open(&path).unwrap();
        owner.start_job("run-live", "session-live", "coder", "live work").unwrap();
        owner.start_job("run-done", "session-done", "coder", "finished work").unwrap();
        owner.settle_matching_job("session-done", "coder", "finished work", true, "saved result").unwrap();
        let observer = CompanyStore::open(&path).unwrap();
        assert_eq!(observer.snapshot(Some("session-live")).unwrap().active_jobs, 1);
        assert_eq!(observer.snapshot(Some("session-live")).unwrap().stale_jobs, 0);
        let done = observer.snapshot(Some("session-done")).unwrap().jobs.remove(0);
        drop(owner);
        // A DB open is still not the gateway restart boundary.
        let restarted = CompanyStore::open(&path).unwrap();
        assert_eq!(restarted.snapshot(Some("session-live")).unwrap().active_jobs, 1);
        assert_eq!(restarted.recover_interrupted_jobs().unwrap(), 1);
        assert_eq!(restarted.recover_interrupted_jobs().unwrap(), 0);
        assert_eq!(restarted.snapshot(Some("session-live")).unwrap().stale_jobs, 1);
        assert_eq!(serde_json::to_value(done).unwrap(),
            serde_json::to_value(restarted.snapshot(Some("session-done")).unwrap().jobs.remove(0)).unwrap());
    }

    #[test]
    fn global_company_snapshot_hides_legacy_test_jobs_without_deleting_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store
            .start_job("main-real", "main-real", "orchestrator", "real work")
            .unwrap();
        store
            .start_job(
                "mesh-bg-test-deadbeef",
                "mesh-bg-test-deadbeef",
                "coder",
                "synthetic work",
            )
            .unwrap();

        let global = store.snapshot(None).unwrap();
        assert_eq!(global.jobs.len(), 1);
        assert_eq!(global.jobs[0].session_id, "main-real");
        assert_eq!(global.active_jobs, 1);

        let diagnostic = store.snapshot(Some("mesh-bg-test-deadbeef")).unwrap();
        assert_eq!(diagnostic.jobs.len(), 1);
        assert_eq!(diagnostic.jobs[0].subject, "synthetic work");
    }

    #[test]
    fn artifact_supersession_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store
            .append(event(
                "run-a",
                "artifact-a",
                CompanyEventKind::ArtifactPublished {
                    artifact_id: "artifact-a".into(),
                    path: "deck.html".into(),
                    content_hash: "aaa".into(),
                    source_artifacts: vec!["report-v1".into()],
                },
            ))
            .unwrap();
        store
            .append(event(
                "run-a",
                "supersede-a",
                CompanyEventKind::ArtifactSuperseded {
                    artifact_id: "artifact-a".into(),
                    replacement_id: "artifact-b".into(),
                    reason: "source changed".into(),
                },
            ))
            .unwrap();
        let conn = store.connection.lock().unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM company_artifacts WHERE artifact_id='artifact-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "superseded");
    }

    #[test]
    fn composed_event_append_obeys_outer_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        for commit in [false, true] {
            {
                let mut connection = store.connection.lock().unwrap();
                let tx = connection.transaction().unwrap();
                tx.execute("CREATE TABLE IF NOT EXISTS composition_probe(id TEXT PRIMARY KEY)", []).unwrap();
                tx.execute("INSERT INTO composition_probe VALUES('reservation')", []).unwrap();
                store.append_many_in_transaction(&tx, vec![event("composed-run", "composed-open",
                    CompanyEventKind::RunOpened { mission: "one atomic reservation".into() })], true).unwrap();
                if commit { tx.commit().unwrap(); }
            }
            assert_eq!(store.events_since(0, 10).unwrap().len(), usize::from(commit));
            let connection = store.connection.lock().unwrap();
            let table_exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='composition_probe')", [], |row| row.get(0)).unwrap();
            assert_eq!(table_exists, commit, "reservation and event must share commit/rollback");
        }
    }

    #[test]
    fn failed_composed_append_cannot_leak_a_prefix_into_outer_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        {
            let mut connection = store.connection.lock().unwrap();
            let tx = connection.transaction().unwrap();
            tx.execute("CREATE TABLE composition_survivor(id INTEGER)", []).unwrap();
            tx.execute("INSERT INTO composition_survivor VALUES(1)", []).unwrap();
            let result = store.append_many_in_transaction(&tx, vec![
                event("partial-run", "partial-open", CompanyEventKind::RunOpened { mission: "must roll back".into() }),
                event("partial-run", "missing-node", CompanyEventKind::WorkStateChanged {
                    node_id: "missing-node".into(), state: WorkState::Claimed, reason: "invalid second event".into(),
                }),
            ], true);
            assert!(result.is_err());
            // A caller may handle an admission rejection and still commit
            // unrelated bookkeeping. The failed batch must remain atomic.
            tx.commit().unwrap();
        }
        assert!(store.events_since(0, 10).unwrap().is_empty());
        let connection = store.connection.lock().unwrap();
        let count: i64 = connection.query_row("SELECT COUNT(*) FROM composition_survivor", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn event_and_page_limits_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let oversized = event(
            "run-a",
            "oversized",
            CompanyEventKind::RunOpened {
                mission: "x".repeat(MAX_EVENT_BYTES + 1),
            },
        );
        assert!(store.append(oversized).is_err());

        store
            .append(event(
                "run-a",
                "small",
                CompanyEventKind::RunOpened {
                    mission: "ok".into(),
                },
            ))
            .unwrap();
        assert_eq!(store.events_since(0, usize::MAX).unwrap().len(), 1);
    }

    #[test]
    fn work_transitions_require_an_existing_run_node_and_batches_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let missing_claim = vec![
            event(
                "run-a",
                "claim-missing",
                CompanyEventKind::WorkClaimed {
                    node_id: "node-a".into(),
                    attempt_id: "attempt-a".into(),
                    identity_id: "agent-a".into(),
                    approach: "implement".into(),
                },
            ),
            event(
                "run-a",
                "state-missing",
                CompanyEventKind::WorkStateChanged {
                    node_id: "node-a".into(),
                    state: WorkState::Claimed,
                    reason: "claimed".into(),
                },
            ),
        ];
        assert!(store.append_many(missing_claim).is_err());
        assert!(store.events_since(0, 10).unwrap().is_empty());

        store
            .append(event(
                "run-a",
                "propose-a",
                CompanyEventKind::WorkProposed {
                    node_id: "node-a".into(),
                    title: "Node A".into(),
                    outcome: "done".into(),
                    acceptance: vec![],
                    dependencies: vec![],
                    pattern: CollaborationPattern::Split,
                },
            ))
            .unwrap();
        store
            .append_many(vec![
                event(
                    "run-a",
                    "claim-a",
                    CompanyEventKind::WorkClaimed {
                        node_id: "node-a".into(),
                        attempt_id: "attempt-a".into(),
                        identity_id: "agent-a".into(),
                        approach: "implement".into(),
                    },
                ),
                event(
                    "run-a",
                    "state-a",
                    CompanyEventKind::WorkStateChanged {
                        node_id: "node-a".into(),
                        state: WorkState::Claimed,
                        reason: "claimed".into(),
                    },
                ),
            ])
            .unwrap();
        assert_eq!(
            store.snapshot(Some("run-a")).unwrap().work[0].state,
            WorkState::Claimed
        );
        assert!(store
            .append(event(
                "run-b",
                "wrong-run",
                CompanyEventKind::WorkStateChanged {
                    node_id: "node-a".into(),
                    state: WorkState::Ready,
                    reason: "wrong run".into(),
                },
            ))
            .is_err());
        assert_eq!(store.events_since(0, 10).unwrap().len(), 3);
    }

    #[test]
    fn bounded_work_views_report_omitted_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        for index in 0..3 {
            store
                .append(event(
                    "run-a",
                    &format!("proposal-{index}"),
                    CompanyEventKind::WorkProposed {
                        node_id: format!("node-{index}"),
                        title: format!("Node {index}"),
                        outcome: "done".into(),
                        acceptance: vec![],
                        dependencies: vec![],
                        pattern: CollaborationPattern::Split,
                    },
                ))
                .unwrap();
        }
        let (snapshot, snapshot_partial) = store
            .bounded_snapshot_for_run("run-a", 2, 1024 * 1024)
            .unwrap();
        assert_eq!(snapshot.work.len(), 2);
        assert!(snapshot_partial);

        let (events, events_partial) = store
            .events_since_bounded_for_run("run-a", 0, 1, 1024 * 1024)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events_partial);
    }

    #[test]
    fn corrupt_projection_state_is_not_reported_as_a_valid_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store
            .start_job("run-a", "session-a", "coder", "work")
            .unwrap();
        store
            .connection
            .lock()
            .unwrap()
            .execute("UPDATE company_jobs SET state='not-a-state'", [])
            .unwrap();
        assert!(store.snapshot(Some("session-a")).is_err());
    }

    #[test]
    fn every_persisted_company_text_column_is_bounded_before_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store
            .start_job("run-a", "session-a", "coder", "work")
            .unwrap();
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE company_jobs SET run_id=?1",
                rusqlite::params!["x".repeat(MAX_EVENT_BYTES + 1)],
            )
            .unwrap();
        let connection = store.connection.lock().unwrap();
        assert!(validate_company_text_bounds(&connection).is_err());
    }

    #[test]
    fn mirrored_read_hash_rejects_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_MIRRORED_READ_BYTES + 1).unwrap();
        assert!(hash_mirrored_read(&path).is_err());
    }

    fn ledger_participant(agent_id: &str) -> super::super::group_conversation::GroupParticipant {
        super::super::group_conversation::GroupParticipant {
            agent_id: agent_id.to_string(),
            internal_role: agent_id.to_string(),
            display_name: format!("{agent_id} display"),
            role_title: "Reviewer".to_string(),
            color: "#123456".to_string(),
            icon_seed: format!("icon-{agent_id}"),
            avatar: Some(serde_json::json!({"kind":"emoji","value":"🧪"})),
            member_role: "member".to_string(),
            history_access: super::super::company_directory::HistoryAccess::Full,
            history_start_message_index: 0,
            explicitly_mentioned: true,
        }
    }

    fn ledger_intent(
        agent_ids: &[&str],
    ) -> super::super::group_conversation::GroupActivationIntent {
        super::super::group_conversation::GroupActivationIntent {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "group-ledger".to_string(),
            roster_fingerprint: "roster-fingerprint-v1".to_string(),
            selection: super::super::group_conversation::GroupActivationSelection::Explicit,
            active_agent_ids: agent_ids.iter().map(|id| (*id).to_string()).collect(),
            execution_mode: super::super::group_conversation::GroupExecutionMode::Parallel,
            execution_dependencies: None,
            execution_waves: if agent_ids.is_empty() {
                Vec::new()
            } else {
                vec![agent_ids.iter().map(|id| (*id).to_string()).collect()]
            },
        }
    }

    #[test]
    fn authored_room_ping_is_durable_once_and_preserves_user_activation() {
        use crate::runtime::group_conversation::{preview_group_activation,resolve_group_turn_from_activation};
        use crate::runtime::company_directory::{GroupProfile,LifecycleState};
        let directory=tempfile::tempdir().unwrap();
        let path=directory.path().join("company.sqlite");
        let store=CompanyStore::open(&path).unwrap();
        store.ensure_full_catalog_team().unwrap();
        store.create_group("user",GroupProfile {
            group_id:"ping-room".into(),name:"Ping room".into(),description:"test".into(),
            color:"#123456".into(),icon_seed:"test".into(),lifecycle:LifecycleState::Active,
            pinned:false,sort_order:1,canonical_session_id:Some("group-ping-room".into()),metadata_json:"{}".into(),
        },vec!["phoenix".into(),"researcher".into()]).unwrap();
        let snapshot=store.directory_snapshot().unwrap();
        let plan=preview_group_activation(&snapshot,"ping-room","@phoenix ping Theo").unwrap().intent();
        let group=resolve_group_turn_from_activation(&snapshot,&plan).unwrap();
        let original=group.explicitly_pinged().into_iter().cloned().collect::<Vec<_>>();
        store.reserve_group_turn(&group.canonical_session_id,"ping-turn","@phoenix ping Theo",&plan,&original).unwrap();
        let target=group.participants.iter().find(|member|member.agent_id=="researcher").unwrap();
        assert!(store.reserve_group_ping(&group,"ping-turn","phoenix","saved-source",target).unwrap());
        assert!(!store.reserve_group_ping(&group,"ping-turn","phoenix","saved-source",target).unwrap());
        assert!(!store.reserve_group_ping(&group,"ping-turn","researcher","saved-reply",&original[0]).unwrap(),"reply cannot wake the completed author again");
        let mut outside=target.clone();outside.agent_id="frontend".into();outside.internal_role="frontend".into();
        assert!(!store.reserve_group_ping(&group,"ping-turn","phoenix","saved-source",&outside).unwrap(),"outside coworkers cannot be activated by room mentions");
        let saved=store.group_turn(&group.canonical_session_id,"ping-turn").unwrap().unwrap();
        assert_eq!(saved.active_agent_ids,vec!["phoenix"]);
        assert_eq!(saved.members.len(),2);
        assert_eq!(saved.members[1].source_receipt_id.as_deref(),Some("saved-source"));
        assert_eq!(saved.activation.as_ref(),Some(&plan));
        assert!(store.reserve_group_turn(&group.canonical_session_id,"ping-turn","@phoenix ping Theo",&plan,&original).is_ok());
        drop(store);
        let reloaded=CompanyStore::open(&path).unwrap();
        assert_eq!(reloaded.group_turn(&group.canonical_session_id,"ping-turn").unwrap().unwrap(),saved);
        reloaded.mark_group_member_working(&group.canonical_session_id,"ping-turn","researcher").unwrap();
        reloaded.bind_group_ask(&group.canonical_session_id,"ping-turn","researcher","ping-question").unwrap();
        reloaded.mark_group_member_waiting_user(&group.canonical_session_id,"ping-turn","researcher","ping-question").unwrap();
        assert!(reloaded.group_turn(&group.canonical_session_id,"ping-turn").unwrap().unwrap().answer_frontier("researcher").contains(&"researcher".to_string()));
    }

    #[test]
    fn group_assignment_policy_survives_store_reopen_and_rejects_changed_retry() {
        let dir=tempfile::tempdir().unwrap();
        let path=dir.path().join("company.sqlite");
        let mut intent=ledger_intent(&["iris","leo"]);
        intent.inspection_participants.insert("iris".into());
        intent.tool_constraints.insert("iris".into(),vec!["read".into(),"image_analyze".into()]);
        intent.tool_constraints.insert("leo".into(),vec!["computer_*".into()]);
        let participants=[ledger_participant("iris"),ledger_participant("leo")];
        {
            let store=CompanyStore::open(&path).unwrap();
            store.reserve_group_turn("group-policy","policy-turn","inspect then repair",&intent,&participants).unwrap();
        }
        let store=CompanyStore::open(&path).unwrap();
        let saved=store.group_turn("group-policy","policy-turn").unwrap().unwrap().activation.unwrap();
        assert_eq!(saved,intent);
        let mut changed=intent.clone();
        changed.tool_constraints.clear();
        assert!(store.reserve_group_turn("group-policy","policy-turn","inspect then repair",&changed,&participants).is_err());
        assert_eq!(store.group_turn("group-policy","policy-turn").unwrap().unwrap().activation.unwrap(),intent);
    }

    #[test]
    fn persisted_group_done_state_requires_a_nonempty_result_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        store.reserve_group_turn("group-result-proof", "original", "review", &ledger_intent(&["iris"]), &[ledger_participant("iris")]).unwrap();
        let baseline = store.group_turn("group-result-proof", "original").unwrap().unwrap();
        for receipt in [None, Some(""), Some("   "), Some("saved-result")] {
            // Simulate persisted legacy/corrupt state, not the validated writer.
            store.connection.lock().unwrap().execute(
                "UPDATE company_group_turn_members SET state='done',receipt_id=?1 WHERE canonical_session_id='group-result-proof'", [receipt],
            ).unwrap();
            let loaded = store.group_turn("group-result-proof", "original");
            if receipt == Some("saved-result") {
                assert!(loaded.unwrap().unwrap().is_done());
            } else {
                assert!(loaded.is_err(), "invalid persisted completion must fail loading");
            }
            // Also cover a deserialized/in-memory snapshot that has not gone
            // through the stricter database loader.
            let mut record = baseline.clone();
            record.members[0].state = super::super::group_conversation::GroupMemberActivationState::Done;
            record.members[0].receipt_id = receipt.map(str::to_string);
            assert_eq!(record.is_done(), receipt == Some("saved-result"));
            assert_eq!(record.members[0].has_committed_result(), record.is_done());
        }
    }

    #[test]
    fn legacy_group_answer_frontier_preserves_wave_dependencies() {
        use super::super::group_conversation::{GroupExecutionMode, GroupMemberActivationState as State};
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let mut plan = ledger_intent(&["theo", "leo", "iris"]);
        plan.execution_mode = GroupExecutionMode::Ordered;
        plan.execution_waves = vec![vec!["theo".into(), "leo".into()], vec!["iris".into()]];
        store.reserve_group_turn("group-legacy", "original", "review", &plan, &["theo", "leo", "iris"].map(ledger_participant)).unwrap();
        let mut record = store.group_turn("group-legacy", "original").unwrap().unwrap();
        record.members[0].state = State::WaitingUser;
        record.members[1].state = State::Working;
        assert_eq!(record.answer_frontier("theo"), vec!["theo"]);
        record.members[1].state = State::Done;
        assert_eq!(record.answer_frontier("theo"), vec!["theo"], "done without a saved result is not an input");
        record.members[1].receipt_id = Some("saved-leo".into());
        assert_eq!(record.answer_frontier("theo"), vec!["theo", "iris"]);
        record.members[0].state = State::Done;
        record.members[0].receipt_id = Some("saved-theo".into());
        assert!(record.ready_frontier().is_empty(), "legacy background work retains its existing dispatcher");
        let plan = record.activation.as_mut().unwrap();
        plan.execution_dependencies = Some(super::super::group_conversation::legacy_wave_dependencies(&plan.execution_waves));
        assert_eq!(record.ready_frontier(), vec!["iris"]);
        record.activation = None;
        assert_eq!(record.answer_frontier("theo"), vec!["theo"]);
        assert!(record.ready_frontier().is_empty());
    }

    #[test]
    fn group_working_admission_requires_committed_prerequisites() {
        use super::super::group_conversation::{GroupDependency, GroupExecutionMode, GroupMemberActivationState as State};
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(&dir.path().join("company.sqlite")).unwrap();
        let mut plan = ledger_intent(&["theo", "leo", "iris"]);
        plan.execution_mode = GroupExecutionMode::Ordered;
        plan.execution_dependencies = Some(vec![GroupDependency {
            prerequisite: "theo".into(), dependent: "iris".into(),
        }]);
        plan.execution_waves = vec![vec!["theo".into(), "leo".into()], vec!["iris".into()]];
        let participants = ["theo", "leo", "iris"].map(ledger_participant);
        store.reserve_group_turn("group-admission", "ordered-turn", "review", &plan, &participants).unwrap();
        let before = store.group_turn("group-admission", "ordered-turn").unwrap().unwrap();
        assert!(store.mark_group_member_working("group-admission", "ordered-turn", "iris").is_err());
        let after = store.group_turn("group-admission", "ordered-turn").unwrap().unwrap();
        assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(after).unwrap());
        store.mark_group_member_working("group-admission", "ordered-turn", "leo").unwrap();
        store.mark_group_member_working("group-admission", "ordered-turn", "theo").unwrap();
        store.mark_group_member_waiting_user("group-admission", "ordered-turn", "theo", "ask-theo").unwrap();
        assert!(store.mark_group_member_working("group-admission", "ordered-turn", "iris").is_err());
        store.mark_group_member_done("group-admission", "ordered-turn", "theo", "saved-brief").unwrap();
        store.mark_group_member_working("group-admission", "ordered-turn", "iris").unwrap();
        store.mark_group_member_working("group-admission", "ordered-turn", "iris").unwrap();
        let current = store.group_turn("group-admission", "ordered-turn").unwrap().unwrap();
        assert_eq!(current.members.iter().find(|m| m.participant.agent_id == "leo").unwrap().state, State::Working);
        // A late old worker must not execute after ownership was transferred.
        store.reserve_group_turn("group-admission", "old-question", "answer", &ledger_intent(&["theo"]), &[ledger_participant("theo")]).unwrap();
        store.mark_group_member_working("group-admission", "old-question", "theo").unwrap();
        store.mark_group_member_waiting_user("group-admission", "old-question", "theo", "ask-transfer").unwrap();
        store.supersede_waiting_group_activation("group-admission", "ask-transfer", "new-answer").unwrap();
        let transferred = store.group_turn("group-admission", "old-question").unwrap().unwrap();
        assert!(store.mark_group_member_working("group-admission", "old-question", "theo").is_err());
        assert_eq!(serde_json::to_value(transferred).unwrap(), serde_json::to_value(store.group_turn("group-admission", "old-question").unwrap().unwrap()).unwrap());
    }

    #[test]
    fn group_receipt_recovery_does_not_require_a_fake_working_transition() {
        use super::super::group_conversation::GroupMemberActivationState as State;
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(&dir.path().join("company.sqlite")).unwrap();
        let participants = vec![ledger_participant("iris")];
        for (turn, blocked) in [("recover_queued", false), ("recover_blocked", true)] {
            store.reserve_group_turn("group-ledger-session", turn, "recover", &ledger_intent(&["iris"]), &participants).unwrap();
            if blocked { store.mark_group_member_blocked("group-ledger-session", turn, "iris", "interrupted").unwrap(); }
            assert!(store.mark_group_member_done("group-ledger-session", turn, "iris", "").is_err());
            store.mark_group_member_done("group-ledger-session", turn, "iris", "persisted-contribution").unwrap();
            let record = store.group_turn("group-ledger-session", turn).unwrap().unwrap();
            assert_eq!(record.members[0].state, State::Done);
            assert_eq!(record.members[0].receipt_id.as_deref(), Some("persisted-contribution"));
        }
    }

    #[test]
    fn group_join_outbox_survives_two_early_answers_and_restart() {
        use super::super::group_conversation::{GroupDependency, GroupExecutionMode};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let mut plan = ledger_intent(&["iris", "remy", "integrator"]);
        plan.execution_mode = GroupExecutionMode::Ordered;
        plan.execution_waves = vec![vec!["iris".into(), "remy".into()], vec!["integrator".into()]];
        plan.execution_dependencies = Some(["iris", "remy"].map(|id| GroupDependency {
            prerequisite: id.into(), dependent: "integrator".into(),
        }).to_vec());
        let participants = ["iris", "remy", "integrator"].map(ledger_participant);
        store.reserve_group_turn("group-join", "original", "join two branches", &plan, &participants).unwrap();
        // Both answers arrive before either resumed branch has completed.
        for id in ["iris", "remy"] {
            store.mark_group_member_working("group-join", "original", id).unwrap();
            store.mark_group_member_waiting_user("group-join", "original", id, &format!("ask-{id}")).unwrap();
        }
        for id in ["iris", "remy"] {
            let successor = format!("answer-{id}");
            store.supersede_waiting_group_activation("group-join", &format!("ask-{id}"), &successor).unwrap();
            store.reserve_group_turn("group-join", &successor, "answer", &ledger_intent(&[id]), &[ledger_participant(id)]).unwrap();
        }
        store.mark_group_member_done("group-join", "answer-iris", "iris", "result-iris").unwrap();
        assert!(store.pending_group_continuations().unwrap().is_empty());
        store.mark_group_member_done("group-join", "answer-remy", "remy", "result-remy").unwrap();
        store.mark_group_member_done("group-join", "answer-remy", "remy", "result-remy").unwrap();
        drop(store);
        let store = CompanyStore::open(&path).unwrap();
        let pending = store.pending_group_continuations().unwrap();
        assert_eq!(pending.len(), 1);
        let ready = &pending[0];
        assert_eq!(ready.activation.active_agent_ids, vec!["integrator"]);
        assert_eq!(ready.activation.execution_dependencies, Some(vec![]));
        assert_eq!(ready.predecessor_receipts, vec!["result-iris", "result-remy"]);
        assert_eq!(ready.original_request.as_deref(), Some("join two branches"));
        assert_eq!(store.freeze_group_continuation_dispatch("group-join", &ready.turn_id, "original envelope").unwrap(), "original envelope");
        assert_eq!(store.freeze_group_continuation_dispatch("group-join", &ready.turn_id, "changed permissions").unwrap(), "original envelope");
        // Acknowledgement follows successful queueing; completing that task
        // settles the original plan without executing either branch again.
        store.acknowledge_group_continuation("group-join", &ready.turn_id).unwrap();
        assert!(store.pending_group_continuations().unwrap().is_empty());
        store.reserve_group_turn("group-join", &ready.turn_id, "integrate", &ready.activation, &[ledger_participant("integrator")]).unwrap();
        store.mark_group_member_done("group-join", &ready.turn_id, "integrator", "integrated-result").unwrap();
        assert!(store.group_turn("group-join", "original").unwrap().unwrap().is_done());
        assert!(store.pending_group_continuations().unwrap().is_empty());
    }

    #[test]
    fn group_question_is_owned_before_yield_and_obsolete_result_cannot_release_join() {
        use super::super::group_conversation::{GroupDependency, GroupExecutionMode, GroupMemberActivationState as State};
        for (recovered, answer_first) in [(false,false),(true,false),(false,true),(true,true)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let mut plan = ledger_intent(&["theo","leo","remy"]);
        plan.execution_mode = GroupExecutionMode::Ordered;
        plan.execution_waves = vec![vec!["theo".into(),"leo".into()],vec!["remy".into()]];
        plan.execution_dependencies = Some(["theo","leo"].map(|id|GroupDependency { prerequisite:id.into(),dependent:"remy".into() }).to_vec());
        let participants = ["theo","leo","remy"].map(ledger_participant);
        store.reserve_group_turn("group-early-question","original","review",&plan,&participants).unwrap();
        for id in ["theo","leo"] { store.mark_group_member_working("group-early-question","original",id).unwrap(); }
        store.bind_group_ask("group-early-question","original","theo","ask-early").unwrap();
        assert!(store.group_ask_is_bound("group-early-question", "ask-early").unwrap());
        assert!(!store.group_ask_is_bound("group-early-question", "unbound-ask").unwrap());
        assert!(!store.group_ask_is_bound("different-group", "ask-early").unwrap());
        store.bind_group_ask("group-early-question","original","theo","ask-early").unwrap();
        assert_eq!(store.group_member_ask_ids("group-early-question","original","theo").unwrap(),vec!["ask-early"]);
        assert!(store.group_member_ask_ids("group-early-question","original","leo").unwrap().is_empty());
        assert!(store.bind_group_ask("group-early-question","original","leo","ask-early").is_err());
        drop(store);
        let store = CompanyStore::open(&path).unwrap();
        if recovered { assert_eq!(store.recover_stale_group_activations().unwrap(),2); }
        let original = store.group_turn_waiting_on_ask("group-early-question","ask-early").unwrap().unwrap();
        assert_eq!(original.answer_frontier("theo"),vec!["theo"]);
        assert_eq!(original.members.iter().find(|m|m.participant.agent_id=="theo").unwrap().state,if recovered { State::Blocked } else { State::Working });
        store.supersede_waiting_group_activation("group-early-question","ask-early","answer-early").unwrap();
        assert!(!store.settle_group_member_result("group-early-question","original","theo","waiting-message",None).unwrap());
        if !answer_first {
            assert!(store.settle_group_member_result("group-early-question","original","leo","audit-result",None).unwrap());
        }
        assert!(store.pending_group_continuations().unwrap().is_empty());
        let original=store.group_turn("group-early-question","original").unwrap().unwrap();
        assert_eq!(original.members.iter().find(|m|m.participant.agent_id=="remy").unwrap().state,State::Queued);
        store.reserve_group_turn("group-early-question","answer-early","0.1 mm",&ledger_intent(&["theo"]),&[ledger_participant("theo")]).unwrap();
        store.mark_group_member_done("group-early-question","answer-early","theo","decision-result").unwrap();
        if answer_first {
            assert!(store.pending_group_continuations().unwrap().is_empty());
            assert!(store.settle_group_member_result("group-early-question","original","leo","audit-result",None).unwrap());
        }
        let ready=store.pending_group_continuations().unwrap();
        assert_eq!(ready.len(),1);
        assert_eq!(ready[0].activation.active_agent_ids,vec!["remy"]);
        assert_eq!(ready[0].predecessor_receipts,vec!["decision-result","audit-result"]);
        }
    }

    #[test]
    fn group_answer_transfers_only_its_unblocked_branch_not_sibling_or_join() {
        use super::super::group_conversation::{GroupDependency, GroupExecutionMode, GroupMemberActivationState as State};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let ids = ["theo", "leo", "iris", "remy", "integrator"];
        let participants = ids.iter().map(|id| ledger_participant(id)).collect::<Vec<_>>();
        let mut intent = ledger_intent(&ids);
        let edges = [("theo", "iris"), ("leo", "remy"), ("iris", "integrator"), ("remy", "integrator")].into_iter()
            .map(|(a,b)| GroupDependency { prerequisite: a.into(), dependent: b.into() }).collect::<Vec<_>>();
        intent.execution_mode = GroupExecutionMode::Ordered;
        intent.execution_waves = super::super::group_conversation::dependency_waves(&intent.active_agent_ids, &edges).unwrap();
        intent.execution_dependencies = Some(edges);
        store.reserve_group_turn("group-branches", "original", "two independent questions", &intent, &participants).unwrap();
        for (agent, ask) in [("theo", "ask-theo"), ("leo", "ask-leo")] {
            store.mark_group_member_working("group-branches", "original", agent).unwrap();
            store.mark_group_member_waiting_user("group-branches", "original", agent, ask).unwrap();
        }
        let original = store.group_turn("group-branches", "original").unwrap().unwrap();
        assert_eq!(original.answer_frontier("theo"), vec!["theo", "iris"]);
        store.supersede_waiting_group_activation("group-branches", "ask-theo", "answer-theo").unwrap();
        drop(store);
        let store = CompanyStore::open(&path).unwrap();
        let restored = store.group_turn("group-branches", "original").unwrap().unwrap();
        assert_eq!(restored.members[0].state, State::Blocked);
        assert_eq!(restored.members[2].receipt_id.as_deref(), Some("answer-theo"));
        assert_eq!(restored.members[1].state, State::WaitingUser);
        assert_eq!(restored.members[1].receipt_id.as_deref(), Some("ask-leo"));
        assert_eq!(restored.members[3].state, State::Queued);
        assert_eq!(restored.members[4].state, State::Queued);
        assert!(store.group_turn_waiting_on_ask("group-branches", "ask-leo").unwrap().is_some());

        let resumed = [ledger_participant("theo"), ledger_participant("iris")];
        store.reserve_group_turn("group-branches", "answer-theo", "resume", &ledger_intent(&["theo", "iris"]), &resumed).unwrap();
        // Theo asks once more. Completing that nested continuation must repair
        // both ancestors, not merely the immediately preceding answer turn.
        store.mark_group_member_working("group-branches", "answer-theo", "theo").unwrap();
        store.mark_group_member_waiting_user("group-branches", "answer-theo", "theo", "ask-again").unwrap();
        store.supersede_waiting_group_activation("group-branches", "ask-again", "answer-again").unwrap();
        store.reserve_group_turn("group-branches", "answer-again", "resume again", &ledger_intent(&["theo", "iris"]), &resumed).unwrap();
        for agent in ["theo", "iris"] {
            let receipt = format!("saved-{agent}");
            store.mark_group_member_done("group-branches", "answer-again", agent, &receipt).unwrap();
            store.mark_group_member_done("group-branches", "answer-again", agent, &receipt).unwrap();
            assert!(store.mark_group_member_done("group-branches", "answer-again", agent, "substituted").is_err());
        }
        drop(store);
        let store = CompanyStore::open(&path).unwrap();
        for turn in ["original", "answer-theo", "answer-again"] {
            let record = store.group_turn("group-branches", turn).unwrap().unwrap();
            for agent in ["theo", "iris"] {
                let member = record.members.iter().find(|m| m.participant.agent_id == agent).unwrap();
                assert_eq!(member.state, State::Done);
                assert_eq!(member.receipt_id, Some(format!("saved-{agent}")));
            }
        }
        let original = store.group_turn("group-branches", "original").unwrap().unwrap();
        assert_eq!(original.members[1].receipt_id.as_deref(), Some("ask-leo"));
        assert_eq!(original.members[1].state, State::WaitingUser);
        assert_eq!(original.answer_frontier("leo"), vec!["leo", "remy", "integrator"]);
    }

    #[test]
    fn durable_group_turn_ledger_preserves_zero_activation_as_explicitly_done() {
        use super::super::group_conversation::GroupTurnLedgerReservation;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let intent = ledger_intent(&[]);
        let first = store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_zero_activation",
                "A room note with no mentions",
                &intent,
                &[],
            )
            .unwrap();
        let GroupTurnLedgerReservation::New(record) = first else {
            panic!("first reservation must be new")
        };
        assert!(record.members.is_empty());
        assert!(record.active_agent_ids.is_empty());
        assert!(record.is_done());

        drop(store);
        let reopened = CompanyStore::open(&path).unwrap();
        let loaded = reopened
            .group_turn("group-ledger-session", "turn_zero_activation")
            .unwrap()
            .unwrap();
        assert!(loaded.is_done());
        assert_eq!(loaded.prompt_hash, record.prompt_hash);
    }

    #[test]
    fn durable_group_turn_retry_is_idempotent_and_payload_substitution_fails() {
        use super::super::group_conversation::GroupTurnLedgerReservation;

        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let intent = ledger_intent(&["iris"]);
        let participants = vec![ledger_participant("iris")];
        let first = store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_duplicate_retry",
                "@iris inspect this",
                &intent,
                &participants,
            )
            .unwrap();
        let first_id = first.record().members[0].activation_id.clone();
        let retried = store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_duplicate_retry",
                "@iris inspect this",
                &intent,
                &participants,
            )
            .unwrap();
        assert!(matches!(retried, GroupTurnLedgerReservation::Existing(_)));
        assert_eq!(retried.record().members[0].activation_id, first_id);

        let prompt_error = store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_duplicate_retry",
                "@iris inspect something else",
                &intent,
                &participants,
            )
            .unwrap_err();
        assert!(prompt_error.to_string().contains("different prompt"));

        let mut substituted_intent = intent.clone();
        substituted_intent.roster_fingerprint = "substituted-roster".to_string();
        let roster_error = store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_duplicate_retry",
                "@iris inspect this",
                &substituted_intent,
                &participants,
            )
            .unwrap_err();
        assert!(roster_error.to_string().contains("different prompt"));

        // Plan identity is immutable too, even when participant order and the
        // human prompt are unchanged. Persist it for future ask continuations.
        let mut substituted_plan = intent.clone();
        substituted_plan.execution_dependencies = Some(Vec::new());
        assert!(store.reserve_group_turn(
            "group-ledger-session", "turn_duplicate_retry", "@iris inspect this",
            &substituted_plan, &participants,
        ).is_err());
        let reloaded = store.group_turn("group-ledger-session", "turn_duplicate_retry").unwrap().unwrap();
        assert_eq!(reloaded.activation, Some(intent.clone()));
    }

    #[test]
    fn durable_group_member_states_survive_reload_and_working_recovers_blocked() {
        use super::super::group_conversation::GroupMemberActivationState as State;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let intent = ledger_intent(&["iris"]);
        let participants = vec![ledger_participant("iris")];
        store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_state_reload",
                "@iris inspect this",
                &intent,
                &participants,
            )
            .unwrap();
        store
            .mark_group_member_working("group-ledger-session", "turn_state_reload", "iris")
            .unwrap();
        assert_eq!(
            store
                .group_turn("group-ledger-session", "turn_state_reload")
                .unwrap()
                .unwrap()
                .members[0]
                .state,
            State::Working
        );

        // Process startup calls this same recovery gate once. A working row is
        // preserved as stale/blocked evidence instead of silently completed.
        assert_eq!(store.recover_stale_group_activations().unwrap(), 1);
        let stale = store
            .group_turn("group-ledger-session", "turn_state_reload")
            .unwrap()
            .unwrap();
        assert_eq!(stale.members[0].state, State::Blocked);
        assert!(stale.members[0].status_detail.contains("stale"));

        store
            .mark_group_member_working("group-ledger-session", "turn_state_reload", "iris")
            .unwrap();
        store
            .mark_group_member_waiting_user(
                "group-ledger-session",
                "turn_state_reload",
                "iris",
                "ask-ledger-1",
            )
            .unwrap();
        let waiting = store
            .group_turn("group-ledger-session", "turn_state_reload")
            .unwrap()
            .unwrap();
        assert_eq!(waiting.members[0].state, State::WaitingUser);
        assert_eq!(
            waiting.members[0].receipt_id.as_deref(),
            Some("ask-ledger-1")
        );
        store
            .mark_group_member_working("group-ledger-session", "turn_state_reload", "iris")
            .unwrap();
        store
            .mark_group_member_done(
                "group-ledger-session",
                "turn_state_reload",
                "iris",
                "group-message-ledger-1",
            )
            .unwrap();
        store
            .mark_group_member_done(
                "group-ledger-session",
                "turn_state_reload",
                "iris",
                "group-message-ledger-1",
            )
            .unwrap();
        assert!(store
            .mark_group_member_done(
                "group-ledger-session",
                "turn_state_reload",
                "iris",
                "group-message-substitution",
            )
            .is_err());
        assert!(store
            .mark_group_member_working("group-ledger-session", "turn_state_reload", "iris")
            .is_err());
        drop(store);

        let reopened = CompanyStore::open(&path).unwrap();
        let done = reopened
            .group_turn("group-ledger-session", "turn_state_reload")
            .unwrap()
            .unwrap();
        assert_eq!(done.members[0].state, State::Done);
        assert_eq!(
            done.members[0].receipt_id.as_deref(),
            Some("group-message-ledger-1")
        );
        assert!(done.is_done());
    }

    #[test]
    fn answered_group_ask_links_successor_and_never_reloads_as_waiting() {
        use super::super::group_conversation::GroupMemberActivationState as State;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let store = CompanyStore::open(&path).unwrap();
        let intent = ledger_intent(&["iris", "theo"]);
        let participants = vec![ledger_participant("iris"), ledger_participant("theo")];
        store
            .reserve_group_turn(
                "group-ledger-session",
                "turn_waiting_answer",
                "@iris ask if blocked",
                &intent,
                &participants,
            )
            .unwrap();
        store
            .mark_group_member_working("group-ledger-session", "turn_waiting_answer", "iris")
            .unwrap();
        store
            .mark_group_member_waiting_user(
                "group-ledger-session",
                "turn_waiting_answer",
                "iris",
                "ask-group-answer-1",
            )
            .unwrap();
        let pending = store
            .group_turn_waiting_on_ask("group-ledger-session", "ask-group-answer-1")
            .unwrap()
            .unwrap();
        assert_eq!(pending.members[0].state, State::WaitingUser);
        assert_eq!(pending.members[1].state, State::Queued);
        assert_eq!(
            store
                .supersede_waiting_group_activation(
                    "group-ledger-session",
                    "ask-group-answer-1",
                    "ask_answer_successor_1",
                )
                .unwrap()
                .as_deref(),
            Some("turn_waiting_answer")
        );
        // Replaying the same answer/queue handoff is idempotent: the original
        // is no longer waiting and therefore cannot be superseded twice.
        assert!(store
            .supersede_waiting_group_activation(
                "group-ledger-session",
                "ask-group-answer-1",
                "ask_answer_successor_1",
            )
            .unwrap()
            .is_none());
        drop(store);

        let reopened = CompanyStore::open(&path).unwrap();
        let original = reopened
            .group_turn("group-ledger-session", "turn_waiting_answer")
            .unwrap()
            .unwrap();
        assert_eq!(original.members[0].state, State::Blocked);
        assert_eq!(
            original.members[0].receipt_id.as_deref(),
            Some("ask_answer_successor_1")
        );
        assert!(original.members[0].status_detail.contains("successor turn"));
        assert_eq!(original.members[1].state, State::Blocked);
        assert_eq!(
            original.members[1].receipt_id.as_deref(),
            Some("ask_answer_successor_1")
        );
    }

    #[cfg(unix)]
    #[test]
    fn company_store_and_mirrored_reads_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"not a sqlite db").unwrap();
        let linked_db = dir.path().join("company.sqlite");
        symlink(&target, &linked_db).unwrap();
        assert!(CompanyStore::open(&linked_db).is_err());
        std::fs::remove_file(&linked_db).unwrap();
        symlink(&target, dir.path().join("company.sqlite-wal")).unwrap();
        assert!(CompanyStore::open(&linked_db).is_err());

        let linked_read = dir.path().join("read.txt");
        symlink(&target, &linked_read).unwrap();
        assert!(hash_mirrored_read(&linked_read).is_err());
    }
}
