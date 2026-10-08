//! Cross-turn background-agent postbox.
//!
//! Mode-2 `talk` from the orchestrator spawns a specialist as a DETACHED task
//! in the gateway daemon: the user's turn finishes normally and the specialist
//! keeps working. This module is where those jobs live between turns:
//!
//! - `job_started` / `job_finished` track running work and hold finished
//!   results until the orchestrator can absorb them;
//! - `take_ready` drains finished results — the mesh calls it between provider
//!   rounds (a result landing mid-task is injected into the orchestrator's
//!   context so it notices, finishes the current step, then handles it) and
//!   the runner calls it at turn start (results that arrived while idle);
//! - `subscribe` registers a live event channel (the TUI's Subscribe wire
//!   connection) so spawn/return render immediately even when no turn is
//!   running.
//!
//! Completed worker bodies and delivery claims are durable in CompanyStore;
//! recovery rehydrates interrupted dead-owner deliveries. Active execution,
//! subscribers, and the bounded foreground event replay remain in-process.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::runtime::CliEvent;

const FOREGROUND_REPLAY_CAP: usize = 512;
const JOURNAL_BOUNDARY_CAP: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExecutionScope {
    pub turn_id: String,
    pub task_id: String,
    pub attempt_id: String,
}

impl ExecutionScope {
    pub fn new(turn_id: String, task_id: String) -> Self {
        Self { turn_id, task_id, attempt_id: uuid::Uuid::new_v4().to_string() }
    }
}

#[derive(Debug, Clone)]
pub struct JournalEvent {
    pub scope: Option<ExecutionScope>,
    pub event: CliEvent,
    pub sequence: u64,
}
const RETURN_RETRY_DELAYS_MS: [u64; 5] = [100, 500, 2_000, 5_000, 15_000];
const TERMINAL_RETURN_RECEIPT_CAP: usize = 512;

/// A background specialist currently working.
#[derive(Debug, Clone)]
pub struct RunningJob {
    pub agent: String,
    pub subject: String,
    pub handoff_id: String,
    pub causation_id: Option<String>,
    pub started: chrono::DateTime<chrono::Utc>,
}

/// One item-scoped worker from an inline `volume_work` batch. This is held
/// only while it is active so reconnecting Environment panels can reconstruct
/// their compact working chips without creating a durable handoff/history row.
#[derive(Debug, Clone)]
struct RunningVolumeWorker {
    batch_id: String,
    worker_id: String,
    label: String,
    item_id: String,
}

impl RunningVolumeWorker {
    fn started_event(&self) -> CliEvent {
        CliEvent::VolumeWorkerLifecycle {
            batch_id: self.batch_id.clone(),
            worker_id: self.worker_id.clone(),
            label: self.label.clone(),
            item_id: self.item_id.clone(),
            status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
        }
    }
}

/// A finished background job whose result has not yet been absorbed by the
/// orchestrator.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReturnKind {
    #[default]
    Specialist,
    Terminal,
}

impl ReturnKind {
    pub fn as_str(self) -> &'static str {
        match self { Self::Specialist => "specialist", Self::Terminal => "terminal" }
    }
    pub fn from_stored(value: &str) -> anyhow::Result<Self> {
        match value { "specialist" => Ok(Self::Specialist), "terminal" => Ok(Self::Terminal),
            _ => anyhow::bail!("unknown return kind {value:?}") }
    }
}

/// Terminal completion is a user-authorized continuation, distinct from a
/// coworker return and from a new user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeRequest {
    UserSteer(String),
    TerminalCompletion(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedJob {
    #[serde(default)]
    pub kind: ReturnKind,
    /// Stable delivery receipt. Empty on construction; `job_finished` mints
    /// it before the result becomes observable or durable.
    #[serde(default)]
    pub delivery_id: String,
    /// Durable event/message that caused the handoff. Kept through the SQLite
    /// return queue so a restart cannot sever lifecycle lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    pub agent: String,
    pub subject: String,
    pub ok: bool,
    /// One-line outcome for activity rows.
    pub summary: String,
    /// The full result markdown the orchestrator receives in context.
    pub body: String,
    pub finished: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
struct PendingCompletedJob {
    generation: u64,
    job: CompletedJob,
}

/// A mid-task message for a WORKING specialist (orchestrator `talk` with
/// a talk to a busy target) — injected into the target's running turn at its next round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerNote {
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub body: String,
}

#[derive(Default)]
struct SessionBox {
    /// Incremented whenever the user permanently discards this session's live
    /// work. An already-cloned retry from an older generation is rejected
    /// before it can recreate a purged durable return.
    generation: u64,
    running: Vec<RunningJob>,
    /// Active inline volume items, keyed by their disposable worker scope.
    /// Unlike `running`, these are not durable background jobs and never have
    /// a return receipt; they exist solely for live/reconnect worker chips.
    volume_workers: HashMap<String, RunningVolumeWorker>,
    /// Cancellation and visible lifecycle share this same session mutex. A
    /// stop can therefore never claim a handle between handle registration
    /// and publication of its `RunningJob` row (or settle a later spawn).
    background_handles: HashMap<String, BackgroundAbortHandle>,
    /// Detached jobs that have been registered but have not yet crossed into
    /// `ActiveTurnGuard` ownership. Kept separate from `running`: the latter
    /// deliberately spans a whole baton chain, while a start marker covers
    /// only the spawn -> first live turn scheduling gap.
    starting_turns: HashMap<String, HashMap<String, String>>,
    active_turns: HashMap<String, usize>,
    ready: Vec<CompletedJob>,
    /// Full completed bodies whose durable staging or publication failed.
    /// They remain non-observable and the corresponding `running` row remains
    /// live until an automatic retry crosses the durable boundary.
    pending_persist: Vec<PendingCompletedJob>,
    return_retry_scheduled: bool,
    /// Bounded in-process terminal receipts suppress duplicate monitor/cancel
    /// completions even after the durable delivery row has been acknowledged.
    terminal_returns: VecDeque<CompletedJob>,
    subscribers: Vec<mpsc::UnboundedSender<CliEvent>>,
    journal_subscribers: Vec<mpsc::UnboundedSender<JournalEvent>>,
    journal_sequence: u64,
    /// Bounded raw events for the current/most-recent foreground turn. A
    /// desktop can leave a conversation without stopping its agent, then
    /// subscribe again and rebuild every missed reasoning/tool row in order.
    foreground_replay: VecDeque<JournalEvent>,
    /// The authored boundary is not part of the bounded event deque. A long
    /// routine may legitimately emit more than `FOREGROUND_REPLAY_CAP` events;
    /// reconnecting must still begin with the Routine/User row that owns them.
    foreground_boundary: Option<JournalEvent>,
    task_boundaries: VecDeque<JournalEvent>,
    completed_scopes: std::collections::HashSet<ExecutionScope>,
    steer: HashMap<String, Vec<SteerNote>>,
    /// Client turn ids of user messages already accepted as mid-turn steers
    /// (bounded). A reconnect retry of the same composer send must not be
    /// injected twice.
    user_steer_ids: VecDeque<String>,
}

fn replayable(event: &CliEvent) -> bool {
    !matches!(
        event,
        CliEvent::Thinking
            | CliEvent::StreamDelta { .. }
            | CliEvent::ContextUsage { .. }
            | CliEvent::LibrarianPass { .. }
            | CliEvent::PermissionCheck { .. }
            | CliEvent::PromptAssembled { .. }
            | CliEvent::SessionResolved { .. }
            | CliEvent::BackgroundResultsAbsorbed { .. }
            // Reconnect receives only the currently active started signals
            // from `SessionBox::volume_workers`, never stale history.
            | CliEvent::VolumeWorkerLifecycle { .. }
    )
}

fn publish(sbox: &mut SessionBox, event: &CliEvent) {
    publish_owned(sbox, event, None);
}

fn publish_owned(sbox: &mut SessionBox, event: &CliEvent, scope: Option<&ExecutionScope>) -> JournalEvent {
    sbox.journal_sequence = sbox.journal_sequence.saturating_add(1);
    let journal = JournalEvent { scope: scope.cloned(), event: event.clone(), sequence: sbox.journal_sequence };
    if matches!(event, CliEvent::Done | CliEvent::TerminalFailure { .. }) {
        if let Some(scope) = scope { sbox.completed_scopes.insert(scope.clone()); }
    }
    if matches!(event, CliEvent::WakeTurn { .. }) {
        if scope.is_some() {
            sbox.task_boundaries.retain(|entry| entry.scope != journal.scope);
            if sbox.task_boundaries.len() >= JOURNAL_BOUNDARY_CAP { sbox.task_boundaries.pop_front(); }
            sbox.task_boundaries.push_back(journal.clone());
        } else {
            sbox.foreground_boundary = Some(journal.clone());
        }
    } else if replayable(event) {
        if sbox.foreground_replay.len() >= FOREGROUND_REPLAY_CAP {
            sbox.foreground_replay.pop_front();
        }
        sbox.foreground_replay.push_back(journal.clone());
    }
    sbox.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    sbox.journal_subscribers.retain(|tx| tx.send(journal.clone()).is_ok());
    journal
}

enum BackgroundAbortHandle {
    Future(futures_util::future::AbortHandle),
    #[cfg(test)]
    Tokio(tokio::task::AbortHandle),
}

impl BackgroundAbortHandle {
    fn abort(self) {
        match self {
            Self::Future(handle) => handle.abort(),
            #[cfg(test)]
            Self::Tokio(handle) => handle.abort(),
        }
    }
}

fn state() -> &'static Mutex<HashMap<String, SessionBox>> {
    static STATE: OnceLock<Mutex<HashMap<String, SessionBox>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register one detached job. Parallel jobs retain their instance suffix
/// (`coder#2`) so either monitor can claim its own completion without removing
/// a sibling's handle.
#[cfg(test)]
pub fn register_background_handle(session_id: &str, agent: &str, handle: tokio::task::AbortHandle) {
    with_box(session_id, |sbox| {
        sbox.background_handles
            .insert(agent.to_string(), BackgroundAbortHandle::Tokio(handle));
    });
}

/// Atomically publish a detached job's start marker, visible running row, and
/// cancellation handle before its future is spawned. The returned guard moves
/// into that future and is consumed by its first [`ActiveTurnGuard`].
pub fn register_background_job(
    session_id: &str,
    agent: &str,
    instance: &str,
    subject: &str,
    handoff_id: &str,
    causation_id: Option<&str>,
    handle: futures_util::future::AbortHandle,
) -> StartingTurnGuard {
    let agent = base_agent(agent).to_string();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let event = CliEvent::BackgroundAgentSpawned {
        agent: instance.to_string(),
        subject: subject.to_string(),
        handoff_id: handoff_id.to_string(),
        requester: "orchestrator".to_string(),
        receiver: instance.to_string(),
        status: "queued".to_string(),
        causation_id: causation_id.map(str::to_string),
    };
    with_box(session_id, |sbox| {
        sbox.starting_turns
            .entry(agent.clone())
            .or_default()
            .insert(token.clone(), instance.to_string());
        let replaced = sbox
            .background_handles
            .insert(instance.to_string(), BackgroundAbortHandle::Future(handle));
        assert!(
            replaced.is_none(),
            "duplicate background instance registered: {instance}"
        );
        sbox.running.push(RunningJob {
            agent: instance.to_string(),
            subject: subject.to_string(),
            handoff_id: handoff_id.to_string(),
            causation_id: causation_id.map(str::to_string),
            started: chrono::Utc::now(),
        });
        // Publish every observable start while the same lifecycle lock is
        // held. A concurrent stop cannot emit a return before subscribers,
        // the company ledger, and the journal have observed the spawn.
        crate::runtime::company::mirror_job_started(session_id, instance, subject);
        crate::runtime::journal::record_handoff(
            session_id,
            handoff_id,
            "orchestrator",
            instance,
            subject,
            "queued",
            None,
            causation_id,
        );
        publish(sbox, &event);
    });
    tracing::info!("background {instance} spawned [{session_id}]: {subject}");
    StartingTurnGuard {
        session_id: session_id.to_string(),
        agent,
        token,
    }
}

/// Claim natural completion of a detached job. `false` means `/stop` already
/// claimed and reported it, so the monitor must not emit a duplicate return.
pub fn claim_background_completion(session_id: &str, agent: &str) -> bool {
    with_box(session_id, |sbox| {
        sbox.background_handles.remove(agent).is_some()
    })
}

/// Stop the detached work represented by one specialist window and settle its
/// postbox state. Parallel instances share that window, so all matching
/// instance handles are stopped; unrelated specialists remain untouched.
/// Returns false when runtime truth says that agent has no live background job.
pub fn cancel_background_agent(session_id: &str, agent: &str) -> bool {
    cancel_background_agent_with_reason(
        session_id,
        agent,
        "stop requested by user",
        "The specialist turn was cancelled from its Phoenix conversation. Cooperative cancellation was requested for any in-flight tool, but a non-cooperative blocking action may still finish after this receipt; verify external state before repeating the action.",
    )
}

fn cancel_background_agent_with_reason(
    session_id: &str,
    agent: &str,
    summary: &str,
    body: &str,
) -> bool {
    let base = base_agent(agent).to_string();
    let (handles, running) = with_box(session_id, |sbox| {
        let keys: Vec<String> = sbox
            .background_handles
            .keys()
            .filter(|instance| base_agent(instance) == base)
            .cloned()
            .collect();
        let key_set: std::collections::HashSet<String> = keys.iter().cloned().collect();
        let running = sbox
            .running
            .iter()
            .filter(|job| key_set.contains(&job.agent))
            .cloned()
            .collect::<Vec<_>>();
        let handles = keys
            .into_iter()
            .filter_map(|key| sbox.background_handles.remove(&key))
            .collect::<Vec<_>>();
        (handles, running)
    });
    if handles.is_empty() {
        return false;
    }
    for handle in handles {
        handle.abort();
    }
    for running in running {
        job_finished(
            session_id,
            CompletedJob {
                kind: crate::runtime::postbox::ReturnKind::Specialist,
                delivery_id: running.handoff_id,
                causation_id: running.causation_id,
                agent: running.agent,
                subject: running.subject,
                ok: false,
                summary: summary.to_string(),
                body: body.to_string(),
                finished: chrono::Utc::now(),
            },
        );
    }
    true
}

/// Pause every detached instance of one coworker across canonical agent and
/// group threads. Lifecycle changes use this after the durable directory
/// transaction commits; unrelated agents and foreground user turns are never
/// touched.
pub fn pause_background_agent_everywhere(agent: &str) -> usize {
    let base = base_agent(agent).to_string();
    let session_ids = {
        let map = state().lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .filter(|(_, sbox)| {
                sbox.background_handles
                    .keys()
                    .any(|instance| base_agent(instance) == base)
            })
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>()
    };
    session_ids
        .iter()
        .filter(|session_id| {
            cancel_background_agent_with_reason(
                session_id,
                &base,
                "coworker paused by lifecycle change",
                "This work was paused because the coworker was archived, disabled, or scheduled for deletion. Phoenix preserved the transcript and durable work receipts; resume or explicitly reroute the owner before continuing.",
            )
        })
        .count()
}

/// Pause all detached coworker work rooted in one canonical group thread.
pub fn pause_background_session(session_id: &str) -> usize {
    let agents = {
        let map = state().lock().unwrap_or_else(|p| p.into_inner());
        map.get(session_id)
            .map(|sbox| {
                sbox.background_handles
                    .keys()
                    .map(|instance| base_agent(instance).to_string())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default()
    };
    agents
        .iter()
        .filter(|agent| {
            cancel_background_agent_with_reason(
                session_id,
                agent,
                "group paused by lifecycle change",
                "This work was paused because its group was archived or scheduled for deletion. Phoenix preserved the canonical group transcript and durable receipts; restore the group before continuing.",
            )
        })
        .count()
}

/// Idle-wake seam for user-authored steering that lands at a live-turn edge.
/// Background completion deliberately does not ping this channel: its result
/// remains durable for the next natural turn instead of creating an
/// unsolicited second owner answer. Unset in tests/no-gateway runs is safe.
fn wake_notifier() -> &'static Mutex<Option<mpsc::UnboundedSender<WakeRequest>>> {
    static WAKE: OnceLock<Mutex<Option<mpsc::UnboundedSender<WakeRequest>>>> = OnceLock::new();
    WAKE.get_or_init(|| Mutex::new(None))
}

/// Register the daemon's wake channel (replaces any previous one).
pub fn set_wake_notifier(tx: mpsc::UnboundedSender<WakeRequest>) {
    *wake_notifier().lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
}

pub fn wake_available() -> bool {
    wake_notifier().lock().unwrap_or_else(|p|p.into_inner()).as_ref().is_some_and(|tx|!tx.is_closed())
}

#[cfg(test)]
fn wake_receipts() -> &'static Mutex<HashMap<String, usize>> {
    static RECEIPTS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    RECEIPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn wake_receipt_count(session_id: &str) -> usize {
    wake_receipts()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(session_id)
        .copied()
        .unwrap_or(0)
}

/// Undelivered results waiting for the orchestrator? (Peek — `take_ready` drains.)
pub fn has_ready(session_id: &str) -> bool {
    if with_box(session_id, |sbox| !sbox.ready.is_empty()) {
        return true;
    }
    crate::runtime::company::global()
        .and_then(|store| store.peek_job_returns(session_id))
        .is_ok_and(|jobs| !jobs.is_empty())
}

/// Only an explicitly managed terminal completion may automatically resume
/// an idle direct conversation. Ordinary coworker returns keep their policy.
pub fn has_terminal_ready(session_id: &str) -> bool {
    let pending = |job: &CompletedJob| job.kind == ReturnKind::Terminal
        && crate::tools::terminal_jobs::receipt_pending(session_id, job).unwrap_or(false);
    with_box(session_id, |sbox| sbox.ready.iter().any(pending))
        || crate::runtime::company::global()
            .and_then(|store| store.peek_job_returns(session_id))
            .is_ok_and(|jobs| jobs.iter().any(pending))
}

/// Does this session still have detached work to finish or completed work
/// waiting to be integrated? The two queues must be inspected under one lock:
/// separately peeking `running` and `ready` can observe the gap while
/// `job_finished` moves a job between them and incorrectly report quiescence.
pub fn has_background_work(session_id: &str) -> bool {
    let (live, retryable) = with_box(session_id, |sbox| {
        (
            !sbox.running.is_empty() || !sbox.ready.is_empty() || !sbox.pending_persist.is_empty(),
            !sbox.pending_persist.is_empty(),
        )
    });
    if retryable {
        schedule_return_retry(session_id);
    }
    live || crate::runtime::company::global()
        .and_then(|store| store.peek_job_returns(session_id))
        .is_ok_and(|jobs| !jobs.is_empty())
}

/// RAII receipt for one live specialist turn. Dropping it on every return,
/// error, or unwind keeps busy-target routing tied to real lane ownership
/// rather than a stale background-job label.
pub struct ActiveTurnGuard {
    session_id: String,
    agent: String,
}

/// RAII receipt for the spawn -> first-live-turn scheduling gap.
///
/// The token is registered synchronously before `tokio::spawn`, then moved
/// into the detached runner. Its exact token is consumed atomically when that
/// runner acquires [`ActiveTurnGuard`]. If the future is dropped first (spawn
/// unwind, cancellation, or abort while waiting for the lane lock), `Drop`
/// removes it so a dead job can never remain steerable.
pub struct StartingTurnGuard {
    session_id: String,
    agent: String,
    token: String,
}

fn remove_starting_token(sbox: &mut SessionBox, agent: &str, token: &str) {
    let remove_agent = if let Some(tokens) = sbox.starting_turns.get_mut(agent) {
        tokens.remove(token);
        tokens.is_empty()
    } else {
        false
    };
    if remove_agent {
        sbox.starting_turns.remove(agent);
    }
}

fn remove_starting_instance(sbox: &mut SessionBox, instance: &str) {
    let base = base_agent(instance).to_string();
    let remove_agent = if let Some(tokens) = sbox.starting_turns.get_mut(&base) {
        tokens.retain(|_, registered_instance| registered_instance != instance);
        tokens.is_empty()
    } else {
        false
    };
    if remove_agent {
        sbox.starting_turns.remove(&base);
    }
}

impl Drop for StartingTurnGuard {
    fn drop(&mut self) {
        with_box(&self.session_id, |sbox| {
            remove_starting_token(sbox, &self.agent, &self.token);
        });
    }
}

/// Register a detached specialist that is scheduled but not active yet.
pub fn starting_turn_guard(session_id: &str, agent: &str, instance: &str) -> StartingTurnGuard {
    let agent = base_agent(agent).to_string();
    let token = uuid::Uuid::new_v4().simple().to_string();
    with_box(session_id, |sbox| {
        sbox.starting_turns
            .entry(agent.clone())
            .or_default()
            .insert(token.clone(), instance.to_string());
    });
    StartingTurnGuard {
        session_id: session_id.to_string(),
        agent,
        token,
    }
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        with_box(&self.session_id, |sbox| {
            if let Some(count) = sbox.active_turns.get_mut(&self.agent) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    sbox.active_turns.remove(&self.agent);
                }
            }
        });
    }
}

