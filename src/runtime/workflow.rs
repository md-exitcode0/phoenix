//! Durable workflow contracts and a conservative scheduler.
//!
//! This layer sits on top of CompanyStore. The company event stream remains
//! the source of truth; the scheduler computes readiness, performs fenced
//! claims, and writes restart receipts. It does not recreate the removed
//! goals/oracle/governor modules.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::company::{
    CompanyEventKind, CompanyStore, NewCompanyEvent, WorkflowNodeProjection, WorkflowSnapshot,
};

const MAX_ID_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_NODES_PER_RUN_HARD: usize = 50_000;
const MAX_EDGES_PER_NODE: usize = 256;
const MAX_EVIDENCE_RECEIPTS: usize = 256;
const MAX_METADATA_ENTRIES: usize = 64;
const MAX_METADATA_VALUE_BYTES: usize = 4096;

/// The only phases a durable workflow may use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    Prepare,
    Plan,
    Execute,
    Review,
    Commit,
}

impl WorkflowPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Plan => "plan",
            Self::Execute => "execute",
            Self::Review => "review",
            Self::Commit => "commit",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Prepare => 0,
            Self::Plan => 1,
            Self::Execute => 2,
            Self::Review => 3,
            Self::Commit => 4,
        }
    }
}

/// Durable node state. Leased, running, and review consume worker slots.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeState {
    Pending,
    Ready,
    Leased,
    Running,
    Review,
    WaitingUser,
    WaitingPeer,
    Blocked,
    Succeeded,
    Failed,
    Canceled,
    Stale,
}

impl WorkflowNodeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Leased => "leased",
            Self::Running => "running",
            Self::Review => "review",
            Self::WaitingUser => "waiting_user",
            Self::WaitingPeer => "waiting_peer",
            Self::Blocked => "blocked",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Stale => "stale",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "leased" => Ok(Self::Leased),
            "running" => Ok(Self::Running),
            "review" => Ok(Self::Review),
            "waiting_user" => Ok(Self::WaitingUser),
            "waiting_peer" => Ok(Self::WaitingPeer),
            "blocked" => Ok(Self::Blocked),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            "stale" => Ok(Self::Stale),
            other => bail!("unknown workflow node state {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Planned,
    Active,
    Paused,
    Completed,
    Failed,
    Canceled,
    Stale,
}

impl WorkflowRunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Stale => "stale",
        }
    }
}

/// Restart is explicit so a worker can distinguish a fresh attempt from a
/// stale result that arrived after the owning process died.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartState {
    // Older projections emitted execution states in this separate field.
    // Accept those receipts without making agents spend a corrective turn.
    #[serde(alias = "new", alias = "running")]
    Fresh,
    Recovering,
    Requeued,
    Waiting,
    Terminal,
}

impl RestartState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Recovering => "recovering",
            Self::Requeued => "requeued",
            Self::Waiting => "waiting",
            Self::Terminal => "terminal",
        }
    }
}

/// Explicit per-node and per-run limits. None means that dimension is not
/// charged by this provider; defaults are finite and cannot run forever.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowBudget {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_total_tokens: Option<u64>,
    /// US dollars in micro-units (1_000_000 = $1.00).
    pub max_cost_micros: Option<u64>,
    pub max_wall_seconds: Option<u64>,
    pub max_iterations: Option<u32>,
}

impl Default for WorkflowBudget {
    fn default() -> Self {
        Self {
            max_input_tokens: Some(200_000),
            max_output_tokens: Some(64_000),
            max_total_tokens: Some(264_000),
            max_cost_micros: Some(5_000_000),
            max_wall_seconds: Some(3_600),
            max_iterations: Some(128),
        }
    }
}

impl WorkflowBudget {
    fn validate(&self) -> Result<()> {
        check_limit(self.max_input_tokens, 10_000_000, "max_input_tokens")?;
        check_limit(self.max_output_tokens, 2_000_000, "max_output_tokens")?;
        check_limit(self.max_total_tokens, 12_000_000, "max_total_tokens")?;
        check_limit(self.max_cost_micros, 1_000_000_000_000, "max_cost_micros")?;
        check_limit(self.max_wall_seconds, 7 * 24 * 60 * 60, "max_wall_seconds")?;
        if self
            .max_iterations
            .is_some_and(|value| value == 0 || value > 100_000)
        {
            bail!("max_iterations must be in 1..=100000 when set");
        }
        if let (Some(input), Some(output), Some(total)) = (
            self.max_input_tokens,
            self.max_output_tokens,
            self.max_total_tokens,
        ) {
            if total < input.saturating_add(output) {
                bail!("max_total_tokens must cover input and output limits");
            }
        }
        Ok(())
    }

