//! Managed shell jobs use the existing Bash executor and durable postbox.
//! Starting a job returns immediately. Only the executor's terminal result
//! can publish a completion and resume the canonical conversation owner.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{ToolCancellation, ToolOutput};
use crate::runtime::postbox::{CompletedJob, ReturnKind};

const MAX_ACTIVE: usize = 16;
const MAX_ACTIVE_PER_SESSION: usize = 4;
const MAX_RECORD_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Unconfirmed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub session_id: String,
    pub agent: String,
    pub label: String,
    pub status: JobStatus,
    pub started: chrono::DateTime<chrono::Utc>,
    pub finished: Option<chrono::DateTime<chrono::Utc>>,
    pub output: Option<String>,
    #[serde(default)]
    pub delivered: bool,
}

impl JobRecord {
    fn receipt(&self) -> CompletedJob {
        CompletedJob {
            kind: ReturnKind::Terminal,
            delivery_id: self.job_id.clone(),
            causation_id: None,
            agent: self.agent.clone(),
            subject: self.label.clone(),
            ok: self.status == JobStatus::Succeeded,
            summary: format!("Terminal job {}: {:?}", self.job_id, self.status),
            body: format!(
                "Managed terminal job `{}` has reached {:?}.\n\n{}",
                self.job_id,
                self.status,
                self.output.as_deref().unwrap_or("No output recorded.")
            ),
            finished: self
                .finished
                .expect("only an actual terminal state has a receipt"),
        }
    }
}

struct ActiveJob {
    record: JobRecord,
    cancellation: ToolCancellation,
}
fn active() -> &'static Mutex<HashMap<String, ActiveJob>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, ActiveJob>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_path(id: &str) -> Result<PathBuf> {
    let suffix = id
        .strip_prefix("terminal_")
        .context("invalid terminal job id")?;
    anyhow::ensure!(
        suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid terminal job id"
    );
    Ok(crate::config::phoenix_home()
        .join("terminal-jobs")
        .join(format!("{id}.json")))
}

fn save(record: &JobRecord) -> Result<()> {
    let path = record_path(&record.job_id)?;
    crate::config::private_io::prepare_phoenix_directory(path.parent().unwrap())?;
    let bytes = serde_json::to_vec(record)?;
    anyhow::ensure!(
        bytes.len() <= MAX_RECORD_BYTES,
        "terminal job record exceeded its bound"
    );
    crate::config::private_io::atomic_write_private(&path, &bytes)
}

fn bounded_output(text: String) -> String {
    const LIMIT: usize = 64 * 1024;
    if text.len() <= LIMIT {
        return text;
    }
    let head = text.floor_char_boundary(LIMIT / 2);
    let tail = text.ceil_char_boundary(text.len() - LIMIT / 2);
    format!(
        "{}\n[terminal output truncated; middle omitted]\n{}",
        &text[..head],
        &text[tail..]
    )
}

pub fn status(session: &str, agent: &str, id: &str) -> Result<JobRecord> {
    let record = {
        let jobs = active().lock().unwrap_or_else(|p| p.into_inner());
        jobs.get(id).map(|job| job.record.clone())
    };
    let record = match record {
        Some(record) => record,
        None => {
            let bytes = crate::config::private_io::read_private_file_limited(
                &record_path(id)?,
                MAX_RECORD_BYTES,
            )?
            .context("terminal job is not available")?;
            serde_json::from_slice::<JobRecord>(&bytes).context("terminal job record is corrupt")?
        }
    };
    anyhow::ensure!(
        record.job_id == id && record.session_id == session && record.agent == agent,
        "terminal job belongs to another conversation or acting owner"
    );
    Ok(record)
}

