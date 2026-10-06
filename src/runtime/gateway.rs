//! The gateway event loop — message-driven agent dispatch (Brick 2).
//!
//! This replaces the old synchronous model (where `deliver_talk` ran a
//! specialist *inline and blocking*, nesting the whole call stack) with an
//! event loop over the [`MessageBus`]:
//!
//! 1. Take the next queued message for some dormant agent.
//! 2. Wake that agent for one turn, reacting to the message.
//! 3. Whatever it wants to say (`talk` to another agent, a reply, a message to
//!    the user) comes back as outbound messages, which are routed into the bus.
//! 4. Repeat until no agent has pending work — then everyone is idle.
//!
//! The agent turn itself is pluggable via [`AgentTurnHandler`]: this module owns
//! the *loop and routing*; the real implementation (driving `AgentRunner` /
//! provider calls) is wired in as the handler in the next brick. That keeps the
//! loop testable in isolation and lets the synchronous path keep working until
//! `phoenix start` is flipped over.
//!
use async_trait::async_trait;

use super::mailbox::{
    canonical_talk_address, same_agent_identity, AgentAddress, AgentMessage, MessageBus,
};
use super::{AgentTurnResponse, FinalResponse, RequestedToolCall};
use crate::tools::TalkInput;

/// Runs a single agent turn in reaction to one inbound message, returning the
/// messages that agent wants to send (talks to other agents, replies, or
/// messages addressed to the user). The gateway routes them; the handler never
/// touches the bus directly — so an agent cannot block waiting on another.
///
/// Takes `&self` so the gateway can run turns for DIFFERENT agents
/// concurrently (new handoffs dispatch on completion); implementations keep their mutable
/// state behind interior mutability.
#[async_trait]
pub trait AgentTurnHandler: Sync {
    async fn run_turn(&self, addr: &AgentAddress, incoming: AgentMessage) -> Vec<AgentMessage>;

    fn join_foreground_returns(&self) -> bool { false }

    async fn run_return_batch(&self, addr: &AgentAddress, incoming: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let mut outbound = Vec::new();
        for message in incoming { outbound.extend(self.run_turn(addr, message).await); }
        outbound
    }
}

/// What a gateway run produced once every agent went idle.
#[derive(Debug, Default)]
pub struct GatewayOutcome {
    /// Messages addressed to the user, in arrival order — what the CLI surfaces.
    pub user_messages: Vec<AgentMessage>,
    /// Agent turns executed this run.
    pub steps: usize,
}

/// The message-driven dispatcher. Owns the bus; drives agents until idle.
pub struct Gateway<H: AgentTurnHandler> {
    bus: MessageBus,
    handler: std::sync::Arc<H>,
    durable_session_id: Option<String>,
    durable_task_id: Option<String>,
    delivery_attempt_id: String,
    delivery_claims: std::collections::HashMap<String, crate::config::private_io::PrivateExecutionClaim>,
    pending_replies: std::collections::HashMap<AgentAddress, std::collections::HashMap<String, AgentAddress>>,
}

fn recover_pending_company_message(
    pending: crate::runtime::company::PendingCompanyMessage,
) -> anyhow::Result<AgentMessage> {
    let from = canonical_talk_address(&pending.from).ok_or_else(|| {
        anyhow::anyhow!(
            "company message {} has unknown sender `{}`",
            pending.message_id,
            pending.from
        )
    })?;
    let to = canonical_talk_address(&pending.to).ok_or_else(|| {
        anyhow::anyhow!(
            "company message {} has unknown recipient `{}`",
            pending.message_id,
            pending.to
        )
    })?;
    let priority = crate::tools::priority_from_transport_body(&pending.body);
    Ok(AgentMessage {
        handoff_id: if pending.handoff_id.is_empty() {
            pending.message_id.clone()
        } else {
            pending.handoff_id
        },
        message_id: pending.message_id,
        reply_to: pending.reply_to,
        causation_id: pending.causation_id,
        group_input_receipts: Vec::new(),
        from,
        to,
        subject: pending.subject,
        body: pending.body,
        priority,
        kind: super::mailbox::MessageKind::Talk {
            reply_expected: pending.reply_expected,
        },
    })
}

impl<H: AgentTurnHandler> Gateway<H> {
    pub fn new(handler: H) -> Self {
        Self {
            bus: MessageBus::new(),
            handler: std::sync::Arc::new(handler),
            durable_session_id: None,
            durable_task_id: None,
            delivery_attempt_id: format!("gateway_{}", uuid::Uuid::new_v4().simple()),
            delivery_claims: std::collections::HashMap::new(),
            pending_replies: Default::default(),
        }
    }

    /// Bind this run to the canonical company message ledger and rehydrate
    /// accepted-but-not-yet-injected coworker handoffs from a prior gateway
    /// process. Fresh user input is persisted by the session store separately.
    pub fn with_durable_session(self, session_id: &str) -> anyhow::Result<Self> {
        let task_id = self.delivery_attempt_id.clone();
        self.with_durable_task(session_id, &task_id)
    }

