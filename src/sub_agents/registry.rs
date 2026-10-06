//! Agent registry — agents as data in `~/.phoenix/agents/` (plan 019).
//!
//! Every agent gets a dir: `~/.phoenix/agents/<role>/` with `agent.toml`
//! (persona, description, model, tools, enabled) and, for custom agents, a
//! `system.md` prompt. Built-in specialists stay compiled (their prompts
//! already overlay from `~/.phoenix/prompts/`), but their manifests are
//! EXPORTED here on first boot and read back as OVERRIDES — edit the toml,
//! restart, and the built-in runs your model/tools/persona. A dir whose role
//! matches no built-in defines a brand-new agent, loaded entirely from disk:
//! the `create_agent` pipeline writes these.
//!
//! ```toml
//! # ~/.phoenix/agents/trader/agent.toml
//! persona = "Ledger"
//! description = "crypto trading analysis: strategies, backtests, market reads"
//! model = ""            # empty = the specialist lane's model
//! tools = ["read", "write", "bash", "web_search", "talk", "final_answer"]
//! enabled = true
//! output_label = "trading analysis"
//! notes = ["Own market analysis and strategy work."]
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::framework::SubAgentConfig;
use crate::runtime::ExecutionStyle;
use crate::session::SubAgentType;

const MAX_AGENT_DIRECTORIES: usize = 1_024;
const MAX_ROLE_BYTES: usize = 64;
pub(crate) const AGENT_MANIFEST_MAX_BYTES: usize = 1024 * 1024;
pub(crate) const AGENT_SYSTEM_PROMPT_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOLS: usize = 256;
const MAX_NOTES: usize = 256;
const MAX_FIELD_BYTES: usize = 16 * 1024;

/// One agent definition from disk (custom agent, or overrides for a built-in).
#[derive(Debug, Clone, Default)]
pub struct AgentManifest {
    pub role: String,
    pub persona: Option<String>,
    pub description: Option<String>,
    /// Default model. Empty/None = the specialist lane's model.
    pub model: Option<String>,
    /// Tool allowlist. None = built-in default (built-ins) / minimal (custom).
    pub tools: Option<Vec<String>>,
    pub enabled: bool,
    /// A custom agent remains provisional until the receipt-gated creation
    /// pipeline atomically publishes it. Built-in overrides are always ready.
    pub ready: bool,
    pub output_label: Option<String>,
    pub notes: Vec<String>,
    /// Custom agents only: the system prompt from `system.md`.
    pub system_prompt: Option<String>,
}

struct RegistryState {
    /// Custom agents keyed by role name.
    custom: HashMap<String, AgentManifest>,
    /// Built-in overrides keyed by role label ("coder").
    overrides: HashMap<String, AgentManifest>,
}

static REGISTRY: std::sync::OnceLock<RwLock<RegistryState>> = std::sync::OnceLock::new();

pub fn agents_dir() -> PathBuf {
    crate::config::phoenix_home().join("agents")
}