pub(super) fn start(
    session: &str,
    agent: &str,
    label: &str,
    run: impl FnOnce(ToolCancellation) -> Result<ToolOutput> + Send + 'static,
) -> Result<JobRecord> {
    crate::session::SessionStore::validate_session_id(session)?;
    anyhow::ensure!(crate::runtime::postbox::wake_available(),
        "Managed background terminal jobs require the running Phoenix gateway; no command was started.");
    anyhow::ensure!(!agent.is_empty(), "terminal job has no acting owner");
    let runtime =
        tokio::runtime::Handle::try_current().context("terminal job needs the gateway runtime")?;
    let record = JobRecord {
        job_id: format!("terminal_{}", uuid::Uuid::new_v4().simple()),
        session_id: session.into(),
        agent: agent.into(),
        label: label.chars().take(160).collect(),
        status: JobStatus::Running,
        started: chrono::Utc::now(),
        finished: None,
        output: None,
        delivered: false,
    };
    let cancellation = ToolCancellation::default();
    {
        let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
        anyhow::ensure!(jobs.len() < MAX_ACTIVE, "too many active terminal jobs");
        anyhow::ensure!(
            jobs.values()
                .filter(|job| job.record.session_id == session)
                .count()
                < MAX_ACTIVE_PER_SESSION,
            "this conversation already has four active terminal jobs"
        );
        save(&record)?;
        jobs.insert(
            record.job_id.clone(),
            ActiveJob {
                record: record.clone(),
                cancellation: cancellation.clone(),
            },
        );
    }
    let id = record.job_id.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("phoenix-{id}"))
        .spawn(move || {
            let _runtime = runtime.enter();
            let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(cancellation.clone())
            })) {
                Ok(result) => result,
                Err(_) => {
                    cancellation.mark_unconfirmed();
                    Err(anyhow::anyhow!(
                        "Terminal job handler panicked; its external effects require verification."
                    ))
                }
            };
            {
                let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
                let Some(job) = jobs.get_mut(&id) else {
                    return;
                };
                job.record.status = if cancellation.has_unconfirmed_external_work() {
                    JobStatus::Unconfirmed
                } else if cancellation.is_requested() {
                    JobStatus::Cancelled
                } else if result.is_ok() {
                    JobStatus::Succeeded
                } else {
                    JobStatus::Failed
                };
                job.record.finished = Some(chrono::Utc::now());
                job.record.output = Some(bounded_output(match result {
                    Ok(output) => format!("{}\n{}", output.summary, output.content),
                    Err(error) => format!("{error:#}"),
                }));
            }
            // Keep the actual result until it is durable. Publishing against
            // the old Running record fails receipt admission and loses the
            // only evidence that the command exited. Never rerun the command
            // to recover from a result-file write failure.
            let finished = loop {
                let persisted = {
                    let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
                    let Some(job) = jobs.get_mut(&id) else { return; };
                    if cancellation.is_requested() {
                        job.record.status = JobStatus::Cancelled;
                    }
                    let finished = job.record.clone();
                    match save(&finished) {
                        Ok(()) => { jobs.remove(&id); Some(finished) }
                        Err(error) => {
                            tracing::error!("terminal job {id}: retaining completed result for retry: {error:#}");
                            None
                        }
                    }
                };
                if let Some(finished) = persisted { break finished; }
                std::thread::sleep(Duration::from_secs(1));
            };
            // User cancellation is a terminal receipt, not permission to start a
            // fresh owner turn after Stop. Natural success/failure resumes once.
            if !cancellation.is_requested()
                && matches!(finished.status, JobStatus::Succeeded | JobStatus::Failed) {
                crate::runtime::postbox::job_finished(&finished.session_id, finished.receipt());
            }
            if !cancellation.has_unconfirmed_external_work() {
                cancellation.confirm();
            }
        });
    if let Err(error) = spawned {
        let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
        jobs.remove(&record.job_id);
        let mut failed = record;
        failed.status = JobStatus::Failed;
        failed.finished = Some(chrono::Utc::now());
        failed.output = Some("No command started: worker creation failed.".into());
        save(&failed)?;
        return Err(error).context("could not start terminal job; no command was run");
    }
    Ok(record)
}

pub fn cancel(session: &str, agent: &str, id: &str) -> Result<JobRecord> {
    status(session, agent, id)?;
    let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
    let Some(job) = jobs.get_mut(id) else {
        drop(jobs);
        return status(session, agent, id);
    };
    anyhow::ensure!(
        job.record.session_id == session && job.record.agent == agent,
        "terminal job owner changed"
    );
    job.cancellation.request();
    job.record.status = if job.record.finished.is_some() {
        JobStatus::Cancelled
    } else { JobStatus::Cancelling };
    save(&job.record)?;
    Ok(job.record.clone())
}

pub fn cancel_session(session: &str, actor: Option<&str>) -> usize {
    let mut jobs = active().lock().unwrap_or_else(|p| p.into_inner());
    let mut count = 0;
    for job in jobs.values_mut().filter(|job| {
        job.record.session_id == session
            && actor
                .is_none_or(|actor| crate::runtime::postbox::base_agent(&job.record.agent) == actor)
    }) {
        job.cancellation.request();
        job.record.status = if job.record.finished.is_some() {
            JobStatus::Cancelled
        } else { JobStatus::Cancelling };
        if let Err(error) = save(&job.record) {
            tracing::error!("terminal cancellation record: {error:#}");
        }
        count += 1;
    }
    drop(jobs);
    match crate::runtime::postbox::suppress_terminal_returns(session, actor) {
        Ok(suppressed) => count += suppressed,
        Err(error) => tracing::error!("could not suppress terminal continuation: {error:#}"),
    }
    count
}

