//! Per-agent X11 desktops.
//!
//! Phoenix used to have one optional isolated display for the whole gateway.
//! That is not an isolation boundary: two concurrent coworkers still steal
//! focus, windows, and pointer state from each other.  This module owns a
//! small nested X server for *each canonical agent scope*. The desktop stays
//! headless by default so agent-owned PDF viewers and helper applications do
//! not escape over Phoenix. A visible Xephyr window is available only through
//! an explicit developer opt-in; user-facing browser work belongs in Canvas's
//! right sidebar.
//!
//! The important design constraint is that `DISPLAY` is never changed in the
//! Phoenix process.  A scope is installed in thread-local state for one tool
//! call and every subprocess receives an explicit environment map instead.
//! That makes simultaneous tool calls genuinely independent.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(unix)]
mod gnome;

// Managed desktops are shown inside Phoenix's inspection workspace and are
// frequently used for dense IDE/Blender/browser UIs. 1440x960 made text and
// controls visibly soft once the capture filled a wide inspection panel.
// 1920x1200 keeps the useful 16:10 vertical workspace while remaining a
// moderate ~2.3 MP headless surface.
const DEFAULT_WIDTH: u32 = 1920;
const DEFAULT_HEIGHT: u32 = 1200;
const STARTUP_BUDGET: Duration = Duration::from_secs(6);
const READY_POLL: Duration = Duration::from_millis(50);
const EPHEMERAL_IDLE: Duration = Duration::from_secs(90);
const DURABLE_IDLE: Duration = Duration::from_secs(15 * 60);

/// One stable desktop identity.  `key` is deliberately hash-derived rather
/// than a sanitized session id, so two user-entered ids cannot collide after
/// punctuation is removed.  It is also safe as a directory component and as
/// a `PHOENIX_DESKTOP_SCOPE` value.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DesktopScope {
    key: String,
    label: String,
    #[serde(default)]
    owner_agent_id: String,
    ephemeral: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    browser_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_browser_instance: Option<String>,
}

impl DesktopScope {
    /// A durable scope for an ordinary Phoenix/coworker conversation.  A
    /// parallel job gets a distinct scope by supplying `job_scope`.
    pub fn agent(session_id: &str, agent_id: &str, job_scope: Option<&str>) -> Result<Self> {
        Self::new(session_id, agent_id, job_scope, job_scope.is_some())
    }

    /// An anonymous volume worker is always disposable.  Its display dies on
    /// completion, cancellation, and idle reaping; it can never become a
    /// durable coworker desktop by accident.
    pub fn worker(session_id: &str, parent_agent_id: &str, worker_scope: &str) -> Result<Self> {
        Self::new(session_id, parent_agent_id, Some(worker_scope), true)
    }

    fn new(
        session_id: &str,
        agent_id: &str,
        worker_or_job_scope: Option<&str>,
        ephemeral: bool,
    ) -> Result<Self> {
        validate_component(session_id, "desktop session id")?;
        validate_component(agent_id, "desktop agent id")?;
        if let Some(scope) = worker_or_job_scope {
            validate_component(scope, "desktop worker scope")?;
        }
        let canonical = format!(
            "session={session_id}\u{0}agent={agent_id}\u{0}scope={}",
            worker_or_job_scope.unwrap_or("canonical")
        );
        let digest = Sha256::digest(canonical.as_bytes());
        let suffix = digest
            .iter()
            .take(12)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let label = compact_label(agent_id, worker_or_job_scope);
        Ok(Self {
            key: format!("desktop-{suffix}"),
            label,
            owner_agent_id: agent_id.to_string(),
            ephemeral,
            browser_instance: None,
            parent_browser_instance: None,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    pub fn browser_instance(&self) -> Option<&str> {
        self.browser_instance.as_deref()
    }

    pub fn parent_browser_instance(&self) -> Option<&str> {
        self.parent_browser_instance.as_deref()
    }

    pub fn with_browser_instance(mut self, instance: Option<impl Into<String>>) -> Self {
        // A canonical coworker owns an isolated desktop for native apps and
        // terminal work, but its browser is the shared Electron surface shown
        // in that coworker's right sidebar. Only disposable parallel/volume
        // scopes own a separate browser process on their private X server.
        self.browser_instance = self
            .ephemeral
            .then(|| instance)
            .flatten()
            .map(Into::into)
            .filter(|value| !value.trim().is_empty());
        self
    }

    pub fn with_parent_browser_instance(mut self, instance: Option<impl Into<String>>) -> Self {
        self.parent_browser_instance = instance
            .map(Into::into)
            .filter(|value| !value.trim().is_empty());
        self
    }
}

fn validate_component(value: &str, name: &str) -> Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.len() <= 4_096
            && !value.chars().any(|character| character.is_control()),
        "{name} must be non-empty, at most 4096 bytes, and contain no control characters"
    );
    Ok(())
}

fn compact_label(agent: &str, scope: Option<&str>) -> String {
    let compact = |value: &str| {
        let value = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .take(32)
            .collect::<String>();
        if value.is_empty() {
            "agent".to_string()
        } else {
            value
        }
    };
    match scope {
        Some(scope) => format!("{} · {}", compact(agent), compact(scope)),
        None => compact(agent),
    }
}

/// Environment sent to every subprocess belonging to one agent.  The
/// Xauthority cookie and runtime directory are per scope, so X clients cannot
/// accidentally attach to another agent's display merely by inheriting the
/// gateway environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopEnvironment {
    pub display: String,
    pub xauthority: PathBuf,
    pub runtime_dir: PathBuf,
    pub scope_key: String,
    pub visible: bool,
    pub native: Option<NativeDesktopEnvironment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDesktopEnvironment {
    pub wayland_display: String,
    pub session_bus: String,
}

impl DesktopEnvironment {
    /// Apply the isolated desktop without touching the process-global
    /// environment.  `WAYLAND_DISPLAY` is explicitly removed so GTK/Qt and
    /// Chromium cannot bypass Xephyr and land on the user's real Wayland
    /// desktop.
    pub fn apply_to_command(&self, command: &mut Command) {
        command
            .env("DISPLAY", &self.display)
            .env("XAUTHORITY", &self.xauthority)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("PHOENIX_DESKTOP_SCOPE", &self.scope_key)
            .env("XDG_SESSION_TYPE", "x11")
            .env("GDK_BACKEND", "x11")
            .env("QT_QPA_PLATFORM", "xcb")
            .env("G_APPLICATION_NON_UNIQUE", "1")
            .env_remove("WAYLAND_DISPLAY")
            // Host-session activation can make `gio launch`/a GTK app join an
            // existing single-instance process, whose window is on the real
            // desktop. Scoped commands deliberately get no host DBus bus.
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("DBUS_STARTER_ADDRESS")
            .env_remove("DBUS_STARTER_BUS_TYPE")
            .env_remove("XDG_ACTIVATION_TOKEN")
            .env_remove("DESKTOP_STARTUP_ID");
        if let Some(native) = &self.native {
            command.env("WAYLAND_DISPLAY", &native.wayland_display)
                .env("DBUS_SESSION_BUS_ADDRESS", &native.session_bus)
                .env("XDG_SESSION_TYPE", "wayland")
                .env("GDK_BACKEND", "wayland,x11")
                .env("QT_QPA_PLATFORM", "wayland;xcb");
        }
    }

