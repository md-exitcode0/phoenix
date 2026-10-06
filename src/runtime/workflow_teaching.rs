//! Human-taught browser routines.
//!
//! The user demonstrates a workflow in the same private browser profile their
//! coworker uses. Phoenix stores a semantic, secret-safe playbook rather than
//! a brittle coordinate macro. Durable workflow runs remain in `workflow.rs`;
//! this module is the reusable library that teaches an owner how to perform a
//! recurring browser-shaped procedure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::company_directory::{DirectorySnapshot, LifecycleState};
use crate::runtime::workflow::WorkflowScope;
use crate::tools::browser_native::{
    BrowserInteractionReceipt, BrowserSemanticTarget, BrowserUserAction,
};

pub const TAUGHT_ROUTINE_SCHEMA_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STEPS: usize = 256;
const MAX_ROUTINES: usize = 4_096;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_SHORT_BYTES: usize = 4_096;

/// Fast profile -> teaching lookup for native Chromium event observers. The
/// durable teaching documents remain authoritative; this cache is rebuilt by
/// a bounded directory scan after a gateway restart.
static ACTIVE_TEACHINGS: OnceLock<Mutex<std::collections::HashMap<String, String>>> =
    OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TeachWorkflowCommand {
    Begin {
        owner_agent_id: String,
        #[serde(default)]
        scope: Option<WorkflowScope>,
        #[serde(default)]
        group_id: Option<String>,
        #[serde(default)]
        start_url: Option<String>,
    },
    Revise {
        routine_id: String,
    },
    Interact {
        teaching_id: String,
        browser_action: BrowserUserAction,
    },
    Finalize {
        teaching_id: String,
        name: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        trigger_phrases: Vec<String>,
        /// Optional last-mile visibility choice from the save dialog. When it
        /// is absent, the scope selected at Begin (including the user's
        /// share-by-default setting) remains authoritative.
        #[serde(default)]
        scope: Option<WorkflowScope>,
        #[serde(default)]
        group_id: Option<String>,
    },
    Cancel {
        teaching_id: String,
    },
    Inspect {
        teaching_id: String,
    },
    List {
        #[serde(default)]
        owner_agent_id: Option<String>,
        #[serde(default)]
        include_archived: bool,
        #[serde(default)]
        include_deleted: bool,
    },
    Archive {
        routine_id: String,
        archived: bool,
    },
    SetScope {
        routine_id: String,
        scope: WorkflowScope,
        #[serde(default)]
        group_id: Option<String>,
    },
    SetOwner {
        routine_id: String,
        owner_agent_id: String,
    },
    /// Recoverable deletion. The exact visible name is required and the
    /// routine remains restorable for thirty days.
    Delete {
        routine_id: String,
        confirmed_name: String,
    },
    RestoreDeleted {
        routine_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeachWorkflowReply {
    pub result: String,
    #[serde(default)]
    pub teaching: Option<TeachingSession>,
    #[serde(default)]
    pub routine: Option<TaughtRoutine>,
    #[serde(default)]
    pub routines: Vec<RoutineSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeachingSession {
    pub schema_version: u32,
    pub teaching_id: String,
    pub owner_agent_id: String,
    pub browser_profile_id: String,
    pub scope: WorkflowScope,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub revises_routine_id: Option<String>,
    pub status: TeachingStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub steps: Vec<RoutineStep>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeachingStatus {
    Recording,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaughtRoutine {
    pub schema_version: u32,
    pub routine_id: String,
    pub name: String,
    pub description: String,
    pub owner_agent_id: String,
    pub scope: WorkflowScope,
    #[serde(default)]
    pub group_id: Option<String>,
    pub status: RoutineStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub trigger_phrases: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<RoutineParameter>,
    pub starting_url: Option<String>,
    pub playbook: String,
    pub content_sha256: String,
    #[serde(default)]
    pub steps: Vec<RoutineStep>,
    #[serde(default)]
    pub successful_runs: u64,
    #[serde(default)]
    pub failed_runs: u64,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub last_result: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub delete_after: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutineStatus {
    Active,
    NeedsReview,
    Archived,
    PendingDeletion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineParameter {
    pub name: String,
    pub description: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RoutineStep {
    Navigate {
        url: String,
    },
    GoBack,
    Click {
        target: BrowserSemanticTarget,
        before_url: String,
        after_url: String,
    },
    Type {
        target: BrowserSemanticTarget,
        value: RoutineValue,
        clear: bool,
    },
    SendKeys {
        keys: String,
        #[serde(default)]
        target: Option<BrowserSemanticTarget>,
    },
    Select {
        target: BrowserSemanticTarget,
        option: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutineValue {
    Literal { value: String },
    Parameter { name: String, sensitive: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineSummary {
    pub routine_id: String,
    pub name: String,
    pub description: String,
    pub owner_agent_id: String,
    pub scope: WorkflowScope,
    #[serde(default)]
    pub group_id: Option<String>,
    pub status: RoutineStatus,
    #[serde(default)]
    pub trigger_phrases: Vec<String>,
    pub step_count: usize,
    pub successful_runs: u64,
    pub failed_runs: u64,
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub delete_after: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoutineToolInput {
    pub action: String,
    #[serde(default)]
    pub routine_id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub result: Option<String>,
    /// Exact visible workflow name, required to confirm action=delete.
    #[serde(default)]
    pub name: Option<String>,
}

pub fn handle_command(command: TeachWorkflowCommand) -> Result<TeachWorkflowReply> {
    match command {
        TeachWorkflowCommand::Begin {
            owner_agent_id,
            scope,
            group_id,
            start_url,
        } => {
            let settings_scope = group_id
                .as_deref()
                .map(|id| crate::settings::SettingsScope::Group { id: id.to_string() })
                .unwrap_or_else(|| crate::settings::SettingsScope::Agent {
                    id: owner_agent_id.clone(),
                });
            let scope = scope.unwrap_or_else(|| {
                if group_id.is_some() {
                    WorkflowScope::Group
                } else if crate::settings::effective_bool(
                    "workflows.share_by_default",
                    &settings_scope,
                )
                .unwrap_or(false)
                {
                    WorkflowScope::Company
                } else {
                    WorkflowScope::Agent
                }
            });
            begin(owner_agent_id, scope, group_id, start_url)
        }
        TeachWorkflowCommand::Revise { routine_id } => begin_revision(&routine_id),
        TeachWorkflowCommand::Interact {
            teaching_id,
            browser_action,
        } => interact(&teaching_id, browser_action),
        TeachWorkflowCommand::Finalize {
            teaching_id,
            name,
            description,
            trigger_phrases,
            scope,
            group_id,
        } => finalize(
            &teaching_id,
            name,
            description,
            trigger_phrases,
            scope,
            group_id,
        ),
        TeachWorkflowCommand::Cancel { teaching_id } => cancel(&teaching_id),
        TeachWorkflowCommand::Inspect { teaching_id } => {
            let teaching = load_teaching(&teaching_id)?;
            Ok(TeachWorkflowReply {
                result: "teaching_session".to_string(),
                teaching: Some(teaching),
                routine: None,
                routines: Vec::new(),
            })
        }
        TeachWorkflowCommand::List {
            owner_agent_id,
            include_archived,
            include_deleted,
        } => {
            let mut routines = list_routines(owner_agent_id.as_deref(), None, include_archived)?;
            if include_deleted {
                routines.extend(list_deleted_routines(owner_agent_id.as_deref(), None)?);
                routines.sort_by(|left, right| {
                    left.name.to_lowercase().cmp(&right.name.to_lowercase())
                });
            }
            Ok(TeachWorkflowReply {
                result: "routine_list".to_string(),
                teaching: None,
                routine: None,
                routines,
            })
        }
        TeachWorkflowCommand::Archive {
            routine_id,
            archived,
        } => {
            let routine = set_archived(&routine_id, archived)?;
            Ok(TeachWorkflowReply {
                result: if archived {
                    "routine_archived"
                } else {
                    "routine_restored"
                }
                .to_string(),
                teaching: None,
                routine: Some(routine),
                routines: Vec::new(),
            })
        }
        TeachWorkflowCommand::SetScope {
            routine_id,
            scope,
            group_id,
        } => {
            let routine = set_scope(&routine_id, scope, group_id)?;
            Ok(TeachWorkflowReply {
                result: "routine_scope_updated".to_string(),
                teaching: None,
                routine: Some(routine),
                routines: Vec::new(),
            })
        }
        TeachWorkflowCommand::SetOwner {
            routine_id,
            owner_agent_id,
        } => {
            let routine = set_owner(&routine_id, &owner_agent_id)?;
            Ok(TeachWorkflowReply {
                result: "routine_owner_updated".to_string(),
                teaching: None,
                routine: Some(routine),
                routines: Vec::new(),
            })
        }
        TeachWorkflowCommand::Delete {
            routine_id,
            confirmed_name,
        } => {
            let routine = delete_routine(&routine_id, &confirmed_name)?;
            Ok(TeachWorkflowReply {
                result: "routine_pending_deletion".to_string(),
                teaching: None,
                routine: Some(routine),
                routines: Vec::new(),
            })
        }
        TeachWorkflowCommand::RestoreDeleted { routine_id } => {
            let routine = restore_deleted_routine(&routine_id)?;
            Ok(TeachWorkflowReply {
                result: "routine_restored_from_trash".to_string(),
                teaching: None,
                routine: Some(routine),
                routines: Vec::new(),
            })
        }
    }
}

fn begin(
    owner: String,
    scope: WorkflowScope,
    group_id: Option<String>,
    start_url: Option<String>,
) -> Result<TeachWorkflowReply> {
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    let (owner_agent_id, browser_profile_id) = resolve_owner(&snapshot, &owner)?;
    validate_scope(&snapshot, scope, group_id.as_deref(), &owner_agent_id)?;
    let now = Utc::now().to_rfc3339();
    let teaching_id = format!("teach-{}", uuid::Uuid::new_v4().simple());
    let mut teaching = TeachingSession {
        schema_version: TAUGHT_ROUTINE_SCHEMA_VERSION,
        teaching_id: teaching_id.clone(),
        owner_agent_id,
        browser_profile_id: browser_profile_id.clone(),
        scope,
        group_id,
        revises_routine_id: None,
        status: TeachingStatus::Recording,
        created_at: now.clone(),
        updated_at: now,
        steps: Vec::new(),
    };
    if let Some(url) = start_url.filter(|value| !value.trim().is_empty()) {
        let action = BrowserUserAction::Navigate { url, new_tab: true };
        let receipt = crate::tools::browser_native::user_interact(&browser_profile_id, &action)
            .map_err(anyhow::Error::msg)?;
        append_recorded_step(&mut teaching.steps, record_step(&action, receipt)?);
    }
    write_teaching(&teaching)?;
    register_active_teaching(&teaching);
    Ok(TeachWorkflowReply {
        result: "teaching_started".to_string(),
        teaching: Some(teaching),
        routine: None,
        routines: Vec::new(),
    })
}

fn begin_revision(routine_id: &str) -> Result<TeachWorkflowReply> {
    let routine = load_routine(routine_id)?;
    anyhow::ensure!(
        routine.status != RoutineStatus::PendingDeletion,
        "restore the workflow before reteaching it"
    );
    let mut reply = begin(
        routine.owner_agent_id.clone(),
        routine.scope,
        routine.group_id.clone(),
        routine.starting_url.clone(),
    )?;
    let teaching = reply
        .teaching
        .as_mut()
        .context("revision did not create a teaching session")?;
    teaching.revises_routine_id = Some(routine.routine_id.clone());
    write_teaching(teaching)?;
    reply.result = "workflow_revision_started".to_string();
    reply.routine = Some(routine);
    Ok(reply)
}

fn interact(teaching_id: &str, action: BrowserUserAction) -> Result<TeachWorkflowReply> {
    validate_id(teaching_id, "teaching_id")?;
    let path = teaching_path(teaching_id);
    let updated = crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("teaching session does not exist")?;
        let mut teaching: TeachingSession =
            serde_json::from_slice(raw).context("teaching session is invalid")?;
        anyhow::ensure!(
            teaching.status == TeachingStatus::Recording,
            "teaching session is not recording"
        );
        anyhow::ensure!(
            teaching.steps.len() < MAX_STEPS,
            "teaching session reached the {MAX_STEPS}-step safety bound"
        );
        let receipt =
            crate::tools::browser_native::user_interact(&teaching.browser_profile_id, &action)
                .map_err(anyhow::Error::msg)?;
        append_recorded_step(&mut teaching.steps, record_step(&action, receipt)?);
        teaching.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&teaching)?;
        Ok((teaching, replacement))
    })?;
    Ok(TeachWorkflowReply {
        result: "step_recorded".to_string(),
        teaching: Some(updated),
        routine: None,
        routines: Vec::new(),
    })
}

fn register_active_teaching(teaching: &TeachingSession) {
    if teaching.status != TeachingStatus::Recording {
        return;
    }
    let _ = ACTIVE_TEACHINGS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map(|mut active| {
            active.insert(
                teaching.browser_profile_id.clone(),
                teaching.teaching_id.clone(),
            )
        });
}

fn unregister_active_teaching(browser_profile_id: &str, teaching_id: &str) {
    let _ = ACTIVE_TEACHINGS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map(|mut active| {
            if active.get(browser_profile_id).map(String::as_str) == Some(teaching_id) {
                // Empty is a negative cache entry. Native observer events can
                // be frequent even outside teaching; do not rescan thousands
                // of durable files after every ordinary click.
                active.insert(browser_profile_id.to_string(), String::new());
            }
        });
}

/// Find an active durable teaching session for one private browser profile.
/// The scan is only a restart fallback; normal Begin/Finalize/Cancel calls
/// maintain the in-memory index. Symlinks and unbounded directories are
/// rejected exactly like the rest of the workflow library.
fn active_teaching_id(browser_profile_id: &str) -> Result<Option<String>> {
    if let Some(cached) = ACTIVE_TEACHINGS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .ok()
        .and_then(|active| active.get(browser_profile_id).cloned())
    {
        if cached.is_empty() {
            return Ok(None);
        }
        if load_teaching(&cached).is_ok_and(|teaching| {
            teaching.status == TeachingStatus::Recording
                && teaching.browser_profile_id == browser_profile_id
        }) {
            return Ok(Some(cached));
        }
        unregister_active_teaching(browser_profile_id, &cached);
    }

    let dir = teaching_dir();
    crate::config::private_io::prepare_phoenix_directory(&dir)?;
    let mut found: Option<TeachingSession> = None;
    let mut seen = 0usize;
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("failed to list {}", dir.display()))?
    {
        seen = seen.saturating_add(1);
        anyhow::ensure!(seen <= MAX_ROUTINES, "too many teaching sessions");
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let Some(raw) =
            crate::config::private_io::read_private_file_limited(&path, MAX_DOCUMENT_BYTES)?
        else {
            continue;
        };
        let Ok(teaching) = serde_json::from_slice::<TeachingSession>(&raw) else {
            continue;
        };
        if teaching.status != TeachingStatus::Recording
            || teaching.browser_profile_id != browser_profile_id
        {
            continue;
        }
        if found
            .as_ref()
            .is_none_or(|current| teaching.updated_at > current.updated_at)
        {
            found = Some(teaching);
        }
    }
    if let Some(teaching) = found {
        let teaching_id = teaching.teaching_id.clone();
        register_active_teaching(&teaching);
        Ok(Some(teaching_id))
    } else {
        let _ = ACTIVE_TEACHINGS
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .map(|mut active| active.insert(browser_profile_id.to_string(), String::new()));
        Ok(None)
    }
}

/// Persist an action already performed through Chromium's native input path.
/// Unlike `Interact`, this never calls `user_interact` and therefore can never
/// click/type/select twice. The isolated-world observer supplies the semantic
/// target and a secret-safe receipt after the human action occurred.
pub(crate) fn record_native_interaction(
    browser_profile_id: &str,
    action: BrowserUserAction,
    receipt: BrowserInteractionReceipt,
    fold_navigation_into_click: bool,
) -> Result<bool> {
    let Some(teaching_id) = active_teaching_id(browser_profile_id)? else {
        return Ok(false);
    };
    validate_id(&teaching_id, "teaching_id")?;
    let path = teaching_path(&teaching_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("teaching session does not exist")?;
        let mut teaching: TeachingSession =
            serde_json::from_slice(raw).context("teaching session is invalid")?;
        anyhow::ensure!(
            teaching.status == TeachingStatus::Recording,
            "teaching session is not recording"
        );
        anyhow::ensure!(
            teaching.browser_profile_id == browser_profile_id,
            "native browser event belongs to a different teaching profile"
        );
        anyhow::ensure!(
            teaching.steps.len() < MAX_STEPS,
            "teaching session reached the {MAX_STEPS}-step safety bound"
        );
        let step = record_step(&action, receipt)?;
        append_native_recorded_step(&mut teaching.steps, step, fold_navigation_into_click);
        teaching.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&teaching)?;
        Ok(((), replacement))
    })?;
    Ok(true)
}

fn finalize(
    teaching_id: &str,
    name: String,
    description: String,
    trigger_phrases: Vec<String>,
    scope: Option<WorkflowScope>,
    group_id: Option<String>,
) -> Result<TeachWorkflowReply> {
    validate_id(teaching_id, "teaching_id")?;
    validate_text(&name, "workflow name", 512)?;
    validate_text(&description, "workflow description", MAX_TEXT_BYTES)?;
    anyhow::ensure!(trigger_phrases.len() <= 64, "too many trigger phrases");
    let mut teaching = load_teaching(teaching_id)?;
    anyhow::ensure!(
        teaching.status == TeachingStatus::Recording,
        "teaching session is not recording"
    );
    anyhow::ensure!(
        !teaching.steps.is_empty(),
        "demonstrate at least one browser action before saving the workflow"
    );
    if let Some(scope) = scope {
        if scope == WorkflowScope::Group {
            let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
            validate_scope(
                &snapshot,
                scope,
                group_id.as_deref(),
                &teaching.owner_agent_id,
            )?;
        } else {
            anyhow::ensure!(group_id.is_none(), "only a group workflow may set group_id");
        }
        teaching.scope = scope;
        teaching.group_id = group_id;
    }
    let previous = teaching
        .revises_routine_id
        .as_deref()
        .map(load_routine)
        .transpose()?;
    if let Some(previous) = previous.as_ref() {
        anyhow::ensure!(
            previous.owner_agent_id == teaching.owner_agent_id,
            "workflow owner changed during reteaching; restart the revision"
        );
    }
    let routine_id = previous
        .as_ref()
        .map(|routine| routine.routine_id.clone())
        .unwrap_or_else(|| routine_slug(&name));
    let path = routine_path(&routine_id);
    if previous.is_none() {
        anyhow::ensure!(
            crate::config::private_io::read_private_file_limited(&path, MAX_DOCUMENT_BYTES)?
                .is_none(),
            "a workflow named `{}` already exists",
            name.trim()
        );
    } else {
        let conflicting = routine_slug(&name);
        if conflicting != routine_id {
            anyhow::ensure!(
                crate::config::private_io::read_private_file_limited(
                    &routine_path(&conflicting),
                    MAX_DOCUMENT_BYTES,
                )?
                .is_none(),
                "a workflow named `{}` already exists",
                name.trim()
            );
        }
    }
    let description = if description.trim().is_empty() {
        format!(
            "Repeat the browser workflow demonstrated for {}.",
            name.trim()
        )
    } else {
        description.trim().to_string()
    };
    let mut triggers = normalize_list(trigger_phrases, 64, 512)?;
    if !triggers
        .iter()
        .any(|trigger| trigger.eq_ignore_ascii_case(name.trim()))
    {
        triggers.insert(0, name.trim().to_string());
    }
    let parameters = collect_parameters(&teaching.steps);
    let starting_url = teaching.steps.iter().find_map(|step| match step {
        RoutineStep::Navigate { url } => Some(url.clone()),
        _ => None,
    });
    let playbook = render_playbook(&teaching.steps, &parameters);
    let content_sha256 = content_hash(&teaching.steps, &playbook)?;
    let now = Utc::now().to_rfc3339();
    let browser_profile_id = teaching.browser_profile_id.clone();
    let routine = TaughtRoutine {
        schema_version: TAUGHT_ROUTINE_SCHEMA_VERSION,
        routine_id: routine_id.clone(),
        name: name.trim().to_string(),
        description,
        owner_agent_id: teaching.owner_agent_id,
        scope: teaching.scope,
        group_id: teaching.group_id,
        status: RoutineStatus::Active,
        created_at: previous
            .as_ref()
            .map(|routine| routine.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
        trigger_phrases: triggers,
        parameters,
        starting_url,
        playbook,
        content_sha256,
        steps: teaching.steps,
        successful_runs: previous
            .as_ref()
            .map(|routine| routine.successful_runs)
            .unwrap_or(0),
        failed_runs: previous
            .as_ref()
            .map(|routine| routine.failed_runs)
            .unwrap_or(0),
        consecutive_failures: 0,
        last_used_at: previous
            .as_ref()
            .and_then(|routine| routine.last_used_at.clone()),
        last_result: previous
            .as_ref()
            .and_then(|routine| routine.last_result.clone()),
        deleted_at: None,
        delete_after: None,
    };
    if let Some(previous) = previous.as_ref() {
        write_json(&routine_revision_path(previous), previous)?;
    }
    write_routine(&routine)?;
    let _ = crate::config::private_io::remove_private_file(&teaching_path(teaching_id));
    unregister_active_teaching(&browser_profile_id, teaching_id);
    Ok(TeachWorkflowReply {
        result: if previous.is_some() {
            "routine_revised"
        } else {
            "routine_saved"
        }
        .to_string(),
        teaching: None,
        routine: Some(routine),
        routines: Vec::new(),
    })
}

fn cancel(teaching_id: &str) -> Result<TeachWorkflowReply> {
    validate_id(teaching_id, "teaching_id")?;
    let mut teaching = load_teaching(teaching_id)?;
    teaching.status = TeachingStatus::Cancelled;
    teaching.updated_at = Utc::now().to_rfc3339();
    let _ = crate::config::private_io::remove_private_file(&teaching_path(teaching_id))?;
    unregister_active_teaching(&teaching.browser_profile_id, teaching_id);
    Ok(TeachWorkflowReply {
        result: "teaching_cancelled".to_string(),
        teaching: Some(teaching),
        routine: None,
        routines: Vec::new(),
    })
}

fn record_step(
    action: &BrowserUserAction,
    receipt: BrowserInteractionReceipt,
) -> Result<RoutineStep> {
    let target = || {
        receipt
            .target
            .clone()
            .context("browser action did not resolve a semantic target")
    };
    Ok(match action {
        BrowserUserAction::Resize { .. }
        | BrowserUserAction::Scroll { .. }
        | BrowserUserAction::SwitchTab { .. }
        | BrowserUserAction::CloseTab { .. } => {
            bail!("browser viewport and tab chrome are UI transport state, not workflow steps")
        }
        BrowserUserAction::Navigate { .. } => RoutineStep::Navigate {
            url: sanitize_url(&receipt.after_url),
        },
        BrowserUserAction::GoBack => RoutineStep::GoBack,
        BrowserUserAction::Click { .. } => RoutineStep::Click {
            target: target()?,
            before_url: sanitize_url(&receipt.before_url),
            after_url: sanitize_url(&receipt.after_url),
        },
        BrowserUserAction::Type {
            text,
            clear,
            parameter_name,
            ..
        } => {
            let target = target()?;
            let value = if receipt.sensitive || parameter_name.is_some() {
                RoutineValue::Parameter {
                    name: parameter_name
                        .as_deref()
                        .map(parameter_slug)
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| parameter_name_for_target(&target, receipt.sensitive)),
                    sensitive: receipt.sensitive,
                }
            } else {
                RoutineValue::Literal {
                    value: text.clone(),
                }
            };
            RoutineStep::Type {
                target,
                value,
                clear: *clear,
            }
        }
        BrowserUserAction::SendKeys { keys } => RoutineStep::SendKeys {
            keys: keys.clone(),
            target: receipt.target,
        },
        BrowserUserAction::Select { option, .. } => RoutineStep::Select {
            target: target()?,
            option: option.clone(),
        },
    })
}

/// Store what the user meant, not the cadence of the UI transport. The canvas
/// deliberately sends text in short batches to keep the embedded browser
/// responsive. Without coalescing here, a sentence typed with two pauses became
/// two workflow instructions (and fast typing could become dozens). Adjacent
/// fills of the same semantic field are one durable action.
fn append_recorded_step(steps: &mut Vec<RoutineStep>, next: RoutineStep) {
    if let (
        Some(RoutineStep::Navigate { url: previous }),
        RoutineStep::Navigate { url: next_url },
    ) = (steps.last(), &next)
    {
        if previous == next_url {
            return;
        }
    }
    if let (
        Some(RoutineStep::Type {
            target: previous_target,
            value: previous_value,
            clear: previous_clear,
        }),
        RoutineStep::Type {
            target: next_target,
            value: next_value,
            clear: next_clear,
        },
    ) = (steps.last_mut(), &next)
    {
        if previous_target == next_target {
            // Native Chromium flushes a field once on blur/change/Enter and
            // sends the complete final value with clear=true. A later edit of
            // the same field replaces that value; transport-batched Canvas
            // input uses clear only on its first chunk and still concatenates
            // the following chunks below.
            if *next_clear {
                *previous_value = next_value.clone();
                *previous_clear = true;
                return;
            }
            match (previous_value, next_value) {
                (
                    RoutineValue::Literal { value: previous },
                    RoutineValue::Literal { value: added },
                ) => previous.push_str(added),
                (
                    RoutineValue::Parameter {
                        name: previous_name,
                        sensitive: previous_sensitive,
                    },
                    RoutineValue::Parameter {
                        name: added_name,
                        sensitive: added_sensitive,
                    },
                ) if previous_name == added_name => {
                    *previous_sensitive |= *added_sensitive;
                }
                _ => {
                    steps.push(next);
                    return;
                }
            }
            *previous_clear |= *next_clear;
            return;
        }
    }
    steps.push(next);
}

fn append_native_recorded_step(
    steps: &mut Vec<RoutineStep>,
    next: RoutineStep,
    fold_navigation_into_click: bool,
) {
    // The app's Back control records GoBack through the explicit command lane;
    // the isolated observer then sees the resulting URL. The latter is
    // evidence of the same action, not a second instruction.
    if matches!(steps.last(), Some(RoutineStep::GoBack))
        && matches!(next, RoutineStep::Navigate { .. })
    {
        return;
    }
    if fold_navigation_into_click {
        if let (
            Some(RoutineStep::Click {
                before_url,
                after_url,
                ..
            }),
            RoutineStep::Navigate { url },
        ) = (steps.last_mut(), &next)
        {
            if before_url == after_url {
                *after_url = url.clone();
                return;
            }
        }
    }
    append_recorded_step(steps, next);
}

fn resolve_owner(snapshot: &DirectorySnapshot, requested: &str) -> Result<(String, String)> {
    let requested = requested.trim();
    let requested = if requested.eq_ignore_ascii_case("orchestrator") {
        "phoenix"
    } else {
        requested
    };
    validate_text(requested, "workflow owner", 512)?;
    let mut matches = snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.profile.agent_id.eq_ignore_ascii_case(requested)
                || agent.profile.internal_role.eq_ignore_ascii_case(requested)
                || agent.profile.display_name.eq_ignore_ascii_case(requested)
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|agent| &agent.profile.agent_id);
    matches.dedup_by_key(|agent| &agent.profile.agent_id);
    anyhow::ensure!(!matches.is_empty(), "unknown workflow owner `{requested}`");
    anyhow::ensure!(
        matches.len() == 1,
        "workflow owner `{requested}` is ambiguous; use the immutable agent id"
    );
    let agent = matches[0];
    anyhow::ensure!(
        agent.profile.lifecycle == LifecycleState::Active,
        "workflow owner `{requested}` is not active"
    );
    Ok((
        agent.profile.agent_id.clone(),
        agent.profile.browser_profile_id.clone(),
    ))
}

fn validate_scope(
    snapshot: &DirectorySnapshot,
    scope: WorkflowScope,
    group_id: Option<&str>,
    owner_agent_id: &str,
) -> Result<()> {
    match scope {
        WorkflowScope::Agent | WorkflowScope::Company => {
            anyhow::ensure!(group_id.is_none(), "only a group workflow may set group_id");
        }
        WorkflowScope::Group => {
            let group_id = group_id.context("group workflow scope requires group_id")?;
            let group = snapshot
                .groups
                .iter()
                .find(|group| group.profile.group_id == group_id)
                .with_context(|| format!("unknown workflow group `{group_id}`"))?;
            anyhow::ensure!(
                group.profile.lifecycle == LifecycleState::Active,
                "workflow group `{group_id}` is not active"
            );
            anyhow::ensure!(
                snapshot.members.iter().any(|member| {
                    member.group_id == group_id && member.agent_id == owner_agent_id
                }),
                "workflow owner is not a member of group `{group_id}`"
            );
        }
    }
    Ok(())
}

pub fn execute_tool(
    input: RoutineToolInput,
    actor: &str,
    group_id: Option<&str>,
) -> Result<serde_json::Value> {
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    let (actor_id, _) = resolve_owner(&snapshot, actor)?;
    match input.action.trim() {
        "list" | "search" => {
            let mut summaries = list_routines(Some(&actor_id), group_id, false)?;
            if let Some(query) = input.query.as_deref().filter(|value| !value.trim().is_empty()) {
                summaries.sort_by_key(|routine| std::cmp::Reverse(summary_score(routine, query)));
                summaries.retain(|routine| summary_score(routine, query) > 0);
                summaries.truncate(16);
            }
            Ok(serde_json::json!({ "routines": summaries }))
        }
        "get" | "begin_run" => {
            let routine_id = required(input.routine_id, "routine_id")?;
            let routine = if input.action == "begin_run" {
                let routine = mark_began(&routine_id, &actor_id, group_id)?;
                crate::runtime::company::mirror_skill_activated(
                    &format!(
                        "routine-{}-{}",
                        routine.routine_id,
                        &uuid::Uuid::new_v4().simple().to_string()[..8]
                    ),
                    &actor_id,
                    &routine.name,
                );
                routine
            } else {
                let routine = load_routine(&routine_id)?;
                ensure_visible(&routine, &actor_id, group_id)?;
                anyhow::ensure!(
                    routine.status != RoutineStatus::Archived,
                    "workflow is archived"
                );
                routine
            };
            Ok(serde_json::json!({
                "routine": routine,
                "execution_note": "Follow the semantic playbook against fresh browser_state. Resolve sensitive parameters through the vault/login flow; never ask for or persist plaintext credentials. Afterward call routine complete_run with success and a short evidence result."
            }))
        }
        "complete_run" => {
            let routine_id = required(input.routine_id, "routine_id")?;
            let success = input.success.context("complete_run requires success")?;
            let result = input.result.unwrap_or_default();
            validate_text(&result, "workflow result", MAX_SHORT_BYTES)?;
            let routine = record_result(&routine_id, &actor_id, group_id, success, &result)?;
            Ok(serde_json::json!({
                "routine": summarize(&routine),
                "needs_review": routine.status == RoutineStatus::NeedsReview,
                "instruction": if routine.status == RoutineStatus::NeedsReview {
                    "This workflow has failed three consecutive times. Stop blind retries and ask the user whether to reteach, archive, or delete it."
                } else { "Run receipt recorded." }
            }))
        }
        // Only on the user's request. Delete stays recoverable for 30 days.
        "archive" | "delete" => {
            let routine_id = required(input.routine_id, "routine_id")?;
            let routine = load_routine(&routine_id)?;
            ensure_visible(&routine, &actor_id, group_id)?;
            let routine = if input.action.trim() == "archive" {
                set_archived(&routine_id, true)?
            } else {
                let name = required(input.name, "name")?;
                delete_routine(&routine_id, &name)?
            };
            Ok(serde_json::json!({ "routine": summarize(&routine), "result": if input.action.trim() == "archive" {
                "Workflow archived; it no longer runs or appears in search."
            } else {
                "Workflow deleted; it can be restored from Settings for 30 days."
            } }))
        }
        other => bail!(
            "unknown routine action `{other}`; expected list, search, get, begin_run, complete_run, archive, or delete"
        ),
    }
}

fn mark_began(routine_id: &str, actor_id: &str, group_id: Option<&str>) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    let path = routine_path(routine_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(raw).context("taught workflow is invalid")?;
        ensure_visible(&routine, actor_id, group_id)?;
        anyhow::ensure!(
            routine.status == RoutineStatus::Active,
            if routine.status == RoutineStatus::NeedsReview {
                "workflow needs review after repeated failures; ask the user to reteach or archive it"
            } else {
                "workflow is archived"
            }
        );
        routine.last_used_at = Some(Utc::now().to_rfc3339());
        routine.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&routine)?;
        Ok((routine, replacement))
    })
}

pub fn context_hints(actor: &str, group_id: Option<&str>, request: &str) -> String {
    let mut routines = matching_routines(actor, group_id, request, false);
    routines.truncate(4);
    if routines.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "TAUGHT WORKFLOWS THAT MATCH THIS REQUEST — use `routine` begin_run before improvising:\n",
    );
    for routine in routines {
        out.push_str(&format!(
            "- `{}` — {} (owner {}, scope {}, status {:?})\n",
            routine.routine_id,
            routine.description,
            routine.owner_agent_id,
            routine.scope.as_str(),
            routine.status
        ));
    }
    out
}

/// Strongest active routine that confidently matches this exact turn. Prompt
/// hints may show a few loose candidates for discovery, but the execution
/// gate uses this stricter result so a generic shared word never blocks work.
pub fn matching_routine_id(actor: &str, group_id: Option<&str>, request: &str) -> Option<String> {
    matching_routines(actor, group_id, request, true)
        .into_iter()
        .next()
        .map(|routine| routine.routine_id)
}

fn matching_routines(
    actor: &str,
    group_id: Option<&str>,
    request: &str,
    confident_only: bool,
) -> Vec<RoutineSummary> {
    let scope = group_id
        .map(|id| crate::settings::SettingsScope::Group { id: id.to_string() })
        .unwrap_or_else(|| crate::settings::SettingsScope::Agent {
            id: actor.to_string(),
        });
    if !crate::settings::effective_bool("workflows.enabled", &scope).unwrap_or(true)
        || !crate::settings::effective_bool("workflows.auto_run", &scope).unwrap_or(true)
    {
        return Vec::new();
    }
    let Ok(snapshot) =
        crate::runtime::company::global().and_then(|store| store.directory_snapshot())
    else {
        return Vec::new();
    };
    let Ok((actor_id, _)) = resolve_owner(&snapshot, actor) else {
        return Vec::new();
    };
    let Ok(mut routines) = list_routines(Some(&actor_id), group_id, false) else {
        return Vec::new();
    };
    routines.sort_by_key(|routine| std::cmp::Reverse(summary_score(routine, request)));
    if confident_only {
        routines.retain(|routine| {
            routine.status == RoutineStatus::Active && confident_summary_match(routine, request)
        });
    } else {
        routines.retain(|routine| summary_score(routine, request) > 0);
    }
    routines
}

pub fn group_id_for_session(session_id: &str) -> Option<String> {
    crate::runtime::company::global()
        .ok()?
        .directory_snapshot()
        .ok()?
        .groups
        .into_iter()
        .find(|group| group.profile.canonical_session_id.as_deref() == Some(session_id))
        .map(|group| group.profile.group_id)
}

fn record_result(
    routine_id: &str,
    actor_id: &str,
    group_id: Option<&str>,
    success: bool,
    result: &str,
) -> Result<TaughtRoutine> {
    let path = routine_path(routine_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(raw).context("taught workflow is invalid")?;
        ensure_visible(&routine, actor_id, group_id)?;
        if success {
            routine.successful_runs = routine.successful_runs.saturating_add(1);
            routine.consecutive_failures = 0;
            if routine.status == RoutineStatus::NeedsReview {
                routine.status = RoutineStatus::Active;
            }
        } else {
            routine.failed_runs = routine.failed_runs.saturating_add(1);
            routine.consecutive_failures = routine.consecutive_failures.saturating_add(1);
            let settings_scope = match routine.scope {
                WorkflowScope::Company => crate::settings::SettingsScope::Global,
                WorkflowScope::Agent => crate::settings::SettingsScope::Agent {
                    id: routine.owner_agent_id.clone(),
                },
                WorkflowScope::Group => crate::settings::SettingsScope::Group {
                    id: routine.group_id.clone().unwrap_or_default(),
                },
            };
            if routine.consecutive_failures >= 3
                && crate::settings::effective_bool("workflows.failure_quarantine", &settings_scope)
                    .unwrap_or(true)
            {
                routine.status = RoutineStatus::NeedsReview;
            }
        }
        routine.last_used_at = Some(Utc::now().to_rfc3339());
        routine.last_result = (!result.trim().is_empty()).then(|| result.trim().to_string());
        routine.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&routine)?;
        Ok((routine, replacement))
    })
}

