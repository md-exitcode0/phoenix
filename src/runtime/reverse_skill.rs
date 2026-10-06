//! Evidence-gated reverse-skill extraction and promotion.
//!
//! A reverse skill is a small, reusable `SKILL.md` playbook derived from a
//! workflow Phoenix actually completed.  This module is intentionally a
//! promotion pipeline, not a text summarizer: a successful run trace, a
//! verified company job, and a content-addressed file artifact are required
//! before a candidate can be proposed.  Review, held-out replay, and canary
//! receipts are durable gates before an atomic publish.
//!
//! The state document is a resumable coordinator; the company event stream
//! remains the source of truth for run/job/artifact/reviewer evidence.  A
//! candidate never copies an artifact or a transcript into a skill.  Tool
//! observations are normalized to placeholders and only a short, bounded
//! procedure is published.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::company::{
    CompanyEvent, CompanyEventKind, CompanyStore, LearningKind, NewCompanyEvent,
};
use crate::runtime::trace::{RunTrace, RUN_TRACE_SCHEMA_VERSION};

const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CANDIDATE_BODY_BYTES: usize = 48 * 1024;
const MAX_DESCRIPTION_BYTES: usize = 1_024;
const MAX_CANDIDATE_ID_BYTES: usize = 128;
const MAX_RUN_IDS: usize = 256;
const MAX_SOURCE_RUNS: usize = 128;
const MAX_CANARY_RUNS: usize = 32;
const MIN_CANARY_RUNS: usize = 3;
const MAX_REVIEWERS: usize = 32;
const MAX_ARTIFACTS_PER_RUN: usize = 64;
const MAX_TOOL_STEPS: usize = 256;
const MAX_TRACE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TRACE_ENTRIES: usize = 10_000;
const MAX_COMPANY_EVENTS: usize = 100_000;
const MAX_COMPANY_EVENT_BYTES: usize = 128 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BENCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_BENCH_TASKS: usize = 256;
const MAX_CARD_RECEIPT_BYTES: usize = 2_048;

pub const REVERSE_SKILL_SCHEMA_VERSION: u32 = 1;

/// Durable stage of a reverse-skill candidate.  Stages only move forward
/// except for an explicit rollback, and each transition is persisted before
/// returning to the caller.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReverseSkillStage {
    Observed,
    Proposed,
    Reviewed,
    Canaried,
    Published,
    RolledBack,
    Blocked,
}

impl ReverseSkillStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Proposed => "proposed",
            Self::Reviewed => "reviewed",
            Self::Canaried => "canaried",
            Self::Published => "published",
            Self::RolledBack => "rolled_back",
            Self::Blocked => "blocked",
        }
    }
}

/// Request for the observation/proposal stage.  `run_ids` may be empty, in
/// which case the bounded trace directory is searched.  Setting
/// `explicit_request` permits one repeated route to be promoted by explicit
/// user direction, but never bypasses successful trace/job/artifact checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReverseSkillRequest {
    #[serde(default)]
    pub candidate_id: Option<String>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub run_ids: Vec<String>,
    #[serde(default)]
    pub explicit_request: bool,
    #[serde(default = "default_actor")]
    pub producer: String,
}

/// A reviewer receipt identifies a durable peer decision.  A reviewer cannot
/// be accepted merely because its name was typed into a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerReceipt {
    pub reviewer_id: String,
    #[serde(default)]
    pub decision_id: Option<String>,
}

/// Input for a held-out replay/bench gate.  The report is read and hashed by
/// Phoenix; callers cannot assert success by setting a boolean in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReceiptInput {
    #[serde(default = "default_bench_path")]
    pub path: PathBuf,
    pub label: String,
    #[serde(default = "default_min_bench_tasks")]
    pub min_tasks: usize,
}

