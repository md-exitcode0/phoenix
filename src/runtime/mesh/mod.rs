//! Mesh turn-runner — real provider-backed agent turns through the gateway.
//!
//! This is the piece that makes the actor mesh *live*: [`MeshRunner`] implements
//! [`AgentTurnHandler`], so the gateway can wake any agent (orchestrator or
//! specialist) for one turn against the real provider.
//!
//! The turn model:
//! - Every agent owns a **durable session** (the orchestrator uses the main
//!   session; each specialist gets `<main>__<agent>`). An incoming message is
//!   pushed into that session — `UserInput` as a user message, a `talk` as a
//!   Talk envelope — and the provider loop runs over the rebuilt transcript.
//! - **Local tools** (read/grep/bash/web_*…) execute inline within the turn,
//!   with results fed back natively and recorded in the session.
//! - A **`talk` call yields the turn** instead of nesting: it becomes an
//!   outbound [`AgentMessage`] the gateway routes, the talk is recorded in the
//!   sender's transcript, and the sender goes dormant. When the reply arrives,
//!   the sender is re-woken with its full session — suspension is free because
//!   the transcript *is* the state.
//! - A **final** is reported to whoever invoked this agent (sticky delegator:
//!   the user for the orchestrator, the delegating agent for a specialist), so
//!   chains like orchestrator → researcher → coder route results back up — or
//!   straight to the user when an agent addresses them directly.
//!
//! Memory: the runner preloads librarian context before the gateway run (the
//! orchestrator receives it via `with_loaded_memories`) and runs the librarian
//! save pass after — see `execute_mesh_slice`. Inside a turn there are no
//! librarian hops.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::gateway::AgentTurnHandler;
use super::mailbox::{AgentAddress, AgentMessage, MessageKind};
use super::prompt::assemble_prompt_with_context;
use super::runner::{
    assistant_session_content, coerce_plain_text_final_response, final_response_from_tool_input,
    first_line, looks_like_json_envelope, normalize_tool_input, raw_contains_tool_intent,
    requested_tool_call_from_native, sanitize_error, strip_visible_reasoning_blocks,
};
use crate::librarian::LoadedMemories;
use crate::providers::{ChatMessage, LLMProvider};
use crate::runtime::delegation::{agent_display_name, specialist_is_executable};
use crate::runtime::limits::AGENT_TEMPERATURE;
use crate::runtime::r#loop::{native_tool_error_content, native_tool_result_content};
use crate::runtime::tool_failure_guard::{FailureLoopDecision, ToolFailureGuard};
use crate::runtime::{
    parse_agent_turn_response, summarize_tool_input, AgentSpec, AgentTarget, AgentTurnResponse,
    CliEvent, FinalResponse, RequestedToolCall, TaskEnvelope, ToolCall, ToolCallResult,
};
use crate::session::{Message, Session, SessionStore};
use crate::sub_agents::specialist_config;
use crate::tools::{
    tool_definitions_for_agent, AgentControlAction, AgentControlInput, MessageAgentInput,
    PermissionMode, TalkInput, ToolExecutor, VolumeWorkInput,
};

mod background;
mod helpers;
mod iris_design;
mod lanes;
mod turn_loop;
mod volume;

pub(crate) use helpers::cap_chars;
pub(crate) use helpers::edit_display_diff;
use helpers::*;

/// Drives one real agent turn per gateway wake. Owns the sticky delegator map
/// (who each agent reports its final to) across the whole gateway run.
#[derive(Clone)]
struct ReplyOwner {
    address: AgentAddress,
    handoff_id: String,
}

pub struct MeshRunner {
    provider: Arc<dyn LLMProvider>,
    workspace_root: PathBuf,
    state_root: PathBuf,
    main_session_id: String,
    orchestrator_spec: AgentSpec,
    specialist_model: Option<String>,
    reasoning_effort: Option<String>,
    /// Primary model per individual specialist (`[profile.llm.agent_models]`),
    /// keyed by role label ("coder"); wins over `specialist_model`.
    agent_models: HashMap<String, String>,
    /// Reasoning effort per lane (`[profile.llm.efforts]`): agent label →
    /// "specialist" → the global `reasoning_effort`.
    role_efforts: HashMap<String, String>,
    /// Explicit usable context ceilings per lane. Missing specialist lanes use
    /// their provider/model catalog maximum; the orchestrator falls back to
    /// the legacy/global ceiling for backward compatibility.
    role_context_windows: HashMap<String, u64>,
    permission_mode: PermissionMode,
    interaction_mode: crate::runtime::InteractionMode,
    event_tx: Option<mpsc::Sender<CliEvent>>,
    group_status_tx: Option<mpsc::Sender<CliEvent>>,
    /// Cheap image model for screenshot grounding (None = vision disabled).
    vision: Option<crate::runtime::vision::VisionConfig>,
    /// Acting model is multimodal: attach the latest screenshot natively and
    /// skip the caption sidecar (config `native_vision`). Applied per turn
    /// only when the turn's provider `supports_native_images`.
    native_vision: bool,
    /// Classified composer image attachments for this authored run. Never
    /// recovered by interpreting model text or paths mentioned in prose.
    design_reference_paths: Vec<PathBuf>,
    /// Nested reply owners. Completing a peer question restores the outer
    /// task's owner instead of replacing it with an acknowledgement loop.
    /// Mutex (never held across an await): concurrent specialist turns update
    /// it from a shared `&self`.
    delegators: std::sync::Mutex<HashMap<AgentAddress, Vec<ReplyOwner>>>,
    /// Librarian-preloaded memory context, injected into the orchestrator's
    /// prompt only (specialists get their context via the delegation message).
    loaded_memories: Option<LoadedMemories>,
    /// (input, output) tokens across every provider call this run — shared so
    /// the runner can read totals after the gateway consumes the MeshRunner.
    /// `tokens.0` is the SUM of per-call inputs (a burn metric that can exceed
    /// the window on a multi-round turn); `peak_input` below is the per-call MAX
    /// for the window gauge.
    tokens: Arc<(AtomicU32, AtomicU32)>,
    /// MAX single-call input this run (not the sum): the real context-window
    /// usage, always ≤ window. Kept separate so the gauge never reads a
    /// multi-round sum as ">100% of the window".
    peak_input: Arc<AtomicU32>,
    /// Active model's context window (tokens) — the auto-compaction ceiling.
    context_window: u64,
    /// Provider for SPECIALIST turns (None = same as the orchestrator's).
    /// The brain/executor split: strong model thinks, cheap provider executes.
    specialist_provider: Option<Arc<dyn LLMProvider>>,
    /// Per-agent provider overrides keyed by role label ("coder", "browser"):
    /// an agent with its own account-fallback chain checks here first, before
    /// the shared specialist provider.
    agent_providers: HashMap<String, Arc<dyn LLMProvider>>,
    /// Provider for the compaction summarizer (librarian tier; None = main).
    compaction_provider: Option<Arc<dyn LLMProvider>>,
    /// Cheap model for the compaction summary (librarian tier); falls back to
    /// the agent's own model when unset.
    compaction_model: Option<String>,
    /// Parallel-instance isolation: when set (a background job running as a
    /// SECOND+ instance of a specialist), every specialist session this runner
    /// touches is suffixed `--<scope>` so concurrent jobs never share a
    /// single-writer session file. None = the canonical durable sessions.
    job_scope: Option<String>,
    /// Exact postbox token bridging a detached root job from synchronous
    /// registration until its first specialist turn owns the live lane.
    /// Baton-chain turns do not get a token: live ownership alone routes them.
    starting_turn: Option<crate::runtime::postbox::StartingTurnGuard>,
    /// Shared group thread, when this mesh run was addressed to a company
    /// group. Member sessions remain individually durable; this canonical
    /// transcript is injected into every member's turn.
    group_context: Option<crate::runtime::group_conversation::GroupTurnContext>,
    /// Authored room turn, independent of peer message causation IDs.
    group_authored_turn_id: std::sync::Mutex<Option<String>>,
    direct_agent_context: Option<crate::runtime::agent_conversation::AgentTurnContext>,
    #[cfg(test)]
    group_direct_context_sessions: HashMap<String, String>,
}