    pub fn with_durable_task(mut self, session_id: &str, task_id: &str) -> anyhow::Result<Self> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        anyhow::ensure!(!task_id.trim().is_empty() && task_id.len() <= 256, "invalid gateway task identity");
        self.durable_session_id = Some(session_id.to_string());
        self.durable_task_id = Some(task_id.to_string());
        let company = crate::runtime::company::global()?;
        for pending in company.pending_company_messages(session_id)? {
            if same_agent_identity(&pending.from, &pending.to) {
                company.quarantine_self_company_message(
                    session_id,
                    &pending.message_id,
                    &pending.from,
                    &pending.to,
                )?;
                tracing::error!(
                    "quarantined legacy self-addressed company message {}: {} -> {}",
                    pending.message_id,
                    pending.from,
                    pending.to
                );
                continue;
            }
            let message = match recover_pending_company_message(pending) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!("{error:#}; leaving it pending");
                    continue;
                }
            };
            if self.claim_delivery(&message)? {
                self.track_reply(&message);
                self.bus.send(message)?;
            }
        }
        self.durable_session_id = Some(session_id.to_string());
        Ok(self)
    }

    fn claim_delivery(&mut self, message: &AgentMessage) -> anyhow::Result<bool> {
        let Some(session_id) = &self.durable_session_id else { return Ok(true) };
        if message.message_id.is_empty() { return Ok(true) }
        if self.delivery_claims.contains_key(&message.message_id) { return Ok(false) }
        let Some(claim) = crate::runtime::company::global()?.try_claim_company_message(
            session_id, &message.message_id,
            self.durable_task_id.as_deref().unwrap_or(&self.delivery_attempt_id),
            &self.delivery_attempt_id,
        )? else { return Ok(false) };
        self.delivery_claims.insert(message.message_id.clone(), claim);
        Ok(true)
    }

    fn send_routed(&mut self, mut message: AgentMessage) -> anyhow::Result<()> {
        anyhow::ensure!(
            !message.is_self_talk(),
            "coworker `{}` cannot route a talk message to itself",
            message.from.label()
        );
        if matches!(message.kind, super::mailbox::MessageKind::Talk { .. })
            && message.message_id.is_empty()
        {
            if let Some(session_id) = &self.durable_session_id {
                let reply_expected = message.reply_expected();
                let operation_id = if message.handoff_id.is_empty() {
                    format!("gateway-talk:{}", uuid::Uuid::new_v4().simple())
                } else {
                    gateway_handoff_operation_id(&message)
                };
                let accepted = crate::runtime::company::global()?
                    .accept_company_message_with_identity(
                        session_id,
                        &operation_id,
                        (!message.handoff_id.is_empty()).then_some(message.handoff_id.as_str()),
                        message.reply_to.as_deref(),
                        message.causation_id.as_deref(),
                        &message.from.label(),
                        &message.to.label(),
                        &message.subject,
                        &message.body,
                        reply_expected,
                    )?;
                message.message_id = accepted.message_id;
                message.handoff_id = accepted.handoff_id;
                if !accepted.should_route {
                    tracing::debug!(
                        operation_id,
                        state = %accepted.state,
                        "suppressed an exact company-message producer replay"
                    );
                    return Ok(());
                }
            }
        }
        if matches!(message.kind, super::mailbox::MessageKind::Talk { .. })
            && message.handoff_id.is_empty()
        {
            message.handoff_id = if message.message_id.is_empty() {
                format!("handoff_{}", uuid::Uuid::new_v4().simple())
            } else {
                message.message_id.clone()
            };
        }
        if self.claim_delivery(&message)? {
            self.track_reply(&message);
            self.bus.send(message)?;
        }
        Ok(())
    }

    fn track_reply(&mut self, message: &AgentMessage) {
        if !self.handler.join_foreground_returns() { return; }
        if message.reply_expected() && !message.correlation_id().is_empty() {
            self.pending_replies.entry(message.from.clone()).or_default()
                .insert(message.correlation_id().to_string(), message.to.clone());
        } else if let Some(id) = message.reply_to.as_deref().filter(|_| message.is_correlated_return()) {
            if let Some(pending) = self.pending_replies.get_mut(&message.to) {
                if pending.get(id) == Some(&message.from) { pending.remove(id); }
            }
        }
    }

    /// Drop a message into the mesh (e.g. the user's request to the orchestrator).
    pub fn submit(&mut self, msg: AgentMessage) {
        if let Err(error) = self.send_routed(msg) {
            tracing::error!("company message could not be accepted: {error:#}");
        }
    }

    /// Drive the mesh until no agent has pending work.
    /// Messages to the `User` are collected as outputs rather than re-dispatched
    /// (the human is not an agent that runs a turn).
    ///
    /// Dispatch each newly ready agent as soon as a handoff arrives, without
    /// waiting for unrelated in-flight turns. Within one agent, messages stay
    /// strictly FIFO — an agent never runs two turns at once.
    pub async fn run(&mut self) -> GatewayOutcome {
        self.run_observed_with_bounds(None, None, |_| Ok(Vec::new()))
            .await
            .expect("infallible gateway observer")
    }

    /// Bounded implementation kept parameterized for deterministic tests. A
    /// turn already has its own provider deadline; this outer envelope stops
    /// an otherwise-valid agent-to-agent ping-pong from resetting that budget
    /// forever on every handoff.
    async fn run_with_limits(
        &mut self,
        max_turns: usize,
        budget: std::time::Duration,
    ) -> GatewayOutcome {
        self.run_observed_with_bounds(Some(max_turns), Some(budget), |_| Ok(Vec::new()))
            .await
            .expect("infallible gateway observer")
    }

    /// Observe actor-authored user contributions at the completion boundary,
    /// before unrelated actors finish. A caller can durably commit the message
    /// before publishing it. Commit failure cancels the remaining in-flight
    /// futures and is returned instead of falsely acknowledging completion.
    pub async fn run_observed(
        &mut self,
        mut observer: impl FnMut(&AgentMessage) -> anyhow::Result<()>,
    ) -> anyhow::Result<GatewayOutcome> {
        self.run_observed_with_bounds(None, None, |message| {
            observer(message)?;
            Ok(Vec::new())
        })
        .await
    }

    /// Persist a completed result, then return newly eligible tasks. Returned
    /// messages enter the same durable routing and per-agent FIFO discipline.
    pub async fn run_dispatching(
        &mut self,
        observer: impl FnMut(&AgentMessage) -> anyhow::Result<Vec<AgentMessage>>,
    ) -> anyhow::Result<GatewayOutcome> {
        self.run_observed_with_bounds(None, None, observer)
            .await
    }

    async fn run_observed_with_bounds(
        &mut self,
        max_turns: Option<usize>,
        budget: Option<std::time::Duration>,
        mut observer: impl FnMut(&AgentMessage) -> anyhow::Result<Vec<AgentMessage>>,
    ) -> anyhow::Result<GatewayOutcome> {
        let mut outcome = GatewayOutcome::default();
        let mut completed_turns = 0usize;
        let mut routed_handoffs = std::collections::HashSet::new();
        let deadline = budget.map(|budget| tokio::time::Instant::now() + budget);
        let mut bounded_reason: Option<String> = None;
        use futures_util::{future::FutureExt, stream::FuturesUnordered, StreamExt};
        let mut results = FuturesUnordered::new();
        let mut active = std::collections::HashSet::new();

        while self.bus.has_pending() || !results.is_empty() {
            if let Some(max_turns) = max_turns {
                if outcome.steps >= max_turns && results.is_empty() {
                    bounded_reason = Some(format!(
                        "the mesh reached its hard limit of {max_turns} agent turns"
                    ));
                    break;
                }
            }
            let remaining = deadline.map(|deadline| {
                deadline.saturating_duration_since(tokio::time::Instant::now())
            });
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                bounded_reason = Some(format!(
                    "the mesh reached its {}-second whole-run deadline",
                    budget.expect("deadline exists only with a budget").as_secs()
                ));
                break;
            }
            // One in-flight turn per canonical agent, not one global wave.
            let mut wave: Vec<(AgentAddress, Vec<AgentMessage>)> = Vec::new();
            let available_turns = max_turns
                .map(|max_turns| max_turns.saturating_sub(outcome.steps))
                .unwrap_or(usize::MAX);
            for addr in self
                .runnable_addresses()
                .into_iter()
                .filter(|addr| !active.contains(addr))
                .take(available_turns)
            {
                let join = self.handler.join_foreground_returns();
                let waiting = self.pending_replies.get(&addr).is_some_and(|pending| !pending.is_empty());
                let incoming = if join { self.bus.next_return_batch(&addr, waiting) }
                    else { self.bus.next_for(&addr).into_iter().collect() };
                if !incoming.is_empty() {
                    wave.push((addr, incoming));
                }
            }
            if wave.is_empty() && results.is_empty() {
                break;
            }

            outcome.steps += wave.len();
            {
                // Backstop: isolate each turn's panics so one agent blowing up never
                // aborts its siblings in the wave or the whole gateway run. The real
                // handler routes its own panics gracefully to the delegator; this is
                // the universal guarantee that the run survives ANY turn.
                for (addr, incoming) in wave {
                    active.insert(addr.clone());
                    let handler = self.handler.clone();
                    results.push(async move {
                        let turn = std::panic::AssertUnwindSafe(
                            handler.run_return_batch(&addr, incoming),
                        )
                        .catch_unwind();
                        let result = if let Some(remaining) = remaining {
                            match tokio::time::timeout(remaining, turn).await {
                                Ok(Ok(outbound)) => Ok(outbound),
                                Ok(Err(_)) => Ok(vec![AgentMessage::talk(
                                    addr.clone(),
                                    AgentAddress::User,
                                    format!("{} turn hit an internal error", addr.label()),
                                    format!(
                                        "The `{}` agent hit an internal error and could not finish this turn. The run continued — retry.",
                                        addr.label()
                                    ),
                                    false,
                                )]),
                                Err(_) => Err(addr.clone()),
                            }
                        } else {
                            match turn.await {
                                Ok(outbound) => Ok(outbound),
                                Err(_) => Ok(vec![AgentMessage::talk(
                                    addr.clone(),
                                    AgentAddress::User,
                                    format!("{} turn hit an internal error", addr.label()),
                                    format!(
                                        "The `{}` agent hit an internal error and could not finish this turn. The run continued — retry.",
                                        addr.label()
                                    ),
                                    false,
                                )]),
                            }
                        };
                        (addr, result)
                    });
                }
            }

            let mut timed_out_agents = Vec::new();
            if let Some((addr, result)) = results.next().await {
                active.remove(&addr);
                match result {
                    Ok(outbound) => {
                        completed_turns += 1;
                        for msg in outbound {
                            if msg.to == AgentAddress::User {
                                for ready in observer(&msg)? {
                                    self.send_routed(ready)?;
                                }
                                outcome.user_messages.push(msg);
                            } else {
                                let fingerprint = format!(
                                    "{}\0{}\0{}\0{}",
                                    msg.from.label().to_ascii_lowercase(),
                                    msg.to.label().to_ascii_lowercase(),
                                    msg.subject.split_whitespace().collect::<Vec<_>>().join(" "),
                                    msg.body.split_whitespace().collect::<Vec<_>>().join(" ")
                                );
                                if !routed_handoffs.insert(fingerprint) {
                                    outcome.user_messages.push(AgentMessage::talk(
                                        AgentAddress::Orchestrator,
                                        AgentAddress::User,
                                        "repeated company handoff stopped",
                                        format!(
                                            "Phoenix stopped an exact repeated handoff from {} to {}. The earlier delivery remains authoritative; the duplicate was not queued and no context was deleted.",
                                            msg.from.label(),
                                            msg.to.label()
                                        ),
                                        false,
                                    ));
                                    continue;
                                }
                                if let Err(error) = self.send_routed(msg) {
                                    outcome.user_messages.push(AgentMessage::talk(
                                        AgentAddress::Orchestrator,
                                        AgentAddress::User,
                                        "company handoff could not be persisted",
                                        format!(
                                            "A coworker handoff was stopped before delivery because Phoenix could not persist it safely: {error:#}"
                                        ),
                                        false,
                                    ));
                                }
                            }
                        }
                    }
                    Err(addr) => timed_out_agents.push(addr.label()),
                }
            }
            if !timed_out_agents.is_empty() {
                bounded_reason = Some(format!(
                    "the whole-run deadline expired while waiting for {}",
                    timed_out_agents.join(", ")
                ));
                break;
            }
        }

        if let Some(reason) = bounded_reason {
            outcome.user_messages.push(AgentMessage::talk(
                AgentAddress::Orchestrator,
                AgentAddress::User,
                "mesh run stopped at runtime bound",
                format!(
                    "Phoenix stopped this mesh run because {reason}. It completed {} agent turn(s). Any results already delivered above remain valid, but pending handoffs did not complete and must not be reported as finished.",
                    completed_turns
                ),
                false,
            ));
        }

        Ok(outcome)
    }

    /// Every address with pending work that is not the user.
    fn runnable_addresses(&self) -> Vec<AgentAddress> {
        self.bus
            .pending_addresses()
            .into_iter()
            .filter(|addr| *addr != AgentAddress::User)
            .collect()
    }
}