/// Input for the canary gate.  Each run id must independently resolve to a
/// successful trace, verified job, and verified file artifact in the company
/// event stream.  At least three distinct runs are required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryInput {
    pub run_ids: Vec<String>,
    pub bench: BenchReceiptInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedArtifact {
    pub artifact_id: String,
    /// Normalized workspace-relative path; no host path is persisted.
    pub path: String,
    pub content_hash: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedRunEvidence {
    pub run_id: String,
    pub trace_hash: String,
    pub job_ids: Vec<String>,
    pub artifacts: Vec<VerifiedArtifact>,
    /// Normalized successful tool route used to cluster repeated workflows.
    pub route: Vec<String>,
    pub pattern_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedBenchReceipt {
    pub path: String,
    pub label: String,
    pub content_hash: String,
    pub passed_tasks: usize,
    pub total_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReverseSkillState {
    pub schema_version: u32,
    pub candidate_id: String,
    pub name: String,
    pub description: String,
    pub producer: String,
    pub explicit_request: bool,
    pub stage: ReverseSkillStage,
    pub created_at: String,
    pub updated_at: String,
    pub pattern_key: String,
    pub route: Vec<String>,
    pub body_hash: String,
    pub body: String,
    pub source_runs: Vec<VerifiedRunEvidence>,
    #[serde(default)]
    pub reviewers: Vec<ReviewerReceipt>,
    #[serde(default)]
    pub canary_runs: Vec<VerifiedRunEvidence>,
    #[serde(default)]
    pub bench: Option<VerifiedBenchReceipt>,
    #[serde(default)]
    pub learning_event_id: Option<String>,
    #[serde(default)]
    pub promotion_event_id: Option<String>,
    #[serde(default)]
    pub published_path: Option<String>,
    #[serde(default)]
    pub rollback_path: Option<String>,
    #[serde(default)]
    pub card_receipt: Option<String>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

impl ReverseSkillState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.stage,
            ReverseSkillStage::Published
                | ReverseSkillStage::RolledBack
                | ReverseSkillStage::Blocked
        )
    }
}

pub struct ReverseSkillPipeline {
    workspace_root: PathBuf,
    runs_dir: PathBuf,
    store: Arc<CompanyStore>,
}

impl ReverseSkillPipeline {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        runs_dir: impl Into<PathBuf>,
        store: Arc<CompanyStore>,
    ) -> Result<Self> {
        let workspace_root = workspace_root.into();
        let runs_dir = runs_dir.into();
        validate_real_directory(&workspace_root, "workspace root")?;
        validate_real_directory(&runs_dir, "run-trace root")?;
        Ok(Self {
            workspace_root,
            runs_dir,
            store,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Observe real evidence and persist an `observed` candidate.  This is
    /// deliberately separate from proposing so a blocked candidate can be
    /// resumed without silently creating a company learning event.
    pub fn observe(&self, request: ReverseSkillRequest) -> Result<ReverseSkillState> {
        validate_request(&request)?;
        let candidate_id = request
            .candidate_id
            .clone()
            .unwrap_or_else(|| format!("learning_{}", uuid::Uuid::new_v4().simple()));
        validate_candidate_id(&candidate_id)?;
        let traces = self.load_requested_traces(&request.run_ids)?;
        let events = read_company_events(&self.store)?;
        let workspace = self.workspace_root.canonicalize()?;
        let mut evidence = Vec::new();
        for trace in traces {
            let Some(execution) = trace.execution.as_ref() else {
                continue;
            };
            if trace.error.is_some() || trace.schema_version > RUN_TRACE_SCHEMA_VERSION {
                continue;
            }
            if !same_directory(&trace.workspace_root, &workspace) {
                continue;
            }
            let run_events = events_for_trace(&events, &trace);
            let jobs = verified_jobs(&run_events);
            if jobs.is_empty() {
                continue;
            }
            let artifacts = verified_artifacts(&self.workspace_root, &run_events)?;
            if artifacts.is_empty() {
                continue;
            }
            let route = execution_route(execution)?;
            if route.is_empty() {
                continue;
            }
            let pattern_key = hash_json(&route)?;
            let trace_path = self.runs_dir.join(&trace.run_id).join("trace.json");
            let trace_hash = hash_regular_file(&trace_path, MAX_TRACE_BYTES)?;
            evidence.push(VerifiedRunEvidence {
                run_id: trace.run_id.clone(),
                trace_hash,
                job_ids: jobs,
                artifacts,
                route,
                pattern_key,
            });
            if evidence.len() >= MAX_SOURCE_RUNS {
                break;
            }
        }
        if evidence.is_empty() {
            bail!(
                "reverse skill is blocked: no successful trace has verified job and file evidence"
            );
        }

        let (pattern_key, repeated) = most_repeated_pattern(&evidence);
        if repeated < 3 && !request.explicit_request {
            bail!(
                "reverse skill is blocked: the verified workflow pattern appears {repeated} time(s); need at least 3 or an explicit request"
            );
        }
        let exemplar_route = evidence
            .iter()
            .find(|item| item.pattern_key == pattern_key)
            .map(|item| item.route.clone())
            .context("selected workflow pattern has no evidence")?;
        let selected = evidence
            .into_iter()
            .filter(|item| item.pattern_key == pattern_key)
            .take(MAX_SOURCE_RUNS)
            .collect::<Vec<_>>();
        let body = render_skill_body(&request.name, &request.description, &exemplar_route)?;
        let body_hash = sha256_bytes(body.as_bytes());
        let now = Utc::now().to_rfc3339();
        let state = ReverseSkillState {
            schema_version: REVERSE_SKILL_SCHEMA_VERSION,
            candidate_id,
            name: request.name,
            description: normalize_text(&request.description)?,
            producer: request.producer,
            explicit_request: request.explicit_request,
            stage: ReverseSkillStage::Observed,
            created_at: now.clone(),
            updated_at: now,
            pattern_key,
            route: exemplar_route,
            body_hash,
            body,
            source_runs: selected,
            reviewers: Vec::new(),
            canary_runs: Vec::new(),
            bench: None,
            learning_event_id: None,
            promotion_event_id: None,
            published_path: None,
            rollback_path: None,
            card_receipt: None,
            blocked_reason: None,
        };
        self.persist(&state)?;
        Ok(state)
    }

    /// Stage a `LearningProposed` event using the existing company
    /// `propose_learning` contract and `LearningKind::SkillPatch` evidence.
    pub fn propose(&self, state: &mut ReverseSkillState) -> Result<()> {
        require_stage(state, ReverseSkillStage::Observed)?;
        let evidence = learning_evidence(state);
        let event = self.store.append(NewCompanyEvent {
            run_id: state.candidate_id.clone(),
            session_id: state.candidate_id.clone(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(state.producer.clone()),
            agent_instance_id: Some(state.producer.clone()),
            causation_id: None,
            correlation_id: Some(state.candidate_id.clone()),
            idempotency_key: Some(format!("reverse-skill:proposal:{}", state.candidate_id)),
            event: CompanyEventKind::LearningProposed {
                candidate_id: state.candidate_id.clone(),
                learning_kind: LearningKind::SkillPatch,
                statement: format!(
                    "Proven workflow route for `{}` extracted from verified Phoenix evidence",
                    state.name
                ),
                evidence,
                producer: state.producer.clone(),
            },
        })?;
        state.learning_event_id = Some(event.event_id);
        transition(state, ReverseSkillStage::Proposed);
        self.persist(state)
    }

    /// Accept only two or more independent durable peer decisions.  The
    /// decision records are checked in the company stream; typed reviewer
    /// names without matching evidence fail honestly.
    pub fn review(
        &self,
        state: &mut ReverseSkillState,
        reviewers: Vec<ReviewerReceipt>,
    ) -> Result<()> {
        require_stage(state, ReverseSkillStage::Proposed)?;
        validate_reviewer_receipts(&reviewers)?;
        let events = read_company_events(&self.store)?;
        let decisions = reviewer_decisions(&events, &state.candidate_id, &state.producer);
        let mut used_decisions = HashSet::new();
        for receipt in &reviewers {
            let found = decisions.iter().any(|(decision_id, reviewer_id)| {
                reviewer_id == &receipt.reviewer_id
                    && receipt
                        .decision_id
                        .as_deref()
                        .is_none_or(|expected| expected == decision_id)
                    && used_decisions.insert(decision_id.clone())
            });
            if !found {
                bail!(
                    "reviewer `{}` has no independent accepted DecisionRecorded evidence for candidate `{}`",
                    receipt.reviewer_id,
                    state.candidate_id
                );
            }
        }
        state.reviewers = reviewers;
        transition(state, ReverseSkillStage::Reviewed);
        self.persist(state)
    }

    /// Validate real held-out bench output and three fresh production canary
    /// runs, then append the existing `LearningPromoted` receipt.
    pub fn canary(&self, state: &mut ReverseSkillState, input: CanaryInput) -> Result<()> {
        require_stage(state, ReverseSkillStage::Reviewed)?;
        if input.run_ids.len() < MIN_CANARY_RUNS {
            bail!("reverse skill canary requires at least {MIN_CANARY_RUNS} real run receipts");
        }
        if input.run_ids.len() > MAX_CANARY_RUNS {
            bail!("reverse skill canary has too many run receipts");
        }
        let mut ids = BTreeSet::new();
        for run_id in &input.run_ids {
            validate_run_id(run_id)?;
            if !ids.insert(run_id.clone()) {
                bail!("reverse skill canary run ids must be distinct");
            }
            if state
                .source_runs
                .iter()
                .any(|source| source.run_id == *run_id)
            {
                bail!("canary run `{run_id}` reuses a source run; provide held-out runs");
            }
        }
        let events = read_company_events(&self.store)?;
        let traces = self.load_requested_traces(&input.run_ids)?;
        let trace_by_id: HashMap<String, RunTrace> = traces
            .into_iter()
            .map(|trace| (trace.run_id.clone(), trace))
            .collect();
        let workspace = self.workspace_root.canonicalize()?;
        let mut canary_runs = Vec::new();
        for run_id in &input.run_ids {
            let trace = trace_by_id
                .get(run_id)
                .with_context(|| format!("missing canary trace `{run_id}`"))?;
            if trace.error.is_some() || trace.execution.is_none() {
                bail!("canary trace `{run_id}` is not a successful trace");
            }
            if !same_directory(&trace.workspace_root, &workspace) {
                bail!("canary trace `{run_id}` belongs to a different workspace");
            }
            let run_events = events_for_trace(&events, trace);
            let jobs = verified_jobs(&run_events);
            if jobs.is_empty() {
                bail!("canary `{run_id}` has no verified job settlement");
            }
            let artifacts = verified_artifacts(&self.workspace_root, &run_events)?;
            if artifacts.is_empty() {
                bail!("canary `{run_id}` has no verified file artifact");
            }
            let execution = trace.execution.as_ref().expect("checked above");
            let route = execution_route(execution)?;
            let pattern_key = hash_json(&route)?;
            if pattern_key != state.pattern_key {
                bail!("canary `{run_id}` used a different workflow route than the proposed skill");
            }
            canary_runs.push(VerifiedRunEvidence {
                run_id: run_id.clone(),
                trace_hash: hash_regular_file(
                    &self.runs_dir.join(run_id).join("trace.json"),
                    MAX_TRACE_BYTES,
                )?,
                job_ids: jobs,
                artifacts,
                route,
                pattern_key,
            });
        }
        let bench = verify_bench_receipt(&self.workspace_root, &input.bench)?;
        let receipt = serde_json::json!({
            "schema_version": REVERSE_SKILL_SCHEMA_VERSION,
            "candidate_id": state.candidate_id,
            "canary_run_ids": input.run_ids,
            "bench": bench,
        });
        let receipt_text = serde_json::to_string(&receipt)?;
        if receipt_text.len() > MAX_CARD_RECEIPT_BYTES * 8 {
            bail!("reverse skill promotion receipt is too large");
        }
        let reviewers = state
            .reviewers
            .iter()
            .map(|reviewer| reviewer.reviewer_id.clone())
            .collect::<Vec<_>>();
        let event = self.store.append(NewCompanyEvent {
            run_id: state.candidate_id.clone(),
            session_id: state.candidate_id.clone(),
            pod_id: None,
            work_node_id: None,
            attempt_id: None,
            agent_identity_id: Some(state.producer.clone()),
            agent_instance_id: Some(state.producer.clone()),
            causation_id: state.learning_event_id.clone(),
            correlation_id: Some(state.candidate_id.clone()),
            idempotency_key: Some(format!("reverse-skill:promotion:{}", state.candidate_id)),
            event: CompanyEventKind::LearningPromoted {
                candidate_id: state.candidate_id.clone(),
                reviewers,
                canary_receipt: receipt_text,
            },
        })?;
        state.promotion_event_id = Some(event.event_id);
        state.canary_runs = canary_runs;
        state.bench = Some(bench);
        transition(state, ReverseSkillStage::Canaried);
        self.persist(state)
    }

    /// Write a minimal strict SKILL.md into a private staging directory and
    /// atomically publish it under the workspace skill root.  Existing skills
    /// are moved to a candidate-specific rollback name before replacement;
    /// any failed rename restores the previous destination.
    pub async fn publish(&self, state: &mut ReverseSkillState) -> Result<String> {
        require_stage(state, ReverseSkillStage::Canaried)?;
        validate_skill_name(&state.name)?;
        validate_skill_body(&state.body, &state.name, &state.description)?;
        let workspace_skills = self.workspace_root.join(".phoenix").join("skills");
        prepare_private_directory(&workspace_skills)?;
        let staging_root = workspace_skills.join(".reverse-staging");
        prepare_private_directory(&staging_root)?;
        let stage = staging_root.join(format!("{}-{}", state.name, state.candidate_id));
        validate_component_name(
            stage
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )?;
        create_private_directory(&stage)?;
        let stage_manifest = stage.join("SKILL.md");
        crate::config::private_io::atomic_write_private(&stage_manifest, state.body.as_bytes())?;
        validate_skill_directory(&stage)?;

        // Keep the reviewable candidate in the workspace, then copy it to a
        // hidden directory on the destination filesystem. The final rename is
        // therefore atomic even when the workspace and Phoenix home live on
        // different mounts.
        let skills_root = crate::config::phoenix_home().join("skills");
        prepare_private_directory(&skills_root)?;
        let install_stage = skills_root.join(format!(
            ".reverse-install-{}-{}",
            state.name, state.candidate_id
        ));
        create_private_directory(&install_stage)?;
        crate::config::private_io::atomic_write_private(
            &install_stage.join("SKILL.md"),
            state.body.as_bytes(),
        )?;
        validate_skill_directory(&install_stage)?;
        let destination = skills_root.join(&state.name);
        let rollback = skills_root.join(format!(".reverse-rollback-{}", state.candidate_id));
        let publish_result = crate::config::private_io::with_private_lock(&destination, || {
            ensure_no_symlink(&destination)?;
            ensure_no_symlink(&rollback)?;
            if rollback.exists() {
                bail!("rollback path already exists: {}", rollback.display());
            }
            let had_previous = destination.exists();
            if had_previous {
                validate_skill_directory(&destination)?;
                std::fs::rename(&destination, &rollback).with_context(|| {
                    format!(
                        "staging existing skill for rollback: {}",
                        destination.display()
                    )
                })?;
            }
            if let Err(error) = std::fs::rename(&install_stage, &destination) {
                if had_previous {
                    let _ = std::fs::rename(&rollback, &destination);
                }
                return Err(error).context("atomically publishing reverse skill");
            }
            if let Ok(directory) = std::fs::File::open(&skills_root) {
                directory.sync_all().context("syncing skill root")?;
            }
            Ok(had_previous)
        });
        if let Err(error) = publish_result {
            let _ = std::fs::remove_file(install_stage.join("SKILL.md"));
            let _ = remove_empty_directory(&install_stage);
            return Err(error);
        }
        let _ = std::fs::remove_file(&stage_manifest);
        let _ = remove_empty_directory(&stage);
        state.published_path = Some(format!("$PHOENIX_HOME/skills/{}/SKILL.md", state.name));
        state.rollback_path = Some(format!(
            "$PHOENIX_HOME/skills/.reverse-rollback-{}/SKILL.md",
            state.candidate_id
        ));
        transition(state, ReverseSkillStage::Published);
        let card_receipt = crate::tools::skills::sync_skill_cards(&self.workspace_root).await;
        state.card_receipt = Some(truncate_text(&card_receipt, MAX_CARD_RECEIPT_BYTES));
        self.persist(state)?;
        Ok(state.published_path.clone().unwrap_or_default())
    }

    /// Restore the prior skill, if one existed, while preserving the current
    /// published directory as a quarantine path.  Publishing a brand-new
    /// skill is rolled back by removing only that controlled destination.
    pub async fn rollback(&self, state: &mut ReverseSkillState) -> Result<()> {
        require_stage(state, ReverseSkillStage::Published)?;
        let skills_root = crate::config::phoenix_home().join("skills");
        let destination = skills_root.join(&state.name);
        let rollback = skills_root.join(format!(".reverse-rollback-{}", state.candidate_id));
        crate::config::private_io::with_private_lock(&destination, || {
            ensure_no_symlink(&destination)?;
            ensure_no_symlink(&rollback)?;
            if !destination.exists() {
                bail!(
                    "published reverse skill is missing: {}",
                    destination.display()
                );
            }
            let quarantine = skills_root.join(format!(
                ".reverse-quarantine-{}-{}",
                state.candidate_id,
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::rename(&destination, &quarantine)
                .context("quarantining published reverse skill")?;
            if rollback.exists() {
                if let Err(error) = std::fs::rename(&rollback, &destination) {
                    let _ = std::fs::rename(&quarantine, &destination);
                    return Err(error).context("restoring previous skill during rollback");
                }
            }
            if let Ok(directory) = std::fs::File::open(&skills_root) {
                directory
                    .sync_all()
                    .context("syncing skill root after rollback")?;
            }
            Ok(())
        })?;
        transition(state, ReverseSkillStage::RolledBack);
        let card_receipt = crate::tools::skills::sync_skill_cards(&self.workspace_root).await;
        state.card_receipt = Some(truncate_text(&card_receipt, MAX_CARD_RECEIPT_BYTES));
        self.persist(state)
    }

    pub fn load(&self, candidate_id: &str) -> Result<Option<ReverseSkillState>> {
        validate_candidate_id(candidate_id)?;
        let path = self.state_path(candidate_id);
        let Some(bytes) =
            crate::config::private_io::read_private_file_limited(&path, MAX_STATE_BYTES)?
        else {
            return Ok(None);
        };
        let state: ReverseSkillState = serde_json::from_slice(&bytes)
            .with_context(|| format!("reverse-skill state is corrupt: {}", path.display()))?;
        validate_state_shape(&state)?;
        Ok(Some(state))
    }

    fn persist(&self, state: &ReverseSkillState) -> Result<()> {
        validate_state_shape(state)?;
        let bytes = serde_json::to_vec_pretty(state)?;
        if bytes.len() > MAX_STATE_BYTES {
            bail!("reverse-skill state exceeds {MAX_STATE_BYTES} bytes");
        }
        crate::config::private_io::atomic_write_private(
            &self.state_path(&state.candidate_id),
            &bytes,
        )
    }

    fn state_path(&self, candidate_id: &str) -> PathBuf {
        self.workspace_root
            .join(".phoenix")
            .join("reverse-skill")
            .join(format!("{candidate_id}.json"))
    }

    fn load_requested_traces(&self, requested: &[String]) -> Result<Vec<RunTrace>> {
        let ids = if requested.is_empty() {
            discover_run_ids(&self.runs_dir)?
        } else {
            if requested.len() > MAX_RUN_IDS {
                bail!("too many requested run traces");
            }
            requested.to_vec()
        };
        let mut traces = Vec::new();
        for run_id in ids.into_iter().take(MAX_TRACE_ENTRIES) {
            validate_run_id(&run_id)?;
            let path = self.runs_dir.join(&run_id).join("trace.json");
            let Some(bytes) = read_regular_bytes(&path, MAX_TRACE_BYTES)? else {
                continue;
            };
            let trace: RunTrace = match serde_json::from_slice(&bytes) {
                Ok(trace) => trace,
                Err(error) => {
                    tracing::warn!("reverse skill skipped malformed trace {}: {error}", run_id);
                    continue;
                }
            };
            if trace.run_id != run_id {
                continue;
            }
            traces.push(trace);
        }
        Ok(traces)
    }
}

fn default_actor() -> String {
    "orchestrator".to_string()
}

fn default_bench_path() -> PathBuf {
    PathBuf::from("bench/RESULTS.md")
}

fn default_min_bench_tasks() -> usize {
    1
}

fn validate_request(request: &ReverseSkillRequest) -> Result<()> {
    validate_skill_name(&request.name)?;
    let description = request.description.trim();
    if description.is_empty() || description.len() > MAX_DESCRIPTION_BYTES {
        bail!("reverse skill description must be 1..={MAX_DESCRIPTION_BYTES} bytes");
    }
    if contains_secret_marker(&request.name) || contains_secret_marker(description) {
        bail!("reverse skill metadata appears to contain a secret-bearing value");
    }
    if request.run_ids.len() > MAX_RUN_IDS {
        bail!("reverse skill request includes too many run ids");
    }
    for run_id in &request.run_ids {
        validate_run_id(run_id)?;
    }
    validate_actor(&request.producer)?;
    Ok(())
}

fn validate_state_shape(state: &ReverseSkillState) -> Result<()> {
    if state.schema_version != REVERSE_SKILL_SCHEMA_VERSION {
        bail!(
            "unsupported reverse-skill state schema {}",
            state.schema_version
        );
    }
    validate_candidate_id(&state.candidate_id)?;
    validate_skill_name(&state.name)?;
    if state.description.is_empty() || state.description.len() > MAX_DESCRIPTION_BYTES {
        bail!("reverse skill state has an invalid description");
    }
    validate_actor(&state.producer)?;
    if state.source_runs.is_empty() || state.source_runs.len() > MAX_SOURCE_RUNS {
        bail!("reverse skill state has an invalid source evidence set");
    }
    if state.route.is_empty() || state.route.len() > MAX_TOOL_STEPS {
        bail!("reverse skill state has an invalid route");
    }
    validate_skill_body(&state.body, &state.name, &state.description)?;
    if sha256_bytes(state.body.as_bytes()) != state.body_hash {
        bail!("reverse skill state body hash does not match body");
    }
    if state.reviewers.len() > MAX_REVIEWERS || state.canary_runs.len() > MAX_CANARY_RUNS {
        bail!("reverse skill state has too many reviewers or canaries");
    }
    Ok(())
}

fn validate_reviewer_receipts(receipts: &[ReviewerReceipt]) -> Result<()> {
    if receipts.len() < 2 || receipts.len() > MAX_REVIEWERS {
        bail!("reverse skill promotion requires 2..={MAX_REVIEWERS} reviewers");
    }
    let mut ids = BTreeSet::new();
    for receipt in receipts {
        validate_actor(&receipt.reviewer_id)?;
        if !ids.insert(receipt.reviewer_id.clone()) {
            bail!("reverse skill reviewers must be distinct");
        }
        if let Some(decision_id) = &receipt.decision_id {
            validate_event_id(decision_id)?;
        }
    }
    Ok(())
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.trim().is_empty() || actor.len() > 256 || actor.chars().any(char::is_control) {
        bail!("actor identity is empty, too large, or contains control characters");
    }
    if contains_secret_marker(actor) {
        bail!("actor identity appears to contain a secret-bearing value");
    }
    Ok(())
}

fn validate_candidate_id(candidate_id: &str) -> Result<()> {
    if candidate_id.is_empty() || candidate_id.len() > MAX_CANDIDATE_ID_BYTES {
        bail!("candidate id must be 1..={MAX_CANDIDATE_ID_BYTES} bytes");
    }
    if !candidate_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("candidate id contains unsafe path characters");
    }
    Ok(())
}

fn validate_event_id(event_id: &str) -> Result<()> {
    if event_id.is_empty()
        || event_id.len() > MAX_CANDIDATE_ID_BYTES
        || !event_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("decision id contains unsafe characters");
    }
    Ok(())
}

fn validate_run_id(run_id: &str) -> Result<()> {
    if run_id.is_empty()
        || run_id.len() > 128
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("run id contains unsafe characters");
    }
    Ok(())
}

fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
    {
        bail!("skill name must be 1..=64 bytes with no leading, trailing, or consecutive hyphen");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("skill name may contain only lowercase ASCII letters, digits, and hyphens");
    }
    Ok(())
}

fn validate_component_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 256
        || name == "."
        || name == ".."
        || name
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'/' | b'\\'))
    {
        bail!("unsafe staging path component");
    }
    Ok(())
}

