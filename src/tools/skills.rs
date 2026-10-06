//! `skill` — SKILL.md-compatible skills loader (the open Agent Skills format).
//!
//! A skill is a directory holding a `SKILL.md` (YAML frontmatter with `name`
//! and `description`, markdown instructions below, optional references/
//! scripts/assets subdirectories). The format is an open, cross-vendor
//! standard, so portable skills work in Phoenix unchanged: drop them under
//! `~/.phoenix/skills/<name>/` (global) or `<workspace>/.phoenix/skills/`
//! (project-local; shadows global on name collision).
//!
//! Progressive disclosure, same discipline as `design_reference`: `list`
//! returns one line per skill (name + description only); the full SKILL.md
//! and its reference files load on demand. Marketplace discovery/auto-install
//! is deliberately NOT here — installs go through the approval board (a skill
//! is arbitrary instructions; nothing self-installs silently).

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use super::ToolOutput;

/// Popularity floor for registry installs. A skill is arbitrary instructions
/// injected into an agent — install count is the community's supply-chain
/// vetting. Below this bar a skills.sh listing is REFUSED outright (user
/// authorization does not override a known-unpopular listing; sources the
/// registry has never seen can still be installed when the user explicitly
/// named them).
const MIN_REGISTRY_INSTALLS: u64 = 500;
const MAX_SKILL_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SKILL_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SKILL_FILES: usize = 4_096;
const MAX_SKILL_DEPTH: u8 = 32;
const MAX_DISCOVERED_SKILLS: usize = 2_048;
const MAX_CARDS_STATE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CARDED_SKILLS: usize = 4_096;
const MAX_REMOTE_SOURCE_BYTES: usize = 2_048;
const MAX_GIT_BRANCH_BYTES: usize = 255;
const MAX_GIT_SUBPATH_BYTES: usize = 1_024;
const MAX_REGISTRY_QUERY_BYTES: usize = 256;
const MAX_REGISTRY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_REGISTRY_HITS: usize = 2_048;
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_CLONE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const GIT_CLONE_TERM_GRACE: Duration = Duration::from_millis(750);
const GIT_CLONE_KILL_GRACE: Duration = Duration::from_millis(750);

fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        bail!("skill name must be 1..=128 bytes");
    }
    if name == "." || name == ".." {
        bail!("skill name cannot be a dot component");
    }
    if !name.as_bytes()[0].is_ascii_alphanumeric() {
        bail!("skill name must start with an ASCII letter or digit");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("skill name may contain only ASCII letters, digits, '-', '_' and '.'");
    }
    Ok(())
}

fn ensure_safe_relative_file(root: &Path, path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!("skill root is not a real directory: {}", root.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting skill root {}", root.display()))
        }
    }
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} escapes skill root {}", path.display(), root.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!(
                "skill path contains an unsafe component: {}",
                path.display()
            );
        }
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("refusing symlink in skill content: {}", current.display())
            }
            Ok(metadata) if current == path => return Ok(metadata.file_type().is_file()),
            Ok(metadata) if !metadata.file_type().is_dir() => {
                bail!("non-directory skill path component: {}", current.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting skill file {}", current.display()))
            }
        }
    }
    Ok(false)
}

fn read_skill_file(root: &Path, path: &Path) -> Result<String> {
    if !ensure_safe_relative_file(root, path)? {
        bail!("skill file does not exist: {}", path.display());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening skill file {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspecting skill file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("skill content is not a regular file: {}", path.display());
    }
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        bail!(
            "skill file {} is {} bytes; maximum is {MAX_SKILL_FILE_BYTES}",
            path.display(),
            metadata.len()
        );
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_SKILL_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading skill file {}", path.display()))?;
    if bytes.len() as u64 > MAX_SKILL_FILE_BYTES {
        bail!("skill file {} grew beyond its size limit", path.display());
    }
    String::from_utf8(bytes).with_context(|| format!("skill file {} is not UTF-8", path.display()))
}

#[derive(Debug, Deserialize)]
pub struct SkillInput {
    /// Skill name (directory name). Omit to list all installed skills.
    #[serde(default)]
    pub name: Option<String>,
    /// File inside the skill (e.g. `references/setup.md`). Omit for SKILL.md.
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

/// Skill roots in shadowing order: workspace-local first, then global.
fn skill_roots(workspace_root: &Path) -> Vec<PathBuf> {
    vec![
        workspace_root.join(".phoenix/skills"),
        crate::config::phoenix_home().join("skills"),
    ]
}

/// Minimal frontmatter scan: `name:`/`description:` between the `---` fences.
/// Full YAML is overkill — the spec only requires these two fields, and a
/// lenient scan tolerates the marketplace's formatting spread.
fn parse_frontmatter(skill_md: &str) -> (Option<String>, Option<String>) {
    let mut in_frontmatter = false;
    let mut name = None;
    let mut description = None;
    for line in skill_md.lines().take(60) {
        let trimmed = line.trim();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            name = Some(
                value
                    .trim()
                    .trim_matches(['"', '\''])
                    .chars()
                    .take(256)
                    .collect(),
            );
        } else if let Some(value) = trimmed.strip_prefix("description:") {
            description = Some(
                value
                    .trim()
                    .trim_matches(['"', '\''])
                    .chars()
                    .take(1_024)
                    .collect(),
            );
        }
    }
    (name, description)
}

/// Every installed skill, deduplicated by name (workspace shadows global).
pub fn discover(workspace_root: &Path) -> Vec<SkillMeta> {
    discover_in(&skill_roots(workspace_root))
}

/// The full SKILL.md content of an installed skill, by name (frontmatter name
/// or directory name). Used by the prompt pinner to re-materialize loaded
/// skills every assembly; `None` workspace falls back to the global root.
pub fn skill_manifest_content(workspace_root: Option<&Path>, name: &str) -> Option<String> {
    let roots = match workspace_root {
        Some(root) => skill_roots(root),
        None => vec![crate::config::phoenix_home().join("skills")],
    };
    let skills = discover_in(&roots);
    let meta = skills.iter().find(|s| s.name == name).or_else(|| {
        skills.iter().find(|s| {
            s.dir
                .file_name()
                .is_some_and(|d| d.to_string_lossy() == name)
        })
    })?;
    read_skill_file(&meta.dir, &meta.dir.join("SKILL.md")).ok()
}

/// Root-parameterized discovery — keeps tests hermetic from the real
/// `~/.phoenix/skills` (which holds the user's actual installs).
fn discover_in(roots: &[PathBuf]) -> Vec<SkillMeta> {
    let mut skills: Vec<SkillMeta> = Vec::new();
    for root in roots {
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                tracing::warn!(
                    "skills: refusing non-directory or symlinked root {}",
                    root.display()
                );
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!("skills: cannot inspect root {} ({error})", root.display());
                continue;
            }
        }
        if let Err(error) = crate::config::private_io::reject_symlink_components(root) {
            tracing::warn!(
                "skills: refusing symlinked root {} ({error:#})",
                root.display()
            );
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.take(MAX_DISCOVERED_SKILLS.saturating_mul(4)) {
            if skills.len() >= MAX_DISCOVERED_SKILLS {
                tracing::warn!("skills: discovery reached the {MAX_DISCOVERED_SKILLS}-skill limit");
                break;
            }
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let dir = entry.path();
            let manifest = dir.join("SKILL.md");
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') {
                continue;
            }
            if let Err(error) = validate_skill_name(&dir_name) {
                tracing::warn!("skills: skipping invalid directory name {dir_name:?} ({error})");
                continue;
            }
            if skills.iter().any(|s| s.name == dir_name) {
                continue; // earlier root shadows
            }
            let raw = match read_skill_file(&dir, &manifest) {
                Ok(raw) => raw,
                Err(error) => {
                    tracing::warn!(
                        "skills: skipping unsafe/unreadable {} ({error:#})",
                        manifest.display()
                    );
                    continue;
                }
            };
            let (fm_name, fm_description) = parse_frontmatter(&raw);
            let display_name = match fm_name {
                Some(name) if validate_skill_name(&name).is_ok() => name,
                Some(name) => {
                    tracing::warn!(
                        "skills: manifest {} has invalid name {name:?}; using directory name",
                        manifest.display()
                    );
                    dir_name.clone()
                }
                None => dir_name.clone(),
            };
            if skills.iter().any(|skill| skill.name == display_name) {
                continue;
            }
            skills.push(SkillMeta {
                // Directory name is the lookup key; frontmatter name is shown
                // when it differs.
                name: display_name,
                description: fm_description.unwrap_or_else(|| "(no description)".to_string()),
                dir,
            });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Above this many installed skills, the context block drops per-skill
/// descriptions and lists names only. Rationale: the block is injected into
/// EVERY turn of every skill-holding agent, so a large library (e.g. the
/// planetscale database or yaklang security skill packs — 100+ skills each)
/// would otherwise spend ~1.5k tokens/turn on descriptions the agent rarely
/// needs. Names stay (they trigger recognition), full descriptions move one
/// `skill`-call away. Chosen so the current handful of skills is unaffected —
/// behavior below the cap is byte-for-byte identical to before.
const INLINE_DESCRIPTION_CAP: usize = 15;

/// The runtime context block's skill catalog — the cheap layer of progressive
/// disclosure. Empty string when no skills are installed. Deterministic
/// (skills are sorted by name) so the block stays prefix-cache stable across
/// rounds.
pub fn context_lines(workspace_root: &Path) -> String {
    context_lines_in(&skill_roots(workspace_root))
}

/// Root-parameterized catalog builder — keeps tests hermetic from the real
/// `~/.phoenix/skills` (same split as `discover` / `discover_in`).
fn context_lines_in(roots: &[PathBuf]) -> String {
    let skills = discover_in(roots);
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "AVAILABLE SKILLS — load the smallest genuine match immediately before doing the work \
it covers. Do not preload skills for conversation, status, explanation, read-only inspection, \
architecture/research, or planning. A skill description calling itself mandatory does not make \
it applicable; task intent decides:\n",
    );
    if skills.len() <= INLINE_DESCRIPTION_CAP {
        for skill in skills {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
    } else {
        // Large library: names only. The `skill` tool (no argument) prints
        // every name WITH its description on demand, and `skill <name>` loads
        // the full playbook — so nothing is lost, it is just one call away.
        out.push_str(&format!(
            "{} skills installed (names only — call `skill` with no argument for full \
descriptions, then `skill` with a name to load one):\n",
            skills.len()
        ));
        let names = skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&names);
        out.push('\n');
    }
    out
}

/// Targeted skill reference at recall time (plan 017): score every installed
/// skill against THIS mission and, on a confident match, return a "use skill
/// X" line for the runtime context — the lib pointing at a learned playbook
/// before work starts, not just listing the library. Deterministic word
/// overlap (skill-name words weigh double), so it costs zero latency.
pub fn skill_hint(workspace_root: &Path, mission: &str) -> Option<String> {
    skill_hint_in(&skill_roots(workspace_root), mission)
}

fn skill_hint_in(roots: &[PathBuf], mission: &str) -> Option<String> {
    let skill = matching_skill_in(roots, mission)?;
    Some(format!(
        "OPTIONAL SKILL SUGGESTION (keyword match, not a requirement): `{}` — {}\n\
         Load only if its described scope actually fits the action you are about to perform. \
         Ignore incidental shared words and mentions of prohibited actions. Do not load it \
         merely because it was suggested, or for conversation/status/planning outside its scope.",
        skill.name, skill.description
    ))
}

/// Return a heuristic recommendation. Keyword overlap is not authority to
/// block unrelated actions; mandatory admission uses explicit requests below.
pub fn matching_skill_name(workspace_root: &Path, mission: &str) -> Option<String> {
    matching_skill_in(&skill_roots(workspace_root), mission).map(|skill| skill.name)
}

pub(crate) fn explicitly_requested_skill_names(workspace_root: &Path, mission: &str) -> Vec<String> {
    explicitly_requested_skills_in(&skill_roots(workspace_root), mission)
}

fn explicitly_requested_skills_in(roots: &[PathBuf], mission: &str) -> Vec<String> {
    discover_in(roots).into_iter().filter(|skill| {
        mission_explicitly_requests_skill(mission, &skill.name)
            && !mission_explicitly_excludes_skill(mission, &skill.name)
    }).map(|skill| skill.name).collect()
}

fn matching_skill_in(roots: &[PathBuf], mission: &str) -> Option<SkillMeta> {
    let mission_words: std::collections::HashSet<String> = significant_words(mission).collect();
    if mission_words.is_empty() {
        return None;
    }
    let non_building_design_discussion = is_non_building_design_discussion(mission);
    let mut best: Option<(usize, SkillMeta)> = None;
    for skill in discover_in(roots) {
        // Delegation briefs often name a tempting playbook specifically to say
        // that it does *not* apply. Counting that name as positive evidence is
        // the exact opposite of the user's instruction and used to force an ML
        // experiment skill into ordinary AI-news research.
        if mission_explicitly_excludes_skill(mission, &skill.name) {
            continue;
        }
        // Visual work already has the bounded Taste contract and explicit
        // design-reference routing. Automatically stacking a broad installed
        // design skill on top was the exact four-playbook cursor failure. A
        // user can still request one by name; generic "design/frontend/UI"
        // overlap is never enough to inject or gate it.
        if is_visual_build_skill(&skill) && !mission_explicitly_requests_skill(mission, &skill.name)
        {
            continue;
        }
        if non_building_design_discussion && is_visual_build_skill(&skill) {
            continue;
        }
        let name_hits = significant_words(&skill.name.replace(['-', '_'], " "))
            .filter(|w| mission_words.contains(w))
            .count();
        // A description may repeat a generic qualifier several times (for
        // example candidate-*only*, code-*only*, run-*only*). Repetition is
        // not independent evidence that the user's task matches the skill.
        let description_words: std::collections::HashSet<String> =
            significant_words(&skill.description).collect();
        let description_hits = description_words
            .iter()
            .filter(|word| mission_words.contains(*word))
            .count();
        let score = name_hits * 2 + description_hits;
        let canonical = skill.name.to_ascii_lowercase();
        let spaced = canonical.replace(['-', '_'], " ");
        let lower_mission = mission.to_ascii_lowercase();
        let explicitly_named = lower_mission.contains(&canonical)
            || (spaced != canonical && lower_mission.contains(&spaced));
        // One generic name/description collision is not activation evidence.
        // Require an explicit skill name, multiple distinctive name tokens, or
        // unusually strong description overlap. This keeps broad manifests
        // such as browsing/design playbooks out of unrelated school turns.
        let confident = explicitly_named
            || name_hits >= 2
            || (name_hits >= 1 && description_hits >= 3)
            || description_hits >= 5;
        if confident && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, skill));
        }
    }
    best.map(|(_, skill)| skill)
}

