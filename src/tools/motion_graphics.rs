//! `motion_graphics` — the bundled motionmaxxing workflow for every coworker.
//!
//! motionmaxxing (github.com/Tejashmakwana/motionmaxxing, Apache-2.0) is a
//! portable SKILL.md package: a motion-direction playbook, a seek-safe GSAP
//! runtime, and scripted render/review gates (`render.mjs`, `lint.mjs`,
//! `look.py`). Phoenix vendors it under `vendor/motionmaxxing/`, embeds it in
//! the binary, and installs it into the ordinary skills root
//! (`~/.phoenix/skills/motionmaxxing/`) the first time it is needed. From
//! there it is a normal installed skill: the `skill` tool lists and loads it,
//! and the runtime skill catalog names it.
//!
//! This tool is the workflow wrapper around that skill. It loads SKILL.md and
//! references on demand (through the skills loader's path-safe reader), runs
//! the skill's scripts through the same bounded `bash` executor every other
//! shell call uses (same permission mode, same Workspace isolation, same
//! cancellation), and returns output paths plus the parsed gate results.
//! Missing Node/Chrome/ffmpeg/Python are reported as a clear dependency
//! receipt instead of an opaque script crash.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{ToolCancellation, ToolOutput};

pub const TOOL_NAME: &str = "motion_graphics";
pub const SKILL_NAME: &str = "motionmaxxing";
/// Upstream revision vendored into `vendor/motionmaxxing/`.
pub const UPSTREAM_REVISION: &str = "8c8ec0f2a6f6c9a0da15cd24f7b1298ab298368c";
/// Written inside the installed skill so Phoenix can tell its own install
/// (refreshable on upgrade) from a copy the user installed themselves.
const BUNDLE_MARKER: &str = ".phoenix-bundle";
/// User-owned state inside the skill: the skill appends the user's verdicts
/// here, so a Phoenix upgrade never overwrites existing files below it.
const USER_OWNED_PREFIXES: &[&str] = &["taste/"];
const DEFAULT_TIMEOUT_SECS: u64 = 180;
const RENDER_TIMEOUT_SECS: u64 = 600;
const MAX_REPORT_TAIL_BYTES: usize = 6 * 1024;

static BUNDLE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/vendor/motionmaxxing");

/// Scripts the `script` action may run. Everything the skill ships; nothing
/// outside its `scripts/` directory.
const SCRIPTS: &[&str] = &[
    "providers.sh",
    "brand.mjs",
    "fetch_logo.mjs",
    "precedent.py",
    "imagegen.py",
    "voice.py",
    "sync.mjs",
    "mix.py",
    "render.mjs",
    "look.py",
    "lint.mjs",
    "blind_review.py",
];

#[derive(Debug, Deserialize, Default)]
pub struct MotionGraphicsInput {
    /// guide | check | start | still | render | lint | look | review | script
    #[serde(default)]
    pub action: Option<String>,
    /// Film project folder (workspace-relative). Default `film`.
    #[serde(default)]
    pub project: Option<String>,
    /// guide: skill file to load (`references/motion.md`, `motion`, `taste/verdicts.md`).
    #[serde(default)]
    pub file: Option<String>,
    /// HTML composition (default `<project>/index.html`).
    #[serde(default)]
    pub html: Option<String>,
    /// render/review: output video (default `<project>/final.mp4`, review `<project>/draft.mp4`).
    /// look: the video to inspect.
    #[serde(default)]
    pub video: Option<String>,
    /// still: comma-separated seconds; lint: sample times.
    #[serde(default)]
    pub times: Option<String>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub from: Option<f64>,
    #[serde(default)]
    pub to: Option<f64>,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub shutter: Option<f64>,
    #[serde(default)]
    pub subframes: Option<u32>,
    #[serde(default)]
    pub grain: Option<f64>,
    /// look/review: planned film length in seconds (G0 length check).
    #[serde(default)]
    pub expect: Option<f64>,
    /// look/review: require a non-silent audio track.
    #[serde(default)]
    pub expect_audio: Option<bool>,
    /// script: one of the skill's scripts (e.g. `brand.mjs`, `precedent.py`).
    #[serde(default)]
    pub script: Option<String>,
    /// script: arguments, passed verbatim (each one shell-quoted).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// start: overwrite an existing index.html with the template.
    #[serde(default)]
    pub force: Option<bool>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Guide,
    Check,
    Start,
    Still,
    Render,
    Lint,
    Look,
    Review,
    Script,
}

impl Action {
    fn parse(raw: Option<&str>) -> Result<Self> {
        let raw = raw.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("guide");
        Ok(match raw.to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "guide" | "load" | "skill" | "reference" | "read" => Self::Guide,
            "check" | "deps" | "dependencies" | "providers" | "doctor" => Self::Check,
            "start" | "scaffold" | "init" | "new" => Self::Start,
            "still" | "stills" => Self::Still,
            "render" | "final" => Self::Render,
            "lint" | "g5" => Self::Lint,
            "look" | "inspect" => Self::Look,
            "review" | "mog_check" | "check_film" | "gates" | "draft" => Self::Review,
            "script" | "run" => Self::Script,
            other => bail!(
                "unknown motion_graphics action `{other}`. Use guide, check, start, still, render, lint, look, review, or script."
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// Bundle install
// ---------------------------------------------------------------------------

fn bundle_files() -> Vec<&'static include_dir::File<'static>> {
    fn walk(dir: &'static Dir<'static>, out: &mut Vec<&'static include_dir::File<'static>>) {
        out.extend(dir.files());
        for child in dir.dirs() {
            walk(child, out);
        }
    }
    let mut files = Vec::new();
    walk(&BUNDLE, &mut files);
    files.sort_by(|a, b| a.path().cmp(b.path()));
    files
}

/// Content hash of the embedded bundle; changes whenever Phoenix ships a
/// different vendored revision.
pub fn bundle_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let mut hasher = Sha256::new();
        for file in bundle_files() {
            hasher.update(file.path().to_string_lossy().as_bytes());
            hasher.update([0]);
            hasher.update(file.contents());
            hasher.update([0]);
        }
        let digest = hasher.finalize();
        let hex = digest.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>();
        format!("{SKILL_NAME}@{}+{hex}", &UPSTREAM_REVISION[..12])
    })
}

/// The embedded SKILL.md (used by tests and as a fallback description).
pub fn bundled_skill_manifest() -> Option<&'static str> {
    BUNDLE.get_file("SKILL.md").and_then(|file| file.contents_utf8())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    /// Phoenix wrote or refreshed the files during this call.
    Installed,
    /// Already current.
    Current,
    /// A motionmaxxing directory the user installed themselves (no Phoenix
    /// marker). Left untouched and used as-is.
    UserManaged,
}

pub fn global_skills_root() -> PathBuf {
    crate::config::phoenix_home().join("skills")
}