pub fn active_turn_guard(session_id: &str, agent: &str) -> ActiveTurnGuard {
    active_turn_guard_from_start(session_id, agent, None)
}

/// Register live ownership and consume this detached job's exact start marker
/// under the same postbox lock. There is therefore no observable instant when
/// the target is neither starting nor active.
pub fn active_turn_guard_from_start(
    session_id: &str,
    agent: &str,
    starting: Option<&StartingTurnGuard>,
) -> ActiveTurnGuard {
    let agent = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        *sbox.active_turns.entry(agent.clone()).or_default() += 1;
        if let Some(starting) =
            starting.filter(|starting| starting.session_id == session_id && starting.agent == agent)
        {
            remove_starting_token(sbox, &agent, &starting.token);
        }
    });
    ActiveTurnGuard {
        session_id: session_id.to_string(),
        agent,
    }
}

/// Has a detached job been registered for this lane but not acquired its live
/// turn guard yet?
pub fn agent_turn_starting(session_id: &str, agent: &str) -> bool {
    let agent = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        sbox.starting_turns
            .get(&agent)
            .is_some_and(|tokens| !tokens.is_empty())
    })
}

pub fn agent_turn_active(session_id: &str, agent: &str) -> bool {
    let agent = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        sbox.active_turns
            .get(&agent)
            .is_some_and(|count| *count > 0)
    })
}

/// Pending steer notes waiting for `agent`? (Peek — `take_steer` drains.) The
/// idle-wake loop treats a pending steer to the orchestrator as a reason to
/// fire a wake: a user message delivered as a steer while a turn was live would
/// otherwise be lost if that turn ended before its next round-top drain. Unlike
/// a `ready` job the note never sat in `sbox.ready`, so `has_ready` alone can't
/// see it — this is the second wake trigger that guarantees the message lands.
pub fn has_pending_steer(session_id: &str, agent: &str) -> bool {
    let base = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        sbox.steer
            .get(&base)
            .is_some_and(|notes| notes.iter().any(|note| !is_room_steer(note)))
    })
}

/// Move every ORPHANED agent-lane steer into the canonical conversation
/// owner's lane, and return how many were moved. A wake runs that owner (Avery
/// in Avery's thread, Phoenix in Phoenix's), so hard-coding `orchestrator` here
/// can both lose the note and manufacture a Phoenix→owner self-handoff.
/// Exactly-once, and it never steals from a live specialist: the wake holds the
/// session lock, so a live FOREGROUND specialist turn already drained its own
/// lane before the lock freed; a live BACKGROUND job runs without the session
/// lock but shows in `starting_turns` or `active_turns`, so its lane is skipped
/// here without treating a baton-spanning `running` label as live ownership.
pub fn rehome_orphan_steers(session_id: &str, owner: &str) -> usize {
    let owner = base_agent(owner).to_string();
    with_box(session_id, |sbox| {
        let live: std::collections::HashSet<String> = sbox
            .active_turns
            .keys()
            .chain(sbox.starting_turns.keys())
            .cloned()
            .collect();
        let mut orphans = Vec::new();
        for (base, notes) in sbox.steer.iter_mut() {
            if base == &owner || base.starts_with("__group_queue_") || live.contains(base) {
                continue;
            }
            // Group-room deliveries belong to their exact member lane; the
            // room watcher settles them (start a turn or retire an FYI).
            let (room, other): (Vec<_>, Vec<_>) =
                notes.drain(..).partition(is_room_steer);
            *notes = room;
            orphans.extend(other);
        }
        sbox.steer.retain(|_, notes| !notes.is_empty());
        let moved = orphans.len();
        if moved > 0 {
            sbox.steer.entry(owner).or_default().extend(orphans);
        }
        moved
    })
}

/// Compatibility wrapper for callers/tests that truly own a Phoenix thread.
pub fn rehome_orphan_steers_to_orchestrator(session_id: &str) -> usize {
    rehome_orphan_steers(session_id, "orchestrator")
}

fn with_box<R>(session_id: &str, f: impl FnOnce(&mut SessionBox) -> R) -> R {
    let mut map = state().lock().unwrap_or_else(|p| p.into_inner());
    f(map.entry(session_id.to_string()).or_default())
}

/// Register a live event channel for a session (the TUI's Subscribe
/// connection). Replays the current state to the new subscriber — running jobs
/// as spawn events, undelivered results as return events — so a freshly opened
/// TUI shows what the mesh is doing right now. Dead senders are pruned on the
/// next notify.
pub fn subscribe(session_id: &str, tx: mpsc::UnboundedSender<CliEvent>) {
    with_box(session_id, |sbox| {
        for job in &sbox.running {
            let _ = tx.send(CliEvent::BackgroundAgentSpawned {
                agent: job.agent.clone(),
                subject: job.subject.clone(),
                handoff_id: job.handoff_id.clone(),
                requester: "orchestrator".to_string(),
                receiver: job.agent.clone(),
                status: "working".to_string(),
                causation_id: job.causation_id.clone(),
            });
        }
        for job in sbox.ready.iter().filter(|job|job.kind==ReturnKind::Specialist) {
            let _ = tx.send(CliEvent::BackgroundAgentReturned {
                agent: job.agent.clone(),
                subject: job.subject.clone(),
                ok: job.ok,
                summary: job.summary.clone(),
                body: job.body.clone(),
                handoff_id: job.delivery_id.clone(),
                requester: "orchestrator".to_string(),
                receiver: job.agent.clone(),
                status: if job.ok { "done" } else { "blocked" }.to_string(),
                reply_to: Some(job.delivery_id.clone()),
                causation_id: job.causation_id.clone(),
            });
        }
        let mut workers = sbox.volume_workers.values().cloned().collect::<Vec<_>>();
        workers.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
        for worker in workers {
            let _ = tx.send(worker.started_event());
        }
        sbox.subscribers.push(tx);
    });
}