fn ensure_visible(routine: &TaughtRoutine, actor_id: &str, group_id: Option<&str>) -> Result<()> {
    match routine.scope {
        WorkflowScope::Company => {}
        WorkflowScope::Agent => {
            anyhow::ensure!(
                routine.owner_agent_id == actor_id,
                "workflow belongs to another coworker"
            );
        }
        WorkflowScope::Group => {
            anyhow::ensure!(
                routine.group_id.as_deref() == group_id,
                "workflow belongs to another group"
            );
        }
    }
    Ok(())
}

fn list_routines(
    owner_agent_id: Option<&str>,
    group_id: Option<&str>,
    include_archived: bool,
) -> Result<Vec<RoutineSummary>> {
    list_routines_in(&routines_dir(), owner_agent_id, group_id, include_archived)
}

fn list_deleted_routines(
    owner_agent_id: Option<&str>,
    group_id: Option<&str>,
) -> Result<Vec<RoutineSummary>> {
    purge_expired_deleted_routines()?;
    list_routines_in(&deleted_routines_dir(), owner_agent_id, group_id, true)
}

/// Permanently remove taught workflows whose recoverable deletion window has
/// elapsed. Gateway startup calls this even when the settings trash view is
/// never opened; listing trash calls it as a second, cheap lifecycle fence.
pub fn purge_expired_deleted_routines() -> Result<usize> {
    crate::config::private_io::with_private_lock(&routine_lifecycle_lock_path(), || {
        let dir = deleted_routines_dir();
        crate::config::private_io::prepare_phoenix_directory(&dir)?;
        let mut purged = 0usize;
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("failed to list {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let Some(raw) =
                crate::config::private_io::read_private_file_limited(&path, MAX_DOCUMENT_BYTES)?
            else {
                continue;
            };
            let routine: TaughtRoutine = serde_json::from_slice(&raw)
                .with_context(|| format!("invalid deleted workflow {}", path.display()))?;
            if routine.status != RoutineStatus::PendingDeletion {
                continue;
            }
            let Some(deadline) = routine.delete_after.as_deref() else {
                continue;
            };
            let deadline = chrono::DateTime::parse_from_rfc3339(deadline)
                .context("deleted workflow has an invalid deletion deadline")?
                .with_timezone(&Utc);
            if deadline <= Utc::now() && crate::config::private_io::remove_private_file(&path)? {
                purged = purged.saturating_add(1);
            }
        }
        Ok(purged)
    })
}