impl MeshRunner {
    pub fn new(
        provider: Arc<dyn LLMProvider>,
        workspace_root: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
        main_session_id: impl Into<String>,
        orchestrator_spec: AgentSpec,
    ) -> Self {
        Self {
            provider,
            workspace_root: workspace_root.into(),
            state_root: state_root.into(),
            main_session_id: main_session_id.into(),
            orchestrator_spec,
            specialist_model: None,
            reasoning_effort: None,
            agent_models: HashMap::new(),
            role_efforts: HashMap::new(),
            role_context_windows: HashMap::new(),
            permission_mode: PermissionMode::Workspace,
            interaction_mode: crate::runtime::InteractionMode::Execute,
            event_tx: None,
            group_status_tx: None,
            vision: None,
            native_vision: false,
            design_reference_paths: Vec::new(),
            delegators: std::sync::Mutex::new(HashMap::new()),
            loaded_memories: None,
            tokens: Arc::new((AtomicU32::new(0), AtomicU32::new(0))),
            peak_input: Arc::new(AtomicU32::new(0)),
            context_window: 200_000,
            compaction_model: None,
            specialist_provider: None,
            agent_providers: HashMap::new(),
            compaction_provider: None,
            job_scope: None,
            starting_turn: None,
            group_context: None,
            group_authored_turn_id: std::sync::Mutex::new(None),
            direct_agent_context: None,
            #[cfg(test)]
            group_direct_context_sessions: HashMap::new(),
        }
    }

    fn remember_group_authored_turn(&self, incoming: &AgentMessage) {
        if matches!(incoming.kind, MessageKind::UserInput) {
            if let Some(id) = incoming.causation_id.as_ref().filter(|id| !id.trim().is_empty()) {
                *self.group_authored_turn_id.lock().unwrap_or_else(|p|p.into_inner()) = Some(id.clone());
            }
        }
    }

    pub(crate) fn with_group_authored_turn_id(self, turn_id: Option<&str>) -> Self {
        *self.group_authored_turn_id.lock().unwrap_or_else(|p| p.into_inner()) = turn_id.map(str::to_string);
        self
    }

    pub fn with_group_context(
        mut self,
        group: Option<crate::runtime::group_conversation::GroupTurnContext>,
    ) -> Self {
        self.group_context = group;
        self
    }

