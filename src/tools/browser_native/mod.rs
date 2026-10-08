//! Native Rust browser capability (donor: browser-use).
//!
//! Every Phoenix coworker can drive its own real Chromium over CDP entirely
//! in Rust — no Python sidecar. Each `browser_*` tool maps onto an action that
//! manipulates a persistent browser session and returns the fresh indexed
//! browser state, so the agent always reasons over the page as it now is.
//!
//! The capability is ported from browser-use: an indexed `[i]` interactive
//! element tree (clickable detection, capped text, tabs, URL) plus the action
//! verbs (navigate/click/input/scroll/extract/...). The DOM indexing runs as
//! injected JavaScript in the page rather than a CDP accessibility-snapshot
//! reimplementation; the agent loop that drives it is Phoenix's own runtime —
//! there is no second embedded LLM pilot.
//!
//! The session spawns lazily on the first browser tool call and persists for
//! the life of the Phoenix process (tabs, cookies, logins survive between
//! agent turns). A dead session is dropped and respawned on the next call.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use headless_chrome::browser::tab::point::Point;
use headless_chrome::browser::tab::ModifierKey;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::protocol::cdp::{Emulation, Input, Network};
use headless_chrome::types::Bounds;
use headless_chrome::{Browser, LaunchOptions, LaunchOptionsBuilder, Tab};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::tools::browser_cookies::is_supported_source;

use super::ToolOutput;

/// Phoenix tool names are `browser_` + the action name. This is the complete
/// browser surface; it mirrors the donor's action registry minus the embedded
/// pilot (`task`) — Phoenix's own loop is the pilot.
pub(crate) const BROWSER_ACTIONS: &[&str] = &[
    "act",
    "navigate",
    "search",
    "go_back",
    "wait",
    "click",
    "input",
    "input_credential",
    "send_keys",
    "scroll",
    "find_text",
    "search_page",
    "find_elements",
    "upload_file",
    "extract",
    "screenshot",
    "save_as_pdf",
    "download",
    "dropdown_options",
    "select_dropdown",
    "switch",
    "close",
    "evaluate",
    "console",
    "status",
    "state",
    "import_cookies",
];

pub(crate) fn is_browser_tool(name: &str) -> bool {
    name.strip_prefix("browser_")
        .map(|action| BROWSER_ACTIONS.contains(&action))
        .unwrap_or(false)
}

const MAX_STATE_CHARS: usize = 12_000;
const MAX_TEXT_CHARS: usize = 8_000;

/// The whole browser drain has one deadline shared by registry acquisition,
/// every slot acquisition, and every Chrome flush. Slots still receive a
/// smaller acquisition cap so a wedged action cannot consume the flush budget.
const SHUTDOWN_LOCK_BUDGET: Duration = Duration::from_secs(2);
const SHUTDOWN_GLOBAL_BUDGET: Duration = Duration::from_secs(8);

static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::Acquire)
}

/// One Chromium per browser-agent instance, keyed by instance id ("" = the
/// canonical durable-session agent; a parallel instance keys its job scope).
/// Each slot is its own Arc<Mutex> so instances lock independently — one
/// wedged instance must never freeze the others' actions. This is the
/// Strawberry-style fan-out: orchestrator → N browser instances, each driving
/// its OWN logged-in chrome.
static SESSIONS: std::sync::OnceLock<
    Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<Option<Session>>>>>,
> = std::sync::OnceLock::new();

static LAST_USED: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Instant>>> =
    std::sync::OnceLock::new();
static RESTORE_REQUESTED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();
static SUSPENDED: std::sync::OnceLock<Mutex<std::collections::HashMap<String, SuspendedBrowser>>> =
    std::sync::OnceLock::new();

/// Lifecycle requested by the desktop for the compositor-backed Chromium
/// surface. `Open` keeps the coworker's existing Phoenix profile and CDP
/// session, but ensures that Chrome owns a real X11 window the desktop can
/// embed. `Close` only detaches that surface; the browser stays alive so an
/// agent can continue the same page without losing cookies, tabs, or state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSurfaceAction {
    Open,
    Close,
    Status,
}

/// Typed gateway reply used by Canvas to locate exactly the Chromium window
/// Phoenix launched. PID + an unguessable per-instance WM_CLASS token form a
/// two-part identity; the desktop must match both before reparenting a window.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSurfaceTab {
    pub id: String,
    pub title: String,
    pub url: String,
    pub active: bool,
    /// The page's own icon as Chromium resolved it from `<link rel="icon">`.
    /// Empty when the document declares none — Canvas then draws its own mark
    /// instead of requesting a `/favicon.ico` most sites do not serve.
    #[serde(default)]
    pub favicon: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSurfaceReply {
    pub supported: bool,
    pub instance: String,
    pub pid: Option<u32>,
    pub window_token: Option<String>,
    pub url: String,
    pub attached: bool,
    #[serde(default)]
    pub tabs: Vec<BrowserSurfaceTab>,
}

