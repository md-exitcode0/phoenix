//! The modern execution paths: the scaffold first-slice and the live
//! mesh slice (persist message → cognee preload → gateway mesh run → save).

use super::*;
use crate::runtime::mailbox::AgentMessage;

fn design_reference_paths(context: &[crate::runtime::ContextItem]) -> Result<Vec<std::path::PathBuf>> {
    let mut items = context.iter().filter(|item| item.label == "phoenix_design_reference_paths");
    let Some(item) = items.next() else { return Ok(Vec::new()); };
    anyhow::ensure!(items.next().is_none() && item.content.len() <= 160 * 1024,
        "duplicate or oversized typed design-reference context");
    let paths: Vec<String> = serde_json::from_str(&item.content)
        .context("invalid typed design-reference context")?;
    anyhow::ensure!(paths.len() <= 32, "too many user design references");
    paths.into_iter().map(|value| {
        let path = std::path::PathBuf::from(&value);
        anyhow::ensure!(!value.is_empty() && value.len() <= 4096 && path.is_absolute(),
            "user design reference must be a bounded absolute path");
        Ok(path)
    }).collect()
}

/// Finite per-turn contribution delivery: one saved reply per participant.
/// Progress may be best-effort, but committed replies must survive channel
/// backpressure and retain their publication order without blocking actors.
struct GroupContributionPublisher {
    queue: Option<tokio::sync::mpsc::Sender<CliEvent>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl GroupContributionPublisher {
    fn new(destination: Option<tokio::sync::mpsc::Sender<CliEvent>>) -> Self {
        let Some(destination) = destination else { return Self { queue: None, stop: None, worker: None }; };
        let (queue, mut receiver) = tokio::sync::mpsc::channel(128);
        let (stop,mut stopping)=tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _=&mut stopping=>{
                        receiver.close();
                        while let Some(event)=receiver.recv().await {
                            destination.send(event).await.map_err(|_|anyhow::anyhow!("group contribution channel closed; saved replies remain recoverable"))?;
                        }
                        break;
                    }
                    event=receiver.recv()=>{
                        let Some(event)=event else {break;};
                        destination.send(event).await.map_err(|_|anyhow::anyhow!("group contribution channel closed; saved replies remain recoverable"))?;
                    }
                }
            }
            Ok(())
        });
        Self { queue: Some(queue), stop:Some(stop), worker: Some(worker) }
    }

    #[cfg(test)]
    fn publish(&self, event: CliEvent) -> Result<()> {
        if let Some(queue) = &self.queue {
            queue.try_send(event).map_err(|_| anyhow::anyhow!("group contribution queue unavailable; saved replies remain recoverable"))?;
        }
        Ok(())
    }

    fn publish_pair(&self, contribution: CliEvent, status: CliEvent) -> Result<()> {
        if let Some(queue) = &self.queue {
            // Reserve both slots before publishing either event. Capacity
            // pressure must not admit a reply but reject its associated state.
            let mut permits = queue.try_reserve_many(2)
                .map_err(|_| anyhow::anyhow!("group contribution pair unavailable; saved replies remain recoverable"))?;
            permits.next().expect("two reserved slots").send(contribution);
            permits.next().expect("two reserved slots").send(status);
        }
        Ok(())
    }

    async fn finish(mut self) -> Result<()> {
        if let Some(stop)=self.stop.take(){let _=stop.send(());}
        self.queue.take();
        if let Some(worker) = self.worker.as_mut() {
            worker.await.context("group contribution delivery worker failed")??;
        }
        self.worker.take();
        Ok(())
    }
}

impl Drop for GroupContributionPublisher {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() { worker.abort(); }
    }
}

#[tokio::test]
async fn group_publication_waits_for_capacity_and_preserves_order() {
    let make = |id: &str| CliEvent::GroupMessage {
        message_id:id.into(), group_id:"fixture".into(), round:1,
        agent_id:"iris".into(), agent_name:"Iris".into(), markdown:id.into(), reply_to:None, causation_id:None,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(make("occupied")).unwrap();
    assert!(tx.try_send(make("old-dropped")).is_err(), "old best-effort path loses capacity race");
    let publisher = GroupContributionPublisher::new(Some(tx));
    publisher.publish_pair(make("first"), CliEvent::GroupMemberStatus {
        turn_id:"turn".into(), group_id:"fixture".into(), agent_id:"iris".into(),
        agent_name:"Iris".into(), state:"done".into(), detail:"saved".into(),
    }).unwrap();
    publisher.queue.as_ref().unwrap().send(CliEvent::GroupMemberStatus {
        turn_id:"turn".into(),group_id:"fixture".into(),agent_id:"builder".into(),
        agent_name:"Builder".into(),state:"working".into(),detail:"prerequisite received".into(),
    }).await.unwrap();
    publisher.publish(make("second")).unwrap();
    let retained_status_sender=publisher.queue.clone();
    let finish = tokio::spawn(publisher.finish());
    for expected in ["occupied", "first", "done", "working", "second"] {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
        match event {
            CliEvent::GroupMessage { message_id, .. } => assert_eq!(message_id, expected),
            CliEvent::GroupMemberStatus { state, .. } => assert_eq!(state, expected),
            _ => panic!("unexpected event"),
        }
    }
    finish.await.unwrap().unwrap();
    drop(retained_status_sender);
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let publisher = GroupContributionPublisher::new(Some(tx));
    publisher.publish(make("closed")).unwrap();
    drop(rx);
    assert!(publisher.finish().await.is_err());
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(make("occupied")).unwrap();
    let publisher = GroupContributionPublisher::new(Some(tx));
    publisher.publish(make("canceled")).unwrap();
    drop(publisher);
    assert!(rx.recv().await.is_some());
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().is_none());
    // No worker consumes this queue: exactly one remaining slot exercises
    // partial-admission rejection deterministically.
    let (queue, mut rx) = tokio::sync::mpsc::channel(2);
    queue.try_send(make("occupied")).unwrap();
    let publisher = GroupContributionPublisher { queue:Some(queue), stop:None, worker:None };
    assert!(publisher.publish_pair(make("rejected-reply"), make("rejected-state")).is_err());
    assert_eq!(rx.len(), 1, "failed pair must not insert a prefix");
    rx.recv().await.unwrap();
    publisher.publish_pair(make("reply"), make("state")).unwrap();
    assert_eq!(rx.len(), 2);
}

fn group_participant_address(
    participant: &crate::runtime::group_conversation::GroupParticipant,
) -> Result<crate::runtime::mailbox::AgentAddress> {
    use crate::runtime::mailbox::AgentAddress;
    if matches!(
        participant.internal_role.as_str(),
        "phoenix" | "orchestrator"
    ) {
        return Ok(AgentAddress::Orchestrator);
    }
    crate::runtime::delegation::specialist_from_talk_name(&participant.internal_role)
        .map(AgentAddress::Specialist)
        .with_context(|| {
            format!(
                "group member `{}` has no active runtime role `{}`",
                participant.display_name, participant.internal_role
            )
        })
}

fn split_user_messages_by_owner(
    messages: Vec<crate::runtime::mailbox::AgentMessage>,
    owner: &crate::runtime::mailbox::AgentAddress,
) -> (
    Vec<crate::runtime::mailbox::AgentMessage>,
    Vec<crate::runtime::mailbox::AgentMessage>,
) {
    messages
        .into_iter()
        .partition(|message| &message.from == owner)
}

/// Once the owner has answered, independent helper results belong in its
/// next-turn context. Re-running the owner on an unrelated late return can
/// replace a completed answer with a status-only "Nothing new" message.
fn record_late_helper_results(
    state_root: &std::path::Path,
    session_id: &str,
    task_id: &str,
    owner: &crate::runtime::mailbox::AgentAddress,
    helpers: &[AgentMessage],
) -> Result<()> {
    use sha2::{Digest, Sha256};
    let mut store = SessionStore::new(state_root.join("sessions"));
    store.load_one_if_absent(session_id)?;
    let session = store.get_mut(session_id)
        .context("completed owner session is missing while saving late helper results")?;
    for helper in helpers {
        let identity = format!("{task_id}\0{}\0{}\0{}\0{}", helper.message_id,
            helper.from.label(), helper.subject, helper.body);
        let id = format!("late-helper-{:x}", Sha256::digest(identity.as_bytes()));
        let marker = format!("<!-- phoenix-late-helper:{id} -->");
        if session.messages.iter().any(|message| matches!(message, Message::Talk { body, .. } if body.contains(&marker))) {
            continue;
        }
        session.push_message(Message::Talk {
            from: helper.from.label(), to: owner.label(), subject: helper.subject.clone(),
            body: format!("{}\n\n{marker}", helper.body), reply_expected: false,
            handoff_id: id.clone(), reply_to: helper.reply_to.clone().or(Some(id)),
            causation_id: helper.causation_id.clone(),
            status: if helper.is_failed_result() { "blocked" } else { "done" }.to_string(),
        });
    }
    store.save_one(session_id).context("could not save late helper results for the completed owner")
}

fn group_participant_for_role<'a>(
    group: &'a crate::runtime::group_conversation::GroupTurnContext,
    role: &str,
) -> Option<&'a crate::runtime::group_conversation::GroupParticipant> {
    group.participants.iter().find(|participant| {
        participant.internal_role == role
            || (role == "orchestrator" && participant.internal_role == "phoenix")
    })
}

/// Resolve only canonical committed prose. Provider retries, summaries and
/// historical messages from other turns cannot create a fresh room delivery.
fn reserve_saved_group_pings(
    state_root: &std::path::Path,
    company: &crate::runtime::company::CompanyStore,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
) -> Result<Vec<String>> {
    let Some(session) = SessionStore::read_one_from_disk(
        &state_root.join("sessions"), &group.canonical_session_id,
    )? else { return Ok(Vec::new()); };
    let mut added = Vec::new();
    for message in &session.messages {
        let Message::GroupContribution { turn_id: saved_turn, group_id, agent_id,
            message_id, subject, body, .. } = message else { continue };
        if saved_turn != turn_id || group_id != &group.group_id
            || AgentMessage::failed_result_parts(subject, body) { continue; }
        if !group.participants.iter().any(|member| member.agent_id == *agent_id) { continue; }
        for target in crate::runtime::group_conversation::authored_ping_targets(body, &group.participants) {
            if target.agent_id == *agent_id { continue; }
            if company.reserve_group_ping(group, turn_id, agent_id, message_id, target)? {
                added.push(target.agent_id.clone());
            }
        }
    }
    Ok(added)
}

fn saved_group_ping_message(
    state_root: &std::path::Path,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    member: &crate::runtime::group_conversation::GroupMemberActivationRecord,
) -> Result<AgentMessage> {
    let receipt = member.source_receipt_id.as_deref().context("room ping has no source receipt")?;
    let session = SessionStore::read_one_from_disk(&state_root.join("sessions"), &group.canonical_session_id)?
        .context("room ping has no canonical conversation")?;
    let (author, body) = session.messages.iter().find_map(|message| match message {
        Message::GroupContribution { turn_id: saved_turn, group_id, agent_id, message_id, body, subject, .. }
            if saved_turn == turn_id && group_id == &group.group_id && message_id == receipt
                && !AgentMessage::failed_result_parts(subject, body) => Some((agent_id, body)),
        _ => None,
    }).context("room ping source receipt is unavailable")?;
    let sender = group.participants.iter().find(|participant| participant.agent_id == *author)
        .context("room ping author is not a participant")?;
    anyhow::ensure!(crate::runtime::group_conversation::authored_ping_targets(body, &group.participants)
        .iter().any(|target| target.agent_id == member.participant.agent_id),
        "saved room contribution does not mention this recipient");
    let mut message = AgentMessage::talk(
        group_participant_address(sender)?, group_participant_address(&member.participant)?,
        format!("Room mention from {}", sender.display_name),
        format!("{} mentioned you in the {} room. Reply directly here under your own name.\n\n{}",
            sender.display_name, group.group_name, body), false,
    );
    message.handoff_id = format!("group-ping:{turn_id}:{}", member.participant.agent_id);
    message.causation_id = Some(receipt.to_string());
    message.group_input_receipts = vec![receipt.to_string()];
    Ok(message)
}

struct GroupActivationWorkGuard {
    canonical_session_id: String,
    turn_id: String,
    agent_id: String,
    settled: bool,
}

fn take_ready_group_messages(
    pending: &mut Vec<(String, AgentMessage)>,
    dependencies: &[crate::runtime::group_conversation::GroupDependency],
    completed: &std::collections::HashSet<String>,
    submitted: &mut std::collections::HashSet<String>,
) -> Vec<AgentMessage> {
    let mut ready = Vec::new();
    let mut index = 0;
    while index < pending.len() {
        let id = &pending[index].0;
        if dependencies
            .iter()
            .filter(|edge| &edge.dependent == id)
            .all(|edge| completed.contains(&edge.prerequisite))
        {
            let (id, message) = pending.remove(index);
            if submitted.insert(id) {
                ready.push(message);
            }
        } else {
            index += 1;
        }
    }
    ready
}

/// Persist a rejected branch before returning to the dispatcher. A transferred
/// assignment is a fence: its successor, not this old run, owns descendants.
fn reject_group_input_branch(
    root: &str,
    dependencies: &[crate::runtime::group_conversation::GroupDependency],
    completed: &std::collections::HashSet<String>,
    mut reject: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    let mut pending = std::collections::VecDeque::from([root.to_string()]);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop_front() {
        if completed.contains(&id) || !seen.insert(id.clone()) || !reject(&id)? {
            continue;
        }
        let mut children = dependencies.iter()
            .filter(|edge| edge.prerequisite == id)
            .map(|edge| edge.dependent.clone()).collect::<Vec<_>>();
        children.sort();
        pending.extend(children);
    }
    Ok(())
}