pub(crate) fn valid_role_name(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= MAX_ROLE_BYTES
        && role
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Construct one direct child of the agents root only after validating the
/// directory/talk label. This is shared by the configure UI so a name read
/// from disk can never become `agents/<name>/...` traversal.
pub(crate) fn agent_dir_for_role(role: &str) -> Result<PathBuf> {
    if !valid_role_name(role) {
        anyhow::bail!("invalid agent role name: {role:?}");
    }
    Ok(agents_dir().join(role))
}

pub fn reserved_agent_role(role: &str) -> bool {
    BUILTIN_ROLES.contains(&role)
        || RETIRED_BUILTIN_ROLES.contains(&role)
        || matches!(role, "orchestrator" | "user" | "librarian" | "lib")
}

fn reserved_custom_role(role: &str) -> bool {
    reserved_agent_role(role)
}

fn agent_directories() -> Result<Vec<(String, PathBuf)>> {
    agent_directories_at(&agents_dir())
}

fn agent_directories_at(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    crate::config::private_io::reject_symlink_components(dir)
        .with_context(|| format!("unsafe agents directory {}", dir.display()))?;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };

    let mut directories = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_AGENT_DIRECTORIES {
            anyhow::bail!("agents directory contains more than {MAX_AGENT_DIRECTORIES} entries");
        }
        let entry =
            entry.with_context(|| format!("failed to read an entry in {}", dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let Some(role) = entry.file_name().to_str().map(str::to_string) else {
            eprintln!("warning: non-UTF-8 agent directory name skipped");
            continue;
        };
        if !valid_role_name(&role) {
            eprintln!("warning: invalid agent directory name skipped: {role:?}");
            continue;
        }
        let path = entry.path();
        if path.parent() != Some(dir) {
            anyhow::bail!(
                "agent directory escaped the registry root: {}",
                path.display()
            );
        }
        crate::config::private_io::reject_symlink_components(&path)
            .with_context(|| format!("unsafe agent directory {}", path.display()))?;
        directories.push((role, path));
    }
    Ok(directories)
}

/// All safe custom role directories containing a regular bounded manifest,
/// including parked agents. Used by the configure picker.
pub(crate) fn custom_roles_on_disk() -> Result<Vec<String>> {
    custom_roles_on_disk_at(&agents_dir())
}

fn custom_roles_on_disk_at(root: &Path) -> Result<Vec<String>> {
    let mut roles = Vec::new();
    for (role, path) in agent_directories_at(root)? {
        if reserved_custom_role(&role) {
            continue;
        }
        if read_agent_text(
            &path.join("agent.toml"),
            AGENT_MANIFEST_MAX_BYTES,
            "agent manifest",
        )?
        .is_some()
        {
            roles.push(role);
        }
    }
    roles.sort();
    Ok(roles)
}

fn registry() -> &'static RwLock<RegistryState> {
    REGISTRY.get_or_init(|| {
        let state = load_from_disk().unwrap_or_else(|error| {
            eprintln!("warning: agent registry unreadable ({error:#}) — running on built-ins only");
            RegistryState {
                custom: HashMap::new(),
                overrides: HashMap::new(),
            }
        });
        // Intern every enabled custom role up front so talk-name resolution
        // and session deserialization can find them from the first turn.
        for role in state.custom.keys() {
            let _ = SubAgentType::custom(role);
        }
        RwLock::new(state)
    })
}

/// Reload the registry from disk after coworker provisioning changes the
/// agents directory. New custom roles are interned immediately.
pub fn refresh() {
    let fresh = match load_from_disk() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("warning: agent registry refresh failed ({error:#}) — keeping previous");
            return;
        }
    };
    for role in fresh.custom.keys() {
        let _ = SubAgentType::custom(role);
    }
    if let Ok(mut state) = registry().write() {
        *state = fresh;
    }
}

fn load_from_disk() -> Result<RegistryState> {
    load_from_disk_at(&agents_dir())
}

fn load_from_disk_at(root: &Path) -> Result<RegistryState> {
    let mut custom = HashMap::new();
    let mut overrides = HashMap::new();
    for (role, path) in agent_directories_at(root)? {
        // Keep old on-disk folders untouched for migration/history, but never
        // reinterpret a retired implementation lane as a custom coworker.
        if RETIRED_BUILTIN_ROLES.contains(&role.as_str()) {
            continue;
        }
        let manifest_path = path.join("agent.toml");
        match parse_manifest(&role, &manifest_path, &path) {
            Ok(manifest) => {
                if BUILTIN_ROLES.contains(&role.as_str()) {
                    overrides.insert(role, manifest);
                } else if reserved_custom_role(&role) {
                    eprintln!("warning: reserved custom agent role skipped: {role:?}");
                } else {
                    custom.insert(role, manifest);
                }
            }
            Err(error) => {
                eprintln!("warning: agent `{role}` skipped — bad agent.toml ({error:#})");
            }
        }
    }
    Ok(RegistryState { custom, overrides })
}

/// Runtime role labels for the eleven visible founding coworkers other than
/// Phoenix. These are the only compiled specialist destinations that can
/// receive new work.
pub const BUILTIN_ROLES: [&str; 10] = [
    "planner",
    "coder",
    "researcher",
    "frontend",
    "presentation",
    "finance",
    "critic",
    "sales",
    "marketing",
    "personal_logistics",
];

/// Historical serialized/runtime labels. Their files and sessions remain
/// readable, but they are neither exported, shown, routable, nor executable.
pub const RETIRED_BUILTIN_ROLES: [&str; 6] =
    ["browser", "computer_use", "database", "hacker", "tester", "scribe"];

