//! Local runtime trace receipts for Phoenix turns.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::runtime::RuntimeExecution;

pub const RUN_TRACE_SCHEMA_VERSION: u32 = 2;
const MAX_RUN_ID_BYTES: usize = 128;
const MAX_RUN_TRACE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RUN_DIRECTORIES: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTrace {
    pub schema_version: u32,
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub mode: String,
    pub backing: String,
    pub workspace_root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunDiagnostics>,
    pub execution: Option<RuntimeExecution>,
    pub error: Option<RunErrorTrace>,
}

/// Extra fields for debugging failed or successful provider-backed turns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_request_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specialist_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub librarian_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specialist_response_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunErrorTrace {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TraceContext {
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    /// The repo the agent operated on (informational; recorded in the trace).
    pub workspace_root: PathBuf,
    /// Where run traces are written — the unified `~/.phoenix/runs` directory.
    pub runs_dir: PathBuf,
}

impl TraceContext {
    pub fn new(workspace_root: impl Into<PathBuf>, runs_dir: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        let runs_dir = runs_dir.into();
        let created_at = Utc::now();
        Self {
            run_id: new_run_id(created_at),
            created_at,
            workspace_root,
            runs_dir,
        }
    }

    pub fn success_trace(
        &self,
        mode: impl Into<String>,
        backing: impl Into<String>,
        execution: RuntimeExecution,
        diagnostics: Option<RunDiagnostics>,
    ) -> RunTrace {
        RunTrace {
            schema_version: RUN_TRACE_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            created_at: self.created_at,
            mode: mode.into(),
            backing: backing.into(),
            workspace_root: self.workspace_root.clone(),
            diagnostics,
            execution: Some(execution),
            error: None,
        }
    }

    pub fn error_trace(
        &self,
        mode: impl Into<String>,
        backing: impl Into<String>,
        error: impl Into<String>,
        diagnostics: Option<RunDiagnostics>,
    ) -> RunTrace {
        let message = error.into();
        RunTrace {
            schema_version: RUN_TRACE_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            created_at: self.created_at,
            mode: mode.into(),
            backing: backing.into(),
            workspace_root: self.workspace_root.clone(),
            diagnostics,
            execution: None,
            error: Some(RunErrorTrace::from_message(message)),
        }
    }
}

impl RunErrorTrace {
    pub fn from_message(message: String) -> Self {
        let chain: Vec<String> = message
            .split(": ")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        let root_cause = chain.last().cloned();
        Self {
            message,
            root_cause,
            chain,
        }
    }

    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        let chain: Vec<String> = error.chain().map(|e| e.to_string()).collect();
        let message = chain.first().cloned().unwrap_or_else(|| error.to_string());
        let root_cause = chain.last().cloned();
        Self {
            message,
            root_cause,
            chain,
        }
    }
}

pub fn preview_user_request(request: &str, max_chars: usize) -> String {
    let trimmed = request.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for ch in trimmed.chars().take(max_chars) {
        out.push(ch);
    }
    out.push('…');
    out
}

pub fn new_run_id(now: DateTime<Utc>) -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    format!("run_{}_{}", now.format("%Y%m%d_%H%M%S"), &uuid[..8])
}

pub fn save_run_trace(trace: &RunTrace, runs_dir: &Path) -> Result<PathBuf> {
    let path = trace_path_for(runs_dir, &trace.run_id)?;
    write_run_trace(trace, &path)?;
    Ok(path)
}

pub fn prune_old_run_traces(runs_dir: &Path, now: DateTime<Utc>) -> Result<usize> {
    let runs_root = runs_dir;
    match std::fs::symlink_metadata(runs_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => anyhow::bail!(
            "run-trace root is not a real directory: {}",
            runs_root.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("inspecting run-trace root"),
    }
    crate::config::private_io::reject_symlink_components(runs_root)?;

    let cutoff = now - chrono::Duration::days(30);
    let mut removed = 0usize;
    for (index, entry) in std::fs::read_dir(runs_root)?.enumerate() {
        if index >= MAX_RUN_DIRECTORIES {
            tracing::warn!(
                "run trace pruning reached the {MAX_RUN_DIRECTORIES}-entry safety limit"
            );
            break;
        }
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(run_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if validate_run_id(&run_id).is_err() {
            continue;
        }
        let trace_path = path.join("trace.json");
        let modified = match std::fs::symlink_metadata(&trace_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                metadata.modified().ok().map(DateTime::<Utc>::from)
            }
            _ => None,
        };
        if let Some(modified) = modified {
            if modified < cutoff {
                // A run directory currently contains only trace.json. Refuse
                // recursive deletion if anything unexpected appeared there;
                // retention must never become a broad cleanup primitive.
                let mut saw_trace = false;
                let mut safe_contents = true;
                for (child_index, child) in std::fs::read_dir(&path)?.enumerate() {
                    if child_index >= 4 {
                        safe_contents = false;
                        break;
                    }
                    let child = child?;
                    let name = child.file_name();
                    let allowed = name == "trace.json" || name == ".trace.json.lock";
                    if !allowed || !child.file_type()?.is_file() {
                        safe_contents = false;
                        break;
                    }
                    saw_trace |= name == "trace.json";
                }
                if !safe_contents || !saw_trace {
                    tracing::warn!(
                        "run trace pruning skipped unexpected directory contents in {}",
                        path.display()
                    );
                    continue;
                }
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("failed to remove old trace {}", path.display()))?;
                removed += 1;
            }
        }
    }

    Ok(removed)
}