/// One handoff can legitimately travel through several agents before the
/// original owner answers the user. The handoff id is the stable lifecycle
/// identity for the whole chain; it is therefore NOT a unique producer
/// operation id. Include the concrete hop and causation message so exact
/// replays deduplicate while a later return carrying new content is accepted.
fn gateway_handoff_operation_id(message: &AgentMessage) -> String {
    format!(
        "gateway-handoff:{}:{}:{}:{}",
        message.handoff_id,
        message.from.label(),
        message.to.label(),
        message.causation_id.as_deref().unwrap_or("root")
    )
}

/// Convert one finished agent turn into the messages it wants to send into the
/// mesh:
/// - a `talk` tool call → an outbound message to the named agent;
/// - a `final` → a report back to whoever invoked this agent (`reply_to`: the
///   user for an orchestrator turn, the delegating agent for a specialist).
///
/// Local tool calls (read/grep/web_search/…) are the agent's *own* actions —
/// they run inline within the turn and are never outbound, so they don't appear
/// here. This is the seam the real turn-runner uses to feed the gateway.
pub fn turn_outputs(
    from: &AgentAddress,
    reply_to: &AgentAddress,
    turn: &AgentTurnResponse,
) -> Vec<AgentMessage> {
    match turn {
        AgentTurnResponse::Final(final_response) => {
            vec![final_to_message(from, reply_to, final_response)]
        }
        AgentTurnResponse::ToolRequest { tool_calls, .. } => tool_calls
            .iter()
            .filter_map(|call| talk_call_to_message(from, call))
            .collect(),
    }
}