fn validate_skill_body(body: &str, name: &str, description: &str) -> Result<()> {
    if body.is_empty() || body.len() > MAX_CANDIDATE_BODY_BYTES {
        bail!("reverse skill body is empty or exceeds {MAX_CANDIDATE_BODY_BYTES} bytes");
    }
    if !body.starts_with("---\n") || !body.contains("\nname: ") || !body.contains("\ndescription: ")
    {
        bail!("reverse skill body is missing strict YAML frontmatter");
    }
    if body.matches("\n---\n").count() != 1 {
        bail!("reverse skill body must contain one closed frontmatter block");
    }
    if body.contains('\0') || body.chars().any(char::is_control) {
        let allowed = body
            .chars()
            .filter(|ch| !matches!(ch, '\n' | '\r' | '\t'))
            .any(char::is_control);
        if allowed {
            bail!("reverse skill body contains control characters");
        }
    }
    if contains_secret_marker(body) {
        bail!("reverse skill body appears to contain a secret-bearing value");
    }
    let encoded_description = serde_json::to_string(description)?;
    if !body.contains(&format!("name: {name}"))
        || !body.contains(&format!("description: {encoded_description}"))
    {
        bail!("reverse skill frontmatter does not match candidate metadata");
    }
    Ok(())
}

fn require_stage(state: &ReverseSkillState, expected: ReverseSkillStage) -> Result<()> {
    if state.stage != expected {
        bail!(
            "reverse skill `{}` is at stage `{}`, expected `{}`",
            state.candidate_id,
            state.stage.as_str(),
            expected.as_str()
        );
    }
    Ok(())
}