impl std::fmt::Debug for BrowserSurfaceReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserSurfaceReply")
            .field("supported", &self.supported)
            .field("instance", &self.instance)
            .field("pid", &self.pid)
            .field(
                "window_token",
                &self.window_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("url", &self.url)
            .field("attached", &self.attached)
            .field("tabs", &self.tabs.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct BrowserSurfaceLease {
    window_token: String,
    attached: bool,
}

static BROWSER_SURFACES: std::sync::OnceLock<
    Mutex<std::collections::HashMap<String, BrowserSurfaceLease>>,
> = std::sync::OnceLock::new();

#[derive(Debug, Clone)]
struct SuspendedBrowser {
    suspended_at: Instant,
    idle_for: Duration,
    host: String,
    tabs: usize,
}

fn session_slot(instance: &str) -> std::sync::Arc<Mutex<Option<Session>>> {
    let registry = SESSIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut map = match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let slot = map.entry(instance.to_string()).or_default().clone();
    drop(map);
    ensure_idle_reaper();
    slot
}

/// Close and forget one disposable volume-worker browser identity.
///
/// Visible coworkers own durable profiles; volume workers deliberately do
/// not. Their useful result is persisted by the calling coworker's
/// `volume_work` tool receipt, so retaining cookies, tabs, registry slots, or
/// profile files would only make a rare fan-out feature grow `.phoenix`
/// forever. The strict prefix and character checks make this helper incapable
/// of targeting a real coworker profile.
pub(crate) fn discard_volume_worker_profile(instance: &str) -> Result<()> {
    anyhow::ensure!(
        is_volume_worker_profile(instance),
        "refusing to discard non-volume browser profile `{instance}`"
    );
    clear_volume_worker_cookie_seed(instance);
    discard_ephemeral_browser_profile_inner(instance)
}

/// Drop a browser profile owned by a non-canonical job scope. This is the
/// sibling of volume-worker cleanup for parallel named-agent instances such as
/// `agent-coder-job-<digest>`; normal `agent-coder` profiles can never pass
/// the strict shape check and therefore remain durable.
pub(crate) fn discard_ephemeral_browser_profile(instance: &str) -> Result<()> {
    anyhow::ensure!(
        is_disposable_browser_profile(instance),
        "refusing to discard durable browser profile `{instance}`"
    );
    clear_volume_worker_cookie_seed(instance);
    discard_ephemeral_browser_profile_inner(instance)
}

fn is_volume_worker_profile(instance: &str) -> bool {
    instance.starts_with("volume-worker-volume-") && safe_browser_instance_id(instance)
}

fn is_disposable_browser_profile(instance: &str) -> bool {
    is_volume_worker_profile(instance)
        || (instance.starts_with("agent-")
            && instance.contains("-job-")
            && safe_browser_instance_id(instance))
}

fn safe_browser_instance_id(instance: &str) -> bool {
    !instance.is_empty()
        && instance.len() <= 128
        && instance
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn discard_ephemeral_browser_profile_inner(instance: &str) -> Result<()> {
    // Never queue behind a possibly abandoned action. Detach state that is
    // immediately obtainable, then terminate exact profile holders through a
    // PIDFD-verified `/proc` sweep below. The sweep remains effective even if
    // an old thread owns the slot mutex forever.
    let slot = SESSIONS
        .get()
        .and_then(|registry| registry.lock().ok())
        .and_then(|sessions| sessions.get(instance).cloned());
    if let Some(slot) = slot {
        if let Some(mut guard) = try_lock_for(&slot, Duration::from_millis(100)) {
            if let Some(session) = guard.as_ref() {
                session
                    .flush_and_close_until(std::time::Instant::now() + Duration::from_millis(500));
            }
            *guard = None;
        }
    }

    if let Some(registry) = SESSIONS.get() {
        let _ = registry
            .lock()
            .map(|mut sessions| sessions.remove(instance));
    }
    if let Some(last_used) = LAST_USED.get() {
        let _ = last_used.lock().map(|mut used| used.remove(instance));
    }
    clear_restore_requested(instance);
    let _ = take_suspended(instance);
    // Current workers use independent Electron tabs in the spawning agent's
    // authenticated partition. Destroy the child's views, never the shared
    // partition; legacy/headless workers continue through exact profile
    // cleanup below.
    if let Err(error) = session::discard_embedded_browser_surface(instance) {
        tracing::warn!(
            browser_instance = %instance,
            "could not discard embedded worker browser surface: {error:#}"
        );
    }
    if let Some(surfaces) = BROWSER_SURFACES.get() {
        let _ = surfaces.lock().map(|mut values| values.remove(instance));
    }

    let profile = session::profile_dir_for_instance(instance, "phoenix")?;
    let force_deadline = std::time::Instant::now() + Duration::from_secs(4);
    anyhow::ensure!(
        engine::force_close_exact_profile_holders(&profile, force_deadline),
        "could not confirm termination of Chromium holding disposable profile `{instance}`; profile was retained"
    );
    match std::fs::symlink_metadata(&profile) {
        Ok(metadata) if metadata.file_type().is_dir() => std::fs::remove_dir_all(&profile)
            .with_context(|| format!("remove disposable browser profile {}", profile.display()))?,
        Ok(_) => std::fs::remove_file(&profile)
            .with_context(|| format!("remove disposable browser entry {}", profile.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", profile.display())),
    }
    Ok(())
}

/// Detach a Chromium process from an X desktop that is being reclaimed.
///
/// This intentionally preserves the durable browser profile for canonical
/// coworkers; a later turn can reopen it on a fresh private display with its
/// cookies and storage intact. Disposable volume workers call their stricter
/// `discard_volume_worker_profile` cleanup afterwards. The bounded lock keeps
/// desktop reaping from hanging behind an abandoned browser action.
pub(crate) fn close_instance_for_desktop(instance: &str) {
    let slot = SESSIONS
        .get()
        .and_then(|registry| registry.lock().ok())
        .and_then(|sessions| sessions.get(instance).cloned());
    let Some(slot) = slot else {
        return;
    };
    let Some(mut guard) = try_lock_for(&slot, Duration::from_secs(3)) else {
        let label = if instance.is_empty() {
            "main"
        } else {
            instance
        };
        tracing::warn!(
            browser_instance = %label,
            "isolated desktop cleanup could not acquire browser slot; the X server will still terminate its visible clients"
        );
        return;
    };
    detach_native_surface(instance);
    if let Some(session) = guard.as_ref() {
        session.flush_and_close();
    }
    *guard = None;
    if let Some(last_used) = LAST_USED.get() {
        let _ = last_used.lock().map(|mut used| used.remove(instance));
    }
    clear_restore_requested(instance);
    let _ = take_suspended(instance);
}

fn touch_instance(instance: &str) {
    let _ = LAST_USED
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map(|mut used| used.insert(instance.to_string(), Instant::now()));
}

struct BrowserActivity(String);

impl BrowserActivity {
    fn begin(instance: &str) -> Self {
        touch_instance(instance);
        Self(instance.to_string())
    }
}

impl Drop for BrowserActivity {
    fn drop(&mut self) {
        touch_instance(&self.0);
    }
}

#[cfg_attr(test, allow(dead_code))]
fn mark_restore_requested(instance: &str) {
    let _ = RESTORE_REQUESTED
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|mut instances| instances.insert(instance.to_string()));
}

pub(super) fn restore_requested(instance: &str) -> bool {
    RESTORE_REQUESTED
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|instances| instances.contains(instance))
        .unwrap_or(false)
}

pub(super) fn clear_restore_requested(instance: &str) {
    let _ = RESTORE_REQUESTED
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|mut instances| instances.remove(instance));
}

fn take_suspended(instance: &str) -> Option<SuspendedBrowser> {
    SUSPENDED
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .ok()
        .and_then(|mut suspended| suspended.remove(instance))
}

fn suspended_status(instance: &str) -> Option<String> {
    let record = SUSPENDED
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .ok()?
        .get(instance)
        .cloned()?;
    Some(format!(
        "Browser is suspended to save RAM (idle {}s; suspended {}s ago; {} tab{}; last site {}). Cookies, localStorage, and the restorable Chrome session are persisted. The next browser action resumes it lazily.",
        record.idle_for.as_secs(),
        record.suspended_at.elapsed().as_secs(),
        record.tabs,
        if record.tabs == 1 { "" } else { "s" },
        if record.host.is_empty() { "unknown" } else { &record.host }
    ))
}

fn ensure_idle_reaper() {
    #[cfg(not(test))]
    {
        static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        STARTED.get_or_init(|| {
            std::thread::spawn(|| loop {
                std::thread::sleep(Duration::from_secs(30));
                if shutdown_requested() {
                    break;
                }
                let Some(timeout) = session::browser_prefs().suspend_after else {
                    continue;
                };
                let now = Instant::now();
                let last_used = LAST_USED
                    .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                    .lock()
                    .map(|used| used.clone())
                    .unwrap_or_default();
                let slots = SESSIONS
                    .get()
                    .and_then(|registry| registry.lock().ok())
                    .map(|registry| {
                        registry
                            .iter()
                            .map(|(instance, slot)| (instance.clone(), Arc::clone(slot)))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for (instance, slot) in slots {
                    // An embedded native window is an active user surface even
                    // while its page is quiet. Suspending it would make the
                    // child window vanish out from under the desktop.
                    if native_surface_attached(&instance) {
                        continue;
                    }
                    let Some(last) = last_used.get(&instance).copied() else {
                        continue;
                    };
                    let idle_for = now.saturating_duration_since(last);
                    if idle_for < timeout {
                        continue;
                    }
                    let Some(mut guard) = try_lock_for(&slot, Duration::from_millis(100)) else {
                        continue;
                    };
                    let current_idle = LAST_USED
                        .get()
                        .and_then(|used| used.lock().ok())
                        .and_then(|used| used.get(&instance).copied())
                        .map(|last| Instant::now().saturating_duration_since(last))
                        .unwrap_or_default();
                    if current_idle < timeout {
                        continue;
                    }
                    let Some(session) = guard.as_ref() else {
                        continue;
                    };
                    if matches!(session.health.mode, "attach" | "embedded" | "reconnect") {
                        continue;
                    }
                    let host = host_of(&session.tab.get_url());
                    let tabs = session
                        ._browser
                        .get_tabs()
                        .lock()
                        .map(|tabs| tabs.len())
                        .unwrap_or(1);
                    let session = guard.take().expect("session checked above");
                    mark_restore_requested(&instance);
                    session.flush_and_close();
                    drop(session);
                    let record = SuspendedBrowser {
                        suspended_at: Instant::now(),
                        idle_for: current_idle,
                        host,
                        tabs,
                    };
                    let _ = SUSPENDED
                        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                        .lock()
                        .map(|mut suspended| suspended.insert(instance.clone(), record));
                    blog(&format!(
                        "suspended instance '{}' after {}s idle — profile flushed; next action restores the Chrome session",
                        if instance.is_empty() { "main" } else { &instance },
                        current_idle.as_secs()
                    ));
                }
            });
        });
    }
}

/// Full-relaunch count per instance (gateway-process lifetime). A browser
/// that keeps dying mid-job is an UNSTABLE ENVIRONMENT — the count rides in
/// the environment-change banner so the agent (and its watchers) can see
/// "this is the 3rd fresh browser this job" instead of treating every
/// relaunch as the first (live 2026-07-06: chrome died repeatedly during a
/// visible Discord login handoff, silently wiping the user's typing each
/// time, and the agent re-ran the identical plan after every wipe).
static RESTARTS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, u32>>> =
    std::sync::OnceLock::new();

/// Instances whose session was dropped by a FAILURE path (wedge recovery
/// failed, transport died twice) — the next lazy open is a relaunch the agent
/// must hear about, not a quiet first launch.
static PENDING_RELAUNCH: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

/// Login-wall sightings per (conversation, instance, host) — the streak that turns
/// "another login page" into a CLASSIFIED, escalating signal instead of an
/// infinitely retryable state. The private bounded file survives gateway and
/// browser relaunches; the map is only a fallback if that file is temporarily
/// unavailable. A crash mid-login must not reset Phoenix's retry counter.
static LOGIN_WALLS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, u32>>> =
    std::sync::OnceLock::new();

const LOGIN_WALL_STORE_VERSION: u32 = 1;
const LOGIN_WALL_STORE_MAX_BYTES: usize = 512 * 1024;
const LOGIN_WALL_STORE_MAX_ENTRIES: usize = 512;
const LOGIN_WALL_STREAK_LIFETIME_HOURS: i64 = 24;

#[derive(Debug, Default, Serialize, Deserialize)]
struct DurableLoginWalls {
    version: u32,
    #[serde(default)]
    entries: std::collections::HashMap<String, DurableLoginWall>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurableLoginWall {
    host: String,
    count: u32,
    updated_at: String,
}

/// Wall sightings on one host before the notice starts (and keeps) firing.
const LOGIN_WALL_ESCALATE: u32 = 3;

fn bump_restarts(instance: &str) -> u32 {
    let mut map = RESTARTS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let count = map.entry(instance.to_string()).or_insert(0);
    *count += 1;
    *count
}

fn restarts_of(instance: &str) -> u32 {
    RESTARTS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map(|map| map.get(instance).copied().unwrap_or(0))
        .unwrap_or(0)
}

fn mark_pending_relaunch(instance: &str) {
    let _ = PENDING_RELAUNCH
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|mut set| set.insert(instance.to_string()));
}

fn take_pending_relaunch(instance: &str) -> bool {
    PENDING_RELAUNCH
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|mut set| set.remove(instance))
        .unwrap_or(false)
}

/// Journal an environment event when the executor told us which session this
/// browser serves (best-effort context for the watchers, never load-bearing).
fn journal_env(session_id: Option<&str>, detail: &str) {
    blog(detail);
    if let Some(sid) = session_id {
        crate::runtime::journal::record(sid, "environment", "browser", detail);
    }
}

/// Append one timestamped browser-lifecycle line to the gateway log
/// (~/.phoenix/gateway.log). File-only — never stdout: browser code also runs
/// inside the interactive CLI process, where a stray println corrupts the
/// TUI's in-place redraws. This lane was previously invisible in the log
/// (tracing goes to stderr with a `warn` default filter), which is why the
/// 2026-07-06 mid-login teardown could not be reconstructed afterwards:
/// launches, crashes, reconnects and clean-closes all left zero trace.
pub(super) fn blog(line: &str) {
    crate::runtime::gwlog(&format!("browser: {line}"));
}

/// Is this chrome pid still running? The load-bearing forensic bit at
/// transport-death time: a live pid means only the SOCKET died (reconnect will
/// keep the user's page); a dead pid means the process crashed or was killed
/// (relaunch is unavoidable and anything unflushed is gone).
fn chrome_pid_alive(pid: Option<u32>) -> Option<bool> {
    pid.map(|p| std::path::Path::new(&format!("/proc/{p}")).exists())
}

fn validate_browser_surface_instance(instance: &str) -> std::result::Result<(), String> {
    if instance.len() <= 128
        && instance
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        Ok(())
    } else {
        Err("invalid browser surface profile id; use letters, numbers, '-' or '_' only".into())
    }
}

/// Native Chromium embedding currently uses an X11 child window. That works
/// on a native X11 session and on Wayland through XWayland, but not on a
/// display-less gateway or a compositor with XWayland disabled. The reply's
/// `supported` bit lets the desktop fall back to the bounded screencast lane.
fn browser_surface_supported() -> bool {
    if crate::config::paths::chromium_bridge_url()
        .ok()
        .flatten()
        .is_some_and(|url| url.starts_with("http://127.0.0.1:"))
    {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn browser_surface_lease(instance: &str) -> Option<BrowserSurfaceLease> {
    BROWSER_SURFACES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .ok()
        .and_then(|leases| leases.get(instance).cloned())
}

/// Token consumed by `Session::launch`. It is exposed only while the desktop
/// has an attached surface, so background relaunches after a detached window
/// return to the normal headless policy.
pub(super) fn native_surface_token(instance: &str) -> Option<String> {
    browser_surface_lease(instance)
        .filter(|lease| lease.attached)
        .map(|lease| lease.window_token)
}

pub(super) fn native_surface_attached(instance: &str) -> bool {
    browser_surface_lease(instance).is_some_and(|lease| lease.attached)
}

fn set_native_surface_attached(instance: &str, attached: bool) -> String {
    let mut leases = BROWSER_SURFACES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lease = leases
        .entry(instance.to_string())
        .or_insert_with(|| BrowserSurfaceLease {
            window_token: uuid::Uuid::new_v4().simple().to_string(),
            attached: false,
        });
    lease.attached = attached;
    lease.window_token.clone()
}

fn detach_native_surface(instance: &str) {
    if let Some(leases) = BROWSER_SURFACES.get() {
        if let Ok(mut leases) = leases.lock() {
            if let Some(lease) = leases.get_mut(instance) {
                lease.attached = false;
            }
        }
    }
}

fn browser_surface_reply(
    instance: &str,
    session: Option<&Session>,
    supported: bool,
) -> BrowserSurfaceReply {
    let lease = browser_surface_lease(instance);
    let process_alive = session
        .and_then(|session| chrome_pid_alive(session.owned_pid))
        .unwrap_or(session.is_some());
    let token_matches = session.is_some_and(|session| {
        lease.as_ref().is_some_and(|lease| {
            session.surface_token.as_deref() == Some(lease.window_token.as_str())
        })
    });
    let attached = supported
        && process_alive
        && token_matches
        && session.is_some_and(|session| !session.headless && session.health.mode == "embedded")
        && lease.as_ref().is_some_and(|lease| lease.attached);
    let shell_state = (supported && process_alive && native_surface_attached(instance))
        .then(|| session::embedded_browser_surface_action(instance, "status", None, None))
        .transpose()
        .ok()
        .flatten()
        .flatten();
    let tabs = shell_state
        .as_ref()
        .map(|state| {
            state
                .tabs
                .iter()
                .take(24)
                .map(|tab| BrowserSurfaceTab {
                    id: tab.id.clone(),
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    active: tab.active,
                    favicon: tab.favicon.clone(),
                })
                .collect::<Vec<_>>()
        })
        .or_else(|| {
            session.filter(|_| process_alive).and_then(|session| {
                session._browser.get_tabs().lock().ok().map(|tabs| {
                    tabs.iter()
                        .filter(|tab| std::sync::Arc::ptr_eq(tab, &session.tab))
                        .map(|tab| BrowserSurfaceTab {
                            id: tab.get_target_id().to_string(),
                            title: tab.get_title().unwrap_or_default(),
                            url: tab.get_url(),
                            active: true,
                            // Raw-CDP fallback path: no favicon channel here.
                            favicon: String::new(),
                        })
                        .collect::<Vec<_>>()
                })
            })
        })
        .unwrap_or_default();
    let active_url = tabs
        .iter()
        .find(|tab| tab.active)
        .map(|tab| tab.url.clone());
    BrowserSurfaceReply {
        supported,
        instance: instance.to_string(),
        pid: session
            .filter(|_| process_alive)
            .and_then(|session| session.owned_pid),
        window_token: session
            .filter(|_| process_alive && token_matches)
            .and_then(|session| session.surface_token.clone()),
        url: active_url
            .or_else(|| {
                session
                    .filter(|_| process_alive)
                    .map(|session| session.tab.get_url())
            })
            .unwrap_or_default(),
        attached,
        tabs,
    }
}

/// Open, detach, or inspect the real Chromium surface for one coworker.
///
/// This does not create a second browser engine: it relaunches the existing
/// managed CDP session against the same per-agent user-data directory only
/// when its current process lacks the verified X11 token. Consequently the
/// agent and user always act on the same tab, cookies, downloads, and history.
pub fn browser_surface(
    instance: &str,
    action: BrowserSurfaceAction,
) -> std::result::Result<BrowserSurfaceReply, String> {
    validate_browser_surface_instance(instance)?;
    if crate::tools::isolated_desktop::browser_instance_is_isolated(instance) {
        // This Chromium belongs to a private Xephyr/Xvfb scope. The host
        // Canvas must not turn it into an embedded WebContentsView or launch a
        // replacement on the real desktop; browser_* remains available to the
        // owning agent on its own display.
        return match action {
            BrowserSurfaceAction::Status => Ok(BrowserSurfaceReply {
                supported: false,
                instance: instance.to_string(),
                pid: None,
                window_token: None,
                url: String::new(),
                attached: false,
                tabs: Vec::new(),
            }),
            BrowserSurfaceAction::Open | BrowserSurfaceAction::Close => Err(
                "this browser belongs to an isolated agent desktop and cannot be embedded in the host Canvas".to_string(),
            ),
        };
    }
    let supported = browser_surface_supported();
    if !supported {
        return Ok(BrowserSurfaceReply {
            supported: false,
            instance: instance.to_string(),
            pid: None,
            window_token: None,
            url: String::new(),
            attached: false,
            tabs: Vec::new(),
        });
    }

    if matches!(action, BrowserSurfaceAction::Status) {
        let slot = SESSIONS
            .get()
            .and_then(|sessions| sessions.lock().ok())
            .and_then(|sessions| sessions.get(instance).cloned());
        let Some(slot) = slot else {
            return Ok(browser_surface_reply(instance, None, true));
        };
        let guard = try_lock_for(&slot, Duration::from_millis(250))
            .ok_or_else(|| "browser surface status is temporarily busy".to_string())?;
        let reply = browser_surface_reply(instance, guard.as_ref(), true);
        if !reply.attached && native_surface_attached(instance) {
            detach_native_surface(instance);
        }
        return Ok(reply);
    }

    if matches!(action, BrowserSurfaceAction::Close) {
        detach_native_surface(instance);
        let slot = SESSIONS
            .get()
            .and_then(|sessions| sessions.lock().ok())
            .and_then(|sessions| sessions.get(instance).cloned());
        let Some(slot) = slot else {
            return Ok(browser_surface_reply(instance, None, true));
        };
        // Detaching the native window is immediate. Re-arm the JPEG fallback
        // opportunistically; if an agent owns the slot right now, its next tab
        // adoption will arm it instead of making close wait behind that work.
        let Some(guard) = try_lock_for(&slot, Duration::from_millis(100)) else {
            let retry_slot = Arc::clone(&slot);
            let retry_instance = instance.to_string();
            let _ = std::thread::Builder::new()
                .name("phoenix-browser-fallback-rearm".to_string())
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while Instant::now() < deadline && !shutdown_requested() {
                        if native_surface_attached(&retry_instance) {
                            return;
                        }
                        if let Some(guard) = try_lock_for(&retry_slot, Duration::from_millis(100)) {
                            if let Some(session) = guard
                                .as_ref()
                                .filter(|session| session.health.mode != "embedded")
                            {
                                screencast::arm(&session.tab, &retry_instance);
                            }
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    blog(&format!(
                        "screencast: fallback rearm timed out for instance '{}'",
                        if retry_instance.is_empty() {
                            "main"
                        } else {
                            &retry_instance
                        }
                    ));
                });
            return Ok(BrowserSurfaceReply {
                supported: true,
                instance: instance.to_string(),
                pid: None,
                window_token: None,
                url: String::new(),
                attached: false,
                tabs: Vec::new(),
            });
        };
        if let Some(session) = guard
            .as_ref()
            .filter(|session| session.health.mode != "embedded")
        {
            screencast::arm(&session.tab, instance);
        }
        return Ok(browser_surface_reply(instance, guard.as_ref(), true));
    }

    if shutdown_requested() {
        return Err("gateway browser lane is shutting down; native surface cannot open".into());
    }
    let previous_attached = native_surface_attached(instance);
    let window_token = set_native_surface_attached(instance, true);
    let slot = session_slot(instance);
    let mut guard = match try_lock_for(&slot, Duration::from_secs(10)) {
        Some(guard) => guard,
        None => {
            if !previous_attached {
                detach_native_surface(instance);
            }
            return Err("browser surface is busy — a browser action is still running".into());
        }
    };
    let _activity = BrowserActivity::begin(instance);
    let current_embedded_target = guard
        .as_ref()
        .filter(|session| session.health.mode == "embedded")
        .and_then(|_| session::embedded_browser_target_id(instance).ok().flatten());
    let reusable = guard.as_ref().is_some_and(|session| {
        !session.headless
            && session.health.mode == "embedded"
            && chrome_pid_alive(session.owned_pid) != Some(false)
            && session.surface_token.as_deref() == Some(window_token.as_str())
            && current_embedded_target.as_deref() == Some(session.tab.get_target_id().as_str())
    });
    if !reusable {
        let restore_url = guard
            .as_ref()
            .map(|session| session.tab.get_url())
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"));
        if let Some(session) = guard.take() {
            session.flush_and_close();
            drop(session);
        }
        match Session::open(instance) {
            Ok(session) => {
                if let Some(url) = restore_url.as_deref() {
                    if let Err(error) = session.tab.navigate_to(url) {
                        let host = host_of(url);
                        tracing::warn!(
                            "native Chromium surface could not restore its prior site {host}: {error:#}"
                        );
                    }
                }
                *guard = Some(session);
            }
            Err(error) => {
                detach_native_surface(instance);
                return Err(format!("native Chromium surface could not open: {error:#}"));
            }
        }
    }
    let session = guard
        .as_ref()
        .ok_or_else(|| "native Chromium surface did not retain its browser session".to_string())?;
    screencast::disarm(&session.tab, instance);
    let reply = browser_surface_reply(instance, Some(session), true);
    if !reply.attached {
        detach_native_surface(instance);
        return Err("native Chromium launched without a verifiable surface identity".into());
    }
    let _ = take_suspended(instance);
    blog(&format!(
        "native surface attached for instance '{}' (pid {}, window token verified)",
        if instance.is_empty() {
            "main"
        } else {
            instance
        },
        reply.pid.unwrap_or_default()
    ));
    Ok(reply)
}

/// Canvas click-to-help (2026-07-16): the user clicks inside a canvas
/// browser window and the click lands on the real page — same tab, same
/// logged-in session the agent is driving, so the user can push past a
/// blocker (a cookie banner, a stray popup) without taking the lane over.
/// Coordinates are page CSS pixels: the screencast frame is the viewport
/// 1:1 below the 1920px capture cap, so the canvas maps image-relative
/// coordinates straight through. No-op when the instance has no live chrome.
pub fn click_instance(instance: &str, x: f64, y: f64) -> Result<(), String> {
    if shutdown_requested() {
        return Err("gateway browser lane is shutting down".to_string());
    }
    let slot = session_slot(instance);
    let guard = try_lock_for(&slot, Duration::from_secs(2))
        .ok_or_else(|| format!("browser instance '{instance}' is busy"))?;
    if shutdown_requested() {
        return Err("gateway browser lane is shutting down".to_string());
    }
    let Some(session) = guard.as_ref() else {
        return Err(format!("no live browser for instance '{instance}'"));
    };
    session.prepare_input().map_err(|error| error.to_string())?;
    session
        .tab
        .click_point(Point { x, y })
        .map_err(|error| format!("assist click failed: {error}"))?;
    blog(&format!(
        "canvas assist click at ({x:.0},{y:.0}) on instance '{instance}'"
    ));
    Ok(())
}

/// A direct human action in Phoenix's embedded browser. This is deliberately
/// smaller than the agent browser catalog: it is the reliable set a user can
/// demonstrate while teaching a routine. Typed text is redacted from Debug
/// output and zeroized when the request is dropped.
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrowserUserAction {
    Navigate {
        url: String,
        #[serde(default)]
        new_tab: bool,
    },
    GoBack,
    SwitchTab {
        tab_id: String,
    },
    CloseTab {
        tab_id: String,
    },
    /// Match the managed Chromium viewport to the visible in-app browser.
    /// This is transport chrome, not a teachable workflow action.
    Resize {
        width: u32,
        height: u32,
    },
    Click {
        x: f64,
        y: f64,
    },
    /// Human wheel input at a viewport point. This changes transient page
    /// position but is deliberately not a durable workflow instruction;
    /// taught clicks retain semantic targets and do not depend on scroll
    /// distances that become stale when a site layout changes.
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Type {
        text: String,
        #[serde(default = "default_true")]
        clear: bool,
        #[serde(default)]
        sensitive: bool,
        #[serde(default)]
        parameter_name: Option<String>,
        /// The semantic target already returned by the preceding user click.
        /// Reusing it removes a full CDP inspection round trip from every
        /// typing batch while retaining a durable, human-readable workflow.
        #[serde(default)]
        target_hint: Option<BrowserSemanticTarget>,
    },
    SendKeys {
        keys: String,
    },
    Select {
        x: f64,
        y: f64,
        option: String,
    },
}

impl std::fmt::Debug for BrowserUserAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Navigate { url, new_tab } => formatter
                .debug_struct("Navigate")
                .field("url", url)
                .field("new_tab", new_tab)
                .finish(),
            Self::GoBack => formatter.write_str("GoBack"),
            Self::SwitchTab { tab_id } => formatter
                .debug_struct("SwitchTab")
                .field("tab_id", tab_id)
                .finish(),
            Self::CloseTab { tab_id } => formatter
                .debug_struct("CloseTab")
                .field("tab_id", tab_id)
                .finish(),
            Self::Resize { width, height } => formatter
                .debug_struct("Resize")
                .field("width", width)
                .field("height", height)
                .finish(),
            Self::Click { x, y } => formatter
                .debug_struct("Click")
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => formatter
                .debug_struct("Scroll")
                .field("x", x)
                .field("y", y)
                .field("delta_x", delta_x)
                .field("delta_y", delta_y)
                .finish(),
            Self::Type {
                clear,
                sensitive,
                parameter_name,
                target_hint,
                ..
            } => formatter
                .debug_struct("Type")
                .field("text", &"[REDACTED]")
                .field("clear", clear)
                .field("sensitive", sensitive)
                .field("parameter_name", parameter_name)
                .field("has_target_hint", &target_hint.is_some())
                .finish(),
            Self::SendKeys { keys } => formatter
                .debug_struct("SendKeys")
                .field("keys", keys)
                .finish(),
            Self::Select { x, y, option } => formatter
                .debug_struct("Select")
                .field("x", x)
                .field("y", y)
                .field("option", option)
                .finish(),
        }
    }
}

impl Drop for BrowserUserAction {
    fn drop(&mut self) {
        if let Self::Type { text, .. } = self {
            text.zeroize();
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSemanticTarget {
    pub tag: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub input_type: String,
    #[serde(default)]
    pub autocomplete: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub element_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub selector: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserInteractionReceipt {
    pub action: String,
    pub before_url: String,
    pub after_url: String,
    #[serde(default)]
    pub target: Option<BrowserSemanticTarget>,
    #[serde(default)]
    pub sensitive: bool,
}

fn embedded_tab_by_target(session: &Session, target_id: &str) -> Result<Arc<Tab>, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        session._browser.register_missing_tabs();
        if let Some(tab) = session
            ._browser
            .get_tabs()
            .lock()
            .map_err(|_| "browser tab registry is unavailable".to_string())?
            .iter()
            .find(|candidate| candidate.get_target_id().as_str() == target_id)
            .cloned()
        {
            return Ok(tab);
        }
        if Instant::now() >= deadline {
            return Err("the embedded browser tab was not discoverable over CDP".to_string());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn direct_interaction_transport_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "underlying connection is closed",
        "connection is closed",
        "connection closed",
        "target closed",
        "the embedded browser tab was not discoverable over cdp",
        "was not discoverable over cdp",
        "sending on a closed channel",
        "receiving on an empty and disconnected channel",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn direct_new_tab_session_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    direct_interaction_transport_error(error)
        || lower.contains("no session with given id")
        || lower.contains("method call error -32602")
        || lower.contains("method call error -32000")
        || (lower.contains("not supported") && lower.contains("method call"))
}

fn reconnect_direct_embedded_session(instance: &str) -> Result<(), String> {
    let slot = session_slot(instance);
    let mut guard = try_lock_for(&slot, Duration::from_secs(10))
        .ok_or_else(|| format!("browser instance '{instance}' is busy during reconnect"))?;
    if let Some(dead) = guard.take() {
        drop(dead);
    }
    *guard =
        Some(Session::open(instance).map_err(|error| {
            format!("could not reconnect the in-app browser session: {error:#}")
        })?);
    blog(&format!(
        "reconnected direct in-app browser controls for instance '{}'",
        if instance.is_empty() {
            "agent-phoenix"
        } else {
            instance
        }
    ));
    Ok(())
}

/// Execute one user demonstration action on the exact private browser profile
/// the selected coworker uses. The caller owns recording policy; this function
/// returns only semantic page evidence and never echoes typed text. A desktop
/// shell restart can leave the detached gateway holding a dead CDP socket; the
/// direct-control lane gets the same single reconnect attempt as agent tools.
pub fn user_interact(
    instance: &str,
    action: &BrowserUserAction,
) -> Result<BrowserInteractionReceipt, String> {
    let error = match user_interact_once(instance, action) {
        Ok(receipt) => return Ok(receipt),
        Err(error) => error,
    };
    let new_tab = matches!(action, BrowserUserAction::Navigate { new_tab: true, .. });
    let recoverable = direct_interaction_transport_error(&error)
        || (new_tab && direct_new_tab_session_error(&error));
    if !native_surface_attached(instance) || !recoverable {
        return Err(error);
    }
    blog(&format!(
        "direct browser control session expired for '{}': reconnecting once",
        if instance.is_empty() {
            "agent-phoenix"
        } else {
            instance
        }
    ));
    reconnect_direct_embedded_session(instance)?;
    // The shell creates and activates a requested new tab before CDP discovers
    // it. If discovery was the failing step, reconnect lands on that tab; do
    // not create a duplicate when replaying the navigation.
    if let BrowserUserAction::Navigate { url, new_tab: true } = action {
        let retry = BrowserUserAction::Navigate {
            url: url.clone(),
            new_tab: false,
        };
        user_interact_once(instance, &retry)
    } else {
        user_interact_once(instance, action)
    }
}

fn user_interact_once(
    instance: &str,
    action: &BrowserUserAction,
) -> Result<BrowserInteractionReceipt, String> {
    if shutdown_requested() {
        return Err("gateway browser lane is shutting down".to_string());
    }
    let slot = session_slot(instance);
    let mut guard = try_lock_for(&slot, Duration::from_secs(10))
        .ok_or_else(|| format!("browser instance '{instance}' is busy"))?;
    if guard.is_none() {
        *guard = Some(Session::open(instance).map_err(|error| format!("{error:#}"))?);
        let _ = take_suspended(instance);
        clear_restore_requested(instance);
    }
    let _activity = BrowserActivity::begin(instance);
    let session = guard
        .as_mut()
        .ok_or_else(|| format!("no live browser for instance '{instance}'"))?;
    if matches!(action, BrowserUserAction::Click { .. } | BrowserUserAction::Scroll { .. }
        | BrowserUserAction::Type { .. } | BrowserUserAction::SendKeys { .. } | BrowserUserAction::Select { .. }) {
        session.prepare_input().map_err(|error| error.to_string())?;
    }
    let tab = session.tab.clone();
    let before_url = real_url(&tab);
    let mut target = None;
    let mut sensitive = false;
    let action_name = match action {
        BrowserUserAction::Resize { width, height } => {
            if !(640..=2_560).contains(width) || !(480..=1_600).contains(height) {
                return Err(
                    "browser viewport is outside the safe 640x480 to 2560x1600 range".into(),
                );
            }
            tab.set_bounds(Bounds::Normal {
                left: None,
                top: None,
                width: Some(f64::from(*width)),
                height: Some(f64::from(*height)),
            })
            .map_err(|error| format!("resizing browser viewport failed: {error}"))?;
            // Browser.setWindowBounds changes outer chrome dimensions and is
            // not an authoritative viewport resize in modern headless Chrome.
            // Match the page's CSS viewport explicitly so the screencast is
            // rendered at the same dimensions as the in-app pane instead of
            // stretching a stale 1280px surface into a larger window.
            tab.call_method(Emulation::SetDeviceMetricsOverride {
                width: *width,
                height: *height,
                device_scale_factor: 1.0,
                mobile: false,
                scale: None,
                screen_width: Some(*width),
                screen_height: Some(*height),
                position_x: None,
                position_y: None,
                dont_set_visible_size: Some(false),
                screen_orientation: None,
                viewport: None,
                display_feature: None,
                device_posture: None,
            })
            .map_err(|error| format!("setting the browser page viewport failed: {error}"))?;
            "resize"
        }
        BrowserUserAction::Navigate { url, new_tab } => {
            let url = normalize_url(url.clone());
            if *new_tab && native_surface_attached(instance) {
                let state =
                    session::embedded_browser_surface_action(instance, "new_tab", None, Some(&url))
                        .map_err(|error| {
                            format!("opening a new embedded browser tab failed: {error:#}")
                        })?
                        .ok_or_else(|| "the Phoenix Chromium shell is unavailable".to_string())?;
                let target_id = state
                    .target_id
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "the new embedded browser tab has no target id".to_string())?;
                match embedded_tab_by_target(session, &target_id) {
                    Ok(next) => session.adopt_native_tab(next).map_err(|error| {
                        format!("opening a new embedded browser tab failed: {error:#}")
                    })?,
                    Err(discovery_error) => {
                        let replacement = Session::open(instance).map_err(|error| {
                            format!("the new sidebar tab opened, but browser control could not reattach after {discovery_error}: {error:#}")
                        })?;
                        *session = replacement;
                    }
                }
                settle_page(&session.tab);
            } else if *new_tab {
                let next = session
                    ._browser
                    .new_tab()
                    .map_err(|error| format!("opening a new browser tab failed: {error}"))?;
                next.set_default_timeout(Duration::from_secs(20));
                bounded_navigate(&next, &url)
                    .map_err(|error| format!("navigation failed: {error:#}"))?;
                settle_page(&next);
                session
                    .adopt_tab(next)
                    .map_err(|error| format!("opening a new browser tab failed: {error:#}"))?;
            } else {
                bounded_navigate(&tab, &url)
                    .map_err(|error| format!("navigation failed: {error:#}"))?;
                settle_page(&tab);
            }
            "navigate"
        }
        BrowserUserAction::GoBack => {
            tab.evaluate("history.back()", false)
                .map_err(|error| format!("back navigation failed: {error}"))?;
            "go_back"
        }
        BrowserUserAction::SwitchTab { tab_id } => {
            if native_surface_attached(instance) {
                let state = session::embedded_browser_surface_action(
                    instance,
                    "switch_tab",
                    Some(tab_id),
                    None,
                )
                .map_err(|error| format!("switching embedded browser tabs failed: {error:#}"))?
                .ok_or_else(|| "the Phoenix Chromium shell is unavailable".to_string())?;
                let target_id = state
                    .target_id
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| tab_id.clone());
                let next = embedded_tab_by_target(session, &target_id)?;
                session.adopt_native_tab(next).map_err(|error| {
                    format!("switching embedded browser tabs failed: {error:#}")
                })?;
                "switch_tab"
            } else {
                let next = session
                    ._browser
                    .get_tabs()
                    .lock()
                    .map_err(|_| "browser tab registry is unavailable".to_string())?
                    .iter()
                    .find(|candidate| candidate.get_target_id().as_str() == tab_id.as_str())
                    .cloned()
                    .ok_or_else(|| "that browser tab is no longer open".to_string())?;
                next.bring_to_front()
                    .map_err(|error| format!("switching browser tabs failed: {error}"))?;
                session
                    .adopt_tab(next)
                    .map_err(|error| format!("switching browser tabs failed: {error:#}"))?;
                "switch_tab"
            }
        }
        BrowserUserAction::CloseTab { tab_id } => {
            if native_surface_attached(instance) {
                let state = session::embedded_browser_surface_action(
                    instance,
                    "close_tab",
                    Some(tab_id),
                    None,
                )
                .map_err(|error| format!("closing embedded browser tab failed: {error:#}"))?
                .ok_or_else(|| "the Phoenix Chromium shell is unavailable".to_string())?;
                let active = state
                    .tabs
                    .iter()
                    .find(|tab| tab.active)
                    .ok_or_else(|| "the browser did not retain an active tab".to_string())?;
                let replacement = embedded_tab_by_target(session, &active.id)?;
                session.adopt_native_tab(replacement).map_err(|error| {
                    format!("activating the remaining embedded browser tab failed: {error:#}")
                })?;
                "close_tab"
            } else {
                let tabs = session
                    ._browser
                    .get_tabs()
                    .lock()
                    .map_err(|_| "browser tab registry is unavailable".to_string())?
                    .clone();
                if tabs.len() <= 1 {
                    return Err("the last browser tab cannot be closed".into());
                }
                let closing = tabs
                    .iter()
                    .find(|candidate| candidate.get_target_id().as_str() == tab_id.as_str())
                    .cloned()
                    .ok_or_else(|| "that browser tab is no longer open".to_string())?;
                let closing_active = std::sync::Arc::ptr_eq(&closing, &session.tab);
                let replacement = tabs
                    .iter()
                    .find(|candidate| !std::sync::Arc::ptr_eq(candidate, &closing))
                    .cloned();
                closing
                    .close(false)
                    .map_err(|error| format!("closing browser tab failed: {error}"))?;
                if closing_active {
                    let replacement = replacement.ok_or_else(|| {
                        "the browser did not retain a replacement tab".to_string()
                    })?;
                    replacement.bring_to_front().map_err(|error| {
                        format!("activating the remaining browser tab failed: {error}")
                    })?;
                    session.adopt_tab(replacement).map_err(|error| {
                        format!("activating the remaining browser tab failed: {error:#}")
                    })?;
                }
                "close_tab"
            }
        }
        BrowserUserAction::Click { x, y } => {
            validate_user_point(*x, *y)?;
            target = semantic_target(&tab, Some((*x, *y)))?;
            tab.click_point(Point { x: *x, y: *y })
                .map_err(|error| format!("click failed: {error}"))?;
            "click"
        }
        BrowserUserAction::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            validate_user_point(*x, *y)?;
            if !delta_x.is_finite()
                || !delta_y.is_finite()
                || delta_x.abs() > 10_000.0
                || delta_y.abs() > 10_000.0
            {
                return Err("browser wheel delta is outside the safe range".into());
            }
            tab.call_method(Input::DispatchMouseEvent {
                Type: Input::DispatchMouseEventTypeOption::MouseWheel,
                x: *x,
                y: *y,
                modifiers: None,
                timestamp: None,
                button: None,
                buttons: None,
                click_count: None,
                force: None,
                tangential_pressure: None,
                tilt_x: None,
                tilt_y: None,
                twist: None,
                delta_x: Some(*delta_x),
                delta_y: Some(*delta_y),
                pointer_Type: Some(Input::DispatchMouseEventPointer_TypeOption::Mouse),
            })
            .map_err(|error| format!("scrolling browser page failed: {error}"))?;
            "scroll"
        }
        BrowserUserAction::Type {
            text,
            clear,
            sensitive: requested_sensitive,
            target_hint,
            ..
        } => {
            if text.len() > 64 * 1024 || text.contains('\0') {
                return Err("typed value is too large or contains an invalid character".into());
            }
            target = match target_hint {
                Some(target) => Some(target.clone()),
                None => semantic_target(&tab, None)?,
            };
            sensitive =
                *requested_sensitive || target.as_ref().is_some_and(semantic_target_is_sensitive);
            // A click leaves the field focused. Re-validating focus through
            // JavaScript before every small input batch doubled perceived
            // typing latency. Clearing still needs the selection operation;
            // otherwise Input.insertText is the only CDP round trip.
            if *clear || target_hint.is_none() {
                focus_active_editable(&tab, *clear)?;
            }
            // Insert the complete UI batch with one CDP call. `type_str`
            // dispatches keydown + keyup separately for every character;
            // over the embedded-browser RPC lane that made ordinary typing
            // trail the keyboard by seconds. Enter/Tab/etc. still use the
            // explicit SendKeys action below.
            tab.send_character(text)
                .map_err(|error| format!("typing failed at the input layer: {error}"))?;
            "type"
        }
        BrowserUserAction::SendKeys { keys } => {
            if keys.is_empty() || keys.len() > 256 || keys.contains('\0') {
                return Err("key sequence is empty, too large, or invalid".into());
            }
            target = semantic_target(&tab, None)?;
            press_keys(&tab, keys).map_err(|error| format!("sending keys failed: {error:#}"))?;
            // Tab and similar keys can move focus. Report the post-key target
            // so the next batched Type action can reuse the correct semantic
            // field instead of describing the field that just lost focus.
            target = semantic_target(&tab, None)?.or(target);
            "send_keys"
        }
        BrowserUserAction::Select { x, y, option } => {
            validate_user_point(*x, *y)?;
            if option.is_empty() || option.len() > 4096 || option.contains('\0') {
                return Err("dropdown option is empty, too large, or invalid".into());
            }
            target = semantic_target(&tab, Some((*x, *y)))?;
            select_at_point(&tab, *x, *y, option)?;
            "select"
        }
    };
    let after_url = real_url(&session.tab);
    blog(&format!(
        "user demonstration {action_name} on instance '{instance}' ({} -> {})",
        host_of(&before_url),
        host_of(&after_url)
    ));
    Ok(BrowserInteractionReceipt {
        action: action_name.to_string(),
        before_url,
        after_url,
        target,
        sensitive,
    })
}

fn validate_user_point(x: f64, y: f64) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x > 100_000.0 || y > 100_000.0 {
        return Err("browser coordinates are outside the safe viewport bound".to_string());
    }
    Ok(())
}

fn semantic_target(
    tab: &Tab,
    point: Option<(f64, f64)>,
) -> Result<Option<BrowserSemanticTarget>, String> {
    let element = point
        .map(|(x, y)| format!("document.elementFromPoint({x},{y})"))
        .unwrap_or_else(|| "document.activeElement".to_string());
    let script = format!(
        r#"(function(){{
          var e={element}; if(!e||e===document.body||e===document.documentElement)return 'null';
          var interactive=e.closest&&e.closest('input,textarea,select,button,a,[role],[contenteditable=true]');
          if(interactive)e=interactive;
          function clean(v){{return String(v||'').replace(/\s+/g,' ').trim().slice(0,240);}}
          function esc(v){{return String(v).replace(/[^a-zA-Z0-9_-]/g,function(c){{return '\\'+c;}});}}
          var selector='';
          if(e.hasAttribute('data-phx-vault-secret'))selector='[data-phx-vault-secret]';
          else if(e.id)selector='#'+esc(e.id);
          else if(e.getAttribute('data-testid'))selector='[data-testid="'+String(e.getAttribute('data-testid')).replace(/"/g,'\\"')+'"]';
          else if(e.getAttribute('aria-label'))selector=e.tagName.toLowerCase()+'[aria-label="'+String(e.getAttribute('aria-label')).replace(/"/g,'\\"')+'"]';
          else if(e.name)selector=e.tagName.toLowerCase()+'[name="'+String(e.name).replace(/"/g,'\\"')+'"]';
          else {{var p=[];var n=e;while(n&&n.nodeType===1&&p.length<6){{var s=n.tagName.toLowerCase();var i=1,q=n;while((q=q.previousElementSibling))if(q.tagName===n.tagName)i++;s+=':nth-of-type('+i+')';p.unshift(s);n=n.parentElement;}}selector=p.join('>');}}
          var label=e.getAttribute('aria-label')||'';
          if(!label&&e.labels&&e.labels.length)label=e.labels[0].innerText||e.labels[0].textContent||'';
          var signal=[e.type,e.autocomplete||e.getAttribute('autocomplete'),e.name,e.id,label,selector].join(' ').toLowerCase();
          var sensitive=e.hasAttribute('data-phx-vault-secret')||e.type==='password'||/(password|passwd|one-time-code|one time code|verification code|security code|otp|totp|2fa|mfa|secret|api key|token|cc-number|ccnumber|cardnumber|card-number|credit card|cc-csc|cvv|cvc)/.test(signal);
          return JSON.stringify({{tag:e.tagName.toLowerCase(),role:e.getAttribute('role')||'',input_type:e.type||'',autocomplete:e.autocomplete||e.getAttribute('autocomplete')||'',name:e.name||'',element_id:e.id||'',label:clean(label),text:sensitive?'':clean(e.innerText||e.textContent||e.value||''),selector:selector}});
        }})()"#
    );
    let raw = eval_string(tab, &script)
        .map_err(|error| format!("target inspection failed: {error:#}"))?;
    if raw == "null" || raw.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("target inspection returned invalid data: {error}"))
}