    fn allows(&self, usage: &WorkflowUsage) -> Result<()> {
        if self
            .max_input_tokens
            .is_some_and(|limit| usage.input_tokens > limit)
        {
            bail!("workflow input-token budget exceeded");
        }
        if self
            .max_output_tokens
            .is_some_and(|limit| usage.output_tokens > limit)
        {
            bail!("workflow output-token budget exceeded");
        }
        if self
            .max_total_tokens
            .is_some_and(|limit| usage.input_tokens.saturating_add(usage.output_tokens) > limit)
        {
            bail!("workflow total-token budget exceeded");
        }
        if self
            .max_cost_micros
            .is_some_and(|limit| usage.cost_micros > limit)
        {
            bail!("workflow cost budget exceeded");
        }
        if self
            .max_wall_seconds
            .is_some_and(|limit| usage.wall_seconds > limit)
        {
            bail!("workflow wall-clock budget exceeded");
        }
        if self
            .max_iterations
            .is_some_and(|limit| usage.iterations > limit)
        {
            bail!("workflow iteration budget exceeded");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WorkflowUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
    pub wall_seconds: u64,
    pub iterations: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRequirement {
    pub kind: String,
    pub description: String,
    pub minimum_receipts: u32,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceReceipt {
    pub receipt_id: String,
    pub kind: String,
    pub summary: String,
    pub uri: Option<String>,
    pub content_hash: Option<String>,
    pub verified: bool,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConcurrencyPolicy {
    /// Queue room for 20-30-agent multi-week plans without unbounded growth.
    pub max_queued_nodes: usize,
    /// Global active leases across all runs.
    pub max_active_workers: usize,
    /// Per-run active leases.
    pub max_active_per_run: usize,
    /// Maximum claims produced by one scheduler tick.
    pub max_claims_per_tick: usize,
    /// Hard per-run graph bound.
    pub max_nodes_per_run: usize,
}

impl Default for ConcurrencyPolicy {
    fn default() -> Self {
        Self {
            max_queued_nodes: 128,
            max_active_workers: 6,
            max_active_per_run: 6,
            max_claims_per_tick: 6,
            max_nodes_per_run: 512,
        }
    }
}

impl ConcurrencyPolicy {
    fn validate(&self) -> Result<()> {
        if self.max_queued_nodes == 0 || self.max_queued_nodes > 10_000 {
            bail!("max_queued_nodes must be in 1..=10000");
        }
        if self.max_active_workers == 0 || self.max_active_workers > 64 {
            bail!("max_active_workers must be in 1..=64");
        }
        if self.max_active_per_run == 0 || self.max_active_per_run > self.max_active_workers {
            bail!("max_active_per_run must be in 1..=max_active_workers");
        }
        if self.max_claims_per_tick == 0 || self.max_claims_per_tick > self.max_active_workers {
            bail!("max_claims_per_tick must be in 1..=max_active_workers");
        }
        if self.max_nodes_per_run == 0 || self.max_nodes_per_run > MAX_NODES_PER_RUN_HARD {
            bail!("max_nodes_per_run is outside the safe graph bound");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableWorkflowContract {
    pub title: String,
    pub objective: String,
    pub budget: WorkflowBudget,
    #[serde(default)]
    pub evidence_requirements: Vec<EvidenceRequirement>,
    #[serde(default)]
    pub concurrency: ConcurrencyPolicy,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub ownership: WorkflowOwnership,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowScope {
    #[serde(alias = "local", alias = "private")]
    Agent,
    Group,
    #[serde(alias = "global", alias = "shared")]
    Company,
}

impl Default for WorkflowScope {
    fn default() -> Self {
        Self::Agent
    }
}

impl WorkflowScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Group => "group",
            Self::Company => "company",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowOwnership {
    #[serde(default)]
    pub scope: WorkflowScope,
    #[serde(default)]
    pub owner_agent_id: String,
    #[serde(default)]
    pub group_id: Option<String>,
}

impl Default for WorkflowOwnership {
    fn default() -> Self {
        // Legacy contracts predate explicit ownership. Phoenix remains the
        // conservative owner until a user deliberately reroutes them.
        Self {
            scope: WorkflowScope::Company,
            owner_agent_id: "phoenix".to_string(),
            group_id: None,
        }
    }
}

impl WorkflowOwnership {
    fn validate(&self) -> Result<()> {
        check_text_bound(
            &self.owner_agent_id,
            "workflow owner",
            MAX_METADATA_VALUE_BYTES,
        )?;
        if self.owner_agent_id.trim().is_empty() {
            bail!("workflow owner_agent_id is required");
        }
        match self.scope {
            WorkflowScope::Group => {
                let group_id = self
                    .group_id
                    .as_deref()
                    .context("group workflow scope requires group_id")?;
                check_text_bound(group_id, "workflow group_id", MAX_METADATA_VALUE_BYTES)?;
            }
            WorkflowScope::Agent | WorkflowScope::Company => {
                if self.group_id.is_some() {
                    bail!("only group-scoped workflows may set group_id");
                }
            }
        }
        Ok(())
    }
}

impl DurableWorkflowContract {
    fn validate(&self) -> Result<()> {
        check_text(&self.title, "workflow title")?;
        check_text(&self.objective, "workflow objective")?;
        self.budget.validate()?;
        self.concurrency.validate()?;
        validate_requirements(&self.evidence_requirements)?;
        if self.metadata.len() > MAX_METADATA_ENTRIES {
            bail!("workflow metadata has too many entries");
        }
        for (key, value) in &self.metadata {
            check_text_bound(key, "workflow metadata key", MAX_METADATA_VALUE_BYTES)?;
            check_text_bound(value, "workflow metadata value", MAX_METADATA_VALUE_BYTES)?;
        }
        self.ownership.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRequest {
    pub goal_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub contract: DurableWorkflowContract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub run_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub goal_id: String,
    #[serde(default)]
    pub budget: WorkflowBudget,
    pub next_wake_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSpec {
    pub node_id: Option<String>,
    /// Explicit assignment; None preserves legacy pooled-worker nodes.
    #[serde(default)]
    pub owner_agent_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub run_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub outcome: String,
    pub phase: WorkflowPhase,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub budget: WorkflowBudget,
    #[serde(default)]
    pub evidence_requirements: Vec<EvidenceRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseToken {
    pub node_id: String,
    pub lease_id: String,
    pub worker_id: String,
    pub fencing_token: String,
}

/// A wait is an addressed, immutable question receipt, not a polling deadline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowWaitReceipt {
    pub receipt_id: String,
    pub responder: String,
    pub question: String,
    #[serde(default)]
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeasedNode {
    pub token: LeaseToken,
    pub phase: WorkflowPhase,
    pub attempt: u32,
    pub leased_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchedulerReport {
    pub recovered: usize,
    pub made_ready: usize,
    pub blocked: usize,
    pub claimed: usize,
    pub completed: usize,
}

#[derive(Clone)]
pub struct DurableWorkflowScheduler {
    store: Arc<CompanyStore>,
    policy: ConcurrencyPolicy,
    lease_ttl: Duration,
}

/// Renews only leases returned by successful workflow ticks in this live
/// execution slice. Dropping the slice aborts all renewal tasks; persisted
/// leases then expire normally for recovery. It never claims or resurrects work.
#[derive(Default)]
pub(crate) struct TurnLeaseKeeper {
    tasks: HashMap<String, (LeaseToken, tokio::task::JoinHandle<()>)>,
}

impl TurnLeaseKeeper {
    pub(crate) fn observe_tool(&mut self, tool: &str, input: &serde_json::Value, output: &str) -> Result<usize> {
        if tool != "work" || input["action"] != "workflow" || input["workflow_action"] != "tick" {
            return Ok(0);
        }
        let Some(store) = crate::runtime::company::global_if_initialized() else { return Ok(0); };
        // Parse only the runtime-produced receipt, never arbitrary model text.
        let json = output.split_once('\n').context("workflow tick receipt has no payload")?.1;
        let receipt: serde_json::Value = serde_json::from_str(json)?;
        let leases: Vec<LeasedNode> = serde_json::from_value(receipt["leased"].clone())?;
        let scheduler = DurableWorkflowScheduler::new(store, ConcurrencyPolicy::default())?;
        let count = leases.len();
        for lease in leases {
            self.track(scheduler.clone(), lease.token);
        }
        Ok(count)
    }

    fn track(&mut self, scheduler: DurableWorkflowScheduler, token: LeaseToken) {
        self.tasks.retain(|_, (_, task)| !task.is_finished());
        if self.tasks.get(&token.node_id).is_some_and(|(current, _)| current == &token) {
            return;
        }
        if let Some((_, previous)) = self.tasks.remove(&token.node_id) {
            previous.abort();
        }
        let owned = token.clone();
        let task = tokio::spawn(async move {
            let delay = scheduler.lease_ttl / 3;
            loop {
                // No immediate heartbeat: tick already returned a fresh lease.
                tokio::time::sleep(delay).await;
                if let Err(error) = scheduler.heartbeat(&owned, Utc::now()) {
                    // A transition, replacement owner, pause, or expiry must
                    // fence this task. Never acquire a replacement token here.
                    tracing::debug!(node_id = %owned.node_id, "turn lease renewal ended: {error:#}");
                    break;
                }
            }
        });
        self.tasks.insert(token.node_id.clone(), (token, task));
    }

    pub(crate) fn stop(&mut self) {
        for (_, (_, task)) in self.tasks.drain() {
            task.abort();
        }
    }
}

impl Drop for TurnLeaseKeeper {
    fn drop(&mut self) { self.stop(); }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlanRequest {
    pub idempotency_key: String,
    pub contract: DurableWorkflowContract,
    pub assignments: Vec<WorkflowPlanAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlanAssignment {
    pub key: String,
    pub owner_agent_id: String,
    pub title: String,
    pub outcome: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowPlanReceipt {
    pub goal_id: String,
    pub run_id: String,
    pub assignments: BTreeMap<String, String>,
}

/// A validated declaration, not a second state authority. Keeping preparation
/// free of writes lets callers compose reservation and workflow events in one
/// CompanyStore transaction instead of publishing half of a group admission.
pub(crate) struct PreparedWorkflowPlan {
    pub(crate) events: Vec<NewCompanyEvent>,
    pub(crate) receipt: WorkflowPlanReceipt,
}

impl DurableWorkflowScheduler {
    /// Publish a validated graph atomically in the existing workflow authority.
    pub fn install_plan(&self, request: WorkflowPlanRequest) -> Result<WorkflowPlanReceipt> {
        let prepared = Self::prepare_plan(request, &self.policy)?;
        self.store.append_many_exact(prepared.events)?;
        Ok(prepared.receipt)
    }

    pub(crate) fn prepare_plan(request: WorkflowPlanRequest, policy: &ConcurrencyPolicy) -> Result<PreparedWorkflowPlan> {
        policy.validate()?;
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        request.contract.validate()?;
        validate_idempotency_key(&request.idempotency_key)?;
        anyhow::ensure!(!request.assignments.is_empty() && request.assignments.len() <= 62
            && request.assignments.len() <= request.contract.concurrency.max_nodes_per_run
            && request.assignments.len() <= request.contract.concurrency.max_queued_nodes
            && request.assignments.len() <= policy.max_nodes_per_run,
            "atomic plan requires 1..=62 assignments within its node budget");
        let goal_id = stable_id("goal", &format!("plan:{}", request.idempotency_key));
        let run_id = stable_id("run", &format!("plan:{}", request.idempotency_key));
        let mut ids = BTreeMap::new();
        for assignment in &request.assignments {
            check_text(&assignment.key, "assignment key")?;
            check_text(&assignment.owner_agent_id, "assignment owner")?;
            check_text(&assignment.title, "assignment title")?;
            check_text(&assignment.outcome, "assignment outcome")?;
            anyhow::ensure!(assignment.dependencies.len() <= MAX_EDGES_PER_NODE, "too many prerequisites");
            let node_id = stable_id("node", &format!("{run_id}:{}", assignment.key));
            anyhow::ensure!(ids.insert(assignment.key.clone(), node_id).is_none(), "duplicate assignment key");
        }
        let mut completed = HashSet::new();
        while completed.len() < ids.len() {
            let before = completed.len();
            for assignment in &request.assignments {
                anyhow::ensure!(assignment.dependencies.iter().all(|key| ids.contains_key(key)), "unknown prerequisite key");
                anyhow::ensure!(assignment.dependencies.iter().collect::<HashSet<_>>().len() == assignment.dependencies.len(),
                    "duplicate prerequisite key");
                if assignment.dependencies.iter().all(|key| completed.contains(key)) {
                    completed.insert(assignment.key.clone());
                }
            }
            anyhow::ensure!(completed.len() > before, "workflow plan contains a dependency cycle");
        }
        let contract = request.contract;
        let mut events = vec![event_input(&goal_id, Some(format!("plan:{run_id}:goal")), None,
            CompanyEventKind::WorkflowGoalCreated { goal_id: goal_id.clone(), title: contract.title.clone(),
                objective: contract.objective.clone(), contract_json: serde_json::to_string(&contract)? }),
            event_input(&run_id, Some(format!("plan:{run_id}:run")), None,
            CompanyEventKind::WorkflowRunOpened { workflow_run_id: run_id.clone(), goal_id: goal_id.clone(),
                budget_json: serde_json::to_string(&contract.budget)?, scope: contract.ownership.scope.as_str().into(),
                owner_agent_id: contract.ownership.owner_agent_id.clone(), group_id: contract.ownership.group_id.clone(),
                restart_state: RestartState::Fresh.as_str().into(), next_wake_at: None })];
        for assignment in request.assignments {
            let node_id = ids[&assignment.key].clone();
            events.push(event_input(&run_id, Some(format!("plan:{node_id}:definition")), Some(node_id.clone()),
                CompanyEventKind::WorkflowNodeDefined { node_id: node_id.clone(), owner_agent_id: Some(assignment.owner_agent_id),
                    title: assignment.title, outcome: assignment.outcome, parent_id: None,
                    phase: WorkflowPhase::Execute.as_str().into(),
                    dependencies: assignment.dependencies.iter().map(|key| ids[key].clone()).collect(),
                    budget_json: serde_json::to_string(&contract.budget)?,
                    evidence_requirements_json: serde_json::to_string(&contract.evidence_requirements)?,
                    node_idempotency_key: format!("plan:{node_id}") }));
        }
        // Bind the whole declaration, including added/removed assignments,
        // rather than only comparing the subset of events present on retry.
        events[0].correlation_id = Some(fingerprint);
        Ok(PreparedWorkflowPlan { events, receipt: WorkflowPlanReceipt { goal_id, run_id, assignments: ids } })
    }

    pub fn new(store: Arc<CompanyStore>, policy: ConcurrencyPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            store,
            policy,
            lease_ttl: Duration::from_secs(90),
        })
    }

    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Result<Self> {
        if lease_ttl < Duration::from_secs(5) || lease_ttl > Duration::from_secs(24 * 60 * 60) {
            bail!("workflow lease TTL must be between 5 seconds and 24 hours");
        }
        self.lease_ttl = lease_ttl;
        Ok(self)
    }

    pub fn store(&self) -> &Arc<CompanyStore> {
        &self.store
    }

    pub fn policy(&self) -> &ConcurrencyPolicy {
        &self.policy
    }

    pub fn create_goal(&self, request: GoalRequest) -> Result<String> {
        request.contract.validate()?;
        let key = request
            .idempotency_key
            .clone()
            .or_else(|| request.goal_id.clone())
            .unwrap_or_else(|| id("request"));
        validate_idempotency_key(&key)?;
        let goal_id = request.goal_id.unwrap_or_else(|| stable_id("goal", &key));
        validate_id(&goal_id, "goal_id", "goal")?;
        let contract_json = serde_json::to_string(&request.contract)?;
        let event = self.store.append(event_input(
            &goal_id,
            Some(format!("workflow:goal:{key}")),
            None,
            CompanyEventKind::WorkflowGoalCreated {
                goal_id: goal_id.clone(),
                title: request.contract.title.clone(),
                objective: request.contract.objective.clone(),
                contract_json,
            },
        ))?;
        match event.envelope.event {
            CompanyEventKind::WorkflowGoalCreated { goal_id, .. } => Ok(goal_id),
            _ => bail!("workflow goal idempotency key belongs to another event"),
        }
    }

    pub fn open_run(&self, request: RunRequest) -> Result<String> {
        validate_id(&request.goal_id, "goal_id", "goal")?;
        request.budget.validate()?;
        let projection = self.store.workflow_snapshot(None)?;
        let goal = projection
            .goals
            .iter()
            .find(|goal| goal.goal_id == request.goal_id)
            .with_context(|| format!("workflow goal `{}` does not exist", request.goal_id))?;
        let contract: DurableWorkflowContract = serde_json::from_str(&goal.contract_json)
            .context("workflow goal contract projection is invalid")?;
        contract.ownership.validate()?;
        let next_wake_at = request
            .next_wake_at
            .as_deref()
            .map(|value| parse_time(value, "next_wake_at").map(|time| time.to_rfc3339()))
            .transpose()?;
        let key = request
            .idempotency_key
            .clone()
            .or_else(|| request.run_id.clone())
            .unwrap_or_else(|| id("request"));
        validate_idempotency_key(&key)?;
        let run_id = request.run_id.unwrap_or_else(|| stable_id("run", &key));
        validate_id(&run_id, "run_id", "run")?;
        let event = self.store.append(event_input(
            &run_id,
            Some(format!("workflow:run:{key}")),
            None,
            CompanyEventKind::WorkflowRunOpened {
                workflow_run_id: run_id.clone(),
                goal_id: request.goal_id,
                budget_json: serde_json::to_string(&request.budget)?,
                scope: contract.ownership.scope.as_str().to_string(),
                owner_agent_id: contract.ownership.owner_agent_id,
                group_id: contract.ownership.group_id,
                restart_state: RestartState::Fresh.as_str().to_string(),
                next_wake_at,
            },
        ))?;
        match event.envelope.event {
            CompanyEventKind::WorkflowRunOpened {
                workflow_run_id, ..
            } => Ok(workflow_run_id),
            _ => bail!("workflow run idempotency key belongs to another event"),
        }
    }

    /// Explicitly resume a paused run, optionally assigning a new accountable
    /// owner. Lifecycle handling never calls this: reassignment is a deliberate
    /// user/agent action that passes through the normal permission gate.
    pub fn resume_or_reroute_run(
        &self,
        run_id: &str,
        actor_agent_id: &str,
        new_owner_agent_id: Option<&str>,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<String> {
        validate_id(run_id, "run_id", "run")?;
        check_text(actor_agent_id, "workflow reroute actor")?;
        check_text(reason, "workflow reroute reason")?;
        validate_idempotency_key(idempotency_key)?;
        if let Some(owner) = new_owner_agent_id {
            check_text(owner, "new workflow owner")?;
        }
        // A committed resume already made the run active. Recover its exact
        // receipt before checking whether a NEW ownership change is allowed.
        self.store.resume_workflow_run(run_id, actor_agent_id, new_owner_agent_id, reason, idempotency_key)
    }

    pub fn define_node(&self, spec: NodeSpec) -> Result<String> {
        validate_id(&spec.run_id, "run_id", "run")?;
        check_text(&spec.title, "node title")?;
        check_text(&spec.outcome, "node outcome")?;
        if let Some(owner) = spec.owner_agent_id.as_deref() {
            check_text(owner, "node owner_agent_id")?;
        }
        spec.budget.validate()?;
        validate_requirements(&spec.evidence_requirements)?;
        if spec.dependencies.len() > MAX_EDGES_PER_NODE {
            bail!("workflow node has too many dependencies");
        }
        let node_key_seed = spec
            .idempotency_key
            .clone()
            .or_else(|| spec.node_id.clone())
            .unwrap_or_else(|| id("request"));
        validate_idempotency_key(&node_key_seed)?;
        let node_id = spec
            .node_id
            .unwrap_or_else(|| stable_id("node", &node_key_seed));
        validate_id(&node_id, "node_id", "node")?;
        if spec.parent_id.as_deref() == Some(node_id.as_str())
            || spec.dependencies.iter().any(|value| value == &node_id)
        {
            bail!("workflow node cannot depend on itself");
        }
        if spec.dependencies.iter().collect::<HashSet<_>>().len() != spec.dependencies.len() {
            bail!("workflow node has duplicate dependencies");
        }
        if let Some(parent_id) = spec.parent_id.as_deref() {
            validate_id(parent_id, "parent_id", "node")?;
        }
        for dependency in &spec.dependencies {
            validate_id(dependency, "dependency", "node")?;
        }
        let node_key = spec.idempotency_key.unwrap_or(node_key_seed);
        let snapshot = self.store.workflow_snapshot(Some(&spec.run_id))?;
        if snapshot.nodes.len() >= self.policy.max_nodes_per_run {
            bail!("workflow run reached its node bound");
        }
        if let Some(existing) = snapshot.nodes.iter().find(|node| node.node_id == node_id) {
            if existing.node_idempotency_key == node_key {
                anyhow::ensure!(existing.owner_agent_id == spec.owner_agent_id,
                    "existing workflow assignment belongs to a different owner");
                return Ok(node_id);
            }
            bail!("workflow node id already belongs to another idempotency key");
        }
        if snapshot.runs.iter().any(|run| run.scope == "group") && spec.owner_agent_id.is_none() {
            bail!("new group workflow assignments require owner_agent_id");
        }
        if workflow_would_cycle(
            &snapshot,
            &node_id,
            spec.parent_id.as_deref(),
            &spec.dependencies,
        ) {
            bail!("workflow parent/dependency edges would create a cycle");
        }
        let event = self.store.append(event_input(
            &spec.run_id,
            Some(format!("workflow:node:{node_key}")),
            Some(node_id.clone()),
            CompanyEventKind::WorkflowNodeDefined {
                node_id: node_id.clone(),
                owner_agent_id: spec.owner_agent_id,
                title: spec.title,
                outcome: spec.outcome,
                parent_id: spec.parent_id,
                phase: spec.phase.as_str().to_string(),
                dependencies: spec.dependencies,
                budget_json: serde_json::to_string(&spec.budget)?,
                evidence_requirements_json: serde_json::to_string(&spec.evidence_requirements)?,
                node_idempotency_key: node_key,
            },
        ))?;
        match event.envelope.event {
            CompanyEventKind::WorkflowNodeDefined { node_id, .. } => Ok(node_id),
            _ => bail!("workflow node idempotency key belongs to another event"),
        }
    }

    /// Recover dead-owner or expired leases and make dependency-ready nodes
    /// visible to the claim path. Safe to call on every daemon wake.
    pub fn reconcile(&self, run_id: &str, now: DateTime<Utc>) -> Result<SchedulerReport> {
        validate_id(run_id, "run_id", "run")?;
        let now_text = now.to_rfc3339();
        let mut report = SchedulerReport::default();
        report.recovered = self.recover_expired(run_id, &now_text)?;

        let snapshot = self.store.workflow_snapshot(Some(run_id))?;
        if snapshot.runs.is_empty() {
            bail!("workflow run does not exist");
        }
        if snapshot.runs[0].next_wake_at.as_deref()
            .map(|wake| parse_time(wake, "run wake time").map(|wake| wake > now))
            .transpose()?.unwrap_or(false) {
            return Ok(report);
        }
        if !matches!(snapshot.runs[0].state.as_str(), "planned" | "active") {
            // Paused and terminal runs are inspectable but inert. In
            // particular, lifecycle-paused work must never silently recover a
            // lease or assign itself to a different coworker.
            return Ok(report);
        }
        let nodes_by_id: HashMap<&str, &WorkflowNodeProjection> = snapshot
            .nodes
            .iter()
            .map(|node| (node.node_id.as_str(), node))
            .collect();
        let successful: HashSet<&str> = snapshot
            .nodes
            .iter()
            .filter(|node| node.state == WorkflowNodeState::Succeeded.as_str())
            .map(|node| node.node_id.as_str())
            .collect();
        let queued = snapshot
            .nodes
            .iter()
            .filter(|node| matches!(node.state.as_str(), "pending" | "ready"))
            .count();
        if queued > self.policy.max_queued_nodes {
            bail!("workflow run exceeds the queued-node bound");
        }
        for node in &snapshot.nodes {
            if !matches!(node.state.as_str(), "pending" | "ready") {
                continue;
            }
            let prerequisites: Vec<&str> = node.dependencies.iter().map(String::as_str)
                .chain(node.parent_id.as_deref()).collect();
            let observed: Vec<(&str, Option<i64>)> = prerequisites.iter()
                .map(|id| (*id, nodes_by_id.get(*id).map(|upstream| upstream.as_of_seq))).collect();
            let failed = prerequisites.iter().find_map(|dependency| {
                nodes_by_id.get(*dependency).filter(|upstream| {
                    matches!(upstream.state.as_str(), "failed" | "canceled" | "blocked" | "stale")
                })
            });
            if let Some(upstream) = failed {
                self.append_node_state(
                    node,
                    WorkflowPhase::from_projection(node)?,
                    WorkflowNodeState::Blocked,
                    RestartState::Waiting,
                    format!("Prerequisite {} is {}; resolve it before this task can start", upstream.node_id, upstream.state),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    true,
                    format!("workflow:block:{}:{}", node.node_id, node.as_of_seq),
                    &observed,
                )?;
                report.blocked += 1;
            } else if node.state == WorkflowNodeState::Pending.as_str()
                && prerequisites.iter().all(|dependency| successful.contains(dependency))
            {
                self.append_node_state(
                    node,
                    WorkflowPhase::from_projection(node)?,
                    WorkflowNodeState::Ready,
                    RestartState::Waiting,
                    "dependencies satisfied".to_string(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    true,
                    format!("workflow:ready:{}", node.node_id),
                    &observed,
                )?;
                report.made_ready += 1;
            }
        }

        let refreshed = self.store.workflow_snapshot(Some(run_id))?;
        let all_succeeded = !refreshed.nodes.is_empty()
            && refreshed
                .nodes
                .iter()
                .all(|node| node.state == WorkflowNodeState::Succeeded.as_str());
        let current_run = &refreshed.runs[0];
        if all_succeeded && current_run.state != WorkflowRunState::Completed.as_str() {
            self.append_run_state(
                run_id,
                WorkflowRunState::Completed,
                RestartState::Terminal,
                "all workflow nodes committed",
                None,
                &format!("workflow:run-complete:{run_id}"),
            )?;
            report.completed = 1;
        } else if !all_succeeded && current_run.state == WorkflowRunState::Planned.as_str() {
            self.append_run_state(
                run_id,
                WorkflowRunState::Active,
                RestartState::Waiting,
                "workflow scheduler active",
                None,
                &format!("workflow:run-active:{run_id}"),
            )?;
        }
        Ok(report)
    }

    /// Reconcile then claim up to the bounded number of workers for one tick.
    pub fn tick(
        &self,
        run_id: &str,
        worker_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(SchedulerReport, Vec<LeasedNode>)> {
        validate_id(run_id, "run_id", "run")?;
        check_text(worker_id, "worker_id")?;
        let mut report = self.reconcile(run_id, now)?;
        let snapshot = self.store.workflow_snapshot(Some(run_id))?;
        if snapshot
            .runs
            .first()
            .is_none_or(|run| run.state != WorkflowRunState::Active.as_str())
        {
            return Ok((report, Vec::new()));
        }
        let mut candidates: Vec<&WorkflowNodeProjection> = snapshot
            .nodes
            .iter()
            .filter(|node| node.state == WorkflowNodeState::Ready.as_str())
            .filter(|node| node.owner_agent_id.as_deref().is_none_or(|owner| owner == worker_id))
            .collect();
        candidates.sort_by_key(|node| {
            (
                WorkflowPhase::from_projection(node)
                    .map(WorkflowPhase::rank)
                    .unwrap_or(u8::MAX),
                node.as_of_seq,
                node.node_id.clone(),
            )
        });
        let mut leased = Vec::new();
        // Count admissions, not attempts: an unavailable first candidate must
        // not consume the slot on every tick and starve independent work.
        // Candidate count remains bounded by max_queued_nodes in reconcile.
        for node in candidates {
            if leased.len() >= self.policy.max_claims_per_tick { break; }
            let Some(lease) = self.claim(node, worker_id, now)? else {
                continue;
            };
            leased.push(lease);
            report.claimed += 1;
        }
        Ok((report, leased))
    }

    pub fn claim(
        &self,
        node: &WorkflowNodeProjection,
        worker_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<LeasedNode>> {
        check_text(worker_id, "worker_id")?;
        let phase = WorkflowPhase::from_projection(node)?;
        if node.owner_agent_id.as_deref().is_some_and(|owner| owner != worker_id) {
            return Ok(None);
        }
        let attempt = node.attempt.saturating_add(1);
        let leased_at = now.to_rfc3339();
        let expires_at = (now + chrono::Duration::from_std(self.lease_ttl)?).to_rfc3339();
        let token = LeaseToken {
            node_id: node.node_id.clone(),
            lease_id: id("lease"),
            worker_id: worker_id.to_string(),
            fencing_token: id("fence"),
        };
        let idempotency_key = format!("workflow:lease:{}:{}", token.node_id, token.lease_id);
        let event = self.store.claim_workflow_node(
            event_input(
                &node.run_id,
                Some(idempotency_key),
                Some(node.node_id.clone()),
                CompanyEventKind::WorkflowNodeLeased {
                    node_id: node.node_id.clone(),
                    lease_id: token.lease_id.clone(),
                    worker_id: token.worker_id.clone(),
                    fencing_token: token.fencing_token.clone(),
                    lease_runtime_epoch: self.store.runtime_epoch().to_string(),
                    leased_at: leased_at.clone(),
                    heartbeat_at: leased_at.clone(),
                    expires_at: expires_at.clone(),
                    attempt,
                },
            ),
            node.as_of_seq,
            self.policy.max_active_workers,
            self.policy.max_active_per_run,
            &leased_at,
        )?;
        let Some(event) = event else {
            return Ok(None);
        };
        match event.envelope.event {
            CompanyEventKind::WorkflowNodeLeased {
                node_id,
                lease_id,
                worker_id,
                fencing_token,
                attempt,
                leased_at,
                heartbeat_at,
                expires_at,
                ..
            } => Ok(Some(LeasedNode {
                token: LeaseToken {
                    node_id,
                    lease_id,
                    worker_id,
                    fencing_token,
                },
                phase,
                attempt,
                leased_at,
                heartbeat_at,
                expires_at,
            })),
            _ => bail!("workflow lease idempotency key belongs to another event"),
        }
    }

    pub fn heartbeat(&self, token: &LeaseToken, now: DateTime<Utc>) -> Result<String> {
        validate_lease_token(token)?;
        let heartbeat_at = now.to_rfc3339();
        let expires_at = (now + chrono::Duration::from_std(self.lease_ttl)?).to_rfc3339();
        self.store.append(event_input(
            &token.node_id,
            Some(format!(
                "workflow:heartbeat:{}:{}:{}",
                token.node_id, token.lease_id, heartbeat_at
            )),
            Some(token.node_id.clone()),
            CompanyEventKind::WorkflowNodeHeartbeat {
                node_id: token.node_id.clone(),
                lease_id: token.lease_id.clone(),
                worker_id: token.worker_id.clone(),
                fencing_token: token.fencing_token.clone(),
                heartbeat_at,
                expires_at: expires_at.clone(),
            },
        ))?;
        Ok(expires_at)
    }

    /// Transition a leased node. Commit is the only path to succeeded and
    /// requires a valid budget receipt and every required evidence kind.
    pub fn transition_node(
        &self,
        node_id: &str,
        idempotency_key: &str,
        lease: Option<&LeaseToken>,
        phase: WorkflowPhase,
        state: WorkflowNodeState,
        restart_state: RestartState,
        reason: impl Into<String>,
        next_wake_at: Option<String>,
        result: Option<serde_json::Value>,
        usage: Option<WorkflowUsage>,
        evidence: Option<Vec<EvidenceReceipt>>,
    ) -> Result<()> {
        validate_id(node_id, "node_id", "node")?;
        validate_idempotency_key(idempotency_key)?;
        if let Some(lease) = lease {
            validate_lease_token(lease)?;
            if lease.node_id != node_id {
                bail!("lease belongs to a different workflow node");
            }
        }
        let next_wake_at = next_wake_at
            .as_deref()
            .map(|value| parse_time(value, "next_wake_at").map(|time| time.to_rfc3339()))
            .transpose()?;
        let snapshot = self.store.workflow_snapshot(None)?;
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .context("workflow node does not exist")?;
        let current = WorkflowNodeState::from_str(&node.state)?;
        if matches!(state, WorkflowNodeState::WaitingUser | WorkflowNodeState::WaitingPeer)
            || (matches!(current, WorkflowNodeState::WaitingUser | WorkflowNodeState::WaitingPeer)
                && state != WorkflowNodeState::Canceled) {
            bail!("waiting assignments require wait_for / resolve_wait with their exact receipt");
        }
        if !valid_transition(current, state) {
            bail!("invalid workflow node transition: {} -> {}. {}", current.as_str(), state.as_str(),
                transition_recovery_hint(current, state));
        }
        if state == WorkflowNodeState::Leased {
            bail!("leased state is scheduler-owned; claim it through workflow tick");
        }
        let current_phase = WorkflowPhase::from_projection(node)?;
        if phase.rank() < current_phase.rank() {
            bail!("workflow phases cannot move backward: {} -> {} for `{node_id}`. Inspect the saved assignment and retain its current phase while repairing or requeuing it.",
                current_phase.as_str(), phase.as_str());
        }
        let parsed_budget: WorkflowBudget = serde_json::from_str(&node.budget_json)
            .context("workflow node budget projection is invalid")?;
        let usage_json = usage.as_ref().map(serde_json::to_string).transpose()?;
        if let Some(usage) = usage.as_ref() {
            parsed_budget.allows(usage)?;
        }
        let evidence_json = evidence
            .as_ref()
            .map(|receipts| -> Result<String> {
                validate_receipts(receipts)?;
                Ok(serde_json::to_string(receipts)?)
            })
            .transpose()?;
        if state == WorkflowNodeState::Succeeded {
            if phase != WorkflowPhase::Commit {
                bail!("a succeeded workflow node must be in Commit phase");
            }
            let requirements: Vec<EvidenceRequirement> =
                serde_json::from_str(&node.evidence_requirements_json)
                    .context("workflow evidence requirements projection is invalid")?;
            let receipts: Vec<EvidenceReceipt> = match evidence.as_ref() {
                Some(receipts) => receipts.clone(),
                None => serde_json::from_str(&node.evidence_json)
                    .context("workflow evidence projection is invalid")?,
            };
            ensure_evidence(&requirements, &receipts)?;
        }
        let clear_lease = matches!(
            state,
            WorkflowNodeState::Ready
                | WorkflowNodeState::Blocked
                | WorkflowNodeState::Succeeded
                | WorkflowNodeState::Failed
                | WorkflowNodeState::Canceled
                | WorkflowNodeState::Stale
        );
        self.append_node_state(
            node,
            phase,
            state,
            restart_state,
            reason.into(),
            next_wake_at,
            result,
            usage_json,
            evidence_json,
            lease.map(|value| value.lease_id.clone()),
            lease.map(|value| value.fencing_token.clone()),
            clear_lease,
            format!("workflow:node-state:{node_id}:{idempotency_key}"),
            &[],
        )
    }

    pub fn record_evidence(&self, token: &LeaseToken, receipt: EvidenceReceipt) -> Result<()> {
        validate_lease_token(token)?;
        validate_receipt(&receipt)?;
        self.store.append(event_input(
            &token.node_id,
            Some(format!(
                "workflow:evidence:{}:{}",
                token.node_id, receipt.receipt_id
            )),
            Some(token.node_id.clone()),
            CompanyEventKind::WorkflowEvidenceRecorded {
                node_id: token.node_id.clone(),
                receipt_json: serde_json::to_string(&receipt)?,
                lease_id: token.lease_id.clone(),
                worker_id: token.worker_id.clone(),
                fencing_token: token.fencing_token.clone(),
            },
        ))?;
        Ok(())
    }

    pub fn wait_for(&self, token: &LeaseToken, receipt: WorkflowWaitReceipt) -> Result<()> {
        validate_lease_token(token)?;
        validate_idempotency_key(&receipt.receipt_id)?;
        check_text_bound(&receipt.responder, "wait responder", MAX_ID_BYTES)?;
        anyhow::ensure!(!receipt.responder.trim().is_empty(), "wait responder is required");
        check_text(&receipt.question, "wait question")?;
        anyhow::ensure!(receipt.answer.is_none(), "a new wait cannot contain an answer");
        let run_id = self.store.workflow_node_run_id(&token.node_id)?;
        let snapshot = self.store.workflow_snapshot(Some(&run_id))?;
        let node = snapshot.nodes.iter().find(|node| node.node_id == token.node_id)
            .context("workflow assignment disappeared")?;
        let current_wait: Option<WorkflowWaitReceipt> = node.wait_json.as_deref()
            .map(serde_json::from_str).transpose()?;
        if current_wait.as_ref() == Some(&receipt)
            && matches!(node.state.as_str(), "waiting_user" | "waiting_peer") {
            return Ok(());
        }
        anyhow::ensure!(!current_wait.as_ref().is_some_and(|existing|
            existing.receipt_id == receipt.receipt_id && existing.answer.is_some()),
            "answered wait receipt cannot be reused for another wait");
        anyhow::ensure!(matches!(node.state.as_str(), "leased" | "running" | "review")
            && node.lease_worker.as_deref() == Some(token.worker_id.as_str())
            && node.lease_id.as_deref() == Some(token.lease_id.as_str())
            && node.fencing_token.as_deref() == Some(token.fencing_token.as_str()),
            "only the executing assignment can enter a wait");
        let state = if receipt.responder == "user" { "waiting_user" } else { "waiting_peer" };
        self.store.transition_workflow_node(node, event_input(&run_id,
            Some(format!("workflow:wait:{}:{}", node.node_id, receipt.receipt_id)), Some(node.node_id.clone()),
            CompanyEventKind::WorkflowNodeStateChanged {
                node_id: node.node_id.clone(), phase: node.phase.clone(), state: state.into(),
                restart_state: "waiting".into(), reason: format!("Waiting for {}: {}", receipt.responder, receipt.question),
                next_wake_at: None, result_json: None, usage_json: None, evidence_json: None,
                lease_id: Some(token.lease_id.clone()), fencing_token: Some(token.fencing_token.clone()),
                clear_lease: true, wait_json: Some(serde_json::to_string(&receipt)?),
            }))?;
        Ok(())
    }

    /// The caller supplies an authenticated responder identity, never a model-
    /// provided impersonation. The revision fence prevents an old answer from
    /// reviving canceled work or replacing a newer question.
    pub fn resolve_wait(&self, node_id: &str, receipt_id: &str, responder: &str, answer: &str) -> Result<bool> {
        validate_id(node_id, "node_id", "node")?;
        validate_idempotency_key(receipt_id)?;
        check_text(answer, "wait answer")?;
        let run_id = self.store.workflow_node_run_id(node_id)?;
        let snapshot = self.store.workflow_snapshot(Some(&run_id))?;
        let node = snapshot.nodes.iter().find(|node| node.node_id == node_id)
            .context("workflow assignment disappeared")?;
        let mut receipt: WorkflowWaitReceipt = serde_json::from_str(node.wait_json.as_deref()
            .context("assignment has no matching wait")?)?;
        anyhow::ensure!(receipt.receipt_id == receipt_id && receipt.responder == responder,
            "answer does not match the waiting receipt and responder");
        if let Some(existing) = receipt.answer.as_deref() {
            anyhow::ensure!(existing == answer, "answered receipt cannot be replaced");
            return Ok(false);
        }
        anyhow::ensure!(matches!(node.state.as_str(), "waiting_user" | "waiting_peer"),
            "assignment is not waiting for this answer");
        receipt.answer = Some(answer.into());
        self.store.transition_workflow_node(node, event_input(&run_id,
            Some(format!("workflow:answer:{node_id}:{receipt_id}")), Some(node_id.into()),
            CompanyEventKind::WorkflowNodeStateChanged {
                node_id: node_id.into(), phase: node.phase.clone(), state: "ready".into(),
                restart_state: "requeued".into(), reason: format!("Answer received from {responder}"),
                next_wake_at: None, result_json: None, usage_json: None, evidence_json: None,
                lease_id: None, fencing_token: None, clear_lease: true,
                wait_json: Some(serde_json::to_string(&receipt)?),
            }))?;
        Ok(true)
    }

    pub fn projection(&self, run_id: Option<&str>) -> Result<WorkflowSnapshot> {
        self.store.workflow_snapshot(run_id)
    }

    fn recover_expired(&self, run_id: &str, now: &str) -> Result<usize> {
        let snapshot = self.store.workflow_snapshot(Some(run_id))?;
        let now_time = parse_time(now, "recovery time")?;
        if snapshot.runs.first().and_then(|run| run.next_wake_at.as_deref())
            .map(|wake| parse_time(wake, "run wake time").map(|wake| wake > now_time))
            .transpose()?.unwrap_or(false) {
            return Ok(0);
        }
        let mut recovered = 0;
        for node in &snapshot.nodes {
            if !matches!(node.state.as_str(), "leased" | "running" | "review") {
                continue;
            }
            if !self.store.workflow_lease_recoverable(
                node.lease_runtime_epoch.as_deref(), node.lease_expires_at.as_deref(), now,
            )? {
                continue;
            }
            recovered += self.store.recover_workflow_node(node, recovery_event(node, now)?, now)? as usize;
        }
        Ok(recovered)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_node_state(
        &self,
        node: &WorkflowNodeProjection,
        phase: WorkflowPhase,
        state: WorkflowNodeState,
        restart_state: RestartState,
        reason: String,
        next_wake_at: Option<String>,
        result: Option<serde_json::Value>,
        usage_json: Option<String>,
        evidence_json: Option<String>,
        lease_id: Option<String>,
        fencing_token: Option<String>,
        clear_lease: bool,
        idempotency_key: String,
        prerequisites: &[(&str, Option<i64>)],
    ) -> Result<()> {
        self.store.transition_workflow_node_observed(node, event_input(
            &node.run_id,
            Some(idempotency_key),
            Some(node.node_id.clone()),
            CompanyEventKind::WorkflowNodeStateChanged {
                node_id: node.node_id.clone(),
                phase: phase.as_str().to_string(),
                state: state.as_str().to_string(),
                restart_state: restart_state.as_str().to_string(),
                reason,
                next_wake_at,
                result_json: result
                    .map(|value| serde_json::to_string(&value))
                    .transpose()?,
                usage_json,
                evidence_json,
                lease_id,
                fencing_token,
                clear_lease,
                wait_json: None,
            },
        ), prerequisites)?;
        Ok(())
    }

    fn append_run_state(
        &self,
        run_id: &str,
        state: WorkflowRunState,
        restart_state: RestartState,
        reason: &str,
        next_wake_at: Option<String>,
        idempotency_key: &str,
    ) -> Result<()> {
        self.store.append(event_input(
            run_id,
            Some(idempotency_key.to_string()),
            None,
            CompanyEventKind::WorkflowRunStateChanged {
                workflow_run_id: run_id.to_string(),
                state: state.as_str().to_string(),
                restart_state: restart_state.as_str().to_string(),
                reason: reason.to_string(),
                next_wake_at,
            },
        ))?;
        Ok(())
    }
}

fn recovery_event(node: &WorkflowNodeProjection, now: &str) -> Result<NewCompanyEvent> {
    Ok(event_input(&node.run_id,
        Some(format!("workflow:recover:{}:{}", node.node_id, node.attempt)),
        Some(node.node_id.clone()), CompanyEventKind::WorkflowNodeStateChanged {
            node_id: node.node_id.clone(), phase: WorkflowPhase::from_projection(node)?.as_str().into(),
            state: WorkflowNodeState::Ready.as_str().into(), restart_state: RestartState::Requeued.as_str().into(),
            reason: "executor lease expired or its owner released the runtime claim".into(),
            next_wake_at: Some(now.into()), result_json: None, usage_json: None, evidence_json: None,
            lease_id: node.lease_id.clone(), fencing_token: node.fencing_token.clone(), clear_lease: true,
            wait_json: None,
        }))
}

impl WorkflowPhase {
    fn from_projection(node: &WorkflowNodeProjection) -> Result<Self> {
        match node.phase.as_str() {
            "prepare" => Ok(Self::Prepare),
            "plan" => Ok(Self::Plan),
            "execute" => Ok(Self::Execute),
            "review" => Ok(Self::Review),
            "commit" => Ok(Self::Commit),
            other => bail!("unknown workflow phase {other}"),
        }
    }
}

fn event_input(
    run_id: &str,
    idempotency_key: Option<String>,
    node_id: Option<String>,
    event: CompanyEventKind,
) -> NewCompanyEvent {
    NewCompanyEvent {
        run_id: run_id.to_string(),
        session_id: run_id.to_string(),
        pod_id: None,
        work_node_id: node_id,
        attempt_id: None,
        agent_identity_id: Some("workflow-scheduler".to_string()),
        agent_instance_id: Some("workflow-scheduler".to_string()),
        causation_id: None,
        correlation_id: None,
        idempotency_key,
        event,
    }
}

fn workflow_would_cycle(
    snapshot: &WorkflowSnapshot,
    new_node_id: &str,
    parent_id: Option<&str>,
    dependencies: &[String],
) -> bool {
    let mut upstreams: HashMap<String, Vec<String>> = snapshot
        .nodes
        .iter()
        .map(|node| {
            let mut edges = node.dependencies.clone();
            if let Some(parent_id) = node.parent_id.as_deref() {
                edges.push(parent_id.to_string());
            }
            (node.node_id.clone(), edges)
        })
        .collect();
    let mut new_upstreams = dependencies.to_vec();
    if let Some(parent_id) = parent_id {
        new_upstreams.push(parent_id.to_string());
    }
    upstreams.insert(new_node_id.to_string(), new_upstreams.clone());
    new_upstreams
        .into_iter()
        .any(|upstream| reaches_node(&upstream, new_node_id, &upstreams, &mut HashSet::new()))
}

fn reaches_node(
    current: &str,
    target: &str,
    upstreams: &HashMap<String, Vec<String>>,
    visited: &mut HashSet<String>,
) -> bool {
    if current == target {
        return true;
    }
    if !visited.insert(current.to_string()) {
        return false;
    }
    upstreams.get(current).is_some_and(|edges| {
        edges
            .iter()
            .any(|edge| reaches_node(edge, target, upstreams, visited))
    })
}

fn transition_recovery_hint(from: WorkflowNodeState, to: WorkflowNodeState) -> &'static str {
    match from {
        WorkflowNodeState::Succeeded | WorkflowNodeState::Failed | WorkflowNodeState::Canceled =>
            "This assignment is terminal and cannot be reopened. Inspect its saved outcome; additional authorized work needs a new assignment.",
        WorkflowNodeState::Pending =>
            "Complete the prerequisites, then use workflow reconcile and tick to claim the ready assignment before execution and review.",
        WorkflowNodeState::Ready =>
            "Claim this ready assignment through workflow tick before execution. Review the result before committing with its required verified evidence.",
        WorkflowNodeState::Blocked | WorkflowNodeState::Stale =>
            "Inspect and resolve the recorded block or recovery reason, then use workflow reconcile and tick to obtain a current lease.",
        _ if to == WorkflowNodeState::Succeeded =>
            "Move active work to review first, then commit with the required verified evidence.",
        _ => "Inspect the current assignment before choosing its next state.",
    }
}

fn valid_transition(from: WorkflowNodeState, to: WorkflowNodeState) -> bool {
    if from == to {
        return true;
    }
    match from {
        WorkflowNodeState::Pending => matches!(
            to,
            WorkflowNodeState::Ready | WorkflowNodeState::Blocked | WorkflowNodeState::Canceled
        ),
        WorkflowNodeState::Ready => matches!(
            to,
            WorkflowNodeState::Leased | WorkflowNodeState::Blocked | WorkflowNodeState::Canceled
        ),
        WorkflowNodeState::Leased => matches!(
            to,
            WorkflowNodeState::Running
                | WorkflowNodeState::Review
                | WorkflowNodeState::Ready
                | WorkflowNodeState::Failed
                | WorkflowNodeState::Canceled
                | WorkflowNodeState::Stale
        ),
        WorkflowNodeState::Running => matches!(
            to,
            WorkflowNodeState::Review
                | WorkflowNodeState::Ready
                | WorkflowNodeState::Failed
                | WorkflowNodeState::Canceled
                | WorkflowNodeState::Stale
        ),
        WorkflowNodeState::Review => matches!(
            to,
            WorkflowNodeState::Ready
                | WorkflowNodeState::Succeeded
                | WorkflowNodeState::Failed
                | WorkflowNodeState::Canceled
        ),
        WorkflowNodeState::Blocked | WorkflowNodeState::Stale => {
            matches!(to, WorkflowNodeState::Ready | WorkflowNodeState::Canceled)
        }
        WorkflowNodeState::WaitingUser | WorkflowNodeState::WaitingPeer => {
            matches!(to, WorkflowNodeState::Ready | WorkflowNodeState::Canceled)
        }
        WorkflowNodeState::Succeeded | WorkflowNodeState::Failed | WorkflowNodeState::Canceled => {
            false
        }
    }
}

fn validate_lease_token(token: &LeaseToken) -> Result<()> {
    validate_id(&token.node_id, "node_id", "node")?;
    validate_id(&token.lease_id, "lease_id", "lease")?;
    check_text(&token.worker_id, "worker_id")?;
    validate_id(&token.fencing_token, "fencing_token", "fence")?;
    Ok(())
}

fn validate_requirements(requirements: &[EvidenceRequirement]) -> Result<()> {
    if requirements.len() > MAX_EVIDENCE_RECEIPTS {
        bail!("workflow has too many evidence requirements");
    }
    for requirement in requirements {
        check_text(&requirement.kind, "evidence kind")?;
        check_text(&requirement.description, "evidence description")?;
        if requirement.minimum_receipts == 0 && requirement.required {
            bail!("required evidence must request at least one receipt");
        }
        if requirement.minimum_receipts as usize > MAX_EVIDENCE_RECEIPTS {
            bail!("evidence requirement asks for too many receipts");
        }
    }
    Ok(())
}

fn validate_receipts(receipts: &[EvidenceReceipt]) -> Result<()> {
    if receipts.len() > MAX_EVIDENCE_RECEIPTS {
        bail!("workflow has too many evidence receipts");
    }
    let mut ids = HashSet::new();
    for receipt in receipts {
        validate_receipt(receipt)?;
        if !ids.insert(&receipt.receipt_id) {
            bail!("duplicate workflow evidence receipt");
        }
    }
    Ok(())
}

fn validate_receipt(receipt: &EvidenceReceipt) -> Result<()> {
    validate_id(&receipt.receipt_id, "receipt_id", "receipt")?;
    check_text(&receipt.kind, "evidence kind")?;
    check_text(&receipt.summary, "evidence summary")?;
    if let Some(uri) = receipt.uri.as_deref() {
        check_text(uri, "evidence uri")?;
    }
    if let Some(hash) = receipt.content_hash.as_deref() {
        check_text(hash, "evidence content hash")?;
    }
    parse_time(&receipt.recorded_at, "evidence recorded_at")?;
    Ok(())
}

fn ensure_evidence(
    requirements: &[EvidenceRequirement],
    receipts: &[EvidenceReceipt],
) -> Result<()> {
    validate_requirements(requirements)?;
    validate_receipts(receipts)?;
    let mut missing = Vec::new();
    for requirement in requirements
        .iter()
        .filter(|requirement| requirement.required)
    {
        let count = receipts
            .iter()
            .filter(|receipt| receipt.kind == requirement.kind && receipt.verified)
            .count();
        if count < requirement.minimum_receipts as usize {
            missing.push(format!("{}: {count}/{} verified receipts ({})",
                requirement.kind.chars().take(80).collect::<String>(), requirement.minimum_receipts,
                requirement.description.chars().take(160).collect::<String>()));
        }
    }
    if !missing.is_empty() {
        bail!("required workflow evidence is missing: {}. Record verified evidence for this assignment before retrying commit.", missing.join("; "));
    }
    Ok(())
}

fn check_limit(value: Option<u64>, maximum: u64, name: &str) -> Result<()> {
    if value.is_some_and(|value| value == 0 || value > maximum) {
        bail!("{name} is outside its safe bound");
    }
    Ok(())
}

fn check_text(value: &str, name: &str) -> Result<()> {
    check_text_bound(value, name, MAX_TEXT_BYTES)
}

fn check_text_bound(value: &str, name: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum {
        bail!("{name} must be non-empty and within its byte bound");
    }
    Ok(())
}

fn validate_id(value: &str, name: &str, prefix: &str) -> Result<()> {
    if value.len() > MAX_ID_BYTES
        || value.is_empty()
        || !value.starts_with(&format!("{prefix}_"))
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
    {
        bail!("{name} must use the stable {prefix}_ identifier form");
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
        })
    {
        bail!("workflow idempotency key is outside its safe bound");
    }
    Ok(())
}

fn parse_time(value: &str, name: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{name} must be RFC3339"))
        .map(|value| value.with_timezone(&Utc))
}

fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn stable_id(prefix: &str, seed: &str) -> String {
    format!("{prefix}_{:x}", Sha256::digest(seed.as_bytes()))
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn unknown_budget_fields_cannot_silently_remove_the_limit() {
        let error = serde_json::from_value::<WorkflowBudget>(serde_json::json!({"max_tokens": 50000})).unwrap_err();
        assert!(error.to_string().contains("max_tokens"));
        let budget: WorkflowBudget = serde_json::from_value(serde_json::json!({"max_total_tokens": 50000})).unwrap();
        assert_eq!(budget.max_total_tokens, Some(50000));
        assert_eq!(serde_json::from_value::<WorkflowBudget>(serde_json::to_value(WorkflowBudget::default()).unwrap()).unwrap(), WorkflowBudget::default());
    }

    #[test]
    fn rejected_workflow_transitions_explain_the_current_recovery_path() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        owner.transition_node("node_ownership", "review", Some(&lease.token), WorkflowPhase::Review,
            WorkflowNodeState::Review, RestartState::Fresh, "Review output", None, None, None, None).unwrap();
        let before = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        let backwards = owner.transition_node("node_ownership", "backwards", Some(&lease.token), WorkflowPhase::Execute,
            WorkflowNodeState::Ready, RestartState::Requeued, "Repair output", None, None, None, None).unwrap_err().to_string();
        assert!(backwards.contains("review -> execute") && backwards.contains("retain its current phase"));
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes[0].as_of_seq, before.as_of_seq);
        owner.transition_node("node_ownership", "requeue", Some(&lease.token), WorkflowPhase::Review,
            WorkflowNodeState::Ready, RestartState::Requeued, "Repair output", None, None, None, None).unwrap();
        let ready = owner.transition_node("node_ownership", "unclaimed", None, WorkflowPhase::Commit,
            WorkflowNodeState::Succeeded, RestartState::Terminal, "Done", None, None, None, None).unwrap_err().to_string();
        assert!(ready.contains("ready -> succeeded") && ready.contains("workflow tick"));
        owner.transition_node("node_ownership", "cancel", None, WorkflowPhase::Review,
            WorkflowNodeState::Canceled, RestartState::Terminal, "Canceled", None, None, None, None).unwrap();
        let canceled = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        let terminal = owner.transition_node("node_ownership", "revive", None, WorkflowPhase::Commit,
            WorkflowNodeState::Succeeded, RestartState::Terminal, "Done", None, None, None, None).unwrap_err().to_string();
        assert!(terminal.contains("canceled -> succeeded") && terminal.contains("terminal and cannot be reopened"));
        assert!(!terminal.contains("Move active work to review"));
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes[0].as_of_seq, canceled.as_of_seq);
    }

    #[test]
    fn rejected_commit_stays_unfinished_until_the_exact_assignment_has_evidence() {
        use crate::runtime::build_contract::BuildContractGuard;
        let dir = tempfile::tempdir().unwrap();
        let requirement = EvidenceRequirement { kind: "render".into(), description: "Inspect the saved output".into(), minimum_receipts: 1, required: true };
        let (owner, lease) = fixture_with_requirements(&dir.path().join("company.sqlite"), vec![requirement]);
        let mut guard = BuildContractGuard::new("Make me a banana in Blender.");
        let input = serde_json::json!({"action":"workflow", "workflow_action":"transition", "workflow_payload":{"node_id":"node_ownership", "state":"succeeded"}});
        let commit = |key| owner.transition_node("node_ownership", key, Some(&lease.token),
            WorkflowPhase::Commit, WorkflowNodeState::Succeeded, RestartState::Terminal, "Verified", None, None, None, None);
        let invalid = commit("skip-review").unwrap_err().to_string();
        assert!(invalid.contains("leased -> succeeded") && invalid.contains("review"));
        guard.observe_workflow_outcome("work", &input, false, &invalid);
        owner.transition_node("node_ownership", "begin-review", Some(&lease.token), WorkflowPhase::Review,
            WorkflowNodeState::Review, RestartState::Fresh, "Inspect", None, None, None, None).unwrap();
        let missing = commit("missing-evidence").unwrap_err().to_string();
        assert!(missing.contains("render: 0/1 verified receipts (Inspect the saved output)"));
        guard.observe_workflow_outcome("work", &input, false, &missing);
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes[0].state, "review");
        assert!(guard.pending_work_feedback("no-todos").unwrap().contains("render: 0/1"));
        assert!(guard.pending_work_feedback("no-todos").is_none(), "an unchanged failure must not cause a reminder loop");
        let mut response = crate::runtime::FinalResponse { summary: "Done".into(), final_markdown: "Saved.".into(),
            execution_mode: "complete".into(), changes_made: vec![], verification: vec![], tool_transcript: vec![] };
        guard.preserve_pending_work("no-todos", &mut response);
        assert_eq!(response.execution_mode, "incomplete_work");
        assert!(!response.final_markdown.contains("node_ownership"), "keep internal diagnostics out of the compact final notice");
        owner.record_evidence(&lease.token, EvidenceReceipt { receipt_id: "receipt_render_proof".into(), kind: "render".into(),
            summary: "Saved output inspected".into(), uri: None, content_hash: None, verified: true, recorded_at: Utc::now().to_rfc3339() }).unwrap();
        commit("verified-commit").unwrap();
        guard.observe_workflow_outcome("work", &input, true, "Committed");
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes[0].state, "succeeded");
        response.execution_mode = "complete".into();
        guard.preserve_pending_work("no-todos", &mut response);
        assert_eq!(response.execution_mode, "complete");
    }

    #[tokio::test]
    async fn live_turn_renews_lease_past_original_expiry_and_drop_stops_renewal() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let owner = owner.with_lease_ttl(Duration::from_secs(5)).unwrap();
        let initial_expiry = owner.heartbeat(&lease.token, Utc::now()).unwrap();
        let mut keeper = TurnLeaseKeeper::default();
        keeper.track(owner.clone(), lease.token.clone());
        keeper.track(owner.clone(), lease.token.clone());
        assert_eq!(keeper.tasks.len(), 1, "duplicate receipts cannot spawn duplicate renewers");
        tokio::time::sleep(Duration::from_secs(6)).await;
        let current = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert!(parse_time(&initial_expiry, "expiry").unwrap() < Utc::now());
        assert!(parse_time(current.lease_expires_at.as_deref().unwrap(), "expiry").unwrap() > Utc::now());
        assert_eq!(current.lease_id.as_deref(), Some(lease.token.lease_id.as_str()));
        assert_eq!(current.attempt, 1, "renewal is not a restart or new claim");
        drop(keeper);
        let stopped = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0).heartbeat_at;
        tokio::time::sleep(Duration::from_millis(1800)).await;
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes.remove(0).heartbeat_at, stopped);
    }

    #[tokio::test]
    async fn live_turn_renewal_cannot_revive_a_released_lease() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let owner = owner.with_lease_ttl(Duration::from_secs(5)).unwrap();
        let mut keeper = TurnLeaseKeeper::default();
        keeper.track(owner.clone(), lease.token.clone());
        owner.transition_node("node_ownership", "release-renewer-test", Some(&lease.token),
            WorkflowPhase::Execute, WorkflowNodeState::Ready, RestartState::Waiting,
            "Released", None, None, None, None).unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(keeper.tasks.values().all(|(_, task)| task.is_finished()));
        assert_eq!(owner.projection(Some("run_ownership")).unwrap().nodes.remove(0).state, "ready");
    }

    #[test]
    fn atomic_plan_installation_is_complete_idempotent_and_rollback_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _) = fixture(&dir.path().join("company.sqlite"));
        let request = WorkflowPlanRequest { idempotency_key: "atomic-fixture".into(),
            contract: DurableWorkflowContract { title: "Review".into(), objective: "Atomic graph".into(),
                budget: WorkflowBudget::default(), evidence_requirements: vec![],
                concurrency: ConcurrencyPolicy::default(), metadata: Default::default(),
                ownership: WorkflowOwnership { scope: WorkflowScope::Agent, owner_agent_id: "coder".into(), group_id: None } },
            assignments: vec![
                WorkflowPlanAssignment { key: "review".into(), owner_agent_id: "coder".into(), title: "Review".into(),
                    outcome: "Verdict".into(), dependencies: vec!["brief".into()] },
                WorkflowPlanAssignment { key: "brief".into(), owner_agent_id: "researcher".into(), title: "Brief".into(),
                    outcome: "Sources".into(), dependencies: vec![] },
            ] };
        let initial_events = owner.store.events_since(0, 100).unwrap().len();
        let prepared = DurableWorkflowScheduler::prepare_plan(request.clone(), &owner.policy).unwrap();
        assert_eq!(owner.store.events_since(0, 100).unwrap().len(), initial_events,
            "preparing a graph must not publish a goal or reserve a task");
        assert_eq!(prepared.events.len(), 4);
        let receipt = owner.install_plan(request.clone()).unwrap();
        assert_eq!(receipt.run_id, prepared.receipt.run_id);
        assert_eq!(receipt.assignments, prepared.receipt.assignments);
        let before = owner.store.events_since(0, 100).unwrap().len();
        let retry = owner.install_plan(request.clone()).unwrap();
        assert_eq!(receipt.run_id, retry.run_id);
        assert_eq!(owner.store.events_since(0, 100).unwrap().len(), before);
        let snapshot = owner.projection(Some(&receipt.run_id)).unwrap();
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.runs[0].state, "planned");
        let (_, none) = owner.tick(&receipt.run_id, "coder", Utc::now()).unwrap();
        assert!(none.is_empty());
        let (_, brief) = owner.tick(&receipt.run_id, "researcher", Utc::now()).unwrap();
        assert_eq!(brief[0].token.node_id, receipt.assignments["brief"]);
        for variant in 0..5 {
            let mut changed = request.clone();
            match variant {
                0 => { changed.assignments.remove(0); }
                1 => { changed.assignments[0].outcome = "Different".into(); }
                2 => { changed.idempotency_key = "invalid-owner".into(); changed.assignments[1].owner_agent_id = "not-a-member".into(); }
                3 => { changed.idempotency_key = "cycle".into(); changed.assignments[1].dependencies = vec!["review".into()]; }
                _ => { let mut extra = changed.assignments[1].clone(); extra.key = "extra".into(); changed.assignments.push(extra); }
            }
            let before = owner.store.events_since(0, 100).unwrap().len();
            assert!(owner.install_plan(changed).is_err());
            assert_eq!(owner.store.events_since(0, 100).unwrap().len(), before, "failed install must not leave partial events");
        }
        let reopened = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        assert_eq!(reopened.workflow_snapshot(Some(&receipt.run_id)).unwrap().nodes.len(), 2);
    }

    #[test]
    fn scheduled_runs_remain_untouched_until_their_wake_time() {
        for active in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, _) = fixture(&dir.path().join("company.sqlite"));
            let now = Utc::now();
            let due = now + chrono::Duration::hours(1);
            owner.open_run(RunRequest { run_id: Some("run_scheduled".into()), goal_id: "goal_ownership".into(),
                idempotency_key: Some("scheduled-run".into()), budget: WorkflowBudget::default(),
                next_wake_at: Some(due.to_rfc3339()) }).unwrap();
            owner.define_node(NodeSpec { node_id: Some("node_scheduled".into()), owner_agent_id: Some("coder".into()),
                idempotency_key: Some("scheduled-node".into()), run_id: "run_scheduled".into(), parent_id: None,
                title: "Scheduled work".into(), outcome: "Wait until due".into(), phase: WorkflowPhase::Execute,
                dependencies: vec![], budget: WorkflowBudget::default(), evidence_requirements: vec![] }).unwrap();
            if active {
                owner.append_run_state("run_scheduled", WorkflowRunState::Active, RestartState::Waiting,
                    "Active but scheduled", Some(due.to_rfc3339()), "active-scheduled").unwrap();
                owner.transition_node("node_scheduled", "scheduled-ready", None, WorkflowPhase::Execute,
                    WorkflowNodeState::Ready, RestartState::Waiting, "Ready", None, None, None, None).unwrap();
            }
            let before = owner.projection(Some("run_scheduled")).unwrap();
            let (_, early) = owner.tick("run_scheduled", "coder", now).unwrap();
            assert!(early.is_empty(), "future run must not execute early");
            let after = owner.projection(Some("run_scheduled")).unwrap();
            assert_eq!(before.runs[0].as_of_seq, after.runs[0].as_of_seq);
            assert_eq!(before.nodes[0].as_of_seq, after.nodes[0].as_of_seq);
            assert_eq!(after.runs[0].next_wake_at.as_deref(), Some(due.to_rfc3339().as_str()));
            if active { assert!(owner.claim(&after.nodes[0], "coder", now).unwrap().is_none()); }
            let (_, ready) = owner.tick("run_scheduled", "coder", due).unwrap();
            assert_eq!(ready.len(), 1, "run should become eligible at the exact wake boundary");
        }
    }

    #[test]
    fn resume_retry_after_reopen_returns_saved_owner_without_reactivation() {
        for target in [None, Some("planner")] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("company.sqlite");
            let (mut owner, _) = fixture(&path);
            owner.append_run_state("run_ownership", WorkflowRunState::Paused, RestartState::Waiting,
                "Explicit pause", None, "pause-before-resume").unwrap();
            let before = owner.projection(Some("run_ownership")).unwrap();
            // The event commits, but its caller loses the acknowledgement.
            owner.resume_or_reroute_run("run_ownership", "coder", target,
                "Resume approved work", "lost-ack").unwrap();
            let committed = owner.projection(Some("run_ownership")).unwrap();
            let events = owner.store.events_since(before.as_of_seq, 10).unwrap();
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0].envelope.event, CompanyEventKind::WorkflowRunOwnershipChanged { .. }));
            assert_eq!(committed.runs[0].state, "active");
            assert_eq!(serde_json::to_value(&committed.nodes).unwrap(), serde_json::to_value(&before.nodes).unwrap(),
                "resuming a run must not replace its assignment leases");
            for reopen in [false, true] {
                if reopen {
                    drop(owner);
                    owner = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
                        ConcurrencyPolicy::default()).unwrap();
                }
                assert_eq!(owner.resume_or_reroute_run("run_ownership", "coder", target,
                    "Resume approved work", "lost-ack").unwrap(), target.unwrap_or("coder"));
                assert_eq!(serde_json::to_value(owner.projection(Some("run_ownership")).unwrap()).unwrap(),
                    serde_json::to_value(&committed).unwrap());
                assert!(owner.store.events_since(committed.as_of_seq, 10).unwrap().is_empty(),
                    "receipt recovery must not append or replay the ownership change");
            }
            owner.append_run_state("run_ownership", WorkflowRunState::Paused, RestartState::Waiting,
                "New owner requested", None, "later-pause").unwrap();
            owner.resume_or_reroute_run("run_ownership", "coder", Some("researcher"),
                "Later authorized reroute", "later-reroute").unwrap();
            for canceled in [false, true] {
                if canceled {
                    owner.append_run_state("run_ownership", WorkflowRunState::Canceled, RestartState::Terminal,
                        "Later cancellation", None, "later-cancel").unwrap();
                }
                let latest = owner.projection(Some("run_ownership")).unwrap();
                assert_eq!(latest.runs[0].owner_agent_id, "researcher");
                assert_eq!(owner.resume_or_reroute_run("run_ownership", "coder", target,
                    "Resume approved work", "lost-ack").unwrap(), target.unwrap_or("coder"));
                assert_eq!(serde_json::to_value(owner.projection(Some("run_ownership")).unwrap()).unwrap(),
                    serde_json::to_value(&latest).unwrap(), "old receipts cannot undo newer ownership or cancellation");
            }
        }
    }

    #[test]
    fn resume_retry_rejects_changed_payload_and_preserves_new_request_guards() {
        for target in [None, Some("planner")] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("company.sqlite");
            let (owner, _) = fixture(&path);
            owner.append_run_state("run_ownership", WorkflowRunState::Paused, RestartState::Waiting,
                "Explicit pause", None, "pause-before-resume").unwrap();
            owner.resume_or_reroute_run("run_ownership", "coder", target,
                "Resume approved work", "bound-request").unwrap();
            drop(owner);
            let owner = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
                ConcurrencyPolicy::default()).unwrap();
            for paused_again in [false, true] {
                if paused_again {
                    owner.append_run_state("run_ownership", WorkflowRunState::Paused, RestartState::Waiting,
                        "Pause a second time", None, "second-pause").unwrap();
                }
                let before = owner.projection(Some("run_ownership")).unwrap();
                let changed_presence = if target.is_some() { None } else { Some("coder") };
                for (actor, new_target, reason) in [
                    ("researcher", target, "Resume approved work"),
                    ("coder", target, "Different instruction"),
                    ("coder", Some("researcher"), "Resume approved work"),
                    ("coder", changed_presence, "Resume approved work"),
                ] {
                    let error = owner.resume_or_reroute_run("run_ownership", actor, new_target,
                        reason, "bound-request").unwrap_err();
                    assert!(error.to_string().contains("different request"), "{error:#}");
                }
                let invalid_new = if paused_again {
                    owner.resume_or_reroute_run("run_ownership", "coder", Some("missing_agent"),
                        "Invalid owner", "new-invalid-owner")
                } else {
                    owner.resume_or_reroute_run("run_ownership", "coder", target,
                        "New request while active", "new-active-request")
                }.unwrap_err();
                assert!(invalid_new.to_string().contains(if paused_again {
                    "not an active coworker"
                } else { "only a paused workflow" }), "{invalid_new:#}");
                assert_eq!(owner.resume_or_reroute_run("run_ownership", "coder", target,
                    "Resume approved work", "bound-request").unwrap(), target.unwrap_or("coder"));
                assert_eq!(serde_json::to_value(owner.projection(Some("run_ownership")).unwrap()).unwrap(),
                    serde_json::to_value(&before).unwrap(), "rejected or recovered requests cannot mutate the store");
            }
        }
    }

    #[test]
    fn legacy_resume_retry_preserves_receipt_and_rejects_payload_substitution() {
        for target in [None, Some("planner")] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("company.sqlite");
            let (owner, _) = fixture(&path);
            owner.append_run_state("run_ownership", WorkflowRunState::Paused, RestartState::Waiting,
                "Explicit pause", None, "legacy-pause").unwrap();
            // Exact pre-fix persisted envelope: no original-request fingerprint.
            let mut legacy = event_input("run_ownership", Some("workflow:ownership:run_ownership:legacy-resume".into()),
                None, CompanyEventKind::WorkflowRunOwnershipChanged {
                    workflow_run_id: "run_ownership".into(), new_owner_agent_id: target.unwrap_or("coder").into(),
                    reason: "Legacy approved work".into(),
                });
            legacy.agent_identity_id = Some("coder".into());
            legacy.agent_instance_id = Some("coder".into());
            let saved_event = owner.store.append(legacy).unwrap();
            let before = owner.projection(Some("run_ownership")).unwrap();
            drop(owner);
            let owner = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
                ConcurrencyPolicy::default()).unwrap();
            assert_eq!(owner.resume_or_reroute_run("run_ownership", "coder", target,
                "Legacy approved work", "legacy-resume").unwrap(), target.unwrap_or("coder"));
            for (actor, new_target, reason) in [
                ("researcher", target, "Legacy approved work"),
                ("coder", Some("researcher"), "Legacy approved work"),
                ("coder", target, "Changed legacy reason"),
            ] {
                assert!(owner.resume_or_reroute_run("run_ownership", actor, new_target,
                    reason, "legacy-resume").unwrap_err().to_string().contains("different request"));
            }
            assert_eq!(serde_json::to_value(owner.projection(Some("run_ownership")).unwrap()).unwrap(),
                serde_json::to_value(before).unwrap());
            let events = owner.store.events_since(saved_event.company_seq - 1, 10).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(serde_json::to_value(&events[0]).unwrap(), serde_json::to_value(saved_event).unwrap(),
                "recovery must not rewrite or migrate a stored legacy receipt");
        }
    }

    #[test]
    fn stale_automatic_activation_does_not_undo_pause_or_cancel() {
        for state in [WorkflowRunState::Paused, WorkflowRunState::Canceled] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, _) = fixture(&dir.path().join("company.sqlite"));
            owner.append_run_state("run_ownership", WorkflowRunState::Planned, RestartState::Waiting,
                "Fixture planned state", None, "fixture-planned").unwrap();
            let observed = owner.projection(Some("run_ownership")).unwrap().runs.remove(0);
            owner.append_run_state("run_ownership", state, RestartState::Waiting,
                "User decision", None, "user-decision").unwrap();
            let changed = owner.projection(Some("run_ownership")).unwrap().runs.remove(0);
            assert_ne!(observed.as_of_seq, changed.as_of_seq);
            assert!(owner.append_run_state("run_ownership", WorkflowRunState::Active, RestartState::Waiting,
                "workflow scheduler active", None, "stale-activation").is_err(),
                "automatic activation must not replace a newer user decision");
            let current = owner.projection(Some("run_ownership")).unwrap().runs.remove(0);
            assert_eq!(current.state, state.as_str());
            assert_eq!(current.as_of_seq, changed.as_of_seq);
            let resumed = owner.resume_or_reroute_run("run_ownership", "coder", None,
                "Explicit user resume", "explicit-resume");
            if state == WorkflowRunState::Paused {
                assert_eq!(resumed.unwrap(), "coder");
                assert_eq!(owner.projection(Some("run_ownership")).unwrap().runs[0].state, "active");
            } else {
                assert!(resumed.is_err(), "canceled work must not silently resume");
            }
        }
    }

    #[test]
    fn completion_and_new_assignments_cannot_leave_finished_runs_with_pending_work() {
        for ordering in [Some(false), Some(true), None] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
            let writer = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(dir.path().join("company.sqlite")).unwrap()),
                ConcurrencyPolicy::default()).unwrap();
            owner.transition_node("node_ownership", "review", Some(&lease.token), WorkflowPhase::Review,
                WorkflowNodeState::Review, RestartState::Fresh, "Review", None, None, None, None).unwrap();
            owner.transition_node("node_ownership", "done", Some(&lease.token), WorkflowPhase::Commit,
                WorkflowNodeState::Succeeded, RestartState::Terminal, "Done", None, None, None, None).unwrap();
            let complete = || owner.append_run_state("run_ownership", WorkflowRunState::Completed,
                RestartState::Terminal, "all workflow nodes committed", None, "completion-race");
            let add = || writer.define_node(NodeSpec { node_id: Some("node_late".into()), owner_agent_id: Some("coder".into()),
                idempotency_key: Some("late".into()), run_id: "run_ownership".into(), parent_id: None,
                title: "Late work".into(), outcome: "Must not be stranded".into(), phase: WorkflowPhase::Execute,
                dependencies: vec![], budget: WorkflowBudget::default(), evidence_requirements: vec![] });
            let completion_first = match ordering {
                Some(true) => {
                    complete().unwrap();
                    assert!(add().is_err(), "a completed run cannot accept a new pending task");
                    true
                }
                Some(false) => {
                    add().unwrap();
                    assert!(complete().is_err(), "completion must recheck tasks added after its snapshot");
                    false
                }
                None => {
                    let barrier = std::sync::Barrier::new(2);
                    let (completed, added) = std::thread::scope(|scope| {
                        let first = scope.spawn(|| { barrier.wait(); complete().is_ok() });
                        let second = scope.spawn(|| { barrier.wait(); add().is_ok() });
                        (first.join().unwrap(), second.join().unwrap())
                    });
                    assert_ne!(completed, added, "two database connections must admit exactly one ordering");
                    completed
                }
            };
            let snapshot = owner.projection(Some("run_ownership")).unwrap();
            assert_eq!(snapshot.runs[0].state, if completion_first { "completed" } else { "active" });
            assert_eq!(snapshot.nodes.len(), if completion_first { 1 } else { 2 });
        }
    }

    #[test]
    fn prerequisite_revision_fences_obsolete_block_decisions() {
        for fenced in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
            owner.define_node(NodeSpec { node_id: Some("node_child".into()), owner_agent_id: Some("coder".into()),
                idempotency_key: Some("child".into()), run_id: "run_ownership".into(),
                parent_id: Some("node_ownership".into()), title: "Child".into(), outcome: "Dependent work".into(),
                phase: WorkflowPhase::Execute, dependencies: vec![], budget: WorkflowBudget::default(),
                evidence_requirements: vec![] }).unwrap();
            owner.transition_node("node_ownership", "parent-stale", Some(&lease.token), WorkflowPhase::Execute,
                WorkflowNodeState::Stale, RestartState::Waiting, "Needs revision", None, None, None, None).unwrap();
            let snapshot = owner.projection(Some("run_ownership")).unwrap();
            let parent = snapshot.nodes.iter().find(|node| node.node_id == "node_ownership").unwrap();
            let child = snapshot.nodes.iter().find(|node| node.node_id == "node_child").unwrap();
            let observed = [(parent.node_id.as_str(), Some(parent.as_of_seq))];
            owner.transition_node("node_ownership", "parent-revised", None, WorkflowPhase::Execute,
                WorkflowNodeState::Ready, RestartState::Waiting, "Revised", None, None, None, None).unwrap();
            let result = owner.append_node_state(child, WorkflowPhase::Execute, WorkflowNodeState::Blocked,
                RestartState::Waiting, "Old parent-stale decision".into(), None, None, None, None,
                None, None, true, "obsolete-parent-block".into(), if fenced { &observed } else { &[] });
            let current = owner.projection(Some("run_ownership")).unwrap().nodes.into_iter()
                .find(|node| node.node_id == "node_child").unwrap();
            if fenced {
                assert!(result.unwrap_err().to_string().contains("prerequisite"));
                assert_eq!(current.state, "pending");
                assert_eq!(current.as_of_seq, child.as_of_seq);
            } else {
                // Before-control: checking only the child's unchanged
                // revision accepts the obsolete upstream-derived decision.
                result.unwrap();
                assert_eq!(current.state, "blocked");
            }
        }
    }

    #[test]
    fn failed_parent_blocks_its_child_without_stopping_independent_work() {
        for ready in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
            for (id, parent) in [("node_child", Some("node_ownership".into())), ("node_peer", None)] {
                owner.define_node(NodeSpec { node_id: Some(id.into()), owner_agent_id: Some("coder".into()),
                    idempotency_key: Some(id.into()), run_id: "run_ownership".into(), parent_id: parent,
                    title: id.into(), outcome: "Deliver result".into(), phase: WorkflowPhase::Execute,
                    dependencies: vec![], budget: WorkflowBudget::default(), evidence_requirements: vec![] }).unwrap();
            }
            if ready {
                owner.transition_node("node_child", "early-ready", None, WorkflowPhase::Execute,
                    WorkflowNodeState::Ready, RestartState::Waiting, "Ready", None, None, None, None).unwrap();
            }
            owner.transition_node("node_ownership", "parent-failed", Some(&lease.token), WorkflowPhase::Execute,
                WorkflowNodeState::Failed, RestartState::Terminal, "Parent failed", None, None, None, None).unwrap();
            let (_, claims) = owner.tick("run_ownership", "coder", Utc::now()).unwrap();
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].token.node_id, "node_peer");
            let child = owner.projection(Some("run_ownership")).unwrap().nodes.into_iter()
                .find(|node| node.node_id == "node_child").unwrap();
            assert_eq!(child.state, "blocked", "failed parent must not leave queued or falsely ready work");
            assert!(child.reason.contains("node_ownership") && child.reason.contains("failed"));
            owner.reconcile("run_ownership", Utc::now()).unwrap();
            let replay = owner.projection(Some("run_ownership")).unwrap().nodes.into_iter()
                .find(|node| node.node_id == "node_child").unwrap();
            assert_eq!(replay.as_of_seq, child.as_of_seq, "unchanged blocked state must not emit duplicate updates");
            owner.transition_node("node_child", "retry-without-parent-fix", None, WorkflowPhase::Execute,
                WorkflowNodeState::Ready, RestartState::Waiting, "Retry", None, None, None, None).unwrap();
            owner.reconcile("run_ownership", Utc::now()).unwrap();
            let retried = owner.projection(Some("run_ownership")).unwrap().nodes.into_iter()
                .find(|node| node.node_id == "node_child").unwrap();
            assert_eq!(retried.state, "blocked", "old block receipt must not suppress a new revision's decision");
            assert!(retried.as_of_seq > child.as_of_seq);
        }
    }

    #[test]
    fn ineligible_ready_work_does_not_consume_the_tick_claim_budget() {
        let dir = tempfile::tempdir().unwrap();
        let (mut owner, _) = fixture(&dir.path().join("company.sqlite"));
        owner.policy.max_claims_per_tick = 1;
        for (id, dependencies, wake) in [
            ("node_delayed", vec![], Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339())),
            ("node_waiting_dependency", vec!["node_ownership".into()], None),
            ("node_independent_first", vec![], None),
            ("node_independent_second", vec![], None),
        ] {
            owner.define_node(NodeSpec { node_id: Some(id.into()), owner_agent_id: Some("coder".into()),
                idempotency_key: Some(id.into()), run_id: "run_ownership".into(), parent_id: None,
                title: id.into(), outcome: "Deliver independent work when eligible".into(),
                phase: WorkflowPhase::Execute, dependencies, budget: WorkflowBudget::default(),
                evidence_requirements: vec![] }).unwrap();
            owner.transition_node(id, &format!("{id}-ready"), None, WorkflowPhase::Execute,
                WorkflowNodeState::Ready, RestartState::Waiting, "Ready", wake, None, None, None).unwrap();
        }
        let (report, claims) = owner.tick("run_ownership", "coder", Utc::now()).unwrap();
        assert_eq!(claims.len(), 1, "unavailable candidates must not starve independent work");
        assert_eq!(report.claimed, 1);
        assert_eq!(claims[0].token.node_id, "node_independent_first");
        let nodes = owner.projection(Some("run_ownership")).unwrap().nodes;
        for id in ["node_delayed", "node_waiting_dependency", "node_independent_second"] {
            let node = nodes.iter().find(|node| node.node_id == id).unwrap();
            assert_eq!(node.state, "ready");
            assert!(node.lease_id.is_none(), "unscheduled work must not gain a lease");
        }
    }

    #[test]
    fn admission_rechecks_prerequisites_even_when_assignment_is_ready() {
        for parent_edge in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
            owner.define_node(NodeSpec { node_id: Some("node_child".into()), owner_agent_id: Some("coder".into()),
                idempotency_key: Some("child".into()), run_id: "run_ownership".into(),
                parent_id: parent_edge.then(|| "node_ownership".into()),
                title: "Consume prerequisite".into(), outcome: "Verified dependent result".into(),
                phase: WorkflowPhase::Execute,
                dependencies: if parent_edge { vec![] } else { vec!["node_ownership".into()] },
                budget: WorkflowBudget::default(), evidence_requirements: vec![] }).unwrap();
            // Readiness may have been set by another decision or an earlier
            // dependency snapshot. Admission must check the actual inputs.
            owner.transition_node("node_child", "child-ready", None, WorkflowPhase::Execute,
                WorkflowNodeState::Ready, RestartState::Waiting, "Ready decision", None, None, None, None).unwrap();
            let child = owner.projection(Some("run_ownership")).unwrap().nodes.into_iter()
                .find(|node| node.node_id == "node_child").unwrap();
            assert!(owner.claim(&child, "coder", Utc::now()).unwrap().is_none(),
                "ready flag cannot bypass an unfinished prerequisite");
            owner.transition_node("node_ownership", "upstream-review", Some(&lease.token), WorkflowPhase::Review,
                WorkflowNodeState::Review, RestartState::Fresh, "Review", None, None, None, None).unwrap();
            owner.transition_node("node_ownership", "upstream-done", Some(&lease.token), WorkflowPhase::Commit,
                WorkflowNodeState::Succeeded, RestartState::Terminal, "Done", None, None, None, None).unwrap();
            assert!(owner.claim(&child, "coder", Utc::now()).unwrap().is_some(),
                "the unchanged child can start once its actual prerequisite succeeds");
        }
    }

    #[test]
    fn lease_retry_cannot_substitute_its_owner_or_route() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let event = owner.store.events_since(0, 100).unwrap().into_iter()
            .find(|event| matches!(event.envelope.event, CompanyEventKind::WorkflowNodeLeased { .. })).unwrap();
        let retry = event.envelope.clone();
        assert_eq!(owner.store.claim_workflow_node(retry.clone(), 0, 4, 4, &Utc::now().to_rfc3339())
            .unwrap().unwrap().event_id, event.event_id);
        for route in [false, true] {
            let mut changed = retry.clone();
            if route { changed.session_id = "other-room".into(); }
            else if let CompanyEventKind::WorkflowNodeLeased { worker_id, .. } = &mut changed.event {
                *worker_id = "researcher".into();
            }
            assert!(owner.store.claim_workflow_node(changed, 0, 4, 4, &Utc::now().to_rfc3339()).is_err(),
                "same retry key must not authorize a different request");
        }
        assert!(owner.store.claim_workflow_node(retry.clone(), 0, 4, 4, &lease.expires_at)
            .unwrap().is_none(), "expiry boundary must retire replay authority");
        owner.heartbeat(&lease.token, Utc::now() + chrono::Duration::seconds(30)).unwrap();
        assert!(owner.store.claim_workflow_node(retry, 0, 4, 4, &lease.expires_at)
            .unwrap().is_some(), "a valid heartbeat extends the same lease, not a new execution");
    }

    #[test]
    fn lease_retry_cannot_resurrect_released_execution() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let retry = owner.store.events_since(0, 100).unwrap().into_iter()
            .find(|event| matches!(event.envelope.event, CompanyEventKind::WorkflowNodeLeased { .. })).unwrap().envelope;
        owner.transition_node("node_ownership", "release-before-retry", Some(&lease.token),
            WorkflowPhase::Execute, WorkflowNodeState::Ready, RestartState::Waiting,
            "Released", None, None, None, None).unwrap();
        let before = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert!(owner.store.claim_workflow_node(retry, 0, 4, 4, &Utc::now().to_rfc3339())
            .unwrap().is_none(), "historical lease receipt is not current execution authority");
        let after = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(before.as_of_seq, after.as_of_seq);
    }

    #[test]
    fn stale_assignment_snapshot_cannot_claim_a_newer_ready_revision() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let stale = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        owner.transition_node("node_ownership", "updated-readiness", Some(&lease.token),
            WorkflowPhase::Execute, WorkflowNodeState::Ready, RestartState::Waiting,
            "Updated assignment is ready", None, None, None, None).unwrap();
        let current = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_ne!(stale.as_of_seq, current.as_of_seq);
        assert!(owner.claim(&stale, "coder", Utc::now()).unwrap().is_none(),
            "claim must reload after the assignment revision changes");
        let unchanged = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(unchanged.as_of_seq, current.as_of_seq);
        assert!(owner.claim(&current, "coder", Utc::now()).unwrap().is_some());
    }

    fn fixture(path: &std::path::Path) -> (DurableWorkflowScheduler, LeasedNode) {
        fixture_with_requirements(path, vec![])
    }

    fn fixture_with_requirements(path: &std::path::Path, evidence_requirements: Vec<EvidenceRequirement>) -> (DurableWorkflowScheduler, LeasedNode) {
        let store = Arc::new(CompanyStore::open(path).unwrap());
        store.ensure_full_catalog_team().unwrap();
        let scheduler = DurableWorkflowScheduler::new(store, ConcurrencyPolicy::default()).unwrap();
        let goal = scheduler.create_goal(GoalRequest {
            goal_id: Some("goal_ownership".into()), idempotency_key: Some("ownership-goal".into()),
            contract: DurableWorkflowContract {
                title: "Ownership fixture".into(), objective: "Do not steal a live worker".into(),
                budget: WorkflowBudget::default(), evidence_requirements: vec![],
                concurrency: ConcurrencyPolicy::default(), metadata: Default::default(),
                ownership: WorkflowOwnership { scope: WorkflowScope::Agent,
                    owner_agent_id: "coder".into(), group_id: None },
            },
        }).unwrap();
        scheduler.open_run(RunRequest { run_id: Some("run_ownership".into()),
            idempotency_key: Some("ownership-run".into()), goal_id: goal,
            budget: WorkflowBudget::default(), next_wake_at: None }).unwrap();
        scheduler.define_node(NodeSpec { node_id: Some("node_ownership".into()),
            owner_agent_id: None,
            idempotency_key: Some("ownership-node".into()), run_id: "run_ownership".into(),
            parent_id: None, title: "Implement".into(), outcome: "Verified artifact".into(),
            phase: WorkflowPhase::Execute, dependencies: vec![], budget: WorkflowBudget::default(),
            evidence_requirements }).unwrap();
        let (_, mut leases) = scheduler.tick("run_ownership", "coder", Utc::now()).unwrap();
        assert_eq!(leases.len(), 1);
        (scheduler, leases.remove(0))
    }

    #[test]
    fn receipt_waits_release_capacity_preserve_results_and_resume_only_the_matching_task() {
        for responder in ["user", "researcher"] {
            let dir = tempfile::tempdir().unwrap();
            let (mut owner, lease) = fixture(&dir.path().join("company.sqlite"));
            owner.policy.max_active_workers = 1;
            owner.policy.max_active_per_run = 1;
            owner.policy.max_claims_per_tick = 1;
            owner.transition_node("node_ownership", "partial", Some(&lease.token), WorkflowPhase::Execute,
                WorkflowNodeState::Running, RestartState::Fresh, "partial artifact", None,
                Some(serde_json::json!({"artifact":"retained"})), None, None).unwrap();
            for (id, dependencies) in [("node_independent", vec![]), ("node_dependent", vec!["node_ownership".into()])] {
                owner.define_node(NodeSpec { node_id: Some(id.into()), owner_agent_id: Some("coder".into()),
                    idempotency_key: Some(id.into()), run_id: "run_ownership".into(), parent_id: None,
                    title: id.into(), outcome: "Deliver result".into(), phase: WorkflowPhase::Execute,
                    dependencies, budget: WorkflowBudget::default(), evidence_requirements: vec![] }).unwrap();
            }
            let receipt = WorkflowWaitReceipt { receipt_id: "question-first".into(), responder: responder.into(),
                question: "Which source revision?".into(), answer: None };
            owner.wait_for(&lease.token, receipt.clone()).unwrap();
            owner.wait_for(&lease.token, receipt.clone()).unwrap();
            let (_, independent) = owner.tick("run_ownership", "coder", Utc::now()).unwrap();
            assert_eq!(independent.len(), 1, "waiting must release the only execution slot");
            assert_eq!(independent[0].token.node_id, "node_independent");
            let snapshot = owner.store.workflow_snapshot(Some("run_ownership")).unwrap();
            let waiting = snapshot.nodes.iter().find(|n| n.node_id == "node_ownership").unwrap();
            assert_eq!(waiting.state, if responder == "user" { "waiting_user" } else { "waiting_peer" });
            assert!(waiting.lease_id.is_none());
            assert_eq!(waiting.result_json.as_deref(), Some("{\"artifact\":\"retained\"}"));
            assert_eq!(snapshot.nodes.iter().find(|n| n.node_id == "node_dependent").unwrap().state, "pending");
            assert!(owner.resolve_wait("node_ownership", "wrong-question", responder, "v2").is_err());
            assert!(owner.resolve_wait("node_ownership", "question-first", "someone-else", "v2").is_err());
            assert!(owner.transition_node("node_ownership", "bypass", None, WorkflowPhase::Execute,
                WorkflowNodeState::Ready, RestartState::Fresh, "skip answer", None, None, None, None).is_err());
            assert!(owner.resolve_wait("node_ownership", "question-first", responder, "v2").unwrap());
            let revision = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().as_of_seq;
            assert!(!owner.resolve_wait("node_ownership", "question-first", responder, "v2").unwrap());
            assert_eq!(owner.store.workflow_snapshot(Some("run_ownership")).unwrap().as_of_seq, revision);
            assert!(owner.resolve_wait("node_ownership", "question-first", responder, "replace answer").is_err());
            owner.transition_node("node_independent", "independent-end", Some(&independent[0].token),
                WorkflowPhase::Execute, WorkflowNodeState::Canceled, RestartState::Terminal,
                "fixture ends", None, None, None, None).unwrap();
            let (_, resumed) = owner.tick("run_ownership", "coder", Utc::now()).unwrap();
            assert_eq!(resumed.len(), 1);
            assert_eq!(resumed[0].token.node_id, "node_ownership");
            assert_ne!(resumed[0].token.lease_id, lease.token.lease_id);
            assert!(owner.heartbeat(&lease.token, Utc::now()).is_err());
            assert!(owner.wait_for(&lease.token, WorkflowWaitReceipt { receipt_id: "stale-lease-wait".into(),
                responder: responder.into(), question: "Stale worker question".into(), answer: None }).is_err());
            assert!(owner.wait_for(&resumed[0].token, receipt).is_err());
            owner.wait_for(&resumed[0].token, WorkflowWaitReceipt { receipt_id: "question-second".into(),
                responder: responder.into(), question: "Which output format?".into(), answer: None }).unwrap();
            assert!(owner.resolve_wait("node_ownership", "question-first", responder, "v2").is_err());
            owner.transition_node("node_ownership", "cancel-wait", None, WorkflowPhase::Execute,
                WorkflowNodeState::Canceled, RestartState::Terminal, "user canceled", None, None, None, None).unwrap();
            assert!(owner.resolve_wait("node_ownership", "question-second", responder, "pdf").is_err());
        }
    }

    #[test]
    fn competing_wait_answers_preserve_one_immutable_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, lease) = fixture(&path);
        owner.wait_for(&lease.token, WorkflowWaitReceipt { receipt_id: "question-race".into(),
            responder: "researcher".into(), question: "Select version".into(), answer: None }).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8).map(|i| {
            let other = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
                ConcurrencyPolicy::default()).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || { barrier.wait();
                other.resolve_wait("node_ownership", "question-race", "researcher", &format!("v{i}"))
            })
        }).collect();
        let winners = handles.into_iter().filter_map(|h| h.join().unwrap().ok()).filter(|v| *v).count();
        assert_eq!(winners, 1);
        let node = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(node.state, "ready");
        let receipt: WorkflowWaitReceipt = serde_json::from_str(node.wait_json.as_deref().unwrap()).unwrap();
        assert!(receipt.answer.unwrap().starts_with('v'));
    }

    #[test]
    fn restart_projection_round_trips_through_the_transition_contract() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _) = fixture(&dir.path().join("company.sqlite"));
        let node = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(node.restart_state, "fresh");
        for value in ["fresh", "new", "running"] {
            let parsed: RestartState = serde_json::from_value(serde_json::json!(value)).unwrap();
            assert_eq!(parsed, RestartState::Fresh);
            assert_eq!(serde_json::to_value(parsed).unwrap(), "fresh");
        }
    }

    #[test]
    fn assignment_column_migration_preserves_legacy_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, _) = fixture(&path);
        let before = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        drop(owner);
        // Model the immediately preceding schema, using only a private fixture.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute("ALTER TABLE workflow_nodes DROP COLUMN owner_agent_id", []).unwrap();
        connection.execute("ALTER TABLE workflow_nodes DROP COLUMN wait_json", []).unwrap();
        drop(connection);
        let reopened = CompanyStore::open(&path).unwrap();
        let after = reopened.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(after.owner_agent_id, None);
        assert_eq!(after.wait_json, None);
        assert_eq!(after.node_id, before.node_id);
        assert_eq!(after.as_of_seq, before.as_of_seq);
        assert_eq!(after.lease_id, before.lease_id);
        assert_eq!(after.state, before.state);
    }

    #[test]
    fn named_assignments_route_only_to_their_owner_even_with_a_stale_caller() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, _) = fixture(&path);
        let spec = NodeSpec { node_id: Some("node_named".into()), owner_agent_id: Some("researcher".into()),
            idempotency_key: Some("named".into()), run_id: "run_ownership".into(), parent_id: None,
            title: "Research".into(), outcome: "Brief".into(), phase: WorkflowPhase::Execute,
            dependencies: vec![], budget: WorkflowBudget::default(), evidence_requirements: vec![] };
        owner.define_node(spec.clone()).unwrap();
        let mut reassigned = spec;
        reassigned.owner_agent_id = Some("coder".into());
        assert!(owner.define_node(reassigned).is_err(), "retry must not silently change ownership");
        let (_, leases) = owner.tick("run_ownership", "coder", Utc::now()).unwrap();
        assert!(leases.is_empty());
        let mut node = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.into_iter()
            .find(|node| node.node_id == "node_named").unwrap();
        assert_eq!(node.owner_agent_id.as_deref(), Some("researcher"));
        // An old client can omit the new field; the database is authoritative.
        node.owner_agent_id = None;
        assert!(owner.claim(&node, "coder", Utc::now()).unwrap().is_none());
        let other = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
            ConcurrencyPolicy::default()).unwrap();
        let (_, leases) = other.tick("run_ownership", "researcher", Utc::now()).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].token.node_id, "node_named");
        assert_eq!(leases[0].token.worker_id, "researcher");
    }

    #[test]
    fn named_assignment_checks_group_membership_again_at_claim_time() {
        use crate::runtime::company_directory::{GroupProfile, LifecycleState};
        let dir = tempfile::tempdir().unwrap();
        let (owner, _) = fixture(&dir.path().join("company.sqlite"));
        owner.store.create_group("phoenix", GroupProfile {
            group_id: "assignment-group".into(), name: "Assignment fixture".into(),
            description: "Synthetic routing test".into(), color: "#334455".into(),
            icon_seed: "assignment".into(), lifecycle: LifecycleState::Active, pinned: false,
            sort_order: 0, canonical_session_id: Some("group-assignment-fixture".into()), metadata_json: "{}".into(),
            leader_agent_id: None,
        }, vec!["coder".into(), "researcher".into()]).unwrap();
        let goal = owner.create_goal(GoalRequest { goal_id: Some("goal_named_group".into()),
            idempotency_key: Some("named-group".into()), contract: DurableWorkflowContract {
                title: "Group routing".into(), objective: "Only assigned members claim".into(),
                budget: WorkflowBudget::default(), evidence_requirements: vec![],
                concurrency: ConcurrencyPolicy::default(), metadata: Default::default(),
                ownership: WorkflowOwnership { scope: WorkflowScope::Group,
                    owner_agent_id: "coder".into(), group_id: Some("assignment-group".into()) },
            } }).unwrap();
        owner.open_run(RunRequest { run_id: Some("run_named_group".into()),
            idempotency_key: Some("named-group-run".into()), goal_id: goal,
            budget: WorkflowBudget::default(), next_wake_at: None }).unwrap();
        let mut spec = NodeSpec { node_id: Some("node_named_group".into()),
            owner_agent_id: Some("finance".into()), idempotency_key: Some("named-group-node".into()),
            run_id: "run_named_group".into(), parent_id: None, title: "Brief".into(),
            outcome: "Research brief".into(), phase: WorkflowPhase::Execute, dependencies: vec![],
            budget: WorkflowBudget::default(), evidence_requirements: vec![] };
        assert!(owner.define_node(spec.clone()).is_err(), "outsider assignment must fail");
        spec.owner_agent_id = None;
        assert!(owner.define_node(spec.clone()).is_err(), "new group tasks need an explicit owner");
        spec.owner_agent_id = Some("researcher".into());
        owner.define_node(spec).unwrap();
        owner.reconcile("run_named_group", Utc::now()).unwrap();
        let node = owner.store.workflow_snapshot(Some("run_named_group")).unwrap().nodes.remove(0);
        let (_, selected) = owner.store.workflow_context("group-assignment-fixture__researcher", "researcher").unwrap();
        assert_eq!(selected.runs.len(), 1);
        assert_eq!(selected.runs[0].run_id, "run_named_group");
        let (_, coordinator) = owner.store.workflow_context("group-assignment-fixture__coder", "coder").unwrap();
        assert_eq!(coordinator.runs.len(), 1, "private coordinator workflow must stay outside the room");
        assert_eq!(coordinator.runs[0].run_id, "run_named_group");
        assert!(owner.store.workflow_context("agent-researcher", "researcher").unwrap().1.runs.is_empty(),
            "group assignments must not enter a private chat");
        assert!(owner.store.workflow_context("group-assignment-fixture__finance", "finance").unwrap().1.runs.is_empty());
        owner.store.set_group_members("phoenix", "assignment-group", ["coder", "finance"].into_iter()
            .map(|agent| crate::runtime::company::GroupMemberInput { agent_id: agent.into(),
                member_role: "member".into(), history_access: crate::runtime::company_directory::HistoryAccess::Full })
            .collect()).unwrap();
        assert!(owner.store.workflow_context("group-assignment-fixture__researcher", "researcher").unwrap().1.runs.is_empty(),
            "removed membership must revoke future room context");
        assert!(owner.claim(&node, "researcher", Utc::now()).unwrap().is_none(),
            "removed member cannot claim a previously assigned task");
        assert!(owner.claim(&node, "coder", Utc::now()).unwrap().is_none(),
            "remaining member cannot silently take over");
        owner.store.set_group_lifecycle("phoenix", "assignment-group", LifecycleState::Archived).unwrap();
        assert!(owner.store.workflow_context("group-assignment-fixture__coder", "coder").unwrap().1.runs.is_empty(),
            "archived room must not fall back to the coordinator's private workflows");
        assert!(owner.store.workflow_context("group-assignment-fixture", "coder").unwrap().1.runs.is_empty(),
            "canonical archived room must also remain isolated");
        assert!(!owner.store.workflow_context("agent-coder", "coder").unwrap().1.runs.is_empty(),
            "archiving a room must not hide the coordinator's private work");
    }

    #[test]
    fn stale_workflow_transition_cannot_resurrect_canceled_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, lease) = fixture(&path);
        owner.transition_node("node_ownership", "release", Some(&lease.token),
            WorkflowPhase::Execute, WorkflowNodeState::Ready, RestartState::Waiting,
            "waiting", None, None, None, None).unwrap();
        let stale = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        let other = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
            ConcurrencyPolicy::default()).unwrap();
        other.transition_node("node_ownership", "cancel", None,
            WorkflowPhase::Execute, WorkflowNodeState::Canceled, RestartState::Terminal,
            "user canceled", None, None, None, None).unwrap();
        let result = owner.append_node_state(&stale, WorkflowPhase::Execute,
            WorkflowNodeState::Ready, RestartState::Waiting, "old readiness decision".into(),
            None, None, None, None, None, None, true, "workflow:stale-ready".into(), &[]);
        assert!(result.is_err(), "stale readiness must not overwrite cancellation");
        let current = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(current.state, "canceled");
    }

    #[test]
    fn competing_workflow_decisions_have_one_revision_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, lease) = fixture(&path);
        let stale = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8).map(|i| {
            let scheduler = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
                ConcurrencyPolicy::default()).unwrap();
            let node = stale.clone();
            let token = lease.token.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                scheduler.append_node_state(&node, WorkflowPhase::Execute,
                    WorkflowNodeState::Running, RestartState::Fresh, format!("decision {i}"),
                    None, None, None, None, Some(token.lease_id), Some(token.fencing_token),
                    false, format!("workflow:competing:{i}"), &[]).is_ok()
            })
        }).collect();
        let winners = handles.into_iter()
            .map(|handle| usize::from(handle.join().unwrap())).sum::<usize>();
        assert_eq!(winners, 1, "one validated revision may commit only one competing decision");
        assert_eq!(owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes[0].state, "running");
    }

    #[test]
    fn identical_workflow_decision_retry_is_idempotent_but_conflicting_retry_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, lease) = fixture(&dir.path().join("company.sqlite"));
        let node = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        let commit = |reason: &str| owner.append_node_state(&node, WorkflowPhase::Execute,
            WorkflowNodeState::Running, RestartState::Fresh, reason.into(), None, None, None, None,
            Some(lease.token.lease_id.clone()), Some(lease.token.fencing_token.clone()),
            false, "workflow:decision-retry".into(), &[]);
        commit("start").unwrap();
        let first = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        commit("start").unwrap();
        assert!(commit("different instruction").is_err());
        let after = owner.store.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(after.as_of_seq, first.as_of_seq);
        assert_eq!(after.reason, "start");
    }

    #[test]
    fn another_connection_cannot_recover_or_claim_a_live_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, lease) = fixture(&path);
        let other = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
            ConcurrencyPolicy::default()).unwrap();
        let node = other.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        assert!(other.claim(&node, "researcher", Utc::now()).unwrap().is_none(),
            "a connection ID mismatch must not steal a live lease");
        let (report, leases) = other.tick("run_ownership", "researcher", Utc::now()).unwrap();
        assert_eq!(report.recovered, 0);
        assert!(leases.is_empty());
        owner.heartbeat(&lease.token, Utc::now()).unwrap();
        drop(owner);
        let (report, leases) = other.tick("run_ownership", "researcher", Utc::now()).unwrap();
        assert_eq!(report.recovered, 1, "a released owner can be recovered immediately");
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt, 2);
        assert_ne!(lease.token.fencing_token, leases[0].token.fencing_token);
        assert!(other.heartbeat(&lease.token, Utc::now()).is_err());
    }

    #[test]
    #[ignore = "subprocess helper; only launched with a private fixture path"]
    fn workflow_owner_child() {
        use std::io::{Read, Write};
        let path = std::env::var_os("PHOENIX_WORKFLOW_OWNER_FIXTURE").expect("fixture path");
        let (_owner, _lease) = fixture(std::path::Path::new(&path));
        println!("WORKFLOW_OWNED");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    #[test]
    fn recovery_snapshot_cannot_discard_a_renewed_lease() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, lease) = fixture(&path);
        let observer = CompanyStore::open(&path).unwrap();
        let old = observer.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        let recovery_time = parse_time(&lease.expires_at, "expiry").unwrap() + chrono::Duration::seconds(1);
        let now = recovery_time.to_rfc3339();
        assert!(observer.workflow_lease_recoverable(old.lease_runtime_epoch.as_deref(),
            old.lease_expires_at.as_deref(), &now).unwrap());
        // Same lease ID and fence, but a heartbeat has extended its deadline.
        owner.heartbeat(&lease.token, recovery_time - chrono::Duration::seconds(2)).unwrap();
        assert!(!observer.recover_workflow_node(&old, recovery_event(&old, &now).unwrap(), &now).unwrap());
        let current = observer.workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(current.lease_id.as_deref(), Some(lease.token.lease_id.as_str()));
        assert_ne!(old.as_of_seq, current.as_of_seq);
        let expired = (recovery_time + chrono::Duration::seconds(91)).to_rfc3339();
        assert!(observer.recover_workflow_node(&current, recovery_event(&current, &expired).unwrap(), &expired).unwrap());
        assert!(!observer.recover_workflow_node(&current, recovery_event(&current, &expired).unwrap(), &expired).unwrap());
    }

    #[test]
    fn concurrent_schedulers_admit_only_one_recovery_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let (owner, _) = fixture(&path);
        let node = owner.projection(Some("run_ownership")).unwrap().nodes.remove(0);
        let stores: Vec<_> = (0..8).map(|_| Arc::new(CompanyStore::open(&path).unwrap())).collect();
        drop(owner);
        let barrier = Arc::new(std::sync::Barrier::new(stores.len()));
        let handles: Vec<_> = stores.iter().map(|store| {
            let store = store.clone();
            let barrier = barrier.clone();
            let node = node.clone();
            std::thread::spawn(move || {
                let scheduler = DurableWorkflowScheduler::new(store, ConcurrencyPolicy::default()).unwrap();
                barrier.wait();
                scheduler.claim(&node, "researcher", Utc::now()).unwrap()
            })
        }).collect();
        let winners: Vec<_> = handles.into_iter().filter_map(|handle| handle.join().unwrap()).collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].attempt, 2);
        let final_node = stores[0].workflow_snapshot(Some("run_ownership")).unwrap().nodes.remove(0);
        assert_eq!(final_node.lease_id.as_deref(), Some(winners[0].token.lease_id.as_str()));
    }

    #[test]
    fn legacy_owner_without_claim_waits_for_its_existing_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let store = CompanyStore::open(dir.path().join("company.sqlite")).unwrap();
        let now = Utc::now();
        let expires = (now + chrono::Duration::seconds(90)).to_rfc3339();
        assert!(!store.workflow_lease_recoverable(Some("epoch_legacy"), Some(&expires), &now.to_rfc3339()).unwrap());
        assert!(!store.workflow_lease_recoverable(None, None, &now.to_rfc3339()).unwrap());
        assert!(store.workflow_lease_recoverable(Some("epoch_legacy"), Some(&expires), &expires).unwrap());
    }

    #[test]
    fn workflow_owner_survives_observer_and_recovers_after_process_death() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("company.sqlite");
        let mut child = Child(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::workflow::ownership_tests::workflow_owner_child", "--ignored", "--nocapture"])
            .env("PHOENIX_WORKFLOW_OWNER_FIXTURE", &path).stdin(Stdio::piped())
            .stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap());
        let mut ready = false;
        for line in std::io::BufReader::new(child.0.stdout.take().unwrap()).lines() {
            if line.unwrap() == "WORKFLOW_OWNED" { ready = true; break; }
        }
        assert!(ready, "fixture owner failed before acquiring its lease");
        let observer = DurableWorkflowScheduler::new(Arc::new(CompanyStore::open(&path).unwrap()),
            ConcurrencyPolicy::default()).unwrap();
        let before = observer.projection(Some("run_ownership")).unwrap();
        let (report, leases) = observer.tick("run_ownership", "researcher", Utc::now()).unwrap();
        assert_eq!(report.recovered, 0);
        assert!(leases.is_empty());
        assert_eq!(observer.projection(Some("run_ownership")).unwrap().as_of_seq, before.as_of_seq);
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        let (report, leases) = observer.tick("run_ownership", "researcher", Utc::now()).unwrap();
        assert_eq!(report.recovered, 1);
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt, 2);
        println!("PROCESS_RECOVERY_OK: observer preserved live lease; killed owner recovered once");
    }
}