    /// `headless_chrome` receives a map instead of a mutable `Command`.
    /// Empty `WAYLAND_DISPLAY` plus the Chromium X11 flag prevents inherited
    /// Wayland discovery from selecting the host compositor.
    pub fn browser_process_env(&self) -> HashMap<String, String> {
        HashMap::from([
            ("DISPLAY".to_string(), self.display.clone()),
            (
                "XAUTHORITY".to_string(),
                self.xauthority.to_string_lossy().into_owned(),
            ),
            (
                "XDG_RUNTIME_DIR".to_string(),
                self.runtime_dir.to_string_lossy().into_owned(),
            ),
            ("PHOENIX_DESKTOP_SCOPE".to_string(), self.scope_key.clone()),
            ("XDG_SESSION_TYPE".to_string(), "x11".to_string()),
            ("GDK_BACKEND".to_string(), "x11".to_string()),
            ("QT_QPA_PLATFORM".to_string(), "xcb".to_string()),
            ("G_APPLICATION_NON_UNIQUE".to_string(), "1".to_string()),
            ("WAYLAND_DISPLAY".to_string(), String::new()),
            ("DBUS_SESSION_BUS_ADDRESS".to_string(), String::new()),
            ("DBUS_STARTER_ADDRESS".to_string(), String::new()),
            ("DBUS_STARTER_BUS_TYPE".to_string(), String::new()),
            ("XDG_ACTIVATION_TOKEN".to_string(), String::new()),
            ("DESKTOP_STARTUP_ID".to_string(), String::new()),
        ])
    }
}

struct DesktopProcess {
    child: Child,
    label: &'static str,
}

struct DesktopState {
    label: String,
    owner_agent_id: String,
    observation: Option<DesktopObservationRecord>,
    environment: DesktopEnvironment,
    root: PathBuf,
    processes: Vec<DesktopProcess>,
    ephemeral: bool,
    browser_instance: Option<String>,
    last_used: Instant,
}

/// A turn-lifetime claim on a desktop scope.  The display itself is lazily
/// created on the first GUI/terminal/browser command, but an active model turn
/// keeps its claimed scope from idle reaping while it is thinking between
/// tool calls. `ToolExecutor` stores this behind an Arc so its per-call clones
/// do not multiply claims.
#[derive(Debug)]
pub struct DesktopLease {
    scope_key: String,
    browser_instance: Option<String>,
    ephemeral: bool,
}

impl Drop for DesktopLease {
    fn drop(&mut self) {
        let Some(counts) = ACTIVE_LEASES.get() else {
            return;
        };
        let mut counts = counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let last_lease = match counts.get_mut(&self.scope_key) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => {
                counts.remove(&self.scope_key);
                true
            }
            None => {
                tracing::warn!(desktop = %self.scope_key, "isolated desktop lease was released twice");
                false
            }
        };
        drop(counts);
        // Ephemeral scopes are a worker/job resource, not an idle cache. Once
        // the final turn executor drops, reclaim its display and browser
        // profile immediately—even after cancellation or a panic unwind.
        if last_lease && self.ephemeral {
            discard_key(&self.scope_key);
        }
        if let Some(instance) = self.browser_instance.as_deref() {
            release_isolated_browser(instance);
        }
    }
}

static ACTIVE_LEASES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
static ISOLATED_BROWSER_LEASES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

/// Claim a scope for a full agent turn. The caller normally holds the returned
/// Arc in its `ToolExecutor`; a worker's Drop cleanup still calls
/// [`discard_scope`] for immediate disposal.
pub fn acquire_lease(scope: &DesktopScope) -> std::sync::Arc<DesktopLease> {
    let _lifecycle = DESKTOP_LIFECYCLE.read().unwrap_or_else(|p|p.into_inner());
    let gate = scope_gate(scope.key());
    let _scope = gate.lock().unwrap_or_else(|p|p.into_inner());
    let counts = ACTIVE_LEASES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut counts = counts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *counts.entry(scope.key.clone()).or_insert(0) += 1;
    // A child with a parent browser owner keeps an isolated OS desktop, but
    // its browser is a separate Electron tab backed by the parent's shared
    // authenticated partition. Do not fence that tab off as a private-X11
    // browser merely because the worker's terminal/apps use Xvfb.
    let isolated_browser_instance = scope
        .parent_browser_instance
        .is_none()
        .then(|| scope.browser_instance.clone())
        .flatten();
    if let Some(instance) = isolated_browser_instance.as_deref() {
        let browsers = ISOLATED_BROWSER_LEASES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut browsers = browsers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *browsers.entry(instance.to_string()).or_insert(0) += 1;
    }
    std::sync::Arc::new(DesktopLease {
        scope_key: scope.key.clone(),
        browser_instance: isolated_browser_instance,
        ephemeral: scope.ephemeral,
    })
}

fn release_isolated_browser(instance: &str) {
    let Some(browsers) = ISOLATED_BROWSER_LEASES.get() else {
        return;
    };
    let mut browsers = browsers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match browsers.get_mut(instance) {
        Some(count) if *count > 1 => *count -= 1,
        Some(_) => {
            browsers.remove(instance);
        }
        None => {
            tracing::warn!(browser_instance = %instance, "isolated browser lease was released twice")
        }
    }
}

fn has_active_lease(scope_key: &str) -> bool {
    ACTIVE_LEASES
        .get()
        .and_then(|counts| counts.lock().ok())
        .is_some_and(|counts| counts.get(scope_key).copied().unwrap_or(0) > 0)
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseRecord {
    version: u8,
    owner_pid: u32,
    #[serde(default)]
    owner_start_ticks: Option<u64>,
    server_pid: u32,
    #[serde(default)]
    server_start_ticks: Option<u64>,
    #[serde(default)]
    native_runtime: Option<PathBuf>,
    server: String,
    display: String,
    xauthority: String,
}

const LEASE_FILE: &str = "lease.json";
const MAX_LEASE_BYTES: usize = 16 * 1024;

static DESKTOPS: OnceLock<Mutex<HashMap<String, DesktopState>>> = OnceLock::new();
static DESKTOP_LIFECYCLE: RwLock<()> = RwLock::new(());
static SCOPE_GATES: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();

fn scope_gate(key: &str) -> Arc<Mutex<()>> {
    let mut gates = SCOPE_GATES.get_or_init(||Mutex::new(HashMap::new()))
        .lock().unwrap_or_else(|p|p.into_inner());
    gates.retain(|_,gate|gate.strong_count()>0);
    if let Some(gate)=gates.get(key).and_then(Weak::upgrade) {return gate;}
    let gate=Arc::new(Mutex::new(()));
    gates.insert(key.into(),Arc::downgrade(&gate));
    gate
}

thread_local! {
    static ACTIVE_SCOPE: RefCell<Option<DesktopScope>> = const { RefCell::new(None) };
}

/// Install a scope for the duration of one synchronous tool call.  This is
/// thread-local by design: concurrent `spawn_blocking` calls each route to
/// their own X display without racing on `std::env::set_var`.
pub fn with_scope<T>(scope: Option<DesktopScope>, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<DesktopScope>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE_SCOPE.with(|active| {
                *active.borrow_mut() = self.0.take();
            });
        }
    }

    let previous = ACTIVE_SCOPE.with(|active| active.replace(scope));
    let _restore = Restore(previous);
    operation()
}

pub fn current_scope() -> Option<DesktopScope> {
    ACTIVE_SCOPE.with(|active| active.borrow().clone())
}

pub fn current_scope_key() -> Option<String> {
    current_scope().map(|scope| scope.key)
}

pub fn current_parent_browser_instance() -> Option<String> {
    current_scope().and_then(|scope| scope.parent_browser_instance)
}

/// True while an instance is owned by a private X desktop. Canvas/native-pane
/// control uses this fence to avoid reparenting that Chromium into the host
/// application and defeating the agent's display isolation.
pub fn browser_instance_is_isolated(instance: &str) -> bool {
    let reserved = ISOLATED_BROWSER_LEASES
        .get()
        .and_then(|browsers| browsers.lock().ok())
        .is_some_and(|browsers| browsers.get(instance).copied().unwrap_or(0) > 0);
    reserved
        || DESKTOPS
            .get()
            .and_then(|registry| registry.lock().ok())
            .is_some_and(|states| {
                states.values().any(|state| {
                    state
                        .browser_instance
                        .as_deref()
                        .is_some_and(|candidate| candidate == instance)
                })
            })
}

/// Lazily allocate the active desktop, if this thread belongs to a scoped
/// agent.  Non-agent callers intentionally receive `None` and retain the
/// legacy desktop behavior.
pub fn current_environment() -> Result<Option<DesktopEnvironment>> {
    let Some(scope) = current_scope() else {
        return Ok(None);
    };
    ensure(&scope).map(Some)
}

/// Read-only viewer inventory. Never allocates a display or returns private
/// bus addresses, Xauthority paths, or unrelated host windows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopView {
    pub scope_key: String,
    pub label: String,
    pub owner_agent_id: String,
    pub backend: String,
    pub running: bool,
    pub in_use: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
struct DesktopObservationRecord {
    path: PathBuf,
    captured_at_ms: u64,
    width: u32,
    height: u32,
    kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DesktopObservation {
    pub scope_key: String,
    pub captured_at_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kind: Option<String>,
    pub data_url: Option<String>,
}

pub fn record_observation(path:&Path,kind:&str,width:u32,height:u32) {
    let Some(key)=current_scope_key() else {return;};
    let Some(registry)=DESKTOPS.get() else {return;};
    let mut states=registry.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    let Some(state)=states.get_mut(&key) else {return;};
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let captured_at_ms=now.max(state.observation.as_ref().map_or(0,|old|old.captured_at_ms.saturating_add(1)));
    state.observation=Some(DesktopObservationRecord{path:path.to_path_buf(),captured_at_ms,width,height,kind:kind.into()});
}

pub fn latest_observation(scope_key:&str,after_ms:Option<u64>)->Result<DesktopObservation> {
    use base64::Engine;
    let record={
        let registry=DESKTOPS.get().context("no agent desktops have been opened")?;
        let states=registry.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
        states.get(scope_key).context("agent desktop is no longer available")?.observation.clone()
    };
    let Some(record)=record else {return Ok(DesktopObservation{scope_key:scope_key.into(),captured_at_ms:None,
        width:None,height:None,kind:None,data_url:None});};
    let data_url=if after_ms.is_some_and(|after|after>=record.captured_at_ms) {None} else {
        let bytes=crate::config::private_io::read_private_file_limited(&record.path,32*1024*1024)?
            .context("the recorded screen image is no longer available")?;
        anyhow::ensure!(bytes.starts_with(&[137,80,78,71,13,10,26,10]),"recorded screen is not a PNG");
        Some(format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)))
    };
    Ok(DesktopObservation{scope_key:scope_key.into(),captured_at_ms:Some(record.captured_at_ms),
        width:Some(record.width),height:Some(record.height),kind:Some(record.kind),data_url})
}