fn semantic_target_is_sensitive(target: &BrowserSemanticTarget) -> bool {
    if target.input_type.eq_ignore_ascii_case("password") {
        return true;
    }
    let signal = format!(
        "{} {} {} {} {} {}",
        target.autocomplete,
        target.name,
        target.element_id,
        target.label,
        target.input_type,
        target.selector
    )
    .to_ascii_lowercase();
    [
        "password",
        "passwd",
        "one-time-code",
        "one time code",
        "verification code",
        "security code",
        "otp",
        "totp",
        "2fa",
        "mfa",
        "secret",
        "api key",
        "api_key",
        "access token",
        "access_token",
        "cardnumber",
        "ccnumber",
        "credit card",
        "card-number",
        "cc-number",
        "cc-csc",
        "data-phx-vault-secret",
        "cvv",
        "cvc",
    ]
    .iter()
    .any(|needle| signal.contains(needle))
}

fn focus_active_editable(tab: &Tab, clear: bool) -> Result<(), String> {
    let script = format!(
        r#"(function(){{var e=document.activeElement;if(!e)return 'NO_ACTIVE';
        var editable=e.matches('input,textarea')||e.isContentEditable;if(!editable)return 'NOT_EDITABLE';
        e.focus();if({clear}){{if(e.matches('input,textarea'))e.select();else{{var r=document.createRange();r.selectNodeContents(e);var s=window.getSelection();s.removeAllRanges();s.addRange(r);}}}}return 'OK';}})()"#
    );
    match eval_string(tab, &script)
        .map_err(|error| format!("input focus failed: {error:#}"))?
        .as_str()
    {
        "NO_ACTIVE" => Err("there is no focused field to type into".to_string()),
        "NOT_EDITABLE" => Err("the focused element is not an editable field".to_string()),
        _ => Ok(()),
    }
}