/// Install (or refresh) the bundled skill into `skills_root/motionmaxxing`.
/// Idempotent and cheap when current. User-owned files (`taste/`) are never
/// overwritten once they exist.
pub fn ensure_installed_in(skills_root: &Path) -> Result<(PathBuf, InstallState)> {
    let dir = skills_root.join(SKILL_NAME);
    let marker = dir.join(BUNDLE_MARKER);
    let version = bundle_version();
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            bail!(
                "{} exists but is not a plain directory; remove it so Phoenix can install the bundled motionmaxxing skill",
                dir.display()
            );
        }
        Ok(_) => {
            match std::fs::read_to_string(&marker) {
                Ok(current) if current.trim() == version => return Ok((dir, InstallState::Current)),
                Ok(_) => {} // older Phoenix bundle: refresh below
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if dir.join("SKILL.md").is_file() {
                        return Ok((dir, InstallState::UserManaged));
                    }
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("reading {}", marker.display()))
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", dir.display())),
    }

    crate::config::private_io::with_private_lock(&marker, || {
        // Another agent may have finished the install while we waited.
        if std::fs::read_to_string(&marker).is_ok_and(|current| current.trim() == version) {
            return Ok(());
        }
        for file in bundle_files() {
            let relative = file.path();
            let rel_text = relative.to_string_lossy().replace('\\', "/");
            let target = dir.join(relative);
            if USER_OWNED_PREFIXES.iter().any(|prefix| rel_text.starts_with(prefix))
                && std::fs::symlink_metadata(&target).is_ok()
            {
                continue;
            }
            crate::config::private_io::atomic_write_private(&target, file.contents())
                .with_context(|| format!("installing {}", target.display()))?;
        }
        // Same lock as the enclosing with_private_lock: write under it.
        crate::config::private_io::atomic_write_private_under_lock(&marker, format!("{version}\n").as_bytes())
    })?;
    Ok((dir, InstallState::Installed))
}

/// Install into the live Phoenix skills root.
pub fn ensure_installed() -> Result<(PathBuf, InstallState)> {
    ensure_installed_in(&global_skills_root())
}

// ---------------------------------------------------------------------------
// Dependency check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DependencyReport {
    pub node: Option<String>,
    pub node_ok: bool,
    pub chrome: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub python: Option<String>,
    pub python_ok: bool,
    pub elevenlabs_key: bool,
    pub codex: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Node,
    Chrome,
    Ffmpeg,
    Python,
}

impl DependencyReport {
    pub fn detect() -> Self {
        Self::detect_with_path(std::env::var_os("PATH").unwrap_or_default().as_os_str())
    }

    /// PATH-parameterized detection so tests can simulate a bare machine.
    pub fn detect_with_path(path: &std::ffi::OsStr) -> Self {
        let node_bin = find_in_path(path, &["node", "nodejs"]);
        let node = node_bin.as_deref().and_then(|bin| probe_version(bin, &["-v"]));
        let node_ok = node.as_deref().is_some_and(|version| major_version(version) >= Some(22));
        let python_bin = find_in_path(path, &["python3"]);
        let python = python_bin.as_deref().and_then(|bin| probe_version(bin, &["-V"]));
        let python_ok = python.as_deref().is_some_and(|version| {
            let digits = version.trim_start_matches(|c: char| !c.is_ascii_digit());
            let mut parts = digits.split('.').filter_map(|p| p.parse::<u32>().ok());
            matches!((parts.next(), parts.next()), (Some(3), Some(minor)) if minor >= 9)
                || digits.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m > 3)
        });
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            node,
            node_ok,
            chrome: find_chrome(path),
            ffmpeg: find_in_path(path, &["ffmpeg"]),
            ffprobe: find_in_path(path, &["ffprobe"]),
            python,
            python_ok,
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY").is_ok_and(|v| !v.trim().is_empty())
                || home.as_deref().is_some_and(|home| {
                    home.join(".elevenlabs").exists() || home.join(".config/elevenlabs").exists()
                }),
            codex: find_in_path(path, &["codex"]),
        }
    }

    fn has(&self, need: Need) -> bool {
        match need {
            Need::Node => self.node_ok,
            Need::Chrome => self.chrome.is_some(),
            Need::Ffmpeg => self.ffmpeg.is_some() && self.ffprobe.is_some(),
            Need::Python => self.python_ok,
        }
    }

    pub fn ready(&self) -> bool {
        [Need::Node, Need::Chrome, Need::Ffmpeg, Need::Python]
            .into_iter()
            .all(|need| self.has(need))
    }

    fn missing(&self, needs: &[Need]) -> Vec<String> {
        needs
            .iter()
            .filter(|need| !self.has(**need))
            .map(|need| match need {
                Need::Node => match &self.node {
                    Some(version) => format!("Node.js 22+ (found {version}; upgrade via nodejs.org, nvm, or `brew install node`)"),
                    None => "Node.js 22+ (nodejs.org, nvm, `brew install node`, or `sudo apt install nodejs`)".to_string(),
                },
                Need::Chrome => "Google Chrome or Chromium (install it, or set CHROME_PATH to the executable; on Debian/Ubuntu `sudo apt install chromium`)".to_string(),
                Need::Ffmpeg => "ffmpeg + ffprobe (`brew install ffmpeg` or `sudo apt install ffmpeg`)".to_string(),
                Need::Python => match &self.python {
                    Some(version) => format!("Python 3.9+ (found {version})"),
                    None => "Python 3.9+ (python3 on PATH)".to_string(),
                },
            })
            .collect()
    }

    pub fn render(&self) -> String {
        let mark = |ok: bool| if ok { "ok  " } else { "MISS" };
        let path = |p: &Option<PathBuf>| {
            p.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".to_string())
        };
        let mut out = String::new();
        out.push_str(&format!(
            "[{}] node        {}\n",
            mark(self.node_ok),
            self.node.clone().unwrap_or_else(|| "not found".into())
        ));
        out.push_str(&format!("[{}] chrome      {}\n", mark(self.chrome.is_some()), path(&self.chrome)));
        out.push_str(&format!(
            "[{}] ffmpeg      {} (ffprobe: {})\n",
            mark(self.has(Need::Ffmpeg)),
            path(&self.ffmpeg),
            path(&self.ffprobe)
        ));
        out.push_str(&format!(
            "[{}] python3     {}\n",
            mark(self.python_ok),
            self.python.clone().unwrap_or_else(|| "not found".into())
        ));
        out.push_str(&format!(
            "[opt ] ElevenLabs  {}\n",
            if self.elevenlabs_key {
                "key found: voice, sfx and music available (voice.py)"
            } else {
                "no key: films are made without voice or music (set ELEVENLABS_API_KEY)"
            }
        ));
        out.push_str(&format!(
            "[opt ] Codex CLI   {}\n",
            if self.codex.is_some() {
                "present: imagegen.py may make surface plates if its image_generation feature is on"
            } else {
                "absent: surfaces come from code, captured assets, or Phoenix image_gen"
            }
        ));
        out
    }
}