fn read_agent_text(path: &Path, max_bytes: usize, label: &str) -> Result<Option<String>> {
    let Some(bytes) = crate::config::private_io::read_private_file(path)
        .with_context(|| format!("failed to read {label} {}", path.display()))?
    else {
        return Ok(None);
    };
    if bytes.len() > max_bytes {
        anyhow::bail!(
            "{label} {} is too large ({} bytes; max {max_bytes})",
            path.display(),
            bytes.len()
        );
    }
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{label} {} is not valid UTF-8", path.display()))?;
    Ok(Some(text))
}

fn manifest_string(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Option<String>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .with_context(|| format!("agent.toml field `{key}` must be a string"))?
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_FIELD_BYTES {
        anyhow::bail!("agent.toml field `{key}` is too large");
    }
    Ok(Some(value.to_string()))
}

fn manifest_string_array(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    max_items: usize,
) -> Result<Option<Vec<String>>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .with_context(|| format!("agent.toml field `{key}` must be an array of strings"))?;
    if values.len() > max_items {
        anyhow::bail!("agent.toml field `{key}` has too many entries (max {max_items})");
    }
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .with_context(|| format!("agent.toml field `{key}` must contain only strings"))?
            .trim();
        if value.is_empty() || value.len() > MAX_FIELD_BYTES {
            anyhow::bail!("agent.toml field `{key}` contains an empty or oversized value");
        }
        parsed.push(value.to_string());
    }
    Ok(Some(parsed))
}

fn parse_manifest(
    role: &str,
    manifest_path: &std::path::Path,
    agent_dir: &std::path::Path,
) -> Result<AgentManifest> {
    if !valid_role_name(role)
        || agent_dir.file_name().and_then(|name| name.to_str()) != Some(role)
        || manifest_path != agent_dir.join("agent.toml")
    {
        anyhow::bail!("agent manifest path is not contained by its validated role directory");
    }
    crate::config::private_io::reject_symlink_components(agent_dir)
        .with_context(|| format!("unsafe agent directory {}", agent_dir.display()))?;
    let raw = read_agent_text(manifest_path, AGENT_MANIFEST_MAX_BYTES, "agent manifest")?
        .with_context(|| format!("agent manifest is missing: {}", manifest_path.display()))?;
    let value: toml::Value = raw
        .parse()
        .with_context(|| format!("invalid TOML in {}", manifest_path.display()))?;
    let table = value.as_table().context("agent.toml is not a table")?;
    let tools = manifest_string_array(table, "tools", MAX_TOOLS)?;
    let notes = manifest_string_array(table, "notes", MAX_NOTES)?.unwrap_or_default();
    let enabled = match table.get("enabled") {
        None => true,
        Some(value) => value
            .as_bool()
            .context("agent.toml field `enabled` must be a boolean")?,
    };
    // Custom agents need a prompt; built-in overrides never carry one (their
    // prompts live in ~/.phoenix/prompts overlays — one prompt home, not two).
    let system_prompt = if BUILTIN_ROLES.contains(&role) {
        None
    } else {
        let prompt_path = agent_dir.join("system.md");
        let prompt = read_agent_text(
            &prompt_path,
            AGENT_SYSTEM_PROMPT_MAX_BYTES,
            "custom agent system prompt",
        )?
        .with_context(|| format!("custom agent needs {}", prompt_path.display()))?;
        if prompt.trim().is_empty() {
            anyhow::bail!("custom agent prompt {} is empty", prompt_path.display());
        }
        Some(prompt)
    };
    let ready = if BUILTIN_ROLES.contains(&role) {
        true
    } else {
        match table.get("ready") {
            None => false,
            Some(value) => value
                .as_bool()
                .context("agent.toml field `ready` must be a boolean")?,
        }
    };
    Ok(AgentManifest {
        role: role.to_string(),
        persona: manifest_string(table, "persona")?,
        description: manifest_string(table, "description")?,
        model: manifest_string(table, "model")?,
        tools,
        enabled,
        ready,
        output_label: manifest_string(table, "output_label")?,
        notes,
        system_prompt,
    })
}

/// The manifest for a custom agent, by interned id.
pub fn custom_manifest(id: u16) -> Option<AgentManifest> {
    let label = crate::session::custom_agent_label(id);
    registry().read().ok()?.custom.get(label).cloned()
}

/// Built-in override manifest for a role label, if the user wrote one.
pub fn builtin_override(role: &str) -> Option<AgentManifest> {
    registry().read().ok()?.overrides.get(role).cloned()
}

