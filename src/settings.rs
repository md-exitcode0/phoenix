//! Typed, searchable, dependency-aware product settings.
//!
//! Secrets never enter this store. Provider credentials and website passwords
//! stay in their dedicated encrypted vaults; this document only records
//! behavior and presentation choices. `canvas-prefs.json` is read once as a
//! migration source and is never modified or removed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SETTINGS_VERSION: u32 = 1;
const SETTINGS_MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCOPE_ENTRIES: usize = 256;
const MAX_OVERRIDES_PER_SCOPE: usize = 512;

fn settings_default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SettingsScope {
    Global,
    Agent { id: String },
    Group { id: String },
}

impl Default for SettingsScope {
    fn default() -> Self {
        Self::Global
    }
}

impl SettingsScope {
    fn label(&self) -> String {
        match self {
            Self::Global => "global".into(),
            Self::Agent { id } => format!("agent:{id}"),
            Self::Group { id } => format!("group:{id}"),
        }
    }

    fn validate(&self) -> Result<()> {
        let id = match self {
            Self::Global => return Ok(()),
            Self::Agent { id } | Self::Group { id } => id,
        };
        if id.is_empty()
            || id.len() > 96
            || !id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            anyhow::bail!("invalid settings scope id")
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SettingControl {
    Toggle,
    Select { options: Vec<SettingOption> },
    Number { min: f64, max: f64, step: f64 },
    Text { max_length: usize },
    Color,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingDependency {
    pub key: String,
    pub equals: Value,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingDefinition {
    pub key: String,
    pub section: String,
    pub label: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub control: SettingControl,
    pub default: Value,
    pub advanced: bool,
    pub agent_override: bool,
    pub group_override: bool,
    pub requires_restart: bool,
    pub dependencies: Vec<SettingDependency>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResolvedSetting {
    pub definition: SettingDefinition,
    pub value: Value,
    pub inherited_from: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsGuideEntry {
    pub title: String,
    pub description: String,
    pub section: String,
    pub search_terms: Vec<String>,
}

/// A searchable settings destination backed by a specialized control plane
/// rather than a scalar toggle. This keeps credentials, prompts, routines,
/// schedules, trash, providers, and integrations discoverable without ever
/// copying their sensitive or high-volume data into `settings.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsDestination {
    pub id: String,
    pub label: String,
    pub description: String,
    pub section: String,
    pub route: String,
    pub keywords: Vec<String>,
    pub advanced: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingsSnapshot {
    pub revision: u64,
    pub scope: SettingsScope,
    pub settings: Vec<ResolvedSetting>,
    pub guide: Vec<SettingsGuideEntry>,
    pub sections: Vec<String>,
    pub destinations: Vec<SettingsDestination>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingsSearchResults {
    pub revision: u64,
    pub scope: SettingsScope,
    pub regular: Vec<ResolvedSetting>,
    pub advanced: Vec<ResolvedSetting>,
    pub regular_destinations: Vec<SettingsDestination>,
    pub advanced_destinations: Vec<SettingsDestination>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub name: String,
    pub context_window: u32,
    pub reasoning: bool,
    pub effort_levels: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCatalogEntry {
    pub id: String,
    pub name: String,
    pub auth_kind: String,
    pub models: Vec<ModelCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAccountSummary {
    pub profile_id: String,
    pub provider_id: String,
    pub auth_kind: String,
    pub display_name: Option<String>,
    pub expires_at: Option<i64>,
    pub cooling_down_until: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelLaneSetting {
    pub lane: String,
    pub provider_id: String,
    pub model: String,
    /// True for a named coworker whose primary route is inherited from the
    /// company-wide specialist lane. A coworker can still own an independent
    /// fallback chain while inheriting its primary model.
    #[serde(default)]
    pub inherited: bool,
    /// False when an optional capability lane has no model configured. The
    /// provider/model fields remain an editable suggestion for the UI; they
    /// must not be presented as a live runtime assignment.
    #[serde(default = "settings_default_true")]
    pub configured: bool,
    pub reasoning_effort: Option<String>,
    pub auth_profile_id: Option<String>,
    /// Effective usable ceiling for this lane after applying a user override.
    pub context_window: Option<u64>,
    /// Catalog maximum for the selected model. The desktop never permits a
    /// custom ceiling above this value.
    pub max_context_window: Option<u64>,
    /// Explicit lower ceiling, or None when the lane uses the model maximum.
    pub context_window_override: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSettingsSnapshot {
    pub config_revision: String,
    pub lanes: Vec<ModelLaneSetting>,
    pub providers: Vec<ProviderCatalogEntry>,
    pub accounts: Vec<ProviderAccountSummary>,
    pub fallback_chains: BTreeMap<String, Vec<String>>,
    pub compaction_modes: BTreeMap<String, String>,
    pub codex_efficiency: CodexEfficiencyStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexEfficiencyStatus {
    pub available: bool,
    pub active: bool,
    pub phoenix_model: String,
    pub team_model: String,
    pub team_effort: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum SettingsCommand {
    Snapshot {
        #[serde(default)]
        scope: SettingsScope,
    },
    Search {
        query: String,
        #[serde(default)]
        section: Option<String>,
        #[serde(default)]
        scope: SettingsScope,
    },
    Set {
        key: String,
        value: Value,
        #[serde(default)]
        scope: SettingsScope,
        #[serde(default)]
        expected_revision: Option<u64>,
    },
    Reset {
        key: String,
        #[serde(default)]
        scope: SettingsScope,
        #[serde(default)]
        expected_revision: Option<u64>,
    },
    ModelsSnapshot,
    ApplyCodexEfficiencyDefaults {
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
    SetModelLane {
        lane: String,
        provider_id: String,
        model: String,
        #[serde(default)]
        reasoning_effort: Option<String>,
        #[serde(default)]
        auth_profile_id: Option<String>,
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
    SetModelContextWindow {
        lane: String,
        /// None restores the selected model's catalog maximum.
        #[serde(default)]
        context_window: Option<u64>,
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
    ResetModelLane {
        lane: String,
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
    SetFallbackChain {
        lane: String,
        profile_ids: Vec<String>,
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
    SetCompactionMode {
        provider_id: String,
        mode: String,
        #[serde(default)]
        expected_config_revision: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum SettingsReply {
    Snapshot { snapshot: SettingsSnapshot },
    Search { results: SettingsSearchResults },
    Models { snapshot: ModelSettingsSnapshot },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingsDocument {
    version: u32,
    revision: u64,
    updated_at: String,
    #[serde(default)]
    migrated_canvas_preferences: bool,
    #[serde(default)]
    global: BTreeMap<String, Value>,
    #[serde(default)]
    agents: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    groups: BTreeMap<String, BTreeMap<String, Value>>,
}

#[derive(Clone)]
struct RuntimeSettingsCache {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
    document: SettingsDocument,
}

fn runtime_cache() -> &'static std::sync::Mutex<Option<RuntimeSettingsCache>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<RuntimeSettingsCache>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

fn invalidate_runtime_cache() {
    *runtime_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            revision: 0,
            updated_at: Utc::now().to_rfc3339(),
            migrated_canvas_preferences: false,
            global: BTreeMap::new(),
            agents: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }
}

fn opt(value: &str, label: &str) -> SettingOption {
    SettingOption {
        value: value.into(),
        label: label.into(),
    }
}

fn dep(key: &str, equals: Value, reason: &str) -> SettingDependency {
    SettingDependency {
        key: key.into(),
        equals,
        reason: reason.into(),
    }
}

fn setting(
    key: &str,
    section: &str,
    label: &str,
    description: &str,
    control: SettingControl,
    default: Value,
) -> SettingDefinition {
    SettingDefinition {
        key: key.into(),
        section: section.into(),
        label: label.into(),
        description: description.into(),
        keywords: Vec::new(),
        control,
        default,
        advanced: false,
        agent_override: false,
        group_override: false,
        requires_restart: false,
        dependencies: Vec::new(),
    }
}

fn catalog() -> Vec<SettingDefinition> {
    let mut out = vec![
        setting(
            "appearance.theme",
            "Appearance",
            "Theme",
            "Use Phoenix in light, dark, or system-matched mode.",
            SettingControl::Select {
                options: vec![
                    opt("light", "Light"),
                    opt("dark", "Dark"),
                    opt("system", "System"),
                ],
            },
            json!("light"),
        ),
        setting(
            "appearance.motion",
            "Appearance",
            "Motion",
            "Controls interface and agent activity animation intensity.",
            SettingControl::Select {
                options: vec![
                    opt("full", "Full"),
                    opt("reduced", "Reduced"),
                    opt("minimal", "Minimal"),
                ],
            },
            json!("full"),
        ),
        setting(
            "appearance.interface_scale",
            "Appearance",
            "Interface scale",
            "Scales the full desktop interface.",
            SettingControl::Number {
                min: 0.8,
                max: 1.4,
                step: 0.05,
            },
            json!(1.0),
        ),
        setting(
            "appearance.font",
            "Appearance",
            "Interface font",
            "Phoenix uses Apfel Grotezk throughout the interface.",
            SettingControl::Select {
                options: vec![opt("apfel", "Apfel Grotezk")],
            },
            json!("apfel"),
        ),
        setting(
            "appearance.background",
            "Appearance",
            "Background",
            "Selects the subtle Phoenix background treatment.",
            SettingControl::Select {
                options: vec![
                    opt("phoenix", "Phoenix"),
                    opt("mountains", "Mountains"),
                    opt("none", "None"),
                ],
            },
            json!("phoenix"),
        ),
        setting(
            "appearance.coworker_icon_style",
            "Appearance",
            "Coworker fire",
            "Every coworker uses the same fire, tinted with their color.",
            SettingControl::Select {
                options: vec![
                    opt("flame_crests", "Rising fire"),
                    opt("bird_family", "Winged fire"),
                    opt("character_glyphs", "Split crest"),
                ],
            },
            json!("flame_crests"),
        ),
        setting(
            "appearance.stage_fire",
            "Appearance",
            "Conversation fire",
            "Washes the conversation with the same ember field as the company rail.",
            SettingControl::Select {
                options: vec![
                    opt("full", "Full"),
                    opt("low", "Low"),
                    opt("off", "Off"),
                ],
            },
            json!("full"),
        ),
        setting(
            "appearance.icon_flicker",
            "Appearance",
            "Fire motion",
            "Idle flicker on coworker fire. Transform-only so it stays on the compositor.",
            SettingControl::Select {
                options: vec![opt("on", "Alive"), opt("off", "Still")],
            },
            json!("on"),
        ),
        setting(
            "appearance.type_size",
            "Appearance",
            "Type size",
            "Scales settings and conversation type without zooming the window.",
            SettingControl::Select {
                options: vec![
                    opt("small", "Small"),
                    opt("regular", "Regular"),
                    opt("large", "Large"),
                ],
            },
            json!("regular"),
        ),
        setting(
            "appearance.rail_weight",
            "Appearance",
            "Prompt rail",
            "How loud the left prompt markers are.",
            SettingControl::Select {
                options: vec![opt("bold", "Bold"), opt("regular", "Quiet")],
            },
            json!("bold"),
        ),
        setting(
            "appearance.composer_glow",
            "Appearance",
            "Composer focus",
            "Warm ember ring when the composer is focused.",
            SettingControl::Select {
                options: vec![opt("ember", "Ember"), opt("quiet", "Quiet")],
            },
            json!("ember"),
        ),
        setting(
            "appearance.reduce_transparency",
            "Appearance",
            "Reduce transparency",
            "Uses opaque surfaces and disables backdrop blur.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "appearance.grain",
            "Appearance",
            "Film grain",
            "Adds a very subtle surface texture. Off by default: the overlay can paint a black tile on WebKitGTK.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "appearance.conversation_view",
            "Appearance",
            "Conversation detail",
            "Compact keeps the thread scannable and folds code out of the way. Detailed shows the agent's edits inline as syntax-highlighted code.",
            SettingControl::Select {
                options: vec![opt("compact", "Compact"), opt("detailed", "Detailed")],
            },
            json!("compact"),
        ),
        setting(
            "appearance.density",
            "Appearance",
            "Density",
            "How tight the company rail, settings rows, and composer chrome sit.",
            SettingControl::Select {
                options: vec![opt("comfortable", "Comfortable"), opt("compact", "Compact")],
            },
            json!("comfortable"),
        ),
        setting(
            "appearance.radius",
            "Appearance",
            "Corners",
            "Rounded or sharp edges on cards, the composer, and menus.",
            SettingControl::Select {
                options: vec![
                    opt("sharp", "Sharp"),
                    opt("soft", "Soft"),
                    opt("round", "Round"),
                ],
            },
            json!("soft"),
        ),
        setting(
            "appearance.sidebar_fire",
            "Appearance",
            "Sidebar fire",
            "Warm wash behind the company rail.",
            SettingControl::Select {
                options: vec![opt("full", "Full"), opt("low", "Low"), opt("off", "Off")],
            },
            json!("full"),
        ),
        setting(
            "appearance.contrast",
            "Appearance",
            "Contrast",
            "Ink strength against the paper.",
            SettingControl::Select {
                options: vec![
                    opt("soft", "Soft"),
                    opt("standard", "Standard"),
                    opt("high", "High"),
                ],
            },
            json!("standard"),
        ),
        setting(
            "appearance.feed",
            "Appearance",
            "Conversation width",
            "How wide the thread sits in the stage.",
            SettingControl::Select {
                options: vec![
                    opt("narrow", "Narrow"),
                    opt("regular", "Regular"),
                    opt("wide", "Wide"),
                ],
            },
            json!("regular"),
        ),
        setting(
            "appearance.accent",
            "Appearance",
            "Accent",
            "The ember color used for fire, checks, and focus.",
            SettingControl::Select {
                options: vec![
                    opt("ember", "Ember"),
                    opt("gold", "Gold"),
                    opt("rose", "Rose"),
                    opt("copper", "Copper"),
                ],
            },
            json!("ember"),
        ),
        setting(
            "sidebar.width",
            "General",
            "Sidebar width",
            "Default expanded width; the sidebar remains directly resizable.",
            SettingControl::Number {
                min: 272.0,
                max: 432.0,
                step: 8.0,
            },
            json!(336),
        ),
        setting(
            "sidebar.prompt_rail_count",
            "General",
            "Visible prompt markers",
            "How many recent user prompts appear in conversation navigation at once.",
            SettingControl::Number {
                min: 4.0,
                max: 10.0,
                step: 1.0,
            },
            json!(6),
        ),
        setting(
            "conversation.initial_visible_turns",
            "General",
            "Recent turns on open",
            "Render this many recent user turns immediately. Older turns stay complete and load smoothly when you scroll upward.",
            SettingControl::Number {
                min: 5.0,
                max: 50.0,
                step: 1.0,
            },
            json!(5),
        ),
        setting(
            "composer.default_permission",
            "Composer",
            "Default access",
            "Default access for new messages; per-message changes remain sticky per agent.",
            SettingControl::Select {
                options: vec![
                    opt("talk", "Talk"),
                    opt("workspace", "Workspace"),
                    opt("full_access", "Full access"),
                ],
            },
            json!("workspace"),
        ),
        setting(
            "composer.show_context_usage",
            "Composer",
            "Show context usage",
            "Shows live model context usage beside the context-window slider.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "composer.show_reasoning",
            "Composer",
            "Show reasoning",
            "Shows model reasoning when the provider exposes it.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "composer.send_shortcut",
            "Composer",
            "Send shortcut",
            "Keyboard shortcut used to send a message.",
            SettingControl::Select {
                options: vec![opt("enter", "Enter"), opt("cmd_enter", "Cmd/Ctrl + Enter")],
            },
            json!("enter"),
        ),
        setting(
            "agents.collaboration_enabled",
            "Agents & Groups",
            "Agent collaboration",
            "Allows coworkers to delegate and talk to one another.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "agents.volume_workers_enabled",
            "Agents & Groups",
            "Volume workers",
            "Enables temporary workers for independent batch items. Workers inherit the caller's permission mode and receive isolated terminal, desktop, and browser contexts; they clean themselves up when done or cancelled.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "agents.volume_worker_limit",
            "Agents & Groups",
            "Default worker concurrency",
            "Default parallelism when a batch does not choose its own. An explicit batch can use all of its independent items; Phoenix imposes no fixed worker-count ceiling.",
            SettingControl::Number {
                min: 1.0,
                // The settings wire format predates unbounded numeric fields.
                // This is only the editable default, not a runtime ceiling;
                // explicit batch concurrency is bounded solely by item count.
                max: 4_294_967_295.0,
                step: 1.0,
            },
            json!(8),
        ),
        setting(
            "agents.outside_group_approval",
            "Agents & Groups",
            "Approve outside-group calls",
            "Ask once before a group calls a coworker outside the group.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "agents.read_group_transcript",
            "Agents & Groups",
            "Read group transcript",
            "Pinged coworkers receive the relevant group transcript by default.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "agents.model_assignment",
            "Models & Providers",
            "Agent model policy",
            "Whether coworkers inherit the company model or use individual assignments.",
            SettingControl::Select {
                options: vec![
                    opt("inherit", "Company default"),
                    opt("individual", "Per agent"),
                ],
            },
            // Phoenix has always honored explicit per-agent lanes. Keep that
            // behavior as the default; selecting Company default is the
            // deliberate action that suppresses individual model/account/
            // effort overrides at runtime.
            json!("individual"),
        ),
        setting(
            "models.fallback_enabled",
            "Models & Providers",
            "Automatic fallbacks",
            "Try connected accounts with the selected model first, then explicitly configured provider backups.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "permissions.action_review",
            "Permissions",
            "Adaptive action review",
            "Reviews every tool at the shared runtime boundary. Shadow records anomalies without blocking; Enforce blocks only high-confidence dangerous actions. Existing exact-action approvals remain authoritative.",
            SettingControl::Select {
                options: vec![
                    opt("shadow", "Shadow"),
                    opt("enforce", "Enforce"),
                    opt("off", "Off"),
                ],
            },
            json!("shadow"),
        ),
        setting(
            "permissions.shell_isolation",
            "Permissions",
            "Workspace shell isolation",
            "Runs Workspace-mode shell commands in an OS sandbox that hides user homes, mounts only the project and private build caches writable, and separates process, IPC, and host identity namespaces. Full Access intentionally runs outside this boundary.",
            SettingControl::Select {
                options: vec![
                    opt("auto", "Automatic"),
                    opt("require", "Require sandbox"),
                    opt("off", "Off"),
                ],
            },
            json!("auto"),
        ),
        setting(
            "permissions.login_import",
            "Permissions",
            "Cookie import approval",
            "Automatically copies portable site sessions from the active browser. Device-bound sessions are never copied.",
            SettingControl::Select {
                options: vec![
                    opt("ask", "Ask once"),
                    opt("allow", "Allow"),
                    opt("deny", "Deny"),
                ],
            },
            json!("allow"),
        ),
        setting(
            "permissions.account_creation",
            "Permissions",
            "Account creation approval",
            "Lets agents create free website accounts automatically. Paid plans always require fresh approval.",
            SettingControl::Select {
                options: vec![
                    opt("ask", "Ask"),
                    opt("allow_free", "Allow free accounts"),
                    opt("deny", "Deny"),
                ],
            },
            json!("allow_free"),
        ),
        setting(
            "permissions.external_send",
            "Permissions",
            "External sends",
            "Requires approval before sending email, messages, or publishing externally.",
            SettingControl::Select {
                options: vec![
                    opt("ask", "Ask"),
                    opt("allow", "Allow"),
                    opt("deny", "Deny"),
                ],
            },
            json!("ask"),
        ),
        setting(
            "permissions.external_delete",
            "Permissions",
            "External deletions",
            "Controls approval before deleting, removing, revoking, or trashing data in connected services.",
            SettingControl::Select {
                options: vec![
                    opt("ask", "Ask"),
                    opt("allow", "Allow"),
                    opt("deny", "Deny"),
                ],
            },
            json!("ask"),
        ),
        setting(
            "permissions.purchases",
            "Permissions",
            "Purchases and paid plans",
            "Monetary actions require explicit approval.",
            SettingControl::Select {
                options: vec![opt("always_ask", "Always ask"), opt("deny", "Deny")],
            },
            json!("always_ask"),
        ),
        setting(
            "browser.enabled",
            "Browser & Accounts",
            "Browser tools",
            "Allows coworkers to use their private managed browser profiles.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "browser.suspend_after_seconds",
            "Browser & Accounts",
            "Suspend idle browsers",
            "Persistent coworker browsers stay open until the gateway stops. Set a nonzero value only to opt into idle suspension.",
            SettingControl::Number {
                min: 0.0,
                max: 86400.0,
                step: 60.0,
            },
            json!(0),
        ),
        setting(
            "browser.profile_source",
            "Browser & Accounts",
            "Managed browser profile source",
            "Chooses Phoenix's durable private profile or an existing Chromium profile source.",
            SettingControl::Text { max_length: 512 },
            json!(""),
        ),
        setting(
            "browser.cookie_import_source",
            "Browser & Accounts",
            "Cookie import source",
            "Selects the installed browser used for portable sessions. Automatic follows the browser used most recently.",
            SettingControl::Select {
                options: vec![
                    opt("auto", "Automatic (recommended)"),
                    opt("none", "None"),
                    opt("chrome", "Chrome"),
                    opt("chromium", "Chromium"),
                    opt("brave", "Brave"),
                    opt("firefox", "Firefox"),
                    opt("zen", "Zen"),
                    opt("edge", "Edge"),
                    opt("librewolf", "LibreWolf"),
                    opt("floorp", "Floorp"),
                    opt("waterfox", "Waterfox"),
                    opt("vivaldi", "Vivaldi"),
                    opt("opera", "Opera"),
                ],
            },
            json!("auto"),
        ),
        setting(
            "browser.headless",
            "Browser & Accounts",
            "Headless managed browser",
            "Runs background coworker browsers without a separate operating-system window.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "browser.attach_port",
            "Advanced",
            "Browser attach port",
            "CDP port used only when attaching to a compatible running Chromium browser.",
            SettingControl::Number {
                min: 1.0,
                max: 65535.0,
                step: 1.0,
            },
            json!(9222),
        ),
        setting(
            "browser.binary",
            "Advanced",
            "Custom Chromium binary",
            "Optional absolute path to a compatible browser binary; empty uses autodetection.",
            SettingControl::Text { max_length: 4096 },
            json!(""),
        ),
        setting(
            "browser.auto_solve_captcha",
            "Browser & Accounts",
            "Automatic CAPTCHA handling",
            "Lets the managed stealth browser attempt supported challenges.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "browser.account_creation",
            "Browser & Accounts",
            "Agent-created accounts",
            "Enables the create-account path when policy permits it.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "browser.automatic_2fa_handoff",
            "Browser & Accounts",
            "Automatic 2FA handoff",
            "Lets an agent request a verification code from the relevant coworker.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "memory.enabled",
            "Memory",
            "Long-term memory",
            "Keeps Phoenix and coworkers grounded in durable memory.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "memory.group_transcript_learning",
            "Memory",
            "Learn from group decisions",
            "Allows the librarian to retain durable decisions from group transcripts.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "workflows.enabled",
            "Workflows & Routines",
            "Saved workflows",
            "Allows coworkers to match and run taught workflows.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "workflows.auto_run",
            "Workflows & Routines",
            "Run trusted workflows automatically",
            "Runs healthy trusted routines without re-teaching each step.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "workflows.share_by_default",
            "Workflows & Routines",
            "Share new workflows",
            "New taught workflows default to company scope instead of the current coworker.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "workflows.failure_quarantine",
            "Workflows & Routines",
            "Quarantine failing workflows",
            "Marks repeatedly failing routines obsolete and asks what to do next.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "schedules.enabled",
            "Schedules",
            "Scheduled work",
            "Allows coworkers to create and run recurring or delayed work.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "notifications.enabled",
            "Notifications",
            "Notifications",
            "Enables Phoenix attention and completion notifications.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "notifications.system",
            "Notifications",
            "System notifications",
            "Uses the operating system notification service when supported.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "notifications.attention",
            "Notifications",
            "Attention requests",
            "Notifies when a coworker needs an answer, approval, login, or other input.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "notifications.completions",
            "Notifications",
            "Finished work",
            "Notifies when the coworker you are talking to finishes a turn.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "notifications.coworkers",
            "Notifications",
            "Coworker updates",
            "Notifies when a delegated coworker finishes or needs attention.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "notifications.sound",
            "Notifications",
            "Notification sound",
            "Plays a subtle sound when enabled events occur.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "notifications.sidebar_badges",
            "Notifications",
            "Sidebar badges",
            "Shows unread and attention badges on coworkers and groups.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "voice.enabled",
            "Voice",
            "Voice input",
            "Enables dictation and voice controls in the composer.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "voice.replies",
            "Voice",
            "Spoken replies",
            "Allows the current coworker to read replies aloud.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "security.vault_auto_lock_minutes",
            "Passes",
            "Auto-lock",
            "Lock Passes again after this many idle minutes. 0 (default) keeps Passes unlocked until Phoenix fully quits.",
            SettingControl::Number {
                min: 0.0,
                max: 1440.0,
                step: 5.0,
            },
            json!(0),
        ),
        setting(
            "security.reveal_requires_password",
            "Passes",
            "Ask for the password on every reveal",
            "Require the master password each time a secret is revealed, even while Passes is unlocked.",
            SettingControl::Toggle,
            json!(false),
        ),
        setting(
            "prompts.allow_overlays",
            "Prompts",
            "Prompt customization",
            "Allows user-owned prompt overlays in .phoenix/prompts.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "advanced.tool_receipts",
            "Advanced",
            "Detailed tool receipts",
            "Keeps expandable exact tool calls and bounded outputs in the activity journal.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "advanced.index_after_task",
            "Advanced",
            "Reindex after coding tasks",
            "Refreshes the codebase index once after a successful coding task.",
            SettingControl::Toggle,
            json!(true),
        ),
        setting(
            "efficiency.weekly_input_budget_millions",
            "Advanced",
            "7-day input planning budget",
            "Local provider-input budget in millions of tokens. This bounds Phoenix replay; it is not a vendor quota percentage because subscription units are dynamic and unpublished.",
            SettingControl::Number {
                min: 1.0,
                max: 200.0,
                step: 1.0,
            },
            json!(20),
        ),
        setting(
            "efficiency.weekly_warning_percent",
            "Advanced",
            "Usage warning",
            "Warns when Phoenix's local seven-day input estimate reaches this share of the planning budget.",
            SettingControl::Number {
                min: 10.0,
                max: 100.0,
                step: 5.0,
            },
            json!(70),
        ),
        setting(
            "efficiency.weekly_hard_stop",
            "Advanced",
            "Enforce 7-day hard stop",
            "Stops new provider rounds after the local planning budget is reached. Off by default because provider subscriptions do not publish a stable token-to-quota conversion.",
            SettingControl::Toggle,
            json!(false),
        ),
    ];

    for entry in &mut out {
        entry.keywords = entry.key.split(['.', '_']).map(str::to_string).collect();
    }
    for key in [
        "composer.default_permission",
        "composer.show_reasoning",
        "agents.outside_group_approval",
        "agents.read_group_transcript",
        "agents.volume_workers_enabled",
        "agents.volume_worker_limit",
        "permissions.login_import",
        "permissions.action_review",
        "permissions.account_creation",
        "permissions.external_send",
        "permissions.external_delete",
        "browser.enabled",
        "browser.suspend_after_seconds",
        "browser.auto_solve_captcha",
        "browser.account_creation",
        "browser.automatic_2fa_handoff",
        "memory.group_transcript_learning",
        "workflows.auto_run",
        "workflows.share_by_default",
        "workflows.failure_quarantine",
        "advanced.index_after_task",
        "notifications.enabled",
        "notifications.system",
        "notifications.sound",
        "notifications.attention",
        "notifications.completions",
        "notifications.coworkers",
    ] {
        if let Some(entry) = out.iter_mut().find(|entry| entry.key == key) {
            entry.agent_override = true;
            entry.group_override = true;
        }
    }
    for (key, dependency) in [
        (
            "agents.outside_group_approval",
            dep(
                "agents.collaboration_enabled",
                json!(true),
                "Turn on agent collaboration first.",
            ),
        ),
        (
            "agents.read_group_transcript",
            dep(
                "agents.collaboration_enabled",
                json!(true),
                "Turn on agent collaboration first.",
            ),
        ),
        (
            "agents.volume_worker_limit",
            dep(
                "agents.volume_workers_enabled",
                json!(true),
                "Turn on volume workers first.",
            ),
        ),
        (
            "browser.suspend_after_seconds",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.profile_source",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.cookie_import_source",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.headless",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.auto_solve_captcha",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.account_creation",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "browser.automatic_2fa_handoff",
            dep(
                "browser.enabled",
                json!(true),
                "Turn on browser tools first.",
            ),
        ),
        (
            "memory.group_transcript_learning",
            dep(
                "memory.enabled",
                json!(true),
                "Turn on long-term memory first.",
            ),
        ),
        (
            "workflows.auto_run",
            dep(
                "workflows.enabled",
                json!(true),
                "Turn on saved workflows first.",
            ),
        ),
        (
            "workflows.share_by_default",
            dep(
                "workflows.enabled",
                json!(true),
                "Turn on saved workflows first.",
            ),
        ),
        (
            "workflows.failure_quarantine",
            dep(
                "workflows.enabled",
                json!(true),
                "Turn on saved workflows first.",
            ),
        ),
        (
            "notifications.system",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "notifications.sound",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "notifications.attention",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "notifications.completions",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "notifications.coworkers",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "notifications.sidebar_badges",
            dep(
                "notifications.enabled",
                json!(true),
                "Turn on notifications first.",
            ),
        ),
        (
            "voice.replies",
            dep("voice.enabled", json!(true), "Turn on voice first."),
        ),
    ] {
        if let Some(entry) = out.iter_mut().find(|entry| entry.key == key) {
            entry.dependencies.push(dependency);
        }
    }
    for entry in &mut out {
        entry.advanced = entry.section == "Advanced";
    }
    out
}

fn guide() -> Vec<SettingsGuideEntry> {
    vec![
        SettingsGuideEntry { title: "Choose what agents may do".into(), description: "Use Permissions for sends, logins, accounts, and purchases. Browser & Accounts controls how those approved actions run.".into(), section: "Permissions".into(), search_terms: vec!["approval".into(), "login".into(), "purchase".into()] },
        SettingsGuideEntry { title: "Assign models and fallbacks".into(), description: "Models & Providers owns company and per-agent model choices, provider accounts, reasoning, and fallback order.".into(), section: "Models & Providers".into(), search_terms: vec!["model".into(), "provider".into(), "fallback".into()] },
        SettingsGuideEntry { title: "Manage learned work".into(), description: "Workflows & Routines contains taught browser routines, sharing, trust, and failure quarantine. Schedules contains recurring work.".into(), section: "Workflows & Routines".into(), search_terms: vec!["routine".into(), "teach".into(), "schedule".into()] },
        SettingsGuideEntry { title: "Protect accounts".into(), description: "Passes holds your logins, cards, and keys, encrypted on this device. Secrets never appear in settings data.".into(), section: "Passes".into(), search_terms: vec!["vault".into(), "credential".into(), "password".into()] },
        SettingsGuideEntry { title: "Tune each coworker".into(), description: "Open an agent profile for scoped overrides. Unset values inherit the company setting automatically.".into(), section: "Agents & Groups".into(), search_terms: vec!["agent".into(), "coworker".into(), "override".into()] },
    ]
}

fn destinations() -> Vec<SettingsDestination> {
    let destination = |id: &str,
                       label: &str,
                       description: &str,
                       section: &str,
                       route: &str,
                       keywords: &[&str],
                       advanced: bool| SettingsDestination {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        section: section.into(),
        route: route.into(),
        keywords: keywords.iter().map(|value| (*value).into()).collect(),
        advanced,
    };
    vec![
        destination("agents", "Coworkers", "Create, configure, pin, archive, restore, and permanently delete coworkers.", "Agents & Groups", "settings/agents", &["agent", "coworker", "archive", "trash", "delete"], false),
        destination("groups", "Groups", "Create groups, choose members, and configure group collaboration.", "Agents & Groups", "settings/groups", &["team", "members", "transcript", "collaboration"], false),
        destination("models", "Models", "Choose models, reasoning, context limits, and per-coworker lanes.", "Models & Providers", "settings/models", &["llm", "reasoning", "context", "agent model"], false),
        destination("providers", "Providers & fallbacks", "Manage provider accounts, fallback order, and compaction policy without exposing credentials.", "Models & Providers", "settings/providers", &["api", "account", "fallback", "custom model", "local model"], false),
        destination("credentials", "Passes", "Logins, cards, and keys — encrypted on this device. Add, edit, copy, and reveal after one unlock.", "Passes", "settings/credentials", &["password", "vault", "login", "secret", "recovery key", "card", "credit card", "api key", "token", "passes"], false),
        destination("browser_profiles", "Browser profiles", "Manage the company account email, private coworker browsers, cookies, imports, website sessions, and saved accounts.", "Browser & Accounts", "settings/browser-profiles", &["email", "account email", "chrome", "cookie", "login", "site", "profile"], false),
        destination("prompts", "Prompts", "Inspect and edit Phoenix and coworker prompt overlays separately from generated core prompts.", "Prompts", "settings/prompts", &["system prompt", "instructions", "guidelines", "persona"], false),
        destination("routines", "Routines & taught workflows", "Review, share, correct, reroute, archive, and retire learned work.", "Workflows & Routines", "settings/routines", &["teach agent", "workflow", "skill", "recording", "failure"], false),
        destination("schedules", "Schedules", "Create and manage recurring work, notifications, pause state, and owners.", "Schedules", "settings/schedules", &["cron", "recurring", "daily", "automation"], false),
        destination("notifications", "Notifications", "Configure desktop alerts, sounds, badges, and completion or attention events.", "Notifications", "settings/notifications", &["linux", "system", "sound", "badge", "alert"], false),
        destination("memory", "Memory", "Inspect company and coworker memory health, indexing, retention, and sharing boundaries.", "Memory", "settings/memory", &["librarian", "indexer", "recall", "knowledge", "thread"], false),
        destination("appearance", "Appearance", "Choose light or dark mode, typography, density, motion, background, and sidebar size.", "Appearance", "settings/appearance", &["theme", "white", "dark", "font", "fire", "animation"], false),
        destination("voice", "Voice", "Choose voice input, spoken replies, devices, and voice behavior.", "Voice", "settings/voice", &["microphone", "speech", "audio"], false),
        destination("data", "Data & storage", "Inspect local Phoenix data, artifact storage, snapshots, exports, archives, and retention.", "General", "settings/data", &[".phoenix", "backup", "export", "artifact", "30 days"], false),
        destination("skills", "Skills", "Installed playbooks from skills.sh — the crafts Phoenix follows instead of improvising.", "Skills", "settings/skills", &["skills.sh", "playbook", "skill", "install"], false),
        destination("composio", "Composio", "Consumer key and connected apps Phoenix can act through.", "Composio", "settings/composio", &["gmail", "github", "notion", "slack", "composio"], false),
        destination("mcp", "MCP", "Local and remote MCP servers the company can call.", "MCP", "settings/mcp", &["mcp", "stdio", "server", "t3mp3st"], false),
        destination("integrations", "Integrations", "Manage Composio and local MCP connections, permissions, and routing.", "Advanced", "settings/integrations", &["linear", "plugin"], true),
        destination("relationships", "Coworker relationships", "Edit durable trust and collaboration patterns between visible coworkers.", "Advanced", "settings/relationships", &["trust", "bonds", "collaboration", "handoff", "coworker relationship"], true),
        destination("diagnostics", "Diagnostics", "Inspect runtime health, provider status, browser lifecycle, indexes, and logs.", "Advanced", "settings/diagnostics", &["logs", "health", "debug", "performance", "failure"], true),
    ]
}

fn settings_path() -> PathBuf {
    crate::config::phoenix_home().join("settings.json")
}

fn load_document_at(path: &Path) -> Result<SettingsDocument> {
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(path, SETTINGS_MAX_BYTES)?
    else {
        return Ok(SettingsDocument::default());
    };
    let document: SettingsDocument = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "settings file {} is malformed; it was preserved",
            path.display()
        )
    })?;
    if document.version != SETTINGS_VERSION {
        anyhow::bail!("unsupported settings version {}", document.version)
    }
    if document.agents.len() > MAX_SCOPE_ENTRIES || document.groups.len() > MAX_SCOPE_ENTRIES {
        anyhow::bail!("settings file contains too many scoped entries")
    }
    Ok(document)
}

fn write_document_at(path: &Path, document: &SettingsDocument) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(document)?;
    if bytes.len() > SETTINGS_MAX_BYTES {
        anyhow::bail!(
            "settings file exceeds the {} byte limit",
            SETTINGS_MAX_BYTES
        )
    }
    crate::config::private_io::atomic_write_private_under_lock(path, &bytes)?;
    invalidate_runtime_cache();
    Ok(())
}

fn load_runtime_document(path: &Path) -> Result<SettingsDocument> {
    let metadata = std::fs::metadata(path).ok();
    let modified = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok());
    let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
    {
        let cache = runtime_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cache) = cache
            .as_ref()
            .filter(|cache| cache.path == path && cache.modified == modified && cache.len == len)
        {
            return Ok(cache.document.clone());
        }
    }
    let document =
        crate::config::private_io::with_private_lock(path, || load_with_migration(path))?;
    let metadata = std::fs::metadata(path).ok();
    let cache = RuntimeSettingsCache {
        path: path.to_path_buf(),
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        len: metadata.as_ref().map_or(0, std::fs::Metadata::len),
        document: document.clone(),
    };
    *runtime_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cache);
    Ok(document)
}

fn migrate_canvas_preferences(home: &Path, document: &mut SettingsDocument) -> Result<bool> {
    if document.migrated_canvas_preferences {
        return Ok(false);
    }
    let legacy = home.join("canvas-prefs.json");
    if let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&legacy, 8 * 1024 * 1024)?
    {
        let value: Value = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "legacy preferences {} are malformed; they were preserved and not imported",
                legacy.display()
            )
        })?;
        let source = value
            .as_object()
            .context("legacy preferences are not a JSON object")?;
        let mappings = [
            ("motion", "appearance.motion"),
            ("uiFont", "appearance.font"),
            ("uiScale", "appearance.interface_scale"),
            ("reduceTransparency", "appearance.reduce_transparency"),
            ("grain", "appearance.grain"),
            ("sidebarWidth", "sidebar.width"),
            ("showThinking", "composer.show_reasoning"),
        ];
        for (old, new) in mappings {
            if let Some(value) = source.get(old) {
                document
                    .global
                    .entry(new.into())
                    .or_insert_with(|| value.clone());
            }
        }
        if let Some(theme) = source.get("theme").and_then(Value::as_str) {
            let theme = match theme {
                "paper" => "light",
                "ember" => "dark",
                other => other,
            };
            if matches!(theme, "light" | "dark" | "system") {
                document
                    .global
                    .entry("appearance.theme".into())
                    .or_insert_with(|| json!(theme));
            }
        }
    }
    document.migrated_canvas_preferences = true;
    Ok(true)
}

fn load_with_migration(path: &Path) -> Result<SettingsDocument> {
    let mut document = load_document_at(path)?;
    let home = path.parent().context("settings path has no parent")?;
    if migrate_canvas_preferences(home, &mut document)? {
        document.updated_at = Utc::now().to_rfc3339();
        write_document_at(path, &document)?;
    }
    Ok(document)
}

fn definition<'a>(catalog: &'a [SettingDefinition], key: &str) -> Result<&'a SettingDefinition> {
    catalog
        .iter()
        .find(|entry| entry.key == key)
        .with_context(|| format!("unknown setting `{key}`"))
}

fn validate_value(definition: &SettingDefinition, value: &Value) -> Result<()> {
    match &definition.control {
        SettingControl::Toggle if !value.is_boolean() => {
            anyhow::bail!("{} expects a boolean", definition.key)
        }
        SettingControl::Select { options } => {
            let selected = value
                .as_str()
                .with_context(|| format!("{} expects a string option", definition.key))?;
            if !options.iter().any(|option| option.value == selected) {
                anyhow::bail!("invalid option for {}", definition.key)
            }
        }
        SettingControl::Number { min, max, step } => {
            let number = value
                .as_f64()
                .with_context(|| format!("{} expects a number", definition.key))?;
            if !number.is_finite() || number < *min || number > *max {
                anyhow::bail!("{} must be between {min} and {max}", definition.key)
            }
            let units = (number - min) / step;
            if (units - units.round()).abs() > 1e-7 {
                anyhow::bail!("{} must use increments of {step}", definition.key)
            }
        }
        SettingControl::Text { max_length } => {
            let text = value
                .as_str()
                .with_context(|| format!("{} expects text", definition.key))?;
            if text.len() > *max_length || text.chars().any(char::is_control) {
                anyhow::bail!("{} contains invalid or excessive text", definition.key)
            }
        }
        SettingControl::Color => {
            let color = value
                .as_str()
                .with_context(|| format!("{} expects a color", definition.key))?;
            if color.len() != 7
                || !color.starts_with('#')
                || !color[1..].chars().all(|ch| ch.is_ascii_hexdigit())
            {
                anyhow::bail!("{} expects a six-digit hex color", definition.key)
            }
        }
        _ => {}
    }
    Ok(())
}

fn scoped_map<'a>(
    document: &'a SettingsDocument,
    scope: &SettingsScope,
) -> Option<&'a BTreeMap<String, Value>> {
    match scope {
        SettingsScope::Global => Some(&document.global),
        SettingsScope::Agent { id } => document.agents.get(id),
        SettingsScope::Group { id } => document.groups.get(id),
    }
}

fn scoped_map_mut<'a>(
    document: &'a mut SettingsDocument,
    scope: &SettingsScope,
) -> &'a mut BTreeMap<String, Value> {
    match scope {
        SettingsScope::Global => &mut document.global,
        SettingsScope::Agent { id } => document.agents.entry(id.clone()).or_default(),
        SettingsScope::Group { id } => document.groups.entry(id.clone()).or_default(),
    }
}

fn value_for(
    document: &SettingsDocument,
    definition: &SettingDefinition,
    scope: &SettingsScope,
) -> (Value, String) {
    if !matches!(scope, SettingsScope::Global) {
        if let Some(value) =
            scoped_map(document, scope).and_then(|values| values.get(&definition.key))
        {
            return (value.clone(), scope.label());
        }
    }
    document
        .global
        .get(&definition.key)
        .cloned()
        .map(|value| (value, "global".into()))
        .unwrap_or_else(|| (definition.default.clone(), "default".into()))
}

fn resolve(
    document: &SettingsDocument,
    catalog: &[SettingDefinition],
    definition: &SettingDefinition,
    scope: &SettingsScope,
) -> ResolvedSetting {
    let (value, inherited_from) = value_for(document, definition, scope);
    let disabled_reason = match scope {
        SettingsScope::Agent { .. } if !definition.agent_override => {
            Some("This setting is controlled by Company defaults.".into())
        }
        SettingsScope::Group { .. } if !definition.group_override => {
            Some("This setting is controlled by Company defaults.".into())
        }
        _ => definition.dependencies.iter().find_map(|dependency| {
            let dependency_definition = catalog.iter().find(|entry| entry.key == dependency.key)?;
            let (actual, _) = value_for(document, dependency_definition, scope);
            (actual != dependency.equals).then(|| dependency.reason.clone())
        }),
    };
    ResolvedSetting {
        definition: definition.clone(),
        value,
        inherited_from,
        enabled: disabled_reason.is_none(),
        disabled_reason,
    }
}

fn snapshot(document: &SettingsDocument, scope: SettingsScope) -> SettingsSnapshot {
    let catalog = catalog();
    let sections = catalog
        .iter()
        .map(|entry| entry.section.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .chain(
            destinations()
                .into_iter()
                .map(|destination| destination.section),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let settings = catalog
        .iter()
        .map(|entry| resolve(document, &catalog, entry, &scope))
        .collect();
    SettingsSnapshot {
        revision: document.revision,
        scope,
        settings,
        guide: guide(),
        sections,
        destinations: destinations(),
    }
}

fn search(
    document: &SettingsDocument,
    query: &str,
    section: Option<&str>,
    scope: SettingsScope,
) -> SettingsSearchResults {
    let catalog = catalog();
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect();
    let mut regular = Vec::new();
    let mut advanced = Vec::new();
    for entry in &catalog {
        if section.is_some_and(|section| !entry.section.eq_ignore_ascii_case(section)) {
            continue;
        }
        let haystack = format!(
            "{} {} {} {}",
            entry.key, entry.section, entry.label, entry.description
        )
        .to_ascii_lowercase()
            + " "
            + &entry.keywords.join(" ").to_ascii_lowercase();
        if !terms.iter().all(|term| haystack.contains(term)) {
            continue;
        }
        let resolved = resolve(document, &catalog, entry, &scope);
        if entry.advanced {
            advanced.push(resolved);
        } else {
            regular.push(resolved);
        }
    }
    let mut regular_destinations = Vec::new();
    let mut advanced_destinations = Vec::new();
    for destination in destinations() {
        if section.is_some_and(|section| !destination.section.eq_ignore_ascii_case(section)) {
            continue;
        }
        let haystack = format!(
            "{} {} {} {} {}",
            destination.id,
            destination.label,
            destination.description,
            destination.section,
            destination.keywords.join(" ")
        )
        .to_ascii_lowercase();
        if !terms.iter().all(|term| haystack.contains(term)) {
            continue;
        }
        if destination.advanced {
            advanced_destinations.push(destination);
        } else {
            regular_destinations.push(destination);
        }
    }
    SettingsSearchResults {
        revision: document.revision,
        scope,
        regular,
        advanced,
        regular_destinations,
        advanced_destinations,
    }
}

fn mutate_at(path: &Path, command: SettingsCommand) -> Result<SettingsReply> {
    crate::config::private_io::with_private_lock(path, || {
        let mut document = load_with_migration(path)?;
        let catalog = catalog();
        let (key, scope, expected_revision, value) = match command {
            SettingsCommand::Set {
                key,
                value,
                scope,
                expected_revision,
            } => (key, scope, expected_revision, Some(value)),
            SettingsCommand::Reset {
                key,
                scope,
                expected_revision,
            } => (key, scope, expected_revision, None),
            _ => anyhow::bail!("not a settings mutation"),
        };
        scope.validate()?;
        if expected_revision.is_some_and(|expected| expected != document.revision) {
            anyhow::bail!(
                "settings changed since revision {}; reload before saving",
                expected_revision.unwrap()
            )
        }
        let definition = definition(&catalog, &key)?;
        match &scope {
            SettingsScope::Agent { .. } if !definition.agent_override => {
                anyhow::bail!("{} is company-wide and cannot be overridden per agent", key)
            }
            SettingsScope::Group { .. } if !definition.group_override => {
                anyhow::bail!("{} is company-wide and cannot be overridden per group", key)
            }
            _ => {}
        }
        if let Some(value) = value.as_ref() {
            validate_value(definition, value)?;
            if scoped_map(&document, &scope).map_or(false, |values| {
                values.len() >= MAX_OVERRIDES_PER_SCOPE && !values.contains_key(&key)
            }) {
                anyhow::bail!("settings scope contains too many overrides")
            }
        }
        let values = scoped_map_mut(&mut document, &scope);
        match value {
            Some(value) => {
                values.insert(key, value);
            }
            None => {
                values.remove(&key);
            }
        }
        document.agents.retain(|_, values| !values.is_empty());
        document.groups.retain(|_, values| !values.is_empty());
        document.revision = document.revision.saturating_add(1);
        document.updated_at = Utc::now().to_rfc3339();
        write_document_at(path, &document)?;
        Ok(SettingsReply::Snapshot {
            snapshot: snapshot(&document, scope),
        })
    })
}

fn config_path() -> PathBuf {
    crate::config::phoenix_home().join("config.toml")
}

fn content_revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_identifier(value: &str, label: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(|ch| ch.is_control()) {
        anyhow::bail!("invalid {label}")
    }
    Ok(())
}

fn validate_effort(effort: Option<&str>) -> Result<()> {
    if let Some(effort) = effort {
        if !matches!(
            effort,
            "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            anyhow::bail!("invalid reasoning effort")
        }
    }
    Ok(())
}

fn auth_kind(credential: &crate::config::auth_profile::AuthProfileCredential) -> &'static str {
    use crate::config::auth_profile::AuthProfileCredential;
    match credential {
        AuthProfileCredential::ApiKey { .. } => "api_key",
        AuthProfileCredential::Token { .. } => "token",
        AuthProfileCredential::OAuth { .. } => "oauth",
    }
}

fn account_display(
    credential: &crate::config::auth_profile::AuthProfileCredential,
) -> (Option<String>, Option<i64>) {
    use crate::config::auth_profile::AuthProfileCredential;
    match credential {
        AuthProfileCredential::ApiKey { display_name, .. } => (display_name.clone(), None),
        AuthProfileCredential::Token { expires, .. } => (None, *expires),
        AuthProfileCredential::OAuth { email, expires, .. } => (email.clone(), Some(*expires)),
    }
}

fn lane_provider_and_model(
    llm: &crate::config::LLMProfile,
    accounts: &crate::config::auth_profile::AuthProfileStore,
    lane: &str,
) -> (String, String) {
    let pinned_provider = llm
        .auth_by_lane
        .get(lane)
        .and_then(|id| accounts.profiles.get(id))
        .map(crate::config::auth_profile::profile_provider_id)
        .map(str::to_string);
    match lane {
        "phoenix" | "orchestrator" => (llm.provider.clone(), llm.orchestrator()),
        "specialist" => (
            llm.specialist_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.specialist(),
        ),
        "librarian" => (
            llm.librarian_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.librarian(),
        ),
        "vision" => (
            llm.vision_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.vision_model
                .clone()
                .unwrap_or_else(|| llm.model.clone()),
        ),
        "image" => (
            llm.image_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.image_model.clone().unwrap_or_else(|| llm.model.clone()),
        ),
        "memory" => (
            llm.memory_provider
                .clone()
                .or_else(|| llm.librarian_provider.clone())
                .unwrap_or_else(|| llm.provider.clone()),
            llm.memory(),
        ),
        "stt" => (
            llm.stt_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.stt_model.clone().unwrap_or_else(|| llm.model.clone()),
        ),
        "tts" => (
            llm.tts_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.tts_model.clone().unwrap_or_else(|| llm.model.clone()),
        ),
        "realtime" => (
            llm.realtime_provider
                .clone()
                .unwrap_or_else(|| llm.provider.clone()),
            llm.realtime_model
                .clone()
                .unwrap_or_else(|| llm.model.clone()),
        ),
        agent => (
            pinned_provider.unwrap_or_else(|| {
                llm.specialist_provider
                    .clone()
                    .unwrap_or_else(|| llm.provider.clone())
            }),
            llm.agent_model(agent),
        ),
    }
}

fn models_snapshot() -> Result<ModelSettingsSnapshot> {
    let path = config_path();
    crate::config::private_io::with_private_lock(&path, || {
        let raw = crate::config::private_io::read_private_file_limited(&path, SETTINGS_MAX_BYTES)?
            .context("Phoenix is not configured yet")?;
        let config = crate::config::PhoenixConfig::load_from_path(path.clone())?;
        let llm = &config.profile.llm;
        let accounts = crate::config::auth_profile::load_auth_profile_store()?;
        // The routing catalog is an assignment surface, not a provider-store
        // advertisement. A model is selectable only when at least one stored
        // account can actually authenticate its provider. The full provider
        // library still comes from `providers_catalog` for the Connect flow.
        let configured_provider_ids: BTreeSet<String> = accounts
            .profiles
            .values()
            .map(|credential| {
                crate::config::auth_profile::profile_provider_id(credential).to_string()
            })
            .collect();

        let providers = crate::providers::providers_data::all_providers()
            .into_iter()
            .filter(|provider| configured_provider_ids.contains(provider.id))
            .map(|provider| ProviderCatalogEntry {
                id: provider.id.to_string(),
                name: provider.name.to_string(),
                auth_kind: format!("{:?}", provider.auth_type).to_ascii_lowercase(),
                models: provider
                    .models
                    .into_iter()
                    .map(|model| ModelCatalogEntry {
                        id: model.id.to_string(),
                        name: model.name.to_string(),
                        context_window: model.context_window,
                        reasoning: model.reasoning,
                        effort_levels: crate::providers::providers_data::effort_levels(
                            provider.id,
                            model.id,
                        )
                        .iter()
                        .map(|level| (*level).to_string())
                        .collect(),
                    })
                    .collect(),
            })
            .collect();

        let now = Utc::now().timestamp_millis();
        let mut account_summaries: Vec<_> = accounts
            .profiles
            .iter()
            .map(|(profile_id, credential)| {
                let (display_name, expires_at) = account_display(credential);
                ProviderAccountSummary {
                    profile_id: profile_id.clone(),
                    provider_id: crate::config::auth_profile::profile_provider_id(credential)
                        .to_string(),
                    auth_kind: auth_kind(credential).into(),
                    display_name: accounts.labels.get(profile_id).cloned().or(display_name),
                    expires_at,
                    cooling_down_until: accounts
                        .state
                        .cooldown_until
                        .get(profile_id)
                        .copied()
                        .filter(|until| *until > now),
                }
            })
            .collect();
        account_summaries.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));

        let mut lane_names: BTreeSet<String> = [
            "phoenix",
            "specialist",
            "volume_worker",
            "librarian",
            "vision",
            "image",
            "memory",
            "stt",
            "tts",
            "realtime",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        lane_names.extend(llm.agent_models.keys().cloned());
        lane_names.extend(llm.fallback.agents.keys().cloned());
        // An agent with only a custom context window or reasoning effort still
        // needs its lane, or the composer falls back to the model maximum.
        lane_names.extend(llm.context_windows.keys().chain(llm.efforts.keys()).filter(|lane| lane.as_str() != "orchestrator").cloned());
        lane_names.extend(
            llm.auth_by_lane
                .keys()
                .filter(|lane| {
                    !matches!(
                        lane.as_str(),
                        "orchestrator" | "specialist" | "librarian" | "vision" | "image"
                    )
                })
                .cloned(),
        );
        let lanes = lane_names
            .into_iter()
            .map(|lane| {
                let (provider_id, model) = lane_provider_and_model(llm, &accounts, &lane);
                let is_runtime_lane = matches!(
                    lane.as_str(),
                    "phoenix"
                        | "specialist"
                        | "volume_worker"
                        | "librarian"
                        | "vision"
                        | "image"
                        | "memory"
                        | "stt"
                        | "tts"
                        | "realtime"
                );
                let inherited = !is_runtime_lane
                    && !llm.agent_models.contains_key(&lane)
                    && !llm.auth_by_lane.contains_key(&lane)
                    && !llm.efforts.contains_key(&lane);
                let configured = match lane.as_str() {
                    "vision" => llm
                        .vision_model
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty()),
                    "image" => llm
                        .image_model
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty()),
                    "stt" => llm
                        .stt_model
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty()),
                    "tts" => llm
                        .tts_model
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty()),
                    "realtime" => llm
                        .realtime_model
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty()),
                    _ => true,
                };
                let context_lane = if lane == "phoenix" {
                    "orchestrator"
                } else {
                    lane.as_str()
                };
                let max_context_window =
                    crate::providers::providers_data::context_window_for(&provider_id, &model);
                let requested_context_window =
                    llm.context_windows.get(context_lane).copied().or_else(|| {
                        (context_lane == "orchestrator")
                            .then_some(llm.context_window)
                            .flatten()
                    });
                let context_window = match (requested_context_window, max_context_window) {
                    (Some(requested), Some(maximum)) => Some(requested.min(maximum)),
                    (Some(requested), None) => Some(requested),
                    (None, maximum) => maximum,
                };
                let context_window_override = match (context_window, max_context_window) {
                    (Some(effective), Some(maximum)) if effective < maximum => Some(effective),
                    (Some(effective), None) => Some(effective),
                    _ => None,
                };
                ModelLaneSetting {
                    inherited,
                    reasoning_effort: if lane == "phoenix" {
                        llm.effort_for("orchestrator")
                    } else if matches!(
                        lane.as_str(),
                        "specialist"
                            | "librarian"
                            | "vision"
                            | "image"
                            | "memory"
                            | "stt"
                            | "tts"
                            | "realtime"
                    ) {
                        llm.effort_for(&lane)
                    } else {
                        llm.agent_effort(&lane)
                    },
                    auth_profile_id: llm
                        .auth_by_lane
                        .get(if lane == "phoenix" {
                            "orchestrator"
                        } else {
                            &lane
                        })
                        .cloned(),
                    context_window,
                    max_context_window,
                    context_window_override,
                    lane,
                    provider_id,
                    model,
                    configured,
                }
            })
            .collect();