/// Bind a ready assignment to the exact persisted results that released it.
/// Read the canonical record, not a model's latest (possibly duplicate) reply.
fn prepare_group_dispatch(
    mut messages: Vec<AgentMessage>,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    dependencies: &[crate::runtime::group_conversation::GroupDependency],
    receipts: &[(String, String)],
    state_root: &std::path::Path,
) -> Result<Vec<AgentMessage>> {
    let mut requested = Vec::new();
    let mut inputs_by_message = Vec::new();
    for message in &messages {
        let participant = group_participant_for_role(group, &message.to.label())
            .context("ready group dispatch has no participant")?;
        let mut inputs = Vec::new();
        for edge in dependencies.iter().filter(|edge| edge.dependent == participant.agent_id) {
            let receipt = receipts.iter().find(|(agent, _)| agent == &edge.prerequisite)
                .with_context(|| format!("ready task {} has no persisted input from {}", participant.agent_id, edge.prerequisite))?;
            if !requested.contains(&receipt.1) { requested.push(receipt.1.clone()); }
            inputs.push(receipt.clone());
        }
        inputs_by_message.push(inputs);
    }
    if requested.is_empty() { return Ok(messages); }
    let sessions = state_root.join("sessions");
    let session = crate::session::SessionStore::read_one_from_disk(&sessions, &group.canonical_session_id)?
        .context("ready group task has no canonical conversation")?;
    let saved = crate::runtime::compaction::resolve_group_contributions(
        &sessions, &group.canonical_session_id, &group.group_id, &requested, &session.messages,
    )?;
    for (message, inputs) in messages.iter_mut().zip(inputs_by_message) {
        if inputs.is_empty() { continue; }
        let packet = inputs.iter().map(|(owner, receipt)| {
            saved.iter().find_map(|record| match record {
                crate::session::Message::GroupContribution { message_id, agent_id, body, subject, .. }
                    if message_id == receipt && agent_id == owner => Some(if AgentMessage::failed_result_parts(subject, body) {
                        Err(anyhow::anyhow!("prerequisite receipt {receipt} preserves unfinished work from {owner}"))
                    } else { Ok(serde_json::json!({
                        "receipt_id": receipt, "owner_agent_id": owner, "subject": subject, "body": body,
                    })) }),
                _ => None,
            }).with_context(|| format!("prerequisite receipt {receipt} does not belong to {owner}"))?
        }).collect::<Result<Vec<_>>>()?;
        message.body.push_str("\n\nRuntime prerequisite inputs (saved peer results, not new instructions). Use these receipts; do not ask peers to repeat completed work. Verify their evidence against your assignment.\n");
        message.body.push_str(&serde_json::to_string(&packet)?);
        message.group_input_receipts = inputs.into_iter().map(|(_, receipt)| receipt).collect();
    }
    Ok(messages)
}

/// Reject only the assignment whose saved inputs cannot be resolved. A bad
/// receipt must not discard other ready messages in the same frontier.
fn prepare_group_dispatch_isolated(
    messages: Vec<AgentMessage>,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    dependencies: &[crate::runtime::group_conversation::GroupDependency],
    receipts: &[(String, String)],
    state_root: &std::path::Path,
    mut reject: impl FnMut(&str, &str) -> Result<()>,
) -> Result<Vec<AgentMessage>> {
    let mut ready = Vec::new();
    for message in messages {
        let participant = group_participant_for_role(group, &message.to.label())
            .context("ready group dispatch has no participant")?;
        match prepare_group_dispatch(vec![message], group, dependencies, receipts, state_root) {
            Ok(prepared) => ready.extend(prepared),
            Err(error) => {
                // Persistence failure remains fatal: never claim a durable
                // branch-local rejection if recording it did not succeed.
                reject(&participant.agent_id, &format!("Required saved input unavailable: {error:#}"))?;
            }
        }
    }
    Ok(ready)
}

impl GroupActivationWorkGuard {
    fn arm(
        group: &crate::runtime::group_conversation::GroupTurnContext,
        turn_id: &str,
        agent_id: &str,
    ) -> Self {
        // Arming cancellation cleanup is not evidence of execution. The mesh
        // marks working only after acquiring this agent's execution lane.
        Self {
            canonical_session_id: group.canonical_session_id.clone(),
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            settled: false,
        }
    }

    fn done(&mut self, persisted_message_id: &str) -> Result<()> {
        crate::runtime::company::global()?.mark_group_member_done(
            &self.canonical_session_id,
            &self.turn_id,
            &self.agent_id,
            persisted_message_id,
        )?;
        self.settled = true;
        Ok(())
    }

    fn waiting_user(&mut self, ask_id: &str) -> Result<()> {
        crate::runtime::company::global()?.mark_group_member_waiting_user(
            &self.canonical_session_id,
            &self.turn_id,
            &self.agent_id,
            ask_id,
        )?;
        self.settled = true;
        Ok(())
    }

    fn blocked(mut self, reason: &str) -> Result<()> {
        crate::runtime::company::global()?.mark_group_member_blocked(
            &self.canonical_session_id,
            &self.turn_id,
            &self.agent_id,
            reason,
        )?;
        self.settled = true;
        Ok(())
    }
}

impl Drop for GroupActivationWorkGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Err(error) = crate::runtime::company::global().and_then(|company| {
            company.mark_group_member_blocked(
                &self.canonical_session_id,
                &self.turn_id,
                &self.agent_id,
                "Activation ended before a contribution or waiting-user receipt was persisted",
            )
        }) {
            tracing::error!(
                turn_id = %self.turn_id,
                agent_id = %self.agent_id,
                "could not preserve interrupted group activation as blocked: {error:#}"
            );
        }
    }
}

fn pending_group_ask_ids(session_id: &str, turn_id: &str, agent_id: &str) -> Result<std::collections::HashSet<String>> {
    // An unreadable question store is not evidence that the owner has no
    // pending questions. Never release dependents on an unknown input state.
    let read = || -> Result<std::collections::HashSet<String>> {
        crate::session::SessionStore::validate_session_id(session_id)?;
        let owned = crate::runtime::company::global()?.group_member_ask_ids(session_id, turn_id, agent_id)?;
        let mut pending = std::collections::HashSet::new();
        for id in owned {
            if crate::runtime::asks::group_question_pending(&id, session_id, agent_id)? {
                pending.insert(id);
            }
        }
        Ok(pending)
    };
    read().context("cannot establish pending group questions; dependent work must remain gated")
}

#[test]
fn unreadable_group_question_state_is_not_empty_readiness() {
    // Exercise the real question reader's validation failure without touching
    // the user's ask store or changing process-global configuration.
    let error = pending_group_ask_ids("../invalid-conversation", "turn", "iris")
        .expect_err("unknown question state must never look like no questions");
    assert!(error.to_string().contains("dependent work must remain gated"));
}

// Resolve settlement from the committed receipt, never a divergent retry body.
fn settle_saved_group_contribution(
    state_root: &std::path::Path,
    company: &crate::runtime::company::CompanyStore,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    agent_id: &str,
    receipt: &str,
    pending_ask: Option<&str>,
) -> Result<(bool, bool)> {
    let session = SessionStore::read_one_from_disk(&state_root.join("sessions"), &group.canonical_session_id)?
        .context("cannot settle a contribution without its saved conversation")?;
    let failed = session.messages.iter().find_map(|message| match message {
        Message::GroupContribution { turn_id: saved_turn, group_id, agent_id: saved_agent, message_id, subject, body, .. }
            if saved_turn == turn_id && group_id == &group.group_id && saved_agent == agent_id && message_id == receipt => {
                Some(AgentMessage::failed_result_parts(subject, body))
            }
        _ => None,
    }).context("cannot settle a contribution without its exact saved receipt")? && pending_ask.is_none();
    let retained = if failed {
        company.settle_group_member_failure(&group.canonical_session_id, turn_id, agent_id, receipt)?
    } else {
        company.settle_group_member_result(&group.canonical_session_id, turn_id, agent_id, receipt, pending_ask)?
    };
    Ok((retained, failed))
}

fn persisted_group_contribution(
    state_root: &std::path::Path,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    agent_id: &str,
) -> Result<Option<(String, CliEvent)>> {
    let session = SessionStore::read_one_from_disk(
        &state_root.join("sessions"), &group.canonical_session_id,
    )?.with_context(|| {
        format!(
            "canonical group session `{}` is missing",
            group.canonical_session_id
        )
    })?;
    Ok(session.messages.iter().find_map(|message| match message {
        Message::GroupContribution {
            turn_id: stored_turn,
            message_id,
            group_id,
            agent_id: stored_agent,
            display_name,
            body,
            reply_to,
            causation_id,
            ..
        } if stored_turn == turn_id && group_id == &group.group_id && stored_agent == agent_id => {
            Some((message_id.clone(), CliEvent::GroupMessage {
                message_id: message_id.clone(), group_id: group_id.clone(), round: 1,
                agent_id: stored_agent.clone(), agent_name: display_name.clone(),
                markdown: body.clone(), reply_to: reply_to.clone(), causation_id: causation_id.clone(),
            }))
        }
        _ => None,
    }))
}

/// Whether the assembled turn summary should also be emitted as the final
/// answer. A room already streamed every coworker's message as its own
/// `GroupMessage`, so emitting the summary there would repeat the entire
/// exchange verbatim under the last speaker. An individual conversation emits
/// nothing live, so the summary is its only user-facing surface.
pub(crate) fn should_emit_final_answer(summary: &str, group_messages_already_shown: bool) -> bool {
    !summary.trim().is_empty() && !group_messages_already_shown
}

#[cfg(test)]
fn persist_group_messages(
    state_root: &std::path::Path,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    messages: &[crate::runtime::mailbox::AgentMessage],
) -> Result<Vec<(String, String)>> {
    persist_group_messages_with_events(state_root, group, turn_id, messages).map(|(receipts, _)| receipts)
}

fn persist_group_messages_with_events(
    state_root: &std::path::Path,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    messages: &[crate::runtime::mailbox::AgentMessage],
) -> Result<(Vec<(String, String)>, Vec<CliEvent>)> {
    SessionStore::update_one(&state_root.join("sessions"), &group.canonical_session_id, |saved| {
    let mut session = saved
        .with_context(|| {
            format!(
                "canonical group session `{}` is missing",
                group.canonical_session_id
            )
        })?;
    let mut receipts = Vec::new();
    for (ordinal, message) in messages.iter().enumerate() {
        let role = message.from.label();
        let participant = group_participant_for_role(group, &role)
            .with_context(|| format!("group response came from unknown runtime role `{role}`"))?;
        let message_id = if message.message_id.trim().is_empty() {
            stable_group_contribution_message_id(
                turn_id,
                &group.group_id,
                &participant.agent_id,
                ordinal,
                &message.subject,
                &message.body,
            )
        } else {
            message.message_id.clone()
        };
        if let Some(stored_message_id) = session.messages.iter().find_map(|stored| match stored {
            Message::GroupContribution {
                turn_id: stored_turn,
                message_id: stored_id,
                group_id: stored_group,
                agent_id: stored_agent,
                ..
            } if stored_turn == turn_id
                && stored_group == &group.group_id
                && stored_agent == &participant.agent_id =>
            {
                Some(stored_id.clone())
            }
            _ => None,
        }) {
            // A crash may happen after the canonical transcript commit but
            // before the ledger's done transition. The first contribution is
            // authoritative for this member activation; reuse its receipt and
            // never append a second answer for the same (turn, agent).
            receipts.push((participant.agent_id.clone(), stored_message_id));
            continue;
        }
        session.push_message(Message::GroupContribution {
            turn_id: turn_id.to_string(),
            message_id: message_id.clone(),
            group_id: group.group_id.clone(),
            agent_id: participant.agent_id.clone(),
            internal_role: participant.internal_role.clone(),
            display_name: participant.display_name.clone(),
            role_title: participant.role_title.clone(),
            color: participant.color.clone(),
            icon_seed: participant.icon_seed.clone(),
            avatar: participant.avatar.clone(),
            subject: message.subject.clone(),
            body: message.body.clone(),
            reply_to: message.reply_to.clone(),
            causation_id: message.causation_id.clone(),
        });
        receipts.push((participant.agent_id.clone(), message_id));
    }
    let mut events = Vec::with_capacity(receipts.len());
    for (agent, receipt) in &receipts {
        let event = session.messages.iter().find_map(|record| match record {
            Message::GroupContribution { turn_id: stored_turn, message_id, group_id, agent_id, display_name, body, reply_to, causation_id, .. }
                if stored_turn == turn_id && message_id == receipt && group_id == &group.group_id && agent_id == agent => {
                Some(CliEvent::GroupMessage {
                    message_id: message_id.clone(), group_id: group_id.clone(), round: 1,
                    agent_id: agent_id.clone(), agent_name: display_name.clone(), markdown: body.clone(),
                    reply_to: reply_to.clone(), causation_id: causation_id.clone(),
                })
            }
            _ => None,
        }).context("committed contribution receipt has no matching canonical message")?;
        events.push(event);
    }
    Ok((session, (receipts, events)))
    })
}

/// Persist the human-authored room boundary even when it intentionally wakes
/// nobody. A private marker makes retries idempotent while remaining excluded
/// from the canonical room transcript shown to coworkers and the desktop.
pub(crate) fn persist_group_user_boundary(
    state_root: &std::path::Path,
    group: &crate::runtime::group_conversation::GroupTurnContext,
    turn_id: &str,
    prompt: &str,
    model: &str,
) -> Result<()> {
    const MARKER: &str = "__phoenix_group_user_boundary";
    // A newly created directory entry reserves its canonical identity, not a
    // transcript file. The first authored boundary creates that file. Read
    // this session only; corrupt/unreadable data must not be treated as new.
    SessionStore::update_one(&state_root.join("sessions"), &group.canonical_session_id, |saved| {
    let mut session = saved.unwrap_or_else(|| Session::new_main_with_id(
        &group.canonical_session_id, model, crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
    ));
    let already_persisted=session.messages.iter().any(|message|matches!(message,Message::ToolResult{tool_name,input,..} if tool_name==MARKER&&input==turn_id));
    if already_persisted {
        return Ok((session, ()));
    }
    session.push_message(Message::User {
        content: prompt.to_string(),
    });
    session.ensure_title(prompt);
    session.push_message(Message::ToolResult {
        tool_name: MARKER.to_string(),
        input: turn_id.to_string(),
        success: true,
        output: String::new(),
    });
    Ok((session, ()))
    })
}

fn stable_group_contribution_message_id(
    turn_id: &str,
    group_id: &str,
    agent_id: &str,
    ordinal: usize,
    subject: &str,
    body: &str,
) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for component in [turn_id, group_id, agent_id, subject, body] {
        digest.update(component.as_bytes());
        digest.update([0]);
    }
    digest.update(ordinal.to_le_bytes());
    let hash = digest.finalize();
    let suffix = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("group-message-{suffix}")
}