fn list_routines_in(
    dir: &Path,
    owner_agent_id: Option<&str>,
    group_id: Option<&str>,
    include_archived: bool,
) -> Result<Vec<RoutineSummary>> {
    crate::config::private_io::prepare_phoenix_directory(dir)?;
    let mut paths = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("failed to list {}", dir.display()))?
    {
        anyhow::ensure!(
            paths.len() < MAX_ROUTINES,
            "workflow library reached its safe bound"
        );
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut routines = Vec::new();
    for path in paths {
        let Some(raw) =
            crate::config::private_io::read_private_file_limited(&path, MAX_DOCUMENT_BYTES)?
        else {
            continue;
        };
        let routine: TaughtRoutine = serde_json::from_slice(&raw)
            .with_context(|| format!("invalid taught workflow {}", path.display()))?;
        if !include_archived && routine.status == RoutineStatus::Archived {
            continue;
        }
        if let Some(owner) = owner_agent_id {
            let visible = match routine.scope {
                WorkflowScope::Company => true,
                WorkflowScope::Agent => routine.owner_agent_id == owner,
                WorkflowScope::Group => routine.group_id.as_deref() == group_id,
            };
            if !visible {
                continue;
            }
        }
        routines.push(summarize(&routine));
    }
    routines.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(routines)
}