fn is_non_building_design_discussion(mission: &str) -> bool {
    let lower = mission.to_ascii_lowercase();
    let advisory = [
        "what do you think",
        "think about how",
        "how would you",
        "architecture report",
        "architecture discussion",
        "integration approach",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let execution = [
        "go ahead and build",
        "build me",
        "create the ui",
        "create a ui",
        "redesign the",
        "then implement",
        "and implement the ui",
        "make the changes",
        "change the ui",
        "update the ui",
        "fix the ui",
    ]
    .iter()
    .any(|term| lower.contains(term));
    advisory && !execution
}

fn is_visual_build_skill(skill: &SkillMeta) -> bool {
    let text = format!("{} {}", skill.name, skill.description).to_ascii_lowercase();
    text.split(|ch: char| !ch.is_alphanumeric())
        .any(|word| matches!(word, "design" | "frontend" | "ui" | "ux" | "website"))
}

pub(crate) fn named_skill_is_visual_build(workspace_root: Option<&Path>, name: &str) -> bool {
    let name_looks_visual = name
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|word| matches!(word, "design" | "frontend" | "ui" | "ux" | "kombai"));
    let Some(root) = workspace_root else {
        return name_looks_visual;
    };
    discover(root)
        .iter()
        .find(|skill| skill.name == name || skill.dir.file_name().is_some_and(|dir| dir == name))
        .is_some_and(is_visual_build_skill)
        || name_looks_visual
}

fn mission_explicitly_excludes_skill(mission: &str, skill_name: &str) -> bool {
    let mission = mission.to_ascii_lowercase();
    let canonical = skill_name.to_ascii_lowercase();
    let spaced = canonical.replace(['-', '_'], " ");
    let names = [canonical.as_str(), spaced.as_str()];
    names.iter().any(|name| {
        [
            format!("{name} is not applicable"),
            format!("{name} is explicitly not applicable"),
            format!("{name} skill is not applicable"),
            format!("{name} skill is explicitly not applicable"),
            format!("{name} does not apply"),
            format!("{name} skill does not apply"),
            format!("do not use {name}"),
            format!("don't use {name}"),
            format!("without {name}"),
            format!("exclude {name}"),
        ]
        .iter()
        .any(|phrase| mission.contains(phrase))
    })
}

fn mission_explicitly_requests_skill(mission: &str, skill_name: &str) -> bool {
    let mission = mission.to_ascii_lowercase();
    let canonical = skill_name.to_ascii_lowercase();
    let spaced = canonical.replace(['-', '_'], " ");
    [canonical.as_str(), spaced.as_str()].iter().any(|name| {
        [
            format!("use {name}"),
            format!("load {name}"),
            format!("apply {name}"),
            format!("with the {name}"),
            format!("{name} skill"),
        ]
        .iter()
        .any(|phrase| mission.contains(phrase))
    })
}

/// Lowercased words ≥4 chars with common task words dropped — the cheap
/// signal layer for `skill_hint`'s overlap scoring.
fn significant_words(text: &str) -> impl Iterator<Item = String> + '_ {
    const STOP: &[&str] = &[
        "with", "that", "this", "from", "into", "make", "build", "create", "please", "need",
        "want", "using", "some", "then", "them", "have", "will", "what", "when", "where", "which",
        "your", "their", "there", "about", "should", "agent", "skill", "workflow", "model",
        "research", "explore", "only", "task", "work", "data", "exact", "include", "return",
        "read", "files", "call", "calls", "list", "action",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.chars().count() >= 4 && !STOP.contains(&w.as_str()))
}

pub fn execute(workspace_root: &Path, input: SkillInput) -> Result<ToolOutput> {
    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let Some(name) = name else {
        let skills = discover(workspace_root);
        if skills.is_empty() {
            return Ok(ToolOutput {
                summary: "no skills installed".to_string(),
                content: format!(
                    "No skills installed. Skills are SKILL.md directories (the open Agent Skills \
format — portable Agent Skills packages) placed under {} or <workspace>/.phoenix/skills/.",
                    crate::config::phoenix_home().join("skills").display()
                ),
            });
        }
        let listing = skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(ToolOutput {
            summary: format!("{} skills installed", skills.len()),
            content: format!("Installed skills (pass `name` to load one's SKILL.md):\n{listing}"),
        });
    };

    let skills = discover(workspace_root);
    let Some(skill) = skills
        .iter()
        .find(|s| s.name == name || s.dir.file_name().is_some_and(|d| d == name))
    else {
        let known = skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "no skill named `{name}`. Installed: {}",
            if known.is_empty() { "(none)" } else { &known }
        );
    };

    let rel = input
        .file
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or("SKILL.md");
    let target = resolve_in_skill(&skill.dir, rel)?;
    let content = match read_skill_file(&skill.dir, &target) {
        Ok(content) => content,
        Err(_error)
            if std::fs::symlink_metadata(&target)
                .is_err_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Err(anyhow!(miss_message(skill, rel)));
        }
        Err(error) => return Err(error),
    };
    Ok(ToolOutput {
        summary: format!("skill {} / {}", skill.name, rel),
        content,
    })
}