        let mut fallback_chains = BTreeMap::new();
        for (lane, chain) in llm.fallback.role_lanes() {
            fallback_chains.insert(lane.to_string(), chain.clone());
        }
        fallback_chains.extend(llm.fallback.agents.clone());
        let compaction_modes = llm
            .compaction_modes
            .iter()
            .map(|(provider, mode)| (provider.clone(), mode.as_str().to_string()))
            .collect();
        Ok(ModelSettingsSnapshot {
            config_revision: content_revision(&raw),
            lanes,
            providers,
            accounts: account_summaries,
            fallback_chains,
            compaction_modes,
            codex_efficiency: CodexEfficiencyStatus {
                available: crate::config::codex_subscription_defaults_applicable(llm),
                active: crate::config::codex_subscription_defaults_active(llm),
                phoenix_model: crate::config::CODEX_PHOENIX_MODEL.to_string(),
                team_model: crate::config::CODEX_TEAM_MODEL.to_string(),
                team_effort: crate::config::CODEX_TEAM_EFFORT.to_string(),
            },
        })
    })
}

fn llm_table_mut(document: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::Table> {
    document
        .get_mut("profile")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|profile| profile.get_mut("llm"))
        .and_then(toml_edit::Item::as_table_mut)
        .context("config.toml has no [profile.llm] table")
}