/// Enabled custom agents as (role, description) rows for configuration and
/// Canvas. This is intentionally disk-backed so a newly scaffolded pending
/// hire is visible without refreshing or mutating the runtime registry.
pub fn custom_roster() -> Vec<(String, String)> {
    let Ok(state) = load_from_disk() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String)> = state
        .custom
        .values()
        .filter(|m| m.enabled)
        .map(|m| {
            (
                m.role.clone(),
                m.description
                    .clone()
                    .unwrap_or_else(|| "custom specialist".to_string()),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// Complete safe custom-agent inventory for the durable company-directory
/// reconciliation pass. This deliberately reads disk instead of the cached
/// registry: boot repair must see provisional hires too, while
/// `production_ready` independently proves whether each hire may receive
/// work. A malformed entry remains quarantined by `load_from_disk`.
pub(crate) fn custom_directory_inventory() -> Result<Vec<(AgentManifest, bool)>> {
    let state = load_from_disk()?;
    let mut rows = state
        .custom
        .into_values()
        .map(|manifest| {
            let ready = production_ready(&manifest);
            (manifest, ready)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.role.cmp(&right.0.role));
    Ok(rows)
}

fn production_ready(manifest: &AgentManifest) -> bool {
    if !manifest.enabled || !manifest.ready {
        return false;
    }
    let dir = match agent_dir_for_role(&manifest.role) {
        Ok(dir) => dir,
        Err(_) => return false,
    };
    if crate::config::private_io::reject_symlink_components(&dir).is_err() {
        return false;
    }
    // A direct manifest edit can deliberately invalidate a published agent
    // without reloading this process. Read the current manifest bit here so a
    // stale in-memory registry cannot continue advertising it as ready.
    let manifest_path = dir.join("agent.toml");
    let Ok(Some(manifest_raw)) = crate::config::private_io::read_private_file_limited(
        &manifest_path,
        AGENT_MANIFEST_MAX_BYTES,
    ) else {
        return false;
    };
    let Some(manifest_value) = std::str::from_utf8(&manifest_raw)
        .ok()
        .and_then(|raw| raw.parse::<toml::Value>().ok())
    else {
        return false;
    };
    if manifest_value.get("ready").and_then(toml::Value::as_bool) != Some(true) {
        return false;
    }
    let prompt_path = dir.join("system.md");
    let Ok(Some(prompt)) = crate::config::private_io::read_private_file_limited(
        &prompt_path,
        AGENT_SYSTEM_PROMPT_MAX_BYTES,
    ) else {
        return false;
    };
    let Ok(prompt) = String::from_utf8(prompt) else {
        return false;
    };
    if prompt.trim().is_empty() || prompt.contains("PROVISIONAL PROMPT") {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("tools")) {
        for entry in entries.flatten() {
            let path = entry.path().join("server.py");
            if std::fs::symlink_metadata(&path).is_err() {
                continue;
            }
            let Ok(Some(source)) =
                crate::config::private_io::read_private_file_limited(&path, 4 * 1024 * 1024)
            else {
                return false;
            };
            let Ok(source) = String::from_utf8(source) else {
                return false;
            };
            if source.contains("not implemented") || source.contains("NotImplementedError") {
                return false;
            }
        }
    }
    // Current coworkers use Phoenix's compact two-step provisioning receipt.
    // It proves that the prompt was published by Phoenix and still matches the
    // content-addressed bytes on disk. Older pipeline receipts remain readable
    // below so existing user data migrates without a legacy UI mode.
    let simple_path = dir.join("provisioning.json");
    if let Ok(Some(raw)) =
        crate::config::private_io::read_private_file_limited(&simple_path, 512 * 1024)
    {
        if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&raw) {
            let expected_hash = state.get("prompt_sha256").and_then(|value| value.as_str());
            let actual_hash = Sha256::digest(prompt.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            return state.get("version").and_then(|value| value.as_u64()) == Some(3)
                && state.get("role").and_then(|value| value.as_str())
                    == Some(manifest.role.as_str())
                && state.get("status").and_then(|value| value.as_str()) == Some("ready")
                && state.get("refined_by").and_then(|value| value.as_str()) == Some("phoenix")
                && expected_hash == Some(actual_hash.as_str());
        }
        return false;
    }

    let path = dir.join("pipeline.json");
    let Ok(Some(raw)) = crate::config::private_io::read_private_file_limited(&path, 512 * 1024)
    else {
        return false;
    };
    let Ok(state) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return false;
    };
    let exam_run_ids = state.get("exam_run_ids").and_then(|value| value.as_array());
    let distinct_exam_runs = exam_run_ids.is_some_and(|values| {
        values.len() >= 2
            && values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
                == values.len()
    });
    state.get("version").and_then(|value| value.as_u64()) == Some(2)
        && state.get("status").and_then(|value| value.as_str()) == Some("ready")
        && state.get("stage").and_then(|value| value.as_str()) == Some("ready")
        && state
            .get("scope_approved")
            .and_then(|value| value.as_bool())
            == Some(true)
        && state
            .get("scope_receipt")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .get("research_receipt")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .get("prompt_receipt")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .get("tools_receipt")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .get("critic_receipt")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .get("critic_run_id")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value.starts_with("run_") && !value.trim().is_empty())
        && state
            .get("exam_receipts")
            .and_then(|value| value.as_array())
            .is_some_and(|values| values.len() >= 2)
        && distinct_exam_runs
}

/// Only receipt-complete custom agents are runtime-valid talk destinations.
/// `custom_roster` intentionally remains a disk roster for configure/Canvas,
/// so a pending hire is visible without being presented as a capability.
pub fn production_custom_roster() -> Vec<(String, String)> {
    let Ok(state) = registry().read() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String)> = state
        .custom
        .values()
        .filter(|manifest| production_ready(manifest))
        .map(|manifest| {
            (
                manifest.role.clone(),
                manifest
                    .description
                    .clone()
                    .unwrap_or_else(|| "custom specialist".to_string()),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// Resolve a receipt-complete production custom agent by talk name. Pending
/// disk roster entries never become runtime-valid through lazy initialization;
/// only validated publish refreshes them into an executable registry state.
pub fn resolve_custom_talk_name(name: &str) -> Option<SubAgentType> {
    let normalized = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let state = registry().read().ok()?;
    if let Some(manifest) = state.custom.get(&normalized) {
        if production_ready(manifest) {
            return Some(SubAgentType::custom(&normalized));
        }
    }
    // The UI and coworkers speak in editable human names. Resolve a persona
    // only when it identifies exactly one live coworker; duplicate names stay
    // explicit rather than routing a private message to the wrong person.
    let human = name.trim().to_lowercase();
    let mut matches = state.custom.values().filter(|manifest| {
        production_ready(manifest)
            && manifest
                .persona
                .as_deref()
                .is_some_and(|persona| persona.trim().to_lowercase() == human)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(SubAgentType::custom(&first.role))
}

/// Persona for a role from the registry (custom agents and built-in
/// overrides). Leaked to `&'static str` to match `agent_persona`'s signature;
/// personas are tiny and stable, so the leak is bounded.
pub fn registry_persona(role: &str) -> Option<&'static str> {
    let state = registry().read().ok()?;
    let persona = state
        .custom
        .get(role)
        .or_else(|| state.overrides.get(role))?
        .persona
        .clone()?;
    Some(Box::leak(persona.into_boxed_str()))
}

/// Build the runtime config for a custom agent. A registered-but-vanished
/// dir still returns a minimal config (sessions referencing it must load),
/// flagged in the prompt so the agent reports the misconfiguration honestly.
pub fn custom_config(id: u16) -> SubAgentConfig {
    let label = crate::session::custom_agent_label(id);
    let manifest = custom_manifest(id).unwrap_or_else(|| AgentManifest {
        role: label.to_string(),
        description: Some("custom agent whose definition dir is missing".to_string()),
        system_prompt: Some(format!(
            "You are `{label}`, a custom Phoenix specialist, but your definition at \
             ~/.phoenix/agents/{label}/ is missing or unreadable. Say exactly that in \
             your final answer and stop — do not improvise a role."
        )),
        enabled: true,
        ready: false,
        ..Default::default()
    });
    // Sanitize the allowlist against the live tool catalog: an unknown tool in
    // a hand-edited manifest must degrade to a warning, never a panic.
    let requested: Vec<String> = manifest.tools.clone().unwrap_or_else(default_custom_tools);
    let mut tools: Vec<String> = Vec::new();
    for tool in requested {
        if crate::tools::validate_tool_allowlist(std::slice::from_ref(&tool)).is_ok() {
            tools.push(tool);
        } else {
            eprintln!(
                "warning: agent `{label}` requests unknown tool `{tool}` — skipped (fix ~/.phoenix/agents/{label}/agent.toml)"
            );
        }
    }
    if !tools.iter().any(|t| t == "final_answer") {
        tools.push("final_answer".to_string());
    }
    let tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let notes: Vec<&str> = manifest.notes.iter().map(String::as_str).collect();
    let output_label = manifest
        .output_label
        .clone()
        .unwrap_or_else(|| "specialist report".to_string());
    let system_prompt = manifest
        .system_prompt
        .clone()
        .unwrap_or_else(|| format!("You are `{label}`, a Phoenix specialist."));
    let model = manifest
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
    let system_prompt = if manifest.ready {
        system_prompt
    } else {
        format!(
            "PROVISIONAL AGENT — this custom specialist has not passed the research, prompt, tools, entrance-exam, and critic gates. Do not claim production capability; report the missing pipeline stage and its receipts.\n\n{system_prompt}"
        )
    };
    SubAgentConfig {
        spec: super::framework::specialist_spec(
            SubAgentType::Custom(id),
            &system_prompt,
            &model,
            tool_refs,
            ExecutionStyle::StagedResearch,
            &output_label,
            vec![],
            notes,
        ),
    }
}

/// Preference order for a newly provisioned coworker. The runtime appends the
/// complete live tool catalog, so this never limits capability.
pub fn default_custom_tools() -> Vec<String> {
    [
        "read",
        "grep",
        "glob",
        "write",
        "str_replace",
        "web_search",
        "web_fetch",
        "todo_write",
        "skill",
        "skill_search",
        "talk",
        "final_answer",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Export built-in manifests to `~/.phoenix/agents/<role>/agent.toml` when
/// missing — the "agents live in .phoenix" contract: every agent has an
/// editable dir, first boot materializes the current compiled reality.
pub fn export_builtin_manifests() -> Result<usize> {
    let mut written = 0;
    crate::config::private_io::prepare_phoenix_directory(&agents_dir())?;
    for role in BUILTIN_ROLES {
        let agent = builtin_by_label(role);
        let Some(agent) = agent else { continue };
        let dir = agent_dir_for_role(role)?;
        let manifest_path = dir.join("agent.toml");
        crate::config::private_io::prepare_phoenix_directory(&dir)
            .with_context(|| format!("failed to prepare {}", dir.display()))?;
        let config = super::specialist_config(agent);
        let persona = crate::runtime::delegation::agent_persona(role).unwrap_or("");
        let tools = config
            .spec
            .tool_allowlist
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let notes = config
            .spec
            .workflow
            .notes
            .iter()
            .map(|n| format!("\"{}\"", n.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        let body = format!(
            "# {role} — built-in Phoenix specialist (exported manifest).\n\
             # Edit and restart the gateway to override the compiled defaults.\n\
             # The PROMPT lives in ~/.phoenix/prompts/{role}_system.md (overlay).\n\
             # Delete this file to fall back to compiled defaults.\n\
             persona = \"{persona}\"\n\
             description = \"{description}\"\n\
             # Empty model = the configured specialist lane's model.\n\
             model = \"\"\n\
             tools = [{tools}]\n\
             enabled = true\n\
             output_label = \"{output_label}\"\n\
             notes = [{notes}]\n",
            description = config.spec.output.label,
            output_label = config.spec.output.label,
        );
        if crate::config::private_io::atomic_write_private_if_missing(
            &manifest_path,
            body.as_bytes(),
        )
        .with_context(|| format!("failed to write {}", manifest_path.display()))?
        {
            written += 1;
        }
    }
    Ok(written)
}

/// Built-in SubAgentType for a role label.
pub fn builtin_by_label(role: &str) -> Option<SubAgentType> {
    Some(match role {
        "planner" => SubAgentType::Planner,
        "coder" => SubAgentType::Coder,
        "researcher" => SubAgentType::Researcher,
        "browser" => SubAgentType::Browser,
        "frontend" => SubAgentType::Frontend,
        "presentation" => SubAgentType::Presentation,
        "finance" => SubAgentType::Finance,
        "computer_use" => SubAgentType::ComputerUse,
        "database" => SubAgentType::Database,
        "hacker" => SubAgentType::Hacker,
        "critic" => SubAgentType::Critic,
        "tester" => SubAgentType::Tester,
        "scribe" => SubAgentType::Scribe,
        "sales" => SubAgentType::Sales,
        "marketing" => SubAgentType::Marketing,
        "personal_logistics" => SubAgentType::PersonalLogistics,
        _ => return None,
    })
}

/// Apply a built-in's registry override. A manifest tool list is a preferred
/// ordering, not a capability denial list: every coworker still receives the
/// complete live catalog and runtime permission gates decide what may run.
pub fn apply_builtin_override(role: &str, mut config: SubAgentConfig) -> SubAgentConfig {
    let Some(over) = builtin_override(role) else {
        return config;
    };
    if let Some(model) = over.model.filter(|m| !m.trim().is_empty()) {
        config.spec.default_model = model;
    }
    if let Some(tools) = over.tools {
        let sane = apply_tools_override_named(tools, role);
        if !sane.is_empty() {
            config.spec.tool_allowlist = sane;
        }
    }
    config
}

/// Validate an override's preferred order and append the live universal
/// catalog. Unknown hand-edited names degrade to a warning, never a panic.
fn apply_tools_override_named(tools: Vec<String>, role: &str) -> Vec<String> {
    let mut sane: Vec<String> = Vec::new();
    for tool in tools {
        if crate::tools::validate_tool_allowlist(std::slice::from_ref(&tool)).is_ok() {
            sane.push(tool);
        } else {
            eprintln!("warning: {role} override requests unknown tool `{tool}` — skipped");
        }
    }
    crate::tools::merge_with_universal_tools(sane)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_custom_agent(root: &Path, role: &str) -> PathBuf {
        let dir = root.join(role);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "persona = \"Ledger\"\ndescription = \"market specialist\"\ntools = [\"read\", \"final_answer\"]\nenabled = true\nnotes = [\"careful\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("system.md"), "You are the market specialist.").unwrap();
        dir
    }

    #[test]
    fn registry_loads_only_valid_direct_child_role_names() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agents");
        std::fs::create_dir(&root).unwrap();
        write_custom_agent(&root, "trader");
        write_custom_agent(&root, "Bad Role");
        write_custom_agent(&root, &"x".repeat(MAX_ROLE_BYTES + 1));
        write_custom_agent(&root, "orchestrator");

        let state = load_from_disk_at(&root).unwrap();
        assert!(state.custom.contains_key("trader"));
        assert_eq!(state.custom.len(), 1);
        assert_eq!(custom_roles_on_disk_at(&root).unwrap(), vec!["trader"]);
    }

    #[test]
    fn malformed_manifest_fields_skip_the_agent_instead_of_partially_defaulting() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agents");
        let dir = write_custom_agent(&root, "trader");
        let corrupt = b"persona = \"Ledger\"\nenabled = \"yes\"\n";
        std::fs::write(dir.join("agent.toml"), corrupt).unwrap();

        let state = load_from_disk_at(&root).unwrap();
        assert!(state.custom.is_empty());
        assert_eq!(std::fs::read(dir.join("agent.toml")).unwrap(), corrupt);
    }

    #[test]
    fn oversized_agent_manifest_is_bounded_and_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agents");
        let dir = write_custom_agent(&root, "trader");
        let file = std::fs::File::create(dir.join("agent.toml")).unwrap();
        file.set_len(AGENT_MANIFEST_MAX_BYTES as u64 + 1).unwrap();

        assert!(load_from_disk_at(&root).unwrap().custom.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_agents_root_and_agent_directories_are_not_followed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        write_custom_agent(&outside, "trader");

        let linked_root = temp.path().join("agents");
        symlink(&outside, &linked_root).unwrap();
        assert!(load_from_disk_at(&linked_root).is_err());

        let safe_root = temp.path().join("safe-agents");
        std::fs::create_dir(&safe_root).unwrap();
        symlink(outside.join("trader"), safe_root.join("trader")).unwrap();
        assert!(load_from_disk_at(&safe_root).unwrap().custom.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_and_prompt_are_not_followed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agents");
        let dir = write_custom_agent(&root, "trader");
        let outside_manifest = temp.path().join("outside.toml");
        std::fs::write(&outside_manifest, "enabled = true\n").unwrap();
        std::fs::remove_file(dir.join("agent.toml")).unwrap();
        symlink(&outside_manifest, dir.join("agent.toml")).unwrap();
        assert!(load_from_disk_at(&root).unwrap().custom.is_empty());

        std::fs::remove_file(dir.join("agent.toml")).unwrap();
        std::fs::write(dir.join("agent.toml"), "enabled = true\n").unwrap();
        let outside_prompt = temp.path().join("outside.md");
        std::fs::write(&outside_prompt, "outside prompt").unwrap();
        std::fs::remove_file(dir.join("system.md")).unwrap();
        symlink(&outside_prompt, dir.join("system.md")).unwrap();
        assert!(load_from_disk_at(&root).unwrap().custom.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_manifest_is_skipped_without_blocking_registry_refresh() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("agents");
        let dir = root.join("trader");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("system.md"), "safe prompt").unwrap();
        let manifest = dir.join("agent.toml");
        let c_path = CString::new(manifest.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        assert!(load_from_disk_at(&root).unwrap().custom.is_empty());
    }

    /// A months-old `agent.toml` must not freeze an agent out of any capability
    /// added since it was written.
    #[test]
    fn a_stale_manifest_cannot_drop_universal_company_tools() {
        let stale = vec![
            "read".to_string(),
            "write".to_string(),
            "agent_pipeline".to_string(),
            "tools_assign".to_string(),
        ];
        let sane = apply_tools_override_named(stale, "coder");
        for tool in crate::tools::universal_agent_tool_names() {
            assert!(
                sane.iter().any(|candidate| candidate == &tool),
                "the universal catalog must survive a stale manifest: {tool}"
            );
        }
        assert_eq!(&sane[..2], &["read", "write"]);
        assert!(!sane.iter().any(|tool| tool == "browser_swarm"));
        assert!(!sane.iter().any(|tool| tool == "agent_pipeline"));
        assert!(!sane.iter().any(|tool| tool == "tools_assign"));
    }

    #[test]
    fn custom_intern_round_trips_names() {
        let a = SubAgentType::custom("Trader");
        let b = SubAgentType::custom("trader");
        let c = SubAgentType::custom("day-trader");
        assert_eq!(a, b, "normalization makes Trader/trader the same agent");
        assert_ne!(a, c);
        assert_eq!(SubAgentType::find_custom("TRADER"), Some(a));
        assert_eq!(SubAgentType::find_custom("nonexistent-agent-xyz"), None);
        // Display capitalizes; serde round-trips through the role name.
        let json = serde_json::to_string(&a).unwrap();
        let back: SubAgentType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn builtin_serde_format_is_unchanged() {
        // Old session files store bare unit-variant strings — they must load.
        let json = serde_json::to_string(&SubAgentType::Coder).unwrap();
        assert_eq!(json, "\"Coder\"");
        let back: SubAgentType = serde_json::from_str("\"ComputerUse\"").unwrap();
        assert_eq!(back, SubAgentType::ComputerUse);
    }

    #[test]
    fn custom_config_sanitizes_unknown_tools_and_keeps_final_answer() {
        let agent = SubAgentType::custom("registry-test-ghost");
        let SubAgentType::Custom(id) = agent else {
            panic!("custom() must return Custom")
        };
        // No dir on disk → fallback config, minimal but valid.
        let config = custom_config(id);
        assert!(config
            .spec
            .tool_allowlist
            .iter()
            .any(|t| t == "final_answer"));
        assert!(config.spec.system_prompt.contains("registry_test_ghost"));
    }

    #[test]
    fn builtin_roles_match_specialist_labels() {
        assert_eq!(BUILTIN_ROLES.len(), 10);
        for role in BUILTIN_ROLES {
            let agent = builtin_by_label(role).expect("every builtin role resolves");
            assert_eq!(crate::runtime::delegation::specialist_label(agent), role);
            let config = super::super::specialist_config(agent);
            assert!(
                config.spec.tool_allowlist.iter().any(|tool| tool == "talk"),
                "built-in `{role}` cannot hand work to another agent"
            );
            assert!(
                config
                    .spec
                    .tool_allowlist
                    .iter()
                    .any(|tool| tool == "final_answer"),
                "built-in `{role}` cannot finish its own turn"
            );
            for tool in crate::tools::universal_agent_tool_names() {
                assert!(
                    config.spec.tool_allowlist.contains(&tool),
                    "visible coworker `{role}` is missing universal capability `{tool}`"
                );
            }
        }
        for retired in RETIRED_BUILTIN_ROLES {
            assert!(!BUILTIN_ROLES.contains(&retired));
            assert!(reserved_agent_role(retired));
        }
    }

    #[test]
    fn custom_agent_floor_can_handoff_and_finish() {
        let tools = default_custom_tools();
        assert!(tools.iter().any(|tool| tool == "talk"));
        assert!(tools.iter().any(|tool| tool == "final_answer"));
    }
}