#[derive(Debug, Deserialize)]
pub struct SkillSearchInput {
    /// What you're looking for, e.g. `remotion`, `pdf`, `stripe`.
    pub query: String,
    /// Max results (default 8).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
struct RegistryHit {
    id: String,
    name: String,
    #[serde(default)]
    installs: u64,
    #[serde(default)]
    source: String,
}

/// One skills.sh registry query. Shared by `skill_search` (display) and
/// `skill_install` (popularity verification).
async fn registry_search(query: &str) -> Result<Vec<RegistryHit>> {
    validate_registry_query(query)?;
    let client = reqwest::Client::builder()
        .user_agent("PhoenixAgent/0.1 (skill search)")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("building HTTP client")?;
    let response = client
        .get("https://skills.sh/api/search")
        .query(&[("q", query)])
        .send()
        .await
        .context("skills.sh search request failed")?;
    if !response.status().is_success() {
        bail!("skills.sh search returned HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REGISTRY_RESPONSE_BYTES as u64)
    {
        bail!("skills.sh search response exceeds the {MAX_REGISTRY_RESPONSE_BYTES}-byte limit");
    }
    #[derive(Deserialize)]
    struct SearchResponse {
        #[serde(default)]
        skills: Vec<RegistryHit>,
    }
    let mut response = response;
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_REGISTRY_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading skills.sh search response")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_REGISTRY_RESPONSE_BYTES {
            bail!("skills.sh search response exceeds the {MAX_REGISTRY_RESPONSE_BYTES}-byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed: SearchResponse =
        serde_json::from_slice(&bytes).context("skills.sh search returned unexpected JSON")?;
    if parsed.skills.len() > MAX_REGISTRY_HITS {
        bail!(
            "skills.sh search returned {} entries; maximum is {MAX_REGISTRY_HITS}",
            parsed.skills.len()
        );
    }
    Ok(filter_registry_hits(parsed.skills))
}

/// The public registry can contain listings for package hosts Phoenix does
/// not support (for example, a `smithery.ai/...` id mixed into otherwise
/// valid GitHub results). Such an entry must never become installable, but it
/// also must not make every safe result disappear. Validate each listing and
/// retain only the exact GitHub source shape accepted by `skill_install`.
fn filter_registry_hits(hits: Vec<RegistryHit>) -> Vec<RegistryHit> {
    let supplied = hits.len();
    let valid = hits
        .into_iter()
        .filter(|hit| validate_registry_hit(hit).is_ok())
        .collect::<Vec<_>>();
    let rejected = supplied.saturating_sub(valid.len());
    if rejected > 0 {
        tracing::warn!(
            rejected,
            "skills: skipped registry listings with unsupported or unsafe metadata"
        );
    }
    valid
}

fn validate_registry_query(query: &str) -> Result<()> {
    if query.is_empty() || query.len() > MAX_REGISTRY_QUERY_BYTES {
        bail!("skill registry query must be 1..={MAX_REGISTRY_QUERY_BYTES} bytes");
    }
    if query.chars().any(char::is_control) {
        bail!("skill registry query cannot contain control characters");
    }
    Ok(())
}

fn validate_registry_hit(hit: &RegistryHit) -> Result<()> {
    if hit.id.is_empty() || hit.id.len() > MAX_REMOTE_SOURCE_BYTES {
        bail!("skills.sh returned an invalid skill id length");
    }
    parse_git_source(&hit.id).context("skills.sh returned an invalid skill id")?;
    if hit.name.is_empty()
        || hit.name.len() > 256
        || hit.name.chars().any(char::is_control)
        || hit.source.len() > MAX_REMOTE_SOURCE_BYTES
        || hit.source.chars().any(char::is_control)
    {
        bail!("skills.sh returned invalid display metadata");
    }
    Ok(())
}

/// Search the skills.sh registry (the cross-agent Agent Skills directory —
/// the same skills `npx skills add` installs). Returns ranked matches with
/// the exact id `skill_install` accepts; listings below the popularity floor
/// are shown but marked banned (skill_install refuses them).
pub async fn search(input: SkillSearchInput) -> Result<ToolOutput> {
    let query = input.query.trim();
    if query.is_empty() {
        bail!("skill_search needs a non-empty `query`");
    }
    let limit = input.limit.unwrap_or(8).clamp(1, 25);
    let hits = registry_search(query).await?;

    if hits.is_empty() {
        return Ok(ToolOutput {
            summary: format!("no skills match `{query}`"),
            content: format!(
                "skills.sh has no match for `{query}`. Try a broader query, or install \
directly from a GitHub repo with skill_install."
            ),
        });
    }
    let rows = hits
        .iter()
        .take(limit)
        .map(|hit| {
            if hit.installs < MIN_REGISTRY_INSTALLS {
                format!(
                    "- {} ({} installs) — from {} — ✗ BANNED: below the {MIN_REGISTRY_INSTALLS}-install trust bar, do not install",
                    hit.name, hit.installs, hit.source
                )
            } else {
                format!(
                    "- {} ({} installs) — from {} — install with skill_install source `{}`",
                    hit.name, hit.installs, hit.source, hit.id
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput {
        summary: format!("{} skills.sh matches for `{query}`", hits.len().min(limit)),
        content: format!(
            "skills.sh results for `{query}` (ranked by installs; entries under \
{MIN_REGISTRY_INSTALLS} installs are banned — pick a passing one or none):\n{rows}"
        ),
    })
}

#[derive(Debug, Deserialize)]
pub struct SkillInstallInput {
    /// Where the skill lives: a GitHub URL (`https://github.com/owner/repo`
    /// or a `/tree/<branch>/<subpath>` deep link), `owner/repo[/subpath]`
    /// shorthand, or a local directory path containing SKILL.md.
    pub source: String,
    /// Install under this name (default: the skill directory's name).
    #[serde(default)]
    pub name: Option<String>,
    /// Set to true ONLY when the USER explicitly named this exact source
    /// themselves. Lets a source the skills.sh registry has never indexed
    /// through the popularity gate; it does NOT override a registry listing
    /// that is below the install floor.
    #[serde(default)]
    pub user_authorized: bool,
}

/// The popularity gate's pure decision: `registry_installs` is the skills.sh
/// install count when the source was found in the registry, None when the
/// registry doesn't know it (or couldn't be reached).
fn popularity_verdict(
    registry_installs: Option<u64>,
    user_authorized: bool,
    label: &str,
) -> Result<()> {
    match registry_installs {
        Some(installs) if installs < MIN_REGISTRY_INSTALLS => bail!(
            "REFUSED: `{label}` has {installs} installs on skills.sh — below the \
{MIN_REGISTRY_INSTALLS}-install trust bar. Unpopular skills are banned (supply-chain \
risk: a skill is arbitrary agent instructions). Pick a listing with \
{MIN_REGISTRY_INSTALLS}+ installs via skill_search, or proceed without a skill."
        ),
        Some(_) => Ok(()),
        None if user_authorized => Ok(()),
        None => bail!(
            "REFUSED: `{label}` is not verifiable on skills.sh, so its popularity is \
unknown. Only install unlisted sources the USER explicitly named (then retry with \
`user_authorized: true`); otherwise pick a registry listing with \
{MIN_REGISTRY_INSTALLS}+ installs via skill_search."
        ),
    }
}

/// skills.sh install count for `owner/repo/skill-name`, when the registry has
/// that exact id. Network/parse failures = None (unverifiable, not "popular").
async fn registry_installs_for(owner_repo_skill: &str) -> Option<u64> {
    let skill_name = owner_repo_skill.rsplit('/').next()?;
    let hits = registry_search(skill_name).await.ok()?;
    hits.into_iter()
        .find(|hit| hit.id.eq_ignore_ascii_case(owner_repo_skill))
        .map(|hit| hit.installs)
}

/// Oracle management acquisition (plan 017): the ONLY background install
/// path, and it is narrower than the agent tool — registry-listed hits above
/// the popularity floor only (no user_authorized escape, no arbitrary URLs),
/// max one call per management pass, skipping anything already installed.
/// Returns the receipt line, or None when nothing qualifying matched.
pub async fn acquire_from_registry(workspace_root: &Path, query: &str) -> Result<Option<String>> {
    let installed: Vec<String> = discover(workspace_root)
        .into_iter()
        .map(|s| s.name.to_ascii_lowercase())
        .collect();
    let hits = registry_search(query).await?;
    let Some(hit) = hits.into_iter().find(|hit| {
        hit.installs >= MIN_REGISTRY_INSTALLS && !installed.contains(&hit.name.to_ascii_lowercase())
    }) else {
        return Ok(None);
    };
    let receipt = install(SkillInstallInput {
        source: hit.id.clone(),
        name: None,
        user_authorized: false, // registry-listed above the floor passes on its own
    })
    .await?;
    Ok(Some(format!(
        "acquired `{}` ({} installs) for \"{query}\" — {}",
        hit.name, hit.installs, receipt.summary
    )))
}

/// Install a skill into `~/.phoenix/skills/<name>/`. Sources are git repos
/// (cloned shallow) or local directories. User/agent-initiated, plus the
/// Oracle's nightly `acquire_from_registry` lane (registry-verified popular
/// skills only); every remote source passes the skills.sh popularity gate
/// before anything is cloned.
pub async fn install(input: SkillInstallInput) -> Result<ToolOutput> {
    let source = input.source.trim();
    if source.is_empty() {
        bail!("skill_install needs a `source` (GitHub URL, owner/repo, or local path)");
    }
    if let Some(name) = &input.name {
        validate_skill_name(name).context("invalid requested skill name")?;
    }

    let local = Path::new(source);
    let remote = if local.is_dir() {
        None
    } else {
        Some(parse_git_source(source)?)
    };
    // Popularity gate — before the clone, so a banned source never even
    // reaches the machine. Local directories are the user's own files.
    if let Some(remote) = &remote {
        let owner_repo = remote
            .repo_url
            .trim_start_matches("https://github.com/")
            .trim_end_matches(".git");
        let skill_segment = remote
            .subpath
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .map(str::to_string)
            .or_else(|| input.name.clone());
        let installs = match &skill_segment {
            Some(seg) => registry_installs_for(&format!("{owner_repo}/{seg}")).await,
            None => None,
        };
        let label = skill_segment
            .map(|seg| format!("{owner_repo}/{seg}"))
            .unwrap_or_else(|| owner_repo.to_string());
        popularity_verdict(installs, input.user_authorized, &label)?;
    }
    let (skill_dir, _clone_guard) = if local.is_dir() {
        (local.to_path_buf(), None)
    } else {
        let remote = remote.as_ref().context("validated remote source missing")?;
        let tmp = tempfile::tempdir().context("creating temp dir for clone")?;
        let repo_root = tmp.path().join("repo");
        let clone = run_git_clone(remote, &repo_root).await?;
        if clone.reason == CloneStopReason::TimedOut {
            bail!(
                "git clone timed out after {} seconds for {}",
                GIT_CLONE_TIMEOUT.as_secs(),
                remote.repo_url
            );
        }
        if !clone.status.success() {
            bail!(
                "git clone failed for {} with status {} — check the URL and network",
                remote.repo_url,
                clone.status
            );
        }
        if clone.descendants_terminated > 0 {
            tracing::warn!(
                "skill clone terminated {} leftover same-group process(es) after git exited",
                clone.descendants_terminated
            );
        }
        validate_checkout_budget(&repo_root).context("validating cloned skill checkout")?;
        let dir = match &remote.subpath {
            Some(sub) => {
                let relative = Path::new(sub);
                if relative.as_os_str().is_empty()
                    || relative
                        .components()
                        .any(|component| !matches!(component, Component::Normal(_)))
                {
                    bail!("skill repository subpath contains unsafe components: {sub:?}");
                }
                let candidate = repo_root.join(relative);
                if ensure_safe_relative_file(&candidate, &candidate.join("SKILL.md"))? {
                    candidate
                } else {
                    // skills.sh ids are `owner/repo/<skill-id>` where the last
                    // segment is the skill's NAME, not a repo path — resolve
                    // by directory name anywhere in the repo.
                    let last = sub.rsplit('/').next().unwrap_or(sub);
                    find_skill_dir(&repo_root, Some(last), true).with_context(|| {
                        format!(
                            "`{sub}` is neither a path nor a skill name in {}",
                            remote.repo_url
                        )
                    })?
                }
            }
            None => find_skill_dir(&repo_root, input.name.as_deref(), false)?,
        };
        (dir, Some(tmp))
    };

    if !ensure_safe_relative_file(&skill_dir, &skill_dir.join("SKILL.md"))? {
        bail!(
            "`{}` is not a skill — no SKILL.md inside",
            skill_dir.display()
        );
    }

    let raw = read_skill_file(&skill_dir, &skill_dir.join("SKILL.md"))
        .context("reading skill manifest before install")?;
    let (fm_name, fm_description) = parse_frontmatter(&raw);
    let install_name = input
        .name
        .or_else(|| {
            skill_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .or(fm_name.clone())
        .unwrap_or_else(|| "skill".to_string());
    validate_skill_name(&install_name).context("invalid installed skill name")?;

    let skills_root = crate::config::phoenix_home().join("skills");
    crate::config::private_io::prepare_phoenix_directory(&skills_root)
        .context("preparing private skills directory")?;
    let dest = skills_root.join(&install_name);
    if std::fs::symlink_metadata(&dest).is_ok() {
        bail!(
            "skill `{install_name}` is already installed at {} — remove it first to reinstall",
            dest.display()
        );
    }

    struct StageGuard(Option<PathBuf>);
    impl Drop for StageGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
    let stage = skills_root.join(format!(".install-{}", uuid::Uuid::new_v4().simple()));
    create_private_skill_dir(&stage)?;
    let mut stage_guard = StageGuard(Some(stage.clone()));
    copy_dir(&skill_dir, &stage).context("staging skill under ~/.phoenix/skills")?;
    if !ensure_safe_relative_file(&stage, &stage.join("SKILL.md"))? {
        bail!("staged skill lost its SKILL.md");
    }
    crate::config::private_io::with_private_lock(&dest, || {
        match std::fs::symlink_metadata(&dest) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => bail!("skill `{install_name}` was installed by another process"),
            Err(error) => return Err(error).context("checking skill install destination"),
        }
        std::fs::rename(&stage, &dest)
            .with_context(|| format!("publishing installed skill {}", dest.display()))?;
        if let Ok(directory) = std::fs::File::open(&skills_root) {
            directory.sync_all().context("syncing skills directory")?;
        }
        Ok(())
    })?;
    stage_guard.0 = None;

    // Card the new skill into Cognee right away so memory_recall surfaces it
    // this session, not after the next maintenance tick.
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let card_receipt = sync_skill_cards(&workspace).await;
    tracing::debug!("skill_install: {card_receipt}");

    Ok(ToolOutput {
        summary: format!("installed skill {install_name}"),
        content: format!(
            "Installed `{}` to {}.\n{}: {}\nIt is live now — `skill` with name `{}` loads it.",
            install_name,
            dest.display(),
            fm_name.unwrap_or_else(|| install_name.clone()),
            fm_description.unwrap_or_else(|| "(no description)".to_string()),
            install_name
        ),
    })
}

// ── Skill cards in Cognee (memory federation, plan 016 follow-up) ────────
//
// Skills are procedural memory but lived outside the recall surface: an agent
// asking memory_recall "how do I handle X" never learned a skill existed for
// X. A skill CARD is a small pointer note in Cognee — name, what it does,
// when to reach for it, and the path — so recall surfaces skills next to
// facts. The SKILL.md file stays the source of truth; Cognee holds pointers.
//
// Reconcile-based, not install-hooked: `sync_skill_cards` diffs the
// discovered skills against `.cards.json` so cards appear no matter how a
// skill arrived (skill_install, manual copy, an agent writing one) and
// removals leave a stale-marker note. Called after install and on the
// daemon's maintenance tick.

fn cards_state_path() -> PathBuf {
    crate::config::phoenix_home()
        .join("skills")
        .join(".cards.json")
}

fn parse_cards_state(bytes: Option<&[u8]>) -> Result<Vec<String>> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    if bytes.len() > MAX_CARDS_STATE_BYTES {
        bail!(
            "skill-card state is {} bytes; maximum is {MAX_CARDS_STATE_BYTES}",
            bytes.len()
        );
    }
    let mut carded: Vec<String> =
        serde_json::from_slice(bytes).context("skill-card state is corrupt")?;
    if carded.len() > MAX_CARDED_SKILLS {
        bail!(
            "skill-card state has {} entries; maximum is {MAX_CARDED_SKILLS}",
            carded.len()
        );
    }
    for name in &carded {
        validate_skill_name(name)
            .with_context(|| format!("skill-card state contains invalid name {name:?}"))?;
    }
    carded.sort();
    carded.dedup();
    Ok(carded)
}

fn load_cards_state(path: &Path) -> Result<Vec<String>> {
    let bytes = crate::config::private_io::read_private_file(path)?;
    parse_cards_state(bytes.as_deref())
}

fn commit_card_changes(path: &Path, added: &[String], removed: &[String]) -> Result<Vec<String>> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let mut carded = parse_cards_state(current)?;
        for name in added {
            validate_skill_name(name)?;
            if !carded.contains(name) {
                carded.push(name.clone());
            }
        }
        carded.retain(|name| !removed.contains(name));
        carded.sort();
        carded.dedup();
        if carded.len() > MAX_CARDED_SKILLS {
            bail!("skill-card state would exceed {MAX_CARDED_SKILLS} entries");
        }
        let json = serde_json::to_vec_pretty(&carded)?;
        if json.len() > MAX_CARDS_STATE_BYTES {
            bail!("skill-card state would exceed {MAX_CARDS_STATE_BYTES} bytes");
        }
        Ok((carded, json))
    })
}

fn skill_card(skill: &SkillMeta) -> String {
    format!(
        "SKILL CARD: an installed skill named `{}` is available. What it does: {} \
         Location: {}. When a task matches this description, load it with the `skill` \
         tool (name: `{}`) and follow it instead of improvising from memory.",
        skill.name,
        skill.description,
        skill.dir.display(),
        skill.name
    )
}

/// Diff installed skills against the carded set; write pointer cards for new
/// skills into Cognee and stale-markers for removed ones. Returns a one-line
/// receipt. Never fails the caller; unconfirmed writes stay absent/present in
/// `.cards.json` as appropriate so the next reconciliation retries them.
pub async fn sync_skill_cards(workspace_root: &Path) -> String {
    let skills = discover(workspace_root);
    let state_path = cards_state_path();
    let carded = match load_cards_state(&state_path) {
        Ok(carded) => carded,
        Err(error) => {
            tracing::warn!("skill cards: refusing corrupt/unsafe state: {error:#}");
            return format!(
                "skill cards: state unavailable/corrupt; no reconciliation performed ({error})"
            );
        }
    };
    let mut added = 0usize;
    let mut unconfirmed = 0usize;
    let mut confirmed_added = Vec::new();
    for skill in &skills {
        if !carded.contains(&skill.name) {
            let outcome = crate::librarian::memory::remember(&skill_card(skill)).await;
            if outcome.is_stored() {
                confirmed_added.push(skill.name.clone());
                added += 1;
            } else {
                unconfirmed += 1;
                tracing::warn!(
                    "skill card `{}` durability not confirmed ({outcome}); leaving it retryable",
                    skill.name
                );
            }
        }
    }
    let installed: std::collections::HashSet<&str> =
        skills.iter().map(|s| s.name.as_str()).collect();
    let removed: Vec<String> = carded
        .iter()
        .filter(|name| !installed.contains(name.as_str()))
        .cloned()
        .collect();
    let mut confirmed_removed = Vec::new();
    for name in &removed {
        let outcome = crate::librarian::memory::remember(&format!(
            "SKILL CARD UPDATE: the skill `{name}` was removed. Earlier cards pointing \
             to it are stale — do not recommend loading it."
        ))
        .await;
        if outcome.is_stored() {
            confirmed_removed.push(name.clone());
        } else {
            unconfirmed += 1;
            tracing::warn!(
                "removed-skill marker for `{name}` durability not confirmed ({outcome}); leaving it retryable"
            );
        }
    }
    let final_carded = match commit_card_changes(&state_path, &confirmed_added, &confirmed_removed)
    {
        Ok(carded) => carded,
        Err(error) => {
            tracing::warn!("skill cards: durable state update failed: {error:#}");
            unconfirmed += confirmed_added.len() + confirmed_removed.len();
            carded
        }
    };
    format!(
        "skill cards: {added} added, {} removed, {unconfirmed} durability-unconfirmed/retry-pending ({} total carded)",
        confirmed_removed.len(),
        final_carded.len()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSource {
    repo_url: String,
    branch: Option<String>,
    subpath: Option<String>,
}

/// `https://github.com/owner/repo[/tree/<branch>/<subpath>]` or
/// `owner/repo[/subpath]` → a canonical HTTPS-only clone source.
fn parse_git_source(source: &str) -> Result<GitSource> {
    if source.is_empty() || source.len() > MAX_REMOTE_SOURCE_BYTES {
        bail!("remote skill source must be 1..={MAX_REMOTE_SOURCE_BYTES} bytes");
    }
    if source.chars().any(char::is_control) {
        bail!("remote skill source cannot contain control characters");
    }
    if source.contains(['?', '#']) {
        bail!("remote skill source cannot contain a query string or fragment");
    }

    let rest = if let Some(rest) = source.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = source.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = source.strip_prefix("github.com/") {
        rest
    } else if source.contains("://") || source.starts_with("git@") {
        bail!("remote skills must use a github.com HTTPS URL or owner/repo shorthand");
    } else {
        source
    }
    .trim_end_matches('/');
    if rest.is_empty() || rest.split('/').any(str::is_empty) {
        bail!("remote skill source contains an empty path component");
    }
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 2 {
        bail!("can't parse `{source}` — expected a GitHub URL, owner/repo, or a local path");
    }
    let owner = parts[0];
    let repo = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
    validate_github_owner(owner)?;
    validate_github_repo(repo)?;

    // Deep link: owner/repo/tree/<branch>/<subpath...>. As with GitHub's own
    // unescaped URL shape, the first segment after `tree` is the branch.
    let (branch, subpath) = if parts.len() >= 4 && parts[2] == "tree" {
        let branch = parts[3];
        validate_git_branch(branch)?;
        let subpath = (!parts[4..].is_empty()).then(|| parts[4..].join("/"));
        (Some(branch.to_string()), subpath)
    } else if parts.len() > 2 {
        (None, Some(parts[2..].join("/")))
    } else {
        (None, None)
    };
    if let Some(subpath) = &subpath {
        validate_git_subpath(subpath)?;
    }
    Ok(GitSource {
        repo_url: format!("https://github.com/{owner}/{repo}.git"),
        branch,
        subpath,
    })
}

fn validate_github_owner(owner: &str) -> Result<()> {
    if owner.is_empty()
        || owner.len() > 100
        || !owner.as_bytes()[0].is_ascii_alphanumeric()
        || !owner.as_bytes()[owner.len() - 1].is_ascii_alphanumeric()
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("GitHub owner must be 1..=100 ASCII letters, digits, or interior '-'");
    }
    Ok(())
}

fn validate_github_repo(repo: &str) -> Result<()> {
    if repo.is_empty()
        || repo.len() > 100
        || matches!(repo, "." | "..")
        || !repo
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("GitHub repository name contains unsupported characters or length");
    }
    Ok(())
}

fn validate_git_branch(branch: &str) -> Result<()> {
    if branch.is_empty()
        || branch.len() > MAX_GIT_BRANCH_BYTES
        || branch.starts_with(['-', '.'])
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("@{")
        || !branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("GitHub branch is not a safe single-segment git ref");
    }
    Ok(())
}

fn validate_git_subpath(subpath: &str) -> Result<()> {
    if subpath.is_empty() || subpath.len() > MAX_GIT_SUBPATH_BYTES {
        bail!("skill repository subpath must be 1..={MAX_GIT_SUBPATH_BYTES} bytes");
    }
    let path = Path::new(subpath);
    if path.components().any(|component| match component {
        Component::Normal(component) => {
            let bytes = component.as_encoded_bytes();
            bytes.is_empty()
                || bytes.len() > 255
                || !bytes.iter().copied().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@' | b'+')
                })
        }
        _ => true,
    }) {
        bail!("skill repository subpath contains unsafe or unsupported components");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloneStopReason {
    Exited,
    TimedOut,
}

#[derive(Debug)]
struct CloneRunOutput {
    status: ExitStatus,
    reason: CloneStopReason,
    descendants_terminated: usize,
}

async fn run_git_clone(source: &GitSource, destination: &Path) -> Result<CloneRunOutput> {
    let clone_parent = destination
        .parent()
        .context("git clone destination had no parent directory")?;
    let mut command = tokio::process::Command::new("git");
    command
        // Do not inherit user/system URL rewrites, credential helpers, or
        // filters. The canonical source is HTTPS GitHub and no other protocol
        // is allowed, even after a malicious environment/config rewrite.
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CEILING_DIRECTORIES", clone_parent)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_EXEC_PATH")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_PROXY_COMMAND")
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSL_NO_VERIFY")
        .env_remove("GIT_TEMPLATE_DIR")
        .current_dir(clone_parent)
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "credential.helper=",
            "-c",
            "core.hooksPath=/dev/null",
            "clone",
            "--depth=1",
            "--single-branch",
            "--no-tags",
        ]);
    if let Some(branch) = &source.branch {
        command.arg("--branch").arg(branch);
    }
    command.arg("--").arg(&source.repo_url).arg(destination);
    run_clone_command(command, GIT_CLONE_TIMEOUT).await
}

