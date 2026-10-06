//! Async agent message-passing spine — the foundation of the actor mesh.
//!
//! Phoenix is migrating from synchronous, inline specialist execution
//! (`deliver_talk` runs the specialist nested + blocking on one call stack) to
//! an **actor mesh**: every agent owns a durable session + an inbox; `talk`
//! becomes a *message* dropped into the target's inbox; a long-running gateway
//! wakes a dormant agent when its inbox fills, lets it work, then it goes quiet
//! again. Any agent can message any other (orchestrator → researcher → coder →
//! presentation), and an agent can message the user (surfacing in the CLI like
//! an incoming chat).
//!
//! This module is the in-process scheduling spine — addresses, messages,
//! inboxes, and the bus. The live gateway rehydrates pending messages from the
//! durable company database before scheduling them; this queue deliberately
//! remains a lightweight runtime view rather than a second persistence layer.

use std::collections::{HashMap, VecDeque};

use crate::runtime::delegation::specialist_from_talk_name;
use crate::session::SubAgentType;
use crate::tools::{priority_from_transport_body, MessagePriority};

/// Identifies an agent (and, later, its durable session) in the mesh. The
/// `User` is a first-class address so agents can deliver results/proactive
/// messages back to the human, who sees them arrive in the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentAddress {
    Orchestrator,
    Specialist(SubAgentType),
    User,
}

impl AgentAddress {
    /// Short label for CLI/log lines (`orchestrator`, `coder`, `researcher`, …).
    pub fn label(&self) -> String {
        match self {
            AgentAddress::Orchestrator => "orchestrator".to_string(),
            AgentAddress::User => "user".to_string(),
            AgentAddress::Specialist(agent) => {
                crate::runtime::delegation::specialist_label(*agent).to_string()
            }
        }
    }

    /// Resolve a `talk` target string (the `to` field) to a mesh address. This
    /// is the routing seam: any agent emitting `talk(to: "coder" | "researcher"
    /// | "orchestrator" | "user")` is mapped to the inbox the gateway delivers
    /// into. Unknown targets return `None` (delivered nowhere / surfaced as an
    /// error by the caller).
    pub fn from_talk_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "orchestrator" | "phoenix" => Some(AgentAddress::Orchestrator),
            "user" => Some(AgentAddress::User),
            other => specialist_from_talk_name(other).map(AgentAddress::Specialist),
        }
    }
}

/// Resolve any runtime-facing coworker label to its canonical mesh address.
/// Besides stable ids and editable names, event/UI labels sometimes carry the
/// private role in parentheses (`Avery (school_coach)`) or an instance suffix
/// (`coder#2`).  Those spellings must not bypass the self-route invariant.
pub fn canonical_talk_address(name: &str) -> Option<AgentAddress> {
    let trimmed = name.trim().trim_start_matches('@');
    let without_instance = trimmed
        .rsplit_once('#')
        .filter(|(_, suffix)| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(base, _)| base.trim())
        .unwrap_or(trimmed);
    let parenthetical = without_instance
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once('('))
        .map(|(_, role)| role.trim());

    [Some(without_instance), parenthetical]
        .into_iter()
        .flatten()
        .flat_map(|candidate| {
            let normalized = candidate
                .trim()
                .trim_start_matches('@')
                .to_ascii_lowercase()
                .replace([' ', '-'], "_");
            [candidate.to_string(), normalized]
        })
        .find_map(|candidate| AgentAddress::from_talk_name(&candidate))
}

fn normalized_route_label(name: &str) -> String {
    let trimmed = name.trim().trim_start_matches('@');
    let without_instance = trimmed
        .rsplit_once('#')
        .filter(|(_, suffix)| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(base, _)| base.trim())
        .unwrap_or(trimmed);
    without_instance
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once('('))
        .map(|(_, role)| role.trim())
        .unwrap_or(without_instance)
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
}

/// Compare string identities only after resolving aliases/display names to the
/// same typed address. Unknown historical labels still get a conservative
/// normalized equality check, so an exact self-message fails closed.
pub fn same_agent_identity(left: &str, right: &str) -> bool {
    match (canonical_talk_address(left), canonical_talk_address(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(left), None) => {
            normalized_route_label(&left.label()) == normalized_route_label(right)
        }
        (None, Some(right)) => {
            normalized_route_label(left) == normalized_route_label(&right.label())
        }
        (None, None) => normalized_route_label(left) == normalized_route_label(right),
    }
}

