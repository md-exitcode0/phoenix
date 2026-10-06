//! Durable FIFO for user prompts submitted while an endless conversation is
//! already working. Queueing and steering are intentionally separate product
//! actions: normal Send lands here; the explicit Steer action uses postbox.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_QUEUE_ROWS_PER_SESSION: i64 = 100;
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUEUE_DB_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QueuedUserTurn {
    /// Client-minted identity for this authored turn. Legacy/internal callers
    /// may omit it, but Canvas always supplies one so reconnect retries cannot
    /// enqueue the same prompt twice.
    #[serde(default)]
    pub turn_id: Option<String>,
    pub user_request: String,
    /// Optional user-facing provenance for daemon-created turns. The model
    /// still receives `user_request`; the story and queue surfaces use this
    /// typed origin so runtime instructions never leak into conversation UI.
    #[serde(default)]
    pub origin: Option<crate::runtime::TurnOrigin>,
    #[serde(default)]
    pub interaction_mode: crate::runtime::InteractionMode,
    pub permission_mode: Option<crate::tools::PermissionMode>,
    pub yolo: Option<bool>,
    pub workspace: Option<PathBuf>,
    pub target_agent: Option<String>,
    pub target_group: Option<String>,
    /// Stable, previewed group wake set. This survives queueing so a roster
    /// change cannot silently wake a different set of coworkers later.
    #[serde(default)]
    pub group_activation: Option<crate::runtime::group_conversation::GroupActivationIntent>,
    pub sticky_notes: Option<Vec<super::daemon::StickyNoteData>>,
    pub viewport: Option<super::daemon::ViewportData>,
    pub attachments: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ClaimedTurn {
    pub queue_id: String,
    pub payload: QueuedUserTurn,
    /// Persisted claim count, including this claim. A retry is not proof that
    /// the previous execution never ran or produced a result.
    pub attempts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct QueuedTurnSummary {
    pub queue_id: String,
    pub preview: String,
    #[serde(default)]
    pub origin: Option<crate::runtime::TurnOrigin>,
    pub interaction_mode: crate::runtime::InteractionMode,
    pub target_agent: Option<String>,
    pub target_group: Option<String>,
    pub state: String,
    pub enqueued_at: String,
    pub failure: Option<String>,
}

fn path() -> PathBuf {
    crate::config::phoenix_home()
        .join("company")
        .join("turn_queue.sqlite")
}

fn open() -> Result<Connection> {
    let path = path();
    crate::config::private_io::prepare_private_parent(&path)?;
    crate::config::private_io::reject_symlink_components(&path)?;
    for candidate in [
        path.clone(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                anyhow::bail!(
                    "refusing unsafe queued-prompt store {}",
                    candidate.display()
                );
            }
            Ok(metadata) if candidate == path && metadata.len() > MAX_QUEUE_DB_BYTES => {
                anyhow::bail!("queued-prompt store is unexpectedly large");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let connection = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         CREATE TABLE IF NOT EXISTS queued_user_turns(
            queue_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            turn_id TEXT,
            payload_json TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('queued','running','failed')),
            attempts INTEGER NOT NULL DEFAULT 0,
            failure TEXT,
            enqueued_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS queued_user_turns_session_fifo
            ON queued_user_turns(session_id,state,enqueued_at,queue_id);",
    )?;
    let has_turn_id = {
        let mut statement = connection.prepare("PRAGMA table_info(queued_user_turns)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns.iter().any(|column| column == "turn_id")
    };
    if !has_turn_id {
        connection.execute("ALTER TABLE queued_user_turns ADD COLUMN turn_id TEXT", [])?;
    }
    connection.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS queued_user_turns_client_turn
           ON queued_user_turns(session_id,turn_id) WHERE turn_id IS NOT NULL;
         CREATE TABLE IF NOT EXISTS immediate_group_turns(
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            payload_hash TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('reserved','settled')),
            runtime_epoch TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(session_id,turn_id)
         );
         CREATE TABLE IF NOT EXISTS queued_turn_receipts(
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            queue_id TEXT NOT NULL,
            payload_hash TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(session_id,turn_id)
         );
         CREATE TABLE IF NOT EXISTS frozen_answer_turns(
            session_id TEXT NOT NULL,
            ask_id TEXT NOT NULL,
            answer_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            PRIMARY KEY(session_id,ask_id)
         );",
    )?;
    let immediate_columns = {
        let mut statement = connection.prepare("PRAGMA table_info(immediate_group_turns)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !immediate_columns
        .iter()
        .any(|column| column == "runtime_epoch")
    {
        connection.execute(
            "ALTER TABLE immediate_group_turns ADD COLUMN runtime_epoch TEXT",
            [],
        )?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(connection)
}

pub(crate) fn validate_client_turn_id(turn_id: &str) -> Result<()> {
    anyhow::ensure!(
        (8..=128).contains(&turn_id.len())
            && turn_id
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') }),
        "invalid client turn_id"
    );
    Ok(())
}

fn payload_fingerprint(payload_json: &str) -> String {
    format!("{:x}", Sha256::digest(payload_json.as_bytes()))
}

pub(crate) fn recover_interrupted() -> Result<usize> {
    let connection = open()?;
    Ok(connection.execute(
        "UPDATE queued_user_turns SET state='queued',updated_at=?1,
            failure='gateway restarted before queued turn settled'
         WHERE state='running'",
        [Utc::now().to_rfc3339()],
    )?)
}

/// Freeze before touching the execution queue. Reconnects must not rebuild a
/// different dependency frontier or inherit newly changed access settings.
/// The builder runs outside the queue transaction (it may read company state).
pub(crate) fn frozen_answer_turn(
    session_id: &str,
    ask_id: &str,
    answer: &str,
    build: impl FnOnce() -> Result<QueuedUserTurn>,
) -> Result<QueuedUserTurn> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    anyhow::ensure!(!ask_id.trim().is_empty() && ask_id.len() <= 256, "invalid answer ask identity");
    let answer_hash = payload_fingerprint(answer);
    if let Some(payload) = lookup_answer_turn(&open()?, session_id, ask_id, &answer_hash)? { return Ok(payload) }
    let candidate = build()?;
    validate_answer_envelope(&candidate, ask_id)?;
    let json = serde_json::to_string(&candidate)?;
    anyhow::ensure!(json.len() <= MAX_PAYLOAD_BYTES, "answer envelope is too large");
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another handler may have frozen the same answer while this one planned.
    if let Some(payload) = lookup_answer_turn(&tx, session_id, ask_id, &answer_hash)? { tx.commit()?; return Ok(payload) }
    tx.execute("INSERT INTO frozen_answer_turns(session_id,ask_id,answer_hash,payload_json) VALUES(?1,?2,?3,?4)", params![session_id,ask_id,answer_hash,json])?;
    tx.commit()?;
    Ok(candidate)
}

/// Read an existing answer envelope without creating or submitting work.
pub(crate) fn existing_answer_turn(
    session_id: &str,
    ask_id: &str,
    answer: &str,
) -> Result<Option<QueuedUserTurn>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    anyhow::ensure!(!ask_id.trim().is_empty() && ask_id.len() <= 256, "invalid answer ask identity");
    lookup_answer_turn(&open()?, session_id, ask_id, &payload_fingerprint(answer))
}

fn lookup_answer_turn(
    connection: &Connection,
    session_id: &str,
    ask_id: &str,
    answer_hash: &str,
) -> Result<Option<QueuedUserTurn>> {
    let saved: Option<(String, String)> = connection.query_row(
        "SELECT answer_hash,payload_json FROM frozen_answer_turns WHERE session_id=?1 AND ask_id=?2",
        params![session_id, ask_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((saved_hash, json)) = saved else { return Ok(None) };
    anyhow::ensure!(saved_hash == answer_hash, "this question already has a different saved answer");
    anyhow::ensure!(json.len() <= MAX_PAYLOAD_BYTES, "saved answer envelope is too large");
    let payload: QueuedUserTurn = serde_json::from_str(&json).context("invalid saved answer envelope")?;
    validate_answer_envelope(&payload, ask_id)?;
    Ok(Some(payload))
}

fn validate_answer_envelope(payload: &QueuedUserTurn, ask_id: &str) -> Result<()> {
    validate_client_turn_id(payload.turn_id.as_deref().context("answer envelope has no turn identity")?)?;
    anyhow::ensure!(!payload.user_request.trim().is_empty(), "answer envelope has no request");
    anyhow::ensure!(matches!(&payload.origin, Some(crate::runtime::TurnOrigin::AskAnswer { ask_id: saved, .. }) if saved == ask_id), "answer envelope ask identity mismatch");
    Ok(())
}

/// A consumed/cancelled queue entry retains its idempotency receipt. Retrying
/// that submission must acknowledge the receipt, not recreate member ownership.
pub(crate) fn retired_turn_receipt(session_id: &str, turn_id: &str) -> Result<Option<String>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    validate_client_turn_id(turn_id)?;
    Ok(open()?.query_row(
        "SELECT r.queue_id FROM queued_turn_receipts r
         WHERE r.session_id=?1 AND r.turn_id=?2 AND NOT EXISTS
         (SELECT 1 FROM queued_user_turns q WHERE q.queue_id=r.queue_id)",
        params![session_id, turn_id], |row| row.get(0),
    ).optional()?)
}

/// Read the original submission receipt whether it is queued, running, or
/// retired. A lost answer acknowledgement must not enqueue or wake it again.
pub(crate) fn submitted_turn_receipt(session_id: &str, turn_id: &str) -> Result<Option<String>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    validate_client_turn_id(turn_id)?;
    Ok(open()?.query_row(
        "SELECT queue_id FROM queued_turn_receipts WHERE session_id=?1 AND turn_id=?2",
        params![session_id, turn_id], |row| row.get(0),
    ).optional()?)
}

pub(crate) fn enqueue(session_id: &str, payload: &QueuedUserTurn) -> Result<String> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    anyhow::ensure!(
        !payload.user_request.trim().is_empty(),
        "queued prompt is empty"
    );
    let payload_json = serde_json::to_string(payload)?;
    let payload_hash = payload_fingerprint(&payload_json);
    anyhow::ensure!(
        payload_json.len() <= MAX_PAYLOAD_BYTES,
        "queued prompt envelope is too large"
    );
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(turn_id) = payload.turn_id.as_deref() {
        validate_client_turn_id(turn_id)?;
        if let Some((queue_id, existing_hash)) = tx
            .query_row(
                "SELECT queue_id,payload_hash FROM queued_turn_receipts
                 WHERE session_id=?1 AND turn_id=?2",
                params![session_id, turn_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            anyhow::ensure!(
                existing_hash == payload_hash,
                "client turn_id was already used with a different queued payload"
            );
            tx.commit()?;
            return Ok(queue_id);
        }
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM queued_user_turns
         WHERE session_id=?1 AND state IN ('queued','running')",
        [session_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        count < MAX_QUEUE_ROWS_PER_SESSION,
        "this conversation already has {MAX_QUEUE_ROWS_PER_SESSION} queued prompts"
    );
    let queue_id = format!("queued_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO queued_user_turns(
            queue_id,session_id,turn_id,payload_json,state,attempts,enqueued_at,updated_at)
         VALUES(?1,?2,?3,?4,'queued',0,?5,?5)",
        params![queue_id, session_id, payload.turn_id, payload_json, now],
    )?;
    if let Some(turn_id) = payload.turn_id.as_deref() {
        tx.execute(
            "INSERT INTO queued_turn_receipts(
                session_id,turn_id,queue_id,payload_hash,created_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![session_id, turn_id, queue_id, payload_hash, now],
        )?;
    }
    tx.commit()?;
    Ok(queue_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImmediateGroupReservation {
    New,
    /// This same gateway process already accepted the turn. A reconnect race
    /// must not execute it twice.
    ExistingInFlight,
    ExistingSettled,
    /// An earlier gateway died before settling. The immutable payload matched
    /// and this process atomically claimed the same receipt for resume.
    Recovered,
}

fn immediate_runtime_epoch() -> &'static str {
    static EPOCH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(|| {
            format!(
                "gateway-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            )
        })
        .as_str()
}

/// Reserve an immediate group turn before execution. Company events cannot be
/// used for this: their general idempotency path returns an existing event
/// without comparing immutable payloads. This small receipt table does compare
/// the exact envelope and durably suppresses reconnect re-execution.
pub(crate) fn reserve_immediate_group(
    session_id: &str,
    turn_id: &str,
    payload: &QueuedUserTurn,
) -> Result<ImmediateGroupReservation> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    validate_client_turn_id(turn_id)?;
    anyhow::ensure!(
        payload.target_group.is_some(),
        "immediate receipt requires a group turn"
    );
    anyhow::ensure!(
        payload.turn_id.as_deref() == Some(turn_id),
        "immediate receipt turn_id does not match its envelope"
    );
    let payload_json = serde_json::to_string(payload)?;
    let payload_hash = payload_fingerprint(&payload_json);
    anyhow::ensure!(
        payload_json.len() <= MAX_PAYLOAD_BYTES,
        "turn envelope is too large"
    );
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some((existing_hash, state, runtime_epoch)) = tx
        .query_row(
            "SELECT payload_hash,state,runtime_epoch FROM immediate_group_turns
             WHERE session_id=?1 AND turn_id=?2",
            params![session_id, turn_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
    {
        anyhow::ensure!(
            existing_hash == payload_hash,
            "client turn_id was already used with a different immediate payload"
        );
        if state == "settled" {
            tx.commit()?;
            return Ok(ImmediateGroupReservation::ExistingSettled);
        }
        anyhow::ensure!(state == "reserved", "invalid immediate group receipt state");
        if runtime_epoch.as_deref() == Some(immediate_runtime_epoch()) {
            tx.commit()?;
            return Ok(ImmediateGroupReservation::ExistingInFlight);
        }
        tx.execute(
            "UPDATE immediate_group_turns SET runtime_epoch=?1,updated_at=?2
             WHERE session_id=?3 AND turn_id=?4 AND state='reserved'",
            params![
                immediate_runtime_epoch(),
                Utc::now().to_rfc3339(),
                session_id,
                turn_id
            ],
        )?;
        tx.commit()?;
        return Ok(ImmediateGroupReservation::Recovered);
    }
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO immediate_group_turns(
            session_id,turn_id,payload_hash,state,runtime_epoch,created_at,updated_at)
         VALUES(?1,?2,?3,'reserved',?4,?5,?5)",
        params![
            session_id,
            turn_id,
            payload_hash,
            immediate_runtime_epoch(),
            now
        ],
    )?;
    tx.commit()?;
    Ok(ImmediateGroupReservation::New)
}

pub(crate) fn settle_immediate_group(session_id: &str, turn_id: &str) -> Result<()> {
    let connection = open()?;
    settle_immediate_group_in(&connection, session_id, turn_id)
}

fn settle_immediate_group_in(connection: &Connection, session_id: &str, turn_id: &str) -> Result<()> {
    let changed = connection.execute(
        "UPDATE immediate_group_turns SET state='settled',updated_at=?1
         WHERE session_id=?2 AND turn_id=?3",
        params![Utc::now().to_rfc3339(), session_id, turn_id],
    )?;
    anyhow::ensure!(changed == 1, "immediate group turn was not reserved");
    Ok(())
}

/// Release an unsuccessful/blocked reservation for an exact same-payload
/// retry in this process. The immutable hash remains, so substitution still
/// fails and the next caller atomically recovers this same receipt.
pub(crate) fn release_immediate_group(session_id: &str, turn_id: &str) -> Result<()> {
    let connection = open()?;
    let changed = connection.execute(
        "UPDATE immediate_group_turns
         SET runtime_epoch=CASE WHEN state='reserved' THEN NULL ELSE runtime_epoch END,
             updated_at=CASE WHEN state='reserved' THEN ?1 ELSE updated_at END
         WHERE session_id=?2 AND turn_id=?3 AND state IN ('reserved','settled')",
        params![Utc::now().to_rfc3339(), session_id, turn_id],
    )?;
    anyhow::ensure!(changed == 1, "immediate group turn is not reserved");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImmediateGroupFinalization {
    Settled,
    WaitingUser,
    Released,
}

/// Settle only on explicit all-member completion evidence. Pending questions
/// retain their reservation; blocked/error/canceled work is released for an
/// exact-payload retry. Silence alone can therefore never settle a turn.
pub(crate) fn finalize_immediate_group(
    session_id: &str,
    turn_id: &str,
    execution_succeeded: bool,
) -> Result<ImmediateGroupFinalization> {
    use crate::runtime::group_conversation::GroupMemberActivationState as State;

    let record = crate::runtime::company::global()?.group_turn(session_id, turn_id)?;
    if execution_succeeded && record.as_ref().is_some_and(|record| record.is_done()) {
        settle_immediate_group(session_id, turn_id)?;
        return Ok(ImmediateGroupFinalization::Settled);
    }
    if record.as_ref().is_some_and(|record| {
        record
            .members
            .iter()
            .any(|member| member.state == State::WaitingUser)
    }) {
        return Ok(ImmediateGroupFinalization::WaitingUser);
    }
    release_immediate_group(session_id, turn_id)?;
    Ok(ImmediateGroupFinalization::Released)
}

pub(crate) fn claim_next(session_id: &str) -> Result<Option<ClaimedTurn>> {
    claim_next_matching(session_id, false)
}

pub(crate) fn claim_next_group_continuation(session_id: &str) -> Result<Option<ClaimedTurn>> {
    claim_next_matching(session_id, true)
}

fn is_group_continuation(payload: &QueuedUserTurn) -> bool {
    matches!(payload.origin, Some(crate::runtime::TurnOrigin::AskAnswer { .. } | crate::runtime::TurnOrigin::GroupContinuation { .. }))
        && payload.turn_id.as_deref().is_some_and(|id| !id.is_empty())
        && payload.target_group.as_ref().zip(payload.group_activation.as_ref()).is_some_and(|(group, activation)|
            group == &activation.group_id && !activation.active_agent_ids.is_empty())
}

fn claim_next_matching(session_id: &str, continuation_only: bool) -> Result<Option<ClaimedTurn>> {
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut selected = None;
    {
        let mut statement = tx.prepare(
            "SELECT queue_id,payload_json,attempts FROM queued_user_turns
             WHERE session_id=?1 AND state='queued'
             ORDER BY enqueued_at,queue_id LIMIT 100",
        )?;
        let rows = statement.query_map([session_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;
        for row in rows {
            let (id, json, attempts) = row?;
            anyhow::ensure!(json.len() <= MAX_PAYLOAD_BYTES, "queued prompt envelope is too large");
            let payload: QueuedUserTurn = serde_json::from_str(&json).context("queued prompt is invalid JSON")?;
            if !continuation_only || is_group_continuation(&payload) {
                anyhow::ensure!(attempts >= 0 && attempts < i64::MAX, "queued prompt has an invalid attempt count");
                selected = Some((id, payload, attempts + 1));
                break;
            }
        }
    }
    let Some((queue_id, payload, attempts)) = selected else {
        tx.commit()?;
        return Ok(None);
    };
    let changed = tx.execute(
        "UPDATE queued_user_turns SET state='running',attempts=attempts+1,
            failure=NULL,updated_at=?1 WHERE queue_id=?2 AND state='queued'",
        params![Utc::now().to_rfc3339(), queue_id],
    )?;
    anyhow::ensure!(changed == 1, "queued prompt was claimed concurrently");
    tx.commit()?;
    Ok(Some(ClaimedTurn { queue_id, payload, attempts }))
}

pub(crate) fn complete(queue_id: &str) -> Result<()> {
    let connection = open()?;
    let changed = connection.execute(
        "DELETE FROM queued_user_turns WHERE queue_id=?1 AND state='running'",
        [queue_id],
    )?;
    anyhow::ensure!(changed == 1, "queued prompt is not running");
    Ok(())
}

/// The user pressed stop: the queued turns this stop aborted are finished,
/// not "interrupted work to review". Without this, the aborted row stayed
/// `running` and the next restart surfaced it as "needs review".
pub(crate) fn retire_stopped(session_id: &str, target_agent: Option<&str>) -> Result<usize> {
    let connection = open()?;
    let mut statement = connection.prepare(
        "SELECT queue_id,payload_json FROM queued_user_turns WHERE session_id=?1 AND state='running'",
    )?;
    let rows = statement
        .query_map([session_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut retired = 0;
    for (queue_id, payload) in rows {
        let owner = serde_json::from_str::<QueuedUserTurn>(&payload).ok().and_then(|turn| turn.target_agent);
        let matches = match (target_agent, owner.as_deref()) {
            (None, _) => true,
            (Some(target), Some(owner)) => crate::runtime::postbox::base_agent(owner) == target,
            (Some(_), None) => false,
        };
        if matches {
            retired += connection.execute(
                "DELETE FROM queued_user_turns WHERE queue_id=?1 AND state='running'",
                [&queue_id],
            )?;
        }
    }
    Ok(retired)
}

pub(crate) fn fail(queue_id: &str, failure: &str) -> Result<()> {
    let connection = open()?;
    let failure: String = failure.chars().take(2_000).collect();
    let changed = connection.execute(
        "UPDATE queued_user_turns SET state='failed',failure=?1,updated_at=?2
         WHERE queue_id=?3 AND state='running'",
        params![failure, Utc::now().to_rfc3339(), queue_id],
    )?;
    anyhow::ensure!(changed == 1, "queued prompt is not running");
    Ok(())
}

pub(crate) fn pending_sessions() -> Result<Vec<String>> {
    let connection = open()?;
    let mut statement = connection.prepare(
        "SELECT DISTINCT session_id FROM queued_user_turns WHERE state='queued' ORDER BY session_id",
    )?;
    let rows = statement.query_map([], |row| row.get(0))?;
    let sessions = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sessions)
}

pub(crate) fn list(session_id: &str) -> Result<Vec<QueuedTurnSummary>> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    let connection = open()?;
    let mut statement = connection.prepare(
        "SELECT queue_id,payload_json,state,enqueued_at,failure
         FROM queued_user_turns WHERE session_id=?1
         ORDER BY enqueued_at,queue_id",
    )?;
    let rows = statement.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut summaries = Vec::new();
    for row in rows {
        let (queue_id, payload_json, state, enqueued_at, failure) = row?;
        anyhow::ensure!(
            payload_json.len() <= MAX_PAYLOAD_BYTES,
            "queued prompt envelope is too large"
        );
        let payload: QueuedUserTurn =
            serde_json::from_str(&payload_json).context("queued prompt is invalid JSON")?;
        let preview_source = match payload.origin.as_ref() {
            Some(crate::runtime::TurnOrigin::AskAnswer { display, .. } | crate::runtime::TurnOrigin::GroupContinuation { display, .. }) => display,
            _ => &payload.user_request,
        };
        summaries.push(QueuedTurnSummary {
            queue_id,
            preview: preview_source
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect(),
            origin: payload.origin.clone(),
            interaction_mode: payload.interaction_mode,
            target_agent: payload.target_agent,
            target_group: payload.target_group,
            state,
            enqueued_at,
            failure,
        });
    }
    Ok(summaries)
}

/// Remove a not-yet-running prompt and return its complete envelope. Used by
/// Cancel and by the explicit "Steer now" action.
pub(crate) fn take_queued(session_id: &str, queue_id: &str) -> Result<QueuedUserTurn> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    anyhow::ensure!(
        queue_id.starts_with("queued_")
            && queue_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid queued prompt id"
    );
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let payload_json = tx
        .query_row(
            "SELECT payload_json FROM queued_user_turns
             WHERE queue_id=?1 AND session_id=?2 AND state='queued'",
            params![queue_id, session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("queued prompt `{queue_id}` is not waiting"))?;
    anyhow::ensure!(
        payload_json.len() <= MAX_PAYLOAD_BYTES,
        "queued prompt envelope is too large"
    );
    let payload = serde_json::from_str(&payload_json).context("queued prompt is invalid JSON")?;
    tx.execute(
        "DELETE FROM queued_user_turns WHERE queue_id=?1 AND session_id=?2 AND state='queued'",
        params![queue_id, session_id],
    )?;
    tx.commit()?;
    Ok(payload)
}

pub(crate) fn cancel(session_id: &str, queue_id: &str) -> Result<QueuedUserTurn> {
    crate::session::SessionStore::validate_session_id(session_id)?;
    anyhow::ensure!(
        queue_id.starts_with("queued_")
            && queue_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid queued prompt id"
    );
    let mut connection = open()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let payload_json = tx
        .query_row(
            "SELECT payload_json FROM queued_user_turns
             WHERE queue_id=?1 AND session_id=?2 AND state IN ('queued','failed')",
            params![queue_id, session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| "queued prompt is running or does not exist")?;
    anyhow::ensure!(
        payload_json.len() <= MAX_PAYLOAD_BYTES,
        "queued prompt envelope is too large"
    );
    let payload: QueuedUserTurn = serde_json::from_str(&payload_json).context("queued prompt is invalid JSON")?;
    let changed = tx.execute(
        "DELETE FROM queued_user_turns
         WHERE queue_id=?1 AND session_id=?2 AND state IN ('queued','failed')",
        params![queue_id, session_id],
    )?;
    anyhow::ensure!(changed == 1, "queued prompt is running or does not exist");
    // Cancellation settles the group's suppressing receipt before any next
    // work can wake. Both records share this database: failure rolls back the
    // removal too, leaving the exact queue row available for another review.
    if payload.target_group.is_some() {
        if let Some(turn_id) = payload.turn_id.as_deref() {
            settle_immediate_group_in(&tx, session_id, turn_id)?;
        }
    }
    tx.commit()?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_recovery_reuses_exact_envelope_without_replanning_or_reexecution() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let make = || QueuedUserTurn {
            turn_id: Some("ask_answer_fixture_0001".into()),
            user_request: "Resume Theo with 0.1 mm; preserve the saved frontier".into(),
            origin: Some(crate::runtime::TurnOrigin::AskAnswer {
                ask_id: "ask-fixture".into(), agent_id: Some("researcher".into()), display: "0.1 mm".into(),
            }),
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            workspace: Some(PathBuf::from("/tmp/original-workspace")),
            target_agent: Some("researcher".into()), target_group: None,
            group_activation: None, yolo: None, sticky_notes: None, viewport: None, attachments: None,
        };
        let original = frozen_answer_turn("answer-recovery", "ask-fixture", "0.1 mm", || Ok(make())).unwrap();
        // Simulate death after freezing, before enqueue: every API reopens SQLite.
        let recovered = frozen_answer_turn("answer-recovery", "ask-fixture", "0.1 mm", || panic!("must not read changed graph/access settings")).unwrap();
        assert_eq!(serde_json::to_value(&original).unwrap(), serde_json::to_value(&recovered).unwrap());
        let id = enqueue("answer-recovery", &recovered).unwrap();
        // Simulate death after queue write, before answer acknowledgement.
        let retry = frozen_answer_turn("answer-recovery", "ask-fixture", "0.1 mm", || panic!("must not replan after enqueue")).unwrap();
        assert_eq!(enqueue("answer-recovery", &retry).unwrap(), id);
        assert_eq!(list("answer-recovery").unwrap().len(), 1);
        assert!(frozen_answer_turn("answer-recovery", "ask-fixture", "5 mm", || Ok(make())).is_err());
        assert_eq!(claim_next("answer-recovery").unwrap().unwrap().queue_id, id);
        assert!(retired_turn_receipt("answer-recovery", retry.turn_id.as_deref().unwrap()).unwrap().is_none());
        complete(&id).unwrap();
        assert_eq!(retired_turn_receipt("answer-recovery", retry.turn_id.as_deref().unwrap()).unwrap(), Some(id.clone()));
        assert_eq!(enqueue("answer-recovery", &retry).unwrap(), id);
        assert!(claim_next("answer-recovery").unwrap().is_none());
        // Distinct conversations never share the saved answer.
        assert!(frozen_answer_turn("other-answer-recovery", "ask-fixture", "5 mm", || Ok(make())).is_ok());
    }

    #[test]
    fn answer_freeze_rechecks_a_competing_writer_and_rejects_bad_identity() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let make = |request: &str| QueuedUserTurn {
            turn_id: Some("ask_answer_competing_0001".into()), user_request: request.into(),
            origin: Some(crate::runtime::TurnOrigin::AskAnswer { ask_id: "ask-race".into(), agent_id: None, display: "yes".into() }),
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: None, yolo: None, workspace: None, target_agent: None,
            target_group: None, group_activation: None, sticky_notes: None, viewport: None, attachments: None,
        };
        let winner = frozen_answer_turn("answer-race", "ask-race", "yes", || {
            frozen_answer_turn("answer-race", "ask-race", "yes", || Ok(make("first writer")))?;
            Ok(make("stale planner"))
        }).unwrap();
        assert_eq!(winner.user_request, "first writer");
        assert!(frozen_answer_turn("answer-invalid", "ask-other", "yes", || Ok(make("wrong ask"))).is_err());
    }

    #[test]
    fn queue_is_fifo_and_recovers_interrupted_claims() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let payload = |message: &str| QueuedUserTurn {
            turn_id: None,
            user_request: message.into(),
            origin: None,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: Some(PathBuf::from("/tmp/work")),
            target_agent: Some("coder".into()),
            target_group: None,
            group_activation: None,
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };
        let first = enqueue("agent-coder", &payload("first")).unwrap();
        let second = enqueue("agent-coder", &payload("second")).unwrap();
        let summaries = list("agent-coder").unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].preview, "first");
        assert_eq!(summaries[1].interaction_mode, crate::runtime::InteractionMode::Execute);
        let claimed = claim_next("agent-coder").unwrap().unwrap();
        assert_eq!(claimed.queue_id, first);
        assert_eq!(claimed.payload.user_request, "first");
        assert_eq!(claimed.attempts, 1);
        assert_eq!(recover_interrupted().unwrap(), 1);
        let claimed = claim_next("agent-coder").unwrap().unwrap();
        assert_eq!(claimed.queue_id, first);
        assert_eq!(claimed.attempts, 2);
        assert_eq!(open().unwrap().query_row(
            "SELECT attempts FROM queued_user_turns WHERE queue_id=?1", [&first],
            |row| row.get::<_, i64>(0),
        ).unwrap(), claimed.attempts);
        complete(&first).unwrap();
        let second_claim = claim_next("agent-coder").unwrap().unwrap();
        assert_eq!(second_claim.queue_id, second);
        assert_eq!(second_claim.attempts, 1);
        assert_eq!(second_claim.payload.interaction_mode, crate::runtime::InteractionMode::Execute);
    }

    #[test]
    fn client_turn_id_enqueues_once_and_rejects_payload_substitution() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let mut payload = QueuedUserTurn {
            turn_id: Some("turn_canvas_00000001".into()),
            user_request: "send this once".into(),
            origin: None,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: Some(PathBuf::from("/tmp/work")),
            target_agent: Some("coder".into()),
            target_group: None,
            group_activation: None,
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };
        let first = enqueue("agent-coder", &payload).unwrap();
        let retry = enqueue("agent-coder", &payload).unwrap();
        assert_eq!(retry, first);
        assert_eq!(list("agent-coder").unwrap().len(), 1);

        payload.user_request = "substituted payload".into();
        let error = enqueue("agent-coder", &payload).unwrap_err().to_string();
        assert!(error.contains("different queued payload"));
    }

    #[test]
    fn immediate_group_receipt_is_durable_and_payload_bound() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let mut payload = QueuedUserTurn {
            turn_id: Some("turn_group_00000001".into()),
            user_request: "@planner ship it".into(),
            origin: None,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: Some(PathBuf::from("/tmp/work")),
            target_agent: None,
            target_group: Some("launch-room".into()),
            group_activation: None,
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };
        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_00000001", &payload).unwrap(),
            ImmediateGroupReservation::New
        );
        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_00000001", &payload).unwrap(),
            ImmediateGroupReservation::ExistingInFlight
        );
        release_immediate_group("group-launch-room", "turn_group_00000001").unwrap();
        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_00000001", &payload).unwrap(),
            ImmediateGroupReservation::Recovered
        );
        settle_immediate_group("group-launch-room", "turn_group_00000001").unwrap();
        // An answer can retire the parent receipt while the parent's sibling
        // is still running. Late parent cleanup must not reopen or reject it.
        release_immediate_group("group-launch-room", "turn_group_00000001").unwrap();
        assert!(release_immediate_group("group-launch-room", "missing-turn").is_err());
        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_00000001", &payload).unwrap(),
            ImmediateGroupReservation::ExistingSettled
        );
        payload.user_request = "@planner do something else".into();
        assert!(
            reserve_immediate_group("group-launch-room", "turn_group_00000001", &payload).is_err()
        );
    }

    #[test]
    fn canceling_a_queued_group_turn_settles_its_immediate_receipt() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let payload = QueuedUserTurn {
            turn_id: Some("turn_group_cancel_0001".into()),
            user_request: "@planner ship it".into(),
            origin: None,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: Some(PathBuf::from("/tmp/work")),
            target_agent: None,
            target_group: Some("launch-room".into()),
            group_activation: None,
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };

        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_cancel_0001", &payload)
                .unwrap(),
            ImmediateGroupReservation::New
        );
        let queue_id = enqueue("group-launch-room", &payload).unwrap();
        let canceled = cancel("group-launch-room", &queue_id).unwrap();
        assert_eq!(canceled.turn_id.as_deref(), Some("turn_group_cancel_0001"));
        assert_eq!(canceled.target_group.as_deref(), Some("launch-room"));
        assert_eq!(reserve_immediate_group("group-launch-room", "turn_group_cancel_0001", &payload).unwrap(),
            ImmediateGroupReservation::ExistingSettled,
            "cancellation commits group settlement with removal");
        settle_immediate_group("group-launch-room", "turn_group_cancel_0001").unwrap();

        assert!(list("group-launch-room").unwrap().is_empty());
        assert_eq!(
            reserve_immediate_group("group-launch-room", "turn_group_cancel_0001", &payload)
                .unwrap(),
            ImmediateGroupReservation::ExistingSettled
        );
    }

    #[test]
    fn group_activation_intent_survives_the_durable_queue_round_trip() {
        use crate::runtime::group_conversation::{
            GroupActivationIntent, GroupActivationSelection, GroupExecutionMode,
        };

        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let activation = GroupActivationIntent {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "launch-room".to_string(),
            roster_fingerprint: "roster-v1".to_string(),
            selection: GroupActivationSelection::Everyone,
            active_agent_ids: vec!["planner".to_string(), "coder".to_string()],
            execution_mode: GroupExecutionMode::Parallel,
            execution_dependencies: None,
            execution_waves: vec![vec!["planner".to_string(), "coder".to_string()]],
        };
        let payload = QueuedUserTurn {
            turn_id: Some("turn_group_launch_0001".to_string()),
            user_request: "@everyone ship it".to_string(),
            origin: None,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            permission_mode: Some(crate::tools::PermissionMode::Workspace),
            yolo: None,
            workspace: Some(PathBuf::from("/tmp/work")),
            target_agent: None,
            target_group: Some("launch-room".to_string()),
            group_activation: Some(activation.clone()),
            sticky_notes: None,
            viewport: None,
            attachments: None,
        };

        let queue_id = enqueue("group-launch-room", &payload).unwrap();
        let summaries = list("group-launch-room").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].queue_id, queue_id);
        assert_eq!(summaries[0].target_group.as_deref(), Some("launch-room"));

        // An internal answer/dependency continuation may bypass an earlier
        // ordinary send, without consuming or reordering ordinary FIFO rows.
        assert!(!is_group_continuation(&payload));
        let mut continuation = payload.clone();
        continuation.turn_id = Some("continuation_launch_0001".into());
        continuation.origin = Some(crate::runtime::TurnOrigin::GroupContinuation {
            original_turn_id: "original-launch".into(), display: "Iris continuing".into(),
        });
        let continuation_id = enqueue("group-launch-room", &continuation).unwrap();
        assert_eq!(claim_next_group_continuation("group-launch-room").unwrap().unwrap().queue_id, continuation_id);
        assert!(claim_next_group_continuation("group-launch-room").unwrap().is_none(), "a running continuation cannot be claimed twice");
        complete(&continuation_id).unwrap();
        continuation.target_group = Some("wrong-room".into());
        assert!(!is_group_continuation(&continuation));

        let claimed = claim_next("group-launch-room").unwrap().unwrap();
        assert_eq!(claimed.queue_id, queue_id);
        assert_eq!(claimed.payload.target_group.as_deref(), Some("launch-room"));
        assert_eq!(claimed.payload.group_activation, Some(activation));
        complete(&queue_id).unwrap();
    }

    #[test]
    fn late_answer_summary_never_exposes_the_internal_continuation() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let origin = crate::runtime::TurnOrigin::AskAnswer {
            ask_id: "ask-example".into(),
            agent_id: Some("researcher".into()),
            display: "Research Grok groups and compare their features.".into(),
        };
        enqueue(
            "agent-researcher",
            &QueuedUserTurn {
                turn_id: None,
                user_request: "[late ask answer] internal wake instructions".into(),
                origin: Some(origin.clone()),
                interaction_mode: crate::runtime::InteractionMode::Execute,
                permission_mode: None,
                yolo: None,
                workspace: None,
                target_agent: Some("researcher".into()),
                target_group: None,
                group_activation: None,
                sticky_notes: None,
                viewport: None,
                attachments: None,
            },
        )
        .unwrap();
        let summaries = list("agent-researcher").unwrap();
        assert_eq!(
            summaries[0].preview,
            "Research Grok groups and compare their features."
        );
        assert_eq!(summaries[0].origin, Some(origin));
    }
}