/// Run clone-like commands under a private process group. This stays generic
/// so the lifecycle can be tested with local injected commands and no network.
async fn run_clone_command(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<CloneRunOutput> {
    if timeout.is_zero() || timeout > GIT_CLONE_TIMEOUT {
        bail!(
            "git clone timeout must be 1ms..={}s",
            GIT_CLONE_TIMEOUT.as_secs()
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        bail!("bounded remote skill cloning is currently supported only on Linux");
    }
    #[cfg(target_os = "linux")]
    {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let child = command
            .spawn()
            .context("running git clone (is git installed?)")?;
        let pid = i32::try_from(child.id().context("git clone child had no pid")?)
            .context("git clone child pid did not fit i32")?;
        let mut owned = CloneChildGuard::new(child, pid);
        let deadline = Instant::now() + timeout;
        let mut poll_error = None;
        let reason = loop {
            if Instant::now() >= deadline {
                break CloneStopReason::TimedOut;
            }
            match clone_child_exited_without_reap(pid) {
                Ok(true) => break CloneStopReason::Exited,
                Ok(false) => {}
                Err(error) => {
                    poll_error = Some(error);
                    break CloneStopReason::TimedOut;
                }
            }
            tokio::time::sleep(
                GIT_CLONE_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        };
        let (status, descendants_terminated) = owned
            .finish()
            .context("terminate, verify, and reap git clone process group")?;
        if let Some(error) = poll_error {
            return Err(error).context("poll git clone process status");
        }
        Ok(CloneRunOutput {
            status,
            reason,
            descendants_terminated,
        })
    }
}

#[cfg(target_os = "linux")]
struct CloneChildGuard {
    child: Option<tokio::process::Child>,
    pgid: i32,
}

#[cfg(target_os = "linux")]
impl CloneChildGuard {
    fn new(child: tokio::process::Child, pgid: i32) -> Self {
        Self {
            child: Some(child),
            pgid,
        }
    }

    fn finish(&mut self) -> Result<(ExitStatus, usize)> {
        let result = terminate_clone_process_group(
            self.child.as_mut().context("git clone child missing")?,
            self.pgid,
        );
        if result.is_ok() {
            self.child.take();
        }
        result
    }
}

#[cfg(target_os = "linux")]
impl Drop for CloneChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if terminate_clone_process_group(child, self.pgid).is_err() {
                // Last-resort cancellation/panic cleanup. Normal returned
                // paths report a verification failure instead of hiding it.
                if self.pgid > 1 && self.pgid != unsafe { libc::getpgrp() } {
                    let actual = unsafe { libc::getpgid(self.pgid) };
                    if actual == self.pgid {
                        unsafe {
                            libc::kill(-self.pgid, libc::SIGKILL);
                        }
                    }
                }
                let deadline = Instant::now() + GIT_CLONE_KILL_GRACE;
                while Instant::now() < deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(GIT_CLONE_POLL_INTERVAL);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn clone_child_exited_without_reap(pid: i32) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is valid writable storage. WNOWAIT preserves the leader
    // (and its PGID identity) until same-group descendants are gone.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("waitid git clone child");
    }
    Ok(info.si_signo == libc::SIGCHLD)
}

#[cfg(target_os = "linux")]
fn clone_live_process_group_members(pgid: i32) -> Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").context("scan /proc for git clone process group")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read /proc entry for git clone"),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("read /proc git clone process stat"),
        };
        let Some(after_name) = stat.rsplit_once(") ").map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.as_bytes().first().copied());
        let _parent_pid = fields.next();
        let member_pgid = fields.next().and_then(|field| field.parse::<i32>().ok());
        if member_pgid == Some(pgid) && !matches!(state, Some(b'Z' | b'X')) {
            members.push(pid);
        }
    }
    Ok(members)
}