/// Atomically attach a calm-journal subscriber and return the bounded raw
/// foreground replay that predates its registration. Events published after
/// registration queue on `tx`, so callers can send replay -> Pong -> live with
/// no snapshot gap and no reordered rows.
pub fn subscribe_journal(session_id: &str, tx: mpsc::UnboundedSender<JournalEvent>) -> Vec<JournalEvent> {
    with_box(session_id, |sbox| {
        let mut replay = sbox.volume_workers.values().cloned().collect::<Vec<_>>();
        replay.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
        let mut replay = replay
            .into_iter()
            .map(|worker| JournalEvent { scope: None, event: worker.started_event(), sequence: 0 })
            .collect::<Vec<_>>();
        replay.extend(
            sbox.foreground_boundary
                .iter()
                .chain(sbox.task_boundaries.iter())
                .chain(sbox.foreground_replay.iter())
                .cloned(),
        );
        replay.sort_by_key(|entry| entry.sequence);
        sbox.journal_subscribers.push(tx);
        replay
    })
}

/// Start a new foreground-turn replay window. The previous completed turn is
/// already durable in the canonical session and desktop feed; retaining it in
/// memory would only replay stale rows into the next turn.
pub fn begin_foreground_replay(session_id: &str) {
    with_box(session_id, |sbox| {
        sbox.foreground_boundary = None;
        sbox.foreground_replay.retain(|entry| entry.scope.as_ref().is_some_and(|scope| !sbox.completed_scopes.contains(scope)));
        sbox.task_boundaries.retain(|entry| entry.scope.as_ref().is_some_and(|scope| !sbox.completed_scopes.contains(scope)));
        sbox.completed_scopes.clear();
    });
}

/// Permanently discard every in-memory execution residue for one canonical
/// conversation before a user-requested transcript deletion is committed.
/// Subscribers remain attached, but no stopped tool/reasoning row, detached
/// return, or steer can replay the deleted turn back into the conversation.
pub fn discard_session_work(session_id: &str) {
    let handles = with_box(session_id, |sbox| {
        sbox.generation = sbox.generation.wrapping_add(1);
        sbox.foreground_replay.clear();
        sbox.foreground_boundary = None;
        sbox.task_boundaries.clear();
        sbox.completed_scopes.clear();
        sbox.running.clear();
        sbox.volume_workers.clear();
        sbox.starting_turns.clear();
        sbox.active_turns.clear();
        sbox.ready.clear();
        sbox.pending_persist.clear();
        sbox.return_retry_scheduled = false;
        sbox.terminal_returns.clear();
        sbox.steer.clear();
        sbox.background_handles
            .drain()
            .map(|(_, handle)| handle)
            .collect::<Vec<_>>()
    });
    for handle in handles {
        handle.abort();
    }
}

/// Fan an event out to every live subscriber of a session, pruning dead ones.
fn notify(session_id: &str, event: &CliEvent) {
    with_box(session_id, |sbox| {
        publish(sbox, event);
    });
}

/// Forward a background job's working-step event (tool rows, notices) to the
/// session's subscribers — the live feed for the TUI's sub-agent visibility
/// toggle while a detached specialist works between turns.
pub fn forward(session_id: &str, event: CliEvent) {
    forward_owned(session_id, None, event);
}

pub fn forward_owned(session_id: &str, scope: Option<&ExecutionScope>, event: CliEvent) -> JournalEvent {
    with_box(session_id, |sbox| {
        if let CliEvent::VolumeWorkerLifecycle {
            batch_id,
            worker_id,
            label,
            item_id,
            status,
        } = &event
        {
            match status {
                crate::runtime::VolumeWorkerLifecycleStatus::Started => {
                    sbox.volume_workers.insert(
                        worker_id.clone(),
                        RunningVolumeWorker {
                            batch_id: batch_id.clone(),
                            worker_id: worker_id.clone(),
                            label: label.clone(),
                            item_id: item_id.clone(),
                        },
                    );
                }
                crate::runtime::VolumeWorkerLifecycleStatus::Completed
                | crate::runtime::VolumeWorkerLifecycleStatus::Failed
                | crate::runtime::VolumeWorkerLifecycleStatus::Cancelled => {
                    sbox.volume_workers.remove(worker_id);
                }
            }
        }
        publish_owned(sbox, &event, scope)
    })
}

/// Forward an event to every session that has a background job RUNNING —
/// for process-global signals that carry no session id, like the provider
/// streaming heartbeat ("model streaming… N KB"). This is what keeps a
/// minutes-long background generation from looking hung in the TUI.
pub fn forward_to_active(event: CliEvent) {
    let sessions: Vec<String> = {
        let map = state().lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .filter(|(_, sbox)| !sbox.running.is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    };
    for session_id in sessions {
        notify(&session_id, &event);
    }
}

/// Broadcast a cross-session answer signal to every session box. The desktop
/// uses it only for an ephemeral owner-aware toast when another conversation
/// is open; it is never conversation content.
pub fn broadcast_answer(
    session_id: &str,
    owner_kind: Option<&str>,
    owner_id: Option<&str>,
    project_name: &str,
    summary: &str,
) {
    let event = CliEvent::CrossAnswer {
        session_id: session_id.to_string(),
        owner_kind: owner_kind.map(str::to_string),
        owner_id: owner_id.map(str::to_string),
        project_name: project_name.to_string(),
        summary: summary.to_string(),
    };
    let sessions: Vec<String> = {
        let map = state().lock().unwrap_or_else(|p| p.into_inner());
        map.keys().cloned().collect()
    };
    for sid in sessions {
        notify(&sid, &event);
    }
}

/// Record a background spawn and tell subscribers.
pub fn job_started(session_id: &str, agent: &str, subject: &str) -> String {
    job_started_with_transition_hook(session_id, agent, subject, || {})
}

/// Start one job while keeping the running-row insertion and Spawned receipt
/// in one lifecycle critical section. The hook gives concurrency tests a
/// deterministic pause at the otherwise-unobservable transition boundary.
fn job_started_with_transition_hook(
    session_id: &str,
    agent: &str,
    subject: &str,
    transition_hook: impl FnOnce(),
) -> String {
    let handoff_id = format!("return_{}", uuid::Uuid::new_v4().simple());
    crate::runtime::company::mirror_job_started(session_id, agent, subject);
    crate::runtime::journal::record_handoff(
        session_id,
        &handoff_id,
        "orchestrator",
        agent,
        subject,
        "queued",
        None,
        None,
    );
    let event = CliEvent::BackgroundAgentSpawned {
        agent: agent.to_string(),
        subject: subject.to_string(),
        handoff_id: handoff_id.clone(),
        requester: "orchestrator".to_string(),
        receiver: agent.to_string(),
        status: "queued".to_string(),
        causation_id: None,
    };
    with_box(session_id, |sbox| {
        sbox.running.push(RunningJob {
            agent: agent.to_string(),
            subject: subject.to_string(),
            handoff_id: handoff_id.clone(),
            causation_id: None,
            started: chrono::Utc::now(),
        });
        transition_hook();
        // A subscriber either receives this live event or registers after the
        // lock is released and replays the running row. It can never do both.
        publish(sbox, &event);
    });
    tracing::info!("background {agent} spawned [{session_id}]: {subject}");
    handoff_id
}

/// Record a background completion: the job moves from running to ready (held
/// for the orchestrator) and subscribers see the colored return immediately.
pub fn job_finished(session_id: &str, job: CompletedJob) {
    job_finished_with_transition_hook(session_id, job, || {});
}

#[cfg(test)]
fn return_persist_faults() -> &'static Mutex<std::collections::HashSet<String>> {
    static FAULTS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn fail_next_return_persist(delivery_id: &str) {
    return_persist_faults()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(delivery_id.to_string());
}

fn persist_job_return(session_id: &str, job: &CompletedJob) -> anyhow::Result<CompletedJob> {
    #[cfg(test)]
    if return_persist_faults()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&job.delivery_id)
    {
        anyhow::bail!("injected coworker-return persistence failure");
    }
    crate::runtime::company::global()?.persist_job_return(session_id, job)
}

fn completed_jobs_match(left: &CompletedJob, right: &CompletedJob) -> bool {
    left.kind == right.kind && left.delivery_id == right.delivery_id
        && left.causation_id == right.causation_id
        && left.agent == right.agent
        && left.subject == right.subject
        && left.ok == right.ok
        && left.summary == right.summary
        && left.body == right.body
        && left.finished == right.finished
}

fn retain_pending_return(session_id: &str, generation: u64, job: &CompletedJob) {
    with_box(session_id, |sbox| {
        if sbox.generation != generation {
            return;
        }
        if job.kind==ReturnKind::Terminal
            && matches!(crate::tools::terminal_jobs::receipt_pending(session_id,job),Ok(false)) {
            return;
        }
        if let Some(existing) = sbox
            .pending_persist
            .iter()
            .find(|pending| pending.job.delivery_id == job.delivery_id)
        {
            if existing.generation != generation || !completed_jobs_match(&existing.job, job) {
                tracing::error!(
                    "coworker return id {} was retried with a different in-memory payload",
                    job.delivery_id
                );
            }
            return;
        }
        sbox.pending_persist.push(PendingCompletedJob {
            generation,
            job: job.clone(),
        });
    });
}

enum CompletionAdmission {
    Publish { job: CompletedJob, generation: u64 },
    AlreadyPublished,
}

fn normalize_completed_job(
    session_id: &str,
    mut job: CompletedJob,
) -> anyhow::Result<CompletionAdmission> {
    with_box(session_id, |sbox| {
        let lifecycle = sbox
            .running
            .iter()
            .find(|running| {
                if job.delivery_id.is_empty() {
                    running.agent == job.agent && running.subject == job.subject
                } else {
                    running.handoff_id == job.delivery_id
                }
            })
            .cloned();
        if let Some(running) = lifecycle {
            if job.delivery_id.is_empty() {
                job.delivery_id = running.handoff_id;
            }
            if job.causation_id.is_none() {
                job.causation_id = running.causation_id;
            }
            return Ok(CompletionAdmission::Publish {
                job,
                generation: sbox.generation,
            });
        }

        anyhow::ensure!(
            !job.delivery_id.is_empty(),
            "completion has no matching running handoff"
        );
        if let Some(pending) = sbox
            .pending_persist
            .iter()
            .find(|pending| pending.job.delivery_id == job.delivery_id)
        {
            anyhow::ensure!(
                completed_jobs_match(&pending.job, &job),
                "pending completion payload changed before durable retry"
            );
            return Ok(CompletionAdmission::Publish {
                job: pending.job.clone(),
                generation: pending.generation,
            });
        }
        if let Some(terminal) = sbox
            .terminal_returns
            .iter()
            .find(|terminal| terminal.delivery_id == job.delivery_id)
        {
            anyhow::ensure!(
                completed_jobs_match(terminal, &job),
                "terminal completion id was reused with a different payload"
            );
            return Ok(CompletionAdmission::AlreadyPublished);
        }
        if job.kind == ReturnKind::Terminal {
            if crate::tools::terminal_jobs::receipt_pending(session_id, &job)? {
                return Ok(CompletionAdmission::Publish {job, generation:sbox.generation});
            }
            return Ok(CompletionAdmission::AlreadyPublished);
        }
        anyhow::bail!(
            "completion `{}` has no live or retryable handoff",
            job.delivery_id
        )
    })
}

/// Cross the durable boundary and publish one return. `after_stage` runs after
/// the immutable body commits but before it becomes claimable. `transition_hook`
/// runs after staged→ready promotion while the SessionBox lock is still held;
/// focused concurrency tests use both seams to prove the ordering protocol.
fn try_job_finished_with_hooks(
    session_id: &str,
    job: CompletedJob,
    after_stage: impl FnOnce(),
    transition_hook: impl FnOnce(),
) -> anyhow::Result<bool> {
    let CompletionAdmission::Publish { job, generation } =
        normalize_completed_job(session_id, job)?
    else {
        return Ok(false);
    };
    try_admitted_job_finished_with_hooks(session_id, job, generation, after_stage, transition_hook)
}