fn nested_table_mut<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table> {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    // Older Phoenix configs intentionally used compact inline maps for lanes,
    // efforts, and agent models. The model settings UI must be able to update
    // those real files, not only freshly generated expanded tables. Normalize
    // the one map being edited while preserving every existing entry.
    if let Some(inline) = table
        .get(key)
        .and_then(toml_edit::Item::as_inline_table)
        .cloned()
    {
        let mut expanded = toml_edit::Table::new();
        for (name, value) in inline.iter() {
            expanded.insert(name, toml_edit::Item::Value(value.clone()));
        }
        table.insert(key, toml_edit::Item::Table(expanded));
    }
    table
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .with_context(|| format!("profile.llm.{key} is not a table"))
}

fn update_config(
    expected_revision: Option<&str>,
    update: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
) -> Result<()> {
    let path = config_path();
    crate::config::private_io::read_modify_write_private(&path, |current| {
        let current = current.context("Phoenix is not configured yet")?;
        if expected_revision.is_some_and(|expected| expected != content_revision(current)) {
            anyhow::bail!("model settings changed in another process; reload before saving")
        }
        let raw = std::str::from_utf8(current).context("config.toml is not UTF-8")?;
        let mut document = raw
            .parse::<toml_edit::DocumentMut>()
            .context("config.toml is malformed; it was preserved")?;
        update(&mut document)?;
        let replacement = document.to_string();
        let _: toml::Value =
            toml::from_str(&replacement).context("model settings would create invalid TOML")?;
        Ok(((), replacement.into_bytes()))
    })
}

