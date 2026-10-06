//! Runtime-owned goal setup for substantial execution. This reuses the existing
//! workflow authority; it does not create a second scheduler or launch workers.
use super::workflow::{DurableWorkflowScheduler, WorkflowPlanReceipt, WorkflowPlanRequest};
use anyhow::Result;
use sha2::{Digest, Sha256};

/// A short continuation selects existing conversation state; it must never
/// create a new goal or unpause a selected one merely to answer.
pub(crate) fn recover_for_continuation(session_id: &str, actor: &str, request: &str) -> Result<Option<WorkflowPlanReceipt>> {
    if !is_continuation(request) { return Ok(None); }
    let store = super::company::global()?;
    recover_in_store(&store, session_id, actor)
}

fn is_continuation(request: &str) -> bool {
    let request = request.trim().trim_end_matches(['.', '!']).to_ascii_lowercase();
    matches!(request.as_str(), "continue" | "continue working" | "please continue" | "you can continue" | "continue please" | "keep going" | "carry on" | "resume" | "go on")
}

fn recover_in_store(store: &super::company::CompanyStore, session_id: &str, actor: &str) -> Result<Option<WorkflowPlanReceipt>> {
    let actor = if actor.eq_ignore_ascii_case("orchestrator") { "phoenix".into() } else { actor.to_ascii_lowercase() };
    let (owner, snapshot) = store.workflow_context(session_id, &actor)?;
    for run in &snapshot.runs {
        let Some(goal) = snapshot.goals.iter().find(|g| g.goal_id == run.goal_id) else { continue; };
        let contract: serde_json::Value = serde_json::from_str(&goal.contract_json)?;
        if run.owner_agent_id == owner && contract["metadata"]["default_goal_session"].as_str() == Some(session_id) {
            return Ok(Some(WorkflowPlanReceipt { goal_id: goal.goal_id.clone(), run_id: run.run_id.clone(),
                assignments: snapshot.nodes.iter().filter(|n| n.run_id == run.run_id)
                    .map(|n| (n.node_id.clone(), n.node_id.clone())).collect() }));
        }
    }
    Ok(None)
}

pub(crate) fn ensure(
    session_id: &str,
    actor: &str,
    operation_id: &str,
    objective: &str,
    group_id: Option<&str>,
) -> Result<WorkflowPlanReceipt> {
    let store = super::company::global()?;
    ensure_in_store(store, session_id, actor, operation_id, objective, group_id)
}