fn try_admitted_job_finished_with_hooks(
    session_id: &str,
    job: CompletedJob,
    generation: u64,
    after_stage: impl FnOnce(),
    transition_hook: impl FnOnce(),
) -> anyhow::Result<bool> {
    // The generation check and staging commit share the SessionBox lock. A
    // permanent discard therefore happens wholly before the write (rejecting
    // it) or wholly after it (and its following purge removes the staged row).
    let job = match with_box(session_id, |sbox| -> anyhow::Result<CompletedJob> {
        anyhow::ensure!(
            sbox.generation == generation,
            "completion belongs to discarded session work"
        );
        if job.kind==ReturnKind::Terminal {
            anyhow::ensure!(crate::tools::terminal_jobs::receipt_pending(session_id,&job)?,
                "terminal completion was cancelled or delivered");
        }
        persist_job_return(session_id, &job)
    }) {
        Ok(job) => job,
        Err(error) => {
            retain_pending_return(session_id, generation, &job);
            return Err(error);
        }
    };
    after_stage();

    let event = CliEvent::BackgroundAgentReturned {
        agent: job.agent.clone(),
        subject: job.subject.clone(),
        ok: job.ok,
        summary: job.summary.clone(),
        body: job.body.clone(),
        handoff_id: job.delivery_id.clone(),
        requester: "orchestrator".to_string(),
        receiver: job.agent.clone(),
        status: if job.ok { "done" } else { "blocked" }.to_string(),
        reply_to: Some(job.delivery_id.clone()),
        causation_id: job.causation_id.clone(),
    };
    let promoted = with_box(session_id, |sbox| -> anyhow::Result<bool> {
        anyhow::ensure!(
            sbox.generation == generation,
            "completion belongs to discarded session work"
        );
        if let Some(terminal) = sbox
            .terminal_returns
            .iter()
            .find(|terminal| terminal.delivery_id == job.delivery_id)
        {
            anyhow::ensure!(
                completed_jobs_match(terminal, &job),
                "terminal completion id was reused with a different payload"
            );
            sbox.pending_persist
                .retain(|pending| pending.job.delivery_id != job.delivery_id);
            return Ok(false);
        }
        // Promotion happens under the lifecycle lock. `take_ready` releases
        // the SQLite lock before entering this lock, so there is no DB→box /
        // box→DB deadlock; a claimant that sees `ready` waits here until the
        // matching Returned event has been queued.
        crate::runtime::company::global()?.promote_job_return(session_id, &job.delivery_id)?;
        if job.kind == ReturnKind::Specialist {
        crate::runtime::company::mirror_job_settled(
            session_id,
            &job.agent,
            &job.subject,
            job.ok,
            &job.summary,
        );
        crate::runtime::journal::record_handoff(
            session_id,
            &job.delivery_id,
            "orchestrator",
            &job.agent,
            &job.subject,
            if job.ok { "done" } else { "blocked" },
            Some(&job.delivery_id),
            job.causation_id.as_deref(),
        );
        }
        if job.kind==ReturnKind::Specialist {sbox.background_handles.remove(&job.agent);}
        if let Some(pos) = sbox
            .running
            .iter()
            .position(|running| running.handoff_id == job.delivery_id)
        {
            sbox.running.remove(pos);
        }
        // Usually the first ActiveTurnGuard consumed this already. This exact
        // instance cleanup covers cancellation/panic before registration and
        // is intentionally not a base-wide clear (parallel guild instances
        // may still be legitimately waiting for the same lane).
        if job.kind==ReturnKind::Specialist {remove_starting_instance(sbox, &job.agent);}
        sbox.pending_persist
            .retain(|pending| pending.job.delivery_id != job.delivery_id);
        if !sbox
            .ready
            .iter()
            .any(|ready| ready.delivery_id == job.delivery_id)
        {
            sbox.ready.push(job.clone());
        }
        if sbox.terminal_returns.len() >= TERMINAL_RETURN_RECEIPT_CAP {
            sbox.terminal_returns.pop_front();
        }
        sbox.terminal_returns.push_back(job.clone());
        transition_hook();
        if job.kind == ReturnKind::Specialist { publish(sbox, &event); }
        Ok(true)
    });
    let published = match promoted {
        Ok(published) => published,
        Err(error) => {
            retain_pending_return(session_id, generation, &job);
            return Err(error);
        }
    };
    if !published {
        return Ok(false);
    }

    // Managed terminal work resumes its existing owner. Detached mode-2
    // specialist work has one notification owner. It settles
    // the visible handoff immediately and remains ready for the owner's next
    // natural turn. It must not synthesize a new owner turn after the user has
    // already received a final answer: that produced the late “I heard back”
    // answer which looked chronologically impossible on replay.
    if job.kind == ReturnKind::Terminal {
        wake_terminal(session_id);
    } else {
        crate::notifications::publish_event(session_id, &event);
    }
    Ok(true)
}

/// Complete one job while keeping durable publication, the in-memory state
/// transition, and its subscriber receipt in one lifecycle critical section.
fn job_finished_with_transition_hook(
    session_id: &str,
    job: CompletedJob,
    transition_hook: impl FnOnce(),
) {
    let agent = job.agent.clone();
    let subject = job.subject.clone();
    if let Err(error) = try_job_finished_with_hooks(session_id, job, || {}, transition_hook) {
        tracing::error!(
            "background return [{agent} / {subject}] remains pending durable persistence: {error:#}"
        );
        if with_box(session_id, |sbox| !sbox.pending_persist.is_empty()) {
            schedule_return_retry(session_id);
        }
    }
}

fn retry_pending_job_returns_once(session_id: &str) -> usize {
    let pending = with_box(session_id, |sbox| sbox.pending_persist.clone());
    let mut completed = 0;
    for pending in pending {
        if try_admitted_job_finished_with_hooks(
            session_id,
            pending.job,
            pending.generation,
            || {},
            || {},
        )
        .is_ok_and(|published| published)
        {
            completed += 1;
        }
    }
    completed
}

fn schedule_return_retry(session_id: &str) {
    let should_schedule = with_box(session_id, |sbox| {
        if sbox.pending_persist.is_empty() || sbox.return_retry_scheduled {
            false
        } else {
            sbox.return_retry_scheduled = true;
            true
        }
    });
    if !should_schedule {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        with_box(session_id, |sbox| sbox.return_retry_scheduled = false);
        tracing::warn!("background returns [{session_id}] await a runtime maintenance retry");
        return;
    };
    let session_id = session_id.to_string();
    runtime.spawn(async move {
        for delay_ms in RETURN_RETRY_DELAYS_MS {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            retry_pending_job_returns_once(&session_id);
            if with_box(&session_id, |sbox| sbox.pending_persist.is_empty()) {
                break;
            }
        }
        // Managed terminal results must not depend on another user message
        // arriving after a transient database failure. Keep a quiet retry
        // while that result remains pending; Stop removes it from this queue.
        while with_box(&session_id, |sbox| sbox.pending_persist.iter()
            .any(|pending| pending.job.kind == ReturnKind::Terminal)) {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            retry_pending_job_returns_once(&session_id);
        }
        let still_pending = with_box(&session_id, |sbox| {
            sbox.return_retry_scheduled = false;
            sbox.pending_persist.len()
        });
        if still_pending > 0 {
            tracing::error!(
                "{still_pending} background return(s) [{session_id}] remain pending after bounded durable-persistence retries"
            );
        }
    });
}

/// Ping the daemon's idle-wake loop for this session (no-op when no notifier is
/// registered). The gateway calls it after delivering a user message as a
/// steer while a turn was live, so a fresh turn drains that steer if the live
/// turn ends before its next round-top drain. The daemon re-checks pending work
/// under the turn lock, so a ping with nothing left is harmless.
pub fn wake(session_id: &str) {
    #[cfg(test)]
    {
        *wake_receipts()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(session_id.to_string())
            .or_default() += 1;
    }
    if let Some(tx) = wake_notifier()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        let _ = tx.send(WakeRequest::UserSteer(session_id.to_string()));
    }
}

fn wake_terminal(session_id: &str) {
    if let Some(tx) = wake_notifier().lock().unwrap_or_else(|p|p.into_inner()).as_ref() {
        let _ = tx.send(WakeRequest::TerminalCompletion(session_id.to_string()));
    }
}

/// Drain finished results for the orchestrator to absorb into its context.
pub fn take_ready(session_id: &str) -> Vec<CompletedJob> {
    with_box(session_id, |sbox| {
        // Claim and drain under one lifecycle lock. Besides preserving the
        // Returned→Absorbed barrier, this prevents two simultaneous consumers
        // from splitting the durable copy and its in-memory mirror and each
        // delivering the same return once.
        let durable = match crate::runtime::company::global()
            .and_then(|store| store.claim_job_returns(session_id))
        {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(
                    "postbox [{session_id}] could not claim durable returns: {error:#}"
                );
                Vec::new()
            }
        };
        let memory = std::mem::take(&mut sbox.ready);
        let mut seen = std::collections::HashSet::new();
        let mut ready = Vec::with_capacity(durable.len().max(memory.len()));
        for job in durable.into_iter().chain(memory) {
            if seen.insert(job.delivery_id.clone()) {
                ready.push(job);
            }
        }
        let specialist_count=ready.iter().filter(|job|job.kind==ReturnKind::Specialist).count();
        if specialist_count>0 {
            // Publish while holding the same lifecycle lock as the drain. A
            // subscriber therefore always observes Returned -> Absorbed in
            // that order, even when another job finishes concurrently.
            let event = CliEvent::BackgroundResultsAbsorbed { count: specialist_count };
            publish(sbox, &event);
        }
        ready
    })
}

/// Settle durable return receipts only after their Talk messages are safely
/// in the canonical session file. Every observable return has a durable row;
/// persistence failures stay in the private retry queue instead.
pub fn acknowledge_ready(session_id: &str, jobs: &[CompletedJob]) -> anyhow::Result<()> {
    for job in jobs.iter().filter(|job|job.kind==ReturnKind::Terminal) {
        crate::tools::terminal_jobs::mark_delivered(session_id,&job.delivery_id)?;
    }
    let ids = jobs
        .iter()
        .map(|job| job.delivery_id.clone())
        .collect::<Vec<_>>();
    crate::runtime::company::global()?.acknowledge_job_returns(session_id, &ids)
}

/// Stop prevents a queued terminal continuation from starting after the
/// user's cancellation. Keep its command result file for later inspection.
pub fn suppress_terminal_returns(session_id:&str, actor:Option<&str>)->anyhow::Result<usize> {
    // Completed records also cover the gap between a worker exiting and
    // publication. Active commands have already received cancellation.
    let completed=crate::tools::terminal_jobs::completed_for_session(session_id,actor)?;
    with_box(session_id,|sbox| -> anyhow::Result<usize> {
        let mut jobs=completed;
        jobs.extend(sbox.ready.iter().cloned());
        jobs.extend(sbox.pending_persist.iter().map(|pending|pending.job.clone()));
        jobs.retain(|job|job.kind==ReturnKind::Terminal&&actor.is_none_or(|actor|base_agent(&job.agent)==actor));
        let ids=jobs.iter().map(|job|job.delivery_id.clone()).collect::<std::collections::HashSet<_>>();
        for id in &ids {crate::tools::terminal_jobs::mark_delivered(session_id,id)?;}
        crate::runtime::company::global()?.discard_terminal_returns(session_id,&ids.iter().cloned().collect::<Vec<_>>())?;
        sbox.ready.retain(|job|!ids.contains(&job.delivery_id));
        sbox.pending_persist.retain(|pending|!ids.contains(&pending.job.delivery_id));
        Ok(ids.len())
    })
}

/// Put a failed transcript save back in both authorities so the next round or
/// restart retries it rather than moving on with an uncommitted result.
pub fn release_ready(session_id: &str, jobs: Vec<CompletedJob>) -> anyhow::Result<()> {
    let ids = jobs
        .iter()
        .map(|job| job.delivery_id.clone())
        .collect::<Vec<_>>();
    with_box(session_id, |sbox| {
        let existing = sbox
            .ready
            .iter()
            .map(|job| job.delivery_id.clone())
            .collect::<std::collections::HashSet<_>>();
        sbox.ready.extend(
            jobs.into_iter()
                .filter(|job| !existing.contains(&job.delivery_id)),
        );
    });
    crate::runtime::company::global()?.release_job_returns(session_id, &ids)
}

/// Gateway-start recovery. Interrupted stages and claims become ready and
/// their complete bodies are rehydrated into the live postbox. The returned
/// session ids are diagnostic for specialist work. Managed terminal results
/// explicitly resume their existing owner after an actual command exit.
pub fn recover_durable_returns() -> anyhow::Result<Vec<String>> {
    let store = crate::runtime::company::global()?;
    let sessions = store.recover_job_returns()?;
    for session_id in &sessions {
        let jobs = store.peek_job_returns(session_id)?;
        with_box(session_id, |sbox| {
            let existing = sbox
                .ready
                .iter()
                .map(|job| job.delivery_id.clone())
                .collect::<std::collections::HashSet<_>>();
            sbox.ready.extend(
                jobs.into_iter()
                    .filter(|job| !existing.contains(&job.delivery_id)),
            );
        });
        if has_terminal_ready(session_id) { wake_terminal(session_id); }
    }
    Ok(sessions)
}

/// Queue a mid-task message for a WORKING specialist. Its turn loop drains
/// this at the top of every round, so the note lands in the specialist's very
/// next model call — the mechanism behind "stop/redirect a running agent".
pub fn steer(session_id: &str, agent: &str, mut note: SteerNote) {
    let base = base_agent(agent).to_string();
    if crate::runtime::mailbox::same_agent_identity(&note.from, &base) {
        crate::runtime::journal::record(
            session_id,
            "steer_rejected",
            &base,
            &format!("self-addressed steer from {} was not queued", note.from),
        );
        tracing::warn!(
            "rejected self-addressed steer in {session_id}: {} -> {base}",
            note.from
        );
        return;
    }
    if note.message_id.is_empty() {
        note.message_id = crate::runtime::company::mirror_message_accepted(
            session_id,
            &note.from,
            &base,
            &note.subject,
            &note.body,
        );
    }
    crate::runtime::journal::record(
        session_id,
        "steer",
        &base,
        &format!("from {}: {}", note.from, note.body),
    );
    let event = CliEvent::SteerQueued {
        from: note.from.clone(),
        to: base.clone(),
        subject: note.subject.clone(),
        body: note.body.clone(),
    };
    with_box(session_id, |sbox| {
        let priority = crate::tools::priority_from_transport_body(&note.body).rank();
        let queue = sbox.steer.entry(base).or_default();
        let index = queue
            .iter()
            .position(|queued| {
                crate::tools::priority_from_transport_body(&queued.body).rank() < priority
            })
            .unwrap_or(queue.len());
        queue.insert(index, note);
    });
    // Outside `with_box`: `notify` takes the same non-reentrant lock.
    notify(session_id, &event);
}

/// Drain pending steer notes for one specialist (called at its round top).
pub fn take_steer(session_id: &str, agent: &str) -> Vec<SteerNote> {
    let base = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        sbox.steer.remove(&base).unwrap_or_default()
    })
}

/// Prefix of a user message that was delivered into a turn that was already
/// running. It is persisted with the message (so history keeps the real order
/// and the model sees why a user line appears between tool calls), and it is
/// deliberately NOT one of the internal runtime prefixes that transcript
/// renderers hide: this is an authored user message.
pub const USER_STEER_MARKER: &str = "[New message from the user while you were working — read it now; it may correct, add to, or replace the current task]";