pub(crate) fn mark_delivered(session: &str, id: &str) -> Result<()> {
    let path = record_path(id)?;
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&path, MAX_RECORD_BYTES)?
    else {
        anyhow::bail!("terminal completion has no managed job record");
    };
    let mut record: JobRecord = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        record.job_id == id && record.session_id == session,
        "terminal result belongs to another conversation"
    );
    record.delivered = true;
    save(&record)
}

pub fn cancel_all_and_wait(bound: Duration) -> usize {
    {
        let jobs = active().lock().unwrap_or_else(|p| p.into_inner());
        for job in jobs.values() {
            job.cancellation.request();
        }
    }
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if active()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
        {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    active().lock().unwrap_or_else(|p| p.into_inner()).len()
}

/// A daemon restart never re-executes a command with uncertain side effects.
/// Republish finished receipts using the same durable delivery id, or mark an
/// interrupted command honestly and let the owner inspect its effects.
pub fn recover() -> Result<usize> {
    let root = crate::config::phoenix_home().join("terminal-jobs");
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(root)?.take(4096) {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let bytes = crate::config::private_io::read_private_file_limited(&path, MAX_RECORD_BYTES)?
            .context("terminal record disappeared")?;
        let mut record: JobRecord = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            record_path(&record.job_id)? == path,
            "terminal record id/path mismatch"
        );
        if record.status == JobStatus::Cancelling {
            record.status = JobStatus::Cancelled;
            record.finished = Some(chrono::Utc::now());
            record.output=Some("The gateway stopped while user cancellation was pending. No continuation or command restart was requested. Verify partial effects before repeating it.".into());
            save(&record)?;
        }
        if record.status == JobStatus::Running {
            record.status = JobStatus::Interrupted;
            record.finished = Some(chrono::Utc::now());
            record.output=Some("The gateway stopped before it recorded command completion. The command was not restarted. Verify partial side effects before repeating it.".into());
            save(&record)?;
        }
        if !record.delivered
            && matches!(record.status, JobStatus::Succeeded | JobStatus::Failed)
            && record.finished.is_some() {
            crate::runtime::postbox::job_finished(&record.session_id, record.receipt());
            count += 1;
        }
    }
    Ok(count)
}

/// Validate immutable completion identity before durable postbox admission.
/// A suppressed/delivered record is never permission for another owner turn.
pub(crate) fn receipt_pending(session: &str, receipt: &CompletedJob) -> Result<bool> {
    let record = status(session, &receipt.agent, &receipt.delivery_id)?;
    if record.delivered
        || matches!(
            record.status,
            JobStatus::Running | JobStatus::Cancelling | JobStatus::Cancelled
                | JobStatus::Unconfirmed | JobStatus::Interrupted
        )
    {
        return Ok(false);
    }
    anyhow::ensure!(
        record.finished.is_some(),
        "terminal receipt has no actual completion"
    );
    let expected = record.receipt();
    anyhow::ensure!(
        expected.subject == receipt.subject
            && expected.ok == receipt.ok
            && expected.summary == receipt.summary
            && expected.body == receipt.body
            && expected.finished == receipt.finished,
        "terminal completion payload does not match its managed command result"
    );
    Ok(true)
}