pub fn existing_desktops() -> Vec<DesktopView> {
    let Some(registry)=DESKTOPS.get() else {return Vec::new();};
    let mut states=registry.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    let mut views=states.values_mut().map(|state| DesktopView {
        scope_key:state.environment.scope_key.clone(),label:state.label.clone(),owner_agent_id:state.owner_agent_id.clone(),
        backend:if state.environment.native.is_some(){"gnome"}else{"x11"}.into(),
        running:state.processes.iter_mut().all(|process|matches!(process.child.try_wait(),Ok(None))),
        in_use:has_active_lease(&state.environment.scope_key),width:DEFAULT_WIDTH,height:DEFAULT_HEIGHT,
    }).collect::<Vec<_>>();
    views.sort_by(|a,b|a.label.cmp(&b.label).then(a.scope_key.cmp(&b.scope_key)));
    views
}

/// Browser processes use a private X desktop only for disposable parallel
/// scopes. Canonical coworker browser calls deliberately ignore their desktop
/// environment and attach to the per-coworker Electron WebContentsView.
pub fn current_browser_environment() -> Result<Option<DesktopEnvironment>> {
    let Some(scope) = current_scope()
        .filter(|scope| scope.is_ephemeral() && scope.parent_browser_instance().is_none())
    else {
        return Ok(None);
    };
    ensure(&scope).map(Some)
}

pub fn ensure(scope: &DesktopScope) -> Result<DesktopEnvironment> {
    ensure_reaper();
    ensure_with_start(scope,start)
}

fn ensure_with_start(scope: &DesktopScope, starter: impl FnOnce(&DesktopScope)->Result<DesktopState>) -> Result<DesktopEnvironment> {
    let _lifecycle = DESKTOP_LIFECYCLE.read().unwrap_or_else(|p|p.into_inner());
    let gate = scope_gate(scope.key());
    let _scope = gate.lock().unwrap_or_else(|p|p.into_inner());
    let registry = DESKTOPS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = states.get_mut(scope.key()) {
        state.last_used = Instant::now();
        if scope.browser_instance.is_some() {
            state.browser_instance = scope.browser_instance.clone();
        }
        return Ok(state.environment.clone());
    }
    drop(states);
    // Startup can take seconds. Only this scope is reserved while the
    // compositor becomes ready; other scopes and observation reads proceed.
    let state = starter(scope)?;
    let environment = state.environment.clone();
    registry.lock().unwrap_or_else(|p|p.into_inner()).insert(scope.key.clone(), state);
    Ok(environment)
}

fn desktop_root(scope: &DesktopScope) -> Result<PathBuf> {
    let root = crate::config::phoenix_home()
        .join("desktops")
        .join(scope.key());
    crate::config::private_io::prepare_phoenix_directory(&root)
        .with_context(|| format!("prepare private desktop directory {}", root.display()))?;
    Ok(root)
}

#[derive(Debug, PartialEq, Eq)]
enum DesktopBackendPreference { Auto, Gnome, X11 }

fn backend_preference(value: Option<&str>) -> Result<DesktopBackendPreference> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("auto") => Ok(DesktopBackendPreference::Auto),
        Some("gnome") => Ok(DesktopBackendPreference::Gnome),
        Some("x11") => Ok(DesktopBackendPreference::X11),
        Some(value) => bail!("unknown PHOENIX_DESKTOP_BACKEND {value:?}; use auto, gnome or x11"),
    }
}

fn start(scope: &DesktopScope) -> Result<DesktopState> {
    #[cfg(unix)]
    {
    let configured = std::env::var("PHOENIX_DESKTOP_BACKEND").ok();
    // Ordinary unit tests exercise the X11 fallback without requiring GNOME.
    // Native integration tests and production-binary acceptance cover auto.
    let default = if cfg!(test) { Some("x11") } else { None };
    let preference = backend_preference(configured.as_deref().or(default))?;
    if preference == DesktopBackendPreference::Gnome {
        return gnome::start(scope);
    }
    let visible_x11_requested = isolated_desktop_visibility_opt_in(
        std::env::var("PHOENIX_SHOW_ISOLATED_DESKTOPS").ok().as_deref());
    if preference == DesktopBackendPreference::Auto && !visible_x11_requested
        && ["gnome-shell", "dbus-run-session", "gdbus"].into_iter().all(executable_in_path)
    {
        match gnome::start(scope) {
            Ok(state) => return Ok(state),
            Err(error) => tracing::warn!(desktop = %scope.key(),
                "native desktop unavailable; using private X11 fallback: {error:#}"),
        }
    }
    }
    start_x11(scope)
}

fn start_x11(scope: &DesktopScope) -> Result<DesktopState> {
    #[cfg(not(unix))]
    {
        let _ = scope;
        bail!("per-agent isolated desktops currently require a Unix X11 runtime");
    }

    #[cfg(unix)]
    {
        for binary in ["xauth", "xdotool"] {
            anyhow::ensure!(
                executable_in_path(binary),
                "isolated desktop needs `{binary}` in PATH"
            );
        }
        let root = desktop_root(scope)?;
        let runtime_dir = root.join("runtime");
        crate::config::private_io::prepare_phoenix_directory(&runtime_dir).with_context(|| {
            format!("prepare isolated desktop runtime {}", runtime_dir.display())
        })?;
        let xauthority = root.join("Xauthority");
        let mut last_error: Option<anyhow::Error> = None;
        for _ in 0..32 {
            let display_number = pick_candidate_display();
            let display = format!(":{display_number}");
            match start_on_display(
                scope,
                &root,
                &runtime_dir,
                &xauthority,
                display_number,
                &display,
            ) {
                Ok(state) => return Ok(state),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("could not allocate an X display")))
    }
}