#[cfg(target_os = "linux")]
fn signal_clone_process_group(pgid: i32, signal: i32) -> Result<()> {
    if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
        bail!("refusing to signal unsafe git clone process group {pgid}");
    }
    let actual = unsafe { libc::getpgid(pgid) };
    if actual != pgid {
        bail!("git clone process group ownership changed: pid={pgid}, pgid={actual}");
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("signal git clone process group")
    }
}

#[cfg(target_os = "linux")]
fn terminate_clone_process_group(
    child: &mut tokio::process::Child,
    pgid: i32,
) -> Result<(ExitStatus, usize)> {
    let initial = clone_live_process_group_members(pgid)?;
    let descendants = initial.iter().filter(|pid| **pid != pgid).count();
    if !initial.is_empty() {
        signal_clone_process_group(pgid, libc::SIGTERM)?;
        let term_deadline = Instant::now() + GIT_CLONE_TERM_GRACE;
        while Instant::now() < term_deadline && !clone_live_process_group_members(pgid)?.is_empty()
        {
            std::thread::sleep(GIT_CLONE_POLL_INTERVAL);
        }
        if !clone_live_process_group_members(pgid)?.is_empty() {
            signal_clone_process_group(pgid, libc::SIGKILL)?;
            let kill_deadline = Instant::now() + GIT_CLONE_KILL_GRACE;
            while Instant::now() < kill_deadline
                && !clone_live_process_group_members(pgid)?.is_empty()
            {
                std::thread::sleep(GIT_CLONE_POLL_INTERVAL);
            }
        }
    }
    let remaining = clone_live_process_group_members(pgid)?;
    if !remaining.is_empty() {
        bail!(
            "git clone process group still has live members: {}",
            remaining
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let reap_deadline = Instant::now() + GIT_CLONE_KILL_GRACE;
    loop {
        if let Some(status) = child.try_wait().context("reap git clone leader")? {
            return Ok((status, descendants));
        }
        if Instant::now() >= reap_deadline {
            bail!("git clone leader did not become reapable after group termination");
        }
        std::thread::sleep(GIT_CLONE_POLL_INTERVAL);
    }
}

/// Locate the skill inside a cloned repo: root itself, or search (depth ≤ 3)
/// for SKILL.md directories. Explicit source selectors must match exactly;
/// an installation name alone may rename a root or single-skill repository.
fn find_skill_dir(
    repo_root: &Path,
    name: Option<&str>,
    require_named_match: bool,
) -> Result<PathBuf> {
    match std::fs::symlink_metadata(repo_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!("skill source root is not a real directory"),
        Err(error) => return Err(error).context("inspecting skill source root"),
    }
    let mut found: Vec<PathBuf> = Vec::new();
    if ensure_safe_relative_file(repo_root, &repo_root.join("SKILL.md"))? {
        if !require_named_match {
            return Ok(repo_root.to_path_buf());
        }
        // A registry id can also name the root skill; the checkout directory
        // is always called `repo`, so use the manifest's declared name here.
        let raw = read_skill_file(repo_root, &repo_root.join("SKILL.md"))?;
        let (root_name, _) = parse_frontmatter(&raw);
        if root_name.as_deref() == name {
            found.push(repo_root.to_path_buf());
        }
    }
    fn walk(dir: &Path, depth: u8, visited: &mut usize, found: &mut Vec<PathBuf>) {
        if depth > 3 || *visited >= MAX_SKILL_FILES || found.len() >= MAX_DISCOVERED_SKILLS {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if *visited >= MAX_SKILL_FILES {
                return;
            }
            *visited += 1;
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && !file_type.is_symlink() && !path.ends_with(".git") {
                if ensure_safe_relative_file(&path, &path.join("SKILL.md")).unwrap_or(false) {
                    found.push(path.clone());
                }
                // Skill repositories can contain nested skills even when a
                // parent directory also has its own manifest.
                walk(&path, depth + 1, visited, found);
            }
        }
    }
    let mut visited = 0usize;
    walk(repo_root, 0, &mut visited, &mut found);
    if let Some(name) = name {
        let matches: Vec<_> = found
            .iter()
            .filter(|p| p.as_path() == repo_root || p.file_name().is_some_and(|n| n == name))
            .collect();
        match matches.as_slice() {
            [hit] => return Ok((*hit).clone()),
            [] if require_named_match => bail!("no skill named `{name}` found in the repo"),
            [] => {}
            _ => bail!(
                "multiple skills named `{name}` found — use an explicit repository path: {}",
                matches.iter().map(|p| p.strip_prefix(repo_root).unwrap_or(p).display().to_string())
                    .collect::<Vec<_>>().join(", ")
            ),
        }
    }
    match found.len() {
        0 => bail!("no SKILL.md found anywhere in the repo"),
        1 => Ok(found.remove(0)),
        _ => {
            let names: Vec<String> = found
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect();
            bail!(
                "repo holds {} skills — pass `name` to pick one: {}",
                names.len(),
                names.join(", ")
            )
        }
    }
}

#[derive(Default)]
struct CopyBudget {
    nodes: usize,
    bytes: u64,
}

/// Account for the entire checked-out worktree (excluding Git's private
/// object database) before selecting or publishing one skill. This prevents a
/// tiny SKILL.md inside an enormous repository from bypassing install limits.
fn validate_checkout_budget(repo_root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(repo_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!("cloned skill checkout is not a real directory"),
        Err(error) => return Err(error).context("inspecting cloned skill checkout"),
    }

    fn walk(dir: &Path, depth: u8, budget: &mut CopyBudget) -> Result<()> {
        if depth > MAX_SKILL_DEPTH {
            bail!("skill checkout nesting exceeds {MAX_SKILL_DEPTH}");
        }
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("reading cloned skill checkout {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
            if entry.file_name() == ".git" {
                continue;
            }
            if budget.nodes >= MAX_SKILL_FILES {
                bail!("skill checkout exceeds the {MAX_SKILL_FILES}-node install limit");
            }
            budget.nodes += 1;
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspecting checkout entry {}", entry.path().display()))?;
            if file_type.is_symlink() {
                // Never follow checkout links. A selected skill containing one
                // is rejected later by copy_dir; unrelated links are inert.
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), depth + 1, budget)?;
            } else if file_type.is_file() {
                let metadata = entry.metadata().with_context(|| {
                    format!("inspecting checkout file {}", entry.path().display())
                })?;
                if metadata.len() > MAX_SKILL_FILE_BYTES {
                    bail!(
                        "skill checkout file {} is {} bytes; per-file maximum is {MAX_SKILL_FILE_BYTES}",
                        entry.path().display(),
                        metadata.len()
                    );
                }
                budget.bytes = budget
                    .bytes
                    .checked_add(metadata.len())
                    .context("skill checkout aggregate byte count overflowed its install budget")?;
                if budget.bytes > MAX_SKILL_TOTAL_BYTES {
                    bail!("skill checkout exceeds the {MAX_SKILL_TOTAL_BYTES}-byte install limit");
                }
            } else {
                bail!(
                    "skill checkout contains special file {}",
                    entry.path().display()
                );
            }
        }
        Ok(())
    }

    walk(repo_root, 0, &mut CopyBudget::default())
}