fn delete_routine(routine_id: &str, confirmed_name: &str) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    validate_text(confirmed_name, "confirmed workflow name", MAX_SHORT_BYTES)?;
    anyhow::ensure!(
        !confirmed_name.trim().is_empty(),
        "confirmed workflow name is empty"
    );
    crate::config::private_io::with_private_lock(&routine_lifecycle_lock_path(), || {
        let source = routine_path(routine_id);
        crate::config::private_io::with_private_lock(&source, || {
            let destination = deleted_routine_path(routine_id);

            // Crash recovery: the trash copy is written before the live copy
            // is removed. If a process stopped between those boundaries,
            // validate the exact identity/name and finish the move
            // idempotently.
            if let Some(raw) = crate::config::private_io::read_private_file_limited(
                &destination,
                MAX_DOCUMENT_BYTES,
            )? {
                let deleted: TaughtRoutine =
                    serde_json::from_slice(&raw).context("deleted taught workflow is invalid")?;
                anyhow::ensure!(
                    deleted.routine_id == routine_id && deleted.name == confirmed_name,
                    "workflow deletion confirmation does not match"
                );
                if let Some(live_raw) = crate::config::private_io::read_private_file_limited(
                    &source,
                    MAX_DOCUMENT_BYTES,
                )? {
                    let live: TaughtRoutine =
                        serde_json::from_slice(&live_raw).context("taught workflow is invalid")?;
                    anyhow::ensure!(
                        live.routine_id == routine_id && live.name == deleted.name,
                        "a different workflow occupies this routine id"
                    );
                }
                let _ = crate::config::private_io::remove_private_file_under_lock(&source)?;
                return Ok(deleted);
            }

            let raw =
                crate::config::private_io::read_private_file_limited(&source, MAX_DOCUMENT_BYTES)?
                    .context("taught workflow does not exist")?;
            let mut routine: TaughtRoutine =
                serde_json::from_slice(&raw).context("taught workflow is invalid")?;
            anyhow::ensure!(
                routine.routine_id == routine_id && routine.name == confirmed_name,
                "workflow deletion confirmation does not match"
            );
            let now = Utc::now();
            routine.status = RoutineStatus::PendingDeletion;
            routine.deleted_at = Some(now.to_rfc3339());
            routine.delete_after = Some((now + chrono::Duration::days(30)).to_rfc3339());
            routine.updated_at = now.to_rfc3339();
            let bytes = serialize_document(&routine)?;
            anyhow::ensure!(
                crate::config::private_io::atomic_write_private_if_missing(&destination, &bytes)?,
                "workflow is already pending deletion"
            );
            match crate::config::private_io::remove_private_file_under_lock(&source) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = crate::config::private_io::remove_private_file(&destination);
                    bail!("taught workflow disappeared before deletion completed");
                }
                Err(error) => {
                    let _ = crate::config::private_io::remove_private_file(&destination);
                    return Err(error).context("could not finish recoverable workflow deletion");
                }
            }
            Ok(routine)
        })
    })
}