fn final_to_message(
    from: &AgentAddress,
    reply_to: &AgentAddress,
    final_response: &FinalResponse,
) -> AgentMessage {
    let subject = if final_response.summary.trim().is_empty() {
        format!("{} result", from.label())
    } else {
        final_response.summary.clone()
    };
    AgentMessage::talk(
        from.clone(),
        reply_to.clone(),
        subject,
        final_response.final_markdown.clone(),
        false,
    )
}

fn talk_call_to_message(from: &AgentAddress, call: &RequestedToolCall) -> Option<AgentMessage> {
    if call.tool_name != "talk" {
        return None; // a local tool — runs inline, not an outbound message
    }
    let talk: TalkInput = serde_json::from_value(call.input.clone()).ok()?;
    let to = AgentAddress::from_talk_name(&talk.to)?;
    let reply_expected = talk.reply_expected();
    Some(AgentMessage::talk(
        from.clone(),
        to,
        talk.subject,
        talk.body,
        reply_expected,
    ))
}

#[cfg(test)]
mod tests {
    struct JoinedReturnHandler;

    #[async_trait]
    impl AgentTurnHandler for JoinedReturnHandler {
        fn join_foreground_returns(&self) -> bool { true }

        async fn run_return_batch(&self, addr: &AgentAddress, mut incoming: Vec<AgentMessage>) -> Vec<AgentMessage> {
            if incoming[0].is_correlated_return() {
                assert_eq!(incoming.len(), 2, "owner must see both returns together");
                assert!(incoming.iter().any(|message| message.body == "fast result"));
                assert!(incoming.iter().any(|message| message.is_failed_result()));
                return vec![AgentMessage::talk(addr.clone(), AgentAddress::User, "Combined", "Both results received; one failed.", false)];
            }
            self.run_turn(addr, incoming.remove(0)).await
        }