fn set_model_lane(
    lane: &str,
    provider_id: &str,
    model: &str,
    effort: Option<&str>,
    auth_profile_id: Option<&str>,
    expected_revision: Option<&str>,
) -> Result<()> {
    validate_identifier(lane, "model lane", 96)?;
    validate_identifier(provider_id, "provider id", 128)?;
    validate_identifier(model, "model id", 256)?;
    validate_effort(effort)?;
    let providers = crate::providers::providers_data::all_providers();
    providers
        .iter()
        .find(|candidate| candidate.id == provider_id)
        .with_context(|| format!("unknown model provider `{provider_id}`"))?;
    if let Some(effort) = effort {
        anyhow::ensure!(
            crate::providers::providers_data::effort_levels(provider_id, model).contains(&effort),
            "reasoning effort `{effort}` is not supported by `{provider_id}/{model}`"
        );
    }
    let model_context_max =
        crate::providers::providers_data::context_window_for(provider_id, model);
    let accounts = crate::config::auth_profile::load_auth_profile_store()?;
    if let Some(profile_id) = auth_profile_id {
        validate_identifier(profile_id, "auth profile id", 160)?;
        let credential = accounts
            .profiles
            .get(profile_id)
            .with_context(|| format!("unknown provider account `{profile_id}`"))?;
        anyhow::ensure!(
            crate::config::auth_profile::profile_provider_id(credential) == provider_id,
            "provider account `{profile_id}` does not belong to `{provider_id}`"
        );
    }
    let known_role = matches!(
        lane,
        "phoenix"
            | "orchestrator"
            | "specialist"
            | "librarian"
            | "vision"
            | "image"
            | "memory"
            | "stt"
            | "tts"
            | "realtime"
    );
    update_config(expected_revision, |document| {
        let llm = llm_table_mut(document)?;
        let pin_lane = if matches!(lane, "phoenix" | "orchestrator") {
            "orchestrator"
        } else {
            lane
        };
        if matches!(lane, "phoenix" | "orchestrator") {
            llm["provider"] = toml_edit::value(provider_id);
            llm["model"] = toml_edit::value(model);
            // `orchestrator_model` wins over `model` when present; a stale pin
            // there made the new choice show and run as the old model.
            if llm.contains_key("orchestrator_model") {
                llm["orchestrator_model"] = toml_edit::value(model);
            }
            if let Some(profile_id) = auth_profile_id {
                let auth = nested_table_mut(llm, "auth")?;
                auth["source"] = toml_edit::value("profile");
                auth["profile"] = toml_edit::value(profile_id);
                auth.remove("env_var");
            } else {
                llm.remove("auth");
            }
        } else if known_role {
            llm[&format!("{lane}_model")] = toml_edit::value(model);
            llm[&format!("{lane}_provider")] = toml_edit::value(provider_id);
        } else {
            nested_table_mut(llm, "agent_models")?[lane] = toml_edit::value(model);
            if auth_profile_id.is_none() {
                let shared_provider = llm
                    .get("specialist_provider")
                    .and_then(toml_edit::Item::as_str)
                    .or_else(|| llm.get("provider").and_then(toml_edit::Item::as_str));
                anyhow::ensure!(
                    shared_provider == Some(provider_id),
                    "a coworker using a different provider needs a provider account selection"
                );
            }
        }
        let pins = nested_table_mut(llm, "auth_by_lane")?;
        if let Some(profile_id) = auth_profile_id {
            pins[pin_lane] = toml_edit::value(profile_id);
        } else {
            pins.remove(pin_lane);
        }
        let efforts = nested_table_mut(llm, "efforts")?;
        if let Some(effort) = effort {
            efforts[pin_lane] = toml_edit::value(effort);
        } else {
            efforts.remove(pin_lane);
        }
        if let Some(maximum) = model_context_max {
            if pin_lane == "orchestrator"
                && llm
                    .get("context_window")
                    .and_then(toml_edit::Item::as_integer)
                    .is_some_and(|window| window > maximum as i64)
            {
                llm.remove("context_window");
            }
            if let Some(windows) = llm
                .get_mut("context_windows")
                .and_then(toml_edit::Item::as_table_mut)
            {
                if windows
                    .get(pin_lane)
                    .and_then(toml_edit::Item::as_integer)
                    .is_some_and(|window| window > maximum as i64)
                {
                    windows.remove(pin_lane);
                }
            }
        }
        Ok(())
    })
}