fn restore_deleted_routine(routine_id: &str) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    crate::config::private_io::with_private_lock(&routine_lifecycle_lock_path(), || {
        let source = routine_path(routine_id);
        let destination = deleted_routine_path(routine_id);
        let raw =
            crate::config::private_io::read_private_file_limited(&destination, MAX_DOCUMENT_BYTES)?
                .context("deleted taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(&raw).context("deleted taught workflow is invalid")?;
        anyhow::ensure!(
            routine.routine_id == routine_id,
            "deleted workflow id mismatch"
        );
        if let Some(value) = routine.delete_after.as_deref() {
            let deadline = chrono::DateTime::parse_from_rfc3339(value)
                .context("deleted workflow has an invalid deletion deadline")?
                .with_timezone(&Utc);
            if deadline <= Utc::now() {
                crate::config::private_io::remove_private_file(&destination)?;
                bail!("the workflow's 30-day recovery window has elapsed");
            }
        }
        routine.status = if routine.consecutive_failures >= 3 {
            RoutineStatus::NeedsReview
        } else {
            RoutineStatus::Active
        };
        routine.deleted_at = None;
        routine.delete_after = None;
        routine.updated_at = Utc::now().to_rfc3339();
        let bytes = serialize_document(&routine)?;
        if !crate::config::private_io::atomic_write_private_if_missing(&source, &bytes)? {
            let existing = load_routine(routine_id)?;
            anyhow::ensure!(
                existing.routine_id == routine.routine_id && existing.name == routine.name,
                "a different workflow already occupies this routine id"
            );
            routine = existing;
        }
        crate::config::private_io::remove_private_file(&destination)
            .context("restored workflow but could not clear its trash copy")?;
        Ok(routine)
    })
}