        async fn run_turn(&self, addr: &AgentAddress, incoming: AgentMessage) -> Vec<AgentMessage> {
            if matches!(incoming.kind, super::super::mailbox::MessageKind::UserInput) {
                return ["coder", "critic"].iter().map(|name| {
                    let mut message = AgentMessage::talk(addr.clone(), AgentAddress::from_talk_name(name).unwrap(), *name, "Review", true);
                    message.handoff_id = name.to_string(); message
                }).collect();
            }
            let slow = addr.label() == "critic";
            if slow { tokio::time::sleep(std::time::Duration::from_millis(30)).await; }
            let mut returned = AgentMessage::talk(addr.clone(), incoming.from.clone(),
                if slow { "critic turn failed" } else { "Reviewed" }, if slow { "provider unavailable" } else { "fast result" }, false);
            returned.reply_to = Some(incoming.correlation_id().to_string());
            vec![returned]
        }
    }

    #[tokio::test]
    async fn owner_joins_fast_success_and_slow_failure_before_one_final() {
        let mut gateway = Gateway::new(JoinedReturnHandler);
        gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Review"));
        let outcome = gateway.run().await;
        assert_eq!(outcome.steps, 4);
        assert_eq!(outcome.user_messages.len(), 1);
        assert_eq!(outcome.user_messages[0].body, "Both results received; one failed.");
    }
    use super::*;

    #[test]
    fn concurrent_gateways_cannot_recover_the_same_delivery() {
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _home = crate::config::test_env::PhoenixHomeGuard::set(home.path());
        let company = crate::runtime::company::global().unwrap();
        company.accept_company_message("gateway-claim-fixture", "planner", "coder", "fixture", "one saved delivery", true).unwrap();
        let first = Gateway::new(ChainHandler).with_durable_task("gateway-claim-fixture", "first-task").unwrap();
        let second = Gateway::new(ChainHandler).with_durable_task("gateway-claim-fixture", "second-task").unwrap();
        assert!(first.bus.has_pending());
        assert!(!second.bus.has_pending(), "a second run must not dispatch the first run's delivery");
        drop(first);
        let recovered = Gateway::new(ChainHandler).with_durable_task("gateway-claim-fixture", "recovered-task").unwrap();
        assert!(recovered.bus.has_pending(), "dropping an unexecuted run must release its accepted delivery");
    }
    use crate::session::SubAgentType;

    fn coder() -> AgentAddress {
        AgentAddress::Specialist(SubAgentType::Coder)
    }
    fn researcher() -> AgentAddress {
        AgentAddress::Specialist(SubAgentType::Researcher)
    }

    #[test]
    fn gateway_recovery_restores_exact_company_handoff_lineage() {
        let recovered = super::recover_pending_company_message(
            crate::runtime::company::PendingCompanyMessage {
                message_id: "message_recovery".to_string(),
                operation_id: "mesh_turn_recovery:round-1:call-0".to_string(),
                handoff_id: "return_recovery".to_string(),
                reply_to: Some("handoff_request".to_string()),
                causation_id: Some("message_cause".to_string()),
                from: "phoenix".to_string(),
                to: "coder".to_string(),
                subject: "resume".to_string(),
                body: "Continue the exact accepted work.".to_string(),
                reply_expected: true,
            },
        )
        .unwrap();
        assert_eq!(recovered.message_id, "message_recovery");
        assert_eq!(recovered.handoff_id, "return_recovery");
        assert_eq!(recovered.reply_to.as_deref(), Some("handoff_request"));
        assert_eq!(recovered.causation_id.as_deref(), Some("message_cause"));
        assert_eq!(recovered.from, AgentAddress::Orchestrator);
        assert_eq!(recovered.to, coder());
        assert!(recovered.reply_expected());

        let legacy = super::recover_pending_company_message(
            crate::runtime::company::PendingCompanyMessage {
                message_id: "message_legacy".to_string(),
                operation_id: String::new(),
                handoff_id: String::new(),
                reply_to: None,
                causation_id: None,
                from: "phoenix".to_string(),
                to: "coder".to_string(),
                subject: "legacy".to_string(),
                body: "Use the message receipt as lifecycle identity.".to_string(),
                reply_expected: false,
            },
        )
        .unwrap();
        assert_eq!(legacy.handoff_id, legacy.message_id);
    }