fn diagnostic_memory_bundle(scope: SessionScope, task: &TaskEnvelope) -> MemoryBundle {
    MemoryBundle {
        session_scope: scope,
        task_frame: format!("{} :: {}", task.title, task.user_request),
        loaded_memory_paths: vec![],
        loaded_knowledge_paths: vec![],
        ranked_context_items: vec![],
        omitted_items: vec![],
        grounding_receipts: vec![],
        completion_state: "scaffold diagnostic: durable memory disabled".to_string(),
        open_questions: vec![],
        recommended_next_agent_or_tool: None,
        context_budget_used: 0,
        summary: "Scaffold diagnostic ran without durable memory I/O.".to_string(),
    }
}

fn diagnostic_memory_pass(scope: SessionScope, phase: LibrarianPhase) -> LibrarianPassRecord {
    let phase_label = match phase {
        LibrarianPhase::Preload => "preload",
        LibrarianPhase::Prune => "prune",
        LibrarianPhase::Save => "save",
    };
    LibrarianPassRecord {
        session_scope: scope,
        phase,
        memory_paths: vec![],
        knowledge_paths: vec![],
        saved_memory_paths: vec![],
        omitted_items: vec![],
        receipts: vec![format!(
            "scaffold diagnostic: durable memory {phase_label} intentionally disabled"
        )],
        context_budget_used: 0,
        pruned_message_count: 0,
        summary: format!(
            "Scaffold diagnostic skipped durable memory {phase_label}; no Phoenix memory I/O occurred."
        ),
    }
}

fn diagnostic_preload(
    scope: SessionScope,
    task: &TaskEnvelope,
) -> (
    MemoryBundle,
    LibrarianPassRecord,
    crate::librarian::LoadedMemories,
) {
    (
        diagnostic_memory_bundle(scope.clone(), task),
        diagnostic_memory_pass(scope, LibrarianPhase::Preload),
        crate::librarian::LoadedMemories::default(),
    )
}