fn summarize(routine: &TaughtRoutine) -> RoutineSummary {
    RoutineSummary {
        routine_id: routine.routine_id.clone(),
        name: routine.name.clone(),
        description: routine.description.clone(),
        owner_agent_id: routine.owner_agent_id.clone(),
        scope: routine.scope,
        group_id: routine.group_id.clone(),
        status: routine.status,
        trigger_phrases: routine.trigger_phrases.clone(),
        step_count: routine.steps.len(),
        successful_runs: routine.successful_runs,
        failed_runs: routine.failed_runs,
        consecutive_failures: routine.consecutive_failures,
        last_used_at: routine.last_used_at.clone(),
        delete_after: routine.delete_after.clone(),
    }
}

fn set_archived(routine_id: &str, archived: bool) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    let path = routine_path(routine_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(raw).context("taught workflow is invalid")?;
        routine.status = if archived {
            RoutineStatus::Archived
        } else if routine.consecutive_failures >= 3 {
            RoutineStatus::NeedsReview
        } else {
            RoutineStatus::Active
        };
        routine.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&routine)?;
        Ok((routine, replacement))
    })
}

fn set_scope(
    routine_id: &str,
    scope: WorkflowScope,
    group_id: Option<String>,
) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    let path = routine_path(routine_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(raw).context("taught workflow is invalid")?;
        anyhow::ensure!(
            routine.status != RoutineStatus::PendingDeletion,
            "restore the workflow before changing its scope"
        );
        if scope == WorkflowScope::Group {
            let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
            validate_scope(
                &snapshot,
                scope,
                group_id.as_deref(),
                &routine.owner_agent_id,
            )?;
        } else {
            anyhow::ensure!(group_id.is_none(), "only a group workflow may set group_id");
        }
        routine.scope = scope;
        routine.group_id = group_id.clone();
        routine.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&routine)?;
        Ok((routine, replacement))
    })
}

fn set_owner(routine_id: &str, requested_owner: &str) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    let snapshot = crate::runtime::company::global()?.directory_snapshot()?;
    let (owner_agent_id, _) = resolve_owner(&snapshot, requested_owner)?;
    let path = routine_path(routine_id);
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let raw = current.context("taught workflow does not exist")?;
        let mut routine: TaughtRoutine =
            serde_json::from_slice(raw).context("taught workflow is invalid")?;
        anyhow::ensure!(
            routine.status != RoutineStatus::PendingDeletion,
            "restore the workflow before rerouting it"
        );
        validate_scope(
            &snapshot,
            routine.scope,
            routine.group_id.as_deref(),
            &owner_agent_id,
        )?;
        routine.owner_agent_id = owner_agent_id.clone();
        routine.updated_at = Utc::now().to_rfc3339();
        let replacement = serialize_document(&routine)?;
        Ok((routine, replacement))
    })
}

fn write_teaching(teaching: &TeachingSession) -> Result<()> {
    validate_id(&teaching.teaching_id, "teaching_id")?;
    let path = teaching_path(&teaching.teaching_id);
    write_json(&path, teaching)
}

fn load_teaching(teaching_id: &str) -> Result<TeachingSession> {
    validate_id(teaching_id, "teaching_id")?;
    load_json(&teaching_path(teaching_id), "teaching session")
}

fn write_routine(routine: &TaughtRoutine) -> Result<()> {
    validate_id(&routine.routine_id, "routine_id")?;
    write_json(&routine_path(&routine.routine_id), routine)
}

fn load_routine(routine_id: &str) -> Result<TaughtRoutine> {
    validate_id(routine_id, "routine_id")?;
    load_json(&routine_path(routine_id), "taught workflow")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serialize_document(value)?;
    crate::config::private_io::atomic_write_private(path, &bytes)
}

fn serialize_document<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(value)?;
    anyhow::ensure!(
        bytes.len() <= MAX_DOCUMENT_BYTES,
        "workflow document is too large"
    );
    Ok(bytes)
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T> {
    let raw = crate::config::private_io::read_private_file_limited(path, MAX_DOCUMENT_BYTES)?
        .with_context(|| format!("{label} does not exist"))?;
    serde_json::from_slice(&raw).with_context(|| format!("{label} is invalid"))
}

fn workflows_dir() -> PathBuf {
    crate::config::phoenix_home().join("workflows")
}

fn teaching_dir() -> PathBuf {
    workflows_dir().join("teaching")
}

fn routines_dir() -> PathBuf {
    workflows_dir().join("library")
}

fn deleted_routines_dir() -> PathBuf {
    workflows_dir().join("trash")
}

fn routine_revisions_dir() -> PathBuf {
    workflows_dir().join("revisions")
}

fn routine_revision_path(routine: &TaughtRoutine) -> PathBuf {
    routine_revisions_dir()
        .join(&routine.routine_id)
        .join(format!(
            "{}.json",
            routine.content_sha256.trim_start_matches("sha256:")
        ))
}

fn routine_lifecycle_lock_path() -> PathBuf {
    workflows_dir().join("routine-lifecycle")
}

fn teaching_path(id: &str) -> PathBuf {
    teaching_dir().join(format!("{id}.json"))
}

fn routine_path(id: &str) -> PathBuf {
    routines_dir().join(format!("{id}.json"))
}

fn deleted_routine_path(id: &str) -> PathBuf {
    deleted_routines_dir().join(format!("{id}.json"))
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')),
        "invalid {label}"
    );
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<()> {
    anyhow::ensure!(
        value.len() <= max && !value.contains('\0'),
        "{label} is too large or invalid"
    );
    Ok(())
}

fn routine_slug(name: &str) -> String {
    let mut slug = parameter_slug(name);
    if slug.is_empty() {
        slug = "workflow".to_string();
    }
    slug.truncate(72);
    format!(
        "{}-{}",
        slug,
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )
}

fn parameter_slug(value: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in value.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            separator = false;
        } else if !separator && !out.is_empty() {
            out.push('_');
            separator = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out.truncate(64);
    out
}

fn parameter_name_for_target(target: &BrowserSemanticTarget, sensitive: bool) -> String {
    let source = [
        target.label.as_str(),
        target.name.as_str(),
        target.input_type.as_str(),
        if sensitive { "password" } else { "value" },
    ]
    .into_iter()
    .find(|value| !value.trim().is_empty())
    .unwrap_or("value");
    let result = parameter_slug(source);
    if result.is_empty() {
        "value".to_string()
    } else {
        result
    }
}

fn sanitize_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.chars().take(2_048).collect();
    };
    url.set_fragment(None);
    let sensitive_keys = [
        "token", "secret", "password", "passwd", "code", "auth", "session", "key",
    ];
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let is_sensitive = sensitive_keys
                .iter()
                .any(|needle| key.to_ascii_lowercase().contains(needle));
            (
                key.into_owned(),
                if is_sensitive {
                    "[redacted]".to_string()
                } else {
                    value.into_owned()
                },
            )
        })
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

fn normalize_list(values: Vec<String>, max: usize, max_item: usize) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        validate_text(value, "workflow trigger", max_item)?;
        let key = value.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(value.to_string());
        }
        anyhow::ensure!(out.len() <= max, "too many workflow triggers");
    }
    Ok(out)
}

fn collect_parameters(steps: &[RoutineStep]) -> Vec<RoutineParameter> {
    let mut parameters = BTreeMap::new();
    for step in steps {
        if let RoutineStep::Type {
            target,
            value: RoutineValue::Parameter { name, sensitive },
            ..
        } = step
        {
            let description = if target.label.is_empty() {
                format!("Value for the {} field", target.tag)
            } else {
                format!("Value for {}", target.label)
            };
            parameters.entry(name.clone()).or_insert(RoutineParameter {
                name: name.clone(),
                description,
                sensitive: *sensitive,
            });
        }
    }
    parameters.into_values().collect()
}