    pub fn with_design_reference_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.design_reference_paths = paths;
        self
    }

    #[cfg(test)]
    pub(super) fn with_group_direct_context_session(
        mut self,
        internal_role: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        self.group_direct_context_sessions
            .insert(internal_role.into(), session_id.into());
        self
    }

    pub fn with_direct_agent_context(
        mut self,
        agent: Option<crate::runtime::agent_conversation::AgentTurnContext>,
    ) -> Self {
        self.direct_agent_context = agent;
        self
    }

    pub fn with_specialist_provider(mut self, provider: Option<Arc<dyn LLMProvider>>) -> Self {
        self.specialist_provider = provider;
        self
    }

    pub fn with_agent_providers(
        mut self,
        providers: HashMap<String, Arc<dyn LLMProvider>>,
    ) -> Self {
        self.agent_providers = providers;
        self
    }

    pub fn with_compaction_provider(mut self, provider: Option<Arc<dyn LLMProvider>>) -> Self {
        self.compaction_provider = provider;
        self
    }

    pub fn with_context_window(mut self, window_tokens: u64) -> Self {
        if window_tokens > 0 {
            self.context_window = window_tokens;
        }
        self
    }

    pub fn with_compaction_model(mut self, model: Option<String>) -> Self {
        self.compaction_model = model.filter(|m| !m.trim().is_empty());
        self
    }

    /// Shared token counter — clone BEFORE moving the runner into the gateway.
    pub fn token_counter(&self) -> Arc<(AtomicU32, AtomicU32)> {
        Arc::clone(&self.tokens)
    }

    /// Shared PEAK single-call input counter (context-window usage), separate
    /// from the summed `token_counter`. Clone before moving the runner.
    pub fn peak_input_counter(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.peak_input)
    }

    pub fn with_loaded_memories(mut self, loaded: Option<LoadedMemories>) -> Self {
        self.loaded_memories = loaded;
        self
    }

    pub fn with_vision(mut self, vision: Option<crate::runtime::vision::VisionConfig>) -> Self {
        self.vision = vision;
        self
    }

    /// Acting model is multimodal: attach screenshots natively, skip the
    /// caption sidecar (config `native_vision`).
    pub fn with_native_vision(mut self, enabled: bool) -> Self {
        self.native_vision = enabled;
        self
    }

    pub fn with_specialist_model(mut self, model: Option<String>) -> Self {
        self.specialist_model = model.filter(|m| !m.trim().is_empty());
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort.filter(|e| !e.trim().is_empty());
        self
    }

    pub fn with_agent_models(mut self, models: HashMap<String, String>) -> Self {
        self.agent_models = models;
        self
    }

    pub fn with_role_efforts(mut self, efforts: HashMap<String, String>) -> Self {
        self.role_efforts = efforts;
        self
    }

    pub fn with_role_context_windows(mut self, windows: HashMap<String, u64>) -> Self {
        self.role_context_windows = windows;
        self
    }

    fn context_window_cap_for_addr(&self, addr: &AgentAddress) -> Option<u64> {
        // Context capacity is a model/lane setting, not a group-membership
        // property. Control redundant injected history separately; a hidden
        // group cap would prematurely compact explicitly larger windows.
        match addr {
            AgentAddress::Specialist(_) => self
                .role_context_windows
                .get(&addr.label())
                .or_else(|| self.role_context_windows.get("specialist"))
                .copied(),
            _ => self
                .role_context_windows
                .get("orchestrator")
                .copied()
                .or(Some(self.context_window)),
        }
    }

    /// The reasoning effort for one agent's turn: its own lane in
    /// `[profile.llm.efforts]` → the shared specialist lane (specialists
    /// only) → orchestrator lane (orchestrator only) → the global default.
    fn effort_for_addr(&self, addr: &AgentAddress) -> Option<String> {
        let lane = match addr {
            AgentAddress::Specialist(_) => self
                .role_efforts
                .get(&addr.label())
                .or_else(|| self.role_efforts.get("specialist")),
            _ => self.role_efforts.get("orchestrator"),
        };
        lane.cloned().or_else(|| self.reasoning_effort.clone())
    }

    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    pub fn with_interaction_mode(mut self, mode: crate::runtime::InteractionMode) -> Self {
        self.interaction_mode = mode;
        self
    }

    pub fn with_event_channel(mut self, tx: mpsc::Sender<CliEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn with_group_status_channel(mut self,tx:mpsc::Sender<CliEvent>)->Self {
        self.group_status_tx=Some(tx);
        self
    }

    fn emit(&self, event: CliEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(event);
        }
    }

    fn spec_for(&self, addr: &AgentAddress) -> Result<(AgentSpec, AgentTarget)> {
        let (mut spec, target) = match addr {
            AgentAddress::Orchestrator => {
                (self.orchestrator_spec.clone(), AgentTarget::Orchestrator)
            }
            AgentAddress::Specialist(agent) => {
                if !specialist_is_executable(*agent) {
                    bail!(
                        "specialist `{}` is on the roster but not executable yet",
                        addr.label()
                    );
                }
                let mut spec = specialist_config(*agent).spec;
                if let Some(model) = &self.specialist_model {
                    spec.default_model = model.clone();
                }
                // An individual pick in [profile.llm.agent_models] beats the
                // shared specialist model for that one agent.
                if let Some(model) = self.agent_models.get(&addr.label()) {
                    spec.default_model = model.clone();
                }
                (spec, AgentTarget::Specialist(*agent))
            }
            AgentAddress::User => bail!("the user is not a runnable agent"),
        };

        // Compiled prompts teach craft; the directory owns editable identity,
        // accountability, and relationships. Resolve by stable agent id for a
        // direct turn, by membership in a group, then by the founding runtime
        // role for internal/background work.
        if let Ok(snapshot) =
            crate::runtime::company::global().and_then(|company| company.directory_snapshot())
        {
            let role = addr.label();
            let agent_id = self
                .direct_agent_context
                .as_ref()
                .filter(|context| {
                    context.internal_role == role
                        || (role == "orchestrator" && context.internal_role == "phoenix")
                })
                .map(|context| context.agent_id.as_str())
                .or_else(|| {
                    self.group_context.as_ref().and_then(|group| {
                        group
                            .participants
                            .iter()
                            .find(|participant| {
                                participant.internal_role == role
                                    || (role == "orchestrator"
                                        && participant.internal_role == "phoenix")
                            })
                            .map(|participant| participant.agent_id.as_str())
                    })
                })
                .or_else(|| {
                    snapshot
                        .agents
                        .iter()
                        .find(|agent| {
                            agent.profile.internal_role == role
                                || (role == "orchestrator"
                                    && agent.profile.internal_role == "phoenix")
                        })
                        .map(|agent| agent.profile.agent_id.as_str())
                });
            if let Some(agent_id) = agent_id {
                if let Some(agent) = snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.profile.agent_id == agent_id)
                {
                    anyhow::ensure!(
                        agent.profile.lifecycle
                            == crate::runtime::company_directory::LifecycleState::Active,
                        "coworker `{}` is {:?} and cannot receive or continue work",
                        agent.profile.display_name,
                        agent.profile.lifecycle
                    );
                }
                if let Some(block) =
                    crate::runtime::company_directory::runtime_identity_block(&snapshot, agent_id)
                {
                    spec.system_prompt.push_str(&block);
                }
            }
        }
        Ok((spec, target))
    }

    fn load_session(
        &self,
        store: &mut SessionStore,
        addr: &AgentAddress,
        spec: &AgentSpec,
    ) -> Result<(Session, Option<String>)> {
        // A call from a room to somebody who is not a member is delivered to
        // that coworker's own endless conversation. It must never mint a
        // hidden `<group>__<specialist>` history: that silently leaks a private
        // exchange into the room's execution scope and makes the coworker lose
        // the personal context the user expects them to retain.
        let outside_group_session = self.group_outside_personal_session_id(addr);
        match addr {
            AgentAddress::Orchestrator => {
                if let Some(group) = self.group_context.as_ref().filter(|_| outside_group_session.is_none()) {
                    // The public room is a contribution projection, never an
                    // actor's mutable working transcript. Phoenix needs the
                    // same isolation as every other room participant.
                    let id = crate::session::actor_session_id_scoped(
                        &group.canonical_session_id, "phoenix", self.job_scope.as_deref(),
                    );
                    store.load_one_if_absent(&id)?;
                    let previous = store.get(&id).map(|session| session.model.clone());
                    return Ok((store.load_or_create_main(&id, &spec.default_model, &spec.system_prompt)?, previous));
                }
                let personal_session = self.personal_canonical_session_id(addr);
                // A root task already owns its requested session. Only a
                // peer delivery from a different direct coworker may redirect
                // Phoenix to its canonical personal thread. Otherwise a fresh
                // CLI/diagnostic conversation silently loads old main history.
                let session_id = outside_group_session
                    .as_deref()
                    .or_else(|| {
                        (self.group_context.is_none()
                            && self.direct_agent_context.is_some()
                            && !self.direct_context_matches(addr))
                            .then_some(personal_session.as_deref())
                            .flatten()
                    })
                    .unwrap_or(&self.main_session_id);
                store.load_one_if_absent(session_id)?;
                let previous = store.get(session_id).map(|s| s.model.clone());
                Ok((
                    store.load_or_create_main(
                        session_id,
                        &spec.default_model,
                        &spec.system_prompt,
                    )?,
                    previous,
                ))
            }
            AgentAddress::Specialist(agent) => {
                if let Some(session_id) = outside_group_session.as_deref() {
                    store.load_one_if_absent(session_id)?;
                    let previous = store.get(session_id).map(|session| session.model.clone());
                    return Ok((
                        store.load_or_create_specialist_with_id(
                            session_id,
                            *agent,
                            &spec.default_model,
                            &spec.system_prompt,
                        )?,
                        previous,
                    ));
                }
                if self.direct_agent_context.as_ref().is_some_and(|context| {
                    context.internal_role == crate::runtime::delegation::specialist_label(*agent)
                        && context.canonical_session_id == self.main_session_id
                }) {
                    store.load_one_if_absent(&self.main_session_id)?;
                    let previous = store
                        .get(&self.main_session_id)
                        .map(|session| session.model.clone());
                    return Ok((
                        store.load_or_create_specialist_with_id(
                            &self.main_session_id,
                            *agent,
                            &spec.default_model,
                            &spec.system_prompt,
                        )?,
                        previous,
                    ));
                }
                // Outside a room, every named coworker always works in their
                // own endless canonical conversation. The former
                // `<caller>__<specialist>` session hid the exchange from both
                // coworkers' sidebars and discarded the receiver's personal
                // context. Scoped sessions remain only for room members and
                // anonymous volume workers.
                if self.group_context.is_none()
                    && !crate::sub_agents::volume_worker::is_agent(*agent)
                {
                    if let Some(session_id) = self.personal_canonical_session_id(addr) {
                        store.load_one_if_absent(&session_id)?;
                        let previous = store.get(&session_id).map(|session| session.model.clone());
                        return Ok((
                            store.load_or_create_specialist_with_id(
                                &session_id,
                                *agent,
                                &spec.default_model,
                                &spec.system_prompt,
                            )?,
                            previous,
                        ));
                    }
                }
                let scope = self.job_scope.as_deref();
                let session_id = crate::session::specialist_session_id_scoped(
                    &self.main_session_id,
                    *agent,
                    scope,
                );
                store.load_one_if_absent(&session_id)?;
                let previous = store.get(&session_id).map(|s| s.model.clone());
                Ok((
                    store.load_or_create_specialist_scoped(
                        &self.main_session_id,
                        *agent,
                        scope,
                        &spec.default_model,
                        &spec.system_prompt,
                    )?,
                    previous,
                ))
            }
            AgentAddress::User => bail!("the user has no agent session"),
        }
    }

    fn direct_context_matches(&self, addr: &AgentAddress) -> bool {
        self.direct_agent_context.as_ref().is_some_and(|context| {
            let role = addr.label();
            context.internal_role == role
                || (matches!(addr, AgentAddress::Orchestrator)
                    && context.internal_role == "phoenix")
        })
    }

    /// The directory is the source of truth for each visible coworker's
    /// endless thread. Peer calls must join that thread, not mint a hidden
    /// caller-scoped transcript.
    fn personal_canonical_session_id(&self, addr: &AgentAddress) -> Option<String> {
        let role = match addr {
            AgentAddress::Orchestrator => "phoenix".to_string(),
            AgentAddress::Specialist(agent) => {
                crate::runtime::delegation::specialist_label(*agent).to_string()
            }
            AgentAddress::User => return None,
        };
        let company = crate::runtime::company::global().ok()?;
        let snapshot = company.directory_snapshot().ok()?;
        let profile = snapshot.agents.into_iter().find(|agent| {
            agent.profile.internal_role == role
                || (role == "phoenix" && agent.profile.internal_role == "orchestrator")
        })?;
        profile.profile.canonical_session_id.or_else(|| {
            company
                .ensure_agent_canonical_session(&profile.profile.agent_id)
                .ok()
        })
    }

    fn group_member_direct_session_id(&self, addr: &AgentAddress) -> Option<String> {
        #[cfg(test)]
        if let Some(session_id) = self.group_direct_context_sessions.get(&addr.label()) {
            return Some(session_id.clone());
        }
        self.personal_canonical_session_id(addr)
    }

    /// Resolve the canonical personal conversation for a coworker contacted
    /// from outside the current room. Room members deliberately keep their
    /// group-scoped working session plus the shared transcript injection.
    fn group_outside_personal_session_id(&self, addr: &AgentAddress) -> Option<String> {
        let group = self.group_context.as_ref()?;
        let role = match addr {
            AgentAddress::Orchestrator => "phoenix".to_string(),
            AgentAddress::Specialist(agent) => {
                crate::runtime::delegation::specialist_label(*agent).to_string()
            }
            AgentAddress::User => return None,
        };
        if group.participants.iter().any(|participant| {
            participant.internal_role == role
                || (role == "phoenix" && participant.internal_role == "orchestrator")
        }) {
            return None;
        }
        crate::runtime::company::global()
            .ok()
            .and_then(|company| company.directory_snapshot().ok())
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .into_iter()
                    .find(|agent| {
                        agent.profile.internal_role == role
                            || (role == "phoenix" && agent.profile.internal_role == "orchestrator")
                    })
                    .map(|agent| {
                        agent
                            .profile
                            .canonical_session_id
                            .unwrap_or_else(|| format!("agent-{}", agent.profile.agent_id))
                    })
            })
    }

    /// Resolve who this agent's final reports to, updating the sticky map from
    /// the incoming message.
    fn resolve_reply_to(&self, addr: &AgentAddress, incoming: &AgentMessage) -> AgentAddress {
        let mut delegators = self.delegators.lock().unwrap_or_else(|p| p.into_inner());
        match &incoming.kind {
            MessageKind::UserInput => {
                delegators.insert(addr.clone(), vec![ReplyOwner { address: AgentAddress::User, handoff_id: incoming.correlation_id().into() }]);
                AgentAddress::User
            }
            MessageKind::Talk {
                reply_expected: true,
            } => {
                // A callee may ask its caller a genuine follow-up question.
                // Preserve the caller's outer owner while servicing that
                // nested request; overwriting it creates a return ping-pong.
                let owners = delegators.entry(addr.clone()).or_default();
                let id = incoming.correlation_id();
                if id.is_empty() || !owners.iter().any(|owner| owner.handoff_id == id && owner.address == incoming.from) {
                    owners.push(ReplyOwner { address: incoming.from.clone(), handoff_id: id.into() });
                }
                incoming.from.clone()
            }
            MessageKind::Talk {
                reply_expected: false,
            } => {
                // A correlated no-reply talk is the answer travelling back
                // along a mode-1 foreground chain. Keep this agent's existing
                // upstream owner so nested A -> B -> C work returns C -> B ->
                // A before A can answer the user. Treating every no-reply talk
                // as a fresh one-way notice skipped A and exposed B's final.
                if incoming
                    .reply_to
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
                {
                    if let Some(upstream) = delegators.get(addr).and_then(|owners| owners.last()).cloned() {
                        return upstream.address;
                    }
                }
                // A no-reply message is one-way. The recipient may still do
                // the requested work, but its completion is delivered once to
                // the user (or captured by the detached-job sink) instead of
                // automatically pinging the sender. Routing this final back to
                // `incoming.from` created an unbounded Avery -> Phoenix ->
                // Avery acknowledgement loop: each perfectly valid final was
                // interpreted as fresh work for the peer.
                delegators.entry(addr.clone()).or_default().push(ReplyOwner { address: AgentAddress::User, handoff_id: incoming.correlation_id().into() });
                AgentAddress::User
            }
        }
    }

    fn complete_reply_owner(&self, addr: &AgentAddress, owner: &AgentAddress) {
        let mut delegators = self.delegators.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(owners) = delegators.get_mut(addr) {
            if owners.last().is_some_and(|frame| &frame.address == owner) { owners.pop(); }
            if owners.is_empty() { delegators.remove(addr); }
        }
    }

    fn restore_reply_owners(&self, addr: &AgentAddress, session: &Session) -> Result<()> {
        anyhow::ensure!(session.reply_owners.len() <= 64, "saved reply ownership exceeds nesting limit");
        let owners = session.reply_owners.iter().map(|frame| {
            anyhow::ensure!(frame.handoff_id.len() <= 512, "saved reply handoff identity exceeds safe bound");
            let address = AgentAddress::from_talk_name(&frame.owner)
                .context("saved reply owner is not a known runtime address")?;
            Ok(ReplyOwner { address, handoff_id: frame.handoff_id.clone() })
        }).collect::<Result<Vec<_>>>()?;
        let mut delegators = self.delegators.lock().unwrap_or_else(|p| p.into_inner());
        if owners.is_empty() { delegators.remove(addr); }
        else { delegators.insert(addr.clone(), owners); }
        Ok(())
    }

    fn checkpoint_reply_owners(&self, addr: &AgentAddress, session: &mut Session) -> Result<()> {
        let delegators = self.delegators.lock().unwrap_or_else(|p| p.into_inner());
        let owners = delegators.get(addr).map(Vec::as_slice).unwrap_or(&[]);
        anyhow::ensure!(owners.len() <= 64, "reply ownership exceeds nesting limit");
        session.reply_owners = owners.iter().map(|frame| crate::session::ReplyOwnerFrame {
            owner: frame.address.label(), handoff_id: frame.handoff_id.clone(),
        }).collect();
        Ok(())
    }

    fn record_incoming(session: &mut Session, addr: &AgentAddress, incoming: &AgentMessage) {
        match &incoming.kind {
            MessageKind::UserInput => {
                // The runner persists the user message to the durable session
                // BEFORE the librarian preload (so an early Esc can't lose it).
                // By the time this mesh turn loads the session it is already the
                // tail message — don't double-record it. (Other mesh entry points
                // — crons, tests — submit user input without pre-persisting, so
                // the push still happens for them.)
                let already_recorded = matches!(
                    session.messages.last(),
                    Some(Message::User { content }) if *content == incoming.body
                );
                if !already_recorded {
                    session.push_message(Message::User {
                        content: incoming.body.clone(),
                    });
                }
            }
            MessageKind::Talk { reply_expected } => session.push_message(Message::Talk {
                from: incoming.from.label(),
                to: addr.label(),
                subject: incoming.subject.clone(),
                body: incoming.body.clone(),
                reply_expected: *reply_expected,
                handoff_id: incoming.correlation_id().to_string(),
                reply_to: incoming.reply_to.clone(),
                causation_id: incoming.causation_id.clone(),
                status: if crate::tools::is_transport_message(&incoming.body) {
                    "message_received".to_string()
                } else if incoming.reply_to.is_some() {
                    if incoming.is_failed_result() { "blocked" } else { "done" }.to_string()
                } else {
                    "working".to_string()
                },
            }),
        }
    }

    /// Push runtime feedback the model sees next round (and a matching native
    /// tool error when the call came through the native channel).
    fn push_feedback(
        session: &mut Session,
        native_tool_messages: &mut Vec<ChatMessage>,
        call_id: Option<&String>,
        tool_name: &str,
        feedback: &str,
    ) {
        session.push_message(Message::ToolResult {
            tool_name: "response_validation".to_string(),
            input: "{}".to_string(),
            success: false,
            output: feedback.to_string(),
        });
        if let Some(call_id) = call_id {
            native_tool_messages.push(ChatMessage::tool_result(
                call_id.clone(),
                native_tool_error_content(tool_name, feedback),
            ));
        }
    }

    /// Push a SUCCESSFUL runtime acknowledgement the model sees next round
    /// (and the matching native tool result when the call came through the
    /// native channel) — `push_feedback`'s success twin, for tool calls the
    /// runtime absorbed rather than rejected (e.g. a background spawn).
    fn push_tool_ack(
        session: &mut Session,
        native_tool_messages: &mut Vec<ChatMessage>,
        call_id: Option<&String>,
        tool_name: &str,
        input: &str,
        ack: &str,
    ) {
        Self::push_tool_outcome(
            session,
            native_tool_messages,
            call_id,
            tool_name,
            input,
            true,
            ack,
        );
    }

    /// Record the actual outcome of a runtime-owned tool. Unlike
    /// `push_tool_ack`, this also represents honest failures for tools such as
    /// memory recall whose request was valid but whose backing service was not
    /// available.
    fn push_tool_outcome(
        session: &mut Session,
        native_tool_messages: &mut Vec<ChatMessage>,
        call_id: Option<&String>,
        tool_name: &str,
        input: &str,
        success: bool,
        output: &str,
    ) {
        // `input` is the call's forensic identity — the ask_user question, the
        // talk target+subject, the recall query. It used to be dropped ("{}"),
        // which made session records unreadable after the fact: an ask the
        // user dismissed left no trace of WHAT was asked (2026-07-07).
        session.push_message(Message::ToolResult {
            tool_name: tool_name.to_string(),
            input: input.to_string(),
            success,
            output: output.to_string(),
        });
        if let Some(call_id) = call_id {
            native_tool_messages.push(ChatMessage::tool_result(
                call_id.clone(),
                native_tool_result_content(&ToolCallResult {
                    tool_name: tool_name.to_string(),
                    input_summary: input.to_string(),
                    success,
                    output: output.to_string(),
                }),
            ));
        }
    }

    /// Convert one finished turn's `talk` into an outbound mesh message, after
    /// routing validation. Returns feedback for the model when the talk is
    /// invalid (unknown / self / non-executable target).
    fn talk_to_outbound(
        addr: &AgentAddress,
        talk: &TalkInput,
    ) -> std::result::Result<AgentMessage, String> {
        let Some(target) = AgentAddress::from_talk_name(&talk.to) else {
            let valid = crate::runtime::delegation::valid_talk_target_names().join(", ");
            return Err(format!(
                "Unknown or disabled talk target `{}`. Current runtime targets: {valid}.",
                talk.to,
            ));
        };
        if &target == addr {
            return Err(format!(
                "You sent `talk` to yourself (`{}`). Talk routes work to ANOTHER agent — pick the right target or finish with `final_answer`.",
                talk.to
            ));
        }
        if let AgentAddress::Specialist(agent) = target {
            if !specialist_is_executable(agent) {
                return Err(format!(
                    "Specialist `{}` is on the roster but not executable yet. Route to coder, researcher, or browser, or handle it yourself.",
                    talk.to
                ));
            }
            let role = crate::runtime::delegation::specialist_label(agent);
            if let Ok(snapshot) =
                crate::runtime::company::global().and_then(|company| company.directory_snapshot())
            {
                if let Some(record) = snapshot
                    .agents
                    .iter()
                    .find(|record| record.profile.internal_role == role)
                {
                    if record.profile.lifecycle
                        != crate::runtime::company_directory::LifecycleState::Active
                    {
                        return Err(format!(
                            "Coworker `{}` is {:?} and cannot receive new work. Restore or finish provisioning them first.",
                            record.profile.display_name, record.profile.lifecycle
                        ));
                    }
                }
            }
        }
        Ok(AgentMessage::talk(
            addr.clone(),
            target,
            talk.subject.clone(),
            talk.body.clone(),
            talk.reply_expected(),
        ))
    }

    /// Cross the durable company-message boundary before a handoff becomes a
    /// visible lifecycle row. This keeps the UI id, session id, and gateway
    /// delivery id identical instead of minting an unrelated semantic guess.
    fn prepare_company_handoff_with_store(
        company: &crate::runtime::company::CompanyStore,
        session_id: &str,
        message: &mut AgentMessage,
        causation_id: Option<&str>,
        producer_operation_id: Option<&str>,
    ) -> Result<bool> {
        if !matches!(message.kind, MessageKind::Talk { .. }) {
            return Ok(true);
        }
        if message.causation_id.is_none() {
            message.causation_id = causation_id
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if message.message_id.is_empty() {
            let operation_id = producer_operation_id
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    (!message.handoff_id.is_empty())
                        .then(|| format!("mesh-handoff:{}", message.handoff_id))
                })
                .unwrap_or_else(|| format!("mesh-talk:{}", uuid::Uuid::new_v4().simple()));
            let accepted = company.accept_company_message_with_identity(
                session_id,
                &operation_id,
                (!message.handoff_id.is_empty()).then_some(message.handoff_id.as_str()),
                message.reply_to.as_deref(),
                message.causation_id.as_deref(),
                &message.from.label(),
                &message.to.label(),
                &message.subject,
                &message.body,
                message.reply_expected(),
            )?;
            message.message_id = accepted.message_id;
            message.handoff_id = accepted.handoff_id;
            return Ok(accepted.should_route);
        }
        if message.handoff_id.is_empty() {
            message.handoff_id = message.message_id.clone();
        }
        Ok(true)
    }

    fn prepare_company_handoff(
        &self,
        message: &mut AgentMessage,
        causation_id: Option<&str>,
        producer_operation_id: Option<&str>,
    ) -> Result<bool> {
        let company = crate::runtime::company::global()?;
        Self::prepare_company_handoff_with_store(
            &company,
            &self.main_session_id,
            message,
            causation_id,
            producer_operation_id,
        )
    }

    /// Persist the session and convert a final into its report-back message.
    fn finalize(
        &self,
        addr: &AgentAddress,
        reply_to: &AgentAddress,
        incoming: &AgentMessage,
        mut final_response: FinalResponse,
        store: &mut SessionStore,
        mut session: Session,
    ) -> Result<Vec<AgentMessage>> {
        final_response.final_markdown =
            strip_private_speaker_tag(&final_response.final_markdown, &addr.label());
        // A native `final_answer` call may carry the complete response while
        // the provider's visible assistant content is only a short summary.
        // Live Story rendering uses `final_markdown`; canonical history must
        // persist that same text or a relaunch silently shrinks the answer.
        persist_canonical_final(&mut session, &final_response.final_markdown);
        let parent_handoff = self.delegators.lock().unwrap_or_else(|p| p.into_inner())
            .get(addr).and_then(|owners| owners.last()).filter(|frame| &frame.address == reply_to)
            .map(|frame| frame.handoff_id.clone());
        self.complete_reply_owner(addr, reply_to);
        self.checkpoint_reply_owners(addr, &mut session)?;
        self.save_session(store, session)?;
        let subject = if final_response.summary.trim().is_empty() {
            format!("{} result", addr.label())
        } else {
            final_response.summary.clone()
        };
        let mut returned = AgentMessage::talk(
            addr.clone(),
            reply_to.clone(),
            subject,
            final_response.final_markdown,
            false,
        );
        // Keep the established durable failure receipt convention even when
        // the saved partial reply itself is ordinary prose. Group routing and
        // recovered mail must not infer success from a nonempty answer.
        if crate::runtime::OutcomeCompletion::from_execution_mode(&final_response.execution_mode)
            == crate::runtime::OutcomeCompletion::Incomplete && !returned.is_failed_result() {
            returned.subject = format!("{} turn failed: {}", addr.label(), returned.subject);
        }
        if matches!(addr, AgentAddress::Specialist(_)) {
            self.emit(CliEvent::SpecialistCompleted {
                agent: agent_display_name(&addr.label()),
                ok: !returned.is_failed_result(),
                summary: cap_chars(final_response.summary.trim(), 160),
            });
        }
        let answered_handoff = parent_handoff.as_deref().filter(|id| !id.is_empty())
            .unwrap_or_else(|| incoming.correlation_id());
        if !answered_handoff.is_empty() {
            returned.reply_to = Some(answered_handoff.to_string());
            // One lifecycle id follows the assignment all the way home. This
            // makes a peer return update the original handoff instead of
            // appearing as a second reverse-direction delegation.
            returned.handoff_id = answered_handoff.to_string();
        }
        if !incoming.message_id.is_empty() {
            returned.causation_id = Some(incoming.message_id.clone());
        }
        Ok(vec![returned])
    }

    fn save_session(&self, store: &mut SessionStore, session: Session) -> Result<()> {
        let id = session.id.clone();
        store.upsert(session);
        // Scoped write: concurrent turns own different sessions; a whole-store
        // save would clobber the others' files with this turn's stale copies.
        store
            .save_one(&id)
            .context("mesh: failed to persist session")
    }

    fn preload_group_histories(&self, addr: &AgentAddress, store: &mut SessionStore) -> Result<()> {
        let Some(group) = &self.group_context else { return Ok(()) };
        // A peer outside the room receives its own direct history, not member
        // context or another coworker's personal conversation.
        if !group.participants.iter().any(|member| member.internal_role == addr.label()) {
            return Ok(());
        }
        if group.read_full_transcript && crate::settings::effective_bool(
            "agents.read_group_transcript",
            &crate::settings::SettingsScope::Group { id: group.group_id.clone() },
        ).unwrap_or(true) {
            store.load_one_if_absent(&group.canonical_session_id)?;
        }
        if let Some(id) = self.group_member_direct_session_id(addr) {
            store.load_one_if_absent(&id)?;
        }
        Ok(())
    }

    fn group_transcript_context(
        &self,
        addr: &AgentAddress,
        store: &SessionStore,
        supplied_receipts: &[String],
    ) -> Option<String> {
        const MAX_GROUP_CONTEXT_CHARS: usize = 120_000;
        let group = self.group_context.as_ref()?;
        if !group.read_full_transcript
            || !crate::settings::effective_bool(
                "agents.read_group_transcript",
                &crate::settings::SettingsScope::Group {
                    id: group.group_id.clone(),
                },
            )
            .unwrap_or(true)
        {
            return None;
        }
        let role = addr.label();
        let participant = group
            .participants
            .iter()
            .find(|participant| participant.internal_role == role)?;
        let session = store.get(&group.canonical_session_id)?;
        let mut rendered = Vec::new();
        let start_index = if participant.history_access
            == crate::runtime::company_directory::HistoryAccess::FromJoin
        {
            participant.history_start_message_index
        } else {
            0
        };
        for (index, message) in session.messages.iter().enumerate().skip(start_index) {
            if matches!(message, Message::GroupContribution { message_id, group_id, .. }
                if group_id == &group.group_id && supplied_receipts.contains(message_id)) {
                continue;
            }
            // The runner persists the current group prompt before preload so
            // Esc cannot lose it. The same text is also the live incoming
            // message for round one, so omit only that final user row here to
            // avoid spending context and attention on an exact duplicate.
            if index + 1 == session.messages.len() && matches!(message, Message::User { .. }) {
                continue;
            }
            let line = match message {
                Message::User { content } => format!("User: {content}"),
                Message::Assistant { content } => format!("Group: {content}"),
                Message::Talk {
                    from,
                    subject,
                    body,
                    ..
                } => format!("{from} — {subject}: {body}"),
                Message::GroupContribution {
                    display_name,
                    subject,
                    body,
                    ..
                } => format!("{display_name} — {subject}: {body}"),
                Message::ToolResult { .. } => continue,
            };
            rendered.push(line);
        }
        let roster = group
            .participants
            .iter()
            .map(|member| {
                let ping = if member.explicitly_mentioned {
                    " — explicitly @mentioned"
                } else {
                    ""
                };
                format!(
                    "{} ({}, {}){}",
                    member.display_name, member.internal_role, member.member_role, ping
                )
            })
            .collect::<Vec<_>>()
            .join("\n- ");
        let mut transcript = rendered.join("\n\n");
        if transcript.chars().count() > MAX_GROUP_CONTEXT_CHARS {
            let keep_from = transcript
                .char_indices()
                .rev()
                .nth(MAX_GROUP_CONTEXT_CHARS)
                .map(|(index, _)| index)
                .unwrap_or(0);
            transcript = format!(
                "[Older group history is available through recall/memory; the active context carries the newest portion.]\n\n{}",
                &transcript[keep_from..]
            );
        }
        Some(format!(
            "GROUP CONVERSATION: {} (`{}`)\nYou are a peer contributing to this shared room. Answer the user directly in your own voice. Use the assignment and its receipt-bound prerequisite inputs; supplied result bodies are not repeated below. This transcript is bounded context, not proof that every earlier result is present. Retrieve missing evidence instead of asking peers to repeat completed work. Dependencies define order; independent coworkers may still be running. Do not wait for unrelated work or invent a plan or consensus. An explicit @ID or @DisplayName in your final room reply wakes that active room member once in this turn; their response is posted here under their own name. Quoted text, code and links do not ping. Use `talk` when you need a private answer or delegated result before finishing. Outside coworkers' replies do not automatically become room contributions.\n\nMembers:\n- {roster}\n\nCanonical group transcript:\n{transcript}",
            group.group_name, group.group_id
        ))
    }

    /// A coworker entering a room keeps continuity with its own canonical
    /// direct conversation. This is deliberately narrower than project brain:
    /// it can expose only this participant's thread, never another coworker's
    /// private chat merely because both use the same workspace.
    fn group_member_direct_context(
        &self,
        addr: &AgentAddress,
        store: &SessionStore,
    ) -> Option<String> {
        const MAX_DIRECT_CONTEXT_CHARS: usize = 60_000;
        let group = self.group_context.as_ref()?;
        let participant = group
            .participants
            .iter()
            .find(|participant| participant.internal_role == addr.label())?;
        let session_id = self.group_member_direct_session_id(addr)?;
        if session_id == group.canonical_session_id {
            return None;
        }
        let session = store.get(&session_id)?;
        let mut lines = session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content } => Some(format!("User: {content}")),
                Message::Assistant { content } => Some(format!("You: {content}")),
                Message::Talk {
                    from,
                    subject,
                    body,
                    ..
                } => Some(format!("{from} — {subject}: {body}")),
                Message::GroupContribution {
                    display_name,
                    subject,
                    body,
                    ..
                } => Some(format!("{display_name} — {subject}: {body}")),
                Message::ToolResult { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if lines.trim().is_empty() {
            return None;
        }
        if lines.chars().count() > MAX_DIRECT_CONTEXT_CHARS {
            let keep_from = lines
                .char_indices()
                .rev()
                .nth(MAX_DIRECT_CONTEXT_CHARS)
                .map(|(index, _)| index)
                .unwrap_or(0);
            lines = format!(
                "[Older direct history remains available through recall; this is the newest exact portion.]\n\n{}",
                &lines[keep_from..]
            );
        }
        Some(format!(
            "YOUR PRIVATE DIRECT-CONVERSATION CONTINUITY ({name})\nThis belongs only to you. Use it for continuity when relevant, but do not quote private material into the group unless the user's current room request clearly calls for it. Other room members do not receive this block.\n\n{lines}",
            name = participant.display_name
        ))
    }

    /// Commit the session to disk WITHOUT consuming it — used at turn start so
    /// the user's just-sent message survives an Esc/abort that never reaches
    /// finalize. Best-effort: a snapshot failure must not fail the turn.
    fn snapshot_session(&self, store: &mut SessionStore, session: &Session) {
        store.upsert(session.clone());
        if let Err(error) = store.save_one(&session.id) {
            tracing::warn!("mesh: turn-start session snapshot failed: {error:#}");
        }
    }

    /// Required persistence boundary for an accepted company handoff. Unlike
    /// ordinary progress snapshots, failure here must leave the SQLite receipt
    /// pending so the handoff is retried after recovery.
    fn persist_received_handoff(store: &mut SessionStore, session: &Session) -> anyhow::Result<()> {
        store.upsert(session.clone());
        store
            .save_one(&session.id)
            .context("mesh: could not durably persist the incoming company handoff")
    }
}