fn select_at_point(tab: &Tab, x: f64, y: f64, option: &str) -> Result<(), String> {
    let option = js_string(option);
    let script = format!(
        r#"(function(){{var e=document.elementFromPoint({x},{y});if(e&&e.closest)e=e.closest('select');if(!e)return 'NOT_SELECT';
        var wanted={option};var i=[...e.options].findIndex(function(o){{return o.text.trim()===wanted||o.value===wanted;}});if(i<0)return 'NO_OPTION';
        var setter=Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype,'value').set;setter.call(e,e.options[i].value);e.selectedIndex=i;
        e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));return 'OK';}})()"#
    );
    match eval_string(tab, &script)
        .map_err(|error| format!("dropdown selection failed: {error:#}"))?
        .as_str()
    {
        "NOT_SELECT" => Err("the selected element is not a dropdown".to_string()),
        "NO_OPTION" => Err("the requested dropdown option is not present".to_string()),
        _ => Ok(()),
    }
}

struct Session {
    _browser: Browser,
    tab: Arc<Tab>,
    /// PID of a Chromium process launched and owned by Phoenix. Unlike
    /// `Browser::get_process_id`, this survives reconnecting a dead CDP socket
    /// through `Browser::connect`, which is essential for keeping the desktop's
    /// embedded X11 child identity stable through transport recovery.
    owned_pid: Option<u32>,
    /// Random WM_CLASS suffix supplied only to a native-surface launch. `None`
    /// on headless sessions and on browsers attached from outside Phoenix.
    surface_token: Option<String>,
    /// The durable coworker browser-profile id this session serves.
    /// Carried so tab swaps (`adopt_tab`) can re-arm the screencast with the
    /// right label for the canvas browser view.
    instance: String,
    /// The mode this Chrome was actually launched in. A managed session stays
    /// headless unless the desktop holds a validated in-app surface lease.
    /// Attached (user-owned) browsers are always visible.
    headless: bool,
    health: BrowserHealth,
    /// Ring of recent console messages + page exceptions (CDP events) — the
    /// agent reads these with `browser_console` to verify its own frontends.
    console: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    /// Set by a CDP `Inspector.targetCrashed` listener when the page's renderer
    /// crashes (browser-use's crash_watchdog). `execute()` checks it before each
    /// action and recovers the tab on the SAME browser — a renderer crash kills
    /// the tab, not the chrome process, so it never needs a full relaunch.
    crashed: Arc<std::sync::atomic::AtomicBool>,
    /// Stable element IDs (2026-07-02): `index_state` captures each indexed
    /// element's Chrome `backendNodeId` (stable across in-place re-renders) in
    /// ONE CDP `DOM.getFlattenedDocument` call, joined to the agent-visible
    /// `[index]` via the `data-phx-idx` attribute the indexer already sets.
    /// `browser_click` then drives a REAL trusted CDP click by backendNodeId,
    /// so an `[index]` survives in-place re-renders that detach the JS ref.
    /// The map always reflects the LATEST index pass (replaced wholesale each
    /// `browser_state`); a remounted node's dead id makes the CDP path fail
    /// and the JS fallback reports NOT_FOUND/STALE honestly. See
    /// `capture_backend_ids` / `resolve_backend_id` in actions.rs.
    backend_ids: std::sync::Mutex<std::collections::HashMap<u64, u32>>,
    /// Hash of the last page state attached to a tool result. When the fresh
    /// state is byte-identical, the attach collapses to a one-line "unchanged"
    /// receipt — re-feeding the same ~12KB block every action was a large,
    /// silent latency/token tax on multi-step turns. `browser_state` (an
    /// explicit look) always prints in full.
    last_state_hash: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone)]