#[cfg(unix)]
fn start_on_display(
    scope: &DesktopScope,
    root: &Path,
    runtime_dir: &Path,
    xauthority: &Path,
    display_number: u32,
    x_display: &str,
) -> Result<DesktopState> {
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display_number}"));
    if socket.exists() {
        bail!("X display {x_display} is already in use");
    }
    remove_regular_file(xauthority)?;
    let cookie = uuid::Uuid::new_v4().simple().to_string();
    let xauth_status = Command::new("xauth")
        .args(["-f"])
        .arg(xauthority)
        .args(["add", x_display, ".", &cookie])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("create isolated Xauthority cookie")?;
    anyhow::ensure!(
        xauth_status.success(),
        "xauth could not create an isolated display cookie"
    );
    secure_file(xauthority)?;

    // Agent desktops are isolated execution surfaces, not surprise top-level
    // application windows. Keep them on Xvfb unless a developer explicitly
    // opts into watching Xephyr. Canonical browser work uses the authenticated
    // Electron surface mounted inside Phoenix's right sidebar instead.
    let host_display = std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.is_empty());
    let show_isolated_desktops = isolated_desktop_visibility_opt_in(
        std::env::var("PHOENIX_SHOW_ISOLATED_DESKTOPS")
            .ok()
            .as_deref(),
    );
    // Unit/integration tests always use Xvfb, even if a developer has opted in.
    let prefer_xephyr = !cfg!(test)
        && show_isolated_desktops
        && host_display.is_some()
        && executable_in_path("Xephyr");
    let mut processes = Vec::new();
    let (visible, server_label) = if prefer_xephyr {
        let mut command = Command::new("Xephyr");
        command
            .arg(x_display)
            .args([
                "-auth",
                xauthority.to_string_lossy().as_ref(),
                "-screen",
                &format!("{DEFAULT_WIDTH}x{DEFAULT_HEIGHT}"),
                "-nolisten",
                "tcp",
                "-no-host-grab",
                "-resizeable",
                "-br",
                "-title",
                &format!("Phoenix - {}", scope.label()),
                "-name",
                scope.key(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Xephyr must connect to the host display, not its own child display.
        if let Some(host_display) = host_display.as_deref() {
            command.env("DISPLAY", host_display);
        }
        command.env_remove("PHOENIX_DESKTOP_SCOPE");
        let child = spawn_owned(&mut command, "Xephyr").context("start visible Xephyr desktop")?;
        processes.push(DesktopProcess {
            child,
            label: "Xephyr",
        });
        (true, "Xephyr")
    } else {
        anyhow::ensure!(
            executable_in_path("Xvfb"),
            "isolated desktop needs Xephyr with a host DISPLAY, or Xvfb for headless fallback"
        );
        let mut command = Command::new("Xvfb");
        command
            .arg(x_display)
            .args([
                "-auth",
                xauthority.to_string_lossy().as_ref(),
                "-screen",
                "0",
                &format!("{}x{}x24", DEFAULT_WIDTH, DEFAULT_HEIGHT),
                "-nolisten",
                "tcp",
                "-noreset",
                "-br",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = spawn_owned(&mut command, "Xvfb").context("start hidden Xvfb desktop")?;
        processes.push(DesktopProcess {
            child,
            label: "Xvfb",
        });
        (false, "Xvfb")
    };

    let environment = DesktopEnvironment {
        display: x_display.to_string(),
        xauthority: xauthority.to_path_buf(),
        runtime_dir: runtime_dir.to_path_buf(),
        scope_key: scope.key.clone(),
        visible,
        native: None,
    };
    let lease = LeaseRecord {
        version: 1,
        owner_pid: std::process::id(),
        owner_start_ticks: current_process_start_ticks(),
        server_pid: processes
            .first()
            .map(|process| process.child.id())
            .context("isolated desktop server process is missing")?,
        server: server_label.to_string(),
        server_start_ticks: processes.first().and_then(|process| process_start_ticks(process.child.id())),
        native_runtime: None,
        display: environment.display.clone(),
        xauthority: environment.xauthority.to_string_lossy().into_owned(),
    };
    if let Err(error) = write_lease(root, &lease) {
        terminate_processes(&mut processes);
        let _ = remove_regular_file(xauthority);
        return Err(error);
    }
    if let Err(error) = wait_for_display(&environment, &socket) {
        terminate_processes(&mut processes);
        let _ = remove_regular_file(xauthority);
        return Err(error);
    }

    // A small X11 WM gives each agent conventional focus and stacking inside
    // its own display. It is deliberately optional: Xvfb/Xephyr + xdotool
    // still work if a minimal host image has no window manager package.
    if let Some(window_manager) = ["openbox", "fluxbox", "twm"]
        .into_iter()
        .find(|binary| executable_in_path(binary))
    {
        match start_window_manager(window_manager, &environment, root) {
            Ok(process) => processes.push(process),
            Err(error) => {
                terminate_processes(&mut processes);
                let _ = remove_regular_file(xauthority);
                return Err(error);
            }
        }
    }

    tracing::info!(
        desktop = %scope.key(),
        x_display = %x_display,
        visible,
        "started isolated agent desktop"
    );
    Ok(DesktopState {
        label: scope.label.clone(),
        owner_agent_id: scope.owner_agent_id.clone(),
        observation: None,
        environment,
        root: root.to_path_buf(),
        processes,
        ephemeral: scope.ephemeral,
        browser_instance: scope.browser_instance.clone(),
        last_used: Instant::now(),
})
}

#[cfg(unix)]
fn start_window_manager(
    binary: &str,
    environment: &DesktopEnvironment,
    root: &Path,
) -> Result<DesktopProcess> {
    let mut command = Command::new(binary);
    environment.apply_to_command(&mut command);
    // X-server readiness is not window-manager readiness. Openbox can own
    // SubstructureRedirect before its startup scan/listener is ready; a client
    // mapped in that interval may remain invisible. Its startup callback runs
    // after listener installation and initial window management. Use that
    // acknowledgement, not a fixed sleep or an early EWMH property.
    let readiness = if binary == "openbox" {
        anyhow::ensure!(
            executable_in_path("touch"),
            "Openbox readiness requires touch"
        );
        let directory = tempfile::Builder::new()
            .prefix("wm-startup-")
            .tempdir_in(root)?;
        let marker = directory.path().join("ready");
        let quoted = marker.to_string_lossy().replace('\'', "'\\''");
        // Openbox parses argv with GLib, not a shell. Quote the entire path
        // nevertheless, including apostrophes in user-selected state roots.
        command.args(["--sm-disable", "--startup", &format!("touch -- '{quoted}'")]);
        Some((directory, marker))
    } else {
        None
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = spawn_owned(&mut command, "window manager")?;
    let mut process = DesktopProcess {
        child,
        label: "window manager",
    };
    if let Some((_directory, marker)) = readiness {
        let ready = (|| -> Result<()> {
            let deadline = Instant::now() + STARTUP_BUDGET;
            loop {
                anyhow::ensure!(
                    process.child.try_wait()?.is_none(),
                    "isolated window manager exited before readiness"
                );
                if marker.is_file() {
                    return Ok(());
                }
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "isolated window manager did not acknowledge readiness"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        if let Err(error) = ready {
            terminate_processes(std::slice::from_mut(&mut process));
            return Err(error);
        }
    }
    Ok(process)
}

#[cfg(unix)]
fn wait_for_display(environment: &DesktopEnvironment, socket: &Path) -> Result<()> {
    let deadline = Instant::now() + STARTUP_BUDGET;
    while Instant::now() < deadline {
        if socket.exists() {
            let mut probe = Command::new("xdpyinfo");
            environment.apply_to_command(&mut probe);
            probe
                .args(["-display", &environment.display])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if probe
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
            {
                return Ok(());
            }
        }
        std::thread::sleep(READY_POLL);
    }
    bail!(
        "isolated X display {} did not become ready within {}s",
        environment.display,
        STARTUP_BUDGET.as_secs()
    )
}

#[cfg(unix)]
fn spawn_owned(command: &mut Command, label: &'static str) -> Result<Child> {
    // Linux binds PDEATHSIG to the spawning THREAD, not the process. Tool
    // workers can retire between model calls, so they must not parent a
    // desktop expected to survive those calls. One process-lifetime spawner
    // preserves crash cleanup without tying the desktop to a worker's life.
    type SpawnRequest = (Command, std::sync::mpsc::SyncSender<std::io::Result<Child>>);
    static SPAWNER: OnceLock<std::sync::mpsc::Sender<SpawnRequest>> = OnceLock::new();
    let spawner = SPAWNER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<SpawnRequest>();
        std::thread::Builder::new()
            .name("phoenix-desktop-spawner".into())
            .spawn(move || {
                for (mut command, reply) in receiver {
                    if let Err(undelivered) = reply.send(command.spawn()) {
                        if let Ok(mut child) = undelivered.0 {
                            let _ = terminate_owned_process_group(&mut child);
                        }
                    }
                }
            })
            .expect("start desktop process owner");
        sender
    });
    use std::os::unix::process::CommandExt;
    // Both setpgid and prctl are async-signal-safe. PDEATHSIG means a crash or
    // forced gateway stop cannot leave a hidden desktop behind indefinitely.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let owned = std::mem::replace(command, Command::new("true"));
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    spawner.send((owned, reply)).map_err(|_| anyhow::anyhow!("desktop process owner stopped"))?;
    result.recv().context("desktop process owner did not return a child")?
        .with_context(|| format!("spawn isolated desktop {label}"))
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("secure {}", path.display()))
}

fn remove_regular_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
        }
        Ok(_) => bail!(
            "refusing non-regular isolated desktop file {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn executable_in_path(binary: &str) -> bool {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return is_executable(candidate);
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| is_executable(&directory.join(binary)))
    })
}

fn isolated_desktop_visibility_opt_in(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn pick_candidate_display() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // X display numbers are operating-system resources, not a worker limit.
    // The rotating space avoids one fixed small range (the old :99..:130 cap)
    // while retaining a bounded Unix-socket filename and no TCP listener.
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    1000 + pid.wrapping_add(sequence).wrapping_mul(37) % 50_000
}

/// Immediately dispose one scope. This is idempotent and intentionally the
/// lifecycle hook used by volume-worker Drop guards and cancellation paths.
pub fn discard_scope(scope: &DesktopScope) {
    discard_key(scope.key());
}

pub fn discard_key(scope_key: &str) {
    discard_matching(scope_key, |_|true);
}

fn discard_matching(scope_key: &str, should_discard: impl FnOnce(&DesktopState)->bool) {
    let _lifecycle = DESKTOP_LIFECYCLE.read().unwrap_or_else(|p|p.into_inner());
    let gate = scope_gate(scope_key);
    let _scope = gate.lock().unwrap_or_else(|p|p.into_inner());
    let Some(registry) = DESKTOPS.get() else {
        return;
    };
    let state = {
        let mut states=registry.lock().unwrap_or_else(|p|p.into_inner());
        if !states.get(scope_key).is_some_and(should_discard) {return;}
        states.remove(scope_key)
    };
    if let Some(state) = state {
        cleanup_state(state);
    }
}

fn cleanup_state(mut state: DesktopState) {
    // A headed Chromium must close before its X server disappears; otherwise
    // a profile can remain held by a renderer that has lost its display.
    if let Some(instance) = state.browser_instance.as_deref() {
        if state.ephemeral {
            // Job/volume profiles are disposable. This path performs an exact
            // PIDFD-verified profile-holder sweep even when a wedged browser
            // action still owns the Session mutex, then removes the profile.
            if let Err(error) =
                crate::tools::browser_native::discard_ephemeral_browser_profile(instance)
            {
                tracing::warn!(
                    browser_instance = %instance,
                    "could not fully discard ephemeral browser during desktop cleanup: {error:#}"
                );
            }
        } else {
            crate::tools::browser_native::close_instance_for_desktop(instance);
        }
    }
    terminate_processes(&mut state.processes);
    if let Err(error) = remove_private_desktop_root(&state.root) {
        tracing::warn!(
            desktop = %state.environment.scope_key,
            path = %state.root.display(),
            "isolated desktop process cleanup completed but state removal failed: {error:#}"
        );
    }
    tracing::info!(desktop = %state.environment.scope_key, "cleaned isolated agent desktop");
}

fn terminate_processes(processes: &mut [DesktopProcess]) {
    // Stop the WM first, then the X server. X clients receive a clean display
    // disconnect when the server exits; every direct child also has PDEATHSIG
    // as a crash-path backstop.
    for process in processes.iter_mut().rev() {
        if let Err(error) = terminate_owned_process_group(&mut process.child) {
            tracing::debug!(
                component = process.label,
                "could not reap isolated desktop child: {error}"
            );
        }
    }
}

#[cfg(unix)]
fn terminate_owned_process_group(child: &mut Child) -> std::io::Result<()> {
    let pid = i32::try_from(child.id())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "child pid overflow"))?;
    // `spawn_owned` creates this exact PGID. Recheck before signalling a
    // numeric process group so a vanished/reused PID can never widen cleanup.
    let owns_group = unsafe { libc::getpgid(pid) } == pid;
    if owns_group {
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    } else {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        if child.try_wait()?.is_some() {
            return ensure_process_group_gone(pid);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if owns_group && unsafe { libc::getpgid(pid) } == pid {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
    let _ = child.wait()?;
    ensure_process_group_gone(pid)
}

#[cfg(unix)]
fn process_group_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    (unsafe { libc::kill(-pgid, 0) }) == 0
        || matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
}

#[cfg(unix)]
fn ensure_process_group_gone(pgid: i32) -> std::io::Result<()> {
    if !process_group_alive(pgid) {
        return Ok(());
    }
    // A child may have exited while a same-PGID helper ignored SIGTERM. The
    // PGID cannot be reused while that helper exists, so this remains scoped
    // to the process tree that `spawn_owned` created.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_millis(750);
    while Instant::now() < deadline && process_group_alive(pgid) {
        std::thread::sleep(Duration::from_millis(20));
    }
    if process_group_alive(pgid) {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "owned desktop process group remained live after SIGKILL",
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn terminate_owned_process_group(child: &mut Child) -> std::io::Result<()> {
    let _ = child.kill();
    let _ = child.wait()?;
    Ok(())
}

fn remove_private_desktop_root(root: &Path) -> Result<()> {
    let desktops_root = crate::config::phoenix_home().join("desktops");
    anyhow::ensure!(
        root.starts_with(&desktops_root)
            && root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("desktop-")
                        && name.len() == "desktop-".len() + 24
                        && name["desktop-".len()..]
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                }),
        "refusing to remove unexpected isolated desktop root {}",
        root.display()
    );
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => std::fs::remove_dir_all(root)
            .with_context(|| format!("remove isolated desktop state {}", root.display())),
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(root)
            .with_context(|| format!("remove isolated desktop state symlink {}", root.display())),
        Ok(_) => bail!(
            "isolated desktop root is not a directory: {}",
            root.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", root.display())),
    }
}

fn write_lease(root: &Path, lease: &LeaseRecord) -> Result<()> {
    let path = root.join(LEASE_FILE);
    let bytes = serde_json::to_vec(lease).context("serialize isolated desktop lease")?;
    crate::config::private_io::atomic_write_private(&path, &bytes)
        .with_context(|| format!("write isolated desktop lease {}", path.display()))
}

fn current_process_start_ticks() -> Option<u64> {
    process_start_ticks(std::process::id())
}

#[cfg(target_os = "linux")]
fn process_start_ticks(pid: u32) -> Option<u64> {
    // `/proc/<pid>/stat` field 22 is starttime. The executable name may have
    // spaces or parentheses, so split only after the final `) `.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(") ")?;
    // Field 3 is state, so field 22 is index 19 after the state field.
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn process_start_ticks(_pid: u32) -> Option<u64> {
    None
}

/// Reclaim a state directory from a previous gateway only after proving its
/// recorded owner is gone (PID + Linux start-time fence). The sole process we
/// signal is the recorded X server and only when its command line still names
/// the private Xauthority file; this avoids ever signalling a reused PID.
fn reclaim_stale_scopes() {
    if crate::config::test_isolated_from_live_home() {
        return;
    }
    let root = crate::config::phoenix_home().join("desktops");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(path = %root.display(), "could not scan isolated desktop state: {error}");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !valid_desktop_directory_name(name) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => metadata,
            Ok(_) => continue,
            Err(_) => continue,
        };
        #[cfg(unix)]
        if !same_owner(&metadata) {
            tracing::warn!(path = %path.display(), "refusing stale isolated desktop owned by another user");
            continue;
        }
        let lease_path = path.join(LEASE_FILE);
        let lease =
            crate::config::private_io::read_private_file_limited(&lease_path, MAX_LEASE_BYTES)
                .ok()
                .flatten()
                .and_then(|bytes| serde_json::from_slice::<LeaseRecord>(&bytes).ok());
        let Some(lease) = lease else {
            // A half-created directory has no proven child to signal. It is
            // safe to remove only when no X socket can be inferred from a
            // valid lease, so leave it for manual forensics rather than risk a
            // broad deletion under a corrupted state tree.
            continue;
        };
        if lease.version != 1 || lease.server_pid == 0 || lease.display.is_empty() {
            continue;
        }
        let owner_alive = process_matches_start(lease.owner_pid, lease.owner_start_ticks);
        if owner_alive {
            continue;
        }
        if !terminate_recorded_server(&lease) {
            continue;
        }
        match remove_private_desktop_root(&path) {
            Ok(()) => tracing::info!(desktop = name, "reclaimed stale isolated desktop scope"),
            Err(error) => {
                tracing::warn!(path = %path.display(), "could not remove stale isolated desktop: {error:#}")
            }
        }
    }
}