fn transition(state: &mut ReverseSkillState, stage: ReverseSkillStage) {
    state.stage = stage;
    state.updated_at = Utc::now().to_rfc3339();
}

fn learning_evidence(state: &ReverseSkillState) -> Vec<String> {
    let mut evidence = Vec::new();
    for run in &state.source_runs {
        evidence.push(format!("trace:{}@{}", run.run_id, run.trace_hash));
        for job in &run.job_ids {
            evidence.push(format!("job:{job}:verified"));
        }
        for artifact in &run.artifacts {
            evidence.push(format!(
                "artifact:{}:{}@{}",
                artifact.artifact_id, artifact.path, artifact.content_hash
            ));
        }
    }
    evidence.push(format!("route:{}", state.pattern_key));
    evidence
}

fn read_company_events(store: &CompanyStore) -> Result<Vec<CompanyEvent>> {
    let mut events = Vec::new();
    let mut after = 0i64;
    let mut bytes = 0usize;
    loop {
        let page = store.events_since(after, 1_000)?;
        if page.is_empty() {
            break;
        }
        for event in page {
            after = event.company_seq;
            bytes = bytes
                .checked_add(serde_json::to_vec(&event)?.len())
                .context("company evidence byte count overflow")?;
            if bytes > MAX_COMPANY_EVENT_BYTES {
                bail!("company evidence exceeds {MAX_COMPANY_EVENT_BYTES} bytes");
            }
            events.push(event);
            if events.len() > MAX_COMPANY_EVENTS {
                bail!("company evidence exceeds {MAX_COMPANY_EVENTS} events");
            }
        }
    }
    Ok(events)
}