pub fn write_run_trace(trace: &RunTrace, path: &Path) -> Result<()> {
    validate_run_id(&trace.run_id)?;
    if path.file_name().and_then(|name| name.to_str()) != Some("trace.json")
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(trace.run_id.as_str())
    {
        anyhow::bail!(
            "run trace path {} does not match run id {}",
            path.display(),
            trace.run_id
        );
    }
    let json = serde_json::to_vec_pretty(trace).context("failed to serialize run trace")?;
    if json.len() > MAX_RUN_TRACE_BYTES {
        anyhow::bail!(
            "run trace serializes to {} bytes; maximum is {MAX_RUN_TRACE_BYTES}",
            json.len()
        );
    }
    crate::config::private_io::atomic_write_private(path, &json)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn validate_run_id(run_id: &str) -> Result<()> {
    if !run_id.starts_with("run_") || run_id.len() > MAX_RUN_ID_BYTES {
        anyhow::bail!("invalid run id {run_id:?}");
    }
    if !run_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        anyhow::bail!("run id contains unsafe path characters");
    }
    Ok(())
}

fn trace_path_for(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    validate_run_id(run_id)?;
    Ok(runs_dir.join(run_id).join("trace.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration as StdDuration, SystemTime};

    #[test]
    fn error_trace_round_trips() {
        let context = TraceContext {
            run_id: "run_20260521_120000_abcdef12".to_string(),
            created_at: DateTime::parse_from_rfc3339("2026-05-21T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            workspace_root: PathBuf::from("/tmp/phoenix"),
            runs_dir: PathBuf::from("/tmp/phoenix/.phoenix/runs"),
        };
        let trace = context.error_trace("scaffold", "test backing", "provider failed", None);

        let json = serde_json::to_string(&trace).unwrap();
        let decoded: RunTrace = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.schema_version, RUN_TRACE_SCHEMA_VERSION);
        assert_eq!(decoded.run_id, "run_20260521_120000_abcdef12");
        assert!(decoded.execution.is_none());
        assert_eq!(decoded.error.unwrap().message, "provider failed");
    }

    #[test]
    fn writer_creates_trace_file() {
        let tempdir = tempfile::tempdir().unwrap();
        let runs_dir = tempdir.path().join(".phoenix").join("runs");
        let context = TraceContext {
            run_id: "run_20260521_120000_abcdef12".to_string(),
            created_at: DateTime::parse_from_rfc3339("2026-05-21T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            workspace_root: tempdir.path().to_path_buf(),
            runs_dir: runs_dir.clone(),
        };
        let trace = context.error_trace("scaffold", "test backing", "provider failed", None);

        let path = save_run_trace(&trace, &context.runs_dir).unwrap();

        assert!(path.exists());
        assert_eq!(
            path,
            runs_dir
                .join("run_20260521_120000_abcdef12")
                .join("trace.json")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn unsafe_run_id_cannot_escape_runs_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut trace = TraceContext::new(tempdir.path(), tempdir.path())
            .error_trace("scaffold", "test", "failed", None);
        trace.run_id = "run_../../escape".into();
        assert!(save_run_trace(&trace, tempdir.path()).is_err());
        assert!(!tempdir.path().join("escape").exists());
    }

    #[test]
    fn trace_retention_removes_old_runs() {
        let tempdir = tempfile::tempdir().unwrap();
        let runs_dir = tempdir.path().join(".phoenix").join("runs");
        let old_run = runs_dir.join("run_old");
        std::fs::create_dir_all(&old_run).unwrap();
        let trace_path = old_run.join("trace.json");
        std::fs::write(&trace_path, "{}").unwrap();

        let old_time = SystemTime::now() - StdDuration::from_secs(31 * 24 * 60 * 60);
        filetime_set(&trace_path, old_time);

        let removed = prune_old_run_traces(&runs_dir, Utc::now()).unwrap();
        assert_eq!(removed, 1);
        assert!(!old_run.exists());
    }

    #[cfg(unix)]
    fn filetime_set(path: &Path, time: SystemTime) {
        use std::process::Command;

        let timestamp = DateTime::<Utc>::from(time)
            .format("%Y%m%d%H%M.%S")
            .to_string();
        let status = Command::new("touch")
            .arg("-t")
            .arg(timestamp)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(not(unix))]
    fn filetime_set(_path: &Path, _time: SystemTime) {}
}