const USER_STEER_ID_CAP: usize = 256;

/// The durable/model form of a mid-turn user message.
pub fn user_steer_content(body: &str) -> String {
    format!("{USER_STEER_MARKER}\n{body}")
}

/// The authored text of a persisted mid-turn user message, or `None` when the
/// content is an ordinary user message.
pub fn strip_user_steer_marker(content: &str) -> Option<&str> {
    let content = content.trim_start();
    std::iter::once(USER_STEER_MARKER)
        .chain(RoomSteerKind::ALL.iter().map(|kind| kind.marker()))
        .find_map(|marker| content.strip_prefix(marker))
        .map(|rest| rest.strip_prefix('\n').unwrap_or(rest))
}

/// Model/durable form of any user-authored steer note: a one-to-one message
/// keeps the original marker; a group-room delivery is framed by its kind
/// (actionable vs FYI) so a member that was not addressed never mistakes the
/// room message for an order.
pub fn user_steer_content_for(note: &SteerNote) -> String {
    match room_steer_parts(&note.subject) {
        Some((kind, _)) => format!("{}\n{}", kind.marker(), note.body),
        None => user_steer_content(&note.body),
    }
}

/// Deliver a user-authored composer message into the running turn owned by
/// `agent` (the steering inbox). `client_turn_id` is the composer's durable
/// message id: a retry of an id already accepted returns `false` and parks
/// nothing, so a reconnect can never inject the same message twice.
pub fn steer_user(
    session_id: &str,
    agent: &str,
    client_turn_id: Option<&str>,
    body: &str,
) -> bool {
    if let Some(id) = client_turn_id.map(str::trim).filter(|id| !id.is_empty()) {
        let fresh = with_box(session_id, |sbox| {
            if sbox.user_steer_ids.iter().any(|seen| seen == id) {
                return false;
            }
            sbox.user_steer_ids.push_back(id.to_string());
            while sbox.user_steer_ids.len() > USER_STEER_ID_CAP {
                sbox.user_steer_ids.pop_front();
            }
            true
        });
        if !fresh {
            return false;
        }
    }
    steer(
        session_id,
        agent,
        SteerNote {
            message_id: String::new(),
            from: "user".to_string(),
            subject: "message from user".to_string(),
            body: body.to_string(),
        },
    );
    true
}

/// Was this composer message id already accepted as a mid-turn steer?
pub fn user_steer_seen(session_id: &str, client_turn_id: &str) -> bool {
    with_box(session_id, |sbox| {
        sbox.user_steer_ids.iter().any(|seen| seen == client_turn_id)
    })
}

/// Remove and return the OLDEST user-authored note waiting in `agent`'s lane,
/// leaving coworker/watcher notes (and any later user notes) in place. Used
/// when the turn a user message was meant for ended before its next round-top
/// drain: that message then becomes the request of a normal new turn, and the
/// new turn's own round-top drain picks up whatever is still parked. The
/// removal is atomic under the session mutex, so the message is delivered
/// exactly once whichever path wins.
pub fn take_first_user_steer(session_id: &str, agent: &str) -> Option<SteerNote> {
    let base = base_agent(agent).to_string();
    with_box(session_id, |sbox| {
        let lane = sbox.steer.get_mut(&base)?;
        let index = lane
            .iter()
            .position(|note| note.from == "user" && !is_room_steer(note))?;
        let note = lane.remove(index);
        if lane.is_empty() {
            sbox.steer.remove(&base);
        }
        Some(note)
    })
}

// ── Group rooms: everyone hears everything, only some act ──────────────
//
// A user message in a group room is written to the canonical room transcript
// once, at send time. Every member whose turn is running in that room gets it
// steered into the running turn through its own lane (keyed by the canonical
// session + agent lane): actionable for the members who must act, FYI for the
// rest. Each per-lane delivery is idempotent by `<client turn id>@<lane>`.

/// Subject prefix of a group-room steer note: `room-steer:<kind>:<delivery id>`.
pub const ROOM_STEER_SUBJECT_PREFIX: &str = "room-steer:";

/// How a room message is framed for one running member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomSteerKind {
    /// Unaddressed message, delivered to the running leader: act on it.
    LeaderAct,
    /// The member was @mentioned: top-priority, act on it now.
    MentionAct,
    /// `@everyone` / `@all`, delivered to a running member: act in your area.
    EveryoneAct,
    /// `@everyone` / `@all`, delivered to the running leader: re-plan.
    EveryoneReplan,
    /// Only the leader was @mentioned: answer alone, no fan-out.
    LeaderSolo,
    /// Not addressed to this member: context only.
    Fyi,
    /// Members were @mentioned; the running leader only keeps the board straight.
    LeaderFyi,
}

impl RoomSteerKind {
    pub const ALL: [RoomSteerKind; 7] = [
        Self::LeaderAct,
        Self::MentionAct,
        Self::EveryoneAct,
        Self::EveryoneReplan,
        Self::LeaderSolo,
        Self::Fyi,
        Self::LeaderFyi,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeaderAct => "leader-act",
            Self::MentionAct => "mention-act",
            Self::EveryoneAct => "everyone-act",
            Self::EveryoneReplan => "everyone-replan",
            Self::LeaderSolo => "leader-solo",
            Self::Fyi => "fyi",
            Self::LeaderFyi => "leader-fyi",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// Must the receiving member act on this message?
    pub fn is_actionable(self) -> bool {
        !matches!(self, Self::Fyi | Self::LeaderFyi)
    }

    /// The persisted/model header. Never one of the hidden runtime prefixes:
    /// this is an authored user message (in the member's own working session;
    /// the canonical room transcript holds the plain message exactly once).
    pub fn marker(self) -> &'static str {
        match self {
            Self::LeaderAct => "[New room message from the user while you were working — not addressed to anyone, so it is yours as group leader. Act on it now: it may correct, add to, or replace the current request. If it changes the plan, update the board and adjust assignments; members who are working have already heard it.]",
            Self::MentionAct => "[TOP PRIORITY — the user just @mentioned you in the room while you were working. Act on this message before anything else; it may correct, add to, or replace your current task. Reply to it in the room.]",
            Self::EveryoneAct => "[The user just addressed @everyone in the room while you were working. Act on it now for your own area; it may correct, add to, or replace your current task. The leader re-plans around it.]",
            Self::EveryoneReplan => "[The user just addressed @everyone in the room while you were working. Every member has it and acts on it in their own area. As leader, re-plan now: update the brief/plan, adjust assignments and claims so nobody collides, then carry on.]",
            Self::LeaderSolo => "[The user just @mentioned only you in the room while you were working. Answer it yourself — do not dispatch, assign, or fan it out to members.]",
            Self::Fyi => "[FYI — the user just said this in the room. It was not addressed to you and someone else is handling it: do not reply to it or start new work for it. If it changes the work you are doing right now (for example a new requirement on what you are building), adapt immediately.]",
            Self::LeaderFyi => "[FYI for the leader — the user just said this in the room to specific members, who own it and act on it directly. Do not take over, redo, or reassign that work. Only keep the board straight: record it (decide / plan item / claims) so claims do not collide.]",
        }
    }
}

/// Per-lane idempotency key of one room delivery.
pub fn room_delivery_id(client_turn_id: &str, lane: &str) -> String {
    format!("{}@{}", client_turn_id.trim(), base_agent(lane))
}

fn room_steer_subject(kind: RoomSteerKind, delivery_id: &str) -> String {
    format!("{ROOM_STEER_SUBJECT_PREFIX}{}:{delivery_id}", kind.as_str())
}

/// `(kind, delivery id)` of a room steer subject.
pub fn room_steer_parts(subject: &str) -> Option<(RoomSteerKind, &str)> {
    let rest = subject.strip_prefix(ROOM_STEER_SUBJECT_PREFIX)?;
    let (kind, id) = rest.split_once(':')?;
    Some((RoomSteerKind::parse(kind)?, id))
}

pub fn is_room_steer(note: &SteerNote) -> bool {
    note.from == "user" && room_steer_parts(&note.subject).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomSteerOutcome {
    /// Parked in the running lane; its next round-top drain injects it.
    Delivered,
    /// This exact (message, lane) was already delivered: nothing parked.
    Duplicate,
    /// The lane is not running any more: the caller falls back (starts a
    /// normal turn for an actionable target; an FYI needs nothing — the
    /// member reads the transcript on its next turn).
    NotRunning,
}

/// Steer one room message into `lane`'s running turn, atomically with the
/// liveness check: the lane is checked and the note parked under the same
/// postbox lock, so a finished lane is reported as `NotRunning` instead of
/// receiving a note nobody drains.
pub fn steer_room_user(
    session_id: &str,
    lane: &str,
    client_turn_id: &str,
    kind: RoomSteerKind,
    body: &str,
) -> RoomSteerOutcome {
    let base = base_agent(lane).to_string();
    let delivery_id = room_delivery_id(client_turn_id, &base);
    let outcome = with_box(session_id, |sbox| {
        if sbox.user_steer_ids.iter().any(|seen| seen == &delivery_id) {
            return RoomSteerOutcome::Duplicate;
        }
        let live = sbox.active_turns.get(&base).is_some_and(|count| *count > 0)
            || sbox.starting_turns.get(&base).is_some_and(|tokens| !tokens.is_empty());
        if !live {
            return RoomSteerOutcome::NotRunning;
        }
        sbox.user_steer_ids.push_back(delivery_id.clone());
        while sbox.user_steer_ids.len() > USER_STEER_ID_CAP {
            sbox.user_steer_ids.pop_front();
        }
        let note = SteerNote {
            message_id: String::new(),
            from: "user".to_string(),
            subject: room_steer_subject(kind, &delivery_id),
            body: body.to_string(),
        };
        let queue = sbox.steer.entry(base.clone()).or_default();
        // An actionable user message goes ahead of coworker chatter; FYI
        // context keeps arrival order behind it.
        let index = if kind.is_actionable() {
            queue.iter().position(|queued| !(queued.from == "user" && room_steer_parts(&queued.subject).is_some_and(|(k, _)| k.is_actionable()))).unwrap_or(queue.len())
        } else {
            queue.len()
        };
        queue.insert(index, note);
        RoomSteerOutcome::Delivered
    });
    if outcome == RoomSteerOutcome::Delivered {
        crate::runtime::journal::record(
            session_id,
            "room_steer",
            &base,
            &format!("{}: {}", kind.as_str(), body),
        );
    }
    outcome
}

/// Where a parked room delivery stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkedRoomSteer {
    /// The running turn drained it (or it was never parked).
    Consumed,
    /// Still parked and the lane is still running: its next round takes it.
    Live,
    /// The lane's turn ended before draining it. The note has been REMOVED
    /// here, atomically, so no later turn can also inject it; the caller
    /// owns the fallback (start a turn for an actionable target).
    Orphaned(SteerNote),
}

/// Race safety for a room delivery. Exactly one of the turn's round-top
/// drain and this call ever gets the note.
pub fn settle_parked_room_steer(session_id: &str, lane: &str, delivery_id: &str) -> ParkedRoomSteer {
    let base = base_agent(lane).to_string();
    with_box(session_id, |sbox| {
        let Some(queue) = sbox.steer.get_mut(&base) else {
            return ParkedRoomSteer::Consumed;
        };
        let Some(index) = queue.iter().position(|note| {
            note.from == "user" && room_steer_parts(&note.subject).is_some_and(|(_, id)| id == delivery_id)
        }) else {
            return ParkedRoomSteer::Consumed;
        };
        let live = sbox.active_turns.get(&base).is_some_and(|count| *count > 0)
            || sbox.starting_turns.get(&base).is_some_and(|tokens| !tokens.is_empty());
        if live {
            return ParkedRoomSteer::Live;
        }
        let note = queue.remove(index);
        if queue.is_empty() {
            sbox.steer.remove(&base);
        }
        ParkedRoomSteer::Orphaned(note)
    })
}

/// Jobs still working, for `/status` and busy checks.
pub fn running_jobs(session_id: &str) -> Vec<RunningJob> {
    with_box(session_id, |sbox| sbox.running.clone())
}

/// Peek at finished-but-unabsorbed results WITHOUT draining them (the
/// orchestrator's `take_ready` stays the only consumer). Feeds the mission
/// map the goal gate reads — "browser already returned, ok" is exactly the
/// context that stops a false NOT MET.
pub fn ready_jobs(session_id: &str) -> Vec<CompletedJob> {
    with_box(session_id, |sbox| sbox.ready.clone())
}

/// Is this specialist's CANONICAL (durable, unscoped) session busy on a
/// background job? Instance labels carry `#n` (e.g. `coder#2`), so an exact
/// label match means the unscoped first instance — the one whose durable
/// session file is single-writer.
pub fn agent_busy(session_id: &str, agent: &str) -> bool {
    with_box(session_id, |sbox| {
        sbox.running.iter().any(|r| r.agent == agent)
    })
}

/// Base role of an instance label: `coder#2` → `coder`.
pub fn base_agent(label: &str) -> &str {
    label.split('#').next().unwrap_or(label)
}

/// Running jobs for a specialist across all parallel instances.
pub fn active_count(session_id: &str, agent: &str) -> usize {
    with_box(session_id, |sbox| {
        sbox.running
            .iter()
            .filter(|r| base_agent(&r.agent) == agent)
            .count()
    })
}