struct BrowserHealth {
    mode: &'static str,
    attach: Option<String>,
    user_data_dir: Option<PathBuf>,
    binary: Option<PathBuf>,
    login_source: String,
    notes: Vec<String>,
}

/// Does this URL look like a login/sign-in wall? Deterministic and
/// conservative: well-known login path shapes, plus the `redirect_to=`
/// pattern sites use to bounce an unauthenticated visitor to their login.
fn is_login_wall_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some((_, rest)) = lower.split_once("://") else {
        return false;
    };
    let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = match location.split_once('/') {
        Some((_, p)) => format!("/{p}"),
        None => String::new(),
    };
    const WALLS: [&str; 8] = [
        "/login",
        "/signin",
        "/sign-in",
        "/sign_in",
        "/auth/login",
        "/session/new",
        "/users/sign_in",
        "/account/login",
    ];
    WALLS
        .iter()
        .any(|wall| path == *wall || path.starts_with(&format!("{wall}/")))
        || (!path.is_empty() && query.contains("redirect_to="))
}

/// Bare host of a URL ("https://www.discord.com:443/login" → "discord.com").
fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_string()
}

/// Update the (instance, host) login-wall streak for a post-action URL.
/// Returns the new sighting count when the URL IS a wall; a non-wall page on
/// the same host clears its streak (the login landed).
fn login_wall_streak(instance: &str, session_id: Option<&str>, url: &str) -> Option<u32> {
    let host = host_of(url);
    if host.is_empty() {
        return None;
    }
    let key = login_wall_key(instance, session_id, &host);
    match update_durable_login_wall(&key, &host, is_login_wall_url(url)) {
        Ok(count) => {
            let mut map = LOGIN_WALLS
                .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(count) = count {
                map.insert(key, count);
            } else {
                map.remove(&key);
            }
            count
        }
        Err(error) => {
            tracing::warn!("browser login-wall streak could not persist: {error:#}");
            let mut map = LOGIN_WALLS
                .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if is_login_wall_url(url) {
                let count = map.entry(key).or_insert(0);
                *count = count.saturating_add(1);
                Some(*count)
            } else {
                map.remove(&key);
                None
            }
        }
    }
}

fn login_wall_key(instance: &str, session_id: Option<&str>, host: &str) -> String {
    let identity = format!(
        "{}\0{}\0{host}",
        session_id.unwrap_or("no-session"),
        instance
    );
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

fn login_wall_store_path() -> PathBuf {
    crate::config::phoenix_home().join("browser/login-wall-streaks.json")
}

fn update_durable_login_wall(key: &str, host: &str, is_wall: bool) -> Result<Option<u32>> {
    let path = login_wall_store_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let mut store = match current {
            Some(bytes) => {
                anyhow::ensure!(
                    bytes.len() <= LOGIN_WALL_STORE_MAX_BYTES,
                    "login-wall streak store is unexpectedly large"
                );
                let store: DurableLoginWalls = serde_json::from_slice(bytes)?;
                anyhow::ensure!(
                    store.version == LOGIN_WALL_STORE_VERSION,
                    "login-wall streak store version mismatch"
                );
                store
            }
            None => DurableLoginWalls {
                version: LOGIN_WALL_STORE_VERSION,
                entries: std::collections::HashMap::new(),
            },
        };
        anyhow::ensure!(
            store.entries.len() <= LOGIN_WALL_STORE_MAX_ENTRIES,
            "login-wall streak store contains too many entries"
        );
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(LOGIN_WALL_STREAK_LIFETIME_HOURS);
        store.entries.retain(|_, entry| {
            entry.count > 0
                && entry.count <= 10_000
                && entry.host.len() <= 253
                && chrono::DateTime::parse_from_rfc3339(&entry.updated_at)
                    .map(|updated| updated.with_timezone(&chrono::Utc) >= cutoff)
                    .unwrap_or(false)
        });
        let count = if is_wall {
            if !store.entries.contains_key(key)
                && store.entries.len() >= LOGIN_WALL_STORE_MAX_ENTRIES
            {
                if let Some(oldest) = store
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.updated_at.as_str())
                    .map(|(key, _)| key.clone())
                {
                    store.entries.remove(&oldest);
                }
            }
            let entry = store
                .entries
                .entry(key.to_string())
                .or_insert_with(|| DurableLoginWall {
                    host: host.to_string(),
                    count: 0,
                    updated_at: now.to_rfc3339(),
                });
            anyhow::ensure!(entry.host == host, "login-wall streak identity mismatch");
            entry.count = entry.count.saturating_add(1).min(10_000);
            entry.updated_at = now.to_rfc3339();
            Some(entry.count)
        } else {
            store.entries.remove(key);
            None
        };
        let bytes = serde_json::to_vec(&store)?;
        anyhow::ensure!(
            bytes.len() <= LOGIN_WALL_STORE_MAX_BYTES,
            "login-wall streak store exceeded its bounded file limit"
        );
        Ok((count, bytes))
    })
}

/// Actions that (re)establish a page location — the only ones that count a
/// login-wall sighting. Passive looks (state/screenshot/console) at the same
/// page must not inflate the streak.
fn action_counts_wall(action: &str) -> bool {
    matches!(
        action,
        "navigate" | "go_back" | "click" | "switch" | "act" | "search"
    )
}

/// The classified login-loop escalation notice (sighting `count` ≥ the
/// threshold). This is the anti-blind-retry playbook: the wall is a STATE to
/// escalate, not an action to repeat.
fn login_wall_notice(count: u32, host: &str, restarts: u32) -> String {
    let crash_line = if restarts > 0 {
        format!(
            " This browser has restarted {restarts}× this job — a restart mid-login WIPES an \
             in-progress login, so the user may have done their part and lost it."
        )
    } else {
        String::new()
    };
    format!(
        "⚠ LOGIN LOOP — login wall sighting #{count} on {host} this job. The account is STILL \
         not logged in here. This count records page sightings, not password submissions; \
         it does not prove that a saved credential was filled or rejected. Classify and \
         escalate instead of retrying: (1) inspect fresh browser_state and call credential_list \
         for this host. If a matching saved credential exists, fill the CURRENT password field \
         with browser_input_credential, submit normally, and verify the signed-in destination; \
         do not ask the user for a password Phoenix already has. A pre-submit field/remount \
         failure is not a rejected password. (2) only when no matching credential exists or a \
         genuine user-only/passkey/MFA ceremony is required, call ask_for_login ONCE with \
         methods=[\"user_login\"]. Phoenix opens this \
         coworker's exact private profile inside the conversation pane; while the user acts, \
         touch NOTHING (no browser calls or relaunches). The turn resumes with \
         user_login_complete; then verify with one navigate. \
         (3) if the embedded login already ran and this wall is back, the login did NOT \
         stick.{crash_line} Report the observed failure accurately, preserve the unfinished \
         work, and reuse any pending login request. Do not open a generic ask_user login \
         card or repeat a dismissed request. A missing desktop bridge is a runtime failure, \
         not evidence of a rejected password. (4) never bounce back to a surface \
         you already established cannot deliver the data."
    )
}