fn create_private_skill_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("creating skill directory {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing skill directory {}", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(path)
        .with_context(|| format!("creating skill directory {}", path.display()))?;
    Ok(())
}

fn copy_regular_skill_file(src: &Path, dst: &Path, budget: &mut CopyBudget) -> Result<()> {
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut source = source_options
        .open(src)
        .with_context(|| format!("opening skill source {}", src.display()))?;
    let metadata = source
        .metadata()
        .with_context(|| format!("inspecting skill source {}", src.display()))?;
    if !metadata.is_file() {
        bail!("skill source is not a regular file: {}", src.display());
    }
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        bail!(
            "skill source {} is {} bytes; per-file maximum is {MAX_SKILL_FILE_BYTES}",
            src.display(),
            metadata.len()
        );
    }
    if budget.bytes.saturating_add(metadata.len()) > MAX_SKILL_TOTAL_BYTES {
        bail!("skill exceeds the file-count or aggregate-byte install limit");
    }

    let mut destination_options = std::fs::OpenOptions::new();
    destination_options.write(true).create_new(true);
    #[cfg(unix)]
    let destination_mode = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let executable = metadata.permissions().mode() & 0o111 != 0;
        let mode = if executable { 0o700 } else { 0o600 };
        destination_options
            .mode(mode)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        mode
    };
    let mut destination = destination_options
        .open(dst)
        .with_context(|| format!("creating installed skill file {}", dst.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        destination
            .set_permissions(std::fs::Permissions::from_mode(destination_mode))
            .with_context(|| format!("securing installed skill file {}", dst.display()))?;
    }
    let copied = std::io::copy(
        &mut source.by_ref().take(MAX_SKILL_FILE_BYTES + 1),
        &mut destination,
    )
    .with_context(|| format!("copying skill file {}", src.display()))?;
    if copied > MAX_SKILL_FILE_BYTES || copied != metadata.len() {
        bail!("skill source {} changed while it was copied", src.display());
    }
    destination
        .sync_all()
        .with_context(|| format!("syncing installed skill file {}", dst.display()))?;
    budget.bytes += copied;
    Ok(())
}

fn copy_dir_inner(from: &Path, to: &Path, depth: u8, budget: &mut CopyBudget) -> Result<()> {
    if depth > MAX_SKILL_DEPTH {
        bail!("skill directory nesting exceeds {MAX_SKILL_DEPTH}");
    }
    for entry in std::fs::read_dir(from)
        .with_context(|| format!("reading skill directory {}", from.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", from.display()))?;
        if entry.file_name() == ".git" {
            continue;
        }
        if budget.nodes >= MAX_SKILL_FILES {
            bail!("skill exceeds the {MAX_SKILL_FILES}-node install limit");
        }
        budget.nodes += 1;
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspecting skill entry {}", entry.path().display()))?;
        if file_type.is_symlink() {
            bail!("skill install refuses symlink {}", entry.path().display());
        }
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if file_type.is_dir() {
            create_private_skill_dir(&dst)?;
            copy_dir_inner(&src, &dst, depth + 1, budget)?;
        } else if file_type.is_file() {
            copy_regular_skill_file(&src, &dst, budget)?;
        } else {
            bail!("skill install refuses special file {}", src.display());
        }
    }
    if let Ok(directory) = std::fs::File::open(to) {
        directory
            .sync_all()
            .with_context(|| format!("syncing staged skill directory {}", to.display()))?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    match std::fs::symlink_metadata(from) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!("skill source is not a real directory: {}", from.display()),
        Err(error) => return Err(error).context("inspecting skill source directory"),
    }
    match std::fs::symlink_metadata(to) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!(
            "skill destination is not a real directory: {}",
            to.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_skill_dir(to)?;
        }
        Err(error) => return Err(error).context("inspecting skill destination directory"),
    }
    copy_dir_inner(from, to, 0, &mut CopyBudget::default())
}

/// Confine `rel` to the skill directory (skills are third-party content —
/// a malicious `file` must not escape into the filesystem).
fn resolve_in_skill(skill_dir: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.strip_prefix("./").unwrap_or(rel);
    let relative = Path::new(rel);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("skill file path must stay inside the skill directory using normal components");
    }
    let candidate = skill_dir.join(relative);
    if ensure_safe_relative_file(skill_dir, &candidate)? {
        return Ok(candidate);
    }
    let with_md = skill_dir.join(format!("{rel}.md"));
    if ensure_safe_relative_file(skill_dir, &with_md)? {
        return Ok(with_md);
    }
    Ok(candidate) // let the read fail into the self-healing miss message
}

