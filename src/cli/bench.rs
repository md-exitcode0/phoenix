//! `phoenix bench` — the fixed-task scoreboard.
//!
//! Fires a small, repeatable suite of tasks through the RUNNING gateway and
//! records per task: success, tokens, provider calls, tool calls, compression
//! savings, wall seconds. Results append to `bench/RESULTS.md` under a label,
//! so two runs (e.g. a feature on vs off, or two builds) sit side by side in
//! one file.
//!
//! This is more than marketing: it is the ground-truth oracle the RSI design
//! (plans/rsi-design.md) gates self-modifications on — a change that regresses
//! this suite does not ship. Keep tasks STABLE; comparable history is the
//! whole value.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::daemon::{self, RemoteOutcome, TurnSummary};
use crate::runtime::CliEvent;

const MAX_CASES_YAML_BYTES: u64 = 1024 * 1024;
const MAX_EXTRA_CASES: usize = 128;
const MAX_CASE_NAME_BYTES: usize = 128;
const MAX_CASE_PROMPT_BYTES: usize = 16 * 1024;
const MAX_CASE_NEEDLES: usize = 32;
const MAX_CASE_NEEDLE_BYTES: usize = 1024;
const MAX_CHECKED_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESULTS_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_BUILTIN_CASES: usize = 7;
/// Once a task has timed out, give cancellation and the daemon's integration
/// wake a bounded window to quiesce before the benchmark considers running the
/// next case. If this window expires, the caller stops the suite instead of
/// letting a late background tool mutate the next case's workspace.
const TIMEOUT_CLEANUP_PROBE: Duration = Duration::from_secs(5);
const TIMEOUT_CLEANUP_SETTLEMENT: Duration = Duration::from_secs(60);

/// How a task's final answer is judged, beyond "the turn completed".
enum Check {
    /// final_markdown contains ANY of these needles (case-insensitive).
    ContainsAny(&'static [&'static str]),
    /// A file must exist under the workspace containing the needle.
    FileContains {
        path: &'static str,
        needle: &'static str,
    },
    /// Any non-empty final answer passes.
    NonEmpty,
    /// The answer must be exactly the number of `.rs` files below
    /// `phoenix_agent/src` in the benchmark workspace.
    RepoRustFileCount,
}

struct BenchTask {
    name: &'static str,
    prompt: &'static str,
    check: Check,
    /// Touches the live desktop — only runs with --desktop.
    desktop: bool,
    /// Needs live web access — skipped with --no-web.
    web: bool,
}

/// The suite. STABILITY MATTERS: edits here invalidate historical comparisons,
/// so add tasks rather than changing existing ones when possible.
const SUITE: &[BenchTask] = &[
    BenchTask {
        name: "phatic",
        prompt: "hi",
        check: Check::NonEmpty,
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "repo-qa",
        prompt: "In one short paragraph: what does phoenix_agent/src/tools/compress.rs do?",
        check: Check::ContainsAny(&["compress", "prune", "tool output"]),
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "code-find",
        prompt: "Which function formats tool output before it reaches model context? Reply with the function name and its file path.",
        check: Check::ContainsAny(&["format_tool_output"]),
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "file-op",
        prompt: "Create a file bench_scratch/bench_note.md containing exactly the line: phoenix bench ok",
        check: Check::FileContains {
            path: "bench_scratch/bench_note.md",
            needle: "phoenix bench ok",
        },
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "repo-count",
        prompt: "How many .rs files are under phoenix_agent/src? Reply with just the number.",
        check: Check::RepoRustFileCount,
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "summarize",
        prompt: "Summarize CURSOR.md in five bullet points.",
        check: Check::ContainsAny(&["phoenix"]),
        desktop: false,
        web: false,
    },
    BenchTask {
        name: "web-fact",
        prompt: "What is the latest stable Rust version right now? One line, with the source URL.",
        check: Check::ContainsAny(&["1."]),
        desktop: false,
        web: true,
    },
    BenchTask {
        name: "desktop-look",
        prompt: "Take a screenshot and tell me which application window is focused right now.",
        check: Check::NonEmpty,
        desktop: true,
        web: false,
    },
];

/// Owned task — static suite entries and YAML-defined cases unify here.
struct OwnedTask {
    name: String,
    prompt: String,
    check: OwnedCheck,
    desktop: bool,
    web: bool,
}

enum OwnedCheck {
    ContainsAny(Vec<String>),
    FileContains { path: String, needle: String },
    NonEmpty,
    RepoRustFileCount,
}

impl OwnedTask {
    fn from_static(task: &BenchTask) -> Self {
        Self {
            name: task.name.to_string(),
            prompt: task.prompt.to_string(),
            check: match &task.check {
                Check::ContainsAny(needles) => {
                    OwnedCheck::ContainsAny(needles.iter().map(|n| n.to_string()).collect())
                }
                Check::FileContains { path, needle } => OwnedCheck::FileContains {
                    path: path.to_string(),
                    needle: needle.to_string(),
                },
                Check::NonEmpty => OwnedCheck::NonEmpty,
                Check::RepoRustFileCount => OwnedCheck::RepoRustFileCount,
            },
            desktop: task.desktop,
            web: task.web,
        }
    }
}