/// Banner for a tab replaced after a renderer crash (same browser, logins
/// intact, page state gone).
const TAB_RECOVERY_NOTE: &str = "⚠ BROWSER ENVIRONMENT CHANGED: the page renderer CRASHED \
since your last action; the tab was replaced with a fresh one on the same browser (logins \
intact, that page's state gone). Your previous view is stale — re-navigate and re-verify \
before acting. If a user step (e.g. a login) was in progress on that page, it was \
interrupted and did NOT complete.";

/// Banner for a transport drop healed by reconnecting to the same chrome.
const RECONNECT_NOTE: &str = "ℹ BROWSER CONNECTION RECOVERED: the control connection \
dropped and was re-established to the same live browser. Tabs and logins survived, but \
verify the current page before assuming your last view is still current.";

/// Banner for a full relaunch — the loudest environment change there is.
fn relaunch_note(restarts: u32) -> String {
    let unstable = if restarts >= 2 {
        format!(
            "\nTreat this environment as UNSTABLE: the browser has now died {restarts} times \
             this job. Do NOT re-run the same plan expecting a different outcome — if a user \
             login handoff keeps getting destroyed, say exactly that to the user (ask_user) or \
             finalize honestly with the blocker instead of looping."
        )
    } else {
        String::new()
    };
    format!(
        "⚠ BROWSER ENVIRONMENT CHANGED (restart #{restarts} this job): the previous browser \
         DIED and a fresh one was launched — every tab and page is gone (logins persist only \
         through the profile). Anything the user was doing in a visible window (typing a \
         login, filling a form) was destroyed mid-action and did NOT complete. Do not repeat \
         your last step as if nothing happened: re-check state first and re-plan from what \
         is actually on screen now.{unstable}"
    )
}

/// Cap on retained console lines; oldest drop first.
mod actions;
mod design_preview;
pub(crate) use design_preview::capture_design_preview;
mod engine;
// ui_snap's isolated one-shot screenshot rides the engine's launch plumbing.
pub(crate) use engine::{atomic_write_artifact_output, snap_url};
mod handoff;
mod screencast;
mod session;
mod teaching_observer;

pub use screencast::{frames_subscribe, BrowserFrame};
pub use session::local_status_line;
pub(crate) use session::DESIGN_REVIEW_INSTANCE_SUFFIX;

use actions::*;
use engine::*;
use session::*;

const VOLUME_COOKIE_SNAPSHOT_BUDGET: Duration = Duration::from_secs(8);
const MAX_VOLUME_WORKER_COOKIES: usize = 50_000;

/// Bridge-down/headless fallback seeding is deliberately in-memory and
/// one-time. Normal workers attach independent tabs to the spawning agent's
/// live Electron session partition and must never overwrite it with an older
/// portable-cookie snapshot.
static VOLUME_WORKER_COOKIE_SEEDED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

#[derive(Clone, Copy)]
enum VolumeCookieSeedSource {
    LiveParentBrowser,
    ManagedProfileSnapshot,
}

impl VolumeCookieSeedSource {
    fn label(self) -> &'static str {
        match self {
            Self::LiveParentBrowser => "the spawning agent's live browser",
            Self::ManagedProfileSnapshot => "the spawning agent's saved browser profile",
        }
    }
}

struct VolumeCookieSeedReport {
    copied: usize,
    skipped_device_bound: usize,
    source: VolumeCookieSeedSource,
}

impl VolumeCookieSeedReport {
    fn note(&self) -> String {
        let skipped = if self.skipped_device_bound == 0 {
            String::new()
        } else {
            format!(
                " {} device-bound Google/Microsoft cookie(s) were deliberately not copied; sign in inside this private worker browser if that site is needed.",
                self.skipped_device_bound
            )
        };
        format!(
            "ℹ PRIVATE BROWSER SNAPSHOT: copied {} portable cookie(s) from {} into this worker's separate browser profile. Tabs, local storage, and the live browser process are not shared.{skipped}",
            self.copied,
            self.source.label(),
        )
    }
}

fn volume_worker_cookie_seeded(instance: &str) -> bool {
    VOLUME_WORKER_COOKIE_SEEDED
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|seeded| seeded.contains(instance))
        .unwrap_or(false)
}

fn mark_volume_worker_cookie_seeded(instance: &str) {
    let _ = VOLUME_WORKER_COOKIE_SEEDED
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|mut seeded| seeded.insert(instance.to_string()));
}

/// Called by the volume-worker cleanup guard even if the browser was never
/// launched. Keeping this separate from profile deletion lets cancellation
/// release all in-memory cookie-copy bookkeeping without serializing secrets.
pub(crate) fn clear_volume_worker_cookie_seed(instance: &str) {
    if let Some(seeded) = VOLUME_WORKER_COOKIE_SEEDED.get() {
        let _ = seeded.lock().map(|mut seeded| seeded.remove(instance));
    }
}

fn validate_volume_cookie_seed_pair(child: &str, parent: &str) -> Result<()> {
    anyhow::ensure!(
        is_disposable_browser_profile(child),
        "refusing to seed a non-disposable worker browser profile"
    );
    anyhow::ensure!(
        !parent.trim().is_empty()
            && parent.len() <= 128
            && parent
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "invalid spawning browser profile id"
    );
    anyhow::ensure!(child != parent, "worker browser profile cannot seed itself");
    Ok(())
}

fn live_cookie_to_param(cookie: Network::Cookie) -> Network::CookieParam {
    let is_host_prefix = cookie.name.starts_with("__Host-");
    let is_secure_prefix = is_host_prefix || cookie.name.starts_with("__Secure-");
    let domain = cookie.domain;
    let path = if is_host_prefix {
        "/".to_string()
    } else {
        cookie.path
    };
    let url = {
        let host = domain.trim_start_matches('.');
        (!host.is_empty()).then(|| format!("https://{host}{path}"))
    };
    Network::CookieParam {
        name: cookie.name,
        value: cookie.value,
        // A URL is required for host-only cookies. For ordinary cookies it
        // also keeps CDP from assigning the worker's current about:blank URL.
        url,
        domain: (!is_host_prefix).then_some(domain),
        path: Some(path),
        secure: Some(cookie.secure || is_secure_prefix),
        http_only: Some(cookie.http_only),
        same_site: cookie.same_site,
        expires: (!cookie.session && cookie.expires.is_finite() && cookie.expires >= 0.0)
            .then_some(cookie.expires),
        priority: Some(cookie.priority),
        same_party: Some(cookie.same_party),
        source_scheme: Some(cookie.source_scheme),
        source_port: Some(cookie.source_port),
        partition_key: cookie.partition_key,
    }
}

fn capture_live_parent_cookie_params(
    parent: &str,
) -> Result<Option<(Vec<Network::CookieParam>, usize)>> {
    let slot = session_slot(parent);
    let Some(guard) = try_lock_for(&slot, VOLUME_COOKIE_SNAPSHOT_BUDGET) else {
        // A parent browser action may legitimately be in flight. Fall back to
        // its safe on-disk profile snapshot instead of waiting forever or
        // reading a mutable profile through a second Chromium process.
        return Ok(None);
    };
    let Some(parent_session) = guard.as_ref() else {
        return Ok(None);
    };
    let tab = Arc::clone(&parent_session.tab);
    drop(guard);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = tab
            .call_method(Network::GetAllCookies(None))
            .map(|reply| reply.cookies)
            .map_err(|error| error.to_string());
        let _ = tx.send(outcome);
    });
    let cookies = match rx.recv_timeout(VOLUME_COOKIE_SNAPSHOT_BUDGET) {
        Ok(Ok(cookies)) => cookies,
        Ok(Err(error)) => anyhow::bail!("parent browser cookie snapshot failed: {error}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("parent browser cookie snapshot exceeded its bounded CDP read budget")
        }
        Err(error) => anyhow::bail!("parent browser cookie snapshot worker failed: {error}"),
    };
    anyhow::ensure!(
        cookies.len() <= MAX_VOLUME_WORKER_COOKIES,
        "parent browser exposes too many cookies to seed a disposable worker"
    );
    let mut skipped_device_bound = 0usize;
    let params = cookies
        .into_iter()
        .filter_map(|cookie| {
            if crate::tools::browser_cookies::is_session_bound_domain(&cookie.domain) {
                skipped_device_bound += 1;
                None
            } else {
                Some(live_cookie_to_param(cookie))
            }
        })
        .collect();
    Ok(Some((params, skipped_device_bound)))
}

fn managed_parent_cookie_params(parent: &str) -> Result<(Vec<Network::CookieParam>, usize)> {
    let profile = session::profile_dir_for_instance(parent, "phoenix")?;
    let cookies = crate::tools::chromium_cookies::read_managed_profile_cookies(&profile)?;
    anyhow::ensure!(
        cookies.len() <= MAX_VOLUME_WORKER_COOKIES,
        "parent browser profile has too many cookies to seed a disposable worker"
    );
    let mut skipped_device_bound = 0usize;
    let params = cookies
        .into_iter()
        .filter_map(|cookie| {
            if crate::tools::browser_cookies::is_session_bound_domain(&cookie.domain) {
                skipped_device_bound += 1;
                None
            } else {
                Some(to_cookie_param(cookie))
            }
        })
        .collect();
    Ok((params, skipped_device_bound))
}

fn inject_worker_cookie_snapshot(
    session: &Session,
    params: Vec<Network::CookieParam>,
) -> Result<()> {
    let tab = Arc::clone(&session.tab);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = if params.is_empty() {
            Ok(())
        } else {
            tab.call_method(Network::SetCookies { cookies: params })
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(VOLUME_COOKIE_SNAPSHOT_BUDGET) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => anyhow::bail!("worker cookie injection failed: {error}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("worker cookie injection exceeded its bounded CDP write budget")
        }
        Err(error) => anyhow::bail!("worker cookie injection worker failed: {error}"),
    }
}

fn seed_volume_worker_cookies_from_parent(
    child: &str,
    parent: &str,
    child_session: &Session,
) -> Result<Option<VolumeCookieSeedReport>> {
    validate_volume_cookie_seed_pair(child, parent)?;
    if volume_worker_cookie_seeded(child) {
        return Ok(None);
    }
    let (params, skipped_device_bound, source) = match capture_live_parent_cookie_params(parent) {
        Ok(Some((params, skipped))) => (params, skipped, VolumeCookieSeedSource::LiveParentBrowser),
        Ok(None) | Err(_) => {
            let (params, skipped) = managed_parent_cookie_params(parent)?;
            (
                params,
                skipped,
                VolumeCookieSeedSource::ManagedProfileSnapshot,
            )
        }
    };
    let copied = params.len();
    inject_worker_cookie_snapshot(child_session, params)?;
    let report = VolumeCookieSeedReport {
        copied,
        skipped_device_bound,
        source,
    };
    mark_volume_worker_cookie_seeded(child);
    Ok(Some(report))
}