fn verified_jobs(events: &[&CompanyEvent]) -> Vec<String> {
    let mut jobs = Vec::new();
    for event in events {
        if let CompanyEventKind::JobSettled {
            job_id,
            ok: true,
            verified: true,
            ..
        } = &event.envelope.event
        {
            jobs.push(job_id.clone());
        }
    }
    jobs.sort();
    jobs.dedup();
    jobs
}

fn events_for_trace<'a>(events: &'a [CompanyEvent], trace: &RunTrace) -> Vec<&'a CompanyEvent> {
    let mut identities = HashSet::from([trace.run_id.as_str()]);
    if let Some(execution) = trace.execution.as_ref() {
        identities.insert(execution.main_session_id.as_str());
        identities.insert(execution.specialist_session_id.as_str());
    }
    events
        .iter()
        .filter(|event| {
            identities.contains(event.envelope.run_id.as_str())
                || identities.contains(event.envelope.session_id.as_str())
        })
        .collect()
}

fn verified_artifacts(workspace: &Path, events: &[&CompanyEvent]) -> Result<Vec<VerifiedArtifact>> {
    let mut artifacts = Vec::new();
    for event in events {
        let CompanyEventKind::ArtifactPublished {
            artifact_id,
            path,
            content_hash,
            ..
        } = &event.envelope.event
        else {
            continue;
        };
        let (resolved, relative) = resolve_workspace_artifact(workspace, path)?;
        let metadata = std::fs::symlink_metadata(&resolved)
            .with_context(|| format!("inspecting artifact {}", resolved.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("artifact {} is not a regular non-symlink file", path);
        }
        let bytes = metadata.len();
        if bytes > MAX_ARTIFACT_BYTES {
            bail!("artifact {} exceeds {MAX_ARTIFACT_BYTES} bytes", path);
        }
        let actual = hash_regular_file(&resolved, MAX_ARTIFACT_BYTES)?;
        if !hashes_equal(&actual, content_hash) {
            bail!(
                "artifact {} no longer matches its published content hash",
                path
            );
        }
        artifacts.push(VerifiedArtifact {
            artifact_id: artifact_id.clone(),
            path: relative,
            content_hash: actual,
            bytes,
        });
        if artifacts.len() >= MAX_ARTIFACTS_PER_RUN {
            break;
        }
    }
    Ok(artifacts)
}

fn reviewer_decisions(
    events: &[CompanyEvent],
    candidate_id: &str,
    producer: &str,
) -> Vec<(String, String)> {
    let mut decisions = Vec::new();
    for event in events {
        let CompanyEventKind::DecisionRecorded {
            decision_id,
            target_id,
            accepted_id,
            voters,
            ..
        } = &event.envelope.event
        else {
            continue;
        };
        if target_id != candidate_id || accepted_id != candidate_id {
            continue;
        }
        let Some(reviewer) = event.envelope.agent_identity_id.as_deref() else {
            continue;
        };
        if reviewer == producer || !voters.iter().any(|voter| voter == reviewer) {
            continue;
        }
        decisions.push((decision_id.clone(), reviewer.to_string()));
    }
    decisions
}

fn execution_route(execution: &crate::runtime::RuntimeExecution) -> Result<Vec<String>> {
    let mut steps = Vec::new();
    for result in execution
        .outcome
        .tool_results
        .iter()
        .chain(execution.specialist_outcome.tool_results.iter())
    {
        if !result.success {
            continue;
        }
        let tool_name = normalize_text(&result.tool_name)?;
        if tool_name.is_empty() {
            continue;
        }
        // A generated skill may preserve the proven tool sequence, never the
        // arguments or transcript-derived summaries that happened to produce
        // it. Those may contain project names, account identifiers, queries,
        // or other user data even after heuristic redaction.
        steps.push(tool_name);
        if steps.len() >= MAX_TOOL_STEPS {
            break;
        }
    }
    Ok(steps)
}

fn most_repeated_pattern(evidence: &[VerifiedRunEvidence]) -> (String, usize) {
    let mut counts = BTreeMap::<String, usize>::new();
    for run in evidence {
        *counts.entry(run.pattern_key.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|(a_key, a_count), (b_key, b_count)| {
            a_count.cmp(b_count).then_with(|| b_key.cmp(a_key))
        })
        .unwrap_or_default()
}

fn render_skill_body(name: &str, description: &str, route: &[String]) -> Result<String> {
    let name = name.trim();
    let description = normalize_text(description)?;
    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("name: {name}\n"));
    body.push_str(&format!(
        "description: {}\n",
        serde_json::to_string(&description)?
    ));
    body.push_str("---\n\n");
    body.push_str("# Verified workflow\n\n");
    body.push_str("Use this playbook only when the task matches the observed route.\n\n");
    body.push_str("## Procedure\n\n");
    body.push_str("1. Confirm the task scope and keep account values, secrets, and transient paths out of notes.\n");
    body.push_str(
        "2. Follow the verified tool route below, adapting only workspace-relative inputs.\n",
    );
    body.push_str(
        "3. Record the resulting artifact and verify its bytes before claiming success.\n\n",
    );
    body.push_str("Observed route:\n\n");
    for (index, step) in route.iter().enumerate() {
        body.push_str(&format!("{}. `{}`\n", index + 1, step));
    }
    body.push_str("\n## Verification\n\n");
    body.push_str("- Require a successful run trace, an accepted and verified company job, and a content-addressed file artifact.\n");
    body.push_str("- If those receipts are unavailable, stop and report the missing evidence.\n");
    if body.len() > MAX_CANDIDATE_BODY_BYTES {
        bail!("rendered reverse skill exceeds {MAX_CANDIDATE_BODY_BYTES} bytes");
    }
    Ok(body)
}

fn normalize_text(value: &str) -> Result<String> {
    if value.len() > MAX_DESCRIPTION_BYTES * 8 || value.chars().any(|ch| ch == '\0') {
        bail!("evidence text is too large or contains NUL");
    }
    if contains_secret_marker(value) {
        bail!("evidence text contains an unnormalizable secret marker");
    }
    let mut out = Vec::new();
    for token in value.split_whitespace() {
        let normalized = if looks_like_account(token) {
            "<account>"
        } else if looks_like_url(token) {
            "<url>"
        } else if looks_like_path(token) {
            "<workspace-path>"
        } else {
            token
        };
        out.push(normalized);
    }
    let normalized = out.join(" ");
    if normalized
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        bail!("normalized evidence contains control characters");
    }
    Ok(normalized.chars().take(MAX_DESCRIPTION_BYTES * 4).collect())
}