/// Self-healing miss: list what IS in the skill so the repair round needs no
/// extra calls (same pattern as design_reference).
fn miss_message(skill: &SkillMeta, rel: &str) -> String {
    let mut files = Vec::new();
    let mut visited = 0usize;
    collect_files(&skill.dir, &skill.dir, 0, &mut visited, &mut files);
    files.sort();
    format!(
        "skill `{}` has no file `{rel}`. Files in the skill:\n{}",
        skill.name,
        files.join("\n")
    )
}

fn collect_files(root: &Path, dir: &Path, depth: u8, visited: &mut usize, out: &mut Vec<String>) {
    if depth > MAX_SKILL_DEPTH || *visited >= MAX_SKILL_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries
        .flatten()
        .take(MAX_SKILL_FILES.saturating_sub(*visited))
    {
        *visited += 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &path, depth + 1, visited, out);
        } else if file_type.is_file() && path.strip_prefix(root).is_ok() {
            let rel = path.strip_prefix(root).expect("checked above");
            out.push(format!("  {}", rel.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_skill(root: &Path, name: &str, frontmatter_name: Option<&str>) {
        let dir = root.join(".phoenix/skills").join(name);
        std::fs::create_dir_all(dir.join("references")).unwrap();
        let fm_name = frontmatter_name.unwrap_or(name);
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {fm_name}\ndescription: renders videos from code\n---\n\n# {fm_name}\n\nUse the render script.\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("references/setup.md"), "install node first").unwrap();
    }

    #[test]
    fn discovers_and_lists_skills_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "remotion-render", None);
        let out = execute(
            dir.path(),
            SkillInput {
                name: None,
                file: None,
            },
        )
        .unwrap();
        assert!(out.content.contains("remotion-render"));
        assert!(out.content.contains("renders videos from code"));
    }

    #[test]
    fn loads_skill_md_and_reference_files() {
        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "remotion-render", None);
        let main = execute(
            dir.path(),
            SkillInput {
                name: Some("remotion-render".into()),
                file: None,
            },
        )
        .unwrap();
        assert!(main.content.contains("Use the render script"));
        let reference = execute(
            dir.path(),
            SkillInput {
                name: Some("remotion-render".into()),
                file: Some("references/setup".into()),
            },
        )
        .unwrap();
        assert!(reference.content.contains("install node first"));
    }

    #[test]
    fn missing_file_lists_skill_contents() {
        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "remotion-render", None);
        let err = execute(
            dir.path(),
            SkillInput {
                name: Some("remotion-render".into()),
                file: Some("references/nope.md".into()),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("references/setup.md"), "{err}");
    }

    #[test]
    fn unknown_skill_lists_installed_ones() {
        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "remotion-render", None);
        let err = execute(
            dir.path(),
            SkillInput {
                name: Some("video-magic".into()),
                file: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("remotion-render"));
    }

    #[test]
    fn path_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "evil", None);
        let err = execute(
            dir.path(),
            SkillInput {
                name: Some("evil".into()),
                file: Some("../../../../etc/passwd".into()),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("inside the skill directory"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_files_are_rejected_without_reading_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        install_skill(dir.path(), "evil", None);
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "secret outside text").unwrap();
        let link = dir.path().join(".phoenix/skills/evil/references/escape.md");
        symlink(&outside, &link).unwrap();
        let error = execute(
            dir.path(),
            SkillInput {
                name: Some("evil".into()),
                file: Some("references/escape.md".into()),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error:#}");
    }

    #[test]
    fn installed_skill_names_are_single_safe_components() {
        for invalid in ["../escape", "/absolute", "two words", "", ".."] {
            assert!(
                validate_skill_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(validate_skill_name("safe-skill_2.0").is_ok());
    }

    #[test]
    fn popularity_gate_bans_unpopular_and_gates_unverifiable() {
        // Registry-known and popular → allowed.
        assert!(popularity_verdict(Some(MIN_REGISTRY_INSTALLS), false, "x").is_ok());
        // Registry-known and below the floor → banned, even user-authorized.
        let err = popularity_verdict(Some(MIN_REGISTRY_INSTALLS - 1), true, "x").unwrap_err();
        assert!(err.to_string().contains("trust bar"), "{err}");
        // Unknown to the registry → only with explicit user authorization.
        assert!(popularity_verdict(None, true, "x").is_ok());
        let err = popularity_verdict(None, false, "x").unwrap_err();
        assert!(err.to_string().contains("user_authorized"), "{err}");
    }

    #[test]
    fn registry_results_skip_unsupported_sources_without_hiding_safe_hits() {
        let hits = filter_registry_hits(vec![
            RegistryHit {
                id: "smithery.ai/nextjs-best-practices".to_string(),
                name: "unsupported".to_string(),
                installs: 50_000,
                source: "smithery.ai".to_string(),
            },
            RegistryHit {
                id: "clerk/skills/clerk-nextjs-patterns".to_string(),
                name: "clerk-nextjs-patterns".to_string(),
                installs: 31_696,
                source: "clerk/skills".to_string(),
            },
        ]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "clerk/skills/clerk-nextjs-patterns");
    }

    /// Live registry check of the install gate: a popular listing passes the
    /// lookup, a made-up id is unverifiable. Ignored (network).
    /// Run: cargo test --lib popularity_gate_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn popularity_gate_live_lookup() {
        let hits = registry_search("nextjs").await.expect("registry search");
        assert!(!hits.is_empty());
        let top = &hits[0];
        println!(
            "top: {} ({} installs) id={}",
            top.name, top.installs, top.id
        );
        let installs = registry_installs_for(&top.id).await;
        assert!(installs.is_some(), "top hit should verify by exact id");
        assert!(registry_installs_for("nobody/nothing/fake-skill-xyz")
            .await
            .is_none());
    }

    #[test]
    fn parse_git_source_handles_urls_shorthand_and_deep_links() {
        let source = parse_git_source("https://github.com/anthropics/skills").unwrap();
        assert_eq!(source.repo_url, "https://github.com/anthropics/skills.git");
        assert_eq!(source.branch, None);
        assert_eq!(source.subpath, None);

        let source =
            parse_git_source("https://github.com/anthropics/skills/tree/main/document-skills/pdf")
                .unwrap();
        assert_eq!(source.repo_url, "https://github.com/anthropics/skills.git");
        assert_eq!(source.branch.as_deref(), Some("main"));
        assert_eq!(source.subpath.as_deref(), Some("document-skills/pdf"));

        let source = parse_git_source("owner/repo/some/skill").unwrap();
        assert_eq!(source.repo_url, "https://github.com/owner/repo.git");
        assert_eq!(source.branch, None);
        assert_eq!(source.subpath.as_deref(), Some("some/skill"));

        assert!(parse_git_source("nonsense").is_err());
        assert!(parse_git_source("https://example.com/owner/repo").is_err());
        assert!(parse_git_source("owner/repo/../../escape").is_err());
        assert!(parse_git_source("owner/repo/tree/-unsafe/path").is_err());
        assert!(parse_git_source(&format!(
            "owner/repo/{}",
            "x".repeat(MAX_GIT_SUBPATH_BYTES + 1)
        ))
        .is_err());
    }

    #[cfg(target_os = "linux")]
    fn injected_clone_command(script: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_clone_command_succeeds_with_closed_stdin() {
        let command = injected_clone_command("if read value; then exit 91; else exit 0; fi");
        let output = run_clone_command(command, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(output.reason, CloneStopReason::Exited);
        assert!(output.status.success(), "{}", output.status);
        assert_eq!(output.descendants_terminated, 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_clone_command_preserves_nonzero_status() {
        let command = injected_clone_command("exit 23");
        let output = run_clone_command(command, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(output.reason, CloneStopReason::Exited);
        assert_eq!(output.status.code(), Some(23));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_clone_command_times_out_and_reaps_leader() {
        let command = injected_clone_command("sleep 30");
        let started = Instant::now();
        let output = run_clone_command(command, Duration::from_millis(80))
            .await
            .unwrap();
        assert_eq!(output.reason, CloneStopReason::TimedOut);
        assert!(!output.status.success());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bounded_clone_command_terminates_background_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("background.pid");
        let mut command = injected_clone_command(
            "sh -c 'trap \"\" HUP TERM; exec sleep 30' & echo $! > \"$PID_FILE\"; sleep 0.05; exit 0",
        );
        command.env("PID_FILE", &pid_file);
        let output = run_clone_command(command, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(output.reason, CloneStopReason::Exited);
        assert!(output.status.success(), "{}", output.status);
        assert!(
            output.descendants_terminated >= 1,
            "background process was not observed"
        );
        let background_pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{background_pid}/stat"));
        if let Ok(stat) = stat {
            let state = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.as_bytes().first().copied());
            assert!(
                matches!(state, Some(b'Z' | b'X')),
                "background pid {background_pid} remained live: {stat}"
            );
        }
    }

    #[test]
    fn find_skill_dir_disambiguates_by_name() {
        let repo = tempfile::tempdir().unwrap();
        for name in ["alpha", "beta"] {
            let dir = repo.path().join("skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }
        // Two skills, no name → error listing both.
        let err = find_skill_dir(repo.path(), None, false).unwrap_err();
        assert!(err.to_string().contains("alpha"), "{err}");
        // Name picks one.
        let hit = find_skill_dir(repo.path(), Some("beta"), false).unwrap();
        assert!(hit.ends_with("skills/beta"));
    }

    #[test]
    fn explicit_skill_selector_does_not_load_root_bootstrap() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("SKILL.md"), "name: flue").unwrap();
        let nested = repo.path().join("skills/blender");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("SKILL.md"), "name: Blender").unwrap();
        assert_eq!(find_skill_dir(repo.path(), Some("blender"), true).unwrap(), nested);
        // A name used only as an installation alias still renames the root.
        assert_eq!(find_skill_dir(repo.path(), Some("my-flue"), false).unwrap(), repo.path());
    }

    #[test]
    fn explicit_skill_selector_rejects_missing_and_ambiguous_names() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("SKILL.md"), "root").unwrap();
        for path in ["skills/blender", "examples/blender"] {
            let dir = repo.path().join(path);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "skill").unwrap();
        }
        let missing = find_skill_dir(repo.path(), Some("absent"), true).unwrap_err();
        assert!(missing.to_string().contains("no skill named `absent`"));
        let ambiguous = find_skill_dir(repo.path(), Some("blender"), true).unwrap_err();
        assert!(ambiguous.to_string().contains("multiple skills named `blender`"));
        assert!(ambiguous.to_string().contains("skills/blender"));
        assert!(ambiguous.to_string().contains("examples/blender"));
    }

    #[test]
    fn explicit_skill_selector_searches_below_other_skills() {
        let repo = tempfile::tempdir().unwrap();
        let parent = repo.path().join("skills/general");
        let child = parent.join("blender");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("SKILL.md"), "general").unwrap();
        std::fs::write(child.join("SKILL.md"), "blender").unwrap();
        assert_eq!(find_skill_dir(repo.path(), Some("blender"), true).unwrap(), child);
    }

    #[test]
    fn explicit_skill_selector_can_match_the_root_manifest_name() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("SKILL.md"), "---\nname: flue\n---\nRoot skill").unwrap();
        assert_eq!(find_skill_dir(repo.path(), Some("flue"), true).unwrap(), repo.path());
        assert!(find_skill_dir(repo.path(), Some("blender"), true).is_err());
    }

    #[test]
    fn explicit_skill_selector_does_not_rename_an_unrelated_only_skill() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join("general");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "general").unwrap();
        assert!(find_skill_dir(repo.path(), Some("blender"), true).is_err());
        assert_eq!(find_skill_dir(repo.path(), Some("alias"), false).unwrap(), dir);
    }

    #[test]
    fn copy_dir_skips_git_metadata() {
        let from = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(from.path().join(".git")).unwrap();
        std::fs::write(from.path().join(".git/HEAD"), "ref").unwrap();
        std::fs::create_dir_all(from.path().join("references")).unwrap();
        std::fs::write(from.path().join("SKILL.md"), "skill").unwrap();
        std::fs::write(from.path().join("references/a.md"), "ref a").unwrap();
        let to = tempfile::tempdir().unwrap();
        let dest = to.path().join("installed");
        copy_dir(from.path(), &dest).unwrap();
        assert!(dest.join("SKILL.md").is_file());
        assert!(dest.join("references/a.md").is_file());
        assert!(!dest.join(".git").exists());
    }

    #[test]
    fn checkout_budget_rejects_oversized_worktree_before_copy() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let oversized = std::fs::File::create(repo.path().join("oversized.bin")).unwrap();
        oversized.set_len(MAX_SKILL_FILE_BYTES + 1).unwrap();
        let error = validate_checkout_budget(repo.path()).unwrap_err();
        assert!(error.to_string().contains("per-file maximum"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_rejects_symlinks_and_creates_private_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let from = tempfile::tempdir().unwrap();
        std::fs::write(from.path().join("SKILL.md"), "skill").unwrap();
        let clean_dest_root = tempfile::tempdir().unwrap();
        let clean_dest = clean_dest_root.path().join("installed");
        copy_dir(from.path(), &clean_dest).unwrap();
        assert_eq!(
            std::fs::metadata(clean_dest.join("SKILL.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("outside-skill-data");
        std::fs::write(&outside, "outside").unwrap();
        symlink(&outside, from.path().join("escape.md")).unwrap();
        let to = tempfile::tempdir().unwrap();
        let dest = to.path().join("installed");
        assert!(copy_dir(from.path(), &dest).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
        assert!(!dest.join("escape.md").exists());
    }

    #[test]
    fn corrupt_card_state_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cards.json");
        crate::config::private_io::atomic_write_private(&path, b"{broken").unwrap();
        assert!(commit_card_changes(&path, &["safe".into()], &[]).is_err());
        assert_eq!(
            crate::config::private_io::read_private_file(&path)
                .unwrap()
                .unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn concurrent_card_state_updates_do_not_drop_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cards.json");
        std::thread::scope(|scope| {
            for index in 0..24 {
                let path = &path;
                scope.spawn(move || {
                    commit_card_changes(path, &[format!("skill-{index}")], &[]).unwrap();
                });
            }
        });
        assert_eq!(load_cards_state(&path).unwrap().len(), 24);
    }

    #[test]
    fn context_lines_one_per_skill_empty_when_none() {
        let dir = tempfile::tempdir().unwrap();
        // Hermetic check via discover_in — plain discover() also reads the
        // real ~/.phoenix/skills, which may hold the user's installs.
        let local_root = dir.path().join(".phoenix/skills");
        assert!(discover_in(&[local_root.clone()]).is_empty());
        assert!(context_lines_in(&[local_root.clone()]).is_empty());
        install_skill(dir.path(), "remotion-render", None);
        assert_eq!(discover_in(&[local_root.clone()]).len(), 1);
        // Keep rendering on the same isolated roots. context_lines() also
        // reads installed/global skills and could cross the description cap.
        let lines = context_lines_in(&[local_root]);
        assert!(lines.contains("remotion-render: renders videos from code"));
    }

    #[test]
    fn skill_hint_matches_on_overlap_and_stays_quiet_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        let skill_dir = root.join("cinematic-landing-images");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: cinematic-landing-images\ndescription: prompting image models to \
             generate cinematic hero images for landing pages\n---\nbody",
        )
        .unwrap();
        let roots = vec![root];
        let hit = skill_hint_in(
            &roots,
            "build a landing page with cinematic hero images for the product",
        )
        .expect("should match");
        assert!(hit.contains("cinematic-landing-images"), "{hit}");
        assert!(hit.to_ascii_lowercase().contains("skill"), "{hit}");
        // An unrelated mission stays hint-free — no noise in the context.
        assert!(skill_hint_in(&roots, "fix the failing database migration test").is_none());
        assert!(skill_hint_in(&roots, "").is_none());
        assert_eq!(
            matching_skill_in(
                &roots,
                "build a landing page with cinematic hero images for the product"
            )
            .map(|skill| skill.name),
            Some("cinematic-landing-images".to_string())
        );
    }

    #[test]
    fn skill_hint_respects_explicit_exclusions_and_ignores_generic_ai_research_words() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        let skill_dir = root.join("ai-research-explore");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: ai-research-explore\ndescription: novel deep learning candidates with datasets benchmarks and governed experiments\n---\nbody",
        )
        .unwrap();
        let roots = vec![root];

        assert!(skill_hint_in(
            &roots,
            "Browse current AI news, research the claims, and summarize the sources"
        )
        .is_none());
        assert!(skill_hint_in(
            &roots,
            "The ai-research-explore skill is explicitly not applicable; this is current-news synthesis"
        )
        .is_none());
        assert!(skill_hint_in(
            &roots,
            "Production acceptance task: read only, inspect my connected primary inbox, return exact sender subject date data in one batched call"
        )
        .is_none());
        assert!(skill_hint_in(
            &roots,
            "Run governed deep learning experiments on dataset candidates and compare benchmarks"
        )
        .is_some());
    }

    #[test]
    fn broad_browser_skill_does_not_activate_for_school_progress() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        let skill_dir = root.join("mobbin-gallery-browse");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: mobbin-gallery-browse\ndescription: browse application screens and inspect interface patterns with browser tools\n---\nbody",
        )
        .unwrap();
        let roots = vec![root];
        assert!(matching_skill_in(
            &roots,
            "Open Moodle and verify whether Science lesson 3 and its assessment are complete"
        )
        .is_none());
        assert!(matching_skill_in(
            &roots,
            "Use mobbin gallery browse to inspect application interface patterns"
        )
        .is_some());
    }

    #[test]
    fn visual_skill_requires_an_explicit_user_request_instead_of_generic_ui_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        let skill_dir = root.join("frontend-design-deslop");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: frontend-design-deslop\ndescription: design and build distinctive frontend UI applications\n---\nbody",
        )
        .unwrap();
        let roots = vec![root];
        assert!(matching_skill_in(
            &roots,
            "@frontend design a great cursor for our computer use UI"
        )
        .is_none());
        assert!(matching_skill_in(
            &roots,
            "Use frontend-design-deslop for this UI implementation"
        )
        .is_some());
    }

    #[test]
    fn ui_architecture_discussion_does_not_match_visual_build_skill() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        let skill_dir = root.join("frontend-design-deslop");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: frontend-design-deslop\ndescription: design and build distinctive frontend UI applications\n---\nbody",
        )
        .unwrap();
        let roots = vec![root];
        let mission = "A visual tool framework builds custom UI. What do you think? Inspect Phoenix and think about how to integrate it as an internal tool.";
        assert!(skill_hint_in(&roots, mission).is_none());
        assert!(matching_skill_in(&roots, mission).is_none());
    }

    #[test]
    fn context_lines_drops_descriptions_above_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".phoenix/skills");
        // A handful of skills: full descriptions, unchanged behavior. Use the
        // hermetic inner builder so the real ~/.phoenix/skills can't tip the
        // count over the cap.
        for i in 0..INLINE_DESCRIPTION_CAP {
            install_skill(dir.path(), &format!("skill-{i:03}"), None);
        }
        let small = context_lines_in(std::slice::from_ref(&root));
        assert!(
            small.contains("skill-000: renders videos from code"),
            "at/below the cap each skill keeps its description:\n{small}"
        );

        // One past the cap flips to names-only — descriptions gone, every name
        // still present, and a pointer to `skill` for the full descriptions.
        install_skill(dir.path(), "skill-extra", None);
        let large = context_lines_in(std::slice::from_ref(&root));
        assert!(
            !large.contains("skill-000: renders videos from code"),
            "above the cap descriptions are dropped:\n{large}"
        );
        assert!(
            large.contains("skill-000"),
            "names are still listed:\n{large}"
        );
        assert!(
            large.contains("skill-extra"),
            "names are still listed:\n{large}"
        );
        assert!(
            large.contains("call `skill` with no argument"),
            "points the agent at the on-demand description reader:\n{large}"
        );
    }
}