    #[test]
    fn gateway_handoff_operation_identity_is_per_hop_not_per_lifecycle() {
        let mut first_return = AgentMessage::talk(
            researcher(),
            coder(),
            "review complete",
            "The review is complete.",
            false,
        );
        first_return.handoff_id = "handoff_review".into();
        first_return.causation_id = Some("message_review_request".into());

        let exact_replay = first_return.clone();
        assert_eq!(
            gateway_handoff_operation_id(&first_return),
            gateway_handoff_operation_id(&exact_replay)
        );

        let mut next_hop = AgentMessage::talk(
            coder(),
            researcher(),
            "integrated review",
            "The corrections are integrated.",
            false,
        );
        next_hop.handoff_id = first_return.handoff_id.clone();
        next_hop.causation_id = Some("message_review_return".into());
        assert_ne!(
            gateway_handoff_operation_id(&first_return),
            gateway_handoff_operation_id(&next_hop),
            "a lifecycle id may span several distinct durable message hops"
        );
    }

    /// A scripted handler that models a proactive chain:
    /// user → orchestrator → researcher → coder → user.
    struct ChainHandler;

    #[async_trait]
    impl AgentTurnHandler for ChainHandler {
        async fn run_turn(&self, addr: &AgentAddress, incoming: AgentMessage) -> Vec<AgentMessage> {
            match addr {
                // Orchestrator kicks off ONE chain: researcher, told to hand to coder.
                AgentAddress::Orchestrator => vec![AgentMessage::talk(
                    AgentAddress::Orchestrator,
                    researcher(),
                    "research then hand to coder",
                    incoming.body,
                    true,
                )],
                AgentAddress::Specialist(SubAgentType::Researcher) => vec![AgentMessage::talk(
                    researcher(),
                    coder(),
                    "integrate findings",
                    "found X, Y, Z",
                    true,
                )],
                // Coder finishes and reports to the user directly (the mesh).
                AgentAddress::Specialist(SubAgentType::Coder) => vec![AgentMessage::talk(
                    coder(),
                    AgentAddress::User,
                    "done",
                    "integrated, here is the result",
                    false,
                )],
                _ => vec![],
            }
        }
    }

    #[tokio::test]
    async fn drives_a_proactive_chain_to_the_user_and_goes_idle() {
        let mut gw = Gateway::new(ChainHandler);
        gw.submit(AgentMessage::user_input(
            AgentAddress::Orchestrator,
            "do the thing",
        ));
        let outcome = gw.run().await;

        // orchestrator + researcher + coder = 3 turns, then idle.
        assert_eq!(outcome.steps, 3);
        assert_eq!(outcome.user_messages.len(), 1);
        assert_eq!(outcome.user_messages[0].from, coder());
        assert_eq!(
            outcome.user_messages[0].body,
            "integrated, here is the result"
        );
    }

    /// A turn that panics must not abort the whole gateway run — it becomes a
    /// graceful failure message and the run completes. This is the rule that
    /// keeps Phoenix alive through its own bugs (a panic used to surface as
    /// `internal: task panicked` and kill the user's whole turn).
    struct PanickingHandler;

    #[async_trait]
    impl AgentTurnHandler for PanickingHandler {
        async fn run_turn(
            &self,
            _addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            panic!("simulated turn panic — slice index starts at 102 but ends at 97");
        }
    }

    struct CompletionOrderHandler;

    struct ObservedContributionHandler(std::sync::Arc<tokio::sync::Notify>);

    #[async_trait]
    impl AgentTurnHandler for ObservedContributionHandler {
        async fn run_turn(
            &self,
            addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            if *addr == coder() {
                self.0.notified().await;
            }
            vec![AgentMessage::talk(
                addr.clone(),
                AgentAddress::User,
                "result",
                addr.label(),
                false,
            )]
        }
    }