fn contains_secret_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key=",
        "apikey=",
        "access_token=",
        "refresh_token=",
        "client_secret=",
        "password=",
        "authorization:",
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_account(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch: char| ",.;:()[]{}<>\"'".contains(ch));
    trimmed.contains('@') && trimmed.split('@').count() == 2
}

fn looks_like_url(token: &str) -> bool {
    token.starts_with("http://") || token.starts_with("https://") || token.starts_with("ftp://")
}

fn looks_like_path(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with('~')
        || token.contains("../")
        || token.contains("\\")
        || (token.contains('/') && !token.starts_with("<"))
}

fn resolve_workspace_artifact(workspace: &Path, input: &str) -> Result<(PathBuf, String)> {
    if input.trim().is_empty() || input.chars().any(char::is_control) {
        bail!("artifact path is empty or contains control characters");
    }
    let path = Path::new(input);
    if path.is_absolute() {
        bail!("artifact path must be workspace-relative");
    }
    if path.components().any(|component| {
        !matches!(component, Component::Normal(_))
            || component.as_os_str().to_string_lossy().contains(':')
    }) {
        bail!("artifact path contains an unsafe component");
    }
    let mut current = workspace.to_path_buf();
    for component in path.components() {
        let Component::Normal(name) = component else {
            bail!("artifact path is not workspace-relative");
        };
        current.push(name);
        let metadata = std::fs::symlink_metadata(&current)
            .with_context(|| format!("inspecting artifact path component {}", current.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("artifact path contains a symlink: {}", current.display());
        }
    }
    let canonical = current.canonicalize()?;
    let canonical_workspace = workspace.canonicalize()?;
    if !canonical.starts_with(&canonical_workspace) {
        bail!("artifact path escaped the workspace");
    }
    Ok((canonical, path.to_string_lossy().to_string()))
}

fn verify_bench_receipt(
    workspace: &Path,
    input: &BenchReceiptInput,
) -> Result<VerifiedBenchReceipt> {
    if input.label.trim().is_empty()
        || input.label.len() > 256
        || input.label.contains(['\n', '\r'])
    {
        bail!("bench receipt label is invalid");
    }
    if input.min_tasks == 0 || input.min_tasks > MAX_BENCH_TASKS {
        bail!("bench receipt min_tasks is outside 1..={MAX_BENCH_TASKS}");
    }
    let relative = &input.path;
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("bench receipt path must be workspace-relative");
    }
    let path = workspace.join(relative);
    let (resolved, normalized_path) =
        resolve_workspace_artifact(workspace, &relative.to_string_lossy())?;
    if resolved != path.canonicalize()? {
        // The comparison is informationally redundant after path validation,
        // but makes the no-follow resolution contract explicit.
        bail!("bench receipt path changed while being resolved");
    }
    let bytes =
        read_regular_bytes(&path, MAX_BENCH_BYTES as u64)?.context("bench report is missing")?;
    let content = String::from_utf8(bytes.clone()).context("bench report is not UTF-8")?;
    let heading = format!("## {}", input.label);
    let section = content
        .split_once(&heading)
        .map(|(_, tail)| tail.split_once("\n## ").map_or(tail, |(part, _)| part))
        .context("bench report label was not found")?;
    let mut passed = 0usize;
    let mut total = 0usize;
    for line in section.lines() {
        if !line.trim_start().starts_with('|') || line.contains("---") || line.contains("task |") {
            continue;
        }
        if line.contains("**total**") {
            let cells = line.split('|').map(str::trim).collect::<Vec<_>>();
            if let Some(value) = cells.get(2) {
                let value = value.trim_matches('*');
                if let Some((left, right)) = value.split_once('/') {
                    passed = left.trim_matches('*').parse().unwrap_or(0);
                    total = right.trim_matches('*').parse().unwrap_or(0);
                }
            }
        } else if line.contains("| ✓ |") || line.contains("| ✗ |") {
            total = total.saturating_add(1);
            if line.contains("| ✓ |") {
                passed = passed.saturating_add(1);
            }
        }
    }
    if total == 0 || total > MAX_BENCH_TASKS || passed != total || total < input.min_tasks {
        bail!("bench receipt does not prove a bounded all-pass held-out run");
    }
    Ok(VerifiedBenchReceipt {
        path: normalized_path,
        label: input.label.clone(),
        content_hash: sha256_bytes(&bytes),
        passed_tasks: passed,
        total_tasks: total,
    })
}