pub(crate) fn completed_for_session(
    session: &str,
    actor: Option<&str>,
) -> Result<Vec<CompletedJob>> {
    let root = crate::config::phoenix_home().join("terminal-jobs");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut receipts = Vec::new();
    for entry in std::fs::read_dir(root)?.take(4096) {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(bytes) =
            crate::config::private_io::read_private_file_limited(&path, MAX_RECORD_BYTES)?
        else {
            continue;
        };
        let record: JobRecord = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            record_path(&record.job_id)? == path,
            "terminal record id/path mismatch"
        );
        if record.session_id == session
            && actor.is_none_or(|actor| crate::runtime::postbox::base_agent(&record.agent) == actor)
            && !record.delivered
            && record.finished.is_some()
        {
            receipts.push(record.receipt());
        }
    }
    Ok(receipts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::postbox::{self, WakeRequest};

    struct Fixture {
        session: String,
        root: tempfile::TempDir,
        _home: crate::config::test_env::PhoenixHomeGuard,
        runtime: tokio::runtime::Runtime,
        wakes: tokio::sync::mpsc::UnboundedReceiver<WakeRequest>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (tx, wakes) = tokio::sync::mpsc::unbounded_channel();
            postbox::set_wake_notifier(tx);
            Self { session: format!("terminal-check-{}", uuid::Uuid::new_v4().simple()),
                root, _home: home, runtime, wakes }
        }

        fn shell(&self, command: &str) -> JobRecord {
            let _entered = self.runtime.enter();
            let root = self.root.path().to_path_buf();
            let command = command.to_string();
            start(&self.session, "phoenix", "terminal check", move |token| {
                super::super::bash::execute_cancellable(&root, false,
                    super::super::bash::BashInput { command, cwd: None,
                        timeout_secs: Some(5), runner: None }, &token)
            }).unwrap()
        }

        fn wait_status(&self, id: &str, expected: JobStatus) -> JobRecord {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let record = status(&self.session, "phoenix", id).unwrap();
                if record.status == expected { return record; }
                assert!(Instant::now() < until, "expected {expected:?}, got {:?}", record.status);
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn wake(&mut self) -> WakeRequest {
            self.runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), self.wakes.recv()).await.unwrap().unwrap()
            })
        }

        fn no_wake(&mut self) {
            assert!(self.runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(120), self.wakes.recv()).await
            }).is_err());
        }
    }

    #[test]
    fn actual_command_exit_publishes_once_only_after_completion() {
        let mut f = Fixture::new();
        let started = Instant::now();
        let job = f.shell("while [ ! -f release ]; do sleep 0.02; done; printf actual-completion");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(job.status, JobStatus::Running);
        // The worker boundary is idle after returning its start receipt.
        // Full daemon/provider resumption is a separate acceptance check.
        assert!(!postbox::has_terminal_ready(&f.session));
        f.no_wake();
        std::fs::write(f.root.path().join("release"), b"go").unwrap();
        assert_eq!(f.wake(), WakeRequest::TerminalCompletion(f.session.clone()));
        assert!(postbox::has_terminal_ready(&f.session));
        let ready = postbox::take_ready(&f.session);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].delivery_id, job.job_id);
        assert!(ready[0].body.contains("actual-completion"));
        assert_eq!(ready[0].kind, ReturnKind::Terminal);
        // Transcript persistence precedes acknowledgement in the turn loop.
        let transcript = f.root.path().join("owner-result.txt");
        std::fs::write(&transcript, &ready[0].body).unwrap();
        postbox::acknowledge_ready(&f.session, &ready).unwrap();
        postbox::job_finished(&f.session, ready[0].clone());
        assert_eq!(recover().unwrap(), 0);
        assert!(!postbox::has_terminal_ready(&f.session));
        f.no_wake();
    }

    #[test]
    fn actual_failed_command_resumes_with_failure_evidence() {
        let mut f = Fixture::new();
        let job = f.shell("printf failure-evidence >&2; exit 7");
        assert_eq!(f.wake(), WakeRequest::TerminalCompletion(f.session.clone()));
        let result = f.wait_status(&job.job_id, JobStatus::Failed);
        assert!(result.output.unwrap().contains("failure-evidence"));
        let ready = postbox::take_ready(&f.session);
        assert_eq!(ready.len(), 1);
        assert!(!ready[0].ok);
        postbox::acknowledge_ready(&f.session, &ready).unwrap();
        f.no_wake();
    }

    #[test]
    fn completed_result_write_failure_retries_without_reexecuting_command() {
        let mut f = Fixture::new();
        let job = f.shell("printf once >> executions; while [ ! -f release ]; do sleep 0.02; done; printf retained-result");
        let path = record_path(&job.job_id).unwrap();
        let backup = f.root.path().join("running-record");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(f.root.path().join("release"), b"go").unwrap();
        f.wait_status(&job.job_id, JobStatus::Succeeded);
        f.no_wake();
        assert_eq!(std::fs::read_to_string(f.root.path().join("executions")).unwrap(), "once");
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&backup, &path).unwrap();
        assert_eq!(f.wake(), WakeRequest::TerminalCompletion(f.session.clone()));
        let ready = postbox::take_ready(&f.session);
        assert_eq!(ready.len(), 1);
        assert!(ready[0].body.contains("retained-result"));
        postbox::acknowledge_ready(&f.session, &ready).unwrap();
        assert_eq!(std::fs::read_to_string(f.root.path().join("executions")).unwrap(), "once");
        f.no_wake();
    }

    #[test]
    fn stop_terminates_real_process_group_without_owner_continuation() {
        let mut f = Fixture::new();
        let job = f.shell("sleep 30 & child=$!; printf '%s' \"$child\" > child-pid; wait");
        let marker = f.root.path().join("child-pid");
        let until = Instant::now() + Duration::from_secs(2);
        while !marker.exists() { assert!(Instant::now() < until); std::thread::sleep(Duration::from_millis(10)); }
        let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        cancel(&f.session, "phoenix", &job.job_id).unwrap();
        f.wait_status(&job.job_id, JobStatus::Cancelled);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        assert!(!postbox::has_terminal_ready(&f.session));
        assert_eq!(recover().unwrap(), 0);
        f.no_wake();
    }

    #[test]
    fn stop_suppresses_a_completion_already_queued_to_wake() {
        let mut f = Fixture::new();
        let job = f.shell("printf completed-before-stop");
        assert_eq!(f.wake(), WakeRequest::TerminalCompletion(f.session.clone()));
        assert!(postbox::has_terminal_ready(&f.session));
        cancel_session(&f.session, Some("phoenix"));
        assert!(status(&f.session, "phoenix", &job.job_id).unwrap().delivered);
        assert!(!postbox::has_terminal_ready(&f.session));
        assert!(postbox::take_ready(&f.session).is_empty());
        assert_eq!(recover().unwrap(), 0);
        f.no_wake();
    }

    #[test]
    fn delivered_receipt_left_in_ready_queue_is_not_wakeable() {
        let mut f = Fixture::new();
        let job = f.shell("printf durable-delivery");
        f.wake();
        assert!(postbox::has_terminal_ready(&f.session));
        // A crash can occur after the transcript and job-file acknowledgement
        // commit, before the queue's database acknowledgement commits.
        mark_delivered(&f.session, &job.job_id).unwrap();
        assert!(!postbox::has_terminal_ready(&f.session));
        assert_eq!(recover().unwrap(), 0);
        f.no_wake();
    }

    #[test]
    fn interrupted_record_is_inspectable_but_never_restarted_or_woken() {
        let mut f = Fixture::new();
        let record = JobRecord { job_id:format!("terminal_{}",uuid::Uuid::new_v4().simple()),
            session_id:f.session.clone(), agent:"phoenix".into(), label:"interrupted".into(),
            status:JobStatus::Running, started:chrono::Utc::now(), finished:None,
            output:None, delivered:false };
        save(&record).unwrap();
        assert_eq!(recover().unwrap(), 0);
        let result = status(&f.session,"phoenix",&record.job_id).unwrap();
        assert_eq!(result.status, JobStatus::Interrupted);
        assert!(result.output.unwrap().contains("not restarted"));
        f.no_wake();
    }

    #[test]
    fn another_owner_cannot_inspect_cancel_or_forge_completion() {
        let mut f = Fixture::new();
        let job = f.shell("printf immutable-evidence");
        f.wake();
        assert!(status("another-session", "phoenix", &job.job_id).is_err());
        assert!(cancel(&f.session, "another-agent", &job.job_id).is_err());
        let ready = postbox::take_ready(&f.session);
        let mut forged = ready[0].clone();
        forged.body.push_str("forged");
        assert!(receipt_pending(&f.session, &forged).is_err());
        postbox::acknowledge_ready(&f.session, &ready).unwrap();
        assert!(!receipt_pending(&f.session, &ready[0]).unwrap());
    }

    #[test]
    fn unconfirmed_handler_return_never_claims_a_completed_command_wake() {
        let mut f = Fixture::new();
        let job = {
            let _entered = f.runtime.enter();
            start(&f.session, "phoenix", "unconfirmed", |token| {
                token.mark_unconfirmed();
                anyhow::bail!("cannot prove owned external work stopped")
            }).unwrap()
        };
        f.wait_status(&job.job_id, JobStatus::Unconfirmed);
        assert!(!postbox::has_terminal_ready(&f.session));
        assert_eq!(recover().unwrap(), 0);
        f.no_wake();
    }

    #[test]
    fn start_without_live_gateway_never_executes_command() {
        let f = Fixture::new();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        postbox::set_wake_notifier(tx);
        let _entered = f.runtime.enter();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = started.clone();
        assert!(start(&f.session, "phoenix", "no gateway", move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolOutput { summary: String::new(), content: String::new() })
        }).is_err());
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
    }
}