    #[tokio::test]
    async fn contribution_observer_runs_before_slow_peer_finishes() {
        let published = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut gateway = Gateway::new(ObservedContributionHandler(published.clone()));
        gateway.submit(AgentMessage::user_input(
            coder(),
            "wait for published research",
        ));
        gateway.submit(AgentMessage::user_input(researcher(), "research"));
        let mut observed = Vec::new();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gateway.run_observed(|message| {
                observed.push(message.from.clone());
                if message.from == researcher() {
                    published.notify_one();
                }
                Ok(())
            }),
        )
        .await
        .expect("publication must not wait for a peer")
        .unwrap();
        assert_eq!(observed, vec![researcher(), coder()]);
        assert_eq!(outcome.user_messages.len(), 2);
    }

    #[tokio::test]
    async fn contribution_commit_failure_is_propagated_without_waiting_for_peer() {
        let mut gateway = Gateway::new(ObservedContributionHandler(std::sync::Arc::new(
            tokio::sync::Notify::new(),
        )));
        gateway.submit(AgentMessage::user_input(coder(), "wait"));
        gateway.submit(AgentMessage::user_input(researcher(), "research"));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gateway.run_observed(|_| anyhow::bail!("simulated durable storage failure")),
        )
        .await
        .expect("storage failure must cancel pending futures");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("simulated durable storage failure"));
    }

    struct HandoffWithoutBarrierHandler {
        integrated: tokio::sync::Notify,
    }

    #[async_trait]
    impl AgentTurnHandler for HandoffWithoutBarrierHandler {
        async fn run_turn(
            &self,
            addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            match addr {
                AgentAddress::Specialist(SubAgentType::Coder) => {
                    // Deliberately cannot complete until the research handoff
                    // has been consumed. A global wave barrier deadlocks here.
                    self.integrated.notified().await;
                    vec![AgentMessage::talk(
                        addr.clone(),
                        AgentAddress::User,
                        "built",
                        "backend finished",
                        false,
                    )]
                }
                AgentAddress::Specialist(SubAgentType::Researcher) => {
                    vec![AgentMessage::talk(
                        addr.clone(),
                        AgentAddress::Orchestrator,
                        "research ready",
                        "use this brief",
                        false,
                    )]
                }
                AgentAddress::Orchestrator => {
                    self.integrated.notify_one();
                    vec![AgentMessage::talk(
                        addr.clone(),
                        AgentAddress::User,
                        "integrated",
                        "research consumed while backend running",
                        false,
                    )]
                }
                _ => vec![],
            }
        }
    }

    #[tokio::test]
    async fn handoff_runs_before_unrelated_inflight_turn_finishes() {
        let mut gateway = Gateway::new(HandoffWithoutBarrierHandler {
            integrated: tokio::sync::Notify::new(),
        });
        gateway.submit(AgentMessage::user_input(coder(), "build backend"));
        gateway.submit(AgentMessage::user_input(researcher(), "research"));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), gateway.run())
            .await
            .expect("research handoff must not wait for the backend");
        assert_eq!(outcome.steps, 3);
        assert_eq!(outcome.user_messages.len(), 2);
        assert_eq!(
            outcome.user_messages[0].body,
            "research consumed while backend running"
        );
        assert_eq!(outcome.user_messages[1].body, "backend finished");
    }

    #[async_trait]
    impl AgentTurnHandler for CompletionOrderHandler {
        async fn run_turn(
            &self,
            addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            let (delay, body) = match addr {
                AgentAddress::Specialist(SubAgentType::Coder) => (30, "coder finished second"),
                AgentAddress::Specialist(SubAgentType::Researcher) => {
                    (2, "researcher finished first")
                }
                _ => (1, "other"),
            };
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            vec![AgentMessage::talk(
                addr.clone(),
                AgentAddress::User,
                "finished",
                body,
                false,
            )]
        }
    }

    #[tokio::test]
    async fn parallel_wave_reports_replies_in_completion_order() {
        let mut gateway = Gateway::new(CompletionOrderHandler);
        gateway.submit(AgentMessage::user_input(coder(), "build"));
        gateway.submit(AgentMessage::user_input(researcher(), "research"));

        let outcome = gateway.run().await;

        assert_eq!(outcome.steps, 2);
        assert_eq!(outcome.user_messages.len(), 2);
        assert_eq!(outcome.user_messages[0].body, "researcher finished first");
        assert_eq!(outcome.user_messages[1].body, "coder finished second");
    }

    #[tokio::test]
    async fn a_panicking_turn_does_not_kill_the_gateway_run() {
        let mut gw = Gateway::new(PanickingHandler);
        gw.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "go"));
        // The whole point: this `.await` must return, not unwind the test.
        let outcome = gw.run().await;
        assert_eq!(outcome.user_messages.len(), 1);
        assert!(
            outcome.user_messages[0].body.contains("internal error"),
            "{:?}",
            outcome.user_messages[0].body
        );
    }

    /// Two specialists delegated in one orchestrator turn must run their
    /// turns CONCURRENTLY (one wave), not one after the other.
    struct FanOutHandler {
        in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        max_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl AgentTurnHandler for FanOutHandler {
        async fn run_turn(&self, addr: &AgentAddress, incoming: AgentMessage) -> Vec<AgentMessage> {
            use std::sync::atomic::Ordering;
            match addr {
                AgentAddress::Orchestrator if matches!(incoming.kind, MessageKind::UserInput) => {
                    vec![
                        AgentMessage::talk(
                            AgentAddress::Orchestrator,
                            coder(),
                            "build",
                            "part A",
                            true,
                        ),
                        AgentMessage::talk(
                            AgentAddress::Orchestrator,
                            researcher(),
                            "research",
                            "part B",
                            true,
                        ),
                    ]
                }
                AgentAddress::Specialist(_) => {
                    let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    self.max_in_flight.fetch_max(now, Ordering::SeqCst);
                    // Yield so the sibling turn (same wave) can start before
                    // this one finishes — overlapping awaits is the point.
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    self.in_flight.fetch_sub(1, Ordering::SeqCst);
                    vec![AgentMessage::talk(
                        addr.clone(),
                        AgentAddress::Orchestrator,
                        "done",
                        "result",
                        false,
                    )]
                }
                _ => vec![],
            }
        }
    }

    #[tokio::test]
    async fn fan_out_specialists_run_in_the_same_wave() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let handler = FanOutHandler {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::clone(&max_in_flight),
        };
        let mut gw = Gateway::new(handler);
        gw.submit(AgentMessage::user_input(
            AgentAddress::Orchestrator,
            "do both parts",
        ));
        let _outcome = gw.run().await;
        // Both specialist turns were in flight at the same moment.
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 2);
    }

    use super::super::mailbox::MessageKind;

    #[tokio::test]
    async fn idle_mesh_does_nothing() {
        let mut gw = Gateway::new(ChainHandler);
        let outcome = gw.run().await;
        assert_eq!(outcome.steps, 0);
        assert!(outcome.user_messages.is_empty());
    }

    struct PingPongHandler;

    #[async_trait]
    impl AgentTurnHandler for PingPongHandler {
        async fn run_turn(
            &self,
            addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            let to = if matches!(addr, AgentAddress::Orchestrator) {
                coder()
            } else {
                AgentAddress::Orchestrator
            };
            vec![AgentMessage::talk(
                addr.clone(),
                to,
                "keep handing off",
                "the deliberately cyclic test baton",
                true,
            )]
        }
    }

    #[tokio::test]
    async fn cyclic_gateway_handoffs_stop_on_the_first_exact_repeat() {
        let mut gw = Gateway::new(PingPongHandler);
        gw.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "loop"));
        let outcome = gw
            .run_with_limits(3, std::time::Duration::from_secs(1))
            .await;

        assert_eq!(outcome.steps, 3);
        assert_eq!(outcome.user_messages.len(), 1);
        assert!(outcome.user_messages[0]
            .body
            .contains("exact repeated handoff"));
        assert!(outcome.user_messages[0]
            .body
            .contains("duplicate was not queued"));
        assert!(!gw.bus.has_pending());
    }

    struct SelfHandoffHandler;

    #[async_trait]
    impl AgentTurnHandler for SelfHandoffHandler {
        async fn run_turn(
            &self,
            addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            vec![AgentMessage::talk(
                addr.clone(),
                addr.clone(),
                "repeat forever",
                "this must never be queued",
                true,
            )]
        }
    }

    #[tokio::test]
    async fn handler_self_handoff_is_rejected_after_one_turn_not_looped() {
        let mut gw = Gateway::new(SelfHandoffHandler);
        gw.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "go"));
        let outcome = gw
            .run_with_limits(8, std::time::Duration::from_secs(1))
            .await;

        assert_eq!(outcome.steps, 1);
        assert_eq!(outcome.user_messages.len(), 1);
        assert!(outcome.user_messages[0]
            .body
            .contains("cannot route a talk message to itself"));
        assert!(!outcome.user_messages[0].body.contains("hard limit"));
        assert!(!gw.bus.has_pending());
    }

    #[test]
    fn direct_gateway_self_talk_is_rejected_before_queueing() {
        let mut gw = Gateway::new(ChainHandler);
        let result = gw.send_routed(AgentMessage::talk(
            coder(),
            coder(),
            "self",
            "do not enqueue",
            false,
        ));
        assert!(result.is_err());
        assert!(!gw.bus.has_pending());
    }

    struct PendingHandler;

    #[async_trait]
    impl AgentTurnHandler for PendingHandler {
        async fn run_turn(
            &self,
            _addr: &AgentAddress,
            _incoming: AgentMessage,
        ) -> Vec<AgentMessage> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn gateway_whole_run_deadline_cancels_a_pending_turn() {
        let mut gw = Gateway::new(PendingHandler);
        gw.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "hang"));
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gw.run_with_limits(10, std::time::Duration::from_millis(20)),
        )
        .await
        .expect("gateway deadline must terminate the pending handler");

        assert_eq!(outcome.steps, 1);
        assert_eq!(outcome.user_messages.len(), 1);
        assert!(outcome.user_messages[0]
            .body
            .contains("whole-run deadline expired"));
    }

    #[test]
    fn final_turn_reports_back_to_its_invoker() {
        let turn = AgentTurnResponse::Final(
            serde_json::from_value(serde_json::json!({
                "summary": "did it",
                "final_markdown": "the answer",
                "execution_mode": "provider_backed"
            }))
            .unwrap(),
        );
        // Researcher invoked by the orchestrator → its final reports to orchestrator.
        let out = turn_outputs(&researcher(), &AgentAddress::Orchestrator, &turn);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].from, researcher());
        assert_eq!(out[0].to, AgentAddress::Orchestrator);
        assert_eq!(out[0].body, "the answer");
        assert!(!out[0].reply_expected());
    }

    #[test]
    fn talk_calls_become_outbound_local_tools_do_not() {
        let tool_calls: Vec<RequestedToolCall> = serde_json::from_value(serde_json::json!([
            { "tool_name": "read", "input": { "path": "x.rs" } },
            { "tool_name": "talk", "input": { "to": "coder", "subject": "wire it", "body": "integrate X", "mode": 1 } }
        ]))
        .unwrap();
        let turn = AgentTurnResponse::ToolRequest {
            tool_calls,
            rationale: String::new(),
        };
        let out = turn_outputs(&researcher(), &AgentAddress::Orchestrator, &turn);
        // `read` is a local action (runs inline) — only the `talk` is outbound.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].from, researcher());
        assert_eq!(out[0].to, coder());
        assert_eq!(out[0].body, "integrate X");
        assert!(out[0].reply_expected());
    }
}