fn discover_run_ids(root: &Path) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(root)?.take(MAX_TRACE_ENTRIES) {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if !metadata.is_dir() || metadata.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if validate_run_id(&name).is_ok() {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids)
}

fn validate_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} is not a real directory: {}", path.display());
    }
    crate::config::private_io::reject_symlink_components(path)
}

fn ensure_no_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlink path {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_skill_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("existing skill path is not a real directory");
    }
    let manifest = path.join("SKILL.md");
    let metadata = std::fs::symlink_metadata(&manifest)
        .with_context(|| format!("existing skill is missing {}", manifest.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("existing skill manifest is unsafe");
    }
    Ok(())
}

fn prepare_private_directory(path: &Path) -> Result<()> {
    crate::config::private_io::reject_symlink_components(path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("unsafe private directory {}", path.display());
        }
        return Ok(());
    }
    let parent = path.parent().context("private directory has no parent")?;
    prepare_private_directory(parent)?;
    create_private_directory(path)
}

fn create_private_directory(path: &Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("unsafe private directory appeared at {}", path.display());
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn remove_empty_directory(path: &Path) -> Result<()> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                std::fs::remove_dir(path)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_regular_bytes(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    crate::config::private_io::reject_symlink_components(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if metadata.len() > max_bytes {
        bail!("{} exceeds the {max_bytes}-byte bound", path.display());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("{} grew beyond its byte bound", path.display());
    }
    Ok(Some(bytes))
}

fn hash_regular_file(path: &Path, max_bytes: u64) -> Result<String> {
    let Some(bytes) = read_regular_bytes(path, max_bytes)? else {
        bail!("regular file is missing: {}", path.display());
    };
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn hashes_equal(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .trim()
            .trim_start_matches("sha256:")
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

fn hash_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(sha256_bytes(&serde_json::to_vec(value)?))
}

fn truncate_text(value: &str, max_bytes: usize) -> String {
    value.chars().take(max_bytes).collect()
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok().as_deref() == Some(right)
}

#[cfg(test)]
mod tests {
    // Reverse-skill acceptance is deliberately exercised through real run and
    // company receipts by integration/live harnesses. No synthetic receipt
    // fixtures belong in this module.
}