/// Allocate a collision-free display label from the live guild membership.
/// Labels are presentation addresses; scoped session ids provide isolation.
pub fn next_instance_label(session_id: &str, agent: &str) -> String {
    let base = base_agent(agent);
    with_box(session_id, |sbox| {
        if !sbox.running.iter().any(|job| job.agent == base) {
            return base.to_string();
        }
        let next = sbox
            .running
            .iter()
            .filter(|job| base_agent(&job.agent) == base)
            .filter_map(|job| {
                job.agent
                    .split_once('#')
                    .and_then(|(_, n)| n.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(1)
            + 1;
        format!("{base}#{next}")
    })
}

/// Maximum live colleagues in a role guild. These are safety ceilings, not
/// quotas or stopping conditions: work shape decides how many candidates are
/// useful. Physical browser/desktop control remains exclusive; thinking,
/// coding, design, research, review, and testing can form real studios.
pub fn instance_cap(agent: &str) -> usize {
    match base_agent(agent) {
        "coder" => 4,
        "frontend" | "presentation" => 3,
        "researcher" | "critic" | "tester" => 3,
        "planner" | "database" | "hacker" | "scribe" => 2,
        "browser" | "computer_use" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(agent: &str, subject: &str, ok: bool) -> CompletedJob {
        CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(),
            causation_id: None,
            agent: agent.to_string(),
            subject: subject.to_string(),
            ok,
            summary: format!("{agent} finished"),
            body: "result body".to_string(),
            finished: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn identical_subject_returns_settle_only_their_exact_handoff_id() {
        let sid = format!("postbox-handoff-id-{}", uuid::Uuid::new_v4());
        job_started(&sid, "coder#1", "same subject");
        job_started(&sid, "coder#2", "same subject");
        let running = running_jobs(&sid);
        assert_eq!(running.len(), 2);
        assert_ne!(running[0].handoff_id, running[1].handoff_id);

        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        let first_spawn = rx.recv().await.expect("first replayed spawn");
        let second_spawn = rx.recv().await.expect("second replayed spawn");
        let spawn_ids = [first_spawn, second_spawn]
            .into_iter()
            .map(|event| match event {
                CliEvent::BackgroundAgentSpawned { handoff_id, .. } => handoff_id,
                other => panic!("expected spawn, got {other:?}"),
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(spawn_ids.len(), 2);

        let mut second = job("coder#2", "same subject", true);
        second.delivery_id = running[1].handoff_id.clone();
        job_finished(&sid, second);
        let returned = rx.recv().await.expect("second job return");
        assert!(matches!(
            returned,
            CliEvent::BackgroundAgentReturned {
                handoff_id,
                reply_to: Some(reply_to),
                ..
            } if handoff_id == running[1].handoff_id && reply_to == running[1].handoff_id
        ));
        let remaining = running_jobs(&sid);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].handoff_id, running[0].handoff_id);

        let mut first = job("coder#1", "same subject", true);
        first.delivery_id = running[0].handoff_id.clone();
        job_finished(&sid, first);
        let returned = rx.recv().await.expect("first job return");
        assert!(matches!(
            returned,
            CliEvent::BackgroundAgentReturned { handoff_id, .. }
                if handoff_id == running[0].handoff_id
        ));
        let _ = take_ready(&sid);
    }

    #[test]
    fn instance_counting_and_caps_drive_parallel_spawns() {
        // active_count still spans instance labels (coder#2 counts toward
        // coder) — the counting machinery is what ENFORCES the cap. The cap
        // itself is 1 for EVERYONE (user rule 2026-07-10: two Sparks ran side
        // by side; multiple of the same agent must never spawn).
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        job_started(&sid, "coder", "task A");
        assert_eq!(next_instance_label(&sid, "coder"), "coder#2");
        job_started(&sid, "coder#2", "task B");
        assert_eq!(next_instance_label(&sid, "coder"), "coder#3");
        assert_eq!(active_count(&sid, "coder"), 2);
        assert!(agent_busy(&sid, "coder"), "canonical instance running");

        job_finished(&sid, job("coder", "task A", true));
        assert_eq!(active_count(&sid, "coder"), 1, "instance #2 still running");
        assert!(!agent_busy(&sid, "coder"), "canonical session is free");

        assert_eq!(base_agent("coder#2"), "coder");
        assert_eq!(instance_cap("coder"), 4);
        assert_eq!(instance_cap("frontend"), 3);
        assert_eq!(instance_cap("browser"), 1);
        assert_eq!(instance_cap("computer_use"), 1);
    }

    #[test]
    fn lifecycle_started_finished_taken() {
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        assert!(!agent_busy(&sid, "researcher"));
        job_started(&sid, "researcher", "find X");
        assert!(agent_busy(&sid, "researcher"));
        assert_eq!(running_jobs(&sid).len(), 1);

        job_finished(&sid, job("researcher", "find X", true));
        assert!(!agent_busy(&sid, "researcher"));
        assert!(running_jobs(&sid).is_empty());

        let ready = take_ready(&sid);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].agent, "researcher");
        // Drained — a second take is empty.
        assert!(take_ready(&sid).is_empty());
    }

    #[test]
    fn result_drain_is_an_ordered_absorption_barrier() {
        let sid = format!("postbox-absorb-test-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);

        job_started(&sid, "researcher", "find release");
        assert!(has_background_work(&sid));
        job_finished(&sid, job("researcher", "find release", true));
        assert!(has_background_work(&sid));
        assert_eq!(take_ready(&sid).len(), 1);
        assert!(!has_background_work(&sid));

        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn concurrent_drains_cannot_split_durable_and_memory_copies() {
        let sid = format!("postbox-concurrent-drain-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "single consumer receipt");
        job_finished(&sid, job("researcher", "single consumer receipt", true));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let drain = |barrier: std::sync::Arc<std::sync::Barrier>| {
            let sid = sid.clone();
            std::thread::spawn(move || {
                barrier.wait();
                take_ready(&sid)
            })
        };
        let first = drain(barrier.clone());
        let second = drain(barrier.clone());
        barrier.wait();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first.len() + second.len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        assert!(rx.try_recv().is_err(), "duplicate Absorbed was published");
    }

    #[tokio::test]
    async fn persistence_failure_stays_invisible_and_automatic_retry_publishes_once() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let sid = format!("postbox-persist-fault-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "durable fault injection");
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        let running = running_jobs(&sid).pop().expect("running job");
        let journal_before = crate::runtime::journal::history_digest(&sid);
        assert_eq!(wake_receipt_count(&sid), 0);

        fail_next_return_persist(&running.handoff_id);
        let mut completed = job("researcher", "durable fault injection", true);
        completed.delivery_id = running.handoff_id.clone();
        completed.body = "exact completed body retained for retry".to_string();
        job_finished(&sid, completed.clone());

        assert_eq!(running_jobs(&sid).len(), 1, "running must not settle");
        with_box(&sid, |sbox| {
            assert!(sbox.ready.is_empty(), "failed persistence is not ready");
            assert_eq!(sbox.pending_persist.len(), 1);
            assert_eq!(sbox.pending_persist[0].job.body, completed.body);
        });
        assert!(take_ready(&sid).is_empty(), "nothing can be absorbed");
        assert!(rx.try_recv().is_err(), "Returned must not be published");
        assert_eq!(
            crate::runtime::journal::history_digest(&sid),
            journal_before,
            "the settled handoff journal must not advance"
        );
        let (snapshot, _) = crate::runtime::company::global()
            .unwrap()
            .bounded_snapshot_for_run(&sid, 8, 1024 * 1024)
            .unwrap();
        let projection = snapshot
            .jobs
            .iter()
            .find(|candidate| candidate.subject == "durable fault injection")
            .expect("durable running projection");
        assert!(projection.settled_at.is_none());
        assert!(projection.ok.is_none());
        assert_eq!(wake_receipt_count(&sid), 0, "failure must not wake");

        let returned = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("bounded retry should publish")
            .expect("subscriber remains live");
        assert!(matches!(
            returned,
            CliEvent::BackgroundAgentReturned { handoff_id, ref body, .. }
                if handoff_id == running.handoff_id && body == &completed.body
        ));
        assert!(running_jobs(&sid).is_empty());
        assert_eq!(with_box(&sid, |sbox| sbox.pending_persist.len()), 0);
        assert_eq!(
            wake_receipt_count(&sid),
            0,
            "a durable mode-2 retry still must not create an owner turn"
        );
        let ready = take_ready(&sid);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].delivery_id, running.handoff_id);
        assert_eq!(ready[0].body, completed.body);
        assert!(
            take_ready(&sid).is_empty(),
            "retry must deliver exactly once"
        );
    }

    #[test]
    fn discard_invalidates_an_already_cloned_persistence_retry() {
        let sid = format!("postbox-discard-retry-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "return deleted with its turn");
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        let running = running_jobs(&sid).pop().expect("running job");
        let mut completed = job("researcher", "return deleted with its turn", true);
        completed.delivery_id = running.handoff_id;
        fail_next_return_persist(&completed.delivery_id);
        job_finished(&sid, completed);
        let cloned_retry = with_box(&sid, |sbox| {
            assert_eq!(sbox.pending_persist.len(), 1);
            sbox.pending_persist[0].clone()
        });
        let journal_before = crate::runtime::journal::history_digest(&sid);

        discard_session_work(&sid);
        crate::runtime::company::global()
            .unwrap()
            .purge_job_returns(&sid)
            .unwrap();
        let error = try_admitted_job_finished_with_hooks(
            &sid,
            cloned_retry.job,
            cloned_retry.generation,
            || {},
            || {},
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("discarded session work"), "{error}");
        assert!(
            crate::runtime::company::global()
                .unwrap()
                .peek_job_returns(&sid)
                .unwrap()
                .is_empty(),
            "a stale retry must not recreate the purged durable row"
        );
        assert!(
            !crate::runtime::company::global()
                .unwrap()
                .recover_job_returns()
                .unwrap()
                .contains(&sid),
            "restart recovery must not resurrect discarded work"
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(wake_receipt_count(&sid), 0);
        assert_eq!(
            crate::runtime::journal::history_digest(&sid),
            journal_before
        );
        with_box(&sid, |sbox| {
            assert!(sbox.running.is_empty());
            assert!(sbox.pending_persist.is_empty());
            assert!(sbox.ready.is_empty());
        });
    }

    #[test]
    fn duplicate_completion_after_ack_is_not_recreated_or_republished() {
        let sid = format!("postbox-terminal-dedupe-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "exact terminal retry");
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        let running = running_jobs(&sid).pop().expect("running job");
        let mut completed = job("researcher", "exact terminal retry", true);
        completed.delivery_id = running.handoff_id.clone();
        job_finished(&sid, completed.clone());
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));
        let ready = take_ready(&sid);
        assert_eq!(ready.len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        acknowledge_ready(&sid, &ready).unwrap();
        let wake_count = wake_receipt_count(&sid);
        let journal = crate::runtime::journal::history_digest(&sid);

        job_finished(&sid, completed);
        assert!(rx.try_recv().is_err(), "duplicate Returned was published");
        assert!(take_ready(&sid).is_empty(), "duplicate row was recreated");
        assert_eq!(wake_receipt_count(&sid), wake_count);
        assert_eq!(crate::runtime::journal::history_digest(&sid), journal);
        assert!(crate::runtime::company::global()
            .unwrap()
            .peek_job_returns(&sid)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn staged_return_is_invisible_to_concurrent_drain_until_published() {
        let sid = format!("postbox-staged-drain-race-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "pause after durable stage");
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));

        let (staged_tx, staged_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let finish_sid = sid.clone();
        let finisher = std::thread::spawn(move || {
            try_job_finished_with_hooks(
                &finish_sid,
                job("researcher", "pause after durable stage", true),
                || {
                    staged_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                || {},
            )
            .unwrap();
        });
        staged_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("return body was durably staged");
        assert!(
            crate::runtime::company::global()
                .unwrap()
                .peek_job_returns(&sid)
                .unwrap()
                .is_empty(),
            "staged rows are not claimable"
        );
        assert!(take_ready(&sid).is_empty());
        assert!(
            rx.try_recv().is_err(),
            "no early Returned or Absorbed event"
        );

        release_tx.send(()).unwrap();
        finisher.join().unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));
        assert_eq!(take_ready(&sid).len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        assert!(take_ready(&sid).is_empty());
    }

    #[test]
    fn subscriber_racing_job_start_receives_one_spawn_not_replay_plus_live() {
        let sid = format!("postbox-start-subscribe-race-{}", uuid::Uuid::new_v4());
        let (transition_tx, transition_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let start_sid = sid.clone();
        let starter = std::thread::spawn(move || {
            job_started_with_transition_hook(&start_sid, "researcher", "race subscription", || {
                transition_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        transition_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("starter reached the insertion-to-publication transition");
        assert!(
            state().try_lock().is_err(),
            "subscription must not enter the half-published start transition"
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let (subscribe_started_tx, subscribe_started_rx) = std::sync::mpsc::channel();
        let subscribe_sid = sid.clone();
        let subscriber = std::thread::spawn(move || {
            subscribe_started_tx.send(()).unwrap();
            subscribe(&subscribe_sid, tx);
        });
        subscribe_started_rx.recv().unwrap();

        release_tx.send(()).unwrap();
        starter.join().unwrap();
        subscriber.join().unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        assert!(
            rx.try_recv().is_err(),
            "the running job was delivered as both a live spawn and a replay"
        );

        job_finished(&sid, job("researcher", "race subscription", true));
        let _ = take_ready(&sid);
    }

    #[test]
    fn finishing_job_blocks_a_concurrent_drain_until_return_is_published() {
        let sid = format!("postbox-finish-drain-race-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        job_started(&sid, "researcher", "race the drain");
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));

        let (transition_tx, transition_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let finish_sid = sid.clone();
        let finisher = std::thread::spawn(move || {
            job_finished_with_transition_hook(
                &finish_sid,
                job("researcher", "race the drain", true),
                || {
                    transition_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            );
        });
        transition_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("finisher reached the running-to-ready transition");
        assert!(
            state().try_lock().is_err(),
            "the return must still own the lifecycle lock before publication"
        );

        let (drain_started_tx, drain_started_rx) = std::sync::mpsc::channel();
        let (drained_tx, drained_rx) = std::sync::mpsc::channel();
        let drain_sid = sid.clone();
        let drainer = std::thread::spawn(move || {
            drain_started_tx.send(()).unwrap();
            drained_tx.send(take_ready(&drain_sid)).unwrap();
        });
        drain_started_rx.recv().unwrap();
        assert!(
            drained_rx
                .recv_timeout(std::time::Duration::from_millis(25))
                .is_err(),
            "take_ready entered before the return receipt was published"
        );

        release_tx.send(()).unwrap();
        finisher.join().unwrap();
        let drained = drained_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("drain completes after return publication");
        drainer.join().unwrap();
        assert_eq!(drained.len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn subscriber_racing_job_finish_receives_one_return_not_replay_plus_live() {
        let sid = format!("postbox-finish-subscribe-race-{}", uuid::Uuid::new_v4());
        job_started(&sid, "researcher", "race subscription");

        let (transition_tx, transition_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let finish_sid = sid.clone();
        let finisher = std::thread::spawn(move || {
            job_finished_with_transition_hook(
                &finish_sid,
                job("researcher", "race subscription", true),
                || {
                    transition_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            );
        });
        transition_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("finisher reached the running-to-ready transition");
        assert!(
            state().try_lock().is_err(),
            "subscription must not enter the half-published transition"
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let (subscribe_started_tx, subscribe_started_rx) = std::sync::mpsc::channel();
        let subscribe_sid = sid.clone();
        let subscriber = std::thread::spawn(move || {
            subscribe_started_tx.send(()).unwrap();
            subscribe(&subscribe_sid, tx);
        });
        subscribe_started_rx.recv().unwrap();

        release_tx.send(()).unwrap();
        finisher.join().unwrap();
        subscriber.join().unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { .. })
        ));
        assert!(
            rx.try_recv().is_err(),
            "the ready job was delivered as both a live return and a replay"
        );

        assert_eq!(take_ready(&sid).len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundResultsAbsorbed { count: 1 })
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn live_turn_tracking_is_not_the_background_job_registry() {
        let sid = format!("postbox-live-turn-{}", uuid::Uuid::new_v4());
        job_started(&sid, "coder", "root job");
        assert!(!agent_turn_active(&sid, "coder"));
        {
            let _guard = active_turn_guard(&sid, "researcher");
            assert!(agent_turn_active(&sid, "researcher"));
            assert!(!agent_turn_active(&sid, "coder"));
        }
        assert!(!agent_turn_active(&sid, "researcher"));
    }

    #[test]
    fn starting_turn_tokens_clean_up_on_drop_and_job_completion() {
        let sid = format!("postbox-starting-turn-{}", uuid::Uuid::new_v4());
        let aborted = starting_turn_guard(&sid, "coder", "coder");
        assert!(agent_turn_starting(&sid, "coder"));
        drop(aborted);
        assert!(
            !agent_turn_starting(&sid, "coder"),
            "dropping an unpolled/aborted detached future clears its marker"
        );

        job_started(&sid, "coder", "registered but never active");
        let completed = starting_turn_guard(&sid, "coder", "coder");
        job_finished(&sid, job("coder", "registered but never active", false));
        assert!(
            !agent_turn_starting(&sid, "coder"),
            "terminal job settlement explicitly clears its exact instance"
        );
        drop(completed);
        let _ = take_ready(&sid);
    }

    #[test]
    fn orphan_rehome_does_not_steal_from_a_starting_turn() {
        let sid = format!("postbox-starting-steer-{}", uuid::Uuid::new_v4());
        let _starting = starting_turn_guard(&sid, "coder", "coder");
        steer(
            &sid,
            "coder",
            SteerNote {
                message_id: String::new(),
                from: "orchestrator".to_string(),
                subject: "correction".to_string(),
                body: "apply this before the first provider call".to_string(),
            },
        );
        assert_eq!(rehome_orphan_steers_to_orchestrator(&sid), 0);
        assert!(has_pending_steer(&sid, "coder"));
        assert!(!has_pending_steer(&sid, "orchestrator"));
    }

    #[tokio::test]
    async fn targeted_cancel_stops_only_addressed_agent_instances() {
        let sid = format!("postbox-cancel-test-{}", uuid::Uuid::new_v4());
        job_started(&sid, "browser", "browse forever");
        job_started(&sid, "browser#2", "browse something else forever");
        job_started(&sid, "coder", "code forever");
        let browser = tokio::spawn(std::future::pending::<()>());
        let browser_2 = tokio::spawn(std::future::pending::<()>());
        let coder = tokio::spawn(std::future::pending::<()>());
        register_background_handle(&sid, "browser", browser.abort_handle());
        register_background_handle(&sid, "browser#2", browser_2.abort_handle());
        register_background_handle(&sid, "coder", coder.abort_handle());

        assert!(cancel_background_agent(&sid, "browser"));
        assert!(!agent_busy(&sid, "browser"));
        assert!(agent_busy(&sid, "coder"), "an unrelated agent must survive");
        assert!(browser.await.unwrap_err().is_cancelled());
        assert!(browser_2.await.unwrap_err().is_cancelled());
        assert!(!coder.is_finished());
        let ready = take_ready(&sid);
        assert_eq!(ready.len(), 2);
        assert!(ready.iter().all(|job| !job.ok));
        assert!(ready
            .iter()
            .all(|job| job.summary == "stop requested by user"));
        assert!(ready
            .iter()
            .all(|job| job.body.contains("may still finish")));
        assert!(
            !cancel_background_agent(&sid, "browser"),
            "a settled agent must not be stopped twice"
        );
        assert!(cancel_background_agent(&sid, "coder"));
        assert!(coder.await.unwrap_err().is_cancelled());
        let _ = take_ready(&sid);
    }

    #[tokio::test]
    async fn detached_registration_is_visible_and_cancellable_before_first_poll() {
        let sid = format!("postbox-atomic-start-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        let starting = register_background_job(
            &sid,
            "coder",
            "coder",
            "not polled yet",
            "return_not_polled",
            Some("message_not_polled"),
            abort,
        );

        assert!(agent_turn_starting(&sid, "coder"));
        assert_eq!(running_jobs(&sid).len(), 1);
        assert!(cancel_background_agent(&sid, "coder"));
        assert!(running_jobs(&sid).is_empty());
        assert!(
            futures_util::future::Abortable::new(std::future::pending::<()>(), registration)
                .await
                .is_err(),
            "a stop before first poll must still abort the future"
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentSpawned { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(CliEvent::BackgroundAgentReturned { ok: false, .. })
        ));
        drop(starting);
        let _ = take_ready(&sid);
    }

    #[test]
    fn steer_notes_queue_and_drain_by_base_label() {
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        assert!(take_steer(&sid, "coder").is_empty());
        // Instance labels collapse to the base role — steering "coder#2"
        // reaches the coder lane.
        steer(
            &sid,
            "coder#2",
            SteerNote {
                message_id: String::new(),
                from: "orchestrator".to_string(),
                subject: "stop".to_string(),
                body: "Stop now and return what you have.".to_string(),
            },
        );
        let notes = take_steer(&sid, "coder");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].from, "orchestrator");
        assert_eq!(notes[0].body, "Stop now and return what you have.");
        // Drained — a second take is empty.
        assert!(take_steer(&sid, "coder").is_empty());
    }

    #[test]
    fn self_addressed_steer_is_rejected_before_queueing() {
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        steer(
            &sid,
            "coder#2",
            SteerNote {
                message_id: String::new(),
                from: "Leo".to_string(),
                subject: "loop".to_string(),
                body: "do not queue this".to_string(),
            },
        );
        assert!(!has_pending_steer(&sid, "coder"));
        assert!(take_steer(&sid, "coder").is_empty());
    }

    #[test]
    fn group_followup_queue_is_never_rehomed_to_phoenix() {
        let sid = format!("group-queue-test-{}", uuid::Uuid::new_v4());
        let lane = "__group_queue_launch";
        steer(
            &sid,
            lane,
            SteerNote {
                message_id: String::new(),
                from: "user".to_string(),
                subject: "queued group message".to_string(),
                body: "continue after the current discussion".to_string(),
            },
        );
        assert_eq!(rehome_orphan_steers_to_orchestrator(&sid), 0);
        assert!(!has_pending_steer(&sid, "orchestrator"));
        assert_eq!(take_steer(&sid, lane).len(), 1);
    }

    #[tokio::test]
    async fn parking_a_steer_notifies_subscribers_immediately() {
        // The card must be drawable the INSTANT the note is parked — a steer
        // can sit in the lane for a whole provider round (minutes on a slow
        // lane), and the user needs to see their message arrive, not stare at
        // a feed that shows nothing until the agent gets around to it.
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        steer(
            &sid,
            "tester#2",
            SteerNote {
                message_id: String::new(),
                from: "user".to_string(),
                subject: "steer from user".to_string(),
                body: "stop and report what you have".to_string(),
            },
        );
        match rx.try_recv() {
            Ok(CliEvent::SteerQueued { from, to, body, .. }) => {
                assert_eq!(from, "user");
                // Instance labels collapse to the base lane, so the card lands
                // in the agent window the user is actually looking at.
                assert_eq!(to, "tester");
                assert_eq!(body, "stop and report what you have");
            }
            other => panic!("expected a queued-steer event, got {other:?}"),
        }
        // Notifying must not consume the note — the agent still drains it.
        assert_eq!(take_steer(&sid, "tester").len(), 1);
    }

    #[tokio::test]
    async fn background_result_waits_for_a_natural_turn_and_has_ready_peeks() {
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        set_wake_notifier(tx);

        assert!(!has_ready(&sid));
        job_started(&sid, "researcher", "find Z");
        job_finished(&sid, job("researcher", "find Z", true));

        // Mode-2 completion is visible to subscribers, but it is absorbed only
        // when the owner next runs naturally. has_ready peeks without
        // draining; take_ready is that natural-turn drain.
        assert!(has_ready(&sid));
        assert!(has_ready(&sid));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        while let Ok(Some(other)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let (WakeRequest::UserSteer(other) | WakeRequest::TerminalCompletion(other)) = other;
            assert_ne!(other, sid, "mode-2 completion must not wake its owner");
        }
        assert_eq!(take_ready(&sid).len(), 1);
        assert!(!has_ready(&sid));
    }

    #[tokio::test]
    async fn subscribers_get_live_events_and_replay() {
        let sid = format!("postbox-test-{}", uuid::Uuid::new_v4());
        job_started(&sid, "coder", "build Y");

        // A subscriber arriving late still learns about the running job.
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);
        match rx.try_recv() {
            Ok(CliEvent::BackgroundAgentSpawned { agent, .. }) => assert_eq!(agent, "coder"),
            other => panic!("expected replayed spawn, got {other:?}"),
        }

        job_finished(&sid, job("coder", "build Y", true));
        match rx.try_recv() {
            Ok(CliEvent::BackgroundAgentReturned { agent, ok, .. }) => {
                assert_eq!(agent, "coder");
                assert!(ok);
            }
            other => panic!("expected live return event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn journal_subscriber_replays_missed_foreground_rows_then_stays_live() {
        let sid = format!("postbox-journal-replay-{}", uuid::Uuid::new_v4());
        begin_foreground_replay(&sid);
        forward(
            &sid,
            CliEvent::AgentThinking {
                agent: "orchestrator".to_string(),
                text: "first visible reasoning".to_string(),
            },
        );
        forward(
            &sid,
            CliEvent::ToolCallStarted {
                agent: "orchestrator".to_string(),
                tool_name: "read".to_string(),
                input_summary: "notes.md".to_string(),
            },
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let replay: Vec<_> = subscribe_journal(&sid, tx).into_iter().map(|entry| entry.event).collect();
        assert!(matches!(
            &replay[0],
            CliEvent::AgentThinking { text, .. } if text == "first visible reasoning"
        ));
        assert!(matches!(
            &replay[1],
            CliEvent::ToolCallStarted { tool_name, .. } if tool_name == "read"
        ));
        assert!(
            rx.try_recv().is_err(),
            "replay must not also enter the live queue"
        );

        forward(
            &sid,
            CliEvent::ToolCallCompleted {
                agent: "orchestrator".to_string(),
                tool_name: "read".to_string(),
                input_summary: "notes.md".to_string(),
                success: true,
                output_summary: "read notes".to_string(),
                diff: None,
            },
        );
        assert!(matches!(
            rx.try_recv().map(|entry| entry.event),
            Ok(CliEvent::ToolCallCompleted { tool_name, .. }) if tool_name == "read"
        ));

        begin_foreground_replay(&sid);
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(subscribe_journal(&sid, tx).is_empty());
    }

    #[tokio::test]
    async fn journal_replays_only_currently_active_volume_workers() {
        let sid = format!("postbox-volume-replay-{}", uuid::Uuid::new_v4());
        let worker_id = "volume-batch-1-1".to_string();
        forward(
            &sid,
            CliEvent::VolumeWorkerLifecycle {
                batch_id: "batch-1".to_string(),
                worker_id: worker_id.clone(),
                label: "Worker 1".to_string(),
                item_id: "item-a".to_string(),
                status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
            },
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let replay: Vec<_> = subscribe_journal(&sid, tx).into_iter().map(|entry| entry.event).collect();
        assert!(matches!(
            replay.as_slice(),
            [CliEvent::VolumeWorkerLifecycle {
                worker_id: replay_worker,
                status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
                ..
            }] if replay_worker == &worker_id
        ));
        assert!(rx.try_recv().is_err(), "replay must not also be live");

        forward(
            &sid,
            CliEvent::VolumeWorkerLifecycle {
                batch_id: "batch-1".to_string(),
                worker_id,
                label: "Worker 1".to_string(),
                item_id: "item-a".to_string(),
                status: crate::runtime::VolumeWorkerLifecycleStatus::Cancelled,
            },
        );
        assert!(matches!(
            rx.recv().await.map(|entry| entry.event),
            Some(CliEvent::VolumeWorkerLifecycle {
                status: crate::runtime::VolumeWorkerLifecycleStatus::Cancelled,
                ..
            })
        ));

        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(
            subscribe_journal(&sid, tx).is_empty(),
            "terminal workers must not replay after reconnect"
        );
    }

    #[tokio::test]
    async fn authored_boundary_survives_a_full_replay_window() {
        let sid = format!("postbox-boundary-replay-{}", uuid::Uuid::new_v4());
        begin_foreground_replay(&sid);
        forward(
            &sid,
            CliEvent::WakeTurn {
                prompt: "Run the recurring check".to_string(),
                turn_id: Some("routine:daily-check:one".to_string()),
                origin: Some(crate::runtime::TurnOrigin::Routine {
                    routine_id: "daily-check".to_string(),
                    scheduled_for: "2026-08-26T11:00:00.000Z".to_string(),
                    schedule: "daily 05:00".to_string(),
                }),
            },
        );
        for index in 0..(FOREGROUND_REPLAY_CAP + 20) {
            forward(&sid, CliEvent::GatewayNotice(format!("event {index}")));
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let replay: Vec<_> = subscribe_journal(&sid, tx).into_iter().map(|entry| entry.event).collect();
        assert_eq!(replay.len(), FOREGROUND_REPLAY_CAP + 1);
        assert!(matches!(
            replay.first(),
            Some(CliEvent::WakeTurn { turn_id: Some(turn_id), .. })
                if turn_id == "routine:daily-check:one"
        ));
    }

    #[test]
    fn owned_replay_keeps_interleaved_tasks_and_completion_isolated() {
        let sid = format!("postbox-owned-replay-{}", uuid::Uuid::new_v4());
        let a = ExecutionScope::new("turn-a".into(), "task-a".into());
        let b = ExecutionScope::new("turn-b".into(), "task-b".into());
        let begin = |scope: &ExecutionScope| {
            begin_foreground_replay(&sid);
            forward_owned(&sid, Some(scope), CliEvent::WakeTurn {
                prompt: scope.turn_id.clone(), turn_id: Some(scope.turn_id.clone()), origin: None,
            });
        };
        let read = || CliEvent::ToolCallCompleted { agent: "coder".into(), tool_name: "read".into(),
            input_summary: "same-file.md".into(), success: true, output_summary: "read".into(), diff: None };
        begin(&a);
        forward_owned(&sid, Some(&a), read());
        begin(&b);
        forward_owned(&sid, Some(&b), read());
        forward_owned(&sid, Some(&a), CliEvent::Done);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let replay = subscribe_journal(&sid, tx);
        assert_eq!(replay.len(), 5);
        assert!(replay.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
        assert_eq!(replay[0].scope.as_ref(), Some(&a));
        assert_eq!(replay[2].scope.as_ref(), Some(&b));
        let mut reducer = crate::runtime::story::OwnedStoryReducer::default();
        let receipts: Vec<_> = replay.iter().flat_map(|entry| reducer.push(entry)).filter_map(|row| {
            if let crate::runtime::story::StoryEvent::Receipt { text, .. } = row { Some(text) } else { None }
        }).collect();
        assert_eq!(receipts, ["read 1 file"]);
        assert!(rx.try_recv().is_err());
        // Beginning a third turn retires completed A, not still-active B.
        begin_foreground_replay(&sid);
        let (tx, _rx) = mpsc::unbounded_channel();
        let remaining = subscribe_journal(&sid, tx);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|entry| entry.scope.as_ref() == Some(&b)));
        forward_owned(&sid, Some(&b), CliEvent::Done);
        let finish = rx.try_recv().unwrap();
        let rows = reducer.push(&finish);
        assert!(rows.iter().any(|row| matches!(row,
            crate::runtime::story::StoryEvent::Receipt { text, .. } if text == "read 1 file")));
        discard_session_work(&sid);
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(subscribe_journal(&sid, tx).is_empty());
    }

    #[tokio::test]
    async fn subscriber_bursts_do_not_drop_story_events() {
        let sid = format!("postbox-burst-test-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        subscribe(&sid, tx);

        for n in 0..10_000 {
            forward(
                &sid,
                CliEvent::AgentThinking {
                    agent: "Compass".to_string(),
                    text: format!("reasoning beat {n}"),
                },
            );
        }

        for n in 0..10_000 {
            match rx.try_recv() {
                Ok(CliEvent::AgentThinking { agent, text }) => {
                    assert_eq!(agent, "Compass");
                    assert_eq!(text, format!("reasoning beat {n}"));
                }
                other => panic!("event {n} was dropped or reordered: {other:?}"),
            }
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn user_steer_marker_round_trips_and_is_not_an_internal_prefix() {
        let content = user_steer_content("use tabs, not spaces");
        assert_eq!(strip_user_steer_marker(&content), Some("use tabs, not spaces"));
        assert_eq!(strip_user_steer_marker("use tabs, not spaces"), None);
        // Transcript renderers hide these runtime envelopes; the steer marker
        // must never collide with them or the user's message would vanish.
        let lower = content.to_ascii_lowercase();
        for hidden in ["[late ask answer]", "[queued wake]", "queued prompt queued_", "[background return]", "[user steer]"] {
            assert!(!lower.starts_with(hidden), "{hidden}");
        }
    }

    #[test]
    fn user_steers_dedupe_by_client_id_and_leave_coworker_notes_in_the_lane() {
        let sid = format!("postbox-user-steer-{}", uuid::Uuid::new_v4());
        steer(
            &sid,
            "orchestrator",
            SteerNote {
                message_id: String::new(),
                from: "coder".to_string(),
                subject: "status".to_string(),
                body: "halfway".to_string(),
            },
        );
        assert!(steer_user(&sid, "orchestrator", Some("turn_a"), "first"));
        assert!(!steer_user(&sid, "orchestrator", Some("turn_a"), "first"));
        assert!(steer_user(&sid, "orchestrator", Some("turn_b"), "second"));
        // Legacy callers without an id are never deduped.
        assert!(steer_user(&sid, "orchestrator", None, "third"));

        let first = take_first_user_steer(&sid, "orchestrator").unwrap();
        assert_eq!(first.body, "first");
        let rest = take_steer(&sid, "orchestrator");
        let bodies = rest.iter().map(|note| note.body.as_str()).collect::<Vec<_>>();
        assert_eq!(bodies, vec!["halfway", "second", "third"]);
        assert!(take_first_user_steer(&sid, "orchestrator").is_none());
    }
    // ── Group rooms: everyone hears everything, only some act ──────────

    #[test]
    fn room_steer_reaches_a_busy_lane_and_reports_an_idle_one() {
        let sid = format!("room-steer-busy-{}", uuid::Uuid::new_v4());
        let _theo = active_turn_guard(&sid, "researcher");
        assert_eq!(
            steer_room_user(&sid, "researcher", "turn_room_0001", RoomSteerKind::MentionAct, "use the dark palette"),
            RoomSteerOutcome::Delivered
        );
        // Idle target: nothing is parked; the caller starts a turn instead.
        assert_eq!(
            steer_room_user(&sid, "coder", "turn_room_0001", RoomSteerKind::Fyi, "use the dark palette"),
            RoomSteerOutcome::NotRunning
        );
        assert!(take_steer(&sid, "coder").is_empty());
        let notes = take_steer(&sid, "researcher");
        assert_eq!(notes.len(), 1);
        assert!(is_room_steer(&notes[0]));
        assert_eq!(room_steer_parts(&notes[0].subject).map(|(kind, _)| kind), Some(RoomSteerKind::MentionAct));
    }

    #[test]
    fn room_steer_is_idempotent_per_lane_not_per_message() {
        let sid = format!("room-steer-dedupe-{}", uuid::Uuid::new_v4());
        let _theo = active_turn_guard(&sid, "researcher");
        let _robin = active_turn_guard(&sid, "coder");
        // One message reaches every running lane once ...
        assert_eq!(steer_room_user(&sid, "researcher", "turn_room_0002", RoomSteerKind::Fyi, "hi"), RoomSteerOutcome::Delivered);
        assert_eq!(steer_room_user(&sid, "coder", "turn_room_0002", RoomSteerKind::MentionAct, "hi"), RoomSteerOutcome::Delivered);
        // ... and a retry of the same message never parks a second copy.
        assert_eq!(steer_room_user(&sid, "researcher", "turn_room_0002", RoomSteerKind::Fyi, "hi"), RoomSteerOutcome::Duplicate);
        assert_eq!(steer_room_user(&sid, "coder#2", "turn_room_0002", RoomSteerKind::MentionAct, "hi"), RoomSteerOutcome::Duplicate);
        assert_eq!(take_steer(&sid, "researcher").len(), 1);
        assert_eq!(take_steer(&sid, "coder").len(), 1);
    }

    #[test]
    fn room_steer_framing_separates_fyi_from_actionable() {
        let note = |kind: RoomSteerKind| SteerNote {
            message_id: String::new(),
            from: "user".into(),
            subject: room_steer_subject(kind, "turn_x@coder"),
            body: "make it dark mode".into(),
        };
        let fyi = user_steer_content_for(&note(RoomSteerKind::Fyi));
        assert!(fyi.starts_with("[FYI"));
        assert!(fyi.contains("not addressed to you"));
        assert!(fyi.contains("adapt immediately"));
        let act = user_steer_content_for(&note(RoomSteerKind::MentionAct));
        assert!(act.starts_with("[TOP PRIORITY"));
        let leader_fyi = user_steer_content_for(&note(RoomSteerKind::LeaderFyi));
        assert!(leader_fyi.contains("Do not take over"));
        assert!(leader_fyi.contains("record it"));
        assert!(user_steer_content_for(&note(RoomSteerKind::LeaderSolo)).contains("do not dispatch"));
        assert!(user_steer_content_for(&note(RoomSteerKind::EveryoneReplan)).contains("re-plan"));
        for kind in RoomSteerKind::ALL {
            let content = user_steer_content_for(&note(kind));
            assert_eq!(strip_user_steer_marker(&content), Some("make it dark mode"));
            assert_eq!(kind.is_actionable(), !matches!(kind, RoomSteerKind::Fyi | RoomSteerKind::LeaderFyi));
            assert_eq!(RoomSteerKind::parse(kind.as_str()), Some(kind));
            let lower = content.to_ascii_lowercase();
            for hidden in ["[late ask answer]", "[queued wake]", "[background return]", "[user steer]"] {
                assert!(!lower.starts_with(hidden));
            }
        }
        // A one-to-one note keeps the original marker.
        let direct = SteerNote { message_id: String::new(), from: "user".into(), subject: "message from user".into(), body: "x".into() };
        assert_eq!(user_steer_content_for(&direct), user_steer_content("x"));
    }

    #[test]
    fn room_steer_race_falls_back_exactly_once() {
        let sid = format!("room-steer-race-{}", uuid::Uuid::new_v4());
        let delivery = room_delivery_id("turn_room_race", "coder");
        {
            let _robin = active_turn_guard(&sid, "coder");
            assert_eq!(steer_room_user(&sid, "coder", "turn_room_race", RoomSteerKind::MentionAct, "add tests"), RoomSteerOutcome::Delivered);
            // Still running: its next round-top drain owns the note.
            assert_eq!(settle_parked_room_steer(&sid, "coder", &delivery), ParkedRoomSteer::Live);
        }
        // The turn ended before draining it: the watcher takes it (once) and
        // starts a normal turn; nothing remains for any later drain.
        let ParkedRoomSteer::Orphaned(note) = settle_parked_room_steer(&sid, "coder", &delivery) else {
            panic!("an undrained note of a finished lane is orphaned");
        };
        assert_eq!(note.body, "add tests");
        assert_eq!(settle_parked_room_steer(&sid, "coder", &delivery), ParkedRoomSteer::Consumed);
        assert!(take_steer(&sid, "coder").is_empty());

        // The other order: the running turn drains first, the watcher no-ops.
        let delivery = room_delivery_id("turn_room_race_2", "coder");
        let _robin = active_turn_guard(&sid, "coder");
        assert_eq!(steer_room_user(&sid, "coder", "turn_room_race_2", RoomSteerKind::MentionAct, "add docs"), RoomSteerOutcome::Delivered);
        assert_eq!(take_steer(&sid, "coder").len(), 1);
        assert_eq!(settle_parked_room_steer(&sid, "coder", &delivery), ParkedRoomSteer::Consumed);
    }

    #[test]
    fn room_steers_are_never_adopted_by_one_to_one_wake_paths() {
        let sid = format!("room-steer-isolated-{}", uuid::Uuid::new_v4());
        {
            let _robin = active_turn_guard(&sid, "coder");
            assert_eq!(steer_room_user(&sid, "coder", "turn_room_iso", RoomSteerKind::Fyi, "fyi"), RoomSteerOutcome::Delivered);
        }
        // Not a wake reason, not re-homed to the owner, not adopted as a turn.
        assert!(!has_pending_steer(&sid, "coder"));
        assert_eq!(rehome_orphan_steers_to_orchestrator(&sid), 0);
        assert!(take_first_user_steer(&sid, "coder").is_none());
        assert!(take_first_user_steer(&sid, "orchestrator").is_none());
        assert_eq!(take_steer(&sid, "coder").len(), 1, "it stays for the room watcher");
    }

}