/// Some providers occasionally prefix a final with the private runtime role
/// (`[school_coach]`, often bolded). That label is routing metadata, not part
/// of the coworker's voice. Strip only the exact current role so authored
/// bracketed text such as `[customer_id]` remains byte-faithful.
fn strip_private_speaker_tag(markdown: &str, role: &str) -> String {
    let trimmed = markdown.trim_start();
    let plain = format!("[{role}]");
    for prefix in [
        plain.clone(),
        format!("**{plain}**"),
        format!("__{plain}__"),
    ] {
        if trimmed
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
        {
            return trimmed[prefix.len()..].trim_start().to_string();
        }
    }
    markdown.to_string()
}

fn persist_canonical_final(session: &mut Session, markdown: &str) {
    let markdown = markdown.trim();
    if markdown.is_empty() {
        return;
    }
    match session.messages.last_mut() {
        Some(Message::Assistant { content }) => {
            if content.trim() != markdown {
                *content = markdown.to_string();
                session.transcript_revision = session.transcript_revision.saturating_add(1);
                session.clear_provider_compaction();
            }
        }
        _ => session.push_message(Message::Assistant {
            content: markdown.to_string(),
        }),
    }
}

#[async_trait]
impl AgentTurnHandler for MeshRunner {
    fn join_foreground_returns(&self) -> bool { self.group_context.is_some() }

