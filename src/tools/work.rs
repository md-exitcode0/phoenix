//! Shared coordination primitives for the company runtime.
//!
//! `talk` carries conversation. `work` changes shared state. Keeping those
//! separate means an agent can chat freely without silently creating work, and
//! every claim, handoff, artifact, challenge, and decision remains replayable.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::runtime::company::{
    self, CollaborationPattern, CompanyEventKind, FocusFrame, LearningKind, MessageKind,
    NewCompanyEvent, WorkState,
};
use crate::runtime::workflow::{
    ConcurrencyPolicy, DurableWorkflowContract, DurableWorkflowScheduler, EvidenceReceipt,
    GoalRequest, LeaseToken, NodeSpec, RestartState, RunRequest, WorkflowNodeState,
    WorkflowOwnership, WorkflowPhase, WorkflowScope, WorkflowUsage, WorkflowWaitReceipt,
    WorkflowPlanRequest,
};

use super::{workspace_io, ToolOutput};

const MAX_WORK_INPUT_BYTES: usize = 1024 * 1024;
const MAX_WORK_FIELD_BYTES: usize = 64 * 1024;
const MAX_WORK_LIST_ITEMS: usize = 256;
/// Tool results are injected into the model transcript, so inspection must
/// stay well below the runtime's general message ceiling.
const MAX_WORK_RESULT_BYTES: usize = 512 * 1024;
const MAX_WORK_INSPECT_ROWS_PER_KIND: usize = 128;
const MAX_WORK_INSPECT_EVENTS: usize = 80;
const MAX_WORK_INSPECT_SNAPSHOT_BYTES: usize = 192 * 1024;
const MAX_WORK_INSPECT_EVENT_BYTES: usize = 192 * 1024;

#[derive(Debug, Deserialize)]
pub struct WorkInput {
    pub action: String,
    /// Additive durable-workflow command surface.  `workflow` deliberately
    /// stays separate from the older human work graph actions below.
    #[serde(default)]
    pub workflow_action: Option<String>,
    #[serde(default)]
    pub workflow_run_id: Option<String>,
    #[serde(default)]
    pub workflow_worker_id: Option<String>,
    /// Typed payload for durable workflow lifecycle actions. Its schema
    /// depends on `workflow_action` and is still bounded with the whole call.
    #[serde(default)]
    pub workflow_payload: Option<serde_json::Value>,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub pattern: Option<CollaborationPattern>,
    #[serde(default)]
    pub approach: Option<String>,
    #[serde(default)]
    pub state: Option<WorkState>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub focus: Option<FocusFrame>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub source_artifacts: Vec<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub claim: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub accepted_id: Option<String>,
    #[serde(default)]
    pub voters: Vec<String>,
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub checkpoint: Option<String>,
    #[serde(default)]
    pub candidate_id: Option<String>,
    #[serde(default)]
    pub learning_kind: Option<LearningKind>,
    #[serde(default)]
    pub statement: Option<String>,
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub canary_receipt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowTransitionInput {
    pub node_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub lease: Option<LeaseToken>,
    pub phase: WorkflowPhase,
    pub state: WorkflowNodeState,
    pub restart_state: RestartState,
    pub reason: String,
    #[serde(default)]
    pub next_wake_at: Option<String>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub usage: Option<WorkflowUsage>,
    #[serde(default)]
    pub evidence: Option<Vec<EvidenceReceipt>>,
}

#[derive(Debug, Deserialize)]
struct WorkflowWaitInput {
    lease: LeaseToken,
    receipt: WorkflowWaitReceipt,
}

#[derive(Debug, Deserialize)]
struct WorkflowWaitAnswerInput {
    node_id: String,
    receipt_id: String,
    answer: String,
}

#[derive(Debug, Deserialize)]
struct WorkflowEvidenceInput {
    pub lease: LeaseToken,
    pub receipt: EvidenceReceipt,
}

#[derive(Debug, Deserialize)]
struct WorkflowOwnershipChangeInput {
    pub idempotency_key: String,
    pub reason: String,
    #[serde(default)]
    pub new_owner_agent_id: Option<String>,
}

struct WorkContext<'a> {
    run_id: &'a str,
    session_id: &'a str,
    agent: &'a str,
}