fn ensure_in_store(
    store: std::sync::Arc<super::company::CompanyStore>,
    session_id: &str,
    actor: &str,
    operation_id: &str,
    objective: &str,
    group_id: Option<&str>,
) -> Result<WorkflowPlanReceipt> {
    let actor = if actor.eq_ignore_ascii_case("orchestrator") {
        "phoenix".into()
    } else {
        actor.to_ascii_lowercase()
    };
    let (owner, snapshot) = store.workflow_context(session_id, &actor)?;
    for run in &snapshot.runs {
        let Some(goal) = snapshot.goals.iter().find(|g| g.goal_id == run.goal_id) else {
            continue;
        };
        let contract: serde_json::Value = serde_json::from_str(&goal.contract_json)?;
        if run.owner_agent_id == owner
            && (contract["metadata"]["default_goal_session"].as_str() == Some(session_id))
        {
            let saved: super::workflow::DurableWorkflowContract =
                serde_json::from_str(&goal.contract_json)?;
            DurableWorkflowScheduler::new(store.clone(), saved.concurrency)?
                .reconcile(&run.run_id, chrono::Utc::now())?;
            return Ok(WorkflowPlanReceipt {
                goal_id: goal.goal_id.clone(),
                run_id: run.run_id.clone(),
                assignments: snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.run_id == run.run_id)
                    .map(|n| (n.node_id.clone(), n.node_id.clone()))
                    .collect(),
            });
        }
    }
    let key = format!(
        "default-goal:{:x}",
        Sha256::digest(format!(
            "{session_id}\0{owner}\0{operation_id}\0{objective}"
        ))
    );
    let ownership = if let Some(group) = group_id {
        serde_json::json!({"scope":"group","owner_agent_id":owner,"group_id":group})
    } else {
        serde_json::json!({"scope":"agent","owner_agent_id":owner})
    };
    let request: WorkflowPlanRequest = serde_json::from_value(serde_json::json!({
        "idempotency_key":key,
        "contract":{
            "title":objective.chars().take(100).collect::<String>(), "objective":objective,
            "budget":{}, "ownership":ownership,
            "metadata":{"default_goal_session":session_id,"created_by":"runtime"},
            "evidence_requirements":[{"kind":"outcome_acceptance","description":"Verify the complete user outcome against the original request and amendments; a passed tool or repair alone is insufficient.","minimum_receipts":1,"required":true}]
        },
        "assignments":[{"key":"outcome","owner_agent_id":owner,"title":"Complete and verify requested outcome","outcome":objective}]
    }))?;
    let scheduler = DurableWorkflowScheduler::new(store, request.contract.concurrency.clone())?;
    let receipt = scheduler.install_plan(request)?;
    // Setup must leave an actionable persisted assignment, not merely an
    // inert declaration. Reconciliation does not claim or launch a worker.
    scheduler.reconcile(&receipt.run_id, chrono::Utc::now())?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_goal_is_durable_reused_and_scoped_to_its_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let _env = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let store =
            std::sync::Arc::new(crate::runtime::company::open_current_home_store().unwrap());
        store.ensure_full_catalog_team().unwrap();
        let first = super::ensure_in_store(
            store.clone(),
            "goal-room-a",
            "coder",
            "request-1",
            "Create the detailed banana",
            None,
        )
        .unwrap();
        let again = super::ensure_in_store(
            store.clone(),
            "goal-room-a",
            "coder",
            "request-2",
            "Continue refining the peel",
            None,
        )
        .unwrap();
        assert_eq!(
            first.run_id, again.run_id,
            "amendment must retain the original goal"
        );
        let other = super::ensure_in_store(
            store.clone(),
            "goal-room-b",
            "coder",
            "request-3",
            "Create the detailed banana",
            None,
        )
        .unwrap();
        assert_ne!(
            first.run_id, other.run_id,
            "unrelated conversations must not share a goal"
        );
        let (_, scoped) = store.workflow_context("goal-room-a", "coder").unwrap();
        assert_eq!(scoped.runs.len(), 1);
        assert_eq!(scoped.runs[0].run_id, first.run_id);
        let view = store.workflow_snapshot(Some(&first.run_id)).unwrap();
        assert_eq!(view.goals[0].objective, "Create the detailed banana");
        assert_eq!(view.runs[0].owner_agent_id, "coder");
        assert_eq!(view.runs[0].scope, "agent");
        assert_eq!(view.runs[0].state, "active");
        assert_eq!(view.nodes.len(), 1);
        assert_eq!(view.nodes[0].state, "ready");
        assert!(
            view.nodes[0].lease_id.is_none(),
            "setup must not launch or claim another worker"
        );
        assert!(view.nodes[0]
            .evidence_requirements_json
            .contains("outcome_acceptance"));
        assert_ne!(
            view.nodes[0].state, "succeeded",
            "creation is not completion"
        );
        assert!(super::is_continuation("Continue!"));
        assert_eq!(super::recover_in_store(&store, "goal-room-a", "coder").unwrap().unwrap().run_id, first.run_id);
        assert_eq!(super::recover_in_store(&store, "goal-room-b", "coder").unwrap().unwrap().run_id, other.run_id);
        assert!(super::recover_in_store(&store, "empty-room", "coder").unwrap().is_none(), "recovery never creates a new outcome");
        assert!(super::recover_in_store(&store, "goal-room-a", "researcher").unwrap().is_none(), "another actor cannot select the owner's goal");
        assert_eq!(store.workflow_snapshot(Some(&first.run_id)).unwrap().as_of_seq, view.as_of_seq, "recovery is read-only and cannot unpause or claim work");
        assert!(super::recover_for_continuation("goal-room-a", "coder", "What is the status?").unwrap().is_none(), "status is not execution");
        assert!(super::recover_for_continuation("goal-room-a", "coder", "Explain how to continue a loop").unwrap().is_none(), "mentioning continuation does not select old work");
        let reopened = crate::runtime::company::open_current_home_store().unwrap();
        assert_eq!(
            reopened
                .workflow_snapshot(Some(&first.run_id))
                .unwrap()
                .goals[0]
                .objective,
            view.goals[0].objective
        );
    }
}