impl AgentRunner {
    pub async fn execute_first_slice(
        &self,
        orchestrator: &Orchestrator,
        task: &TaskEnvelope,
    ) -> Result<RuntimeExecution> {
        if !matches!(task.target_agent, AgentTarget::Orchestrator) {
            bail!("first runtime slice expects an orchestrator task");
        }

        // DisabledDiagnostic is wholly ephemeral: do not even construct a
        // production-rooted store/cache. Besides avoiding cross-process
        // Cognee contention, this keeps mock transcripts out of the idle
        // session-digest scanner by construction.
        let persistent_state_enabled = self.memory_policy == RunnerMemoryPolicy::Enabled;
        let mut session_store = if persistent_state_enabled {
            let mut store = SessionStore::new(self.state_root.join("sessions"));
            store.load_from_disk()?;
            Some(store)
        } else {
            None
        };

        let main_session_id = task.session_id.clone();
        let main_session_status = if session_store
            .as_ref()
            .is_some_and(|store| store.get(&main_session_id).is_some())
        {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut main_session = match session_store.as_mut() {
            Some(store) => store.load_or_create_main(
                &main_session_id,
                &orchestrator.spec().default_model,
                &orchestrator.spec().system_prompt,
            )?,
            None => Session::new_main_with_id(
                &main_session_id,
                &orchestrator.spec().default_model,
                &orchestrator.spec().system_prompt,
            ),
        };
        let main_session_path = session_store
            .as_ref()
            .map(|store| store.session_path(&main_session_id))
            .unwrap_or_default();
        main_session.push_message(Message::User {
            content: task.user_request.clone(),
        });

        let cache_root = persistent_state_enabled.then(|| self.state_root.join("session_cache"));
        let main_cache_path = cache_root
            .as_ref()
            .map(|root| SessionCache::path_for(root, &main_session_id))
            .unwrap_or_default();
        let main_cache_status = if persistent_state_enabled && main_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut session_cache = match cache_root.as_ref() {
            Some(root) => SessionCache::load_or_create(root, &main_session_id)?,
            None => SessionCache::new(&main_session_id),
        };
        let (main_bundle, main_preload_record, _loaded_main) = if persistent_state_enabled {
            memory_hooks::preload(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                self.librarian_model(&orchestrator.spec().default_model),
                SessionScope::Main,
                task,
                &main_session,
                &mut session_cache,
                self.event_tx.clone(),
            )
            .await?
        } else {
            diagnostic_preload(SessionScope::Main, task)
        };
        let mut librarian_passes = vec![main_preload_record];

        let decision = orchestrator.route_task(task)?;
        self.emit(CliEvent::Routing {
            target: match &decision.target {
                AgentTarget::Orchestrator => "Direct answer".to_string(),
                AgentTarget::Specialist(st) => format!("{:?}", st),
            },
            rationale: decision.rationale.clone(),
        });
        if !matches!(decision.mode, DelegationMode::Handoff)
            || !matches!(
                decision.target,
                AgentTarget::Specialist(SubAgentType::Coder)
            )
        {
            bail!("first runtime slice only supports orchestrator handoff to coder");
        }

        let coder = coder_config();
        let specialist_session_id =
            crate::session::specialist_session_id(&main_session.id, SubAgentType::Coder);
        let specialist_session_status = if session_store
            .as_ref()
            .is_some_and(|store| store.get(&specialist_session_id).is_some())
        {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session = match session_store.as_mut() {
            Some(store) => store.load_or_create_specialist(
                &main_session.id,
                SubAgentType::Coder,
                &coder.spec.default_model,
                &coder.spec.system_prompt,
            )?,
            None => Session::new_sub_agent_with_id(
                &specialist_session_id,
                SubAgentType::Coder,
                &coder.spec.default_model,
                &coder.spec.system_prompt,
            ),
        };
        let specialist_session_path = session_store
            .as_ref()
            .map(|store| store.session_path(&specialist_session_id))
            .unwrap_or_default();
        specialist_session.push_message(Message::Talk {
            from: "Orchestrator".to_string(),
            to: "Coder".to_string(),
            subject: task.title.clone(),
            body: task.user_request.clone(),
            reply_expected: true,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: "working".to_string(),
        });

        let specialist_session_id = specialist_session.id.clone();
        let specialist_cache_path = cache_root
            .as_ref()
            .map(|root| SessionCache::path_for(root, &specialist_session_id))
            .unwrap_or_default();
        let specialist_cache_status = if persistent_state_enabled && specialist_cache_path.exists()
        {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session_cache = match cache_root.as_ref() {
            Some(root) => SessionCache::load_or_create(root, &specialist_session_id)?,
            None => SessionCache::new(&specialist_session_id),
        };
        let (specialist_bundle, specialist_preload_record, loaded_specialist) =
            if persistent_state_enabled {
                memory_hooks::preload(
                    &self.memory_root,
                    &self.workspace_root,
                    Arc::clone(&self.provider),
                    self.librarian_model(&coder.spec.default_model),
                    SessionScope::Specialist(SubAgentType::Coder),
                    task,
                    &specialist_session,
                    &mut specialist_session_cache,
                    self.event_tx.clone(),
                )
                .await?
            } else {
                diagnostic_preload(SessionScope::Specialist(SubAgentType::Coder), task)
            };
        librarian_passes.push(specialist_preload_record);

        let specialist_prompt = assemble_prompt_with_context(
            &coder.spec,
            task,
            &loaded_specialist,
            &specialist_session,
            Some(&self.workspace_root),
            Some(self.provider.name()),
            None, // project brain injected on the mesh turn (turn_loop)
        );
        let mut request = specialist_prompt.to_completion_request(
            &coder.spec.default_model,
            Some(specialist_session_id.clone()),
        );
        specialist_prompt.append_tail(&mut request);
        let response = self.provider.complete(request).await?;
        let parsed_turn = parse_coder_turn_result(&response.content)?;

        let specialist_outcome = AgentOutcome {
            completion: crate::runtime::OutcomeCompletion::Completed,
            agent: AgentTarget::Specialist(SubAgentType::Coder),
            summary: parsed_turn.final_markdown.clone(),
            artifacts: {
                let mut artifacts = parsed_turn.to_artifacts();
                artifacts.push(AgentArtifact {
                    kind: ArtifactKind::Plan,
                    title: "Execution contract".to_string(),
                    body: coder.spec.summary(),
                });
                artifacts
            },
            tool_results: parsed_turn.to_tool_results(),
            provider_response: Some(ProviderTurn::from(response)),
        };
        specialist_session.push_message(Message::Assistant {
            content: specialist_outcome.summary.clone(),
        });
        librarian_passes.push(if persistent_state_enabled {
            memory_hooks::prune_session(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                self.librarian_model(&coder.spec.default_model),
                SessionScope::Specialist(SubAgentType::Coder),
                task,
                &mut specialist_session,
                &specialist_bundle,
                &mut specialist_session_cache,
                self.event_tx.clone(),
            )
            .await
        } else {
            diagnostic_memory_pass(
                SessionScope::Specialist(SubAgentType::Coder),
                LibrarianPhase::Prune,
            )
        });

        let specialist_save = if persistent_state_enabled {
            with_preload_context(
                memory_hooks::save_phase(
                    &self.memory_root,
                    &self.workspace_root,
                    Arc::clone(&self.provider),
                    self.librarian_model(&coder.spec.default_model),
                    SessionScope::Specialist(SubAgentType::Coder),
                    task,
                    &specialist_session,
                    &decision,
                    &specialist_outcome,
                    &mut specialist_session_cache,
                    self.event_tx.clone(),
                )
                .await?,
                &specialist_bundle,
            )
        } else {
            diagnostic_memory_pass(
                SessionScope::Specialist(SubAgentType::Coder),
                LibrarianPhase::Save,
            )
        };
        librarian_passes.push(specialist_save);
        if let Some(root) = cache_root.as_ref() {
            specialist_session_cache.save(root)?;
        }
        if let Some(store) = session_store.as_mut() {
            store.upsert(specialist_session.clone());
        }

        let outcome = orchestrator.finalize_after_specialist(task, &decision, &specialist_outcome);
        main_session.push_message(Message::Talk {
            from: "Coder".to_string(),
            to: "Orchestrator".to_string(),
            subject: task.title.clone(),
            body: specialist_outcome.summary.clone(),
            reply_expected: false,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: "done".to_string(),
        });
        main_session.push_message(Message::Assistant {
            content: outcome.summary.clone(),
        });
        librarian_passes.push(if persistent_state_enabled {
            memory_hooks::prune_session(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                &orchestrator.spec().default_model,
                SessionScope::Main,
                task,
                &mut main_session,
                &main_bundle,
                &mut session_cache,
                self.event_tx.clone(),
            )
            .await
        } else {
            diagnostic_memory_pass(SessionScope::Main, LibrarianPhase::Prune)
        });

        let main_save = if persistent_state_enabled {
            with_preload_context(
                memory_hooks::save_phase(
                    &self.memory_root,
                    &self.workspace_root,
                    Arc::clone(&self.provider),
                    self.librarian_model(&orchestrator.spec().default_model),
                    SessionScope::Main,
                    task,
                    &main_session,
                    &decision,
                    &outcome,
                    &mut session_cache,
                    self.event_tx.clone(),
                )
                .await?,
                &main_bundle,
            )
        } else {
            diagnostic_memory_pass(SessionScope::Main, LibrarianPhase::Save)
        };
        librarian_passes.push(main_save);
        if let Some(root) = cache_root.as_ref() {
            session_cache.save(root)?;
        }
        if let Some(store) = session_store.as_mut() {
            store.upsert(main_session.clone());
            store.save_to_disk()?;
        }

        self.emit_librarian_passes(&librarian_passes);
        self.emit(CliEvent::FinalOutput(outcome.summary.clone()));
        self.emit(CliEvent::Done);

        Ok(RuntimeExecution {
            main_session_id,
            specialist_session_id,
            main_session_status,
            specialist_session_status,
            main_session_path,
            specialist_session_path,
            main_cache_status,
            specialist_cache_status,
            main_cache_path,
            specialist_cache_path,
            decision,
            main_bundle,
            specialist_bundle,
            librarian_passes,
            specialist_prompt,
            specialist_outcome,
            outcome,
            orchestrator_parse: ParseRecord::not_applicable(
                "orchestrator",
                "Scaffold first slice used deterministic Rust routing.",
            ),
            coder_parse: ParseRecord::parsed(
                "coder",
                "Scaffold provider returned valid coder JSON.",
            ),
        })
    }

    /// Run one user turn through Phoenix's supported actor mesh (gateway +
    /// per-agent canonical sessions). The older synchronous slice remains only
    /// as an internal compatibility harness for focused unit tests.
    pub async fn execute_mesh_slice(
        &self,
        orchestrator: &Orchestrator,
        task: &TaskEnvelope,
    ) -> Result<RuntimeExecution> {
        use crate::runtime::gateway::Gateway;
        use crate::runtime::mailbox::{AgentAddress, AgentMessage};
        use crate::runtime::mesh::MeshRunner;

        // Targeted turns — the user typed into a SPECIALIST's window prompt —
        // enter the mesh aimed at that agent (the kickoff below already maps
        // AgentTarget::Specialist → AgentAddress::Specialist). The old guard
        // here bailed on them, so every agent-window message died instantly
        // with a provider-flavored error ("this spark?" → "mesh slice expects
        // an orchestrator task", read by the user as a grok quota failure).
        let targeted_specialist = match &task.target_agent {
            AgentTarget::Orchestrator => None,
            AgentTarget::Specialist(st) => Some(*st),
        };

        let main_session_id = task.session_id.clone();
        let mut session_store = SessionStore::new(self.state_root.join("sessions"));
        if let Some(group) = &task.group {
            persist_group_user_boundary(
                &self.state_root, group, &task.id, &task.user_request,
                &orchestrator.spec().default_model,
            )?;
        }
        if task.group.is_some() {
            session_store.load_one_if_absent(&main_session_id)?;
        } else {
            session_store.load_from_disk()?;
        }
        let main_session_path = session_store.session_path(&main_session_id);
        let main_session_status = if main_session_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        self.emit(CliEvent::SessionResolved {
            scope: "main".to_string(),
            session_id: main_session_id.clone(),
            status: format!("{main_session_status:?} (mesh)"),
        });
        // Lib index-at-session-start (plan 017): a NEW session is exactly
        // when the previous session's still-pending saves are needed
        // searchable ("let's continue X" openers recall semantically).
        // Detached — cognify serializes against itself and is near-free on
        // an empty backlog; this turn's preload proceeds without waiting.
        if !cfg!(test) && matches!(main_session_status, PersistenceStatus::Created) {
            tokio::spawn(async {
                match crate::librarian::memory::cognify_backlog().await {
                    Ok(receipt) => {
                        tracing::info!("memory: session-start backlog cognify — {receipt}")
                    }
                    Err(error) => {
                        tracing::debug!("memory: session-start cognify deferred: {error:#}")
                    }
                }
            });
        }

        // Persist the just-sent user message to the durable session NOW — before
        // the librarian preload's model call (which can take seconds). The mesh
        // turn that normally records it runs only AFTER preload, so a user who
        // hit Esc during preload (an early cancel) used to lose what they already
        // typed — confirmed live: a message cancelled 2.7s in vanished while one
        // cancelled at 127s survived. A sent message must survive any stop;
        // stopping halts the agent's work, it does not unsay the user. The mesh's
        // `record_incoming` is idempotent on this tail message (see mesh.rs), so
        // it is not double-recorded.
        let mut main_session =
            if let (Some(specialist), Some(context)) = (targeted_specialist, task.agent.as_ref()) {
                let mut specialist_spec = crate::sub_agents::specialist_config(specialist).spec;
                if let Some(model) = &self.specialist_model {
                    specialist_spec.default_model = model.clone();
                }
                if let Some(model) = self
                    .agent_models
                    .get(crate::runtime::delegation::specialist_label(specialist))
                {
                    specialist_spec.default_model = model.clone();
                }
                anyhow::ensure!(
                    context.canonical_session_id == main_session_id,
                    "direct coworker context does not own session `{main_session_id}`"
                );
                session_store.load_or_create_specialist_with_id(
                    &main_session_id,
                    specialist,
                    &specialist_spec.default_model,
                    &specialist_spec.system_prompt,
                )?
            } else {
                session_store.load_or_create_main(
                    &main_session_id,
                    &orchestrator.spec().default_model,
                    &orchestrator.spec().system_prompt,
                )?
            };
        // A specialist-targeted turn belongs to THAT agent's transcript (the
        // mesh records it there) — writing it into the main session too would
        // pollute Phoenix's context with side-window chatter.
        if task.group.is_none() && (targeted_specialist.is_none() || task.agent.is_some()) {
            main_session.push_message(Message::User {
                content: task.user_request.clone(),
            });
            // Name the session from its first message so the global list reads
            // as a title, not a UUID (the OpenClaw-style session model).
            main_session.ensure_title(&task.user_request);
            session_store.upsert(main_session.clone());
            session_store.save_one(&main_session_id)?;
        }

        // Memory: librarian preload before the mesh run (orchestrator context),
        // save pass after — so every turn can recall and persist durable facts.
        // Escape hatch for token-constrained runs: PHOENIX_NO_LIBRARIAN=1.
        let librarian_enabled = std::env::var("PHOENIX_NO_LIBRARIAN")
            .map(|v| v.trim() != "1")
            .unwrap_or(true)
            // Room participants recall within their own actor lanes. A
            // separate main preload would pin private memory into the public
            // room and later overwrite contributions from a stale snapshot.
            && task.group.is_none()
            // Direct specialist chats are quick window exchanges — the main
            // session's memory preload/save ceremony doesn't apply to them.
            && (targeted_specialist.is_none() || task.agent.is_some())
            // Phatic turns ("thanks!", "ok cool") carry nothing to recall or
            // persist — skipping both librarian passes turns a 30-60s
            // pleasantry round-trip into just the orchestrator call.
            && !is_phatic_message(&task.user_request);
        // Recall is Cognee-only now. Do not gate it on the archived Markdown
        // file tier: a fresh installation can have a populated Cognee graph
        // and no `memory/**/*.md` files at all. The local search is bounded by
        // the timeout below and honestly reports cold/no-match/failure.
        let preload_enabled = librarian_enabled;
        let primary_memory_scope = targeted_specialist
            .map(SessionScope::Specialist)
            .unwrap_or(SessionScope::Main);
        let cache_root = self.state_root.join("session_cache");
        let mut session_cache = SessionCache::load_or_create(&cache_root, &main_session_id)?;
        let mut librarian_passes: Vec<LibrarianPassRecord> = Vec::new();
        let (main_bundle, loaded_main) = if preload_enabled {
            // The librarian sees the session as the orchestrator will: with this
            // user request already appended (persisted just above).
            // This is before the first provider call, so it gets an INTERACTION
            // budget rather than a background-job budget. Cognee's cold ONNX
            // initialization continues on the blocking pool after this future
            // is dropped; later turns and the explicit memory tool benefit from
            // the warmed singleton without making the current turn look dead.
            match tokio::time::timeout(
                std::time::Duration::from_millis(850),
                memory_hooks::preload(
                    &self.memory_root,
                    &self.workspace_root,
                    self.librarian_provider(),
                    self.librarian_model(&orchestrator.spec().default_model),
                    primary_memory_scope.clone(),
                    task,
                    &main_session,
                    &mut session_cache,
                    self.event_tx.clone(),
                ),
            )
            .await
            {
                Ok(result) => {
                    let (bundle, record, mut loaded) = result?;
                    self.emit_librarian_pass(&record);
                    librarian_passes.push(record);
                    // Pin the librarian's loaded memories into the durable
                    // transcript so the agent carries them across turns instead
                    // of the librarian re-reading the same files every message;
                    // refresh any whose source file changed. Persist so the mesh
                    // loads them and the next preload knows not to re-read them.
                    pin_loaded_memories_into_session(
                        &mut session_store,
                        &main_session_id,
                        &self.memory_root,
                        &mut loaded,
                        &mut session_cache,
                    );
                    let _ = session_store.save_one(&main_session_id);
                    let _ = session_cache.save(&cache_root);
                    (Some(bundle), Some(loaded))
                }
                Err(_) => {
                    self.emit(CliEvent::GatewayNotice(
                        "Memory lookup is taking longer than expected. Continuing with the saved conversation; additional memories may not be included in this reply."
                            .to_string(),
                    ));
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        let mut mesh = MeshRunner::new(
            Arc::clone(&self.provider),
            self.workspace_root.clone(),
            self.state_root.clone(),
            main_session_id.clone(),
            orchestrator.spec().clone(),
        )
        .with_specialist_model(self.specialist_model.clone())
        .with_reasoning_effort(self.reasoning_effort.clone())
        .with_agent_models(self.agent_models.clone())
        .with_role_efforts(self.role_efforts.clone())
        .with_role_context_windows(self.role_context_windows.clone())
        .with_vision(self.vision.clone())
        .with_native_vision(self.native_vision)
        .with_loaded_memories(loaded_main)
        .with_permission_mode(self.permission_mode)
        .with_interaction_mode(task.interaction_mode)
        .with_context_window(crate::config::resolve_context_window())
        // Compaction folds REWRITE an agent's working history — the summary
        // becomes ground truth the agent keeps acting on. That is
        // specialist-grade work, not throwaway librarian work: running it on
        // the cheapest model (librarian/deepseek-flash) let folds soften or
        // drop the user's own instructions ("discard previous, full redesign,
        // make the images green") — the "compaction made them dumb" failure.
        // Route folds through the specialist tier (glm-5.2) so continuity holds.
        .with_compaction_model(self.specialist_model.clone())
        .with_specialist_provider(self.specialist_provider.clone())
        .with_agent_providers(self.agent_providers.clone())
        .with_compaction_provider(self.specialist_provider.clone())
        .with_direct_agent_context(task.agent.clone())
        .with_design_reference_paths(design_reference_paths(&task.context)?)
        .with_group_context(task.group.clone())
        .with_group_authored_turn_id(task.group.as_ref().map(|_| task.id.as_str()));
        if let Some(tx) = &self.event_tx {
            mesh = mesh.with_event_channel(tx.clone());
        }
        let mut group_publisher=task.group.as_ref().map(|_|GroupContributionPublisher::new(self.event_tx.clone()));
        if let Some(queue)=group_publisher.as_ref().and_then(|publisher|publisher.queue.as_ref()) {
            mesh=mesh.with_group_status_channel(queue.clone());
        }

        let token_counter = mesh.token_counter();
        let peak_input_counter = mesh.peak_input_counter();
        let mut gateway = Gateway::new(mesh).with_durable_task(&main_session_id, &task.id)?;
        let accountable_owner = if task.group.is_none() {
            Some(match &task.target_agent {
                AgentTarget::Orchestrator => AgentAddress::Orchestrator,
                AgentTarget::Specialist(agent) => AgentAddress::Specialist(*agent),
            })
        } else {
            None
        };
        if task.group.is_none() {
            let mut authored_turn = AgentMessage::user_input(
                accountable_owner
                    .clone()
                    .expect("non-group turn has an accountable owner"),
                task.user_request.clone(),
            );
            // The client turn id is durable across daemon retry/replay. Carry
            // it into the mesh as causation (not a handoff lifecycle id) so
            // provider call ids are namespaced by the same producer boundary
            // after a crash without changing how the user reply is rendered.
            authored_turn.causation_id = Some(task.id.clone());
            gateway.submit(authored_turn);
        }
        let mut mesh_steps = 0;
        let mut final_user_messages = Vec::new();
        // In a room every contribution is emitted live as its own GroupMessage.
        // The turn summary is still assembled below (memory learns from it, and
        // it is the outcome's record), but re-emitting it as the final answer
        // would print each coworker's message on screen a second time.
        let mut group_messages_already_shown = false;
        let mut group_incomplete = false;
        if let Some(group) = &task.group {
            let dependencies = group.dependencies();
            for participant in group.explicitly_pinged() {
                let waits_for = dependencies
                    .iter()
                    .filter(|edge| edge.dependent == participant.agent_id)
                    .filter_map(|edge| {
                        group
                            .participants
                            .iter()
                            .find(|member| member.agent_id == edge.prerequisite)
                    })
                    .map(|member| member.display_name.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ");
                self.emit(CliEvent::GroupMemberStatus {
                    turn_id: task.id.clone(),
                    group_id: group.group_id.clone(),
                    agent_id: participant.agent_id.clone(),
                    agent_name: participant.display_name.clone(),
                    state: "queued".to_string(),
                    detail: if waits_for.is_empty() {
                        "Ready for execution".to_string()
                    } else {
                        format!("Waiting for {waits_for}")
                    },
                });
            }
            {
                let mut activations = Vec::new();
                let publisher = group_publisher.take().expect("group publisher initialized");
                let mut pending_messages = Vec::new();
                let mut completed = std::collections::HashSet::new();
                let mut submitted = std::collections::HashSet::new();
                let mut persisted = Vec::<(String, String)>::new();
                let mut recovered_failures = Vec::new();
                let company = crate::runtime::company::global()?;
                reserve_saved_group_pings(&self.state_root, company.as_ref(), group, &task.id)?;
                let ledger = company.group_turn(&group.canonical_session_id, &task.id)?
                    .context("room turn has no activation ledger")?;
                for participant in group.participants.iter().filter(|participant|
                    ledger.members.iter().any(|member| member.participant.agent_id == participant.agent_id)) {
                    let mut activation =
                        GroupActivationWorkGuard::arm(group, &task.id, &participant.agent_id);
                    if let Some((message_id, committed_event)) = persisted_group_contribution(
                        &self.state_root,
                        group,
                        &task.id,
                        &participant.agent_id,
                    )? {
                        // Recover the contribution without mistaking a saved
                        // waiting message (or transferred producer) for a result
                        // that can unlock its dependents.
                        let pending_ask = pending_group_ask_ids(&group.canonical_session_id, &task.id, &participant.agent_id)?.into_iter().next();
                        let (retained, failed) = settle_saved_group_contribution(
                            &self.state_root, crate::runtime::company::global()?.as_ref(), group,
                            &task.id, &participant.agent_id, &message_id, pending_ask.as_deref(),
                        )?;
                        group_incomplete |= failed;
                        if retained && failed { recovered_failures.push(participant.agent_id.clone()); }
                        activation.settled = true;
                        if retained && !failed && pending_ask.is_none() {
                            completed.insert(participant.agent_id.clone());
                            persisted.push((participant.agent_id.clone(), message_id.clone()));
                        }
                        // A prior process may have saved this contribution
                        // before publishing it. Replay its exact stable ID and
                        // body; consumers deduplicate by message ID. Recovery
                        // never generates a replacement answer.
                        group_messages_already_shown = true;
                        publisher.publish_pair(committed_event, CliEvent::GroupMemberStatus {
                            turn_id: task.id.clone(),
                            group_id: group.group_id.clone(),
                            agent_id: participant.agent_id.clone(),
                            agent_name: participant.display_name.clone(),
                            state: if !retained { "continued" } else if pending_ask.is_some() { "waiting_user" } else if failed { "blocked" } else { "done" }.to_string(),
                            detail: if !retained { "Recovered the saved contribution; its answer continuation owns the remaining work" } else if pending_ask.is_some() { "Recovered the saved contribution; its question is still pending" } else if failed { "Partial evidence saved; this task did not finish" } else { "Recovered the already persisted contribution" }.to_string(),
                        })?;
                        continue;
                    }
                    let member = ledger.members.iter().find(|member|
                        member.participant.agent_id == participant.agent_id).context("room activation missing")?;
                    let activation_message = if member.source_receipt_id.is_some() {
                        saved_group_ping_message(&self.state_root, group, &task.id, member)?
                    } else {
                        let mut message = AgentMessage::user_input(group_participant_address(participant)?, task.user_request.clone());
                        message.handoff_id = task.id.clone();
                        message.causation_id = Some(task.id.clone());
                        message
                    };
                    pending_messages.push((participant.agent_id.clone(), activation_message));
                    activations.push((participant, activation));
                }
                let mut rejected_dispatches = std::collections::HashSet::new();
                // Recovered results predate dispatch. A descendant of a newly
                // rejected input cannot complete in this run: its prerequisite
                // was never admitted. Preserve recovered results as barriers.
                let recovered_results = completed.clone();
                let mut reject_dispatch = |agent_id: &str, reason: &str| -> Result<()> {
                    let owner = group.participants.iter().find(|member| member.agent_id == agent_id)
                        .context("rejected group assignment has no participant")?.display_name.clone();
                    reject_group_input_branch(agent_id, &dependencies, &recovered_results, |affected| {
                        if rejected_dispatches.contains(affected) { return Ok(false); }
                        let detail = if affected == agent_id { reason.to_string() } else {
                            format!("Cannot start: {owner}'s required saved input is unavailable. Repair that prerequisite before resuming this branch.")
                        };
                        let retained = crate::runtime::company::global()?.reject_group_member_input(
                            &group.canonical_session_id, &task.id, affected, &detail,
                        )?;
                        rejected_dispatches.insert(affected.to_string());
                        let participant = group.participants.iter().find(|member| member.agent_id == affected)
                            .context("rejected group descendant has no participant")?;
                        self.emit(CliEvent::GroupMemberStatus {
                            turn_id: task.id.clone(), group_id: group.group_id.clone(),
                            agent_id: affected.to_string(), agent_name: participant.display_name.clone(),
                            state: if retained { "blocked" } else { "continued" }.to_string(),
                            detail: if retained { detail } else { "An answer continuation already owns this assignment".to_string() },
                        });
                        Ok(retained)
                    })
                };
                for failed_agent in &recovered_failures {
                    for edge in dependencies.iter().filter(|edge| &edge.prerequisite == failed_agent) {
                        reject_dispatch(&edge.dependent, "Required coworker task did not finish; its saved partial result is not a completed prerequisite")?;
                    }
                }
                for message in prepare_group_dispatch_isolated(take_ready_group_messages(
                    &mut pending_messages,
                    &dependencies,
                    &completed,
                    &mut submitted,
                ), group, &dependencies, &persisted, &self.state_root, &mut reject_dispatch)? {
                    gateway.submit(message);
                }

                let mut publish = |message: &AgentMessage,
                                   release: bool|
                 -> Result<Vec<AgentMessage>> {
                    let role = message.from.label();
                    let Some(speaker) = group_participant_for_role(group, &role) else {
                        return Ok(Vec::new());
                    };
                    if persisted
                        .iter()
                        .any(|(agent_id, _)| agent_id == &speaker.agent_id)
                    {
                        return Ok(Vec::new());
                    }
                    // Commit before emitting; an unrelated slow coworker must
                    // not hide this completed contribution from the room.
                    let (receipts, mut committed_events) = persist_group_messages_with_events(
                        &self.state_root,
                        group,
                        &task.id,
                        std::slice::from_ref(message),
                    )?;
                    let (_, message_id) = receipts
                        .first()
                        .context("group contribution has no durable receipt")?;
                    let committed_event = committed_events.pop().context("committed group contribution has no event")?;
                    let (new_pending_ask, retained_ownership, failed) = activations
                        .iter_mut()
                        .find(|(member, _)| member.agent_id == speaker.agent_id)
                        .map(|(_, activation)| -> Result<(Option<String>,bool,bool)> {
                            let pending =
                            pending_group_ask_ids(&group.canonical_session_id, &task.id, &speaker.agent_id)?
                                .into_iter()
                                .next();
                            let (retained, failed) = settle_saved_group_contribution(
                                &self.state_root, crate::runtime::company::global()?.as_ref(), group,
                                &task.id, &speaker.agent_id, message_id, pending.as_deref(),
                            )?;
                            activation.settled = true;
                            Ok((pending,retained,failed))
                        }).transpose()?.unwrap_or((None,false,false));
                    group_incomplete |= failed;
                    if retained_ownership && failed {
                        for edge in dependencies.iter().filter(|edge| edge.prerequisite == speaker.agent_id) {
                            reject_dispatch(&edge.dependent, "Required coworker task did not finish; its saved partial result is not a completed prerequisite")?;
                        }
                    }
                    // Match recovery ordering: do not advertise a contribution
                    // while its task settlement can still fail. The canonical
                    // saved reply remains available for replay after an error.
                    // Completion is a per-result transition, not a whole-run
                    // barrier. Keep the durable ledger and visible state in
                    // step with the contribution even while peers are running.
                    publisher.publish_pair(committed_event, CliEvent::GroupMemberStatus {
                        turn_id: task.id.clone(),
                        group_id: group.group_id.clone(),
                        agent_id: speaker.agent_id.clone(),
                        agent_name: speaker.display_name.clone(),
                        state: if !retained_ownership { "continued" } else if new_pending_ask.is_some() { "waiting_user" } else if failed { "blocked" } else { "done" }.to_string(),
                        detail: if !retained_ownership { "Answer received; this task continues in its queued successor" } else if new_pending_ask.is_some() { "Waiting for your answer; other independent coworkers can continue" } else if failed { "Partial evidence saved; this task did not finish" } else { "Contribution saved" }.to_string(),
                    })?;
                    persisted.extend(receipts);
                    group_messages_already_shown = true;
                    let new_pings = reserve_saved_group_pings(&self.state_root,
                        crate::runtime::company::global()?.as_ref(), group, &task.id)?;
                    if !new_pings.is_empty() {
                        let ledger = crate::runtime::company::global()?.group_turn(&group.canonical_session_id, &task.id)?
                            .context("room ping ledger disappeared")?;
                        for id in new_pings {
                            let target = group.participants.iter().find(|member| member.agent_id == id)
                                .context("room ping target disappeared")?;
                            let member = ledger.members.iter().find(|member| member.participant.agent_id == id)
                                .context("room ping activation disappeared")?;
                            pending_messages.push((id.clone(), saved_group_ping_message(&self.state_root, group, &task.id, member)?));
                            activations.push((target, GroupActivationWorkGuard::arm(group, &task.id, &id)));
                            self.emit(CliEvent::GroupMemberStatus {
                                turn_id: task.id.clone(), group_id: group.group_id.clone(),
                                agent_id: id, agent_name: target.display_name.clone(), state: "queued".into(),
                                detail: format!("Mentioned by {}", speaker.display_name),
                            });
                        }
                    }
                    if release && retained_ownership && !failed && new_pending_ask.is_none() {
                        completed.insert(speaker.agent_id.clone());
                    }
                    if release {
                        prepare_group_dispatch_isolated(take_ready_group_messages(
                            &mut pending_messages,
                            &dependencies,
                            &completed,
                            &mut submitted,
                        ), group, &dependencies, &persisted, &self.state_root, &mut reject_dispatch)
                    } else {
                        Ok(Vec::new())
                    }
                };
                let outcome = gateway
                    .run_dispatching(|message| publish(message, true))
                    .await?;
                mesh_steps += outcome.steps;
                let visible_messages = outcome
                    .user_messages
                    .into_iter()
                    .filter(|message| {
                        group_participant_for_role(group, &message.from.label()).is_some()
                    })
                    .collect::<Vec<_>>();
                // Runtime-generated terminal diagnostics are collected outside
                // the actor boundary. Publish them too; receipts deduplicate
                // contributions that were already delivered by the observer.
                for message in &visible_messages {
                    publish(message, false)?;
                }
                final_user_messages.extend(visible_messages);
                publisher.finish().await?;
                for (participant, mut activation) in activations {
                    if rejected_dispatches.contains(&participant.agent_id) {
                        // Already durably blocked before the healthy frontier ran.
                        activation.settled = true;
                        continue;
                    }
                    if activation.settled {
                        continue;
                    }
                    if !submitted.contains(&participant.agent_id) {
                        // Still waiting on a prerequisite. Preserve queued state
                        // for durable ask/recovery continuation, not false failure.
                        activation.settled = true;
                        continue;
                    }
                    let new_pending_ask =
                        pending_group_ask_ids(&group.canonical_session_id, &task.id, &participant.agent_id)?
                            .into_iter()
                            .next();
                    if let Some(ask_id) = new_pending_ask {
                        activation.waiting_user(&ask_id)?;
                        self.emit(CliEvent::GroupMemberStatus {
                            turn_id: task.id.clone(),
                            group_id: group.group_id.clone(),
                            agent_id: participant.agent_id.clone(),
                            agent_name: participant.display_name.clone(),
                            state: "waiting_user".to_string(),
                            detail:
                                "Waiting for your answer; other independent coworkers can continue"
                                    .to_string(),
                        });
                    } else if let Some((_, message_id)) = persisted
                        .iter()
                        .rev()
                        .find(|(agent_id, _)| agent_id == &participant.agent_id)
                    {
                        // Done is recorded only after the canonical session
                        // write above yielded this immutable message id.
                        activation.done(message_id)?;
                        self.emit(CliEvent::GroupMemberStatus {
                            turn_id: task.id.clone(),
                            group_id: group.group_id.clone(),
                            agent_id: participant.agent_id.clone(),
                            agent_name: participant.display_name.clone(),
                            state: "done".to_string(),
                            detail: "Contribution saved".to_string(),
                        });
                    } else {
                        activation
                            .blocked("Coworker went idle without a persisted group contribution")?;
                        self.emit(CliEvent::GroupMemberStatus {
                            turn_id: task.id.clone(),
                            group_id: group.group_id.clone(),
                            agent_id: participant.agent_id.clone(),
                            agent_name: participant.display_name.clone(),
                            state: "blocked".to_string(),
                            detail: "Stopped without a persisted contribution".to_string(),
                        });
                    }
                }
            }
        } else if let Some(owner) = accountable_owner.as_ref() {
            let first_outcome = gateway.run().await;
            mesh_steps += first_outcome.steps;
            final_user_messages = first_outcome.user_messages;
            // An individual conversation has exactly one accountable voice:
            // the coworker the user addressed. A reply-expected delegation
            // naturally comes back through that owner. Fire-and-forget work,
            // recovery messages, and proactive helpers can still reach the
            // user boundary independently. If the owner has not answered,
            // let it integrate the evidence. Once it has answered, save late
            // evidence for its next turn without replacing the completed reply.
            // A helper never becomes the voice of somebody else's thread.
            let (mut owner_messages, mut helper_messages) =
                split_user_messages_by_owner(final_user_messages, owner);
            for integration_round in 1..=3 {
                if helper_messages.is_empty() {
                    break;
                }
                if !owner_messages.is_empty() {
                    // Required mode-1 work returns before an owner may finish.
                    // Independent late work must not replace that final answer.
                    record_late_helper_results(&self.state_root, &main_session_id,
                        &task.id, owner, &helper_messages)?;
                    helper_messages.clear();
                    break;
                }
                for message in helper_messages.drain(..) {
                    gateway.submit(AgentMessage::talk(
                        message.from,
                        owner.clone(),
                        format!("Internal company result · {}", message.subject),
                        format!(
                            "A coworker returned this internal result while you remain accountable to the user. Integrate it with your own work. Your reply goes to the USER, never to the coworker: no \"You're right\", \"thanks\" or instructions to them. Give one complete answer to the current user request using the relevant evidence. Keep results about earlier tasks separate from the current request.\n\n{}",
                            message.body
                        ),
                        false,
                    ));
                }
                let integrated = gateway.run().await;
                mesh_steps += integrated.steps;
                let (next_owner, next_helpers) =
                    split_user_messages_by_owner(integrated.user_messages, owner);
                if !next_owner.is_empty() {
                    owner_messages = next_owner;
                }
                helper_messages = next_helpers;
                if integration_round == 3 && !helper_messages.is_empty() {
                    self.emit(CliEvent::GatewayNotice(format!(
                        "{} helper result(s) stayed internal after three owner-integration passes",
                        helper_messages.len()
                    )));
                }
            }
            final_user_messages = owner_messages;
        }
        let (mesh_input_tokens, mesh_output_tokens) = (
            token_counter.0.load(std::sync::atomic::Ordering::Relaxed),
            token_counter.1.load(std::sync::atomic::Ordering::Relaxed),
        );
        // Peak single-call input (context-window usage), separate from the sum.
        let mesh_peak_input = peak_input_counter.load(std::sync::atomic::Ordering::Relaxed);

        // Surface every user-addressed message in arrival order; messages from
        // specialists are attributed so the user sees who is talking.
        let mut sections: Vec<String> = Vec::new();
        if let Some(group) = &task.group {
            for message in &final_user_messages {
                let role = message.from.label();
                let name = group
                    .participants
                    .iter()
                    .find(|participant| participant.internal_role == role)
                    .map(|participant| participant.display_name.as_str())
                    .unwrap_or(role.as_str());
                sections.push(format!("**{name}**\n\n{}", message.body));
            }
        } else {
            for message in &final_user_messages {
                if message.from == AgentAddress::Orchestrator {
                    sections.push(message.body.clone());
                } else {
                    sections.push(format!("**[{}]** {}", message.from.label(), message.body));
                }
            }
        }
        let mut final_markdown = sections.join("\n\n---\n\n");
        if final_markdown.trim().is_empty() && task.group.is_none() {
            final_markdown =
                "## Result\nThe mesh went idle without producing a user-facing message."
                    .to_string();
        }
        // Lib beat (plan 017): the completion gets the same detached librarian
        // look as every specialist return — durable learnings saved and
        // indexed now, not at the next 12h tick. Attributed to whoever the
        // user actually addressed.
        let group_learning_enabled = task.group.as_ref().map_or(true, |group| {
            crate::settings::effective_bool(
                "memory.group_transcript_learning",
                &crate::settings::SettingsScope::Group {
                    id: group.group_id.clone(),
                },
            )
            .unwrap_or(true)
        });
        if group_learning_enabled && !final_markdown.trim().is_empty() {
            crate::runtime::memory_beat::on_completion(
                task.group
                    .as_ref()
                    .map(|group| group.group_id.as_str())
                    .or_else(|| task.agent.as_ref().map(|agent| agent.agent_id.as_str()))
                    .or_else(|| {
                        targeted_specialist.map(crate::runtime::delegation::specialist_label)
                    })
                    .unwrap_or("phoenix"),
                &task.user_request,
                &final_markdown,
            );
        }
        let outcome = AgentOutcome {
            completion: if group_incomplete { crate::runtime::OutcomeCompletion::Incomplete }
                else { crate::runtime::mailbox::AgentMessage::outcome_completion(&final_user_messages) },
            agent: task.target_agent.clone(),
            summary: final_markdown.clone(),
            artifacts: vec![],
            tool_results: vec![],
            // Carries the run-wide token totals so the CLI footer and gateway
            // log report real numbers instead of "0 tokens" on mesh turns.
            provider_response: Some(ProviderTurn {
                request_model: orchestrator.spec().default_model.clone(),
                output_text: String::new(),
                input_tokens: mesh_input_tokens,
                output_tokens: mesh_output_tokens,
                peak_input_tokens: mesh_peak_input,
            }),
        };
        let decision = OrchestratorDecision {
            mode: DelegationMode::StayLocal,
            target: task.target_agent.clone(),
            rationale: format!(
                "Mesh gateway run: {} agent turn(s), {} final user message(s).",
                mesh_steps,
                final_user_messages.len()
            ),
        };

        // A room has already shown every coworker's message as it landed, so
        // the assembled summary would be a verbatim second copy of the whole
        // exchange — that is why one coworker appeared to reply twice with the
        // exact same text. Individual conversations still answer here, where
        // the summary is the only user-facing surface.
        if should_emit_final_answer(&final_markdown, group_messages_already_shown) {
            self.emit(CliEvent::FinalOutput(final_markdown.clone()));
        }

        // DETACHED save pass: the answer is already on screen — persisting
        // memory must not hold the turn hostage (observed: 126s of librarian
        // save calls AFTER a finished answer, while the spinner kept running).
        // The background task owns clones of everything it needs; the session
        // cache write is last-write-wins and the next turn's preload simply
        // reads whatever has landed on disk by then (memory is eventually
        // consistent across turns, never blocking).
        if librarian_enabled {
            let memory_root = self.memory_root.clone();
            let workspace_root = self.workspace_root.clone();
            let provider = self.librarian_provider();
            let model = self
                .librarian_model(&orchestrator.spec().default_model)
                .to_string();
            let state_root = self.state_root.clone();
            let save_task = task.clone();
            let save_decision = decision.clone();
            let save_outcome = outcome.clone();
            let mut save_cache = session_cache;
            let save_cache_root = cache_root.clone();
            let event_tx = self.event_tx.clone();
            let save_session_id = main_session_id.clone();
            let save_scope = primary_memory_scope.clone();
            tokio::spawn(async move {
                let mut post_store = SessionStore::new(state_root.join("sessions"));
                if let Err(error) = post_store.load_from_disk() {
                    tracing::warn!("detached save: session load failed: {error:#}");
                    return;
                }
                let Some(post_session) = post_store.get(&save_session_id).cloned() else {
                    tracing::warn!(
                        "detached save: canonical session `{save_session_id}` disappeared"
                    );
                    return;
                };
                let notify = |text: String| {
                    if let Some(tx) = &event_tx {
                        let _ = tx.try_send(CliEvent::GatewayNotice(text));
                    }
                };
                match tokio::time::timeout(
                    std::time::Duration::from_secs(240),
                    memory_hooks::save_phase(
                        &memory_root,
                        &workspace_root,
                        provider,
                        &model,
                        save_scope,
                        &save_task,
                        &post_session,
                        &save_decision,
                        &save_outcome,
                        &mut save_cache,
                        event_tx.clone(),
                    ),
                )
                .await
                {
                    Ok(Ok(record)) => {
                        if !record.saved_memory_paths.is_empty() {
                            notify(format!(
                                "librarian saved (background): {}",
                                record.saved_memory_paths.join(", ")
                            ));
                        }
                    }
                    Ok(Err(error)) => notify(format!(
                        "librarian save failed (turn unaffected): {error:#}"
                    )),
                    Err(_) => {
                        notify("librarian save exceeded 240s — nothing saved this turn".to_string())
                    }
                }
                if let Err(error) = save_cache.save(&save_cache_root) {
                    tracing::warn!("detached save: cache write failed: {error:#}");
                }
            });
        }

        self.emit(CliEvent::Done);

        let specialist_session_id =
            crate::session::specialist_session_id(&main_session_id, SubAgentType::Coder);
        let specialist_session_path = session_store.session_path(&specialist_session_id);
        let empty_bundle = |scope: SessionScope| MemoryBundle {
            session_scope: scope,
            task_frame: task.title.clone(),
            loaded_memory_paths: vec![],
            loaded_knowledge_paths: vec![],
            ranked_context_items: vec![],
            omitted_items: vec![],
            grounding_receipts: vec![],
            completion_state: "mesh-v1: librarian passes skipped".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: None,
            context_budget_used: 0,
            summary: "Mesh run: librarian preload/save skipped.".to_string(),
        };
        Ok(RuntimeExecution {
            main_session_id: main_session_id.clone(),
            specialist_session_id: specialist_session_id.clone(),
            main_session_status,
            specialist_session_status: if specialist_session_path.exists() {
                PersistenceStatus::Resumed
            } else {
                PersistenceStatus::Created
            },
            main_session_path,
            specialist_session_path,
            main_cache_status: PersistenceStatus::Created,
            specialist_cache_status: PersistenceStatus::Created,
            main_cache_path: SessionCache::path_for(&cache_root, &main_session_id),
            specialist_cache_path: SessionCache::path_for(&cache_root, &specialist_session_id),
            decision,
            main_bundle: main_bundle.unwrap_or_else(|| empty_bundle(primary_memory_scope)),
            specialist_bundle: empty_bundle(SessionScope::Specialist(SubAgentType::Coder)),
            librarian_passes,
            specialist_prompt: PromptAssembly {
                system_prompt: String::new(),
                user_prompt: "Mesh run: specialists own their sessions.".to_string(),
            tail: String::new(),
            },
            specialist_outcome: AgentOutcome {
                completion: crate::runtime::OutcomeCompletion::Unknown,
                agent: task.target_agent.clone(),
                summary: String::new(),
                artifacts: vec![],
                tool_results: vec![],
                provider_response: None,
            },
            outcome,
            orchestrator_parse: ParseRecord::parsed(
                "orchestrator",
                format!("Mesh gateway completed in {} agent turn(s).", mesh_steps),
            ),
            coder_parse: ParseRecord::not_applicable(
                "coder",
                "Mesh run: specialist turns report through the gateway.",
            ),
        })
    }
}

#[cfg(test)]
mod accountability_tests {
    use super::*;

    #[test]
    fn failed_group_contribution_never_unlocks_dependents_after_reopen() {
        use crate::runtime::company::CompanyStore;
        use crate::runtime::group_conversation::{GroupActivationIntent, GroupDependency, GroupParticipant, GroupTurnContext, GroupMemberActivationState as State};
        for scenario in ["live", "restart", "legacy_done"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("company.sqlite");
            let mut company = CompanyStore::open(&path).unwrap();
            let participants = [("theo", "researcher"), ("iris", "frontend"), ("leo", "coder")].map(|(id, role)| GroupParticipant {
                agent_id:id.into(), internal_role:role.into(), display_name:id.into(), role_title:String::new(),
                color:String::new(), icon_seed:id.into(), avatar:None, member_role:"member".into(),
                history_access:crate::runtime::company_directory::HistoryAccess::Full,
                history_start_message_index:0, explicitly_mentioned:true,
            });
            let edges = vec![GroupDependency { prerequisite:"theo".into(), dependent:"iris".into() }];
            let group = GroupTurnContext {
                tool_constraints:Default::default(), inspection_participants:Default::default(),
                group_id:"failure-room".into(), group_name:"Failure room".into(),
                canonical_session_id:"group-failure-room".into(), participants:participants.to_vec(),
                discussion_rounds:1, read_full_transcript:false,
                execution_waves:vec![vec!["theo".into(), "leo".into()], vec!["iris".into()]],
                execution_dependencies:Some(edges.clone()),
            };
            let intent: GroupActivationIntent = serde_json::from_value(serde_json::json!({
                "group_id":group.group_id, "roster_fingerprint":"fixture-roster", "active_agent_ids":["theo","iris","leo"],
                "execution_mode":"ordered", "execution_waves":group.execution_waves, "execution_dependencies":edges,
            })).unwrap();
            persist_group_user_boundary(directory.path(), &group, "failure-turn", "research then design", "test").unwrap();
            company.reserve_group_turn(&group.canonical_session_id, "failure-turn", "research then design", &intent, &participants).unwrap();
            for id in ["theo", "leo"] { company.mark_group_member_working(&group.canonical_session_id, "failure-turn", id).unwrap(); }
            let messages = [
                AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Researcher), AgentAddress::User,
                    "researcher turn failed: provider unavailable", "Useful partial evidence; investigation unfinished.", false),
                AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Coder), AgentAddress::User,
                    "Independent result", "Independent audit completed.", false),
            ];
            let receipts = persist_group_messages(directory.path(), &group, "failure-turn", &messages).unwrap();
            let canonical = directory.path().join("sessions").join(format!("{}.json", group.canonical_session_id));
            let before = std::fs::read(&canonical).unwrap();
            if scenario == "legacy_done" {
                company.mark_group_member_done(&group.canonical_session_id, "failure-turn", "theo", &receipts[0].1).unwrap();
            }
            if scenario == "restart" {
                drop(company); company = CompanyStore::open(&path).unwrap();
                assert_eq!(company.recover_stale_group_activations().unwrap(), 2);
            }
            for _ in 0..2 {
                let (retained, failed) = settle_saved_group_contribution(directory.path(), &company, &group,
                    "failure-turn", "theo", &receipts[0].1, None).unwrap();
                assert!(retained && failed, "persisting partial evidence must retain failure state");
                let (retained, failed) = settle_saved_group_contribution(directory.path(), &company, &group,
                    "failure-turn", "leo", &receipts[1].1, None).unwrap();
                assert!(retained && !failed);
                assert!(company.mark_group_member_working(&group.canonical_session_id, "failure-turn", "iris").is_err(),
                    "a failed prerequisite must not admit its dependent");
                let record = company.group_turn(&group.canonical_session_id, "failure-turn").unwrap().unwrap();
                assert_eq!(record.members[0].state, State::Blocked);
                assert_eq!(record.members[0].receipt_id.as_deref(), Some(receipts[0].1.as_str()));
                assert_eq!(record.members[2].state, State::Done);
                assert!(!record.is_done());
                assert!(company.pending_group_continuations().unwrap().is_empty());
                let dependent = AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Frontend), "continue design");
                let error = prepare_group_dispatch(vec![dependent], &group, &edges, &receipts, directory.path())
                    .expect_err("a stored failed receipt must not enter a prerequisite packet even if an old frontier admitted it");
                assert!(error.to_string().contains("unfinished work"));
                assert!(settle_saved_group_contribution(directory.path(), &company, &group,
                    "failure-turn", "theo", &receipts[1].1, None).is_err(), "another coworker's receipt cannot settle this assignment");
                drop(company); company = CompanyStore::open(&path).unwrap();
            }
            assert_eq!(std::fs::read(canonical).unwrap(), before, "recovery preserves exact saved evidence");
        }
    }

    #[test]
    fn group_input_failure_blocks_descendants_not_independent_or_completed_work() {
        use crate::runtime::group_conversation::GroupDependency;
        let edges = [("theo","iris"),("iris","review"),("leo","review")]
            .into_iter().map(|(a,b)| GroupDependency { prerequisite:a.into(), dependent:b.into() }).collect::<Vec<_>>();
        let mut rejected = Vec::new();
        reject_group_input_branch("theo", &edges, &Default::default(), |id| {
            rejected.push(id.to_string()); Ok(true)
        }).unwrap();
        assert_eq!(rejected, ["theo", "iris", "review"]);
        rejected.clear();
        reject_group_input_branch("theo", &edges, &["iris".into()].into_iter().collect(), |id| {
            rejected.push(id.to_string()); Ok(true)
        }).unwrap();
        assert_eq!(rejected, ["theo"], "existing results fence their descendants");
        rejected.clear();
        reject_group_input_branch("theo", &edges, &Default::default(), |id| {
            rejected.push(id.to_string()); Ok(id != "iris")
        }).unwrap();
        assert_eq!(rejected, ["theo", "iris"], "the old run must not block a successor's descendants");
        let error = reject_group_input_branch("theo", &edges, &Default::default(), |_| {
            anyhow::bail!("durable rejection failed")
        }).unwrap_err();
        assert!(error.to_string().contains("durable rejection failed"));
    }

    #[tokio::test]
    async fn group_rejection_reaches_descendants_before_independent_completion() {
        use crate::runtime::gateway::{AgentTurnHandler, Gateway};
        use crate::runtime::group_conversation::GroupDependency;
        use crate::runtime::mailbox::AgentAddress;
        let released = std::sync::Arc::new(tokio::sync::Notify::new());
        struct Handler(std::sync::Arc<tokio::sync::Notify>);
        #[async_trait::async_trait]
        impl AgentTurnHandler for Handler {
            async fn run_turn(&self, addr: &AgentAddress, _: AgentMessage) -> Vec<AgentMessage> {
                if matches!(addr, AgentAddress::Specialist(SubAgentType::Coder)) {
                    // A post-drain failure update deadlocks this deliberately
                    // interlocked scenario: Leo cannot finish before the UI's
                    // blocked branch has been settled.
                    self.0.notified().await;
                }
                vec![AgentMessage::talk(addr.clone(), AgentAddress::User, "result", addr.label(), false)]
            }
        }
        let edges = [("iris", "review"), ("review", "publish")].into_iter()
            .map(|(a,b)| GroupDependency { prerequisite:a.into(), dependent:b.into() }).collect::<Vec<_>>();
        let mut before = Gateway::new(Handler(std::sync::Arc::new(tokio::sync::Notify::new())));
        before.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Coder), "independent work"));
        before.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Researcher), "prerequisite"));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(25),
            before.run_dispatching(|_| Ok(Vec::new()))).await.is_err(),
            "before: settling descendants only after gateway drain cannot release this run");
        let mut gateway = Gateway::new(Handler(released.clone()));
        gateway.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Coder), "independent work"));
        gateway.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Researcher), "prerequisite"));
        let mut blocked = Vec::new();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), gateway.run_dispatching(|message| {
            if matches!(message.from, AgentAddress::Specialist(SubAgentType::Researcher)) {
                reject_group_input_branch("iris", &edges, &Default::default(), |id| {
                    blocked.push(id.to_string()); Ok(true)
                })?;
                assert_eq!(blocked, ["iris", "review", "publish"]);
                released.notify_one();
            } else {
                assert_eq!(blocked.len(), 3, "independent completion must not precede branch settlement");
            }
            Ok(Vec::new())
        })).await.expect("failure settlement must not wait for unrelated work").unwrap();
        assert_eq!(outcome.user_messages.len(), 2);
    }

    #[tokio::test]
    async fn group_dependency_frontier_releases_iris_while_leo_is_running() {
        use crate::runtime::gateway::{AgentTurnHandler, Gateway};
        use crate::runtime::group_conversation::GroupDependency;
        use crate::runtime::mailbox::AgentAddress;
        struct Handler(tokio::sync::Notify);
        #[async_trait::async_trait]
        impl AgentTurnHandler for Handler {
            async fn run_turn(&self, addr: &AgentAddress, _: AgentMessage) -> Vec<AgentMessage> {
                let body = match addr {
                    AgentAddress::Specialist(SubAgentType::Coder) => {
                        self.0.notified().await;
                        "leo"
                    }
                    AgentAddress::Specialist(SubAgentType::Researcher) => "theo",
                    _ => {
                        self.0.notify_one();
                        "iris"
                    }
                };
                vec![AgentMessage::talk(
                    addr.clone(),
                    AgentAddress::User,
                    "result",
                    body,
                    false,
                )]
            }
        }
        let mut pending = vec![
            (
                "theo".into(),
                AgentMessage::user_input(
                    AgentAddress::Specialist(SubAgentType::Researcher),
                    "research",
                ),
            ),
            (
                "leo".into(),
                AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Coder), "backend"),
            ),
            (
                "iris".into(),
                AgentMessage::user_input(AgentAddress::Orchestrator, "design"),
            ),
        ];
        let edges = vec![GroupDependency {
            prerequisite: "theo".into(),
            dependent: "iris".into(),
        }];
        let mut completed = std::collections::HashSet::new();
        let mut submitted = std::collections::HashSet::new();
        let mut gateway = Gateway::new(Handler(tokio::sync::Notify::new()));
        for message in take_ready_group_messages(&mut pending, &edges, &completed, &mut submitted) {
            gateway.submit(message);
        }
        assert_eq!(pending.len(), 1);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gateway.run_dispatching(|message| {
                completed.insert(message.body.clone());
                Ok(take_ready_group_messages(
                    &mut pending,
                    &edges,
                    &completed,
                    &mut submitted,
                ))
            }),
        )
        .await
        .expect("Iris must not wait for Leo")
        .unwrap();
        assert_eq!(
            result
                .user_messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            vec!["theo", "iris", "leo"]
        );
        assert!(pending.is_empty());
        assert_eq!(submitted.len(), 3);
    }

    #[test]
    fn group_waiting_prerequisite_does_not_release_dependent_or_duplicate_dispatch() {
        use crate::runtime::group_conversation::GroupDependency;
        use crate::runtime::mailbox::AgentAddress;
        let mut pending = vec![(
            "iris".into(),
            AgentMessage::user_input(AgentAddress::Orchestrator, "design"),
        )];
        let edges = vec![GroupDependency {
            prerequisite: "theo".into(),
            dependent: "iris".into(),
        }];
        let mut completed = std::collections::HashSet::from(["leo".to_string()]);
        let mut submitted = std::collections::HashSet::new();
        assert!(
            take_ready_group_messages(&mut pending, &edges, &completed, &mut submitted).is_empty()
        );
        assert_eq!(pending.len(), 1);
        completed.insert("theo".into());
        assert_eq!(
            take_ready_group_messages(&mut pending, &edges, &completed, &mut submitted).len(),
            1
        );
        assert!(
            take_ready_group_messages(&mut pending, &edges, &completed, &mut submitted).is_empty()
        );
    }
    use crate::runtime::mailbox::{AgentAddress, AgentMessage};
    use crate::session::{Session, SubAgentType};

    #[test]
    fn individual_thread_keeps_only_the_addressed_coworker_user_facing() {
        let owner = AgentAddress::Specialist(SubAgentType::Coder);
        let messages = vec![
            AgentMessage::talk(
                AgentAddress::Specialist(SubAgentType::Researcher),
                AgentAddress::User,
                "research",
                "internal evidence",
                false,
            ),
            AgentMessage::talk(
                owner.clone(),
                AgentAddress::User,
                "answer",
                "Leo's integrated answer",
                false,
            ),
            AgentMessage::talk(
                AgentAddress::Orchestrator,
                AgentAddress::User,
                "coordination",
                "Phoenix note",
                false,
            ),
        ];

        let (owned, internal) = split_user_messages_by_owner(messages, &owner);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].body, "Leo's integrated answer");
        assert_eq!(internal.len(), 2);
        assert_eq!(internal[0].body, "internal evidence");
        assert_eq!(internal[1].body, "Phoenix note");
    }

    #[test]
    fn late_helper_results_preserve_the_completed_answer_and_survive_restart_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let mut store = SessionStore::new(root.clone());
        let mut session = Session::new_main_with_id("late-helper-owner", "m", "test prompt");
        session.push_message(Message::User { content: "Explain Phoenix memory".into() });
        session.push_message(Message::Assistant { content: "The complete memory explanation".into() });
        store.upsert(session);store.save_one("late-helper-owner").unwrap();
        let helpers = vec![AgentMessage::talk(
            crate::runtime::mailbox::AgentAddress::Specialist(SubAgentType::Frontend),
            crate::runtime::mailbox::AgentAddress::User,
            "Earlier visualization fixed", "Verified the offline sender path", false,
        )];
        let owner = crate::runtime::mailbox::AgentAddress::Orchestrator;
        record_late_helper_results(dir.path(), "late-helper-owner", "memory-turn", &owner, &helpers).unwrap();
        record_late_helper_results(dir.path(), "late-helper-owner", "memory-turn", &owner, &helpers).unwrap();
        let restored = SessionStore::read_one_from_disk(&root, "late-helper-owner").unwrap().unwrap();
        assert_eq!(restored.messages.len(), 3, "restart/replay must not duplicate the late result");
        assert!(matches!(&restored.messages[1], Message::Assistant { content } if content == "The complete memory explanation"));
        assert!(matches!(&restored.messages[2], Message::Talk { body, reply_expected: false, reply_to: Some(_), status, .. }
            if body.contains("Verified the offline sender path") && status == "done"));
    }

    #[test]
    fn a_room_does_not_repeat_its_transcript_as_the_final_answer() {
        // Every room contribution is already on screen as its own GroupMessage.
        // Emitting the assembled summary too printed each coworker's reply a
        // second time, verbatim, which read as one agent answering twice.
        let summary = "**Theo**\n\nThe shortest dependable route is Composio.";
        assert!(
            !should_emit_final_answer(summary, true),
            "a room must not restate messages it already streamed"
        );
        // An individual conversation streams nothing, so the summary is the
        // only user-facing surface and must still be emitted.
        assert!(should_emit_final_answer(summary, false));
        // An idle mesh has nothing to say either way.
        assert!(!should_emit_final_answer("   \n ", false));
        assert!(!should_emit_final_answer("", true));
    }

    #[tokio::test]
    async fn group_dispatch_carries_only_exact_saved_prerequisites() {
        use crate::runtime::gateway::{AgentTurnHandler, Gateway};
        use crate::runtime::group_conversation::{GroupDependency, GroupParticipant, GroupTurnContext};
        struct IndependentHandler;
        #[async_trait::async_trait]
        impl AgentTurnHandler for IndependentHandler {
            async fn run_turn(&self, addr: &AgentAddress, message: AgentMessage) -> Vec<AgentMessage> {
                assert_eq!(addr, &AgentAddress::Specialist(SubAgentType::Coder));
                assert_eq!(message.body, "independent backend work");
                vec![AgentMessage::talk(addr.clone(), AgentAddress::User, "Backend result", "backend completed once", false)]
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let mut group = GroupTurnContext {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "packet-room".into(), group_name: "Packet room".into(),
            canonical_session_id: "group-packet-room".into(), participants: vec![],
            discussion_rounds: 1, read_full_transcript: false, execution_waves: vec![],
            execution_dependencies: Some(vec![]),
        };
        for (id, role) in [("theo", "researcher"), ("iris", "frontend"), ("leo", "coder")] {
            group.participants.push(GroupParticipant { agent_id: id.into(), internal_role: role.into(),
                display_name: id.into(), role_title: String::new(), color: String::new(),
                icon_seed: id.into(), avatar: None, member_role: "member".into(),
                history_access: crate::runtime::company_directory::HistoryAccess::Full,
                history_start_message_index: 0, explicitly_mentioned: true });
        }
        persist_group_user_boundary(directory.path(), &group, "turn-packet", "build", "test").unwrap();
        let responses = [
            AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Researcher), AgentAddress::User,
                "Brief", "exact-é🦊\n| a | b |", false),
            AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Coder), AgentAddress::User,
                "Unrelated", "SIBLING_NOT_AN_INPUT", false),
        ];
        let receipts = persist_group_messages(directory.path(), &group, "turn-packet", &responses).unwrap();
        let edges = vec![GroupDependency { prerequisite: "theo".into(), dependent: "iris".into() }];
        let incoming = AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Frontend), "design");
        let output = prepare_group_dispatch(vec![incoming.clone()], &group, &edges, &receipts, directory.path()).unwrap();
        let packet: serde_json::Value = serde_json::from_str(output[0].body.rsplit_once('\n').unwrap().1).unwrap();
        assert_eq!(packet[0]["body"], responses[0].body);
        assert_eq!(packet[0]["receipt_id"], receipts[0].1);
        assert_eq!(output[0].group_input_receipts, vec![receipts[0].1.clone()]);
        assert!(!output[0].body.contains("SIBLING_NOT_AN_INPUT"));
        assert_eq!(output[0].causation_id, incoming.causation_id);
        assert_eq!(output[0].handoff_id, incoming.handoff_id);
        assert!(prepare_group_dispatch(vec![incoming.clone()], &group, &edges, &[], directory.path()).is_err());
        let substituted = vec![("theo".into(), receipts[1].1.clone())];
        assert!(prepare_group_dispatch(vec![incoming.clone()], &group, &edges, &substituted, directory.path()).is_err());
        let independent_message = AgentMessage::user_input(
            AgentAddress::Specialist(SubAgentType::Coder), "independent backend work");
        // Before: one missing prerequisite made the entire ready frontier fail.
        assert!(prepare_group_dispatch(vec![incoming.clone(), independent_message.clone()],
            &group, &edges, &[], directory.path()).is_err());
        for invalid_receipts in [&[][..], substituted.as_slice()] {
            let mut rejected = Vec::new();
            let ready = prepare_group_dispatch_isolated(
                vec![incoming.clone(), independent_message.clone()],
                &group, &edges, invalid_receipts, directory.path(),
                |owner, reason| { rejected.push((owner.to_string(), reason.to_string())); Ok(()) },
            ).unwrap();
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].body, independent_message.body);
            assert_eq!(ready[0].handoff_id, independent_message.handoff_id);
            assert_eq!(rejected.len(), 1);
            assert_eq!(rejected[0].0, "iris");
            assert!(rejected[0].1.starts_with("Required saved input unavailable:"));
            let mut gateway = Gateway::new(IndependentHandler);
            for message in ready { gateway.submit(message); }
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), gateway.run_dispatching(|_| Ok(vec![])))
                .await.expect("healthy sibling must execute despite the invalid prerequisite").unwrap();
            assert_eq!(outcome.user_messages.len(), 1);
            assert_eq!(outcome.user_messages[0].body, "backend completed once");
        }
        assert!(prepare_group_dispatch_isolated(
            vec![incoming.clone(), independent_message], &group, &edges, &[], directory.path(),
            |_, _| anyhow::bail!("durable rejection write failed"),
        ).unwrap_err().to_string().contains("durable rejection write failed"));
        // Compaction can move the required contribution out of live history.
        // The dispatched packet must remain identical after that relocation.
        let sessions = directory.path().join("sessions");
        let mut saved = SessionStore::read_one_from_disk(&sessions, &group.canonical_session_id).unwrap().unwrap();
        let archived = saved.messages.iter().filter(|record| matches!(record,
            Message::GroupContribution { message_id, .. } if message_id == &receipts[0].1)).cloned().collect::<Vec<_>>();
        let archive = sessions.join(format!("{}.archive.jsonl", group.canonical_session_id));
        let contents = archived.iter().map(|record| serde_json::to_string(record).unwrap() + "\n").collect::<String>();
        std::fs::write(&archive, contents).unwrap();
        saved.messages.retain(|record| !matches!(record,
            Message::GroupContribution { message_id, .. } if message_id == &receipts[0].1));
        let mut store = SessionStore::new(sessions);
        store.upsert(saved);
        store.save_one(&group.canonical_session_id).unwrap();
        let recovered = prepare_group_dispatch(vec![incoming.clone()], &group, &edges, &receipts, directory.path()).unwrap();
        assert_eq!(recovered[0].body, output[0].body);
        // An independent task is neither rewritten nor made to load history.
        let isolated = tempfile::tempdir().unwrap();
        let independent = prepare_group_dispatch(vec![incoming], &group, &[], &[], isolated.path()).unwrap();
        assert_eq!(independent[0].body, "design");
        assert!(independent[0].group_input_receipts.is_empty());
    }

    #[test]
    fn authored_room_ping_replays_saved_source_and_publishes_recipient_once() {
        use crate::runtime::group_conversation::{preview_group_activation,resolve_group_turn_from_activation};
        use crate::runtime::company_directory::{GroupProfile,LifecycleState};
        let directory=tempfile::tempdir().unwrap();
        let company=crate::runtime::company::CompanyStore::open(directory.path().join("company.sqlite")).unwrap();
        company.ensure_full_catalog_team().unwrap();
        company.create_group("user",GroupProfile {
            group_id:"ping-room".into(),name:"Marketing Team".into(),description:"test".into(),
            color:"#123456".into(),icon_seed:"test".into(),lifecycle:LifecycleState::Active,
            pinned:false,sort_order:1,canonical_session_id:Some("group-ping-room".into()),metadata_json:"{}".into(),
        },vec!["phoenix".into(),"researcher".into()]).unwrap();
        let snapshot=company.directory_snapshot().unwrap();
        let prompt="@phoenix could you ping theo in this conv";
        let intent=preview_group_activation(&snapshot,"ping-room",prompt).unwrap().intent();
        let group=resolve_group_turn_from_activation(&snapshot,&intent).unwrap();
        let participants=group.explicitly_pinged().into_iter().cloned().collect::<Vec<_>>();
        company.reserve_group_turn(&group.canonical_session_id,"ping-turn",prompt,&intent,&participants).unwrap();
        persist_group_user_boundary(directory.path(),&group,"ping-turn",prompt,"test").unwrap();
        let authored=AgentMessage::talk(AgentAddress::Orchestrator,AgentAddress::User,
            "Invite Theo", "@researcher Theo, come join us here in the Marketing Team chat.",false);
        let (source_receipts,source_events)=persist_group_messages_with_events(directory.path(),&group,"ping-turn",&[authored]).unwrap();
        assert_eq!(source_events.len(),1);
        // This starts after the author commit, modeling interruption before
        // dispatch. The canonical receipt recovers the original delivery.
        assert_eq!(reserve_saved_group_pings(directory.path(),&company,&group,"ping-turn").unwrap(),vec!["researcher"]);
        assert!(reserve_saved_group_pings(directory.path(),&company,&group,"ping-turn").unwrap().is_empty());
        let ledger=company.group_turn(&group.canonical_session_id,"ping-turn").unwrap().unwrap();
        let member=ledger.members.iter().find(|member|member.participant.agent_id=="researcher").unwrap();
        let ping=saved_group_ping_message(directory.path(),&group,"ping-turn",member).unwrap();
        assert_eq!(ping.from,AgentAddress::Orchestrator);
        assert_eq!(ping.to,AgentAddress::Specialist(SubAgentType::Researcher));
        assert!(!ping.reply_expected(),"room reply goes to the room, without waking the caller again");
        assert_eq!(ping.causation_id.as_deref(),Some(source_receipts[0].1.as_str()));
        assert!(ping.body.contains("@researcher Theo, come join us"));
        company.mark_group_member_working(&group.canonical_session_id,"ping-turn","researcher").unwrap();
        let reply=AgentMessage::talk(ping.to.clone(),AgentAddress::User,"Theo joined","I'm here, @phoenix.",false);
        let (receipts,events)=persist_group_messages_with_events(directory.path(),&group,"ping-turn",&[reply.clone()]).unwrap();
        assert!(matches!(&events[0],CliEvent::GroupMessage{agent_id,markdown,..} if agent_id=="researcher" && markdown=="I'm here, @phoenix."));
        company.mark_group_member_done(&group.canonical_session_id,"ping-turn","researcher",&receipts[0].1).unwrap();
        assert!(reserve_saved_group_pings(directory.path(),&company,&group,"ping-turn").unwrap().is_empty(),"no acknowledgement loop");
        let replay=persist_group_messages_with_events(directory.path(),&group,"ping-turn",&[reply]).unwrap();
        assert_eq!(replay.0,receipts);
        let saved=SessionStore::read_one_from_disk(&directory.path().join("sessions"),&group.canonical_session_id).unwrap().unwrap();
        assert_eq!(saved.messages.iter().filter(|message|matches!(message,Message::User{..})).count(),1,"no invented human message");
        assert_eq!(saved.messages.iter().filter(|message|matches!(message,Message::GroupContribution{..})).count(),2,"one real contribution per speaker");
    }

    #[test]
    fn first_group_note_creates_its_transcript_and_is_durable_and_idempotent() {
        let directory = tempfile::tempdir().expect("temporary state root");
        let session_id = "group-silent-note";
        assert!(!directory.path().join("sessions").join(format!("{session_id}.json")).exists());
        let group = crate::runtime::group_conversation::GroupTurnContext {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "quiet-room".into(),
            group_name: "Quiet room".into(),
            canonical_session_id: session_id.into(),
            participants: vec![],
            discussion_rounds: 1,
            read_full_transcript: true,
            execution_dependencies: None,
            execution_waves: Vec::new(),
        };

        persist_group_user_boundary(
            directory.path(),
            &group,
            "turn-quiet-1",
            "A room note with no mentions",
            "test-model",
        )
        .expect("persist room note");
        persist_group_user_boundary(
            directory.path(),
            &group,
            "turn-quiet-1",
            "A room note with no mentions",
            "test-model",
        )
        .expect("retry room note");

        let reloaded =
            SessionStore::read_one_from_disk(&directory.path().join("sessions"), session_id)
                .expect("read group session")
                .expect("group session exists");
        assert_eq!(reloaded.messages.iter().filter(|message|matches!(message,Message::User{content} if content=="A room note with no mentions")).count(),1);
        assert_eq!(reloaded.messages.iter().filter(|message|matches!(message,Message::ToolResult{tool_name,input,..} if tool_name=="__phoenix_group_user_boundary"&&input=="turn-quiet-1")).count(),1);
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "invoked only by the parent crash-recovery test with a private fixture"]
    fn group_publication_crash_child() {
        let fixture = std::path::PathBuf::from(std::env::var_os("PHOENIX_TEST_PUBLICATION_FIXTURE").expect("private fixture required"));
        let group: crate::runtime::group_conversation::GroupTurnContext =
            serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
        let response = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Frontend), AgentAddress::User,
            "UI finding", "The historical attribution belongs to Iris.", false,
        );
        let (_, unpublished) = persist_group_messages_with_events(
            fixture.parent().unwrap(), &group, "turn-stable-1", &[response],
        ).unwrap();
        assert_eq!(unpublished.len(), 1);
        // Terminate this owned test process before returning its event to any
        // publisher. SIGKILL skips cleanup and cannot create a core dump.
        unsafe { libc::raise(libc::SIGKILL); }
        panic!("SIGKILL unexpectedly returned");
    }

    #[test]
    fn persisted_group_attribution_survives_rename_removal_and_reload() {
        let directory = tempfile::tempdir().expect("temporary state root");
        let session_id = "group-stable-history";
        let mut store = SessionStore::new(directory.path().join("sessions"));
        store.upsert(Session::new_main_with_id(session_id, "test", "system"));
        store.save_one(session_id).expect("seed group session");

        let mut group = crate::runtime::group_conversation::GroupTurnContext {
            tool_constraints: Default::default(),
            inspection_participants: Default::default(),
            group_id: "design-room".to_string(),
            group_name: "Design room".to_string(),
            canonical_session_id: session_id.to_string(),
            participants: vec![crate::runtime::group_conversation::GroupParticipant {
                agent_id: "agent-iris-stable".to_string(),
                internal_role: "frontend".to_string(),
                display_name: "Iris".to_string(),
                role_title: "Product Designer".to_string(),
                color: "#d46a43".to_string(),
                icon_seed: "iris-original".to_string(),
                avatar: Some(serde_json::json!({
                    "mode": "flame",
                    "shape": "wild",
                    "expression": "focused",
                    "accessory": "round_glasses"
                })),
                member_role: "designer".to_string(),
                history_access: crate::runtime::company_directory::HistoryAccess::Full,
                history_start_message_index: 0,
                explicitly_mentioned: true,
            }],
            discussion_rounds: 1,
            read_full_transcript: true,
            execution_dependencies: None,
            execution_waves: vec![vec!["agent-iris-stable".to_string()]],
        };
        let response = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Frontend),
            AgentAddress::User,
            "UI finding",
            "The historical attribution belongs to Iris.",
            false,
        );

        #[cfg(unix)]
        let first_receipts = {
            use std::os::unix::process::ExitStatusExt;
            let fixture = directory.path().join("publication-fixture.json");
            std::fs::write(&fixture, serde_json::to_vec(&group).unwrap()).unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::runner::slices::accountability_tests::group_publication_crash_child", "--ignored", "--nocapture"])
                .env("PHOENIX_TEST_PUBLICATION_FIXTURE", &fixture)
                .output().expect("start owned publication process");
            assert_eq!(child.status.signal(), Some(libc::SIGKILL),
                "child must stop at publication boundary, stderr: {}", String::from_utf8_lossy(&child.stderr));
            let (receipt, recovered) = persisted_group_contribution(
                directory.path(), &group, "turn-stable-1", "agent-iris-stable",
            ).unwrap().expect("killed writer's durable message must be recoverable");
            assert!(matches!(recovered, CliEvent::GroupMessage { ref message_id, ref markdown, .. }
                if message_id == &receipt && markdown == &response.body));
            vec![("agent-iris-stable".to_string(), receipt)]
        };
        #[cfg(not(unix))]
        let first_receipts = persist_group_messages(
            directory.path(),
            &group,
            "turn-stable-1",
            &[response.clone()],
        )
        .expect("persist contribution");
        // A retry after the transcript commit can produce a fresh provider
        // message id and wording. The first (turn, agent) contribution remains
        // authoritative and supplies the same receipt to ledger reconciliation.
        let changed_retry = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Frontend),
            AgentAddress::User,
            "UI finding retried",
            "A second generated answer must not be appended.",
            false,
        );
        let (retry_receipts, retry_events) =
            persist_group_messages_with_events(directory.path(), &group, "turn-stable-1", &[changed_retry])
                .expect("idempotent contribution retry");
        assert_eq!(retry_receipts, first_receipts);
        assert!(matches!(&retry_events[0], CliEvent::GroupMessage { message_id, markdown, .. }
            if message_id == &first_receipts[0].1 && markdown == "The historical attribution belongs to Iris."),
            "reused receipt must publish the saved body, never the divergent retry body");
        assert_eq!(
            persisted_group_contribution(
                directory.path(),
                &group,
                "turn-stable-1",
                "agent-iris-stable",
            )
            .unwrap().map(|(receipt, _)| receipt),
            Some(first_receipts[0].1.clone())
        );

        // Simulate both presentation edits and eventual roster removal after
        // the contribution was authored. Reload must use only the snapshot in
        // the canonical session, never the mutable current roster.
        group.participants[0].display_name = "Renamed coworker".to_string();
        group.participants[0].role_title = "Different title".to_string();
        group.participants[0].color = "#000000".to_string();
        group.participants[0].icon_seed = "changed-icon".to_string();
        group.participants.clear();

        // Crash boundary: the canonical write succeeded but no consumer saw
        // the event. Reconstruct it from disk, with no provider response or
        // current roster snapshot. Repeated recovery must be read-only and
        // return the same event identity/body for UI deduplication.
        let canonical_path = directory.path().join("sessions").join(format!("{session_id}.json"));
        let before_recovery = std::fs::read(&canonical_path).unwrap();
        std::fs::write(directory.path().join("sessions/unrelated-corrupt.json"), b"not a session").unwrap();
        for _ in 0..2 {
            let (receipt, recovered_event) = persisted_group_contribution(
                directory.path(), &group, "turn-stable-1", "agent-iris-stable",
            ).unwrap().unwrap();
            assert_eq!(receipt, first_receipts[0].1);
            assert!(matches!(recovered_event, CliEvent::GroupMessage { message_id, agent_name, markdown, .. }
                if message_id == receipt && agent_name == "Iris" && markdown == "The historical attribution belongs to Iris."));
        }
        assert!(persisted_group_contribution(directory.path(), &group, "different-turn", "agent-iris-stable").unwrap().is_none());
        assert!(persisted_group_contribution(directory.path(), &group, "turn-stable-1", "different-agent").unwrap().is_none());
        assert_eq!(std::fs::read(&canonical_path).unwrap(), before_recovery);

        let reloaded =
            SessionStore::read_one_from_disk(&directory.path().join("sessions"), session_id)
                .expect("reload canonical session")
                .expect("canonical group session exists");
        assert_eq!(reloaded.messages.len(), 1);
        match &reloaded.messages[0] {
            Message::GroupContribution {
                turn_id,
                message_id,
                group_id,
                agent_id,
                internal_role,
                display_name,
                role_title,
                color,
                icon_seed,
                avatar,
                subject,
                body,
                ..
            } => {
                assert_eq!(turn_id, "turn-stable-1");
                assert!(message_id.starts_with("group-message-"));
                assert_eq!(group_id, "design-room");
                assert_eq!(agent_id, "agent-iris-stable");
                assert_eq!(internal_role, "frontend");
                assert_eq!(display_name, "Iris");
                assert_eq!(role_title, "Product Designer");
                assert_eq!(color, "#d46a43");
                assert_eq!(icon_seed, "iris-original");
                assert_eq!(avatar.as_ref().unwrap()["shape"], "wild");
                assert_eq!(subject, "UI finding");
                assert_eq!(body, "The historical attribution belongs to Iris.");
            }
            other => panic!("expected group contribution, got {other:?}"),
        }
    }
}