/// Declarative extra cases (donor: promptfoo's YAML eval pattern) —
/// `bench/cases.yaml` in the workspace. Grow the oracle without recompiling:
/// ```yaml
/// - name: my-case
///   prompt: "do the thing"
///   contains_any: ["expected", "phrases"]   # or file_contains / non_empty
///   web: false
///   desktop: false
/// ```
#[derive(serde::Deserialize)]
struct YamlCase {
    name: String,
    prompt: String,
    #[serde(default)]
    contains_any: Option<Vec<String>>,
    #[serde(default)]
    file_contains: Option<YamlFileCheck>,
    #[serde(default)]
    desktop: bool,
    #[serde(default)]
    web: bool,
}

#[derive(serde::Deserialize)]
struct YamlFileCheck {
    path: String,
    needle: String,
}

fn yaml_cases(workspace: &std::path::Path) -> Result<Vec<OwnedTask>> {
    let path = workspace.join("bench/cases.yaml");
    let Some(bytes) = read_regular_workspace_file(
        workspace,
        Path::new("bench/cases.yaml"),
        MAX_CASES_YAML_BYTES,
    )?
    else {
        return Ok(Vec::new());
    };
    let raw =
        String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))?;
    let parsed: Vec<YamlCase> = serde_yaml::from_str(&raw)
        .with_context(|| format!("{} is invalid YAML", path.display()))?;
    if parsed.len() > MAX_EXTRA_CASES {
        bail!("bench/cases.yaml has more than {MAX_EXTRA_CASES} cases");
    }
    parsed
        .into_iter()
        .map(|case| -> Result<OwnedTask> {
            validate_yaml_case(&case)?;
            Ok(OwnedTask {
                name: case.name,
                prompt: case.prompt,
                check: match (case.contains_any, case.file_contains) {
                    (Some(needles), _) => OwnedCheck::ContainsAny(needles),
                    (None, Some(fc)) => OwnedCheck::FileContains {
                        path: fc.path,
                        needle: fc.needle,
                    },
                    (None, None) => OwnedCheck::NonEmpty,
                },
                desktop: case.desktop,
                web: case.web,
            })
        })
        .collect()
}

fn validate_yaml_case(case: &YamlCase) -> Result<()> {
    if case.name.trim().is_empty()
        || case.name.len() > MAX_CASE_NAME_BYTES
        || case
            .name
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '|'))
    {
        bail!("bench case has an invalid name");
    }
    if case.prompt.trim().is_empty() || case.prompt.len() > MAX_CASE_PROMPT_BYTES {
        bail!("bench case {} has an invalid prompt size", case.name);
    }
    if case.contains_any.is_some() && case.file_contains.is_some() {
        bail!(
            "bench case {} must choose contains_any or file_contains, not both",
            case.name
        );
    }
    if let Some(needles) = &case.contains_any {
        if needles.is_empty() || needles.len() > MAX_CASE_NEEDLES {
            bail!("bench case {} has an invalid contains_any list", case.name);
        }
        if needles
            .iter()
            .any(|needle| needle.is_empty() || needle.len() > MAX_CASE_NEEDLE_BYTES)
        {
            bail!("bench case {} has an invalid contains_any value", case.name);
        }
    }
    if let Some(check) = &case.file_contains {
        validate_workspace_relative(Path::new(&check.path))?;
        if check.needle.is_empty() || check.needle.len() > MAX_CASE_NEEDLE_BYTES {
            bail!("bench case {} has an invalid file needle", case.name);
        }
    }
    Ok(())
}

fn validate_workspace_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.as_os_str().len() > 4_096
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe workspace-relative path {}", path.display());
    }
    Ok(())
}

fn read_regular_workspace_file(
    workspace: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>> {
    validate_workspace_relative(relative)?;
    let mut current = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("unsafe workspace path");
        };
        current.push(name);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            bail!("refusing symlinked workspace path {}", current.display());
        }
    }
    let metadata = std::fs::symlink_metadata(&current)?;
    if !metadata.is_file() {
        bail!(
            "workspace path is not a regular file: {}",
            current.display()
        );
    }
    if metadata.len() > max_bytes {
        bail!(
            "workspace file {} is too large ({} bytes; max {max_bytes})",
            current.display(),
            metadata.len()
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&current)?;
    if !file.metadata()?.is_file() {
        bail!("workspace file changed to a non-regular file");
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("workspace file grew beyond its size limit while reading");
    }
    Ok(Some(bytes))
}