const MIN_MODEL_CONTEXT_WINDOW_TOKENS: u64 = 8_192;

fn set_model_context_window(
    lane: &str,
    requested: Option<u64>,
    expected_revision: Option<&str>,
) -> Result<()> {
    validate_identifier(lane, "model lane", 96)?;
    let config = crate::config::PhoenixConfig::load_from_path(config_path())?;
    let accounts = crate::config::auth_profile::load_auth_profile_store()?;
    let normalized_lane = if matches!(lane, "phoenix" | "orchestrator") {
        "orchestrator"
    } else {
        lane
    };
    let (provider_id, model) = lane_provider_and_model(&config.profile.llm, &accounts, lane);
    let maximum = crate::providers::providers_data::context_window_for(&provider_id, &model)
        .with_context(|| {
            format!("Phoenix does not know the official context window for `{provider_id}/{model}`")
        })?;
    if let Some(window) = requested {
        anyhow::ensure!(
            window >= MIN_MODEL_CONTEXT_WINDOW_TOKENS,
            "custom context must be at least {MIN_MODEL_CONTEXT_WINDOW_TOKENS} tokens"
        );
        anyhow::ensure!(
            window <= maximum,
            "custom context {window} exceeds `{provider_id}/{model}` maximum of {maximum} tokens"
        );
    }
    update_config(expected_revision, |document| {
        let llm = llm_table_mut(document)?;
        // The old scalar was company-wide. Once the orchestrator is edited in
        // the lane-aware UI, retire it so changing Phoenix cannot silently
        // constrain every coworker.
        if normalized_lane == "orchestrator" {
            llm.remove("context_window");
        }
        let windows = nested_table_mut(llm, "context_windows")?;
        match requested.filter(|window| *window < maximum) {
            Some(window) => windows[normalized_lane] = toml_edit::value(window as i64),
            None => {
                windows.remove(normalized_lane);
            }
        }
        Ok(())
    })
}