pub fn execute(
    workspace: &Path,
    mut input: WorkInput,
    session_id: Option<&str>,
    agent: Option<&str>,
) -> Result<ToolOutput> {
    normalize_obvious_aliases(&mut input);
    validate_input(&input)?;
    if session_id.is_some_and(|value| value.len() > 1024)
        || agent.is_some_and(|value| value.len() > 1024)
    {
        bail!("work session/agent identity exceeds the 1024-byte limit");
    }
    let session_id = session_id.unwrap_or("main-session");
    let context = WorkContext {
        run_id: session_id,
        session_id,
        agent: agent.unwrap_or("orchestrator"),
    };
    let store = company::global()?;
    let mut summary = None;
    let result = match input.action.as_str() {
        "inspect" => {
            let (mut snapshot, snapshot_partial) = store.bounded_snapshot_for_run(
                context.run_id,
                MAX_WORK_INSPECT_ROWS_PER_KIND,
                MAX_WORK_INSPECT_SNAPSHOT_BYTES,
            )?;
            // The bounded legacy snapshot intentionally avoids taking a
            // second connection lock.  Add the durable projection after it
            // returns so Canvas and agents see one coherent inspect result.
            snapshot.workflow = store.workflow_snapshot(Some(context.run_id))?;
            let (events, events_partial) = store.events_since_bounded_for_run(
                context.run_id,
                input
                    .after_seq
                    .unwrap_or(snapshot.as_of_seq.saturating_sub(40)),
                MAX_WORK_INSPECT_EVENTS,
                MAX_WORK_INSPECT_EVENT_BYTES,
            )?;
            bounded_inspect_value(snapshot, events, snapshot_partial || events_partial)?
        }
        "workflow" => {
            let now = chrono::Utc::now();
            match input.workflow_action.as_deref().unwrap_or("inspect") {
                "install_plan" => {
                    let ownership_was_explicit = input.workflow_payload.as_ref()
                        .and_then(|payload| payload.get("contract"))
                        .and_then(|contract| contract.get("ownership")).is_some();
                    let mut request: WorkflowPlanRequest = workflow_contract_payload(input.workflow_payload)?;
                    if !ownership_was_explicit || request.contract.ownership.owner_agent_id.trim().is_empty() {
                        request.contract.ownership = WorkflowOwnership { scope: WorkflowScope::Agent,
                            owner_agent_id: directory_agent_id_for_actor(&store, context.agent)?, group_id: None };
                    }
                    validate_workflow_ownership(&store, &request.contract.ownership)?;
                    let scheduler = DurableWorkflowScheduler::new(store.clone(), request.contract.concurrency.clone())?;
                    serde_json::to_value(scheduler.install_plan(request)?)?
                }
                "create_goal" => {
                    let scheduler =
                        DurableWorkflowScheduler::new(store.clone(), ConcurrencyPolicy::default())?;
                    let ownership_was_explicit = input
                        .workflow_payload
                        .as_ref()
                        .and_then(|payload| payload.get("contract"))
                        .and_then(|contract| contract.get("ownership"))
                        .is_some();
                    let mut request: GoalRequest = workflow_contract_payload(input.workflow_payload)
                        .context("create_goal expects evidence_requirements objects {kind, description, minimum_receipts, required}, not strings. Omit goal_id and use a stable idempotency_key to let Phoenix generate it; an explicit goal_id must start with goal_.")?;
                    let actor_agent_id = directory_agent_id_for_actor(&store, context.agent)?;
                    if !ownership_was_explicit
                        || request.contract.ownership.owner_agent_id.trim().is_empty()
                    {
                        request.contract.ownership.scope = WorkflowScope::Agent;
                        request.contract.ownership.owner_agent_id = actor_agent_id.clone();
                        request.contract.ownership.group_id = None;
                    }
                    validate_workflow_ownership(&store, &request.contract.ownership)?;
                    let goal_id = scheduler.create_goal(request)?;
                    serde_json::json!({
                        "goal_id": goal_id,
                    })
                }
                "open_run" => {
                    let scheduler =
                        DurableWorkflowScheduler::new(store.clone(), ConcurrencyPolicy::default())?;
                    let request: RunRequest = workflow_payload(input.workflow_payload)?;
                    let run_id = scheduler.open_run(request)?;
                    serde_json::json!({
                        "run_id": run_id,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                "define_node" => {
                    let spec: NodeSpec = workflow_payload(input.workflow_payload)?;
                    let run_id = spec.run_id.clone();
                    let scheduler = workflow_scheduler_for_run(&store, &spec.run_id)?;
                    let node_id = scheduler.define_node(spec)?;
                    serde_json::json!({
                        "node_id": node_id,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                "inspect" => {
                    let scheduler = match input.workflow_run_id.as_deref() {
                        Some(run_id) => workflow_scheduler_for_run(&store, run_id)?,
                        None => DurableWorkflowScheduler::new(
                            store.clone(),
                            ConcurrencyPolicy::default(),
                        )?,
                    };
                    serde_json::to_value(scheduler.projection(input.workflow_run_id.as_deref())?)?
                }
                "reconcile" => {
                    let run_id = required(input.workflow_run_id, "workflow_run_id")?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    let report = scheduler.reconcile(&run_id, now)?;
                    serde_json::json!({
                        "report": report,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                "tick" => {
                    let run_id = required(input.workflow_run_id, "workflow_run_id")?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    let worker_id = directory_agent_id_for_actor(&store, context.agent)?;
                    if let Some(requested_worker) = input.workflow_worker_id.as_deref() {
                        let requested_worker =
                            directory_agent_id_for_actor(&store, requested_worker)?;
                        anyhow::ensure!(
                            requested_worker == worker_id,
                            "workflow_worker_id cannot impersonate another coworker; this call is bound to `{worker_id}`"
                        );
                    }
                    let (report, leased) = scheduler.tick(&run_id, &worker_id, now)?;
                    serde_json::json!({
                        "report": report,
                        "leased": leased,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                "heartbeat" => {
                    let lease: LeaseToken = workflow_payload(input.workflow_payload)?;
                    validate_workflow_lease_actor(&store, context.agent, &lease)?;
                    let scheduler = workflow_scheduler_for_node(&store, &lease.node_id)?;
                    let expires_at = scheduler.heartbeat(&lease, now)?;
                    serde_json::json!({ "expires_at": expires_at })
                }
                "wait_for" => {
                    let mut wait: WorkflowWaitInput = workflow_payload(input.workflow_payload)?;
                    validate_workflow_lease_actor(&store, context.agent, &wait.lease)?;
                    anyhow::ensure!(wait.receipt.responder != "user",
                        "use ask_user for user questions; workflow user waits require runtime-owned ask binding");
                    let run_id = store.workflow_node_run_id(&wait.lease.node_id)?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    if wait.receipt.responder != "user" {
                        wait.receipt.responder = directory_agent_id_for_actor(&store, &wait.receipt.responder)?;
                        validate_wait_peer(&store, &run_id, &wait.receipt.responder)?;
                    }
                    summary = Some(format!("Waiting for {}; assignment execution slot released", wait.receipt.responder));
                    scheduler.wait_for(&wait.lease, wait.receipt)?;
                    serde_json::json!({"workflow": scheduler.projection(Some(&run_id))?})
                }
                "resolve_wait" => {
                    let answer: WorkflowWaitAnswerInput = workflow_payload(input.workflow_payload)?;
                    let responder = directory_agent_id_for_actor(&store, context.agent)?;
                    let run_id = store.workflow_node_run_id(&answer.node_id)?;
                    validate_wait_peer(&store, &run_id, &responder)?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    let made_ready = scheduler.resolve_wait(&answer.node_id, &answer.receipt_id, &responder, &answer.answer)?;
                    summary = Some(if made_ready { "Matching answer recorded; assignment is ready" }
                        else { "Answer already recorded; no duplicate readiness event" }.into());
                    serde_json::json!({"made_ready": made_ready, "workflow": scheduler.projection(Some(&run_id))?})
                }
                "record_evidence" => {
                    let evidence: WorkflowEvidenceInput = workflow_payload(input.workflow_payload)?;
                    validate_workflow_lease_actor(&store, context.agent, &evidence.lease)?;
                    let scheduler = workflow_scheduler_for_node(&store, &evidence.lease.node_id)?;
                    scheduler.record_evidence(&evidence.lease, evidence.receipt)?;
                    serde_json::json!({ "node_id": evidence.lease.node_id, "recorded": true })
                }
                "transition" => {
                    let transition: WorkflowTransitionInput =
                        workflow_payload(input.workflow_payload)?;
                    if let Some(lease) = transition.lease.as_ref() {
                        validate_workflow_lease_actor(&store, context.agent, lease)?;
                    }
                    let run_id = store.workflow_node_run_id(&transition.node_id)?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    if transition.lease.is_none() {
                        let actor = directory_agent_id_for_actor(&store, context.agent)?;
                        let snapshot = scheduler.projection(Some(&run_id))?;
                        let node = snapshot.nodes.iter().find(|n| n.node_id == transition.node_id)
                            .context("workflow assignment does not exist")?;
                        let coordinator = snapshot.runs.first().context("workflow run does not exist")?;
                        anyhow::ensure!(node.owner_agent_id.as_deref() == Some(actor.as_str())
                            || coordinator.owner_agent_id == actor,
                            "only the assignment owner or run coordinator may change an unleased task");
                    }
                    scheduler.transition_node(
                        &transition.node_id,
                        &transition.idempotency_key,
                        transition.lease.as_ref(),
                        transition.phase,
                        transition.state,
                        transition.restart_state,
                        transition.reason,
                        transition.next_wake_at,
                        transition.result,
                        transition.usage,
                        transition.evidence,
                    )?;
                    serde_json::json!({
                        "node_id": transition.node_id,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                "resume" | "reroute" => {
                    let action = input.workflow_action.as_deref().unwrap();
                    let run_id = required(input.workflow_run_id, "workflow_run_id")?;
                    let change: WorkflowOwnershipChangeInput =
                        workflow_payload(input.workflow_payload)?;
                    if action == "reroute" && change.new_owner_agent_id.is_none() {
                        bail!("workflow reroute requires new_owner_agent_id");
                    }
                    let actor_agent_id = directory_agent_id_for_actor(&store, context.agent)?;
                    let new_owner = change
                        .new_owner_agent_id
                        .as_deref()
                        .map(|owner| directory_agent_id_for_actor(&store, owner))
                        .transpose()?;
                    let scheduler = workflow_scheduler_for_run(&store, &run_id)?;
                    let owner = scheduler.resume_or_reroute_run(
                        &run_id,
                        &actor_agent_id,
                        new_owner.as_deref(),
                        &change.reason,
                        &change.idempotency_key,
                    )?;
                    serde_json::json!({
                        "run_id": run_id,
                        "owner_agent_id": owner,
                        "workflow": scheduler.projection(Some(&run_id))?,
                    })
                }
                other => {
                    bail!("unknown workflow action `{other}`; expected install_plan, create_goal, open_run, define_node, inspect, reconcile, tick, heartbeat, wait_for, resolve_wait, record_evidence, transition, resume, or reroute")
                }
            }
        }
        "propose" => {
            let node_id = input.node_id.unwrap_or_else(|| id("work"));
            append(
                &store,
                &context,
                Some(&node_id),
                CompanyEventKind::WorkProposed {
                    node_id: node_id.clone(),
                    title: required(input.title, "title")?,
                    outcome: required(input.outcome, "outcome")?,
                    acceptance: input.acceptance,
                    dependencies: input.dependencies,
                    pattern: input.pattern.unwrap_or(CollaborationPattern::Split),
                },
            )?;
            serde_json::json!({ "node_id": node_id, "state": "proposed" })
        }
        "claim" | "join" => {
            let node_id = required(input.node_id, "node_id")?;
            let attempt_id = id("attempt");
            append_many(
                &store,
                &context,
                Some(&node_id),
                vec![
                    CompanyEventKind::WorkClaimed {
                        node_id: node_id.clone(),
                        attempt_id: attempt_id.clone(),
                        identity_id: context.agent.to_string(),
                        approach: required(input.approach, "approach")?,
                    },
                    CompanyEventKind::WorkStateChanged {
                        node_id: node_id.clone(),
                        state: WorkState::Claimed,
                        reason: format!("{} joined as {attempt_id}", context.agent),
                    },
                ],
            )?;
            serde_json::json!({ "node_id": node_id, "attempt_id": attempt_id, "agent": context.agent })
        }
        "status" => {
            let node_id = required(input.node_id, "node_id")?;
            let state = input.state.context(
                "state is required for work action=status; choose proposed, ready, claimed, active, review_ready, accepted, rejected, superseded, waiting_user, or canceled",
            )?;
            let mut updates = vec![CompanyEventKind::WorkStateChanged {
                node_id: node_id.clone(),
                state: state.clone(),
                reason: input.reason.unwrap_or_default(),
            }];
            if let Some(frame) = input.focus {
                updates.push(CompanyEventKind::FocusUpdated { frame });
            }
            append_many(&store, &context, Some(&node_id), updates)?;
            serde_json::json!({ "node_id": node_id, "state": state })
        }
        "release" => {
            let node_id = required(input.node_id, "node_id")?;
            append(
                &store,
                &context,
                Some(&node_id),
                CompanyEventKind::WorkStateChanged {
                    node_id: node_id.clone(),
                    state: WorkState::Ready,
                    reason: input
                        .reason
                        .unwrap_or_else(|| format!("released by {}", context.agent)),
                },
            )?;
            serde_json::json!({ "node_id": node_id, "state": "ready" })
        }
        "publish" => {
            let path = required(input.path, "path")?;
            workspace_io::validate_path_input(&path)?;
            let (content_hash, bytes) = workspace_io::sha256_regular_file(workspace, &path, true)?;
            let artifact_id = id("artifact");
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::ArtifactPublished {
                    artifact_id: artifact_id.clone(),
                    path: path.clone(),
                    content_hash: content_hash.clone(),
                    source_artifacts: input.source_artifacts,
                },
            )?;
            serde_json::json!({ "artifact_id": artifact_id, "path": path, "content_hash": content_hash, "bytes": bytes })
        }
        "challenge" => {
            let challenge_id = id("challenge");
            let target_id = required(input.target_id, "target_id")?;
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::ChallengeRaised {
                    challenge_id: challenge_id.clone(),
                    target_id: target_id.clone(),
                    claim: required(input.claim, "claim")?,
                    evidence: input.evidence,
                },
            )?;
            serde_json::json!({ "challenge_id": challenge_id, "target_id": target_id })
        }
        "decide" => {
            let decision_id = id("decision");
            let target_id = required(input.target_id, "target_id")?;
            let accepted_id = required(input.accepted_id, "accepted_id")?;
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::DecisionRecorded {
                    decision_id: decision_id.clone(),
                    target_id,
                    accepted_id: accepted_id.clone(),
                    voters: input.voters,
                    rationale: required(input.reason, "reason")?,
                },
            )?;
            serde_json::json!({ "decision_id": decision_id, "accepted_id": accepted_id })
        }
        "note" => {
            let message_id = id("message");
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::MessageAccepted {
                    message_id: message_id.clone(),
                    operation_id: String::new(),
                    handoff_id: message_id.clone(),
                    reply_to: None,
                    causation_id: None,
                    from: context.agent.to_string(),
                    to: "company".to_string(),
                    subject: input.title.unwrap_or_else(|| "company note".to_string()),
                    body: required(input.reason, "reason")?,
                    message_kind: MessageKind::Status,
                    reply_expected: false,
                },
            )?;
            serde_json::json!({ "message_id": message_id })
        }
        "skill_checkpoint" => {
            let skill = required(input.skill, "skill")?;
            let checkpoint = required(input.checkpoint, "checkpoint")?;
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::SkillCheckpoint {
                    skill: skill.clone(),
                    checkpoint: checkpoint.clone(),
                },
            )?;
            serde_json::json!({ "skill": skill, "checkpoint": checkpoint })
        }
        "propose_learning" => {
            let candidate_id = input.candidate_id.unwrap_or_else(|| id("learning"));
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::LearningProposed {
                    candidate_id: candidate_id.clone(),
                    learning_kind: input.learning_kind.context("learning_kind is required")?,
                    statement: required(input.statement, "statement")?,
                    evidence: input.evidence,
                    producer: context.agent.to_string(),
                },
            )?;
            serde_json::json!({ "candidate_id": candidate_id, "state": "proposed" })
        }
        "promote_learning" => {
            let candidate_id = required(input.candidate_id, "candidate_id")?;
            if input.reviewers.len() < 2 {
                bail!("learning promotion requires at least two peer reviewers");
            }
            let canary_receipt = required(input.canary_receipt, "canary_receipt")?;
            append(
                &store,
                &context,
                input.node_id.as_deref(),
                CompanyEventKind::LearningPromoted {
                    candidate_id: candidate_id.clone(),
                    reviewers: input.reviewers,
                    canary_receipt,
                },
            )?;
            serde_json::json!({ "candidate_id": candidate_id, "state": "promoted" })
        }
        other => bail!("unknown work action `{other}`"),
    };

    let content = serde_json::to_string_pretty(&result)?;
    if content.len() > MAX_WORK_RESULT_BYTES {
        bail!("work result exceeds the {MAX_WORK_RESULT_BYTES}-byte limit");
    }
    Ok(ToolOutput {
        summary: summary.unwrap_or_else(|| format!("company work {} accepted", input.action)),
        content,
    })
}

fn validate_input(input: &WorkInput) -> Result<()> {
    if input.action.is_empty() || input.action.len() > 64 {
        bail!("work action must contain 1..=64 bytes");
    }

    let mut total = input.action.len();
    if let Some(payload) = &input.workflow_payload {
        let payload_bytes = serde_json::to_vec(payload)?;
        total = total
            .checked_add(payload_bytes.len())
            .context("work input byte count overflowed")?;
        if total > MAX_WORK_INPUT_BYTES {
            bail!("work input exceeds the {MAX_WORK_INPUT_BYTES}-byte aggregate limit");
        }
    }
    let mut charge = |label: &str, value: &str| -> Result<()> {
        if value.len() > MAX_WORK_FIELD_BYTES {
            bail!(
                "work {label} is too large ({} bytes; max {})",
                value.len(),
                MAX_WORK_FIELD_BYTES
            );
        }
        total = total
            .checked_add(value.len())
            .context("work input byte count overflowed")?;
        if total > MAX_WORK_INPUT_BYTES {
            bail!("work input exceeds the {MAX_WORK_INPUT_BYTES}-byte aggregate limit");
        }
        Ok(())
    };

    for (label, value) in [
        ("node_id", input.node_id.as_deref()),
        ("workflow_action", input.workflow_action.as_deref()),
        ("workflow_run_id", input.workflow_run_id.as_deref()),
        ("workflow_worker_id", input.workflow_worker_id.as_deref()),
        ("title", input.title.as_deref()),
        ("outcome", input.outcome.as_deref()),
        ("approach", input.approach.as_deref()),
        ("reason", input.reason.as_deref()),
        ("path", input.path.as_deref()),
        ("target_id", input.target_id.as_deref()),
        ("claim", input.claim.as_deref()),
        ("accepted_id", input.accepted_id.as_deref()),
        ("skill", input.skill.as_deref()),
        ("checkpoint", input.checkpoint.as_deref()),
        ("candidate_id", input.candidate_id.as_deref()),
        ("statement", input.statement.as_deref()),
        ("canary_receipt", input.canary_receipt.as_deref()),
    ] {
        if let Some(value) = value {
            charge(label, value)?;
        }
    }

    for (label, values) in [
        ("acceptance", input.acceptance.as_slice()),
        ("dependencies", input.dependencies.as_slice()),
        ("source_artifacts", input.source_artifacts.as_slice()),
        ("evidence", input.evidence.as_slice()),
        ("voters", input.voters.as_slice()),
        ("reviewers", input.reviewers.as_slice()),
    ] {
        if values.len() > MAX_WORK_LIST_ITEMS {
            bail!(
                "work {label} has too many entries ({}; max {})",
                values.len(),
                MAX_WORK_LIST_ITEMS
            );
        }
        for value in values {
            charge(label, value)?;
        }
    }

    if let Some(focus) = &input.focus {
        charge("focus.intent", &focus.intent)?;
        charge("focus.next_event", &focus.next_event)?;
        for (label, values) in [
            ("focus.evidence", focus.evidence.as_slice()),
            ("focus.collaborators", focus.collaborators.as_slice()),
            ("focus.artifacts", focus.artifacts.as_slice()),
            ("focus.blockers", focus.blockers.as_slice()),
        ] {
            if values.len() > MAX_WORK_LIST_ITEMS {
                bail!(
                    "work {label} has too many entries ({}; max {})",
                    values.len(),
                    MAX_WORK_LIST_ITEMS
                );
            }
            for value in values {
                charge(label, value)?;
            }
        }
    }
    Ok(())
}

/// The first work schema predated action-discriminated requirements and put
/// every action's fields in one flat object. Models therefore sometimes used
/// a semantically obvious sibling field. Accept only the two unambiguous,
/// lossless aliases so an old session does not burn another provider round;
/// the published schema below still teaches the canonical field names.
fn normalize_obvious_aliases(input: &mut WorkInput) {
    if matches!(
        input.action.as_str(),
        "claim" | "join" | "status" | "release"
    ) && input.node_id.as_deref().is_none_or(str::is_empty)
    {
        input.node_id = input.target_id.take();
    }
    if input.action == "note" && input.reason.as_deref().is_none_or(str::is_empty) {
        input.reason = input.statement.take();
    }
}

/// Reject misspelled controls on new declarations before they can become a
/// defaulted, weaker contract. Stored contracts keep their tolerant reader:
/// older projections may contain fields that this version does not interpret.
fn workflow_contract_payload<T>(payload: Option<serde_json::Value>) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let payload = payload.context("workflow_payload is required")?;
    for (value, path, fields) in [
        (
            payload.get("contract"),
            "workflow_payload.contract",
            &[
                "title", "objective", "budget", "evidence_requirements", "concurrency",
                "metadata", "ownership",
            ][..],
        ),
        (
            payload.pointer("/contract/concurrency"),
            "workflow_payload.contract.concurrency",
            &[
                "max_queued_nodes", "max_active_workers", "max_active_per_run",
                "max_claims_per_tick", "max_nodes_per_run",
            ][..],
        ),
    ] {
        if let Some(object) = value.and_then(serde_json::Value::as_object) {
            if let Some(field) = object.keys().find(|field| !fields.contains(&field.as_str())) {
                let field: String = field.chars().take(120).collect();
                bail!("unknown field {field:?} in {path}; expected one of {}", fields.join(", "));
            }
        }
    }
    workflow_payload(Some(payload))
}

fn workflow_payload<T>(payload: Option<serde_json::Value>) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(payload.context("workflow_payload is required")?)
        .context("workflow_payload does not match the selected workflow action")
}

fn workflow_scheduler_for_run(
    store: &std::sync::Arc<company::CompanyStore>,
    run_id: &str,
) -> Result<DurableWorkflowScheduler> {
    let snapshot = store.workflow_snapshot(Some(run_id))?;
    let run = snapshot
        .runs
        .iter()
        .find(|run| run.run_id == run_id)
        .with_context(|| format!("workflow run `{run_id}` does not exist"))?;
    let goal = snapshot
        .goals
        .iter()
        .find(|goal| goal.goal_id == run.goal_id)
        .with_context(|| format!("workflow goal `{}` does not exist", run.goal_id))?;
    let contract: DurableWorkflowContract = serde_json::from_str(&goal.contract_json)
        .context("workflow goal contract projection is invalid")?;
    DurableWorkflowScheduler::new(store.clone(), contract.concurrency)
}

fn directory_agent_id_for_actor(store: &company::CompanyStore, actor: &str) -> Result<String> {
    let normalized = actor
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    let normalized = if normalized == "orchestrator" {
        "phoenix".to_string()
    } else {
        normalized
    };
    let snapshot = store.directory_snapshot()?;
    let matches = snapshot
        .agents
        .iter()
        .filter(|agent| {
            [
                agent.profile.agent_id.as_str(),
                agent.profile.internal_role.as_str(),
                agent.profile.display_name.as_str(),
            ]
            .into_iter()
            .any(|candidate| {
                candidate
                    .trim()
                    .to_ascii_lowercase()
                    .replace([' ', '-'], "_")
                    == normalized
            })
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(!matches.is_empty(), "unknown workflow coworker `{actor}`");
    anyhow::ensure!(
        matches.len() == 1,
        "workflow coworker `{actor}` is ambiguous; use the immutable agent id"
    );
    anyhow::ensure!(
        matches[0].profile.lifecycle == crate::runtime::company_directory::LifecycleState::Active,
        "workflow coworker `{actor}` is not active"
    );
    Ok(matches[0].profile.agent_id.clone())
}

fn validate_workflow_ownership(
    store: &company::CompanyStore,
    ownership: &WorkflowOwnership,
) -> Result<()> {
    let owner = directory_agent_id_for_actor(store, &ownership.owner_agent_id)?;
    anyhow::ensure!(
        owner == ownership.owner_agent_id,
        "workflow owner_agent_id must use the immutable agent id `{owner}`"
    );
    if ownership.scope == WorkflowScope::Group {
        let group_id = ownership
            .group_id
            .as_deref()
            .context("group workflow scope requires group_id")?;
        let snapshot = store.directory_snapshot()?;
        let group = snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == group_id)
            .with_context(|| format!("unknown workflow group `{group_id}`"))?;
        anyhow::ensure!(
            group.profile.lifecycle == crate::runtime::company_directory::LifecycleState::Active,
            "workflow group `{group_id}` is not active"
        );
        anyhow::ensure!(
            snapshot
                .members
                .iter()
                .any(|member| member.group_id == group_id && member.agent_id == owner),
            "workflow owner `{owner}` is not a member of group `{group_id}`"
        );
    }
    Ok(())
}

fn validate_workflow_lease_actor(
    store: &company::CompanyStore,
    actor: &str,
    lease: &LeaseToken,
) -> Result<()> {
    let actor_id = directory_agent_id_for_actor(store, actor)?;
    let lease_owner = directory_agent_id_for_actor(store, &lease.worker_id)?;
    anyhow::ensure!(
        actor_id == lease_owner,
        "workflow lease belongs to `{lease_owner}`; `{actor_id}` cannot use another coworker's lease"
    );
    anyhow::ensure!(
        lease.worker_id == lease_owner,
        "workflow lease worker_id must use the immutable agent id `{lease_owner}`"
    );
    Ok(())
}

fn validate_wait_peer(store: &company::CompanyStore, run_id: &str, responder: &str) -> Result<()> {
    let snapshot = store.workflow_snapshot(Some(run_id))?;
    let run = snapshot.runs.first().context("workflow run does not exist")?;
    validate_workflow_ownership(store, &WorkflowOwnership {
        scope: if run.scope == "group" { WorkflowScope::Group } else { WorkflowScope::Company },
        owner_agent_id: responder.into(), group_id: run.group_id.clone(),
    })
}

fn workflow_scheduler_for_node(
    store: &std::sync::Arc<company::CompanyStore>,
    node_id: &str,
) -> Result<DurableWorkflowScheduler> {
    let run_id = store.workflow_node_run_id(node_id)?;
    workflow_scheduler_for_run(store, &run_id)
}

fn append(
    store: &company::CompanyStore,
    context: &WorkContext<'_>,
    node_id: Option<&str>,
    event: CompanyEventKind,
) -> Result<()> {
    store.append(event_input(context, node_id, event))?;
    Ok(())
}

fn append_many(
    store: &company::CompanyStore,
    context: &WorkContext<'_>,
    node_id: Option<&str>,
    events: Vec<CompanyEventKind>,
) -> Result<()> {
    let inputs = events
        .into_iter()
        .map(|event| event_input(context, node_id, event))
        .collect();
    store.append_many(inputs)?;
    Ok(())
}

fn event_input(
    context: &WorkContext<'_>,
    node_id: Option<&str>,
    event: CompanyEventKind,
) -> NewCompanyEvent {
    NewCompanyEvent {
        run_id: context.run_id.to_string(),
        session_id: context.session_id.to_string(),
        pod_id: None,
        work_node_id: node_id.map(str::to_string),
        attempt_id: None,
        agent_identity_id: Some(context.agent.to_string()),
        agent_instance_id: Some(context.agent.to_string()),
        causation_id: None,
        correlation_id: node_id.map(str::to_string),
        idempotency_key: None,
        event,
    }
}

fn bounded_inspect_value(
    mut snapshot: company::CompanySnapshot,
    mut events: Vec<company::CompanyEvent>,
    mut partial: bool,
) -> Result<serde_json::Value> {
    loop {
        let result = serde_json::json!({
            "snapshot": &snapshot,
            "recent_events": &events,
            "partial": partial,
            "limits": {
                "max_result_bytes": MAX_WORK_RESULT_BYTES,
                "max_snapshot_rows_per_kind": MAX_WORK_INSPECT_ROWS_PER_KIND,
                "max_recent_events": MAX_WORK_INSPECT_EVENTS,
            }
        });
        if serde_json::to_string_pretty(&result)?.len() <= MAX_WORK_RESULT_BYTES {
            return Ok(result);
        }
        partial = true;
        if events.pop().is_some() {
            continue;
        }
        if snapshot.work.pop().is_some() {
            continue;
        }
        if snapshot.workflow.edges.pop().is_some() {
            continue;
        }
        if snapshot.workflow.nodes.pop().is_some() {
            continue;
        }
        if snapshot.workflow.runs.pop().is_some() {
            continue;
        }
        if snapshot.workflow.goals.pop().is_some() {
            continue;
        }
        if snapshot.jobs.pop().is_some() {
            snapshot.active_jobs = snapshot
                .jobs
                .iter()
                .filter(|job| is_active(&job.state))
                .count();
            snapshot.stale_jobs = snapshot
                .jobs
                .iter()
                .filter(|job| job.state == company::AgentState::Stale)
                .count();
            continue;
        }
        bail!("minimal work inspection exceeds the {MAX_WORK_RESULT_BYTES}-byte limit");
    }
}

fn is_active(state: &company::AgentState) -> bool {
    matches!(
        state,
        company::AgentState::Queued
            | company::AgentState::Starting
            | company::AgentState::Reasoning
            | company::AgentState::UsingTool
            | company::AgentState::WaitingPeer
            | company::AgentState::WaitingUser
            | company::AgentState::Reviewing
            | company::AgentState::Integrating
    )
}

fn required(value: Option<String>, name: &str) -> Result<String> {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => Ok(value),
        None => bail!("{name} is required"),
    }
}

fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_workflow_contract_rejects_unknown_controls_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let store = company::global().unwrap();
        store.ensure_full_catalog_team().unwrap();
        let requirement = serde_json::json!({
            "kind":"artifact_check", "description":"Exercise the saved result",
            "minimum_receipts":1, "required":true
        });
        let contract = serde_json::json!({
            "title":"Contract validation", "objective":"Retain required proof", "budget":{},
            "evidence_requirements":[requirement], "concurrency":ConcurrencyPolicy::default()
        });
        let declaration = |action: &str, contract: serde_json::Value| {
            let mut payload = serde_json::json!({"idempotency_key":"strict-contract", "contract":contract});
            if action == "install_plan" {
                payload["assignments"] = serde_json::json!([
                    {"key":"build", "owner_agent_id":"coder", "title":"Build", "outcome":"Verified result"}
                ]);
            }
            input(serde_json::json!({"action":"workflow", "workflow_action":action, "workflow_payload":payload}))
        };
        let before = store.workflow_snapshot(None).unwrap().as_of_seq;
        for action in ["install_plan", "create_goal"] {
            for (key, expected_path) in [
                ("evidence_requirement", "workflow_payload.contract"),
                ("concurency", "workflow_payload.contract"),
                ("max_workers", "workflow_payload.contract.concurrency"),
            ] {
                let mut invalid = contract.clone();
                match key {
                    "evidence_requirement" => {
                        let proof = invalid.as_object_mut().unwrap().remove("evidence_requirements").unwrap();
                        invalid[key] = proof;
                    }
                    "concurency" => {
                        let limits = invalid.as_object_mut().unwrap().remove("concurrency").unwrap();
                        invalid[key] = limits;
                    }
                    _ => invalid["concurrency"][key] = serde_json::json!(1),
                }
                let error = execute(dir.path(), declaration(action, invalid), Some("agent-coder"), Some("coder"))
                    .unwrap_err();
                let error = format!("{error:#}");
                assert!(error.contains(key) && error.contains(expected_path), "{error}");
                assert_eq!(store.workflow_snapshot(None).unwrap().as_of_seq, before,
                    "invalid {action} must not install any part of a weaker contract");
            }
        }

        let output = execute(dir.path(), declaration("install_plan", contract), Some("agent-coder"), Some("coder")).unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&output.content).unwrap();
        let snapshot = store.workflow_snapshot(receipt["run_id"].as_str()).unwrap();
        let saved: DurableWorkflowContract = serde_json::from_str(&snapshot.goals[0].contract_json).unwrap();
        assert_eq!(saved.evidence_requirements.len(), 1);
        assert_eq!(saved.evidence_requirements[0].kind, "artifact_check");
        assert!(saved.evidence_requirements[0].required);
        assert_eq!(saved.concurrency, ConcurrencyPolicy::default());
        assert_eq!(snapshot.nodes[0].evidence_requirements_json,
            serde_json::to_string(&saved.evidence_requirements).unwrap());

        let canonical = serde_json::to_value(&saved).unwrap();
        let round_trip: GoalRequest = workflow_contract_payload(Some(serde_json::json!({"contract":canonical.clone()}))).unwrap();
        assert_eq!(serde_json::to_value(&round_trip.contract).unwrap(), canonical);
        let mut legacy = canonical;
        legacy["legacy_annotation"] = serde_json::json!("stored by an older version");
        legacy["concurrency"]["legacy_annotation"] = serde_json::json!(true);
        let restored: DurableWorkflowContract = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.evidence_requirements, saved.evidence_requirements);
        assert_eq!(restored.concurrency, saved.concurrency);
    }

    #[test]
    fn install_plan_tool_returns_compact_ids_and_uses_caller_scope() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let store = company::global().unwrap();
        store.ensure_full_catalog_team().unwrap();
        let payload = serde_json::json!({"action":"workflow","workflow_action":"install_plan",
            "workflow_payload":{"idempotency_key":"tool-plan-fixture",
                "contract":{"title":"Source review","objective":"Review one brief","budget":{}},
                "assignments":[{"key":"brief","owner_agent_id":"researcher","title":"Brief","outcome":"Cited finding"},
                    {"key":"review","owner_agent_id":"coder","title":"Review","outcome":"Verdict","dependencies":["brief"]}]}});
        let output = execute(dir.path(), input(payload.clone()), Some("agent-coder"), Some("coder")).unwrap();
        let result: serde_json::Value = serde_json::from_str(&output.content[output.content.find('{').unwrap()..]).unwrap();
        assert!(result.get("workflow").is_none(), "setup receipt must not dump the graph back into context");
        let run = result["run_id"].as_str().unwrap();
        let snapshot = store.workflow_snapshot(Some(run)).unwrap();
        assert_eq!(snapshot.runs[0].owner_agent_id, "coder");
        assert_eq!(snapshot.runs[0].scope, "agent");
        assert_eq!(snapshot.nodes.len(), 2);
        let count = store.events_since(0, 100).unwrap().len();
        execute(dir.path(), input(payload), Some("agent-coder"), Some("coder")).unwrap();
        assert_eq!(store.events_since(0, 100).unwrap().len(), count);
    }

    fn input(value: serde_json::Value) -> WorkInput {
        serde_json::from_value(value).expect("valid work test input")
    }

    #[test]
    fn normalizes_only_unambiguous_legacy_work_aliases() {
        let mut claim = input(serde_json::json!({
            "action": "claim",
            "target_id": "work_123",
            "approach": "own the outcome"
        }));
        normalize_obvious_aliases(&mut claim);
        assert_eq!(claim.node_id.as_deref(), Some("work_123"));
        assert!(claim.target_id.is_none());

        let mut note = input(serde_json::json!({
            "action": "note",
            "statement": "verified status"
        }));
        normalize_obvious_aliases(&mut note);
        assert_eq!(note.reason.as_deref(), Some("verified status"));
        assert!(note.statement.is_none());

        let mut challenge = input(serde_json::json!({
            "action": "challenge",
            "target_id": "claim_123",
            "claim": "evidence conflicts"
        }));
        normalize_obvious_aliases(&mut challenge);
        assert!(challenge.node_id.is_none());
        assert_eq!(challenge.target_id.as_deref(), Some("claim_123"));
    }
}