fn render_playbook(steps: &[RoutineStep], parameters: &[RoutineParameter]) -> String {
    let mut out = String::from(
        "Use fresh browser state at every navigation or page-changing click. Match targets by label, role, text, and stable attributes; the recorded selector is a fallback, never a reason to click stale coordinates.\n",
    );
    if parameters.iter().any(|parameter| parameter.sensitive) {
        out.push_str("Sensitive parameters come from the Phoenix vault/login flow and must never be requested, logged, or written as plaintext.\n");
    }
    for (index, step) in steps.iter().enumerate() {
        let line = match step {
            RoutineStep::Navigate { url } => format!("Navigate to {url}."),
            RoutineStep::GoBack => "Go back one page.".to_string(),
            RoutineStep::Click { target, .. } => format!("Click {}.", target_label(target)),
            RoutineStep::Type {
                target,
                value: RoutineValue::Literal { value },
                clear,
            } => format!(
                "{} `{}` in {}.",
                if *clear { "Type" } else { "Append" },
                value,
                target_label(target)
            ),
            RoutineStep::Type {
                target,
                value: RoutineValue::Parameter { name, sensitive },
                clear,
            } => format!(
                "{} parameter `{{{{{name}}}}}`{} in {}.",
                if *clear { "Type" } else { "Append" },
                if *sensitive { " from the vault" } else { "" },
                target_label(target)
            ),
            RoutineStep::SendKeys { keys, target } => format!(
                "Send keys `{keys}`{}.",
                target
                    .as_ref()
                    .map(|target| format!(" while focused on {}", target_label(target)))
                    .unwrap_or_default()
            ),
            RoutineStep::Select { target, option } => {
                format!("Select `{option}` in {}.", target_label(target))
            }
        };
        out.push_str(&format!("{}. {line}\n", index + 1));
    }
    out
}

fn target_label(target: &BrowserSemanticTarget) -> String {
    if !target.label.is_empty() {
        format!("the {} `{}`", target.tag, target.label)
    } else if !target.text.is_empty() {
        format!("the {} `{}`", target.tag, target.text)
    } else if !target.name.is_empty() {
        format!("the {} named `{}`", target.tag, target.name)
    } else {
        format!("the {} (`{}`)", target.tag, target.selector)
    }
}