/// Count regular Rust source files below the repository's source root without
/// following symlinks. The benchmark asks for a number, so judging against the
/// workspace's actual count makes a stray digit or a stale hard-coded oracle a
/// failure instead of a false pass.
fn count_rust_files(root: &Path) -> Result<usize> {
    let metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("reading Rust source root {}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Rust source root is not a regular directory: {}",
            root.display()
        );
    }

    fn visit(directory: &Path, count: &mut usize) -> Result<()> {
        for entry in std::fs::read_dir(directory)
            .with_context(|| format!("reading Rust source directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                visit(&path, count)?;
            } else if metadata.is_file()
                && path.extension().is_some_and(|extension| extension == "rs")
            {
                *count = count
                    .checked_add(1)
                    .context("Rust source file count overflowed")?;
            }
        }
        Ok(())
    }

    let mut count = 0;
    visit(root, &mut count)?;
    Ok(count)
}

struct TaskResult {
    name: String,
    success: bool,
    /// False only when a timed-out request could not be cancelled and settled;
    /// the suite must stop before launching another workspace-mutating case.
    cleanup_complete: bool,
    total_tokens: u32,
    provider_calls: u32,
    tool_calls: u32,
    failed_tool_calls: u32,
    duplicate_tool_calls: u32,
    compression_saved_tokens: u64,
    first_tool_seconds: Option<f32>,
    seconds: f32,
    note: String,
}

/// Per-task ceiling — a wedged task must not stall the whole suite. The timeout
/// path explicitly cancels the gateway turn and settles any detached work.
const TASK_TIMEOUT: Duration = Duration::from_secs(900);

fn task_is_selected(task: &OwnedTask, desktop: bool, no_web: bool) -> bool {
    (!task.desktop || desktop) && (!task.web || !no_web)
}

/// The release gate's no-flag/no-extra-case invocation is intentionally a
/// seven-case contract. Desktop and web-filtered/custom runs remain flexible;
/// only the canonical default is rejected if its built-in shape drifts.
fn validate_canonical_default_suite(
    tasks: &[OwnedTask],
    desktop: bool,
    no_web: bool,
    has_extra_cases: bool,
) -> Result<()> {
    if desktop || no_web || has_extra_cases {
        return Ok(());
    }
    let selected = tasks
        .iter()
        .filter(|task| task_is_selected(task, desktop, no_web))
        .count();
    if selected != DEFAULT_BUILTIN_CASES {
        bail!(
            "canonical default benchmark must select exactly {DEFAULT_BUILTIN_CASES} built-in cases, found {selected}"
        );
    }
    Ok(())
}

fn validate_canonical_default_results(
    result_count: usize,
    desktop: bool,
    no_web: bool,
    has_extra_cases: bool,
) -> Result<()> {
    if desktop || no_web || has_extra_cases {
        return Ok(());
    }
    if result_count != DEFAULT_BUILTIN_CASES {
        bail!(
            "canonical default benchmark must execute exactly {DEFAULT_BUILTIN_CASES} built-in cases, ran {result_count}"
        );
    }
    Ok(())
}

fn timeout_settlement_summary(session_id: &str) -> TurnSummary {
    TurnSummary {
        completion: crate::cli::daemon::TurnCompletion::Unknown,
        final_markdown: String::new(),
        main_session_id: session_id.to_string(),
        run_id: "benchmark-timeout-cleanup".to_string(),
        trace_path: String::new(),
        route: "benchmark-timeout-cleanup".to_string(),
        total_tokens: 0,
        orchestrator_tokens: None,
        coder_tokens: None,
        compression_saved_tokens: 0,
        compression_raw_tokens: 0,
        context_window: None,
        background_work_pending: true,
    }
}

/// Stop a timed-out foreground request, then wait for any detached work from
/// that request to be absorbed before the benchmark starts another case. The
/// fresh subscription replays running/ready jobs, which covers the race where
/// the original turn's event stream was dropped by the timeout.
async fn cancel_and_settle_timed_out(session_id: &str) -> Result<()> {
    daemon::cancel_turn(session_id.to_string())
        .await
        .context("cancelling timed-out benchmark turn")?;

    let mut events = daemon::subscribe_events(session_id.to_string())
        .await
        .context("subscribing to timed-out benchmark cleanup")?;
    let deadline = Instant::now() + TIMEOUT_CLEANUP_PROBE;
    let mut buffered = Vec::new();
    let mut saw_background = false;

    while !saw_background {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let event = match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(_) => break,
        };
        saw_background = matches!(
            &event,
            CliEvent::BackgroundAgentSpawned { .. }
                | CliEvent::BackgroundAgentReturned { .. }
                | CliEvent::BackgroundResultsAbsorbed { .. }
        );
        buffered.push(event);
        if buffered.len() >= 1_024 {
            bail!("timed-out benchmark cleanup produced too many lifecycle events");
        }
    }

    // The cancellation ack stops the foreground turn. With no replayed
    // background lifecycle event there is no detached work to settle, so the
    // next case is safe to start after the short probe window.
    if !saw_background {
        return Ok(());
    }

    let (tx, rx) = tokio::sync::mpsc::channel(2_048);
    for event in buffered {
        tx.send(event)
            .await
            .context("buffering timed-out benchmark lifecycle event")?;
    }
    let forwarder = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });

    let (mut settled_events, settlement) = daemon::spawn_background_settlement(
        rx,
        timeout_settlement_summary(session_id),
        TIMEOUT_CLEANUP_SETTLEMENT,
    );
    while settled_events.recv().await.is_some() {}
    let outcome = settlement.await.unwrap_or(RemoteOutcome::Lost);
    let _ = forwarder.await;
    match outcome {
        RemoteOutcome::Summary(_) => Ok(()),
        RemoteOutcome::Error(message) => {
            bail!("timed-out benchmark background cleanup failed: {message}")
        }
        RemoteOutcome::Lost => bail!("gateway lost the timed-out benchmark cleanup stream"),
    }
}