fn valid_desktop_directory_name(name: &str) -> bool {
    name.starts_with("desktop-")
        && name.len() == "desktop-".len() + 24
        && name["desktop-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(unix)]
fn same_owner(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == unsafe { libc::geteuid() }
}

fn process_matches_start(pid: u32, expected_start: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }
    if !process_alive(pid) {
        return false;
    }
    #[cfg(not(unix))]
    return false;
    match expected_start {
        Some(expected) => process_start_ticks(pid) == Some(expected),
        // A pre-starttime record cannot safely be identified after PID reuse.
        None => false,
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        (unsafe { libc::kill(pid as i32, 0) }) == 0
            || matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn terminate_recorded_server(lease: &LeaseRecord) -> bool {
    #[cfg(target_os = "linux")]
    {
        if lease.native_runtime.is_some() {
            return gnome::terminate_recorded(lease);
        }
        if !process_alive(lease.server_pid) {
            return true;
        }
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", lease.server_pid))
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "));
        let expected_server = matches!(lease.server.as_str(), "Xephyr" | "Xvfb");
        let proven = expected_server
            && cmdline.as_deref().is_some_and(|line| {
                line.contains(&lease.server)
                    && line.contains(&lease.xauthority)
                    && line.contains(&lease.display)
            });
        if !proven {
            tracing::warn!(
                pid = lease.server_pid,
                "stale isolated desktop server identity was not provable; leaving its PID untouched"
            );
            return false;
        }
        unsafe {
            libc::kill(lease.server_pid as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && process_alive(lease.server_pid) {
            std::thread::sleep(Duration::from_millis(25));
        }
        if process_alive(lease.server_pid) {
            unsafe {
                libc::kill(lease.server_pid as i32, libc::SIGKILL);
            }
        }
        !process_alive(lease.server_pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = lease;
        false
    }
}

fn ensure_reaper() {
    static REAPER: OnceLock<()> = OnceLock::new();
    REAPER.get_or_init(|| {
        reclaim_stale_scopes_on_startup();
        if cfg!(test) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("phoenix-desktop-reaper".to_string())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(30));
                reap_idle();
            });
    });
}

/// Startup reclamation is separate from the idle reaper so a restarted
/// gateway removes dead Xephyr/Xvfb/Openbox state before accepting new work.
/// The identity fence in [`reclaim_stale_scopes`] prevents a stale lease from
/// ever signalling a reused PID.
pub fn reclaim_stale_scopes_on_startup() {
    static RECLAIMED: OnceLock<()> = OnceLock::new();
    RECLAIMED.get_or_init(reclaim_stale_scopes);
}

fn reap_idle() {
    let Some(registry) = DESKTOPS.get() else {
        return;
    };
    let now = Instant::now();
    let expired = {
        let states = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        states
            .iter()
            .filter_map(|(key, state)| {
                if has_active_lease(key) {
                    return None;
                }
                let idle_limit = if state.ephemeral {
                    EPHEMERAL_IDLE
                } else {
                    DURABLE_IDLE
                };
                (now.duration_since(state.last_used) >= idle_limit).then(|| key.clone())
            })
            .collect::<Vec<_>>()
    };
    for key in expired {
        discard_if_idle(&key);
    }
}

fn discard_if_idle(key: &str) {
    discard_matching(key, |state| {
        let idle_limit=if state.ephemeral {EPHEMERAL_IDLE} else {DURABLE_IDLE};
        !has_active_lease(key) && state.last_used.elapsed()>=idle_limit
    });
}

/// Gateway shutdown hook. It is safe to call more than once and does not
/// mutate the caller's desktop or process environment.
pub fn shutdown() {
    let _lifecycle = DESKTOP_LIFECYCLE.write().unwrap_or_else(|p|p.into_inner());
    let Some(registry) = DESKTOPS.get() else {
        return;
    };
    let states = {
        let mut guard = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
            .into_values()
            .collect::<Vec<_>>()
    };
    for state in states {
        cleanup_state(state);
    }
    if let Some(counts) = ACTIVE_LEASES.get() {
        counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
    if let Some(browsers) = ISOLATED_BROWSER_LEASES.get() {
        browsers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

/// Explicitly named variant used by the daemon lifecycle. Kept alongside
/// `shutdown` for callers added during the desktop-isolation migration.
pub fn shutdown_all() {
    shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startup_fixture(scope: &DesktopScope) -> Result<DesktopState> {
        let root=desktop_root(scope)?;
        Ok(DesktopState {
            label:scope.label.clone(), owner_agent_id:scope.owner_agent_id.clone(), observation:None,
            environment:DesktopEnvironment {display:":12345".into(),xauthority:root.join("Xauthority"),
                runtime_dir:root.join("runtime"),scope_key:scope.key.clone(),visible:false,native:None},
            root,processes:vec![],ephemeral:scope.ephemeral,browser_instance:None,last_used:Instant::now()})
    }

    #[test]
    fn stale_idle_candidate_is_retained_after_lease_or_new_activity() {
        let home=tempfile::tempdir().unwrap();
        let _home=crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope=DesktopScope::agent("idle-candidate","owner",None).unwrap();
        ensure_with_start(&scope,startup_fixture).unwrap();
        let expire=|| {
            DESKTOPS.get().unwrap().lock().unwrap().get_mut(scope.key()).unwrap().last_used=
                Instant::now()-DURABLE_IDLE-Duration::from_secs(1);
        };
        let present=||existing_desktops().iter().any(|view|view.scope_key==scope.key);
        expire();
        let lease=acquire_lease(&scope);
        discard_if_idle(scope.key());
        assert!(present(),"a stale reaper candidate discarded an active lease");
        drop(lease);
        ensure_with_start(&scope,|_|panic!("live desktop must be reused")).unwrap();
        discard_if_idle(scope.key());
        assert!(present(),"a stale reaper candidate discarded resumed work");
        expire();discard_if_idle(scope.key());assert!(!present());
    }

    #[test]
    fn slow_desktop_startups_overlap_without_blocking_observation_reads() {
        use std::sync::mpsc;
        let home=tempfile::tempdir().unwrap();
        let _home=crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (started_tx,started_rx)=mpsc::channel();
        let mut workers=vec![];let mut releases=vec![];let mut scopes=vec![];
        for name in ["first","second"] {
            let scope=DesktopScope::agent("concurrent-start",name,None).unwrap();
            scopes.push(scope.clone());
            let (release_tx,release_rx)=mpsc::channel();releases.push(release_tx);
            let started_tx=started_tx.clone();
            workers.push(std::thread::spawn(move||ensure_with_start(&scope,|scope| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).context("release startup fixture")?;
                startup_fixture(scope)
            })));
        }
        let first=started_rx.recv_timeout(Duration::from_secs(2));
        let second=started_rx.recv_timeout(Duration::from_secs(2));
        let (read_tx,read_rx)=mpsc::channel();
        let reader=std::thread::spawn(move||{read_tx.send(existing_desktops()).unwrap();});
        let observed=read_rx.recv_timeout(Duration::from_secs(1));
        let shutdown_blocked=DESKTOP_LIFECYCLE.try_write().is_err();
        for release in releases {let _=release.send(());}
        for worker in workers {worker.join().unwrap().unwrap();}
        reader.join().unwrap();
        for scope in &scopes {discard_scope(scope);}
        assert!(first.is_ok()&&second.is_ok(),"independent startup was serialized");
        assert!(observed.is_ok(),"observation registry blocked on startup");
        assert!(shutdown_blocked,"shutdown must wait for in-flight startup");
    }

    #[test]
    fn same_scope_startup_is_shared_and_discard_waits_for_publication() {
        use std::sync::{mpsc,atomic::{AtomicUsize,Ordering}};
        let home=tempfile::tempdir().unwrap();
        let _home=crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope=DesktopScope::agent("shared-start","owner",None).unwrap();
        let count=Arc::new(AtomicUsize::new(0));
        let (started_tx,started_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
        let first_scope=scope.clone();let first_count=count.clone();
        let first=std::thread::spawn(move||ensure_with_start(&first_scope,|scope| {
            first_count.fetch_add(1,Ordering::SeqCst);started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).context("release startup fixture")?;
            startup_fixture(scope)
        }));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second_scope=scope.clone();let second_count=count.clone();
        let second=std::thread::spawn(move||ensure_with_start(&second_scope,|scope| {
            second_count.fetch_add(1,Ordering::SeqCst);startup_fixture(scope)
        }));
        release_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().scope_key,second.join().unwrap().unwrap().scope_key);
        assert_eq!(count.load(Ordering::SeqCst),1);
        discard_scope(&scope);

        let (started_tx,started_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
        let worker_scope=scope.clone();
        let worker=std::thread::spawn(move||ensure_with_start(&worker_scope,|scope| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).context("release startup fixture")?;
            startup_fixture(scope)
        }));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done_tx,done_rx)=mpsc::channel();let cleanup_scope=scope.clone();
        let cleanup=std::thread::spawn(move||{discard_scope(&cleanup_scope);done_tx.send(()).unwrap();});
        let premature=done_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        release_tx.send(()).unwrap();worker.join().unwrap().unwrap();cleanup.join().unwrap();
        assert!(!premature,"discard returned while startup could still publish");
        assert!(!existing_desktops().iter().any(|view|view.scope_key==scope.key));
        assert!(!home.path().join("desktops").join(scope.key()).exists());
    }

    #[test]
    fn desktop_backend_defaults_to_auto_and_preserves_explicit_choices() {
        for value in [None,Some(""),Some(" auto ")] {
            assert_eq!(backend_preference(value).unwrap(),DesktopBackendPreference::Auto);
        }
        assert_eq!(backend_preference(Some("gnome")).unwrap(),DesktopBackendPreference::Gnome);
        assert_eq!(backend_preference(Some("x11")).unwrap(),DesktopBackendPreference::X11);
        assert!(backend_preference(Some("gnmoe")).is_err());
    }

    #[test]
    fn managed_desktop_default_resolution_is_high_density() {
        assert_eq!((DEFAULT_WIDTH,DEFAULT_HEIGHT),(1920,1200));
    }

    #[test]
    fn isolated_desktops_are_headless_unless_explicitly_requested() {
        assert!(!isolated_desktop_visibility_opt_in(None));
        assert!(!isolated_desktop_visibility_opt_in(Some("false")));
        assert!(!isolated_desktop_visibility_opt_in(Some("unexpected")));
        assert!(isolated_desktop_visibility_opt_in(Some("true")));
        assert!(isolated_desktop_visibility_opt_in(Some("  ON ")));
    }

    #[test]
    fn canonical_scopes_are_stable_and_parallel_scopes_are_distinct() {
        let canonical_a = DesktopScope::agent("session-a", "coder", None).unwrap();
        let canonical_b = DesktopScope::agent("session-a", "coder", None).unwrap();
        let parallel = DesktopScope::agent("session-a", "coder", Some("job-2")).unwrap();
        let worker = DesktopScope::worker("session-a", "coder", "volume-a-1").unwrap();
        assert_eq!(canonical_a.key(), canonical_b.key());
        assert_ne!(canonical_a.key(), parallel.key());
        assert_ne!(parallel.key(), worker.key());
        assert!(!canonical_a.is_ephemeral());
        assert!(parallel.is_ephemeral());
        assert!(worker.is_ephemeral());
    }

    #[test]
    fn nested_scope_restores_the_outer_scope_without_global_env_mutation() {
        let outer = DesktopScope::agent("session-a", "coder", None).unwrap();
        let inner = DesktopScope::worker("session-a", "coder", "volume-a-1").unwrap();
        let display_before = std::env::var_os("DISPLAY");
        with_scope(Some(outer.clone()), || {
            assert_eq!(current_scope_key().as_deref(), Some(outer.key()));
            with_scope(Some(inner.clone()), || {
                assert_eq!(current_scope_key().as_deref(), Some(inner.key()));
            });
            assert_eq!(current_scope_key().as_deref(), Some(outer.key()));
        });
        assert!(current_scope().is_none());
        assert_eq!(std::env::var_os("DISPLAY"), display_before);
    }

    #[test]
    fn canonical_desktop_keeps_browser_in_sidebar_while_parallel_scope_is_isolated() {
        let canonical = DesktopScope::agent("session-reserve", "coder", None)
            .unwrap()
            .with_browser_instance(Some("agent-coder"));
        assert!(!browser_instance_is_isolated("agent-coder"));
        let canonical_lease = acquire_lease(&canonical);
        assert!(!browser_instance_is_isolated("agent-coder"));
        drop(canonical_lease);

        let parallel = DesktopScope::agent("session-reserve", "coder", Some("job-2"))
            .unwrap()
            .with_browser_instance(Some("agent-coder-job-2"));
        let lease = acquire_lease(&parallel);
        assert!(browser_instance_is_isolated("agent-coder-job-2"));
        assert!(!browser_instance_is_isolated("agent-coder"));
        drop(lease);
        assert!(!browser_instance_is_isolated("agent-coder-job-2"));

        let inherited_worker = DesktopScope::worker("session-reserve", "school_coach", "job-3")
            .unwrap()
            .with_browser_instance(Some("volume-worker-volume-test-3"))
            .with_parent_browser_instance(Some("agent-school_coach"));
        let inherited_lease = acquire_lease(&inherited_worker);
        assert!(
            !browser_instance_is_isolated("volume-worker-volume-test-3"),
            "the worker browser is an independent tab in the parent's authenticated Electron profile"
        );
        with_scope(Some(inherited_worker), || {
            assert!(
                current_browser_environment().unwrap().is_none(),
                "only the worker's OS apps belong on Xvfb; its browser must use the shared authenticated profile"
            );
            assert_eq!(
                current_parent_browser_instance().as_deref(),
                Some("agent-school_coach")
            );
        });
        drop(inherited_lease);
    }

    #[test]
    fn browser_environment_is_explicitly_x11_scoped() {
        let environment = DesktopEnvironment {
            display: ":1234".to_string(),
            xauthority: PathBuf::from("/tmp/phoenix-xauth"),
            runtime_dir: PathBuf::from("/tmp/phoenix-runtime"),
            scope_key: "desktop-0123456789abcdef01234567".to_string(),
            visible: true,
            native: None,
        };
        let values = environment.browser_process_env();
        assert_eq!(values.get("DISPLAY").map(String::as_str), Some(":1234"));
        assert_eq!(values.get("WAYLAND_DISPLAY").map(String::as_str), Some(""));
        assert_eq!(
            values.get("PHOENIX_DESKTOP_SCOPE").map(String::as_str),
            Some(environment.scope_key.as_str())
        );
        assert_eq!(
            values.get("DBUS_SESSION_BUS_ADDRESS").map(String::as_str),
            Some("")
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "launches two real Blender GUIs on private X servers; explicit binary and artifact required"]
    fn live_blender_coworker_and_worker_keep_independent_focus_and_pointer() {
        let binary = PathBuf::from(
            std::env::var("PHOENIX_BLENDER_BINARY").expect("explicit Blender binary"),
        );
        let artifact = PathBuf::from(
            std::env::var("PHOENIX_BLENDER_ARTIFACT").expect("explicit test artifact"),
        );
        assert!(binary.is_absolute() && binary.is_file());
        assert!(artifact.is_absolute() && artifact.is_file());
        for tool in ["Xvfb", "xauth", "xdotool", "xdpyinfo", "openbox"] {
            assert!(executable_in_path(tool), "required tool missing: {tool}");
        }
        let original = std::fs::read(&artifact).unwrap();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let coworker = DesktopScope::agent("native-blender-proof", "frontend", None).unwrap();
        let worker =
            DesktopScope::worker("native-blender-proof", "frontend", "worker-one").unwrap();
        struct Cleanup {
            scopes: Vec<DesktopScope>,
            children: Vec<Child>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for child in &mut self.children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                for scope in &self.scopes {
                    discard_scope(scope);
                }
            }
        }
        let mut cleanup = Cleanup {
            scopes: vec![coworker.clone(), worker.clone()],
            children: vec![],
        };
        let first = ensure(&coworker).unwrap();
        let second = ensure(&worker).unwrap();
        assert_ne!(first.display, second.display);
        assert!(
            !first.visible && !second.visible,
            "test must not open host windows"
        );
        for env in [&first, &second] {
            let mut command = Command::new(&binary);
            env.apply_to_command(&mut command);
            cleanup.children.push(command.args(["--factory-startup", "--disable-autoexec", "--threads", "1"])
                .arg(&artifact).args(["--python-expr", "import bpy, os, json; from pathlib import Path; Path(os.environ['XDG_RUNTIME_DIR'], 'blender-loaded.json').write_text(json.dumps({'file': bpy.data.filepath, 'meshes': len([o for o in bpy.context.scene.objects if o.type == 'MESH'])}))"])
                .env("LIBGL_ALWAYS_SOFTWARE", "1")
                .stdin(Stdio::null()).stdout(Stdio::null())
                .stderr(std::fs::File::create(home.path().join(format!("{}.stderr", env.scope_key))).unwrap())
                .spawn().unwrap());
        }
        let window = |env: &DesktopEnvironment, pid: u32| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let mut command = Command::new("xdotool");
                env.apply_to_command(&mut command);
                let result = command
                    .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
                    .output()
                    .unwrap();
                if result.status.success() {
                    if let Some(id) = String::from_utf8_lossy(&result.stdout).lines().next() {
                        return id.to_string();
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "Blender {pid} did not map a private window: {}",
                    std::fs::read_to_string(home.path().join(format!("{}.stderr", env.scope_key)))
                        .unwrap_or_default()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let a = window(&first, cleanup.children[0].id());
        let b = window(&second, cleanup.children[1].id());
        for env in [&first, &second] {
            let ready = env.runtime_dir.join("blender-loaded.json");
            let deadline = Instant::now() + Duration::from_secs(30);
            let data = loop {
                if let Ok(bytes) = std::fs::read(&ready) {
                    if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        break data;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "Blender did not finish opening the scene"
                );
                std::thread::sleep(Duration::from_millis(50));
            };
            assert_eq!(data["file"].as_str().unwrap(), artifact.to_str().unwrap());
            assert!(
                data["meshes"].as_u64().unwrap() > 0,
                "the loaded scene must contain geometry"
            );
        }
        let act = |env: &DesktopEnvironment, args: &[&str]| {
            let mut command = Command::new("xdotool");
            env.apply_to_command(&mut command);
            let output = command.args(args).output().unwrap();
            assert!(
                output.status.success(),
                "xdotool failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        act(&first, &["windowfocus", "--sync", &a]);
        act(&second, &["windowfocus", "--sync", &b]);
        act(&first, &["mousemove", "111", "112"]);
        act(&second, &["mousemove", "222", "223"]);
        for _ in 0..20 {
            act(
                &first,
                &["windowfocus", "--sync", &a, "mousemove", "333", "334"],
            );
            assert_eq!(act(&second, &["getwindowfocus"]).trim(), b);
            assert!(act(&second, &["getmouselocation", "--shell"]).contains("X=222\nY=223"));
            act(
                &second,
                &["windowfocus", "--sync", &b, "mousemove", "222", "223"],
            );
            assert_eq!(act(&first, &["getwindowfocus"]).trim(), a);
            assert!(act(&first, &["getmouselocation", "--shell"]).contains("X=333\nY=334"));
        }
        assert!(cleanup
            .children
            .iter_mut()
            .all(|child| child.try_wait().unwrap().is_none()));
        assert_eq!(std::fs::read(&artifact).unwrap(), original);
        println!("LIVE_BLENDER_ISOLATION: two loaded mesh scenes in live GUI processes, coworker + worker displays, 20 independent focus/pointer cycles, artifact unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn two_scopes_receive_independent_live_x_servers_and_active_leases_block_reaping() {
        if !["Xvfb", "xauth", "xdotool", "xdpyinfo"]
            .into_iter()
            .all(executable_in_path)
        {
            eprintln!("skipping live X isolation test: Xvfb toolchain unavailable");
            return;
        }
        // Exercise startup-command quoting as well as readiness: a private
        // state root is allowed to contain spaces and apostrophes.
        let home = tempfile::Builder::new()
            .prefix("phoenix WM's readiness ")
            .tempdir()
            .unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let first = DesktopScope::worker("live-x", "coder", "worker-a").unwrap();
        let second = DesktopScope::worker("live-x", "coder", "worker-b").unwrap();
        struct Cleanup(DesktopScope, DesktopScope);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                discard_scope(&self.0);
                discard_scope(&self.1);
            }
        }
        let _cleanup = Cleanup(first.clone(), second.clone());
        let first_env = ensure(&first).expect("start first Xvfb desktop");
        let second_env = ensure(&second).expect("start second Xvfb desktop");
        assert_ne!(first_env.display, second_env.display);
        assert!(!first_env.visible && !second_env.visible);

        let run_xdotool = |environment: &DesktopEnvironment, args: &[&str]| {
            let mut command = Command::new("xdotool");
            environment.apply_to_command(&mut command);
            let output = command.args(args).output().expect("run xdotool");
            assert!(
                output.status.success(),
                "xdotool {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        run_xdotool(&first_env, &["mousemove", "111", "112"]);
        run_xdotool(&second_env, &["mousemove", "222", "223"]);
        let first_pointer = run_xdotool(&first_env, &["getmouselocation", "--shell"]);
        let second_pointer = run_xdotool(&second_env, &["getmouselocation", "--shell"]);
        assert!(first_pointer.contains("X=111") && first_pointer.contains("Y=112"));
        assert!(second_pointer.contains("X=222") && second_pointer.contains("Y=223"));
        eprintln!("LIVE_POINTER_ISOLATION: two real displays retain independent coordinates");

        // Exercise real application focus, not just distinct DISPLAY strings.
        // Each scope gets two windows so accidentally targeting the wrong
        // display cannot pass merely because X resource IDs happen to match.
        if executable_in_path("xmessage") {
            struct AppChildren(Vec<Child>);
            impl Drop for AppChildren {
                fn drop(&mut self) {
                    for child in &mut self.0 {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
            let mut apps = AppChildren(Vec::new());
            for (env, title) in [
                (&first_env, "phoenix-focus-a1"),
                (&first_env, "phoenix-focus-a2"),
                (&second_env, "phoenix-focus-b1"),
                (&second_env, "phoenix-focus-b2"),
            ] {
                let mut command = Command::new("xmessage");
                env.apply_to_command(&mut command);
                apps.0.push(
                    command
                        .args(["-title", title, "isolated focus verification"])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .expect("launch isolated test window"),
                );
            }
            let find_window = |env: &DesktopEnvironment, title: &str| {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let mut command = Command::new("xdotool");
                    env.apply_to_command(&mut command);
                    let output = command
                        .args(["search", "--onlyvisible", "--name", title])
                        .output()
                        .unwrap();
                    if output.status.success() {
                        if let Some(id) = String::from_utf8_lossy(&output.stdout).lines().next() {
                            return id.to_string();
                        }
                    }
                    if Instant::now() >= deadline {
                        let mut tree = Command::new("xwininfo");
                        env.apply_to_command(&mut tree);
                        if let Ok(tree) = tree.args(["-root", "-tree"]).output() {
                            eprintln!(
                                "isolated window tree for {}: {}",
                                env.display,
                                String::from_utf8_lossy(&tree.stdout)
                            );
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "test application did not map its window: {title}"
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
            };
            let a1 = find_window(&first_env, "phoenix-focus-a1");
            let a2 = find_window(&first_env, "phoenix-focus-a2");
            let b1 = find_window(&second_env, "phoenix-focus-b1");
            let b2 = find_window(&second_env, "phoenix-focus-b2");
            run_xdotool(&first_env, &["windowfocus", "--sync", &a1]);
            run_xdotool(&second_env, &["windowfocus", "--sync", &b1]);
            for _ in 0..20 {
                run_xdotool(&first_env, &["windowfocus", "--sync", &a2]);
                assert_eq!(run_xdotool(&second_env, &["getwindowfocus"]).trim(), b1);
                run_xdotool(&second_env, &["windowfocus", "--sync", &b2]);
                assert_eq!(run_xdotool(&first_env, &["getwindowfocus"]).trim(), a2);
                run_xdotool(&first_env, &["windowfocus", "--sync", &a1]);
                run_xdotool(&second_env, &["windowfocus", "--sync", &b1]);
            }
            eprintln!("LIVE_FOCUS_ISOLATION: four real windows, 20 cross-display focus cycles, zero interference");
        } else {
            eprintln!("live focus verification not run: xmessage unavailable");
        }

        // A direct scoped `.desktop` Exec gets this same command environment.
        // With no host DBus/session activation token, an already-running
        // single-instance host app has no bus route through which it can take
        // the launch and show a window on the user's real desktop.
        let mut environment_probe = Command::new("env");
        first_env.apply_to_command(&mut environment_probe);
        let environment_text = String::from_utf8(
            environment_probe
                .output()
                .expect("inspect scoped command environment")
                .stdout,
        )
        .unwrap();
        assert!(environment_text.contains(&format!("DISPLAY={}", first_env.display)));
        assert!(!environment_text.contains("DBUS_SESSION_BUS_ADDRESS="));
        assert!(!environment_text.contains("XDG_ACTIVATION_TOKEN="));

        let lease = acquire_lease(&first);
        {
            let registry = DESKTOPS.get().unwrap();
            let mut states = registry.lock().unwrap();
            states.get_mut(first.key()).unwrap().last_used = Instant::now() - EPHEMERAL_IDLE;
        }
        reap_idle();
        assert!(
            DESKTOPS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key(first.key()),
            "a worker display must survive while its model turn holds a lease"
        );
        drop(lease);
        assert!(
            !DESKTOPS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key(first.key()),
            "an unleased ephemeral display should be reclaimed immediately"
        );
    }

    #[cfg(unix)]
    #[test]
    fn desktop_child_survives_the_tool_worker_thread_exiting() {
        let mut child = std::thread::spawn(|| {
            let mut command = Command::new("sleep");
            command.arg("30");
            spawn_owned(&mut command, "retired-worker-test").unwrap()
        }).join().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let survived = child.try_wait().unwrap().is_none();
        terminate_owned_process_group(&mut child).unwrap();
        assert!(survived, "desktop died when its temporary tool worker exited");
    }

    #[cfg(unix)]
    #[test]
    fn owned_processes_are_killed_and_reaped_as_one_cleanup_unit() {
        // The background sleep shares the shell's PGID. Killing only the
        // direct child would leave it behind; this proves desktop cleanup
        // terminates the complete owned process group.
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = spawn_owned(&mut command, "test sleeper").unwrap();
        let pid = child.id();
        let mut processes = vec![DesktopProcess {
            child,
            label: "test sleeper",
        }];
        terminate_processes(&mut processes);
        assert!(!process_alive(pid), "owned process {pid} was not reaped");
        assert!(
            !process_group_alive(pid as i32),
            "same-group descendant escaped cleanup for process group {pid}"
        );
    }

    #[test]
    fn stale_dead_scope_is_reclaimed_without_touching_unrelated_paths() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope = DesktopScope::worker("stale", "coder", "worker-a").unwrap();
        let root = home.path().join("desktops").join(scope.key());
        crate::config::private_io::prepare_phoenix_directory(&root).unwrap();
        let lease = LeaseRecord {
            version: 1,
            owner_pid: u32::MAX,
            owner_start_ticks: Some(1),
            server_pid: u32::MAX - 1,
            server_start_ticks: None,
            native_runtime: None,
            server: "Xvfb".to_string(),
            display: ":49999".to_string(),
            xauthority: root.join("Xauthority").to_string_lossy().into_owned(),
        };
        write_lease(&root, &lease).unwrap();
        reclaim_stale_scopes();
        assert!(!root.exists());
        assert!(home.path().exists(), "reclamation escaped the desktop root");
    }
}