fn content_hash(steps: &[RoutineStep], playbook: &str) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(steps)?);
    hasher.update(playbook.as_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn summary_score(routine: &RoutineSummary, request: &str) -> usize {
    let query = token_set(request);
    if query.is_empty() {
        return 0;
    }
    let haystack = token_set(&format!(
        "{} {} {}",
        routine.name,
        routine.description,
        routine.trigger_phrases.join(" ")
    ));
    query.intersection(&haystack).count()
}

fn confident_summary_match(routine: &RoutineSummary, request: &str) -> bool {
    if summary_score(routine, request) >= 2 {
        return true;
    }
    let request = request.to_ascii_lowercase();
    routine.trigger_phrases.iter().any(|phrase| {
        let phrase = phrase.trim().to_ascii_lowercase();
        phrase.len() >= 5 && request.contains(&phrase)
    })
}

fn token_set(value: &str) -> BTreeSet<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "from", "into", "this", "that", "your", "you", "our", "are",
        "was", "were", "have", "has", "can", "could", "please",
    ];
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 3 && !STOP.contains(&token.as_str()))
        .collect()
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{field} is required"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(input_type: &str) -> BrowserSemanticTarget {
        BrowserSemanticTarget {
            tag: "input".into(),
            input_type: input_type.into(),
            label: "Account password".into(),
            selector: "#password".into(),
            ..Default::default()
        }
    }

    fn test_routine(routine_id: &str, name: &str) -> TaughtRoutine {
        TaughtRoutine {
            schema_version: 1,
            routine_id: routine_id.into(),
            name: name.into(),
            description: "Test routine".into(),
            owner_agent_id: "agent-nico".into(),
            scope: WorkflowScope::Agent,
            group_id: None,
            status: RoutineStatus::Active,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            trigger_phrases: vec!["test".into()],
            parameters: Vec::new(),
            starting_url: None,
            playbook: "1. Test.".into(),
            content_sha256: "sha256:test".into(),
            steps: vec![RoutineStep::GoBack],
            successful_runs: 0,
            failed_runs: 0,
            consecutive_failures: 0,
            last_used_at: None,
            last_result: None,
            deleted_at: None,
            delete_after: None,
        }
    }

    #[test]
    fn password_recording_never_persists_the_value() {
        let action = BrowserUserAction::Type {
            text: "correct horse battery staple".into(),
            clear: true,
            sensitive: false,
            parameter_name: None,
            target_hint: None,
        };
        let step = record_step(
            &action,
            BrowserInteractionReceipt {
                action: "type".into(),
                before_url: "https://example.com/login".into(),
                after_url: "https://example.com/login".into(),
                target: Some(target("password")),
                sensitive: true,
            },
        )
        .unwrap();
        let encoded = serde_json::to_string(&step).unwrap();
        assert!(!encoded.contains("correct horse"));
        assert!(encoded.contains("parameter"));
        assert!(encoded.contains("sensitive"));
    }

    #[test]
    fn adjacent_typing_batches_become_one_semantic_fill() {
        let field = BrowserSemanticTarget {
            tag: "textarea".into(),
            label: "Prompt".into(),
            selector: "#prompt".into(),
            ..Default::default()
        };
        let mut steps = Vec::new();
        append_recorded_step(
            &mut steps,
            RoutineStep::Type {
                target: field.clone(),
                value: RoutineValue::Literal {
                    value: "Draw a ".into(),
                },
                clear: true,
            },
        );
        append_recorded_step(
            &mut steps,
            RoutineStep::Type {
                target: field,
                value: RoutineValue::Literal {
                    value: "phoenix".into(),
                },
                clear: false,
            },
        );
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            RoutineStep::Type { value, clear, .. } => {
                assert!(*clear);
                assert!(matches!(
                    value,
                    RoutineValue::Literal { value } if value == "Draw a phoenix"
                ));
            }
            _ => panic!("expected one type step"),
        }
    }

    #[test]
    fn typing_in_different_fields_remains_two_actions() {
        let mut steps = Vec::new();
        for selector in ["#first-name", "#last-name"] {
            append_recorded_step(
                &mut steps,
                RoutineStep::Type {
                    target: BrowserSemanticTarget {
                        tag: "input".into(),
                        selector: selector.into(),
                        ..Default::default()
                    },
                    value: RoutineValue::Literal {
                        value: "Ada".into(),
                    },
                    clear: true,
                },
            );
        }
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn native_field_flush_replaces_instead_of_appending_the_complete_value() {
        let field = BrowserSemanticTarget {
            tag: "input".into(),
            selector: "#title".into(),
            ..Default::default()
        };
        let mut steps = vec![RoutineStep::Type {
            target: field.clone(),
            value: RoutineValue::Literal {
                value: "Phoenix".into(),
            },
            clear: true,
        }];
        append_recorded_step(
            &mut steps,
            RoutineStep::Type {
                target: field,
                value: RoutineValue::Literal {
                    value: "Phoenix launch".into(),
                },
                clear: true,
            },
        );
        assert!(matches!(
            &steps[0],
            RoutineStep::Type {
                value: RoutineValue::Literal { value },
                clear: true,
                ..
            } if value == "Phoenix launch"
        ));
    }

    #[test]
    fn observed_link_navigation_updates_click_instead_of_reloading_as_a_second_step() {
        let mut steps = vec![RoutineStep::Click {
            target: BrowserSemanticTarget {
                tag: "a".into(),
                label: "Dashboard".into(),
                selector: "#dashboard".into(),
                ..Default::default()
            },
            before_url: "https://example.com/".into(),
            after_url: "https://example.com/".into(),
        }];
        append_native_recorded_step(
            &mut steps,
            RoutineStep::Navigate {
                url: "https://example.com/dashboard".into(),
            },
            true,
        );
        assert_eq!(steps.len(), 1);
        assert!(matches!(
            &steps[0],
            RoutineStep::Click { after_url, .. }
                if after_url == "https://example.com/dashboard"
        ));
    }

    #[test]
    fn observer_navigation_after_explicit_back_is_not_a_duplicate_step() {
        let mut steps = vec![RoutineStep::GoBack];
        append_native_recorded_step(
            &mut steps,
            RoutineStep::Navigate {
                url: "https://example.com/previous".into(),
            },
            false,
        );
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], RoutineStep::GoBack));
    }

    #[test]
    fn sensitive_query_values_are_redacted() {
        let sanitized = sanitize_url(
            "https://example.com/callback?code=very-secret&view=compact#access_token=also-secret",
        );
        assert!(!sanitized.contains("very-secret"));
        assert!(!sanitized.contains("also-secret"));
        assert!(sanitized.contains("view=compact"));
        assert!(sanitized.contains("redacted"));
    }

    #[test]
    fn routine_execution_match_requires_real_overlap_or_an_exact_trigger() {
        let mut routine = summarize(&test_routine("weekly-inbox", "Weekly inbox review"));
        routine.description = "Review unread mail and prepare the weekly digest".into();
        routine.trigger_phrases = vec!["clear my weekly inbox".into()];

        assert!(confident_summary_match(
            &routine,
            "Please review my weekly inbox and prepare its digest"
        ));
        assert!(confident_summary_match(
            &routine,
            "Can you clear my weekly inbox before lunch?"
        ));
        assert!(
            !confident_summary_match(&routine, "review the engineering release"),
            "one generic shared token must remain a hint, never a blocking match"
        );
    }

    #[test]
    fn three_failures_require_review_and_success_recovers() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let routine = test_routine("test-routine", "Test");
        write_routine(&routine).unwrap();
        for _ in 0..3 {
            record_result("test-routine", "agent-nico", None, false, "page changed").unwrap();
        }
        assert_eq!(
            load_routine("test-routine").unwrap().status,
            RoutineStatus::NeedsReview
        );
        assert!(mark_began("test-routine", "agent-nico", None)
            .unwrap_err()
            .to_string()
            .contains("needs review"));
        record_result("test-routine", "agent-nico", None, true, "verified").unwrap();
        let recovered = load_routine("test-routine").unwrap();
        assert_eq!(recovered.status, RoutineStatus::Active);
        assert_eq!(recovered.consecutive_failures, 0);
    }

    #[test]
    fn failure_quarantine_setting_keeps_a_failing_workflow_available() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        crate::settings::execute(crate::settings::SettingsCommand::Set {
            key: "workflows.failure_quarantine".into(),
            value: serde_json::json!(false),
            scope: crate::settings::SettingsScope::Agent {
                id: "agent-nico".into(),
            },
            expected_revision: Some(0),
        })
        .unwrap();
        write_routine(&test_routine("unquarantined", "Unquarantined")).unwrap();
        for _ in 0..4 {
            record_result("unquarantined", "agent-nico", None, false, "page changed").unwrap();
        }
        let routine = load_routine("unquarantined").unwrap();
        assert_eq!(routine.status, RoutineStatus::Active);
        assert_eq!(routine.consecutive_failures, 4);
    }

    #[test]
    fn recoverable_delete_requires_the_exact_visible_name() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        write_routine(&test_routine("invoice-run", "Send invoices")).unwrap();

        let error = delete_routine("invoice-run", "send invoices").unwrap_err();
        assert!(error
            .to_string()
            .contains("workflow deletion confirmation does not match"));
        assert!(routine_path("invoice-run").exists());
        assert!(!deleted_routine_path("invoice-run").exists());
    }

    #[test]
    fn deleted_routine_is_hidden_for_thirty_days_and_can_be_restored() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        write_routine(&test_routine("invoice-run", "Send invoices")).unwrap();
        let before = Utc::now();

        let deleted = delete_routine("invoice-run", "Send invoices").unwrap();
        assert_eq!(deleted.status, RoutineStatus::PendingDeletion);
        assert!(!routine_path("invoice-run").exists());
        assert!(deleted_routine_path("invoice-run").exists());
        let deadline = chrono::DateTime::parse_from_rfc3339(
            deleted.delete_after.as_deref().expect("deletion deadline"),
        )
        .unwrap()
        .with_timezone(&Utc);
        assert!(deadline >= before + chrono::Duration::days(30));
        assert!(deadline <= Utc::now() + chrono::Duration::days(30));
        assert!(list_routines(Some("agent-nico"), None, true)
            .unwrap()
            .is_empty());
        let trash = list_deleted_routines(Some("agent-nico"), None).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].status, RoutineStatus::PendingDeletion);

        let restored = restore_deleted_routine("invoice-run").unwrap();
        assert_eq!(restored.status, RoutineStatus::Active);
        assert!(restored.deleted_at.is_none());
        assert!(restored.delete_after.is_none());
        assert!(routine_path("invoice-run").exists());
        assert!(!deleted_routine_path("invoice-run").exists());
    }

    #[test]
    fn expired_deleted_routines_are_permanently_purged() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        write_routine(&test_routine("expired-run", "Expired routine")).unwrap();
        let mut deleted = delete_routine("expired-run", "Expired routine").unwrap();
        deleted.delete_after = Some((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
        crate::config::private_io::atomic_write_private(
            &deleted_routine_path("expired-run"),
            &serialize_document(&deleted).unwrap(),
        )
        .unwrap();

        assert_eq!(purge_expired_deleted_routines().unwrap(), 1);
        assert!(!deleted_routine_path("expired-run").exists());
        assert_eq!(purge_expired_deleted_routines().unwrap(), 0);
    }

    #[test]
    fn expired_deleted_routine_cannot_be_restored_before_a_sweep() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        write_routine(&test_routine("expired-run", "Expired routine")).unwrap();
        let mut deleted = delete_routine("expired-run", "Expired routine").unwrap();
        deleted.delete_after = Some((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
        crate::config::private_io::atomic_write_private(
            &deleted_routine_path("expired-run"),
            &serialize_document(&deleted).unwrap(),
        )
        .unwrap();

        let error = restore_deleted_routine("expired-run").unwrap_err();
        assert!(error.to_string().contains("recovery window has elapsed"));
        assert!(!routine_path("expired-run").exists());
        assert!(!deleted_routine_path("expired-run").exists());
    }

    #[test]
    fn finalize_atomically_promotes_a_private_semantic_playbook() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let teaching = TeachingSession {
            schema_version: 1,
            teaching_id: "teach-finalize".into(),
            owner_agent_id: "agent-nico".into(),
            browser_profile_id: "agent-nico".into(),
            scope: WorkflowScope::Agent,
            group_id: None,
            revises_routine_id: None,
            status: TeachingStatus::Recording,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            steps: vec![
                RoutineStep::Navigate {
                    url: "https://example.com/login".into(),
                },
                RoutineStep::Type {
                    target: target("password"),
                    value: RoutineValue::Parameter {
                        name: "account_password".into(),
                        sensitive: true,
                    },
                    clear: true,
                },
            ],
        };
        write_teaching(&teaching).unwrap();
        let reply = finalize(
            "teach-finalize",
            "Sign in to Example".into(),
            "Open Example and establish the saved account session.".into(),
            vec!["log into example".into()],
            Some(WorkflowScope::Company),
            None,
        )
        .unwrap();
        let routine = reply.routine.unwrap();
        assert_eq!(routine.scope, WorkflowScope::Company);
        assert!(routine.playbook.contains("fresh browser state"));
        assert!(routine.playbook.contains("from the vault"));
        assert!(!routine.playbook.contains("x:"));
        assert!(routine.content_sha256.starts_with("sha256:"));
        assert!(!teaching_path("teach-finalize").exists());
        let path = routine_path(&routine.routine_id);
        assert!(path.is_file());
        let private = set_scope(&routine.routine_id, WorkflowScope::Agent, None).unwrap();
        assert_eq!(private.scope, WorkflowScope::Agent);
        assert!(private.group_id.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let summaries = list_routines(Some("agent-nico"), None, false).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].scope, WorkflowScope::Agent);
        assert!(summaries[0]
            .trigger_phrases
            .iter()
            .any(|trigger| trigger == "log into example"));

        let mut failed = load_routine(&routine.routine_id).unwrap();
        failed.successful_runs = 4;
        failed.failed_runs = 3;
        failed.consecutive_failures = 3;
        failed.status = RoutineStatus::NeedsReview;
        let revision_backup = routine_revision_path(&failed);
        write_routine(&failed).unwrap();
        let revision_teaching = TeachingSession {
            teaching_id: "teach-revision".into(),
            revises_routine_id: Some(failed.routine_id.clone()),
            steps: vec![RoutineStep::Navigate {
                url: "https://example.com/account".into(),
            }],
            updated_at: Utc::now().to_rfc3339(),
            created_at: Utc::now().to_rfc3339(),
            status: TeachingStatus::Recording,
            schema_version: 1,
            owner_agent_id: failed.owner_agent_id.clone(),
            browser_profile_id: failed.owner_agent_id.clone(),
            scope: failed.scope,
            group_id: failed.group_id.clone(),
        };
        write_teaching(&revision_teaching).unwrap();
        let revised = finalize(
            "teach-revision",
            "Sign in to Example safely".into(),
            "Use the corrected account flow.".into(),
            vec!["open my example account".into()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(revised.result, "routine_revised");
        let revised = revised.routine.unwrap();
        assert_eq!(revised.routine_id, routine.routine_id);
        assert_eq!(revised.successful_runs, 4);
        assert_eq!(revised.failed_runs, 3);
        assert_eq!(revised.consecutive_failures, 0);
        assert_eq!(revised.status, RoutineStatus::Active);
        assert!(revision_backup.is_file());
    }
}