pub async fn run_bench(label: Option<String>, desktop: bool, no_web: bool) -> Result<()> {
    if !daemon::socket_path().exists() {
        bail!("no running gateway — start `phoenix` in another terminal first");
    }
    let label = label.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
    if label.trim().is_empty()
        || label.len() > 256
        || label
            .chars()
            .any(|character| matches!(character, '\n' | '\r'))
    {
        bail!("bench label must be 1..=256 bytes on one line");
    }
    let workspace = std::env::current_dir().context("cwd")?;
    let scratch = workspace.join("bench_scratch");
    match std::fs::symlink_metadata(&scratch) {
        Ok(_) => bail!(
            "reserved benchmark scratch path already exists: {}; move it before running bench",
            scratch.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspecting benchmark scratch path"),
    }

    let mut tasks: Vec<OwnedTask> = SUITE.iter().map(OwnedTask::from_static).collect();
    let extra = yaml_cases(&workspace)?;
    let has_extra_cases = !extra.is_empty();
    if has_extra_cases {
        println!("(+{} cases from bench/cases.yaml)", extra.len());
        tasks.extend(extra);
    }
    validate_canonical_default_suite(&tasks, desktop, no_web, has_extra_cases)?;
    let mut results: Vec<TaskResult> = Vec::new();
    for task in &tasks {
        if !task_is_selected(task, desktop, no_web) {
            continue;
        }
        println!("▶ {} — {}", task.name, task.prompt);
        let result = run_task(task, &workspace).await;
        println!(
            "  {} · {} tokens · {} provider calls · {} tools ({} failed, {} duplicate) · first tool {} · {:.1}s{}",
            if result.success { "ok" } else { "FAIL" },
            result.total_tokens,
            result.provider_calls,
            result.tool_calls,
            result.failed_tool_calls,
            result.duplicate_tool_calls,
            format_optional_seconds(result.first_tool_seconds),
            result.seconds,
            if result.note.is_empty() {
                String::new()
            } else {
                format!(" · {}", result.note)
            }
        );
        results.push(result);
        if !results.last().is_some_and(|result| result.cleanup_complete) {
            eprintln!(
                "benchmark stopping: timed-out task cleanup did not settle; no later case will run"
            );
            break;
        }
    }

    validate_canonical_default_results(results.len(), desktop, no_web, has_extra_cases)?;

    // bench_scratch is the file-op task's target — clean it after judging.
    cleanup_scratch(&scratch)?;

    let report = render_report(&label, &results);
    let out_dir = workspace.join("bench");
    ensure_bench_directory(&out_dir)?;
    let out_path = out_dir.join("RESULTS.md");
    append_bench_report(&workspace, &out_path, &report)?;

    println!("\n{report}");
    println!("appended to {}", out_path.display());
    fail_if_any_tasks_failed(&results)
}

fn ensure_bench_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("refusing unsafe bench directory {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(path)?;
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        bail!("unsafe bench directory appeared at {}", path.display());
                    }
                    Ok(())
                }
                Err(error) => Err(error).context("creating bench/"),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn cleanup_scratch(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).context("cleaning benchmark scratch directory")
        }
        Ok(_) => std::fs::remove_file(path).context("cleaning benchmark scratch path"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn append_bench_report(workspace: &Path, out_path: &Path, report: &str) -> Result<()> {
    let lock_path = out_path.with_extension("md.lock");
    let mut lock_options = std::fs::OpenOptions::new();
    lock_options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options
            .mode(0o644)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let lock = lock_options
        .open(&lock_path)
        .with_context(|| format!("opening bench result lock {}", lock_path.display()))?;
    if !lock.metadata()?.is_file() {
        bail!("bench result lock is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
            return Err(std::io::Error::last_os_error())
                .context("another benchmark is writing RESULTS.md");
        }
    }

    let relative = out_path
        .strip_prefix(workspace)
        .context("bench results path escaped the workspace")?;
    let mut existing = match read_regular_workspace_file(workspace, relative, MAX_RESULTS_BYTES)? {
        Some(bytes) => String::from_utf8(bytes).context("bench/RESULTS.md is not UTF-8")?,
        None => "# Phoenix Bench Results\n\nFixed task suite (`phoenix bench`). Each run appends under its label —\ncompare labels to see what a change actually bought.\n".to_string(),
    };
    existing.push('\n');
    existing.push_str(report);
    if existing.len() as u64 > MAX_RESULTS_BYTES {
        bail!("bench/RESULTS.md would exceed {MAX_RESULTS_BYTES} bytes");
    }

    let prior_permissions = std::fs::symlink_metadata(out_path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut staged: Option<PathBuf> = None;
    for _ in 0..8 {
        let candidate =
            out_path.with_file_name(format!(".RESULTS.md.tmp.{}", uuid::Uuid::new_v4().simple()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o644)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        match options.open(&candidate) {
            Ok(mut file) => {
                let result = (|| -> Result<()> {
                    file.write_all(existing.as_bytes())?;
                    if let Some(permissions) = prior_permissions.clone() {
                        file.set_permissions(permissions)?;
                    }
                    file.sync_all()?;
                    Ok(())
                })();
                if let Err(error) = result {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                drop(file);
                staged = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let staged = staged.context("could not allocate a bench result staging file")?;
    if let Err(error) = std::fs::rename(&staged, out_path) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).context("publishing bench/RESULTS.md");
    }
    if let Some(parent) = out_path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    drop(lock);
    Ok(())
}

async fn run_task(task: &OwnedTask, workspace: &std::path::Path) -> TaskResult {
    let session_id = format!("bench-{}", uuid::Uuid::new_v4());
    let request_session_id = session_id.clone();
    let started = Instant::now();
    let mut provider_calls: u32 = 0;
    let mut tool_calls: u32 = 0;
    let mut failed_tool_calls: u32 = 0;
    let mut duplicate_tool_calls: u32 = 0;
    let mut seen_tool_inputs = std::collections::BTreeSet::new();
    let mut first_tool_seconds: Option<f32> = None;

    let timed = tokio::time::timeout(TASK_TIMEOUT, async {
        // Attach before Turn submission. The benchmark must judge the answer
        // that integrates detached same-session work, not the provisional
        // foreground "still working" text.
        let background_events = match daemon::subscribe_events(request_session_id.clone()).await {
            Ok(events) => events,
            Err(error) => {
                return RemoteOutcome::Error(format!("background subscription failed: {error:#}"));
            }
        };
        let (mut events, handle) =
            match daemon::submit_turn(request_session_id.clone(), task.prompt.to_string(), false)
                .await
            {
                Ok(turn) => turn,
                Err(error) => {
                    return RemoteOutcome::Error(format!("submit failed: {error:#}"));
                }
            };
        while let Some(event) = events.recv().await {
            match event {
                CliEvent::Thinking => provider_calls = provider_calls.saturating_add(1),
                CliEvent::ToolCallStarted { .. } => {
                    first_tool_seconds.get_or_insert_with(|| started.elapsed().as_secs_f32());
                }
                CliEvent::ToolCallCompleted {
                    tool_name,
                    input_summary,
                    success,
                    ..
                } => {
                    tool_calls = tool_calls.saturating_add(1);
                    if !success {
                        failed_tool_calls = failed_tool_calls.saturating_add(1);
                    }
                    let tool_key = format!("{tool_name}\n{input_summary}");
                    if !seen_tool_inputs.insert(tool_key) {
                        duplicate_tool_calls = duplicate_tool_calls.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        let summary = match handle.await.unwrap_or(RemoteOutcome::Lost) {
            RemoteOutcome::Summary(summary) => summary,
            other => return other,
        };
        if !summary.background_work_pending {
            return RemoteOutcome::Summary(summary);
        }

        let remaining = TASK_TIMEOUT.saturating_sub(started.elapsed());
        let (mut settled_events, settlement) =
            daemon::spawn_background_settlement(background_events, summary, remaining);
        while let Some(event) = settled_events.recv().await {
            match event {
                // Integration-wake provider/tool work counts toward the task
                // whose final answer the benchmark judges.
                CliEvent::Thinking => provider_calls = provider_calls.saturating_add(1),
                CliEvent::ToolCallStarted { .. } => {
                    first_tool_seconds.get_or_insert_with(|| started.elapsed().as_secs_f32());
                }
                CliEvent::ToolCallCompleted {
                    tool_name,
                    input_summary,
                    success,
                    ..
                } => {
                    tool_calls = tool_calls.saturating_add(1);
                    if !success {
                        failed_tool_calls = failed_tool_calls.saturating_add(1);
                    }
                    let tool_key = format!("{tool_name}\n{input_summary}");
                    if !seen_tool_inputs.insert(tool_key) {
                        duplicate_tool_calls = duplicate_tool_calls.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        settlement.await.unwrap_or(RemoteOutcome::Lost)
    })
    .await;
    let mut cleanup_complete = true;
    let outcome = match timed {
        Ok(outcome) => outcome,
        Err(_) => match cancel_and_settle_timed_out(&session_id).await {
            Ok(()) => RemoteOutcome::Error(format!(
                "timed out after {}s; cancelled and settled before continuing",
                TASK_TIMEOUT.as_secs()
            )),
            Err(error) => {
                cleanup_complete = false;
                RemoteOutcome::Error(format!(
                    "timed out after {}s; cancellation/settlement failed: {error:#}",
                    TASK_TIMEOUT.as_secs()
                ))
            }
        },
    };

    let seconds = started.elapsed().as_secs_f32();
    match outcome {
        RemoteOutcome::Summary(summary) => {
            let (success, note) = judge(task, &summary, workspace);
            TaskResult {
                name: task.name.clone(),
                success,
                cleanup_complete,
                total_tokens: summary.total_tokens,
                provider_calls,
                tool_calls,
                failed_tool_calls,
                duplicate_tool_calls,
                compression_saved_tokens: summary.compression_saved_tokens,
                first_tool_seconds,
                seconds,
                note,
            }
        }
        RemoteOutcome::Error(message) => TaskResult {
            name: task.name.clone(),
            success: false,
            cleanup_complete,
            total_tokens: 0,
            provider_calls,
            tool_calls,
            failed_tool_calls,
            duplicate_tool_calls,
            compression_saved_tokens: 0,
            first_tool_seconds,
            seconds,
            note: first_line_of(&message),
        },
        RemoteOutcome::Lost => TaskResult {
            name: task.name.clone(),
            success: false,
            cleanup_complete,
            total_tokens: 0,
            provider_calls,
            tool_calls,
            failed_tool_calls,
            duplicate_tool_calls,
            compression_saved_tokens: 0,
            first_tool_seconds,
            seconds,
            note: "connection lost mid-turn".to_string(),
        },
    }
}

fn judge(task: &OwnedTask, summary: &TurnSummary, workspace: &std::path::Path) -> (bool, String) {
    let answer = summary.final_markdown.to_lowercase();
    match &task.check {
        OwnedCheck::NonEmpty => (!answer.trim().is_empty(), String::new()),
        OwnedCheck::ContainsAny(needles) => {
            let hit = needles.iter().any(|n| answer.contains(&n.to_lowercase()));
            (
                hit,
                if hit {
                    String::new()
                } else {
                    "answer missing expected content".to_string()
                },
            )
        }
        OwnedCheck::RepoRustFileCount => {
            let source_root = workspace.join("phoenix_agent/src");
            let expected = match count_rust_files(&source_root) {
                Ok(count) => count,
                Err(error) => {
                    return (
                        false,
                        format!(
                            "could not count Rust source files: {}",
                            first_line_of(&format!("{error:#}"))
                        ),
                    )
                }
            };
            let actual = summary.final_markdown.trim().parse::<usize>().ok();
            let hit = actual == Some(expected);
            (
                hit,
                if hit {
                    String::new()
                } else {
                    format!("answer must be exactly the Rust source count ({expected})")
                },
            )
        }
        OwnedCheck::FileContains { path, needle } => {
            match read_regular_workspace_file(workspace, Path::new(path), MAX_CHECKED_FILE_BYTES) {
                Ok(Some(bytes)) => match String::from_utf8(bytes) {
                    Ok(content) if content.contains(needle) => (true, String::new()),
                    Ok(_) => (false, "file exists but content wrong".to_string()),
                    Err(_) => (false, "file exists but is not UTF-8".to_string()),
                },
                Ok(None) => (false, "file was not created".to_string()),
                Err(error) => (
                    false,
                    format!(
                        "file check was unsafe or unreadable: {}",
                        first_line_of(&format!("{error:#}"))
                    ),
                ),
            }
        }
    }
}

fn render_report(label: &str, results: &[TaskResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!("## {label}\n\n"));
    out.push_str(
        "| task | ok | tokens | provider calls | tool calls | failed tools | duplicate tools | first tool | comp saved | seconds |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    let mut tokens: u64 = 0;
    let mut provider_calls: u32 = 0;
    let mut tool_calls: u32 = 0;
    let mut failed_tool_calls: u32 = 0;
    let mut duplicate_tool_calls: u32 = 0;
    let mut saved: u64 = 0;
    let mut passed: usize = 0;
    for r in results {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} |\n",
            r.name,
            if r.success { "✓" } else { "✗" },
            r.total_tokens,
            r.provider_calls,
            r.tool_calls,
            r.failed_tool_calls,
            r.duplicate_tool_calls,
            format_optional_seconds(r.first_tool_seconds),
            r.compression_saved_tokens,
            r.seconds,
        ));
        tokens = tokens.saturating_add(r.total_tokens as u64);
        provider_calls = provider_calls.saturating_add(r.provider_calls);
        tool_calls = tool_calls.saturating_add(r.tool_calls);
        failed_tool_calls = failed_tool_calls.saturating_add(r.failed_tool_calls);
        duplicate_tool_calls = duplicate_tool_calls.saturating_add(r.duplicate_tool_calls);
        saved = saved.saturating_add(r.compression_saved_tokens);
        if r.success {
            passed += 1;
        }
    }
    out.push_str(&format!(
        "| **total** | **{}/{}** | **{}** | **{}** | **{}** | **{}** | **{}** | | **{}** | |\n",
        passed,
        results.len(),
        tokens,
        provider_calls,
        tool_calls,
        failed_tool_calls,
        duplicate_tool_calls,
        saved,
    ));
    for r in results {
        if !r.note.is_empty() {
            out.push_str(&format!("- {}: {}\n", r.name, r.note));
        }
    }
    out
}

fn fail_if_any_tasks_failed(results: &[TaskResult]) -> Result<()> {
    let failed: Vec<&str> = results
        .iter()
        .filter(|result| !result.success)
        .map(|result| result.name.as_str())
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        bail!(
            "benchmark regression: {} of {} task(s) failed: {}",
            failed.len(),
            results.len(),
            failed.join(", ")
        );
    }
}

fn format_optional_seconds(value: Option<f32>) -> String {
    value
        .map(|seconds| format!("{seconds:.1}s"))
        .unwrap_or_else(|| "-".to_string())
}

fn first_line_of(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(markdown: &str) -> TurnSummary {
        TurnSummary {
            completion: crate::cli::daemon::TurnCompletion::Completed,
            final_markdown: markdown.to_string(),
            main_session_id: "s".into(),
            run_id: "r".into(),
            trace_path: String::new(),
            route: "direct".into(),
            total_tokens: 10,
            orchestrator_tokens: None,
            coder_tokens: None,
            compression_saved_tokens: 0,
            compression_raw_tokens: 0,
            context_window: None,
            background_work_pending: false,
        }
    }

    #[test]
    fn judge_contains_any_is_case_insensitive() {
        let task = OwnedTask::from_static(&SUITE[2]); // code-find expects format_tool_output
        let (ok, _) = judge(
            &task,
            &summary("It is `Format_Tool_Output` in tools/mod.rs"),
            std::path::Path::new("/tmp"),
        );
        assert!(ok);
        let (bad, note) = judge(&task, &summary("no idea"), std::path::Path::new("/tmp"));
        assert!(!bad);
        assert!(!note.is_empty());
    }

    #[test]
    fn repo_count_judge_requires_the_exact_workspace_count() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("phoenix_agent/src/nested");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(dir.path().join("phoenix_agent/src/one.rs"), "").unwrap();
        std::fs::write(source.join("two.rs"), "").unwrap();
        std::fs::write(source.join("not-rust.txt"), "").unwrap();

        let task = OwnedTask::from_static(&SUITE[4]);
        let (ok, _) = judge(&task, &summary("2"), dir.path());
        assert!(ok);
        let (wrong_digit, note) = judge(&task, &summary("9"), dir.path());
        assert!(!wrong_digit);
        assert!(note.contains("exactly"));
        let (explanation, _) = judge(&task, &summary("There are 2 files"), dir.path());
        assert!(!explanation);
    }

    #[test]
    fn canonical_default_suite_is_exactly_seven_without_filters_or_extras() {
        let tasks: Vec<OwnedTask> = SUITE.iter().map(OwnedTask::from_static).collect();
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task_is_selected(task, false, false))
                .count(),
            DEFAULT_BUILTIN_CASES
        );
        validate_canonical_default_suite(&tasks, false, false, false).unwrap();
        validate_canonical_default_results(DEFAULT_BUILTIN_CASES, false, false, false).unwrap();
        assert!(validate_canonical_default_results(6, false, false, false).is_err());

        // Legitimate filtered/desktop/custom runs are not forced into the
        // seven-case release shape.
        validate_canonical_default_suite(&tasks, true, false, false).unwrap();
        validate_canonical_default_suite(&tasks, false, true, false).unwrap();
        validate_canonical_default_suite(&tasks, false, false, true).unwrap();
    }

    #[test]
    fn judge_file_check_reads_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bench_scratch")).unwrap();
        std::fs::write(
            dir.path().join("bench_scratch/bench_note.md"),
            "phoenix bench ok\n",
        )
        .unwrap();
        let task = OwnedTask::from_static(&SUITE[3]);
        let (ok, _) = judge(&task, &summary("done"), dir.path());
        assert!(ok);
    }

    #[test]
    fn yaml_cases_parse_and_default_sanely() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bench")).unwrap();
        std::fs::write(
            dir.path().join("bench/cases.yaml"),
            "- name: greet\n  prompt: say hi\n- name: find\n  prompt: locate x\n  contains_any: [\"found\"]\n  web: true\n",
        )
        .unwrap();
        let cases = yaml_cases(dir.path()).unwrap();
        assert_eq!(cases.len(), 2);
        assert!(matches!(cases[0].check, OwnedCheck::NonEmpty));
        assert!(!cases[0].web);
        assert!(matches!(cases[1].check, OwnedCheck::ContainsAny(_)));
        assert!(cases[1].web);
        // Missing file and invalid YAML both degrade to empty, never panic.
        assert!(yaml_cases(std::path::Path::new("/nonexistent"))
            .unwrap()
            .is_empty());
        std::fs::write(dir.path().join("bench/cases.yaml"), ": not yaml [").unwrap();
        assert!(yaml_cases(dir.path()).is_err());
    }

    #[test]
    fn report_renders_totals_row() {
        let results = vec![TaskResult {
            name: "phatic".to_string(),
            success: true,
            cleanup_complete: true,
            total_tokens: 42,
            provider_calls: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
            duplicate_tool_calls: 0,
            compression_saved_tokens: 5,
            first_tool_seconds: None,
            seconds: 1.2,
            note: String::new(),
        }];
        let report = render_report("test-label", &results);
        assert!(report.contains("## test-label"));
        assert!(report.contains("| phatic | ✓ | 42 | 1 | 0 | 0 | 0 | - | 5 | 1.2 |"));
        assert!(report.contains("**1/1**"));
    }

    #[test]
    fn benchmark_exit_status_fails_when_any_task_failed() {
        let failing = vec![
            TaskResult {
                name: "phatic".to_string(),
                success: true,
                cleanup_complete: true,
                total_tokens: 42,
                provider_calls: 1,
                tool_calls: 0,
                failed_tool_calls: 0,
                duplicate_tool_calls: 0,
                compression_saved_tokens: 5,
                first_tool_seconds: None,
                seconds: 1.2,
                note: String::new(),
            },
            TaskResult {
                name: "web-fact".to_string(),
                success: false,
                cleanup_complete: true,
                total_tokens: 0,
                provider_calls: 2,
                tool_calls: 0,
                failed_tool_calls: 0,
                duplicate_tool_calls: 0,
                compression_saved_tokens: 0,
                first_tool_seconds: None,
                seconds: 7.2,
                note: "answer missing expected content".to_string(),
            },
        ];
        let error = fail_if_any_tasks_failed(&failing).unwrap_err();
        assert!(error.to_string().contains("1 of 2 task(s) failed"));
        assert!(error.to_string().contains("web-fact"));

        let passing = vec![TaskResult {
            name: "phatic".to_string(),
            success: true,
            cleanup_complete: true,
            total_tokens: 42,
            provider_calls: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
            duplicate_tool_calls: 0,
            compression_saved_tokens: 5,
            first_tool_seconds: None,
            seconds: 1.2,
            note: String::new(),
        }];
        fail_if_any_tasks_failed(&passing).unwrap();
    }

    #[test]
    fn yaml_cases_reject_workspace_escape_and_oversized_input() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bench")).unwrap();
        std::fs::write(
            dir.path().join("bench/cases.yaml"),
            "- name: escape\n  prompt: inspect\n  file_contains:\n    path: ../secret\n    needle: x\n",
        )
        .unwrap();
        assert!(yaml_cases(dir.path()).is_err());

        let file = std::fs::File::create(dir.path().join("bench/cases.yaml")).unwrap();
        file.set_len(MAX_CASES_YAML_BYTES + 1).unwrap();
        assert!(yaml_cases(dir.path()).is_err());
    }

    #[test]
    fn bench_results_append_atomically_and_preserve_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let bench = dir.path().join("bench");
        ensure_bench_directory(&bench).unwrap();
        let results = bench.join("RESULTS.md");
        std::fs::write(&results, "existing\n").unwrap();
        append_bench_report(dir.path(), &results, "## next\n").unwrap();
        let content = std::fs::read_to_string(&results).unwrap();
        assert!(content.starts_with("existing\n"));
        assert!(content.contains("## next"));
        assert!(std::fs::read_dir(&bench)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp.")));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_file_checks_do_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "phoenix bench ok").unwrap();
        std::fs::create_dir(dir.path().join("bench_scratch")).unwrap();
        symlink(&outside, dir.path().join("bench_scratch/bench_note.md")).unwrap();
        let task = OwnedTask::from_static(&SUITE[3]);
        let (ok, note) = judge(&task, &summary("done"), dir.path());
        assert!(!ok);
        assert!(note.contains("unsafe"));
    }
}