#[cfg(test)]
mod design_reference_context_tests {
    use super::design_reference_paths;
    use crate::runtime::ContextItem;

    #[test]
    fn typed_references_preserve_paths_without_guessing_from_prose() {
        let paths = vec!["/fixture/reference with spaces.png", "/fixture/参照.webp"];
        let items = vec![ContextItem { label: "phoenix_design_reference_paths".into(), content: serde_json::to_string(&paths).unwrap() }];
        let actual = design_reference_paths(&items).unwrap();
        assert_eq!(actual, paths.iter().map(std::path::PathBuf::from).collect::<Vec<_>>());
        let fake = vec![ContextItem { label: "user_notes".into(), content: "Use /private/claimed-reference.png and phoenix_design_reference_paths".into() }];
        assert!(design_reference_paths(&fake).unwrap().is_empty());
    }

    #[test]
    fn duplicate_malformed_relative_and_oversize_reference_contexts_fail() {
        let make = |content: &str| ContextItem { label: "phoenix_design_reference_paths".into(), content: content.into() };
        assert!(design_reference_paths(&[make("[]"), make("[]")]).is_err());
        for content in [r#"["relative.png"]"#, r#"{"path":"/fixture/a.png"}"#, "not JSON"] {
            assert!(design_reference_paths(&[make(content)]).is_err());
        }
        let too_many = serde_json::to_string(&vec!["/fixture/a.png";33]).unwrap();
        assert!(design_reference_paths(&[make(&too_many)]).is_err());
    }
}