    async fn run_turn(&self, addr: &AgentAddress, incoming: AgentMessage) -> Vec<AgentMessage> {
        self.run_return_batch(addr, vec![incoming]).await
    }

    async fn run_return_batch(&self, addr: &AgentAddress, mut incoming_batch: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let Some(incoming) = incoming_batch.pop() else { return Vec::new() };
        let mut previous_owners = self
            .delegators
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(addr)
            .cloned().unwrap_or_default();
        let mut reply_to = match &incoming.kind {
            MessageKind::Talk { reply_expected: true } => incoming.from.clone(),
            MessageKind::Talk { reply_expected: false } if incoming.reply_to.as_deref().is_some_and(|id| !id.trim().is_empty()) => previous_owners.last().map(|frame| frame.address.clone()).unwrap_or(AgentAddress::User),
            _ => AgentAddress::User,
        };
        let mut answered_handoff =
            (!incoming.correlation_id().is_empty()).then(|| incoming.correlation_id().to_string());
        let causation_id = (!incoming.message_id.is_empty()).then(|| incoming.message_id.clone());
        let is_peer_return = matches!(incoming.kind, MessageKind::Talk { reply_expected: false })
            && incoming.reply_to.as_deref().is_some_and(|id| !id.trim().is_empty());
        let is_root_input = matches!(incoming.kind, MessageKind::UserInput);
        // Panic isolation: a bug ANYWHERE in the turn — a bad slice, an
        // unwrap, an overflow — becomes a graceful failure routed to whoever
        // invoked this agent, never an aborted task that takes down the rest of
        // the wave and the whole gateway run with it. This is the rule that lets
        // Phoenix keep running THROUGH its own bugs instead of dying on them
        // (the exact failure that stopped Phoenix from fixing its own panic).
        use futures_util::future::FutureExt;
        let outcome = std::panic::AssertUnwindSafe(self.run_turn_inner(addr, incoming, incoming_batch))
            .catch_unwind()
            .await;
        if !matches!(&outcome, Ok(Ok(_))) {
            // A failed nested request must not leave a phantom reply owner.
            // Restore the pre-turn stack even if failure preceded its push.
            let mut owners = self.delegators.lock().unwrap_or_else(|p| p.into_inner());
            // The inner turn may have restored an upstream owner from disk
            // after a restart. Route failure to that owner, not the default
            // user sink chosen before the session was loaded.
            if is_peer_return {
                if let Some(owner) = owners.get(addr).and_then(|stack| stack.last()) {
                    reply_to = owner.address.clone();
                    if !owner.handoff_id.is_empty() { answered_handoff = Some(owner.handoff_id.clone()); }
                }
            }
            if is_root_input { previous_owners.clear(); }
            else if is_peer_return && previous_owners.last().is_some_and(|frame| frame.address == reply_to) {
                previous_owners.pop();
            }
            if previous_owners.is_empty() { owners.remove(addr); }
            else { owners.insert(addr.clone(), previous_owners); }
        }
        match outcome {
            Ok(Ok(outbound)) => outbound,
            Ok(Err(error)) => {
                // Bounded failure: the mesh stays alive; whoever invoked this
                // agent learns the turn failed instead of the run dying.
                tracing::warn!("mesh turn for {} failed: {error:#}", addr.label());
                if matches!(addr, AgentAddress::Specialist(_)) {
                    self.emit(CliEvent::SpecialistCompleted {
                        agent: agent_display_name(&addr.label()),
                        ok: false,
                        summary: cap_chars(&sanitize_error(&error.to_string()), 160),
                    });
                }
                let mut returned = AgentMessage::talk(
                    addr.clone(),
                    reply_to,
                    format!("{} turn failed", addr.label()),
                    format!(
                        "The `{}` agent could not complete its turn: {}",
                        addr.label(),
                        sanitize_error(&error.to_string())
                    ),
                    false,
                );
                returned.reply_to = answered_handoff;
                returned.causation_id = causation_id;
                vec![returned]
            }
            Err(panic) => {
                let detail = panic_payload_message(panic);
                tracing::error!("mesh turn for {} PANICKED: {detail}", addr.label());
                if matches!(addr, AgentAddress::Specialist(_)) {
                    self.emit(CliEvent::SpecialistCompleted {
                        agent: agent_display_name(&addr.label()),
                        ok: false,
                        summary: format!(
                            "internal error: {}",
                            cap_chars(&sanitize_error(&detail), 140)
                        ),
                    });
                }
                let mut returned = AgentMessage::talk(
                    addr.clone(),
                    reply_to,
                    format!("{} turn hit an internal error", addr.label()),
                    format!(
                        "The `{}` agent hit an internal error (a panic) and could not finish this turn: {}. The session is preserved — retry, ideally with a tighter scope. This is a Phoenix bug to report, not a problem with your request.",
                        addr.label(),
                        sanitize_error(&detail)
                    ),
                    false,
                );
                returned.reply_to = answered_handoff;
                returned.causation_id = causation_id;
                vec![returned]
            }
        }
    }
}

#[cfg(test)]
#[path = "../mesh_tests.rs"]
mod mesh_tests;