fn major_version(version: &str) -> Option<u32> {
    version
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn probe_version(bin: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = if output.stdout.is_empty() { output.stderr } else { output.stdout };
    let text = String::from_utf8_lossy(&text).trim().to_string();
    (!text.is_empty()).then(|| text.lines().next().unwrap_or_default().to_string())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_in_path(path: &std::ffi::OsStr, names: &[&str]) -> Option<PathBuf> {
    let dirs = std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .take(256)
        .collect::<Vec<_>>();
    for name in names {
        for dir in &dirs {
            let candidate = dir.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Same search order as the skill's own `_cdp.mjs`/`providers.sh`, plus the
/// PATH names Phoenix's native browser looks for. The result is handed to the
/// scripts as CHROME_PATH so they never disagree with this check.
fn find_chrome(path: &std::ffi::OsStr) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CHROME_PATH").map(PathBuf::from) {
        if is_executable_file(&explicit) {
            return Some(explicit);
        }
    }
    if let Some(found) = find_in_path(
        path,
        &["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"],
    ) {
        return Some(found);
    }
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|candidate| is_executable_file(candidate))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Everything one call needs; separated from the executor so tests can point
/// it at a temporary skills root and a simulated dependency report.
pub struct MotionContext<'a> {
    pub workspace_root: &'a Path,
    pub confined: bool,
    pub cancellation: &'a ToolCancellation,
    pub skills_root: PathBuf,
    pub deps: Option<DependencyReport>,
}

pub(super) fn execute_cancellable(
    workspace_root: &Path,
    confined: bool,
    input: MotionGraphicsInput,
    cancellation: &ToolCancellation,
) -> Result<ToolOutput> {
    let skills_root = if crate::config::test_isolated_from_live_home() {
        // Never install into the live ~/.phoenix from a test build.
        std::env::temp_dir().join(format!("phoenix-motion-test-skills-{}", std::process::id()))
    } else {
        global_skills_root()
    };
    execute_in(
        MotionContext { workspace_root, confined, cancellation, skills_root, deps: None },
        input,
    )
}

pub fn execute_in(ctx: MotionContext<'_>, input: MotionGraphicsInput) -> Result<ToolOutput> {
    let action = Action::parse(input.action.as_deref())?;
    if action == Action::Check {
        return Ok(check(&ctx));
    }
    let (skill_dir, state) = ensure_installed_in(&ctx.skills_root)?;
    match action {
        Action::Guide => guide(&skill_dir, &state, &input),
        Action::Start => start(&ctx, &skill_dir, &input),
        Action::Still => still(&ctx, &skill_dir, &input),
        Action::Render => render(&ctx, &skill_dir, &input),
        Action::Lint => lint(&ctx, &skill_dir, &input),
        Action::Look => look(&ctx, &skill_dir, &input),
        Action::Review => review(&ctx, &skill_dir, &input),
        Action::Script => script(&ctx, &skill_dir, &input),
        Action::Check => unreachable!(),
    }
}

fn deps(ctx: &MotionContext<'_>) -> DependencyReport {
    ctx.deps.clone().unwrap_or_else(DependencyReport::detect)
}

fn check(ctx: &MotionContext<'_>) -> ToolOutput {
    let report = deps(ctx);
    let installed = ctx.skills_root.join(SKILL_NAME);
    let install_line = match std::fs::read_to_string(installed.join(BUNDLE_MARKER)) {
        Ok(version) if version.trim() == bundle_version() => format!("installed at {} ({})", installed.display(), version.trim()),
        Ok(version) => format!("installed at {} ({}; refreshes to {} on next use)", installed.display(), version.trim(), bundle_version()),
        Err(_) if installed.join("SKILL.md").is_file() => format!("user-managed copy at {} (Phoenix leaves it as-is)", installed.display()),
        Err(_) => format!("installs to {} on first use ({})", installed.display(), bundle_version()),
    };
    let verdict = if report.ready() {
        "READY: renders, stills, lint and look checks can run.".to_string()
    } else {
        let missing = report.missing(&[Need::Node, Need::Chrome, Need::Ffmpeg, Need::Python]);
        format!(
            "NOT READY: missing {}. Planning, guide/reference loading and `start` still work; rendering and the scripted gates need the missing tools. Tell the user exactly what to install instead of hand-rolling a substitute.",
            missing.join("; ")
        )
    };
    ToolOutput {
        summary: if report.ready() {
            "motion_graphics ready".to_string()
        } else {
            "motion_graphics: dependencies missing".to_string()
        },
        content: format!("motionmaxxing skill: {install_line}\n{}\n{verdict}", report.render()),
    }
}

fn require(ctx: &MotionContext<'_>, needs: &[Need], what: &str) -> Result<DependencyReport> {
    let report = deps(ctx);
    let missing = report.missing(needs);
    if missing.is_empty() {
        return Ok(report);
    }
    Err(anyhow!(
        "motion_graphics cannot {what}: missing {}.\nNothing was run. Dependency report:\n{}Install the missing tools (or ask the user to), then call motion_graphics again. `guide` and `start` work without them.",
        missing.join("; "),
        report.render()
    ))
}

fn guide(skill_dir: &Path, state: &InstallState, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let requested = input.file.as_deref().map(str::trim).filter(|f| !f.is_empty());
    let rel = match requested {
        None => "SKILL.md".to_string(),
        Some(file) if file.contains('/') || file.eq_ignore_ascii_case("SKILL.md") => file.to_string(),
        Some(file) => {
            let bare = file.trim_end_matches(".md");
            if skill_dir.join("references").join(format!("{bare}.md")).is_file() {
                format!("references/{bare}.md")
            } else {
                file.to_string()
            }
        }
    };
    let target = super::skills::resolve_in_skill(skill_dir, &rel)?;
    let content = super::skills::read_skill_file(skill_dir, &target).map_err(|error| {
        let mut refs = std::fs::read_dir(skill_dir.join("references"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| format!("references/{}", entry.file_name().to_string_lossy()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        refs.sort();
        anyhow!(
            "motionmaxxing has no file `{rel}` ({error:#}). Load one of: SKILL.md, {}, runtime/README.md, taste/verdicts.md, taste/edits.md, docs/KNOWN-LIMITS.md",
            refs.join(", ")
        )
    })?;
    let header = if rel == "SKILL.md" {
        phoenix_adapter(skill_dir, state)
    } else {
        format!("motionmaxxing `{rel}` (SKILL = {})\n\n", skill_dir.display())
    };
    Ok(ToolOutput { summary: format!("motionmaxxing / {rel}"), content: format!("{header}{content}") })
}

/// The Phoenix adapter printed above SKILL.md: how the skill's steps map onto
/// this tool, so agents never hand-type script paths.
fn phoenix_adapter(skill_dir: &Path, state: &InstallState) -> String {
    let managed = match state {
        InstallState::UserManaged => " (your own copy; Phoenix does not refresh it)",
        _ => "",
    };
    format!(
        "PHOENIX ADAPTER for motionmaxxing{managed}. SKILL = {dir}\n\
Run the steps below through `motion_graphics` (same permissions and shell isolation as bash):\n\
- step 0 providers.sh → action `check`\n\
- step 6 copy templates/film.html + runtime/ → action `start` (project `film` by default)\n\
- render.mjs --still → action `still` (times \"1.2,2.4\")\n\
- step 7 draft render + look.py + lint.mjs → action `review` (scale 0.5, expect <planned s>); it returns G0/G2/G3 (look.py) and G5 (lint.mjs)\n\
- step 9 final → action `render` (audio, shutter, subframes, grain), then `look` with expect_audio\n\
- any other script (brand.mjs, fetch_logo.mjs, precedent.py, voice.py, sync.mjs, mix.py, imagegen.py, blind_review.py) → action `script` with script + args (paths workspace-relative)\n\
- references on demand → action `guide` with file (e.g. `motion`, `ui-demo`, `references/world.md`, `taste/verdicts.md`)\n\
G1 and G4 are judged by eye: open look/sheet.jpg and the stills with image_analyze, describe every sheet frame, and quote the script numbers in NOTE.md. Never certify a film you did not look at.\n\
Not bundled in Phoenix: docs/media showcase films and the studies/*.jpg calibration strips (studies/PROVENANCE.md explains them).\n\n",
        dir = skill_dir.display()
    )
}

/// A workspace path → (host path, the spelling to use inside the command).
fn workspace_arg(ctx: &MotionContext<'_>, raw: &str, must_exist: bool) -> Result<(PathBuf, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty path");
    }
    let resolved = super::resolve_workspace_path(ctx.workspace_root, raw, must_exist, ctx.confined)?;
    let root = ctx.workspace_root.canonicalize().context("failed to canonicalize workspace root")?;
    let spelled = match resolved.strip_prefix(&root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => resolved.to_string_lossy().into_owned(),
    };
    Ok((resolved, spelled))
}

fn project(input: &MotionGraphicsInput) -> String {
    input
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("film")
        .trim_end_matches('/')
        .to_string()
}

fn join_rel(base: &str, leaf: &str) -> String {
    if base == "." || base.is_empty() {
        leaf.to_string()
    } else {
        format!("{base}/{leaf}")
    }
}

fn html_arg(ctx: &MotionContext<'_>, input: &MotionGraphicsInput) -> Result<(PathBuf, String)> {
    let raw = input.html.clone().unwrap_or_else(|| join_rel(&project(input), "index.html"));
    workspace_arg(ctx, &raw, true).map_err(|error| {
        anyhow!("{error:#}. Run motion_graphics action `start` first (it copies templates/film.html and runtime/ into the project), or pass `html`.")
    })
}

fn start(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let (project_dir, spelled) = workspace_arg(ctx, &project(input), false)?;
    std::fs::create_dir_all(&project_dir).with_context(|| format!("creating {}", project_dir.display()))?;
    let index = project_dir.join("index.html");
    let mut notes = Vec::new();
    if index.exists() && !input.force.unwrap_or(false) {
        notes.push(format!("kept existing {}/index.html (pass force:true to replace it with the template)", spelled));
    } else {
        std::fs::copy(skill_dir.join("templates/film.html"), &index)
            .with_context(|| format!("copying the film template to {}", index.display()))?;
        notes.push(format!("{}/index.html ← templates/film.html", spelled));
    }
    let copied = copy_tree(&skill_dir.join("runtime"), &project_dir.join("runtime"))?;
    notes.push(format!("{}/runtime/ ← runtime/ ({copied} files refreshed)", spelled));
    let report = deps(ctx);
    let readiness = if report.ready() {
        String::new()
    } else {
        format!(
            "\nRendering is not possible on this machine yet: missing {}.",
            report.missing(&[Need::Node, Need::Chrome, Need::Ffmpeg, Need::Python]).join("; ")
        )
    };
    Ok(ToolOutput {
        summary: format!("motion film project ready at {spelled}"),
        content: format!(
            "{}\nNext: write {spelled}/STORYBOARD.md (idea, world, beat table), build the hardest beat in {spelled}/index.html, then `still` and `review`. Runtime API: guide file `runtime/README.md`.{readiness}",
            notes.join("\n")
        ),
    })
}

fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    let mut copied = 0usize;
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = pending.pop() {
        std::fs::create_dir_all(&dst).with_context(|| format!("creating {}", dst.display()))?;
        for entry in std::fs::read_dir(&src).with_context(|| format!("reading {}", src.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let target = dst.join(entry.file_name());
            if kind.is_dir() {
                pending.push((entry.path(), target));
            } else if kind.is_file() {
                std::fs::copy(entry.path(), &target)
                    .with_context(|| format!("copying {}", target.display()))?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

pub(crate) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ',' | ':' | '=' | '+' | '@'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct ScriptRun {
    exit: Option<i32>,
    stdout: String,
    stderr: String,
}

impl ScriptRun {
    fn tail(&self) -> String {
        let mut text = String::new();
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        if !stdout.is_empty() {
            text.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        if text.len() > MAX_REPORT_TAIL_BYTES {
            let mut cut = text.len() - MAX_REPORT_TAIL_BYTES;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            text = format!("…{}", &text[cut..]);
        }
        text
    }
}

/// Build the exact shell command for one skill script. The exit status is
/// captured into the output so scripted verdicts (lint exits 1 on a G5 FAIL)
/// are results rather than executor failures.
fn script_command(skill_dir: &Path, chrome: Option<&Path>, script: &str, args: &[String]) -> String {
    let path = skill_dir.join("scripts").join(script);
    let interpreter = match Path::new(script).extension().and_then(|e| e.to_str()) {
        Some("mjs") | Some("js") => "node",
        Some("py") => "python3",
        _ => "bash",
    };
    let mut command = String::new();
    if let Some(chrome) = chrome {
        command.push_str(&format!("CHROME_PATH={} ", shell_quote(&chrome.to_string_lossy())));
    }
    command.push_str(interpreter);
    command.push(' ');
    command.push_str(&shell_quote(&path.to_string_lossy()));
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command.push_str("; __motion_status=$?; printf '\\n__MOTION_EXIT=%s\\n' \"$__motion_status\"; exit 0");
    command
}

fn run_script(
    ctx: &MotionContext<'_>,
    skill_dir: &Path,
    report: &DependencyReport,
    script: &str,
    args: &[String],
    timeout: u64,
) -> Result<ScriptRun> {
    let command = script_command(skill_dir, report.chrome.as_deref(), script, args);
    let output = super::bash::execute_cancellable(
        ctx.workspace_root,
        ctx.confined,
        super::bash::BashInput {
            command,
            cwd: None,
            timeout_secs: Some(timeout),
            runner: None,
        },
        ctx.cancellation,
    )
    .with_context(|| format!("motionmaxxing {script} did not finish"))?;
    let content = output.content;
    let (stdout, stderr) = match content.split_once("\n\nstderr:\n") {
        Some((out, err)) => (out.trim_start_matches("stdout:\n").to_string(), err.to_string()),
        None => (content.clone(), String::new()),
    };
    let mut exit = None;
    let mut kept = Vec::new();
    for line in stdout.lines() {
        if let Some(code) = line.trim().strip_prefix("__MOTION_EXIT=") {
            exit = code.trim().parse().ok();
        } else {
            kept.push(line);
        }
    }
    Ok(ScriptRun { exit, stdout: kept.join("\n"), stderr })
}

fn timeout(input: &MotionGraphicsInput, default: u64) -> u64 {
    input.timeout_secs.unwrap_or(default).clamp(10, RENDER_TIMEOUT_SECS)
}

fn push_num(args: &mut Vec<String>, flag: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|v| v.is_finite()) {
        args.push(flag.to_string());
        args.push(trim_float(value));
    }
}

fn trim_float(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

fn still(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let report = require(ctx, &[Need::Node, Need::Chrome], "render stills")?;
    let (_, html) = html_arg(ctx, input)?;
    let times = input.times.clone().unwrap_or_else(|| "0.5,2,4".to_string());
    let (out_dir, out_spelled) = workspace_arg(ctx, &join_rel(&project(input), "stills"), false)?;
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut args = vec![html, "--still".to_string(), times, out_spelled.clone()];
    push_num(&mut args, "--scale", input.scale);
    let run = run_script(ctx, skill_dir, &report, "render.mjs", &args, timeout(input, DEFAULT_TIMEOUT_SECS))?;
    let mut stills = list_files(&out_dir, |name| name.starts_with("still_") && name.ends_with(".png"));
    stills.sort();
    let ok = run.exit == Some(0) && !stills.is_empty();
    let listing = stills.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n");
    if !ok {
        bail!("render.mjs --still failed (exit {:?}).\n{}", run.exit, run.tail());
    }
    Ok(ToolOutput {
        summary: format!("{} stills rendered", stills.len()),
        content: format!("Stills (open them with image_analyze before judging):\n{listing}\n\nrender.mjs:\n{}", run.tail()),
    })
}

fn render_args(
    ctx: &MotionContext<'_>,
    input: &MotionGraphicsInput,
    default_video: &str,
    default_scale: Option<f64>,
) -> Result<(Vec<String>, PathBuf, String)> {
    let (_, html) = html_arg(ctx, input)?;
    let video_raw = input.video.clone().unwrap_or_else(|| join_rel(&project(input), default_video));
    let (video_path, video) = workspace_arg(ctx, &video_raw, false)?;
    ensure_parent(&video_path)?;
    let mut args = vec![html, video.clone()];
    push_num(&mut args, "--scale", input.scale.or(default_scale));
    push_num(&mut args, "--fps", input.fps);
    push_num(&mut args, "--from", input.from);
    push_num(&mut args, "--to", input.to);
    if let Some(audio) = input.audio.as_deref().filter(|a| !a.trim().is_empty()) {
        let (_, audio) = workspace_arg(ctx, audio, true)?;
        args.push("--audio".into());
        args.push(audio);
    }
    push_num(&mut args, "--shutter", input.shutter);
    if let Some(subframes) = input.subframes {
        args.push("--subframes".into());
        args.push(subframes.to_string());
    }
    push_num(&mut args, "--grain", input.grain);
    Ok((args, video_path, video))
}

fn render(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let report = require(ctx, &[Need::Node, Need::Chrome, Need::Ffmpeg], "render video")?;
    let (args, video_path, video) = render_args(ctx, input, "final.mp4", None)?;
    let run = run_script(ctx, skill_dir, &report, "render.mjs", &args, timeout(input, RENDER_TIMEOUT_SECS))?;
    let size = std::fs::metadata(&video_path).map(|m| m.len()).unwrap_or(0);
    if run.exit != Some(0) || size == 0 {
        bail!("render.mjs failed (exit {:?}); no video at {video}.\n{}", run.exit, run.tail());
    }
    let events = PathBuf::from(format!("{}.events.json", video_path.display()));
    Ok(ToolOutput {
        summary: format!("rendered {video} ({} KB)", size / 1024),
        content: format!(
            "Video: {}\n{}\nNext: action `look` on this video (expect, expect_audio) and open look/sheet.jpg with image_analyze.\n\nrender.mjs:\n{}",
            video_path.display(),
            if events.is_file() { format!("Events: {}", events.display()) } else { "Events: none (page exposes no window.__events)".to_string() },
            run.tail()
        ),
    })
}

struct LintResult {
    gate: String,
    text: String,
}

fn run_lint(ctx: &MotionContext<'_>, skill_dir: &Path, report: &DependencyReport, input: &MotionGraphicsInput) -> Result<LintResult> {
    let (_, html) = html_arg(ctx, input)?;
    let mut args = vec![html];
    if let Some(times) = input.times.as_deref().filter(|t| !t.trim().is_empty()) {
        args.push("--times".into());
        args.push(times.to_string());
    }
    let run = run_script(ctx, skill_dir, report, "lint.mjs", &args, timeout(input, DEFAULT_TIMEOUT_SECS))?;
    let gate_line = run.stdout.lines().find(|line| line.trim_start().starts_with("GATE G5:")).map(str::trim);
    let gate = match (gate_line, run.exit) {
        (Some(line), _) if line.contains("FAIL") => "FAIL",
        (Some(_), Some(0)) => "PASS",
        (Some(_), _) => "FAIL",
        (None, _) => "ERROR",
    };
    if gate == "ERROR" {
        bail!("lint.mjs did not produce a G5 verdict (exit {:?}).\n{}", run.exit, run.tail());
    }
    Ok(LintResult { gate: gate.to_string(), text: run.tail() })
}

fn lint(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let report = require(ctx, &[Need::Node, Need::Chrome], "lint the composition")?;
    let result = run_lint(ctx, skill_dir, &report, input)?;
    Ok(ToolOutput {
        summary: format!("G5 no page chrome: {}", result.gate),
        content: format!(
            "GATE G5 (lint.mjs): {}\nA FAIL is fixed by deleting the element, never argued.\n\n{}",
            result.gate, result.text
        ),
    })
}

struct LookResult {
    out_dir: PathBuf,
    gates: Vec<(String, String, String, String)>,
    motion: Option<f64>,
    files: Vec<PathBuf>,
    text: String,
}

fn run_look(
    ctx: &MotionContext<'_>,
    skill_dir: &Path,
    report: &DependencyReport,
    input: &MotionGraphicsInput,
    video: &str,
    out_leaf: &str,
) -> Result<LookResult> {
    let (_, video) = workspace_arg(ctx, video, true)?;
    let (out_dir, out) = workspace_arg(ctx, &join_rel(&project(input), out_leaf), false)?;
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut args = vec![video, "--out".to_string(), out];
    push_num(&mut args, "--expect", input.expect);
    if input.expect_audio.unwrap_or(false) {
        args.push("--expect-audio".into());
    }
    let run = run_script(ctx, skill_dir, report, "look.py", &args, timeout(input, DEFAULT_TIMEOUT_SECS))?;
    let gates_path = out_dir.join("gates.json");
    let parsed: Option<serde_json::Value> = std::fs::read(&gates_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let Some(parsed) = parsed else {
        bail!("look.py wrote no gates.json (exit {:?}).\n{}", run.exit, run.tail());
    };
    let gates = parsed["gates"]
        .as_array()
        .map(|gates| {
            gates
                .iter()
                .map(|gate| {
                    let text = |key: &str| match &gate[key] {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    (text("id"), text("name"), text("status"), text("detail"))
                })
                .collect()
        })
        .unwrap_or_default();
    let files = ["sheet.jpg", "mid.jpg", "strips.jpg", "cuts.txt", "still.txt", "gates.json"]
        .into_iter()
        .map(|leaf| out_dir.join(leaf))
        .filter(|path| path.is_file())
        .collect();
    Ok(LookResult {
        out_dir,
        gates,
        motion: parsed["motion_mean_luma_diff"].as_f64(),
        files,
        text: run.tail(),
    })
}

fn render_gates(look: &LookResult) -> String {
    let mut out = String::new();
    for (id, name, status, detail) in &look.gates {
        out.push_str(&format!("{id} {name}: {status}"));
        if !detail.trim().is_empty() {
            out.push_str(&format!(" ({})", detail.trim()));
        }
        out.push('\n');
    }
    if let Some(motion) = look.motion {
        out.push_str(&format!(
            "Motion note: mean luma change {motion:.2} vs human band median 6.8, IQR 4.4-9.8 (a question, never a gate)\n"
        ));
    }
    out
}

fn look(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let report = require(ctx, &[Need::Python, Need::Ffmpeg], "inspect the film")?;
    let video = input.video.clone().unwrap_or_else(|| join_rel(&project(input), "final.mp4"));
    let leaf = if video.contains("draft") { "look" } else { "look-final" };
    let result = run_look(ctx, skill_dir, &report, input, &video, leaf)?;
    let failed = result.gates.iter().filter(|g| g.2 == "FAIL").count();
    Ok(ToolOutput {
        summary: format!("look.py: {} gates, {failed} FAIL", result.gates.len()),
        content: format!(
            "{}Files:\n{}\nOpen {} with image_analyze and describe every frame (G1 proof readable, G4 one hero per frame are judged by eye).\n\nlook.py:\n{}",
            render_gates(&result),
            result.files.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n"),
            result.out_dir.join("sheet.jpg").display(),
            result.text
        ),
    })
}

/// The "mog check": draft render → look.py → lint.mjs in one call, with
/// every gate the scripts can compute and the by-eye gates named.
fn review(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let report = require(ctx, &[Need::Node, Need::Chrome, Need::Ffmpeg, Need::Python], "review the film")?;
    let (args, video_path, video) = render_args(ctx, input, "draft.mp4", Some(0.5))?;
    let run = run_script(ctx, skill_dir, &report, "render.mjs", &args, timeout(input, RENDER_TIMEOUT_SECS))?;
    if run.exit != Some(0) || std::fs::metadata(&video_path).map(|m| m.len()).unwrap_or(0) == 0 {
        bail!("review stopped at the draft render: render.mjs failed (exit {:?}).\n{}", run.exit, run.tail());
    }
    let look = run_look(ctx, skill_dir, &report, input, &video, "look")?;
    let lint = run_lint(ctx, skill_dir, &report, input);
    let (g5, lint_text) = match &lint {
        Ok(result) => (result.gate.clone(), result.text.clone()),
        Err(error) => ("ERROR".to_string(), format!("{error:#}")),
    };
    let failed = look.gates.iter().filter(|g| g.2 == "FAIL").count() + usize::from(g5 != "PASS");
    Ok(ToolOutput {
        summary: format!(
            "motion review: {} — {failed} gate(s) failing",
            if failed == 0 { "scripted gates pass" } else { "fix before shipping" }
        ),
        content: format!(
            "Draft: {}\n\nGATES\n{}G5 no page chrome (lint.mjs): {g5}\nG1 proof readable / G4 one hero per frame: by eye — open {} with image_analyze now and describe each frame in one sentence.\nA passing gate is a floor, not a verdict on taste. Fix order: broken/untrue, idea, connections, hierarchy, timing, finish, sound.\n\nFiles:\n{}\n\nlook.py:\n{}\n\nlint.mjs:\n{}",
            video_path.display(),
            render_gates(&look),
            look.out_dir.join("sheet.jpg").display(),
            look.files.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n"),
            look.text,
            lint_text
        ),
    })
}

fn script(ctx: &MotionContext<'_>, skill_dir: &Path, input: &MotionGraphicsInput) -> Result<ToolOutput> {
    let name = input
        .script
        .as_deref()
        .map(str::trim)
        .map(|s| s.trim_start_matches("scripts/"))
        .filter(|s| !s.is_empty())
        .context("action `script` needs `script` (one of the skill's scripts)")?;
    if !SCRIPTS.contains(&name) {
        bail!("`{name}` is not a motionmaxxing script. Available: {}", SCRIPTS.join(", "));
    }
    let mut needs = Vec::new();
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("mjs") => needs.push(Need::Node),
        Some("py") => needs.push(Need::Python),
        _ => {}
    }
    if matches!(name, "brand.mjs" | "render.mjs" | "lint.mjs") {
        needs.push(Need::Chrome);
    }
    if matches!(name, "render.mjs" | "look.py" | "mix.py" | "voice.py") {
        needs.push(Need::Ffmpeg);
    }
    let report = require(ctx, &needs, &format!("run {name}"))?;
    let args = input.args.clone().unwrap_or_default();
    let long = matches!(name, "render.mjs" | "imagegen.py" | "voice.py" | "brand.mjs");
    let run = run_script(
        ctx,
        skill_dir,
        &report,
        name,
        &args,
        timeout(input, if long { RENDER_TIMEOUT_SECS } else { DEFAULT_TIMEOUT_SECS }),
    )?;
    let status = match run.exit {
        Some(0) => "ok".to_string(),
        Some(code) => format!("exit {code}"),
        None => "exit unknown".to_string(),
    };
    Ok(ToolOutput {
        summary: format!("motionmaxxing {name}: {status}"),
        content: format!("{name} {} → {status}\n\n{}", args.join(" "), run.tail()),
    })
}

fn list_files(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
                .filter(|entry| keep(&entry.file_name().to_string_lossy()))
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_deps() -> DependencyReport {
        DependencyReport::default()
    }

    fn ctx<'a>(workspace: &'a Path, skills: &Path, cancel: &'a ToolCancellation, deps: Option<DependencyReport>) -> MotionContext<'a> {
        MotionContext {
            workspace_root: workspace,
            confined: true,
            cancellation: cancel,
            skills_root: skills.to_path_buf(),
            deps,
        }
    }

    #[test]
    fn bundle_embeds_the_skill_and_its_licenses() {
        let manifest = bundled_skill_manifest().expect("SKILL.md embedded");
        assert!(manifest.contains("name: motionmaxxing"));
        for path in [
            "LICENSE",
            "NOTICE",
            "UPSTREAM.json",
            "scripts/render.mjs",
            "scripts/lint.mjs",
            "scripts/look.py",
            "scripts/providers.sh",
            "runtime/motion.js",
            "templates/film.html",
            "references/motion.md",
            "examples/selftest/index.html",
        ] {
            assert!(BUNDLE.get_file(path).is_some(), "bundled file missing: {path}");
        }
        assert!(BUNDLE.get_dir("docs/media").is_none(), "heavy showcase media stays out of the binary");
        assert!(bundle_version().starts_with("motionmaxxing@8c8ec0f2a6f6+"));
        for script in SCRIPTS {
            assert!(BUNDLE.get_file(format!("scripts/{script}")).is_some(), "allowlisted script missing: {script}");
        }
    }

    #[test]
    fn install_is_a_normal_skill_and_preserves_user_taste() {
        let skills = tempfile::tempdir().unwrap();
        let (dir, state) = ensure_installed_in(skills.path()).unwrap();
        assert_eq!(state, InstallState::Installed);
        assert!(dir.join("SKILL.md").is_file() && dir.join("scripts/render.mjs").is_file());
        // The existing skills loader sees it like any installed skill.
        let meta = crate::tools::skills::discover_in_roots_for_tests(&[skills.path().to_path_buf()]);
        assert!(meta.iter().any(|(name, description)| name == "motionmaxxing" && description.contains("motion")));
        assert_eq!(ensure_installed_in(skills.path()).unwrap().1, InstallState::Current);

        // A Phoenix upgrade refreshes bundled files but keeps the user's verdicts.
        std::fs::write(dir.join("taste/verdicts.md"), "my verdicts").unwrap();
        std::fs::write(dir.join("references/motion.md"), "stale").unwrap();
        std::fs::write(dir.join(BUNDLE_MARKER), "motionmaxxing@old\n").unwrap();
        assert_eq!(ensure_installed_in(skills.path()).unwrap().1, InstallState::Installed);
        assert_eq!(std::fs::read_to_string(dir.join("taste/verdicts.md")).unwrap(), "my verdicts");
        assert_ne!(std::fs::read_to_string(dir.join("references/motion.md")).unwrap(), "stale");

        // A copy the user installed themselves is never overwritten.
        let user = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user.path().join("motionmaxxing")).unwrap();
        std::fs::write(user.path().join("motionmaxxing/SKILL.md"), "---\nname: motionmaxxing\n---\nmine").unwrap();
        assert_eq!(ensure_installed_in(user.path()).unwrap().1, InstallState::UserManaged);
        assert!(std::fs::read_to_string(user.path().join("motionmaxxing/SKILL.md")).unwrap().ends_with("mine"));
    }

    #[test]
    fn guide_loads_skill_and_references_on_demand() {
        let workspace = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let cancel = ToolCancellation::default();
        let out = execute_in(ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())), MotionGraphicsInput::default()).unwrap();
        assert!(out.content.contains("PHOENIX ADAPTER"));
        assert!(out.content.contains("action `review`"));
        assert!(out.content.contains("# motionmaxxing"));
        for file in ["motion", "references/ui-demo.md", "taste/verdicts.md", "runtime/README.md"] {
            let out = execute_in(
                ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
                MotionGraphicsInput { action: Some("guide".into()), file: Some(file.into()), ..Default::default() },
            )
            .unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert!(out.content.len() > 200, "{file} loaded");
        }
        let miss = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("guide".into()), file: Some("nope".into()), ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(miss.contains("references/motion.md"), "miss lists real references: {miss}");
        let escape = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("guide".into()), file: Some("../../etc/passwd".into()), ..Default::default() },
        );
        assert!(escape.is_err());
    }

    #[test]
    fn dependency_check_degrades_gracefully_on_a_bare_machine() {
        let empty = tempfile::tempdir().unwrap();
        let report = DependencyReport::detect_with_path(empty.path().as_os_str());
        assert!(report.node.is_none() && report.ffmpeg.is_none() && !report.python_ok);
        let workspace = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let cancel = ToolCancellation::default();
        let out = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("check".into()), ..Default::default() },
        )
        .expect("check never errors");
        assert!(out.content.contains("NOT READY"));
        assert!(out.content.contains("Node.js 22+") && out.content.contains("ffmpeg") && out.content.contains("Chromium"));
        assert!(!skills.path().join("motionmaxxing").exists(), "check does not install anything");

        // Render-type actions refuse with a clear receipt, never a crash.
        for action in ["render", "still", "lint", "review", "look"] {
            let error = execute_in(
                ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
                MotionGraphicsInput { action: Some(action.into()), ..Default::default() },
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("missing") && error.contains("Nothing was run"), "{action}: {error}");
        }
        // Planning and scaffolding still work without the toolchain.
        let started = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("start".into()), project: Some("films/launch".into()), ..Default::default() },
        )
        .unwrap();
        assert!(started.content.contains("Rendering is not possible"));
        assert!(workspace.path().join("films/launch/index.html").is_file());
        assert!(workspace.path().join("films/launch/runtime/motion.js").is_file());
        assert!(workspace.path().join("films/launch/runtime/vendor/gsap.min.js").is_file());
    }

    #[test]
    fn start_keeps_an_existing_composition_and_stays_in_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let cancel = ToolCancellation::default();
        std::fs::create_dir_all(workspace.path().join("film")).unwrap();
        std::fs::write(workspace.path().join("film/index.html"), "<mine>").unwrap();
        execute_in(ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())), MotionGraphicsInput { action: Some("start".into()), ..Default::default() }).unwrap();
        assert_eq!(std::fs::read_to_string(workspace.path().join("film/index.html")).unwrap(), "<mine>");
        let outside = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("start".into()), project: Some("/tmp/elsewhere-motion".into()), ..Default::default() },
        );
        assert!(outside.unwrap_err().to_string().contains("escapes workspace"));
    }

    #[test]
    fn script_commands_are_quoted_and_allowlisted() {
        let dir = Path::new("/home/me/.phoenix/skills/motionmaxxing");
        let command = script_command(
            dir,
            Some(Path::new("/usr/bin/chromium")),
            "render.mjs",
            &["film/index.html".into(), "film/it's.mp4".into(), "--scale".into(), "0.5".into()],
        );
        assert!(command.starts_with("CHROME_PATH=/usr/bin/chromium node /home/me/.phoenix/skills/motionmaxxing/scripts/render.mjs film/index.html"));
        assert!(command.contains("'film/it'\\''s.mp4'"));
        assert!(command.contains("__MOTION_EXIT="));
        assert!(script_command(dir, None, "look.py", &[]).starts_with("python3 "));
        assert!(script_command(dir, None, "providers.sh", &[]).starts_with("bash "));

        let workspace = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let cancel = ToolCancellation::default();
        let error = execute_in(
            ctx(workspace.path(), skills.path(), &cancel, Some(bare_deps())),
            MotionGraphicsInput { action: Some("script".into()), script: Some("../../bin/sh".into()), ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a motionmaxxing script"));
    }

    #[test]
    fn motion_graphics_is_registered_for_every_coworker() {
        assert!(crate::tools::universal_agent_tool_names().iter().any(|t| t == TOOL_NAME));
        assert!(crate::tools::validate_tool_allowlist(&[TOOL_NAME.to_string()]).is_ok());
        assert_eq!(
            crate::tools::required_permission_for_tool(TOOL_NAME),
            crate::tools::PermissionMode::Workspace
        );
        let definition = crate::tools::tool_definitions_for_agent(&[TOOL_NAME.to_string()])
            .into_iter()
            .find(|tool| tool.name == TOOL_NAME)
            .expect("native definition");
        for trigger in ["launch films", "brand stings", "kinetic type", "UI demo", "animated heroes", "loading", "transition"] {
            assert!(definition.description.contains(trigger), "description names `{trigger}`");
        }
        let actions = definition.parameters["properties"]["action"]["enum"].as_array().unwrap();
        for action in ["guide", "check", "start", "still", "review", "render", "look", "lint", "script"] {
            assert!(actions.iter().any(|a| a == action), "schema offers `{action}`");
        }
        // Built-in coworkers, the chief of staff, and any custom coworker.
        for role in crate::sub_agents::registry::BUILTIN_ROLES {
            let agent = crate::sub_agents::registry::builtin_by_label(role).expect("role resolves");
            let tools = crate::sub_agents::specialist_config(agent).spec.tool_allowlist;
            assert!(tools.iter().any(|t| t == TOOL_NAME), "`{role}` sees motion_graphics");
        }
        let orchestrator = crate::orchestrator::Orchestrator::new("model", crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT);
        assert!(orchestrator.spec().tool_allowlist.iter().any(|t| t == TOOL_NAME));
        let custom = crate::tools::merge_with_universal_tools(crate::sub_agents::registry::default_custom_tools());
        assert!(custom.iter().any(|t| t == TOOL_NAME));
        // Not hidden behind a progressive-disclosure family.
        assert!(crate::tools::deferral::family_of(TOOL_NAME).is_none());
    }

    #[test]
    fn executor_dispatches_the_dependency_check() {
        let workspace = tempfile::tempdir().unwrap();
        let executor = crate::tools::ToolExecutor::new(workspace.path()).unwrap();
        let result = executor.execute(crate::runtime::ToolCall {
            tool_name: TOOL_NAME.to_string(),
            input: serde_json::json!({"action": "check"}),
        });
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("motionmaxxing skill"), "{}", result.output);
        assert!(result.output.contains("READY") , "{}", result.output);
    }

    #[test]
    fn unknown_actions_name_the_real_ones() {
        let error = Action::parse(Some("explode")).unwrap_err().to_string();
        assert!(error.contains("review") && error.contains("guide"));
        assert_eq!(Action::parse(None).unwrap(), Action::Guide);
        assert_eq!(Action::parse(Some("mog-check")).unwrap(), Action::Review);
    }

    /// Real end-to-end render through the bash executor. Skips (passes) when
    /// the machine lacks Node 22+/Chrome/ffmpeg/Python, so CI without media
    /// tools stays green; set PHOENIX_MOTION_RENDER_TEST=1 to require it.
    #[test]
    fn real_render_of_the_selftest_when_the_toolchain_exists() {
        let report = DependencyReport::detect();
        if !report.ready() {
            assert!(std::env::var_os("PHOENIX_MOTION_RENDER_TEST").is_none(), "toolchain required:\n{}", report.render());
            eprintln!("skipping real motion render: {}", report.render());
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let cancel = ToolCancellation::default();
        let (dir, _) = ensure_installed_in(skills.path()).unwrap();
        // The selftest page references ../../runtime, so mirror that layout.
        copy_tree(&dir.join("runtime"), &workspace.path().join("runtime")).unwrap();
        copy_tree(&dir.join("examples/selftest"), &workspace.path().join("examples/selftest")).unwrap();
        let mut context = ctx(workspace.path(), skills.path(), &cancel, Some(report));
        context.confined = false; // the skills root is a tempdir outside the workspace
        let out = execute_in(
            context,
            MotionGraphicsInput {
                action: Some("review".into()),
                project: Some("examples/selftest".into()),
                from: Some(0.0),
                to: Some(2.0),
                scale: Some(0.25),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("review failed: {e:#}"));
        eprintln!("{}\n{}", out.summary, out.content);
        assert!(out.content.contains("G0"), "{}", out.content);
        assert!(out.content.contains("G5 no page chrome (lint.mjs)"), "{}", out.content);
        assert!(workspace.path().join("examples/selftest/draft.mp4").is_file());
        assert!(workspace.path().join("examples/selftest/look/sheet.jpg").is_file());
    }

    /// Full agent path: Workspace permission mode through the ToolExecutor,
    /// so the scripts run inside Phoenix's Workspace shell isolation
    /// (Bubblewrap when installed) with the skill read from PHOENIX_HOME.
    /// Opt-in (slow): PHOENIX_MOTION_RENDER_TEST=1.
    #[test]
    fn workspace_mode_review_of_the_demo_through_the_executor() {
        if std::env::var_os("PHOENIX_MOTION_RENDER_TEST").is_none() {
            return;
        }
        let report = DependencyReport::detect();
        assert!(report.ready(), "toolchain required:\n{}", report.render());
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        crate::config::private_io::prepare_phoenix_directory(&home.path().join("skills")).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let executor = crate::tools::ToolExecutor::new(workspace.path()).unwrap();
        let call = |input: serde_json::Value| {
            executor.execute(crate::runtime::ToolCall { tool_name: TOOL_NAME.to_string(), input })
        };
        let started = call(serde_json::json!({"action": "start"}));
        let skill_md = home.path().join("skills/motionmaxxing/SKILL.md");
        let probe = executor.execute(crate::runtime::ToolCall {
            tool_name: "bash".to_string(),
            input: serde_json::json!({"command": format!("test -f {} && echo skill-visible", skill_md.display())}),
        });
        assert!(probe.output.contains("skill-visible"), "skills root visible to Workspace bash: {}", probe.output);
        eprintln!("isolation: {}", probe.output.lines().next().unwrap_or_default());
        assert!(started.success, "{}", started.output);
        assert!(home.path().join("skills/motionmaxxing/SKILL.md").is_file(), "installed into PHOENIX_HOME/skills");
        let demo = std::fs::read_to_string(home.path().join("skills/motionmaxxing/examples/demo/index.html"))
            .unwrap()
            .replace("../../runtime/", "./runtime/");
        std::fs::write(workspace.path().join("film/index.html"), demo).unwrap();
        let stills = call(serde_json::json!({"action": "still", "times": "1,4.5,8"}));
        assert!(stills.success, "{}", stills.output);
        let reviewed = call(serde_json::json!({"action": "review", "expect": 9, "scale": 0.5}));
        eprintln!("{}", reviewed.output);
        assert!(reviewed.success, "{}", reviewed.output);
        assert!(reviewed.output.contains("G0 render exists: PASS"), "{}", reviewed.output);
        assert!(workspace.path().join("film/draft.mp4").is_file());
        assert!(workspace.path().join("film/look/sheet.jpg").is_file());
    }
}