/// Execute one `browser_*` tool call against the live session. `instance` is
/// None/"" for Phoenix's original profile; every coworker otherwise passes its
/// durable browser-profile id. Disposable workers retain their own tab/workspace
/// identity while using the spawning coworker's authenticated Electron session
/// partition when the desktop bridge is live. `session_id` is the main session
/// this browser serves — environment events (crashes, relaunches, login walls)
/// are journaled there for the watchers.
pub(crate) fn execute(
    tool_name: &str,
    input: Value,
    instance: Option<&str>,
    session_id: Option<&str>,
    credential_agent_id: Option<&str>,
    current_group_id: Option<&str>,
) -> Result<ToolOutput> {
    let action = tool_name
        .strip_prefix("browser_")
        .filter(|action| BROWSER_ACTIONS.contains(action))
        .with_context(|| format!("unknown browser tool: {tool_name}"))?;
    if shutdown_requested() {
        bail!("gateway browser lane is shutting down; do not start another browser action");
    }
    let instance = instance.unwrap_or("").to_string();
    let slot = session_slot(&instance);

    // Bounded acquisition: when a previous browser action is wedged past its
    // runtime timeout, its abandoned thread still holds this lock. Queueing
    // behind it forever would re-freeze every later browser call — fail fast
    // and honestly instead.
    let mut guard = try_lock_for(&slot, Duration::from_secs(10)).ok_or_else(|| {
        anyhow::anyhow!(
            "browser session is busy — a previous browser action is still running or wedged after a timeout. Do not retry in a tight loop; if this repeats, the session is stuck and the user should run `phoenix restart`."
        )
    })?;
    // The shutdown flag is set before the registry is drained. An action can
    // have obtained its Arc just before that drain and only acquire the slot
    // afterwards, so check again under the slot lock before it can launch a
    // replacement Chrome into a retiring gateway.
    if shutdown_requested() {
        bail!("gateway browser lane is shutting down; browser action cancelled before launch");
    }
    // Status is observational: inspecting an idle-suspended browser must not
    // spend hundreds of MB relaunching Chromium merely to say it is asleep.
    if action == "status" && guard.is_none() {
        if let Some(content) = suspended_status(&instance) {
            return Ok(ToolOutput {
                summary: "browser suspended".to_string(),
                content,
            });
        }
    }
    // Created after the slot guard so Drop refreshes last-used immediately
    // before the mutex unlocks. The reaper can never suspend a just-finished
    // long action based on its start timestamp.
    let _activity = BrowserActivity::begin(&instance);
    // The current coworker owns browser lifetime. This explicit action lets it release
    // its Chrome after securing the task result without a runtime policy
    // killing the browser at an arbitrary turn boundary.
    if action == "quit" {
        detach_native_surface(&instance);
        let content = if let Some(session) = guard.as_ref() {
            let attached = matches!(session.health.mode, "attach" | "embedded" | "reconnect");
            session.flush_and_close();
            *guard = None;
            if attached {
                "Disconnected Phoenix from the attached browser; the user's browser remains open."
                    .to_string()
            } else {
                "Closed this managed browser and flushed its profile. A later browser action can relaunch it with saved logins."
                    .to_string()
            }
        } else {
            let was_suspended = take_suspended(&instance).is_some();
            clear_restore_requested(&instance);
            if was_suspended {
                "This browser instance was suspended and remains closed. Saved logins persist, but its prior tabs will not auto-restore after this explicit quit."
                    .to_string()
            } else {
                "This browser instance is already closed.".to_string()
            }
        };
        return Ok(ToolOutput {
            summary: "browser closed".to_string(),
            content,
        });
    }
    // Environment-change banner for THIS call, set by any recovery path below
    // and prepended to the successful output. The old silent recoveries are
    // exactly how a browser died mid-login-handoff and the agent re-ran its
    // plan as if nothing happened (live 2026-07-06).
    let mut recovery_note: Option<String> = None;
    let mut cookie_seed_note: Option<String> = None;
    if guard.is_none() {
        if shutdown_requested() {
            bail!("gateway browser lane began shutting down before Chrome launch");
        }
        *guard = Some(Session::open(&instance)?);
        if let Some(parent_instance) =
            crate::tools::isolated_desktop::current_parent_browser_instance()
        {
            let child_session = guard.as_ref().expect("browser session just opened");
            if child_session.health.mode == "embedded" {
                cookie_seed_note = Some(format!(
                    "ℹ AUTH PROFILE INHERITED: this worker has independent tabs in {parent_instance}'s live authenticated browser partition, including cookies and site storage. Vault secrets remain brokered and never enter worker context."
                ));
            } else {
                match seed_volume_worker_cookies_from_parent(
                    &instance,
                    &parent_instance,
                    child_session,
                ) {
                    Ok(Some(report)) => cookie_seed_note = Some(report.note()),
                    Ok(None) => {}
                    Err(error) => {
                        // The bridge is unavailable, so the child is already a
                        // distinct headless process/profile. Keep it usable if
                        // portable fallback seeding fails, but make the missing
                        // auth state explicit instead of pretending it inherited.
                        cookie_seed_note = Some(format!(
                            "⚠ AUTH PROFILE FALLBACK UNAVAILABLE: Phoenix's embedded browser bridge was unavailable and this worker's headless fallback could not import the spawning agent's portable cookies. ({error:#})"
                        ));
                    }
                }
            }
        }
        if let Some(suspended) = take_suspended(&instance) {
            let note = format!(
                "ℹ BROWSER RESUMED: this coworker's private browser was cleanly suspended after {}s idle to save RAM. Chrome restored its durable profile and requested its prior {} tab{}; verify the current page before acting because a site may have redirected while idle.",
                suspended.idle_for.as_secs(),
                suspended.tabs,
                if suspended.tabs == 1 { "" } else { "s" }
            );
            journal_env(
                session_id,
                &format!(
                    "browser resumed after idle suspension ({}s idle)",
                    suspended.idle_for.as_secs()
                ),
            );
            recovery_note = Some(note);
        }
        // A session a FAILURE path dropped earlier relaunches here, lazily —
        // that is a restart the agent must hear about, not a first launch.
        if take_pending_relaunch(&instance) {
            let restarts = bump_restarts(&instance);
            journal_env(
                session_id,
                &format!("browser relaunched after an earlier failure (restart #{restarts}) — tabs/page state lost"),
            );
            recovery_note = Some(relaunch_note(restarts));
        }
    }
    if action == "import_cookies" {
        let source = input
            .get("source")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("browser_import_cookies needs `source`")?;
        let site = input
            .get("site")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("browser_import_cookies needs `site`")?;
        let requested_scope = input
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("agent");
        let agent_id = credential_agent_id
            .map(str::to_string)
            .map(Ok)
            .unwrap_or_else(|| {
                crate::tools::browser_cookie_grants::agent_id_for_profile(&instance)
            })?;
        let scope = crate::tools::browser_cookie_grants::authorized_scope(
            requested_scope,
            &agent_id,
            current_group_id,
        )?;
        let session = guard.as_ref().context("browser session did not open")?;
        let (refreshed, skipped) = session.import_site_cookies(source, site)?;
        let grant = crate::tools::browser_cookie_grants::add(source, site, scope, &agent_id)?;
        return Ok(ToolOutput {
            summary: format!("imported {refreshed} cookies for {}", grant.site),
            content: format!(
                "Imported {refreshed} portable cookies for {} into {}'s private browser and saved grant {} with {:?} scope. {skipped} device-bound cookies were skipped; use login handoff if that site still requires authentication.",
                grant.site, agent_id, grant.grant_id, grant.scope
            ),
        });
    }

    // Crash watchdog (browser-use): if the renderer crashed since the last call,
    // swap to a fresh tab on the SAME browser before acting — a renderer crash
    // kills the tab, not the chrome process, so it never needs a full relaunch.
    // (Done via `guard` directly so we can respawn without an outstanding borrow.)
    let crashed = guard
        .as_ref()
        .is_some_and(|s| s.crashed.load(std::sync::atomic::Ordering::SeqCst));
    if crashed {
        tracing::warn!("browser: recovering from a renderer crash before next action");
        let recovered = guard
            .as_mut()
            .map(|s| s.recover_tab().is_ok())
            .unwrap_or(false);
        if !recovered {
            // The whole browser is gone (not just the tab) — respawn cleanly.
            // Flush first in case the process is still alive enough to (a crash
            // can take the renderer while the browser process lingers).
            if let Some(session) = guard.as_ref() {
                session.flush_and_close();
            }
            *guard = None;
            if shutdown_requested() {
                bail!("gateway browser lane began shutting down during crash recovery");
            }
            *guard = Some(Session::open(&instance)?);
            let restarts = bump_restarts(&instance);
            journal_env(
                session_id,
                &format!("browser crashed and was relaunched (restart #{restarts}) — tabs/page state lost"),
            );
            recovery_note = Some(relaunch_note(restarts));
        } else {
            journal_env(
                session_id,
                "page renderer crashed — tab replaced on the same browser",
            );
            recovery_note = Some(TAB_RECOVERY_NOTE.to_string());
        }
    }
    let session = guard.as_mut().expect("session just ensured");

    // Resolve stored secrets only after the browser is healthy, keep them in
    // a zeroizing vault object for the bounded action, and never add them to
    // the model-authored input or tool output. Site binding prevents a stolen
    // credential id from being filled into an unrelated origin.
    let revealed_credential = if action == "input_credential" {
        let credential_id = input
            .get("credential_id")
            .and_then(Value::as_str)
            .context("browser_input_credential needs `credential_id`")?;
        let agent_id = credential_agent_id
            .map(str::to_string)
            .map(Ok)
            .unwrap_or_else(|| {
                crate::tools::browser_cookie_grants::agent_id_for_profile(&instance)
            })?;
        let scopes = crate::tools::browser_cookie_grants::visible_credential_scopes(
            &agent_id,
            current_group_id,
        )?;
        let credential = crate::security::vault::Vault::open_default()
            .reveal_for_agent(credential_id, &scopes)
            .context("stored credential is unavailable")?;
        validate_credential_origin(session, &credential)?;
        Some(credential)
    } else {
        None
    };

    let native_recovery_url = native_surface_attached(&instance)
        .then(|| session.tab.get_url())
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"));
    let result = match attempt_action(session, action, &input, revealed_credential.as_ref()) {
        Ok(output) => Ok(output),
        Err(error) if is_navigate_wedge(&error) => {
            // keep_alive (browser-use doctrine): a slow navigate must NOT tear the
            // browser down — killing chrome over one slow navigation is exactly the
            // "browser randomly closes and reopens, cancelling all progress" the
            // user hit. headless_chrome is synchronous, so the wedged navigate's
            // worker thread can't be cancelled and keeps interfering with the same
            // tab. The robust fix within this engine: swap to a FRESH tab on the
            // SAME browser — cookies/logins survive (the cookie jar is browser-
            // global), the wedged tab + its abandoned worker are left behind and
            // closed off-thread. The agent recovers with a clean tab on the next
            // call. Only a genuinely dead transport (next arm) respawns the browser.
            if let Err(recover_err) = session.recover_tab() {
                tracing::warn!(
                    "browser: fresh-tab recovery after wedge failed ({recover_err:#}); \
                     transport likely dead, dropping session for a clean respawn"
                );
                session.flush_and_close();
                *guard = None;
                mark_pending_relaunch(&instance);
                if native_surface_attached(&instance) && !shutdown_requested() {
                    let fresh = Session::open(&instance)?;
                    if let Some(url) = native_recovery_url.as_deref() {
                        let _ = fresh.tab.navigate_to(url);
                    }
                    *guard = Some(fresh);
                }
            }
            Err(error)
        }
        Err(error) if is_transport_dead(&error) => {
            // The CDP websocket died (closed channel, idle-stale socket, tab gone).
            // FIRST try to RECONNECT to the same chrome — the process is usually
            // still alive (the live forensic capture: chrome pid stayed up across a
            // 17-min idle, only the socket went stale). Reconnecting keeps the
            // user's page/tabs/cookies — no "browser died and went back to twitter".
            // The dead Session is `mem::forget`-ten so its Drop can't SIGKILL the
            // chrome we just reconnected to. Only if no live chrome is reachable do
            // we relaunch (the original behavior — guaranteed fallback).
            let profile = session.health.user_data_dir.clone();
            let prev_target = session.tab.get_target_id().to_string();
            let was_headless = session.headless;
            let prev_pid = session
                .owned_pid
                .or_else(|| session._browser.get_process_id());
            let surface_token = session.surface_token.clone();
            match chrome_pid_alive(prev_pid) {
                Some(true) => blog(&format!(
                    "transport dead on '{action}' but chrome pid {} is ALIVE — socket-only death, reconnecting to keep the page",
                    prev_pid.unwrap_or_default()
                )),
                Some(false) => blog(&format!(
                    "transport dead on '{action}' and chrome pid {} is GONE — the process crashed or was killed externally; relaunch unavoidable",
                    prev_pid.unwrap_or_default()
                )),
                None => blog(&format!(
                    "transport dead on '{action}' (attach/reconnect mode, no owned pid) — probing for a live chrome"
                )),
            }
            let reconnected = profile.as_deref().and_then(|p| {
                reconnect_to_live_chrome(
                    p,
                    Some(&prev_target),
                    was_headless,
                    prev_pid,
                    surface_token,
                )
            });
            let mut reconnect_attempt: Option<Result<ToolOutput>> = None;
            if let Some(mut fresh) = reconnected {
                if let Some(dead) = guard.take() {
                    std::mem::forget(dead);
                }
                fresh.instance = instance.clone();
                teaching_observer::arm(&fresh.tab, &instance);
                screencast::arm(&fresh.tab, &instance);
                *guard = Some(fresh);
                let session = guard.as_mut().expect("reconnected session");
                tracing::info!("browser: reconnected to the live chrome after a transport drop");
                blog("reconnected to the live chrome after a transport drop — page/tabs kept");
                reconnect_attempt = Some(attempt_action(
                    session,
                    action,
                    &input,
                    revealed_credential.as_ref(),
                ));
            }
            match reconnect_attempt {
                Some(Ok(output)) => {
                    recovery_note = Some(RECONNECT_NOTE.to_string());
                    Ok(output)
                }
                // A live reconnect followed by a normal page/element error is
                // NOT a dead browser. Preserve Chrome and return the useful
                // stale/remount error so the agent can refresh state. Treating
                // every retry error as transport death caused needless full
                // relaunches and destroyed login-page progress.
                Some(Err(retry_err)) if !is_transport_dead(&retry_err) => Err(retry_err),
                Some(Err(_)) | None => {
                    if let Some(session) = guard.as_ref() {
                        session.flush_and_close();
                    }
                    *guard = None;
                    if shutdown_requested() {
                        bail!("gateway browser lane began shutting down during transport recovery");
                    }
                    let fresh = Session::open(&instance)?;
                    *guard = Some(fresh);
                    let restarts = bump_restarts(&instance);
                    journal_env(
                        session_id,
                        &format!("browser died and was relaunched (restart #{restarts}) — tabs/page state lost"),
                    );
                    recovery_note = Some(relaunch_note(restarts));
                    if action == "input_credential" {
                        // A clean browser has a different document and element
                        // index space. Never replay a secret-bearing action into
                        // it automatically, even when a same-site startup page
                        // happens to appear. The next model step must observe
                        // the new page and choose its fresh field explicitly.
                        return Err(anyhow::anyhow!(
                            "browser restarted before credential input could be confirmed; the secret was not replayed. Reopen the login page, get fresh browser_state, and retry the saved credential once"
                        ));
                    }
                    let session = guard.as_mut().expect("session just ensured");
                    match attempt_action(session, action, &input, revealed_credential.as_ref()) {
                        Ok(output) => Ok(output),
                        Err(retry_err) => {
                            if is_transport_dead(&retry_err) {
                                *guard = None;
                                mark_pending_relaunch(&instance);
                            }
                            Err(retry_err)
                        }
                    }
                }
            }
        }
        Err(error) => {
            // A recoverable action error — element not found, not
            // interactable, a stale index, a JS eval error, an element-wait
            // timeout. The BROWSER IS FINE. Keep it alive and hand the error
            // back to the agent to recover (re-read state, scroll, retry).
            // Killing the whole session here was the old "randomly closes"
            // instability; browser-use keeps the browser up across these.
            Err(error)
        }
    };

    // Post-flight: environment banners lead the output, then the login-wall
    // classifier — a location-establishing action that lands on a login wall
    // bumps the (instance, host) streak; a non-wall page clears it. From the
    // escalation threshold on, the classified playbook rides EVERY sighting.
    let mut output = result?;
    let wall_notice = guard.as_ref().and_then(|session| {
        if !action_counts_wall(action) {
            return None;
        }
        let url = real_url(&session.tab);
        let count = login_wall_streak(&instance, session_id, &url)?;
        if count < LOGIN_WALL_ESCALATE {
            return None;
        }
        let host = host_of(&url);
        if let Some(sid) = session_id {
            crate::runtime::journal::record(
                sid,
                "login-wall",
                "browser",
                &format!("sighting #{count} on {host} — account still logged out"),
            );
        }
        Some(login_wall_notice(count, &host, restarts_of(&instance)))
    });
    let mut lead = String::new();
    if let Some(note) = recovery_note {
        lead.push_str(&note);
        lead.push_str("\n\n");
    }
    if let Some(note) = cookie_seed_note {
        lead.push_str(&note);
        lead.push_str("\n\n");
    }
    if let Some(wall) = wall_notice {
        lead.push_str(&wall);
        lead.push_str("\n\n");
    }
    if !lead.is_empty() {
        output.content = format!("{lead}{}", output.content);
    }
    Ok(output)
}