/// Why a message is being delivered — drives how the receiver treats it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// A fresh request from the human user (starts/continues a turn).
    UserInput,
    /// An inter-agent talk: delegation, a chain handoff, or a report-back.
    /// `reply_expected` distinguishes "do this and report back" (the sender
    /// will resume on the reply) from fire-and-forget.
    Talk { reply_expected: bool },
}

/// One message in flight between two addresses. The receiver treats an inbound
/// `Talk` as if it were a user turn in its own session (marked as a talk), which
/// is exactly how a reply re-wakes the sender.
#[derive(Debug, Clone)]
pub struct AgentMessage {
    /// Durable company-message receipt. Empty only for fresh user input and
    /// isolated tests; inter-agent routing assigns it before delivery.
    pub message_id: String,
    /// Stable UI/business identity for the delegated unit of work. Company
    /// messages normally reuse their durable `message_id`; detached jobs may
    /// instead use the durable return-delivery id so spawn and return keep the
    /// same identity across a gateway restart.
    pub handoff_id: String,
    /// The handoff this message is answering, when this message is a return.
    pub reply_to: Option<String>,
    /// The durable delivery/event that caused this message, when known.
    pub causation_id: Option<String>,
    /// Runtime-bound prerequisite bodies already present in this dispatch.
    /// Never inferred from model-authored text; used only to avoid duplicating
    /// these same records in the additional group transcript context.
    pub group_input_receipts: Vec<String>,
    pub from: AgentAddress,
    pub to: AgentAddress,
    pub subject: String,
    pub body: String,
    /// Queue/steer urgency. Equal-priority messages remain FIFO.
    pub priority: MessagePriority,
    pub kind: MessageKind,
}

impl AgentMessage {
    /// Runtime-generated failure receipts are not successful completions,
    /// even when they preserve useful evidence from earlier tool calls.
    pub fn is_failed_result(&self) -> bool {
        Self::failed_result_parts(&self.subject, &self.body)
    }

    pub(crate) fn failed_result_parts(subject: &str, body: &str) -> bool {
        subject.contains("turn hit an internal error")
            || subject.contains("turn failed")
            || body.starts_with("The provider became unavailable after")
            || body.starts_with("Phoenix stopped this agent at a hard runtime boundary:")
            || body.starts_with("I could not advance because my only next action repeated the already-blocked")
            || subject == "Unfinished workflow checks remain"
            || subject == "Unfinished task items remain"
    }

    pub(crate) fn outcome_completion(messages: &[Self]) -> crate::runtime::OutcomeCompletion {
        if messages.is_empty() {
            crate::runtime::OutcomeCompletion::Unknown
        } else if messages.iter().any(Self::is_failed_result) {
            crate::runtime::OutcomeCompletion::Incomplete
        } else {
            crate::runtime::OutcomeCompletion::Completed
        }
    }

    pub fn is_correlated_return(&self) -> bool {
        self.reply_to.as_deref().is_some_and(|id| !id.trim().is_empty())
            && matches!(self.kind, MessageKind::Talk { reply_expected: false })
    }

    /// Build an inter-agent talk.
    pub fn talk(
        from: AgentAddress,
        to: AgentAddress,
        subject: impl Into<String>,
        body: impl Into<String>,
        reply_expected: bool,
    ) -> Self {
        Self {
            message_id: String::new(),
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            group_input_receipts: Vec::new(),
            from,
            to,
            subject: subject.into(),
            body: body.into(),
            priority: MessagePriority::Normal,
            kind: MessageKind::Talk { reply_expected },
        }
    }

    /// Build a fresh user request addressed to an agent (usually the orchestrator).
    pub fn user_input(to: AgentAddress, body: impl Into<String>) -> Self {
        Self {
            message_id: String::new(),
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            group_input_receipts: Vec::new(),
            from: AgentAddress::User,
            to,
            subject: "User message".to_string(),
            body: body.into(),
            priority: MessagePriority::Normal,
            kind: MessageKind::UserInput,
        }
    }

