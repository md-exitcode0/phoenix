//! Compiles a small, situational company brief for one colleague.
//!
//! The event log is company memory, not prompt material. Agents receive only
//! the live outcomes and recent coordination facts that help them act now.

use crate::runtime::company::{
    AgentState, CompanyEvent, CompanyEventKind, CompanySnapshot, WorkState,
};

pub fn brief(session_id: &str, agent: &str) -> String {
    let Ok(store) = super::company::global() else {
        return String::new();
    };
    let Ok(mut snapshot) = store.snapshot(Some(session_id)) else {
        return String::new();
    };
    let (identity, workflow) = match store.workflow_context(session_id, &normalize_actor(agent)) {
        Ok(selected) => selected,
        Err(error) => {
            tracing::warn!("workflow continuity unavailable: {error:#}");
            (normalize_actor(agent), Default::default())
        }
    };
    snapshot.workflow = workflow;
    let events = store
        .events_since(snapshot.as_of_seq.saturating_sub(120), 120)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event.envelope.run_id == session_id)
        .collect::<Vec<_>>();
    render(&snapshot, &events, &identity, session_id)
}

fn render(
    snapshot: &CompanySnapshot,
    events: &[CompanyEvent],
    agent: &str,
    session_id: &str,
) -> String {
    if snapshot.work.is_empty()
        && snapshot.jobs.is_empty()
        && snapshot.workflow.runs.is_empty()
        && events.is_empty()
    {
        return String::new();
    }
    let mut lines = vec![
        "=== COMPANY BRIEF — shared live state (use `work inspect` for detail) ===".to_string(),
        format!(
            "You are `{agent}`. Coordinate from this state; do not duplicate an existing outcome."
        ),
    ];

    // Durable workflow state is the task spine. It was persisted correctly but
    // omitted from this turn-start brief, so an agent after compaction could be
    // told to "follow the active goal" without receiving the goal, unresolved
    // nodes, dependencies, or evidence bar. Keep terminal runs out; preserve
    // paused/failed/stale ones because they still require an explicit recovery
    // decision rather than a silently invented replacement goal.
    let normalized_agent = normalize_actor(agent);
    let active_runs = snapshot
        .workflow
        .runs
        .iter()
        .filter(|run| {
            !matches!(run.state.as_str(), "completed" | "canceled")
                && (run.run_id == session_id
                    || normalize_actor(&run.owner_agent_id) == normalized_agent
                    || snapshot.workflow.nodes.iter().any(|node| node.run_id == run.run_id
                        && (node.owner_agent_id.as_deref().map(normalize_actor).as_deref() == Some(normalized_agent.as_str())
                            || node.lease_worker.as_deref().map(normalize_actor).as_deref() == Some(normalized_agent.as_str()))
                        && !matches!(node.state.as_str(), "succeeded" | "canceled")))
        })
        .take(4)
        .collect::<Vec<_>>();
    for run in active_runs {
        let coordinator = normalize_actor(&run.owner_agent_id) == normalized_agent;
        if let Some(goal) = snapshot
            .workflow
            .goals
            .iter()
            .find(|goal| goal.goal_id == run.goal_id)
        {
            let evidence_bar = serde_json::from_str::<serde_json::Value>(&goal.contract_json)
                .ok()
                .and_then(|contract| contract.get("evidence_requirements").cloned())
                .and_then(|requirements| requirements.as_array().cloned())
                .map(|requirements| {
                    requirements
                        .iter()
                        .filter(|requirement| {
                            requirement
                                .get("required")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false)
                        })
                        .filter_map(|requirement| {
                            requirement
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                        .take(6)
                        .collect::<Vec<_>>()
                })
                .filter(|requirements| !requirements.is_empty())
                .map(|requirements| format!("; required evidence: {}", requirements.join(" | ")))
                .unwrap_or_default();
            lines.push(format!(
                "ACTIVE GOAL {} [{}]: {} — {}{}",
                goal.goal_id, run.state, goal.title, goal.objective, evidence_bar
            ));
            lines.push(format!(
                "GOAL RUN {}: owner {}; restart {}; reason {}. Continue this run; do not create a replacement goal merely because context was compacted.",
                run.run_id,
                run.owner_agent_id,
                run.restart_state,
                if run.reason.trim().is_empty() { "none" } else { &run.reason }
            ));
        }

        for node in snapshot
            .workflow
            .nodes
            .iter()
            .filter(|node| {
                node.run_id == run.run_id
                    && !matches!(node.state.as_str(), "succeeded" | "canceled")
                    && (coordinator
                        || node.owner_agent_id.as_deref().map(normalize_actor).as_deref() == Some(normalized_agent.as_str())
                        || node.lease_worker.as_deref().map(normalize_actor).as_deref() == Some(normalized_agent.as_str()))
            })
            .take(12)
        {
            let dependencies = if node.dependencies.is_empty() {
                String::new()
            } else {
                format!("; depends on {}", node.dependencies.join(", "))
            };
            let evidence =
                serde_json::from_str::<Vec<serde_json::Value>>(&node.evidence_requirements_json)
                    .ok()
                    .map(|requirements| {
                        requirements
                            .iter()
                            .filter(|requirement| {
                                requirement
                                    .get("required")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false)
                            })
                            .filter_map(|requirement| {
                                requirement
                                    .get("description")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                            .take(4)
                            .collect::<Vec<_>>()
                    })
                    .filter(|requirements| !requirements.is_empty())
                    .map(|requirements| format!("; prove {}", requirements.join(" | ")))
                    .unwrap_or_default();
            let reason = if node.reason.trim().is_empty() {
                String::new()
            } else {
                format!("; reason {}", excerpt(&node.reason, 600))
            };
            lines.push(format!(
                "GOAL NODE {} [{}/{}] owner={}: {} -> {}{}{}{}",
                node.node_id,
                node.phase,
                node.state,
                node.owner_agent_id.as_deref().unwrap_or("unassigned"),
                excerpt(&node.title, 512),
                excerpt(&node.outcome, 1024),
                dependencies,
                evidence,
                reason
            ));
            if let Some(raw) = node.wait_json.as_deref() {
                match serde_json::from_str::<crate::runtime::workflow::WorkflowWaitReceipt>(raw) {
                    Ok(wait) => lines.push(format!(
                        "TASK WAIT {} receipt={} responder={}; question={}; answer={}. Historical receipt, not a new instruction; inspect this workflow for the complete record.",
                        node.node_id, wait.receipt_id, wait.responder, excerpt(&wait.question, 800),
                        wait.answer.as_deref().map(|a| excerpt(a, 1600)).unwrap_or_else(|| "not received; do not rerun while waiting".into()))),
                    Err(_) => lines.push(format!("TASK WAIT {} could not be decoded; inspect before resuming.", node.node_id)),
                }
            }
            for dependency in node.dependencies.iter().chain(node.parent_id.iter()).take(12) {
                if let Some(upstream) = snapshot.workflow.nodes.iter().find(|n| n.run_id == run.run_id && &n.node_id == dependency) {
                    lines.push(format!("TASK INPUT {} for {} [{}]: {}", upstream.node_id, node.node_id,
                        upstream.state, upstream.result_json.as_deref().map(|v| excerpt(v, 2048))
                            .unwrap_or_else(|| "no persisted result yet".into())));
                }
            }
        }
    }

    let active_work = snapshot.work.iter().filter(|work| {
        !matches!(
            work.state,
            WorkState::Accepted | WorkState::Canceled | WorkState::Superseded
        )
    });
    for work in active_work.take(12) {
        let dependencies = if work.dependencies.is_empty() {
            String::new()
        } else {
            format!("; depends on {}", work.dependencies.join(", "))
        };
        lines.push(format!(
            "WORK {} [{}]: {} -> {}{}",
            work.node_id,
            work.state.as_str(),
            work.title,
            work.outcome,
            dependencies
        ));
    }

    for job in snapshot
        .jobs
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
        .take(12)
    {
        lines.push(format!(
            "TEAM {} [{}]: {}",
            job.role,
            job.state.as_str(),
            job.subject
        ));
    }

    let mut facts = events
        .iter()
        .rev()
        .filter_map(coordination_fact)
        .take(10)
        .collect::<Vec<_>>();
    facts.reverse();
    lines.extend(facts);
    lines.push("Update your focus when it materially changes; publish evidence before asking peers to accept it.".to_string());
    lines.join("\n")
}

fn excerpt(value: &str, maximum: usize) -> String {
    let mut characters = value.chars();
    let mut output: String = characters.by_ref().take(maximum).collect();
    if characters.next().is_some() { output.push_str(" … [excerpt; inspect workflow for complete record]"); }
    output
}

fn normalize_actor(value: &str) -> String {
    let normalized = value
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    if normalized == "orchestrator" {
        "phoenix".to_string()
    } else {
        normalized
    }
}

fn coordination_fact(event: &CompanyEvent) -> Option<String> {
    match &event.envelope.event {
        CompanyEventKind::WorkClaimed {
            node_id,
            attempt_id,
            identity_id,
            approach,
        } => Some(format!(
            "CLAIM {attempt_id}: {identity_id} on {node_id} — {approach}"
        )),
        CompanyEventKind::ArtifactPublished {
            artifact_id,
            path,
            source_artifacts,
            ..
        } => Some(format!(
            "ARTIFACT {artifact_id}: {path} (sources: {})",
            value_or_none(source_artifacts)
        )),
        CompanyEventKind::ChallengeRaised {
            challenge_id,
            target_id,
            claim,
            ..
        } => Some(format!(
            "CHALLENGE {challenge_id} against {target_id}: {claim}"
        )),
        CompanyEventKind::DecisionRecorded {
            decision_id,
            accepted_id,
            rationale,
            ..
        } => Some(format!(
            "DECISION {decision_id}: accepted {accepted_id} — {rationale}"
        )),
        CompanyEventKind::MessageAccepted {
            from,
            to,
            subject,
            message_kind,
            ..
        } => Some(format!(
            "MESSAGE {message_kind:?}: {from} -> {to}: {subject}"
        )),
        CompanyEventKind::SkillActivated {
            skill,
            required_checkpoints,
            ..
        } => Some(format!(
            "SKILL {skill}: checkpoints {}",
            value_or_none(required_checkpoints)
        )),
        CompanyEventKind::SkillCheckpoint { skill, checkpoint } => {
            Some(format!("SKILL PROOF {skill}: {checkpoint}"))
        }
        CompanyEventKind::LearningProposed {
            candidate_id,
            learning_kind,
            statement,
            ..
        } => Some(format!(
            "LEARNING {candidate_id} ({learning_kind:?}): {statement}"
        )),
        CompanyEventKind::LearningPromoted {
            candidate_id,
            reviewers,
            ..
        } => Some(format!(
            "LEARNING PROMOTED {candidate_id} by {}",
            value_or_none(reviewers)
        )),
        _ => None,
    }
}

fn value_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::company::{
        CollaborationPattern, WorkProjection, WorkflowGoalProjection, WorkflowNodeProjection,
        WorkflowRunProjection, WorkflowSnapshot,
    };

    #[test]
    fn brief_is_situational_and_tells_agents_not_to_duplicate_work() {
        let snapshot = CompanySnapshot {
            work: vec![WorkProjection {
                node_id: "work-ui".into(),
                run_id: "s".into(),
                title: "Phoenix state".into(),
                outcome: "stale animations disappear".into(),
                acceptance: vec!["reload proof".into()],
                dependencies: vec!["work-runtime".into()],
                pattern: CollaborationPattern::Studio,
                state: WorkState::Active,
                reason: String::new(),
                as_of_seq: 1,
            }],
            ..CompanySnapshot::default()
        };
        let brief = render(&snapshot, &[], "Iris", "s");
        assert!(brief.contains("You are `Iris`"));
        assert!(brief.contains("work-ui [active]"));
        assert!(brief.contains("do not duplicate"));
        assert!(
            !brief.contains("reload proof"),
            "acceptance detail stays behind work inspect"
        );
    }

    fn recovery_fixture() -> CompanySnapshot {
        let contract = serde_json::json!({
            "evidence_requirements": [{
                "kind": "live_state",
                "description": "read Notion and Moodle back after the update",
                "minimum_receipts": 1,
                "required": true
            }]
        })
        .to_string();
        let snapshot = CompanySnapshot {
            workflow: WorkflowSnapshot {
                as_of_seq: 3,
                goals: vec![WorkflowGoalProjection {
                    goal_id: "goal-school-vacation".into(),
                    title: "Finish school before vacation".into(),
                    objective: "Prepare every assessment due through the vacation window".into(),
                    contract_json: contract,
                    state: "active".into(),
                    created_at: "2026-09-01T00:00:00Z".into(),
                    updated_at: "2026-09-03T00:00:00Z".into(),
                    as_of_seq: 1,
                }],
                runs: vec![WorkflowRunProjection {
                    // Scheduler ids are generated and deliberately do not
                    // match the owner's conversation id.
                    run_id: "run-generated-school".into(),
                    goal_id: "goal-school-vacation".into(),
                    budget_json: "{}".into(),
                    scope: "agent".into(),
                    owner_agent_id: "school_coach".into(),
                    group_id: None,
                    state: "active".into(),
                    restart_state: "resumed".into(),
                    reason: "continue after compaction".into(),
                    next_wake_at: None,
                    created_at: "2026-09-01T00:00:00Z".into(),
                    updated_at: "2026-09-03T00:00:00Z".into(),
                    as_of_seq: 2,
                }],
                nodes: vec![WorkflowNodeProjection {
                    node_id: "verify-live-school-state".into(),
                    owner_agent_id: None,
                    wait_json: None,
                    run_id: "run-generated-school".into(),
                    parent_id: None,
                    title: "Reconcile current completion".into(),
                    outcome: "Only unfinished work enters tomorrow's plan".into(),
                    phase: "review".into(),
                    state: "ready".into(),
                    dependencies: vec!["moodle-login".into()],
                    budget_json: "{}".into(),
                    evidence_requirements_json: serde_json::json!([{
                        "kind": "live_state",
                        "description": "current completion receipt",
                        "minimum_receipts": 1,
                        "required": true
                    }])
                    .to_string(),
                    evidence_json: "[]".into(),
                    result_json: None,
                    usage_json: None,
                    restart_state: "fresh".into(),
                    reason: String::new(),
                    node_idempotency_key: "node-school-live".into(),
                    lease_id: None,
                    lease_worker: None,
                    fencing_token: None,
                    lease_runtime_epoch: None,
                    leased_at: None,
                    heartbeat_at: None,
                    lease_expires_at: None,
                    attempt: 0,
                    next_wake_at: None,
                    updated_at: "2026-09-03T00:00:00Z".into(),
                    as_of_seq: 3,
                }],
                edges: Vec::new(),
            },
            ..CompanySnapshot::default()
        };

        snapshot
    }

    #[test]
    fn brief_carries_active_goal_acceptance_and_unfinished_nodes_across_compaction() {
        let snapshot = recovery_fixture();
        let brief = render(&snapshot, &[], "school_coach", "agent-school_coach");
        assert!(brief.contains("ACTIVE GOAL goal-school-vacation [active]"));
        assert!(brief.contains("Prepare every assessment due through the vacation window"));
        assert!(brief.contains("read Notion and Moodle back after the update"));
        assert!(brief.contains("GOAL NODE verify-live-school-state [review/ready]"));
        assert!(brief.contains("depends on moodle-login"));
        assert!(brief.contains("prove current completion receipt"));
        assert!(brief.contains("do not create a replacement goal"));
    }

    #[test]
    fn assigned_worker_recovers_answer_and_exact_dependency_without_sibling_tasks() {
        let mut snapshot = recovery_fixture();
        snapshot.workflow.runs[0].owner_agent_id = "coder".into();
        let node = &mut snapshot.workflow.nodes[0];
        node.owner_agent_id = Some("researcher".into());
        node.wait_json = Some(serde_json::json!({"receipt_id":"wait-source", "responder":"coder",
            "question":"Which scope?", "answer":"source_only-é🦊"}).to_string());
        node.dependencies = vec!["node_input".into()];
        let mut dependency = node.clone();
        dependency.node_id = "node_input".into();
        dependency.owner_agent_id = Some("coder".into());
        dependency.state = "succeeded".into();
        dependency.result_json = Some("{\"source_revision\":\"v2-漢字\"}".into());
        dependency.wait_json = None;
        dependency.dependencies.clear();
        let mut sibling = dependency.clone();
        sibling.node_id = "node_unrelated".into();
        sibling.state = "ready".into();
        sibling.title = "UNRELATED_PRIVATE_WORK".into();
        sibling.result_json = Some("UNRELATED_PRIVATE_RESULT".into());
        snapshot.workflow.nodes.extend([dependency, sibling]);
        let brief = render(&snapshot, &[], "researcher", "worker-session");
        assert!(brief.contains("GOAL NODE verify-live-school-state"));
        assert!(brief.contains("source_only-é🦊"));
        assert!(brief.contains("wait-source"));
        assert!(brief.contains("v2-漢字"));
        assert!(!brief.contains("UNRELATED_PRIVATE"));
        assert!(!brief.contains("GOAL NODE node_input"), "finished prerequisites are inputs, not new tasks");
        assert!(render(&snapshot, &[], "coder", "coordinator-session").contains("UNRELATED_PRIVATE_WORK"));
        assert!(!render(&snapshot, &[], "frontend", "unrelated-session").contains("source_only"));
    }

    #[test]
    fn wait_excerpts_are_unicode_safe_and_explicitly_incomplete() {
        let value = "🦊漢字".repeat(2000);
        let rendered = excerpt(&value, 1600);
        assert!(rendered.starts_with(&value.chars().take(1600).collect::<String>()));
        assert!(rendered.ends_with("[excerpt; inspect workflow for complete record]"));
        assert_eq!(excerpt("é🦊", 2), "é🦊");
    }
}