fn attempt_action(
    session: &mut Session,
    action: &str,
    input: &Value,
    credential: Option<&crate::security::vault::RevealedCredential>,
) -> Result<ToolOutput> {
    if action == "input_credential" {
        let index = input
            .get("index")
            .and_then(Value::as_i64)
            .context("browser_input_credential needs an element `index`")?;
        let credential = credential.context("stored credential was not resolved")?;
        // Readiness may await a compositor frame. Re-check the secret's origin
        // after that wait, immediately before the existing field-fill path.
        session.prepare_input()?;
        // Re-check the live origin on every attempt, including a reconnect
        // retry. The cached URL used before a transport failure is not an
        // authorization to type a secret into whatever page is active now.
        validate_credential_origin(session, credential)?;
        let field = input.get("field").and_then(Value::as_str);
        let value = crate::tools::passes::resolve_field(credential, field)?;
        actions::input_secret(session, index, value.as_str())?;
        let label = field.unwrap_or(crate::security::vault::primary_field(&credential.metadata.kind));
        return finish_action(
            session,
            format!(
                "Filled the `{label}` of pass `{}` into [{index}] without exposing it.",
                credential.metadata.credential_id
            ),
            true,
            false,
        );
    }
    let (output, attach_state) = run_action(session, action, input)?;
    finish_action(session, output, attach_state, action == "state")
}

fn validate_credential_origin(
    session: &Session,
    credential: &crate::security::vault::RevealedCredential,
) -> Result<()> {
    // Ask the live document first. tab.get_url() is event-backed cache and can
    // lag across redirects/reconnects, exactly where secret binding matters.
    let live_url = eval_string(&session.tab, "location.href")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| real_url(&session.tab));
    let current_host = credential_host_for_url(&live_url)?;
    // Cards and one-time codes saved without a site are meant for whichever
    // checkout or verification page the task is on. Purchases stay behind
    // the separate purchase approval gate.
    if crate::security::vault::fillable_anywhere(&credential.metadata) {
        return Ok(());
    }
    anyhow::ensure!(
        crate::tools::browser_cookie_grants::domain_matches_site(
            &current_host,
            &credential.metadata.site,
        ),
        "credential `{}` is bound to {}; refusing to fill it on {}",
        credential.metadata.credential_id,
        credential.metadata.site,
        current_host
    );
    Ok(())
}

fn credential_host_for_url(page_url: &str) -> Result<String> {
    url::Url::parse(page_url)
        .ok()
        .and_then(|url| {
            matches!(url.scheme(), "http" | "https")
                .then(|| url.host_str().map(str::to_ascii_lowercase))
                .flatten()
        })
        .context("current browser page has no safe HTTP(S) site for credential binding")
}

/// Append fresh page state to a successful action's output. Identical state
/// collapses to a one-line receipt (`force_full_state` = the explicit
/// `browser_state` look, which always prints everything).
fn finish_action(
    session: &Session,
    output: String,
    attach_state: bool,
    force_full_state: bool,
) -> Result<ToolOutput> {
    let mut content = output.clone();
    if attach_state {
        let state = index_state(session).with_context(|| {
            format!(
                "browser action completed, but Phoenix could not read fresh browser state. The CDP session is unhealthy.\n{}",
                browser_diagnostics(session)
            )
        })?;
        if state.trim().is_empty() {
            bail!(
                "browser action completed, but fresh browser state was empty. The CDP session is unhealthy.\n{}",
                browser_diagnostics(session)
            );
        }
        let hash = state_signature(&state);
        let previous = session
            .last_state_hash
            .swap(hash, std::sync::atomic::Ordering::SeqCst);
        if previous == hash && !force_full_state {
            let first = state.lines().take(2).collect::<Vec<_>>().join(" · ");
            content.push_str(&format!(
                "\n\n=== BROWSER STATE unchanged since your last look ({first}) — same URL, same interactive elements; the indexes you have are still valid (visible text may have ticked). Call browser_state to re-print in full ===",
            ));
        } else {
            content.push_str("\n\n=== BROWSER STATE (current page) ===\n");
            content.push_str(&state);
        }
    }
    let summary = if output.trim().is_empty() {
        "browser state".to_string()
    } else {
        first_line(&output)
    };
    Ok(ToolOutput { summary, content })
}

/// Structural signature of a page state: the URL plus each indexed element's
/// identity (index + opening tag with attributes) — NOT the free text. A
/// byte-identity hash of the whole state never fired on live sites: a ticking
/// timestamp, a view counter, or a rotating ad reflowed the full ~12KB block
/// after every action even though every index the agent holds was still
/// valid. Element text changing in place doesn't invalidate an index; a
/// structural change (element added/removed/re-attributed, URL moved) does —
/// so that is exactly what the signature covers. Typed input still registers:
/// the indexer serializes `value=` into the attributes.
fn state_signature(state: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in state.lines() {
        if line.starts_with("URL:") {
            line.hash(&mut hasher);
        } else if line.starts_with('[') {
            // `[12]<button type="submit">Post</button> (below)` → hash only
            // through the opening tag: index, tag, attributes. The bare
            // `[Start of page]` / `[End of page]` boundary lines have no tag
            // and hash whole — reaching the top/bottom counts as a change.
            match line.find('>') {
                Some(end) => line[..=end].hash(&mut hasher),
                None => line.hash(&mut hasher),
            }
        }
    }
    hasher.finish()
}

fn browser_diagnostics(session: &Session) -> String {
    let url = real_url(&session.tab);
    let tab_count = session
        ._browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.len().to_string())
        .unwrap_or_else(|_| "unknown (tabs lock poisoned)".to_string());
    let mut lines = vec![
        format!("Browser mode: {}", session.health.mode),
        format!("Current URL: {url}"),
        format!("Open tabs: {tab_count}"),
    ];
    if let Some(attach) = &session.health.attach {
        lines.push(format!("Attached CDP endpoint: {attach}"));
    }
    if let Some(dir) = &session.health.user_data_dir {
        lines.push(format!("Profile dir: {}", dir.display()));
    }
    if let Some(binary) = &session.health.binary {
        lines.push(format!("Binary: {}", binary.display()));
    }
    if !session.health.login_source.trim().is_empty() {
        lines.push(format!("Login source: {}", session.health.login_source));
    }
    if let Some(dir) = &session.health.user_data_dir {
        if let Some(holders) = profile_holders(dir) {
            lines.push(format!("Profile holders: {holders}"));
        }
    }
    for note in &session.health.notes {
        lines.push(format!("Note: {note}"));
    }
    lines.join("\n")
}

/// A navigate that blew its bounded budget (the page never settled). The
/// session is suspect; recovery is drop-without-retry.

/// Best-effort close of every browser instance on CLI exit.
pub fn shutdown() {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
    if let Some(surfaces) = BROWSER_SURFACES.get() {
        let _ = surfaces.lock().map(|mut surfaces| surfaces.clear());
    }
    let deadline = std::time::Instant::now() + SHUTDOWN_GLOBAL_BUDGET;
    engine::signal_phoenix_profile_holders_for_shutdown();
    let Some(registry) = SESSIONS.get() else {
        handoff::shutdown(deadline);
        engine::force_close_phoenix_profile_holders(deadline);
        #[cfg(test)]
        SHUTDOWN_REQUESTED.store(false, std::sync::atomic::Ordering::Release);
        return;
    };
    let slots: Vec<_> = {
        let lock_deadline = deadline.min(std::time::Instant::now() + SHUTDOWN_LOCK_BUDGET);
        let Some(mut map) = try_lock_until(registry, lock_deadline) else {
            blog(&format!(
                "shutdown: browser registry stayed busy for {:.1}s; skipping graceful browser cleanup so gateway shutdown remains bounded (latest browser storage may not be persisted)",
                SHUTDOWN_LOCK_BUDGET.as_secs_f32()
            ));
            handoff::shutdown(deadline);
            engine::force_close_phoenix_profile_holders(deadline);
            #[cfg(test)]
            SHUTDOWN_REQUESTED.store(false, std::sync::atomic::Ordering::Release);
            return;
        };
        map.drain().collect()
    };
    blog(&format!(
        "shutdown: draining {} browser instance(s) in parallel",
        slots.len()
    ));
    // All owned browsers share one bounded graceful-close deadline. Acquire
    // and flush them in parallel so daemon shutdown is bounded by the slowest
    // instance rather than either a wedged action or a timeout multiplied by
    // the number of coworker profiles.
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(slots.len() + 1);
        workers.push(scope.spawn(move || handoff::shutdown(deadline)));
        for (instance, slot) in slots {
            workers.push(scope.spawn(move || {
                let label = if instance.is_empty() {
                    "main".to_string()
                } else {
                    instance
                };
                let lock_deadline =
                    deadline.min(std::time::Instant::now() + SHUTDOWN_LOCK_BUDGET);
                let Some(mut guard) = try_lock_until(&slot, lock_deadline) else {
                    blog(&format!(
                        "shutdown: browser instance '{label}' stayed busy for {:.1}s; skipping its graceful profile flush so gateway shutdown remains bounded (latest browser storage may not be persisted)",
                        SHUTDOWN_LOCK_BUDGET.as_secs_f32()
                    ));
                    return;
                };
                // Clean-close on exit so the last session's logins flush to the
                // profile and are still there next Phoenix run.
                if let Some(session) = guard.take() {
                    session.flush_and_close_until(deadline);
                    // Browser::drop performs a synchronous CDP close before it
                    // reaps Chrome. A poisoned transport can make that close
                    // outlive our global deadline even though the OS process
                    // was already signalled above. The gateway is exiting, so
                    // production intentionally leaves the Rust wrapper for OS
                    // teardown; the final exact-profile pidfd sweep below is
                    // the process-level no-overlap fence.
                    #[cfg(not(test))]
                    std::mem::forget(session);
                    // Browser tests reuse this process after shutdown and need
                    // normal destructor cleanup once their Chrome has exited.
                    #[cfg(test)]
                    drop(session);
                }
            }));
        }
        for worker in workers {
            if worker.join().is_err() {
                blog(
                    "shutdown: a browser cleanup worker panicked; continuing gateway shutdown (that profile's final storage flush is not guaranteed)",
                );
            }
        }
    });
    engine::force_close_phoenix_profile_holders(deadline);
    blog("shutdown: bounded browser cleanup pass finished");
    #[cfg(test)]
    SHUTDOWN_REQUESTED.store(false, std::sync::atomic::Ordering::Release);
}

/// Poll a standard mutex for at most `budget`, recovering poisoned state but
/// never queueing forever behind an abandoned synchronous browser action.
fn try_lock_for<T>(mutex: &Mutex<T>, budget: Duration) -> Option<std::sync::MutexGuard<'_, T>> {
    try_lock_until(mutex, std::time::Instant::now() + budget)
}

fn try_lock_until<T>(
    mutex: &Mutex<T>,
    deadline: std::time::Instant,
) -> Option<std::sync::MutexGuard<'_, T>> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                return Some(poisoned.into_inner());
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return None;
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
            }
        }
    }
}

#[cfg(test)]
#[path = "../browser_native_tests.rs"]
mod browser_native_tests;