    pub fn reply_expected(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Talk {
                reply_expected: true
            }
        )
    }

    pub fn is_self_talk(&self) -> bool {
        matches!(self.kind, MessageKind::Talk { .. })
            && (self.from == self.to || same_agent_identity(&self.from.label(), &self.to.label()))
    }

    /// Exact correlation key used by lifecycle events and durable transcripts.
    /// Legacy/synthetic messages only have `message_id`, so retain that as the
    /// fallback instead of forcing callers back to subject/agent guesses.
    pub fn correlation_id(&self) -> &str {
        if self.handoff_id.trim().is_empty() {
            self.message_id.as_str()
        } else {
            self.handoff_id.as_str()
        }
    }

    pub fn recover_priority_from_body(&mut self) {
        self.priority = priority_from_transport_body(&self.body);
    }
}

/// A per-agent inbox (FIFO mailbox). An agent is **dormant** when its inbox is
/// empty and **wakes** to process messages when one arrives.
#[derive(Debug, Default)]
pub struct Inbox {
    queue: VecDeque<AgentMessage>,
}

impl Inbox {
    pub fn push(&mut self, msg: AgentMessage) {
        let rank = msg.priority.rank();
        let index = self
            .queue
            .iter()
            .position(|queued| queued.priority.rank() < rank)
            .unwrap_or(self.queue.len());
        self.queue.insert(index, msg);
    }
    pub fn pop(&mut self) -> Option<AgentMessage> {
        self.queue.pop_front()
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

/// Routes messages to per-address inboxes. The gateway loop drains inboxes and
/// wakes the corresponding agent sessions. Durable ownership remains in the
/// company database; the gateway rebuilds this in-memory projection on start.
#[derive(Debug, Default)]
pub struct MessageBus {
    inboxes: HashMap<AgentAddress, Inbox>,
}

impl MessageBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver a message into the recipient's inbox (a `talk` from any agent).
    pub fn send(&mut self, msg: AgentMessage) -> anyhow::Result<()> {
        anyhow::ensure!(
            !msg.is_self_talk(),
            "coworker `{}` cannot send a talk message to itself",
            msg.from.label()
        );
        self.inboxes.entry(msg.to.clone()).or_default().push(msg);
        Ok(())
    }

    /// Take the next queued message for an address — an agent waking to work.
    pub fn next_for(&mut self, addr: &AgentAddress) -> Option<AgentMessage> {
        self.inboxes.get_mut(addr).and_then(Inbox::pop)
    }

    /// Hold partial returns while siblings work, but allow a callee's question
    /// through: its owner may need to answer before the callee can finish.
    pub fn next_return_batch(&mut self, addr: &AgentAddress, waiting: bool) -> Vec<AgentMessage> {
        let Some(inbox) = self.inboxes.get_mut(addr) else { return Vec::new() };
        let mut messages = Vec::new();
        while let Some(message) = inbox.pop() { messages.push(message); }
        let position = messages.iter().position(|message| !waiting || !message.is_correlated_return());
        let mut selected = Vec::new();
        if let Some(position) = position {
            selected.push(messages.remove(position));
            if selected[0].is_correlated_return() {
                let mut index = 0;
                while index < messages.len() {
                    if messages[index].is_correlated_return() { selected.push(messages.remove(index)); }
                    else { index += 1; }
                }
            }
        }
        for message in messages { inbox.push(message); }
        selected
    }

    /// Addresses that currently have queued work, for the gateway scheduler.
    pub fn pending_addresses(&self) -> Vec<AgentAddress> {
        self.inboxes
            .iter()
            .filter(|(_, inbox)| !inbox.is_empty())
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Any work anywhere? When false, every agent is dormant (the gateway idles).
    pub fn has_pending(&self) -> bool {
        self.inboxes.values().any(|inbox| !inbox.is_empty())
    }

    pub fn pending_count(&self, addr: &AgentAddress) -> usize {
        self.inboxes.get(addr).map_or(0, Inbox::len)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mixed_mesh_results_preserve_interrupted_completion() {
        use crate::runtime::OutcomeCompletion;
        let reply = |subject, body| super::AgentMessage::talk(super::AgentAddress::Orchestrator,
            super::AgentAddress::User, subject, body, false);
        let good = reply("Result", "The requested file is ready.");
        let interrupted = reply("Provider unavailable after tool evidence was collected",
            "The provider became unavailable after this agent had already performed work. Saved tests passed.");
        assert_eq!(super::AgentMessage::outcome_completion(&[]), OutcomeCompletion::Unknown);
        assert_eq!(super::AgentMessage::outcome_completion(&[good.clone()]), OutcomeCompletion::Completed);
        assert_eq!(super::AgentMessage::outcome_completion(&[good.clone(), interrupted.clone()]), OutcomeCompletion::Incomplete);
        assert_eq!(super::AgentMessage::outcome_completion(&[interrupted, good]), OutcomeCompletion::Incomplete);
        assert_eq!(super::AgentMessage::outcome_completion(&[reply("Unfinished workflow checks remain", "One part is ready.")]), OutcomeCompletion::Incomplete);
        assert_eq!(super::AgentMessage::outcome_completion(&[reply("Result", "I found the phrase The provider became unavailable after in a log.")]), OutcomeCompletion::Completed);
    }

    use super::*;

    #[test]
    fn pending_return_does_not_block_a_callee_question() {
        let mut bus = MessageBus::new();
        let owner = AgentAddress::Orchestrator;
        let peer = AgentAddress::from_talk_name("coder").unwrap();
        let mut returned = AgentMessage::talk(peer.clone(), owner.clone(), "Result", "First result", false);
        returned.reply_to = Some("first".into());
        bus.send(returned).unwrap();
        bus.send(AgentMessage::talk(peer, owner.clone(), "Question", "Need a clarification", true)).unwrap();
        let question = bus.next_return_batch(&owner, true);
        assert_eq!(question.len(), 1);
        assert!(question[0].reply_expected());
        assert!(bus.next_return_batch(&owner, true).is_empty());
        assert_eq!(bus.next_return_batch(&owner, false).len(), 1);
        assert!(!bus.has_pending());
    }

    #[test]
    fn runtime_failure_receipts_are_not_successful_returns() {
        for (subject, body) in [
            ("scribe turn failed", "Provider usage limit reached"),
            ("Provider unavailable after tool evidence was collected", "The provider became unavailable after this agent had already performed work."),
            ("Stopped", "Phoenix stopped this agent at a hard runtime boundary: deadline"),
        ] {
            let mut message = AgentMessage::talk(coder(), AgentAddress::Orchestrator, subject, body, false);
            message.reply_to = Some("original-assignment".into());
            assert!(message.is_failed_result());
            assert!(message.is_correlated_return());
        }
        let message = AgentMessage::talk(coder(), AgentAddress::Orchestrator, "Report", "Tests passed; prior failures were fixed.", false);
        assert!(!message.is_failed_result());
        assert!(!message.is_correlated_return());
    }

    fn coder() -> AgentAddress {
        AgentAddress::Specialist(SubAgentType::Coder)
    }
    fn researcher() -> AgentAddress {
        AgentAddress::Specialist(SubAgentType::Researcher)
    }

    #[test]
    fn send_then_receive_is_fifo() {
        let mut bus = MessageBus::new();
        bus.send(AgentMessage::talk(
            AgentAddress::Orchestrator,
            coder(),
            "first",
            "do A",
            true,
        ))
        .unwrap();
        bus.send(AgentMessage::talk(
            AgentAddress::Orchestrator,
            coder(),
            "second",
            "do B",
            true,
        ))
        .unwrap();
        assert_eq!(bus.pending_count(&coder()), 2);
        assert_eq!(bus.next_for(&coder()).unwrap().subject, "first");
        assert_eq!(bus.next_for(&coder()).unwrap().subject, "second");
        assert!(bus.next_for(&coder()).is_none());
    }

    #[test]
    fn urgent_messages_jump_normal_work_without_reversing_equal_priority() {
        let mut bus = MessageBus::new();
        let normal = AgentMessage::talk(
            AgentAddress::Orchestrator,
            coder(),
            "normal",
            "ordinary",
            false,
        );
        let mut urgent_one = AgentMessage::talk(
            AgentAddress::Orchestrator,
            coder(),
            "urgent one",
            "fix now",
            false,
        );
        urgent_one.priority = MessagePriority::Urgent;
        let mut urgent_two = AgentMessage::talk(
            AgentAddress::Orchestrator,
            coder(),
            "urgent two",
            "then this",
            false,
        );
        urgent_two.priority = MessagePriority::Urgent;
        bus.send(normal).unwrap();
        bus.send(urgent_one).unwrap();
        bus.send(urgent_two).unwrap();
        assert_eq!(bus.next_for(&coder()).unwrap().subject, "urgent one");
        assert_eq!(bus.next_for(&coder()).unwrap().subject, "urgent two");
        assert_eq!(bus.next_for(&coder()).unwrap().subject, "normal");
    }

    #[test]
    fn routing_keeps_inboxes_separate() {
        let mut bus = MessageBus::new();
        bus.send(AgentMessage::talk(
            AgentAddress::Orchestrator,
            researcher(),
            "web",
            "find repos; then hand to coder",
            true,
        ))
        .unwrap();
        // Researcher chains to coder directly (the mesh: any agent → any agent).
        bus.send(AgentMessage::talk(
            researcher(),
            coder(),
            "integrate",
            "wire these findings",
            true,
        ))
        .unwrap();
        let mut pending = bus.pending_addresses();
        pending.sort_by_key(AgentAddress::label);
        assert_eq!(pending, vec![coder(), researcher()]);
        assert!(bus.has_pending());
        assert_eq!(
            bus.next_for(&researcher()).unwrap().from,
            AgentAddress::Orchestrator
        );
        assert_eq!(bus.next_for(&coder()).unwrap().from, researcher());
        assert!(!bus.has_pending());
    }

    #[test]
    fn user_input_and_reply_expectation() {
        let user_msg = AgentMessage::user_input(AgentAddress::Orchestrator, "hi who are you");
        assert_eq!(user_msg.kind, MessageKind::UserInput);
        assert!(!user_msg.reply_expected());

        let fire_and_forget =
            AgentMessage::talk(AgentAddress::Orchestrator, coder(), "fyi", "note", false);
        assert!(!fire_and_forget.reply_expected());
    }

    #[test]
    fn address_labels_match_agent_names() {
        assert_eq!(AgentAddress::Orchestrator.label(), "orchestrator");
        assert_eq!(researcher().label(), "researcher");
        assert_eq!(AgentAddress::User.label(), "user");
    }

    #[test]
    fn talk_target_strings_route_to_addresses() {
        assert_eq!(AgentAddress::from_talk_name("coder"), Some(coder()));
        assert_eq!(AgentAddress::from_talk_name("research"), Some(researcher()));
        assert_eq!(
            AgentAddress::from_talk_name("Orchestrator"),
            Some(AgentAddress::Orchestrator)
        );
        assert_eq!(
            AgentAddress::from_talk_name("Phoenix"),
            Some(AgentAddress::Orchestrator)
        );
        assert_eq!(
            AgentAddress::from_talk_name("user"),
            Some(AgentAddress::User)
        );
        assert_eq!(AgentAddress::from_talk_name("nobody"), None);
    }

    #[test]
    fn aliases_display_labels_and_instances_share_one_identity() {
        assert!(same_agent_identity("coder", "Leo"));
        assert!(same_agent_identity("coder#2", "database"));
        assert!(same_agent_identity("Phoenix (orchestrator)", "@phoenix"));
        assert!(same_agent_identity("Avery (school_coach)", "school-coach"));
        assert!(!same_agent_identity("coder", "researcher"));
    }

    #[test]
    fn mailbox_rejects_a_typed_self_talk_before_queueing() {
        let mut bus = MessageBus::new();
        let result = bus.send(AgentMessage::talk(
            coder(),
            coder(),
            "loop",
            "do not enqueue this",
            true,
        ));
        assert!(result.is_err());
        assert!(!bus.has_pending());
        assert_eq!(bus.pending_count(&coder()), 0);

        let retired_alias = AgentAddress::Specialist(SubAgentType::Database);
        assert!(bus
            .send(AgentMessage::talk(
                retired_alias,
                coder(),
                "same owner through a retired alias",
                "do not enqueue this either",
                true,
            ))
            .is_err());
        assert!(!bus.has_pending());
    }
}