/// Apply the explicit subscription-efficient company defaults selected in
/// Settings. The optimistic revision is mandatory: this action changes
/// several shared lanes at once and must never race a user's individual edit.
/// Individual agent models, efforts, account pins, and fallback chains are not
/// touched.
fn apply_codex_efficiency_defaults(
    expected_revision: Option<&str>,
) -> Result<crate::config::CodexEfficiencyMigration> {
    let expected_revision = expected_revision.context(
        "model settings must be reloaded before applying subscription-efficient defaults",
    )?;
    let mut target = crate::config::PhoenixConfig::load_from_path(config_path())?
        .profile
        .llm;
    let migration = crate::config::apply_codex_subscription_defaults(&mut target).context(
        "Sol + Luna defaults require Codex-backed Phoenix, coworker, librarian, and memory lanes",
    )?;

    update_config(Some(expected_revision), |document| {
        let llm = llm_table_mut(document)?;
        llm["model"] = toml_edit::value(target.model.as_str());
        llm["orchestrator_model"] = toml_edit::value(target.orchestrator().as_str());
        llm["specialist_model"] = toml_edit::value(target.specialist().as_str());
        llm["librarian_model"] = toml_edit::value(target.librarian().as_str());
        llm["memory_model"] = toml_edit::value(target.memory().as_str());
        let efforts = nested_table_mut(llm, "efforts")?;
        for lane in ["orchestrator", "specialist", "librarian"] {
            if let Some(effort) = target.efforts.get(lane) {
                efforts[lane] = toml_edit::value(effort.as_str());
            }
        }
        Ok(())
    })?;
    Ok(migration)
}

fn reset_model_lane(lane: &str, expected_revision: Option<&str>) -> Result<()> {
    validate_identifier(lane, "model lane", 96)?;
    anyhow::ensure!(
        !matches!(
            lane,
            "phoenix"
                | "orchestrator"
                | "specialist"
                | "volume_worker"
                | "librarian"
                | "vision"
                | "image"
                | "memory"
                | "stt"
                | "tts"
                | "realtime"
        ),
        "only a coworker model override can inherit the company default"
    );
    update_config(expected_revision, |document| {
        let llm = llm_table_mut(document)?;
        nested_table_mut(llm, "agent_models")?.remove(lane);
        nested_table_mut(llm, "auth_by_lane")?.remove(lane);
        nested_table_mut(llm, "efforts")?.remove(lane);
        Ok(())
    })
}

fn set_fallback_chain(
    lane: &str,
    profile_ids: &[String],
    expected_revision: Option<&str>,
) -> Result<()> {
    validate_identifier(lane, "fallback lane", 96)?;
    anyhow::ensure!(
        profile_ids.len() <= 16,
        "fallback chains are limited to 16 accounts"
    );
    let accounts = crate::config::auth_profile::load_auth_profile_store()?;
    let mut unique = BTreeSet::new();
    for profile_id in profile_ids {
        validate_identifier(profile_id, "auth profile id", 160)?;
        anyhow::ensure!(
            accounts.profiles.contains_key(profile_id),
            "unknown provider account `{profile_id}`"
        );
        anyhow::ensure!(
            unique.insert(profile_id),
            "fallback chain contains a duplicate account"
        );
    }
    update_config(expected_revision, |document| {
        let llm = llm_table_mut(document)?;
        let fallback = nested_table_mut(llm, "fallback")?;
        let target = if matches!(
            lane,
            "orchestrator" | "specialist" | "librarian" | "vision" | "image"
        ) {
            fallback
        } else {
            nested_table_mut(fallback, "agents")?
        };
        if profile_ids.is_empty() {
            target.remove(lane);
        } else {
            let mut values = toml_edit::Array::new();
            for profile_id in profile_ids {
                values.push(profile_id.as_str());
            }
            target[lane] = toml_edit::value(values);
        }
        Ok(())
    })
}

fn set_compaction_mode(
    provider_id: &str,
    mode: &str,
    expected_revision: Option<&str>,
) -> Result<()> {
    validate_identifier(provider_id, "provider id", 128)?;
    anyhow::ensure!(
        matches!(mode, "phoenix_only" | "native_preferred"),
        "invalid compaction mode"
    );
    update_config(expected_revision, |document| {
        let llm = llm_table_mut(document)?;
        nested_table_mut(llm, "compaction_modes")?[provider_id] = toml_edit::value(mode);
        Ok(())
    })
}

/// Execute a typed Settings request. All disk work is synchronous by design;
/// the daemon dispatches it through a blocking worker.
pub fn execute(command: SettingsCommand) -> Result<SettingsReply> {
    let path = settings_path();
    match command {
        SettingsCommand::Snapshot { scope } => {
            scope.validate()?;
            let document =
                crate::config::private_io::with_private_lock(&path, || load_with_migration(&path))?;
            Ok(SettingsReply::Snapshot {
                snapshot: snapshot(&document, scope),
            })
        }
        SettingsCommand::Search {
            query,
            section,
            scope,
        } => {
            scope.validate()?;
            if query.len() > 256 || section.as_ref().is_some_and(|section| section.len() > 64) {
                anyhow::bail!("settings search is too long")
            }
            let document =
                crate::config::private_io::with_private_lock(&path, || load_with_migration(&path))?;
            Ok(SettingsReply::Search {
                results: search(&document, &query, section.as_deref(), scope),
            })
        }
        SettingsCommand::ModelsSnapshot => Ok(SettingsReply::Models {
            snapshot: models_snapshot()?,
        }),
        SettingsCommand::ApplyCodexEfficiencyDefaults {
            expected_config_revision,
        } => {
            apply_codex_efficiency_defaults(expected_config_revision.as_deref())?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        SettingsCommand::SetModelLane {
            lane,
            provider_id,
            model,
            reasoning_effort,
            auth_profile_id,
            expected_config_revision,
        } => {
            set_model_lane(
                &lane,
                &provider_id,
                &model,
                reasoning_effort.as_deref(),
                auth_profile_id.as_deref(),
                expected_config_revision.as_deref(),
            )?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        SettingsCommand::SetModelContextWindow {
            lane,
            context_window,
            expected_config_revision,
        } => {
            set_model_context_window(&lane, context_window, expected_config_revision.as_deref())?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        SettingsCommand::ResetModelLane {
            lane,
            expected_config_revision,
        } => {
            reset_model_lane(&lane, expected_config_revision.as_deref())?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        SettingsCommand::SetFallbackChain {
            lane,
            profile_ids,
            expected_config_revision,
        } => {
            set_fallback_chain(&lane, &profile_ids, expected_config_revision.as_deref())?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        SettingsCommand::SetCompactionMode {
            provider_id,
            mode,
            expected_config_revision,
        } => {
            set_compaction_mode(&provider_id, &mode, expected_config_revision.as_deref())?;
            Ok(SettingsReply::Models {
                snapshot: models_snapshot()?,
            })
        }
        mutation @ (SettingsCommand::Set { .. } | SettingsCommand::Reset { .. }) => {
            mutate_at(&path, mutation)
        }
    }
}

/// Read one effective value for runtime consumers. Invalid/corrupt settings
/// fail closed to the catalog default and never crash a conversation turn.
pub fn effective_value(key: &str, scope: &SettingsScope) -> Option<Value> {
    let catalog = catalog();
    let definition = catalog.iter().find(|entry| entry.key == key)?;
    if crate::config::test_isolated_from_live_home() {
        return Some(definition.default.clone());
    }
    let document = load_runtime_document(&settings_path()).ok()?;
    Some(value_for(&document, definition, scope).0)
}

/// Read only a value the user or migration explicitly stored. This is used
/// when the older config remains the compatibility fallback: adding a new
/// Settings row must not silently overwrite a working browser configuration
/// merely because the catalog has a default.
pub fn explicit_value(key: &str, scope: &SettingsScope) -> Option<Value> {
    if crate::config::test_isolated_from_live_home() {
        return None;
    }
    let document = load_runtime_document(&settings_path()).ok()?;
    if !matches!(scope, SettingsScope::Global) {
        if let Some(value) = scoped_map(&document, scope).and_then(|values| values.get(key)) {
            return Some(value.clone());
        }
    }
    document.global.get(key).cloned()
}

pub fn explicit_string(key: &str, scope: &SettingsScope) -> Option<String> {
    explicit_value(key, scope)?.as_str().map(str::to_string)
}

pub fn explicit_bool(key: &str, scope: &SettingsScope) -> Option<bool> {
    explicit_value(key, scope)?.as_bool()
}

pub fn explicit_u64(key: &str, scope: &SettingsScope) -> Option<u64> {
    explicit_value(key, scope)?.as_u64()
}

pub fn effective_bool(key: &str, scope: &SettingsScope) -> Option<bool> {
    effective_value(key, scope)?.as_bool()
}

pub fn effective_u64(key: &str, scope: &SettingsScope) -> Option<u64> {
    effective_value(key, scope)?.as_u64()
}

pub fn effective_string(key: &str, scope: &SettingsScope) -> Option<String> {
    effective_value(key, scope)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_temp_home() -> (tempfile::TempDir, crate::config::test_env::PhoenixHomeGuard) {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        (dir, guard)
    }

    #[test]
    fn volume_workers_are_enabled_by_default_with_isolated_runtime_contexts() {
        let definition = catalog()
            .into_iter()
            .find(|entry| entry.key == "agents.volume_workers_enabled")
            .expect("volume-worker setting");
        assert_eq!(definition.default, json!(true));
        assert!(definition.description.contains("isolated terminal"));
    }

    #[test]
    fn per_agent_model_lanes_are_enabled_until_the_user_selects_company_default() {
        let (_dir, _guard) = private_temp_home();
        assert_eq!(
            effective_string("agents.model_assignment", &SettingsScope::Global).as_deref(),
            Some("individual")
        );
        execute(SettingsCommand::Set {
            key: "agents.model_assignment".into(),
            value: json!("inherit"),
            scope: SettingsScope::Global,
            expected_revision: Some(0),
        })
        .unwrap();
        assert_eq!(
            effective_string("agents.model_assignment", &SettingsScope::Global).as_deref(),
            Some("inherit")
        );
    }

    #[test]
    fn browser_access_defaults_to_automatic_portable_login_and_free_accounts() {
        let (_dir, _guard) = private_temp_home();
        let scope = SettingsScope::Global;
        assert_eq!(
            effective_string("permissions.login_import", &scope).as_deref(),
            Some("allow")
        );
        assert_eq!(
            effective_string("permissions.account_creation", &scope).as_deref(),
            Some("allow_free")
        );
        assert_eq!(
            effective_string("browser.cookie_import_source", &scope).as_deref(),
            Some("auto")
        );
        assert_eq!(
            effective_string("permissions.purchases", &scope).as_deref(),
            Some("always_ask")
        );
    }

    #[test]
    fn search_separates_advanced_matches_and_dependencies_are_live() {
        let (_dir, _guard) = private_temp_home();
        let reply = execute(SettingsCommand::Set {
            key: "notifications.enabled".into(),
            value: json!(false),
            scope: SettingsScope::Global,
            expected_revision: Some(0),
        })
        .unwrap();
        let revision = match reply {
            SettingsReply::Snapshot { snapshot } => snapshot.revision,
            _ => unreachable!(),
        };
        let snapshot = match execute(SettingsCommand::Snapshot {
            scope: SettingsScope::Global,
        })
        .unwrap()
        {
            SettingsReply::Snapshot { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let sound = snapshot
            .settings
            .iter()
            .find(|entry| entry.definition.key == "notifications.sound")
            .unwrap();
        assert!(!sound.enabled);
        assert!(sound
            .disabled_reason
            .as_deref()
            .unwrap()
            .contains("notifications"));
        let results = match execute(SettingsCommand::Search {
            query: "receipt".into(),
            section: None,
            scope: SettingsScope::Global,
        })
        .unwrap()
        {
            SettingsReply::Search { results } => results,
            _ => unreachable!(),
        };
        assert_eq!(results.revision, revision);
        assert!(results.regular.is_empty());
        assert!(results
            .advanced
            .iter()
            .any(|entry| entry.definition.key == "advanced.tool_receipts"));

        let credential_results = match execute(SettingsCommand::Search {
            query: "credential vault".into(),
            section: None,
            scope: SettingsScope::Global,
        })
        .unwrap()
        {
            SettingsReply::Search { results } => results,
            _ => unreachable!(),
        };
        assert!(credential_results
            .regular_destinations
            .iter()
            .any(|entry| entry.id == "credentials"));
        let integration_results = match execute(SettingsCommand::Search {
            query: "composio".into(),
            section: None,
            scope: SettingsScope::Global,
        })
        .unwrap()
        {
            SettingsReply::Search { results } => results,
            _ => unreachable!(),
        };
        assert!(integration_results
            .regular_destinations
            .iter()
            .any(|entry| entry.id == "composio"));
        assert!(integration_results
            .advanced_destinations
            .iter()
            .any(|entry| entry.id == "integrations"));

        let relationship_results = match execute(SettingsCommand::Search {
            query: "coworker trust".into(),
            section: None,
            scope: SettingsScope::Global,
        })
        .unwrap()
        {
            SettingsReply::Search { results } => results,
            _ => unreachable!(),
        };
        assert!(relationship_results.regular_destinations.is_empty());
        assert!(relationship_results
            .advanced_destinations
            .iter()
            .any(|entry| entry.id == "relationships"));
    }

    #[test]
    fn scoped_override_inherits_and_reset_restores_global() {
        let (_dir, _guard) = private_temp_home();
        let global = execute(SettingsCommand::Set {
            key: "composer.show_reasoning".into(),
            value: json!(false),
            scope: SettingsScope::Global,
            expected_revision: Some(0),
        })
        .unwrap();
        let revision = match global {
            SettingsReply::Snapshot { snapshot } => snapshot.revision,
            _ => unreachable!(),
        };
        let scope = SettingsScope::Agent { id: "iris".into() };
        let scoped = execute(SettingsCommand::Set {
            key: "composer.show_reasoning".into(),
            value: json!(true),
            scope: scope.clone(),
            expected_revision: Some(revision),
        })
        .unwrap();
        let revision = match scoped {
            SettingsReply::Snapshot { snapshot } => {
                let row = snapshot
                    .settings
                    .iter()
                    .find(|entry| entry.definition.key == "composer.show_reasoning")
                    .unwrap();
                assert_eq!(row.value, json!(true));
                assert_eq!(row.inherited_from, "agent:iris");
                snapshot.revision
            }
            _ => unreachable!(),
        };
        let reset = execute(SettingsCommand::Reset {
            key: "composer.show_reasoning".into(),
            scope,
            expected_revision: Some(revision),
        })
        .unwrap();
        match reset {
            SettingsReply::Snapshot { snapshot } => {
                let row = snapshot
                    .settings
                    .iter()
                    .find(|entry| entry.definition.key == "composer.show_reasoning")
                    .unwrap();
                assert_eq!(row.value, json!(false));
                assert_eq!(row.inherited_from, "global");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn every_catalog_setting_validates_persists_and_reports_its_real_scope() {
        let (_dir, _guard) = private_temp_home();
        let definitions = catalog();
        let unique = definitions
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            definitions.len(),
            "setting keys must be unique"
        );

        let mut revision = 0;
        for definition in &definitions {
            validate_value(definition, &definition.default)
                .unwrap_or_else(|error| panic!("invalid default for {}: {error}", definition.key));
            let reply = execute(SettingsCommand::Set {
                key: definition.key.clone(),
                value: definition.default.clone(),
                scope: SettingsScope::Global,
                expected_revision: Some(revision),
            })
            .unwrap_or_else(|error| panic!("could not persist {}: {error}", definition.key));
            revision = match reply {
                SettingsReply::Snapshot { snapshot } => snapshot.revision,
                _ => unreachable!(),
            };
        }

        let document = load_document_at(&settings_path()).unwrap();
        assert_eq!(document.revision, definitions.len() as u64);
        assert_eq!(document.global.len(), definitions.len());
        for definition in &definitions {
            assert_eq!(
                document.global.get(&definition.key),
                Some(&definition.default)
            );
        }

        // Scope enablement is what this second half verifies. Turn on the
        // opt-in parent so its numeric child is not correctly disabled by the
        // dependency engine while we inspect agent/group override flags.
        execute(SettingsCommand::Set {
            key: "agents.volume_workers_enabled".into(),
            value: json!(true),
            scope: SettingsScope::Global,
            expected_revision: Some(revision),
        })
        .unwrap();

        for scope in [
            SettingsScope::Agent { id: "iris".into() },
            SettingsScope::Group {
                id: "launch-room".into(),
            },
        ] {
            let snapshot = match execute(SettingsCommand::Snapshot {
                scope: scope.clone(),
            })
            .unwrap()
            {
                SettingsReply::Snapshot { snapshot } => snapshot,
                _ => unreachable!(),
            };
            for row in snapshot.settings {
                let expected = match &scope {
                    SettingsScope::Agent { .. } => row.definition.agent_override,
                    SettingsScope::Group { .. } => row.definition.group_override,
                    SettingsScope::Global => true,
                };
                assert_eq!(
                    row.enabled,
                    expected,
                    "{} exposed the wrong control state in {}",
                    row.definition.key,
                    scope.label()
                );
            }
        }
    }

    #[test]
    fn legacy_preferences_are_imported_without_being_rewritten() {
        let (dir, _guard) = private_temp_home();
        let legacy = dir.path().join("canvas-prefs.json");
        let original = br#"{"theme":"paper","sidebarWidth":376,"projects":{"keep":"all"}}"#;
        crate::config::private_io::atomic_write_private(&legacy, original).unwrap();
        let reply = execute(SettingsCommand::Snapshot {
            scope: SettingsScope::Global,
        })
        .unwrap();
        let snapshot = match reply {
            SettingsReply::Snapshot { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert_eq!(
            snapshot
                .settings
                .iter()
                .find(|entry| entry.definition.key == "appearance.theme")
                .unwrap()
                .value,
            json!("light")
        );
        assert_eq!(
            snapshot
                .settings
                .iter()
                .find(|entry| entry.definition.key == "sidebar.width")
                .unwrap()
                .value,
            json!(376)
        );
        assert_eq!(std::fs::read(&legacy).unwrap(), original);
        assert_eq!(
            explicit_string("appearance.theme", &SettingsScope::Global).as_deref(),
            Some("light")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("settings.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rejects_wrong_type_stale_revision_and_non_overridable_scope() {
        let (_dir, _guard) = private_temp_home();
        assert!(execute(SettingsCommand::Set {
            key: "appearance.theme".into(),
            value: json!(true),
            scope: SettingsScope::Global,
            expected_revision: Some(0)
        })
        .is_err());
        let first = execute(SettingsCommand::Set {
            key: "appearance.theme".into(),
            value: json!("dark"),
            scope: SettingsScope::Global,
            expected_revision: Some(0),
        })
        .unwrap();
        let revision = match first {
            SettingsReply::Snapshot { snapshot } => snapshot.revision,
            _ => unreachable!(),
        };
        assert!(execute(SettingsCommand::Set {
            key: "appearance.theme".into(),
            value: json!("light"),
            scope: SettingsScope::Global,
            expected_revision: Some(0)
        })
        .is_err());
        assert!(execute(SettingsCommand::Set {
            key: "appearance.theme".into(),
            value: json!("light"),
            scope: SettingsScope::Agent { id: "iris".into() },
            expected_revision: Some(revision)
        })
        .is_err());

        let scoped = match execute(SettingsCommand::Snapshot {
            scope: SettingsScope::Agent { id: "iris".into() },
        })
        .unwrap()
        {
            SettingsReply::Snapshot { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let company_theme = scoped
            .settings
            .iter()
            .find(|entry| entry.definition.key == "appearance.theme")
            .unwrap();
        assert!(!company_theme.enabled);
        assert_eq!(
            company_theme.disabled_reason.as_deref(),
            Some("This setting is controlled by Company defaults.")
        );
        let reasoning = scoped
            .settings
            .iter()
            .find(|entry| entry.definition.key == "composer.show_reasoning")
            .unwrap();
        assert!(reasoning.enabled);
    }

    #[test]
    fn model_control_updates_real_config_without_exposing_credentials() {
        use crate::config::auth_profile::{
            AuthProfileCredential, AuthProfileState, AuthProfileStore,
        };

        let (dir, _guard) = private_temp_home();
        crate::config::private_io::atomic_write_private(
            &dir.path().join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"local\"\nefforts = { specialist = \"medium\" }\nagent_models = { planner = \"local\" }\nauth_by_lane = {}\n",
        )
        .unwrap();
        let mut profiles = std::collections::HashMap::new();
        profiles.insert(
            "openai:work".to_string(),
            AuthProfileCredential::ApiKey {
                provider: "openai".into(),
                key: "must-never-cross-settings".into(),
                display_name: Some("Work".into()),
            },
        );
        let store = AuthProfileStore {
            version: 1,
            profiles,
            models: Default::default(),
            labels: Default::default(),
            assignments: Default::default(),
            state: AuthProfileState::default(),
            profile_auth_epochs: Default::default(),
        };
        crate::config::private_io::atomic_write_private(
            &dir.path().join("auth-profiles.json"),
            &serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();

        let initial = match execute(SettingsCommand::ModelsSnapshot).unwrap() {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert_eq!(
            initial
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["openai"],
            "the route catalog must expose only providers backed by configured accounts"
        );
        assert!(initial
            .lanes
            .iter()
            .find(|lane| lane.lane == "phoenix")
            .is_some_and(|lane| lane.configured));
        for optional in ["vision", "image", "stt", "tts", "realtime"] {
            assert!(initial
                .lanes
                .iter()
                .find(|lane| lane.lane == optional)
                .is_some_and(|lane| !lane.configured));
        }
        assert!(!serde_json::to_string(&initial)
            .unwrap()
            .contains("must-never-cross-settings"));
        let sol = initial
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == "gpt-5.6-sol")
            })
            .expect("OpenAI Sol catalog entry");
        assert_eq!(
            sol.effort_levels,
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        let revision = initial.config_revision.clone();
        let unknown_provider = execute(SettingsCommand::SetModelLane {
            lane: "phoenix".into(),
            provider_id: "invented-provider".into(),
            model: "custom-model".into(),
            reasoning_effort: None,
            auth_profile_id: None,
            expected_config_revision: Some(revision.clone()),
        })
        .unwrap_err();
        assert!(unknown_provider
            .to_string()
            .contains("unknown model provider"));
        let unsupported_effort = execute(SettingsCommand::SetModelLane {
            lane: "phoenix".into(),
            provider_id: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            reasoning_effort: Some("max".into()),
            auth_profile_id: None,
            expected_config_revision: Some(revision),
        })
        .unwrap_err();
        assert!(unsupported_effort.to_string().contains("is not supported"));
        let updated = match execute(SettingsCommand::SetModelLane {
            lane: "iris".into(),
            provider_id: "openai".into(),
            model: "gpt-5.6-sol".into(),
            reasoning_effort: Some("high".into()),
            auth_profile_id: Some("openai:work".into()),
            expected_config_revision: Some(initial.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let iris = updated
            .lanes
            .iter()
            .find(|lane| lane.lane == "iris")
            .unwrap();
        assert_eq!(iris.provider_id, "openai");
        assert_eq!(iris.model, "gpt-5.6-sol");
        assert_eq!(iris.auth_profile_id.as_deref(), Some("openai:work"));
        assert_eq!(iris.max_context_window, Some(1_050_000));
        assert_eq!(iris.context_window, Some(1_050_000));
        assert_eq!(iris.context_window_override, None);
        let contextual = match execute(SettingsCommand::SetModelContextWindow {
            lane: "iris".into(),
            context_window: Some(262_144),
            expected_config_revision: Some(updated.config_revision.clone()),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let contextual_iris = contextual
            .lanes
            .iter()
            .find(|lane| lane.lane == "iris")
            .unwrap();
        assert_eq!(contextual_iris.context_window, Some(262_144));
        assert_eq!(contextual_iris.max_context_window, Some(1_050_000));
        assert_eq!(contextual_iris.context_window_override, Some(262_144));
        assert_eq!(
            crate::config::PhoenixConfig::load()
                .unwrap()
                .profile
                .llm
                .context_windows
                .get("iris")
                .copied(),
            Some(262_144)
        );
        let oversized_context = execute(SettingsCommand::SetModelContextWindow {
            lane: "iris".into(),
            context_window: Some(1_050_001),
            expected_config_revision: Some(contextual.config_revision.clone()),
        })
        .unwrap_err();
        assert!(oversized_context.to_string().contains("exceeds"));
        let restored_context = match execute(SettingsCommand::SetModelContextWindow {
            lane: "iris".into(),
            context_window: None,
            expected_config_revision: Some(contextual.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let restored_iris = restored_context
            .lanes
            .iter()
            .find(|lane| lane.lane == "iris")
            .unwrap();
        assert_eq!(restored_iris.context_window, Some(1_050_000));
        assert_eq!(restored_iris.context_window_override, None);
        let loaded_after_inline_normalization = crate::config::PhoenixConfig::load().unwrap();
        assert_eq!(
            loaded_after_inline_normalization
                .profile
                .llm
                .agent_models
                .get("planner")
                .map(String::as_str),
            Some("local")
        );
        assert_eq!(
            loaded_after_inline_normalization
                .profile
                .llm
                .efforts
                .get("specialist")
                .map(String::as_str),
            Some("medium")
        );

        let with_fallback = match execute(SettingsCommand::SetFallbackChain {
            lane: "iris".into(),
            profile_ids: vec!["openai:work".into()],
            expected_config_revision: Some(restored_context.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let final_snapshot = match execute(SettingsCommand::SetCompactionMode {
            provider_id: "openai".into(),
            mode: "native_preferred".into(),
            expected_config_revision: Some(with_fallback.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert_eq!(
            final_snapshot.fallback_chains.get("iris"),
            Some(&vec!["openai:work".into()])
        );
        assert_eq!(
            final_snapshot
                .compaction_modes
                .get("openai")
                .map(String::as_str),
            Some("native_preferred")
        );
        let loaded = crate::config::PhoenixConfig::load().unwrap();
        assert_eq!(loaded.profile.llm.agent_model("iris"), "gpt-5.6-sol");
        assert_eq!(
            loaded
                .profile
                .llm
                .auth_by_lane
                .get("iris")
                .map(String::as_str),
            Some("openai:work")
        );
        let inherited = match execute(SettingsCommand::ResetModelLane {
            lane: "iris".into(),
            expected_config_revision: Some(final_snapshot.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let iris = inherited
            .lanes
            .iter()
            .find(|lane| lane.lane == "iris")
            .expect("the fallback-only route remains visible in settings");
        assert!(iris.inherited);
        assert_eq!(iris.model, loaded.profile.llm.specialist());
        assert_eq!(
            inherited.fallback_chains.get("iris"),
            Some(&vec!["openai:work".into()]),
            "changing the primary model must not silently erase an agent's backups"
        );
        let loaded = crate::config::PhoenixConfig::load().unwrap();
        assert!(!loaded.profile.llm.agent_models.contains_key("iris"));
        assert!(!loaded.profile.llm.auth_by_lane.contains_key("iris"));
        assert!(!loaded.profile.llm.efforts.contains_key("iris"));
    }

    #[test]
    fn image_generation_primary_and_fallback_are_configurable_in_settings() {
        use crate::config::auth_profile::{
            AuthProfileCredential, AuthProfileState, AuthProfileStore,
        };

        let (dir, _guard) = private_temp_home();
        crate::config::private_io::atomic_write_private(
            &dir.path().join("config.toml"),
            b"[profile]\nname = \"test\"\n[profile.llm]\nprovider = \"ollama\"\nmodel = \"local\"\n",
        )
        .unwrap();
        let profiles = ["openai:image-primary", "openai:image-backup"]
            .into_iter()
            .map(|id| {
                (
                    id.to_string(),
                    AuthProfileCredential::ApiKey {
                        provider: "openai".into(),
                        key: format!("secret-{id}"),
                        display_name: Some(id.to_string()),
                    },
                )
            })
            .collect();
        let store = AuthProfileStore {
            version: 1,
            profiles,
            models: Default::default(),
            labels: Default::default(),
            assignments: Default::default(),
            state: AuthProfileState::default(),
            profile_auth_epochs: Default::default(),
        };
        crate::config::private_io::atomic_write_private(
            &dir.path().join("auth-profiles.json"),
            &serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();

        let initial = match execute(SettingsCommand::ModelsSnapshot).unwrap() {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let routed = match execute(SettingsCommand::SetModelLane {
            lane: "image".into(),
            provider_id: "openai".into(),
            model: "gpt-image-1".into(),
            reasoning_effort: None,
            auth_profile_id: Some("openai:image-primary".into()),
            expected_config_revision: Some(initial.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        let image = routed
            .lanes
            .iter()
            .find(|lane| lane.lane == "image")
            .expect("image capability lane remains visible");
        assert!(image.configured);
        assert_eq!(image.provider_id, "openai");
        assert_eq!(image.model, "gpt-image-1");
        assert_eq!(
            image.auth_profile_id.as_deref(),
            Some("openai:image-primary")
        );

        let with_fallback = match execute(SettingsCommand::SetFallbackChain {
            lane: "image".into(),
            profile_ids: vec!["openai:image-backup".into()],
            expected_config_revision: Some(routed.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert_eq!(
            with_fallback.fallback_chains.get("image"),
            Some(&vec!["openai:image-backup".to_string()])
        );
        let loaded = crate::config::PhoenixConfig::load().unwrap();
        assert_eq!(loaded.profile.llm.image_provider.as_deref(), Some("openai"));
        assert_eq!(
            loaded.profile.llm.image_model.as_deref(),
            Some("gpt-image-1")
        );
        assert_eq!(
            loaded.profile.llm.fallback.image,
            vec!["openai:image-backup".to_string()]
        );
    }

    #[test]
    fn codex_efficiency_action_is_explicit_deterministic_and_preserves_agent_overrides() {
        let (dir, _guard) = private_temp_home();
        let config_path = dir.path().join("config.toml");
        crate::config::private_io::atomic_write_private(
            &config_path,
            br#"[profile]
name = "test"
[profile.llm]
provider = "openai-codex"
model = "gpt-5.6-sol"
orchestrator_model = "gpt-5.6-sol"
specialist_model = "gpt-5.6-sol"
librarian_model = "gpt-5.6-sol"
memory_model = "gpt-5.6-sol"
efforts = { orchestrator = "high", specialist = "medium", librarian = "medium", iris = "xhigh" }
agent_models = { iris = "gpt-5.6-sol-pro" }
auth_by_lane = { iris = "openai-codex:work" }
fallback = { agents = { iris = ["openai-codex:backup"] } }
"#,
        )
        .unwrap();

        let initial = match execute(SettingsCommand::ModelsSnapshot).unwrap() {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert!(initial.codex_efficiency.available);
        assert!(!initial.codex_efficiency.active);
        assert!(execute(SettingsCommand::ApplyCodexEfficiencyDefaults {
            expected_config_revision: None,
        })
        .unwrap_err()
        .to_string()
        .contains("must be reloaded"));

        let migrated = match execute(SettingsCommand::ApplyCodexEfficiencyDefaults {
            expected_config_revision: Some(initial.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert!(migrated.codex_efficiency.active);
        let volume = migrated
            .lanes
            .iter()
            .find(|lane| lane.lane == "volume_worker")
            .expect("volume worker route");
        assert_eq!(volume.provider_id, "openai-codex");
        assert_eq!(volume.model, crate::config::CODEX_TEAM_MODEL);
        assert_eq!(
            volume.reasoning_effort.as_deref(),
            Some(crate::config::CODEX_TEAM_EFFORT)
        );
        let iris = migrated
            .lanes
            .iter()
            .find(|lane| lane.lane == "iris")
            .expect("individual coworker route");
        assert_eq!(iris.model, "gpt-5.6-sol-pro");
        assert_eq!(iris.reasoning_effort.as_deref(), Some("xhigh"));

        let loaded = crate::config::PhoenixConfig::load().unwrap();
        assert_eq!(
            loaded.profile.llm.orchestrator(),
            crate::config::CODEX_PHOENIX_MODEL
        );
        assert_eq!(
            loaded.profile.llm.specialist(),
            crate::config::CODEX_TEAM_MODEL
        );
        assert_eq!(
            loaded.profile.llm.agent_model("volume_worker"),
            crate::config::CODEX_TEAM_MODEL
        );
        assert_eq!(
            loaded.profile.llm.agent_effort("volume_worker").as_deref(),
            Some(crate::config::CODEX_TEAM_EFFORT)
        );
        assert_eq!(loaded.profile.llm.agent_model("iris"), "gpt-5.6-sol-pro");
        assert_eq!(
            loaded.profile.llm.agent_effort("iris").as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            loaded
                .profile
                .llm
                .auth_by_lane
                .get("iris")
                .map(String::as_str),
            Some("openai-codex:work")
        );
        assert_eq!(
            loaded.profile.llm.fallback.agents.get("iris"),
            Some(&vec!["openai-codex:backup".to_string()])
        );

        let once = std::fs::read(&config_path).unwrap();
        let again = match execute(SettingsCommand::ApplyCodexEfficiencyDefaults {
            expected_config_revision: Some(migrated.config_revision),
        })
        .unwrap()
        {
            SettingsReply::Models { snapshot } => snapshot,
            _ => unreachable!(),
        };
        assert!(again.codex_efficiency.active);
        assert_eq!(std::fs::read(config_path).unwrap(), once);
    }
}
