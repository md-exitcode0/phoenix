//! Typed desktop control surface for coworkers, groups, and the new sidebar.
//!
//! The UI never edits SQLite, session JSON, or agent manifests directly. Every
//! mutation returns a fresh authoritative view so reconnects and concurrent
//! windows converge without client-side optimistic state becoming truth.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::company::{CompanyStore, GroupMemberInput, SidebarItemKey};
use super::company_directory::{AgentRecord, DirectorySnapshot, GroupProfile, LifecycleState};

const MAX_PROMPT_RAIL_ROWS: usize = 24;
const MAX_PROMPT_PREVIEW_CHARS: usize = 180;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRailEntry {
    pub message_index: usize,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidebarActivity {
    pub item: SidebarItemKey,
    pub canonical_session_id: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub activity_label: Option<String>,
    pub active_agent_ids: Vec<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub transcript_revision: u64,
    pub last_read_revision: u64,
    pub unread: bool,
    pub modified_at: Option<String>,
    pub recent_prompts: Vec<PromptRailEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyDirectoryView {
    pub directory: DirectorySnapshot,
    pub activities: Vec<SidebarActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<CompanyDirectoryMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompanyDirectoryMutation {
    AgentRequested { agent_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupRoutingMode {
    MentionsOnly,
    Suggested,
    Automatic,
}

impl Default for GroupRoutingMode {
    fn default() -> Self {
        Self::MentionsOnly
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupConversationSettings {
    #[serde(default = "default_discussion_rounds")]
    pub discussion_rounds: u8,
    #[serde(default = "default_read_full_transcript")]
    pub read_full_transcript: bool,
    /// Typed P2 seams. They are deliberately persisted but cannot be enabled
    /// until their execution and authorization paths are implemented.
    #[serde(default)]
    pub routing_mode: GroupRoutingMode,
    #[serde(default)]
    pub structured_handoffs: bool,
    #[serde(default)]
    pub visible_execution_queues: bool,
    #[serde(default)]
    pub group_templates: bool,
    #[serde(default)]
    pub granular_retention: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentAvatarMode {
    Flame,
    Custom,
    Sidekick,
    /// An animated vector mark (see `AVATAR_MORPHS`) in one gradient.
    Morph,
}

impl Default for AgentAvatarMode {
    fn default() -> Self {
        Self::Flame
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSidekickModel {
    Ember,
    Cinder,
    Kiln,
    Wisp,
}

const SIDEKICK_FAMILIES: &[&str] = &[
    "classic_flame",
    "ember_orb",
    "shard_flame",
    "split_flame",
    "halo_core",
    "smoke_wisp",
];

const SIDEKICK_EYE_STYLES: &[&str] = &["round", "spark", "slit", "visor", "closed"];

/// Animated avatar marks and their gradient presets (canvas-app/ui/avatar-morph.js).
const AVATAR_MORPHS: &[&str] = &["phoenix", "orbit"];
const AVATAR_GRADIENTS: &[&str] = &[
    "01-amber", "02-coral", "03-rose", "04-violet", "05-iris",
    "06-ocean", "07-mint", "08-lime", "09-gold", "10-ember", "custom",
];

fn valid_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentAvatarConfig {
    #[serde(default)]
    pub mode: AgentAvatarMode,
    #[serde(default = "default_avatar_shape")]
    pub shape: String,
    #[serde(default = "default_avatar_expression")]
    pub expression: String,
    #[serde(default = "default_avatar_accessory")]
    pub accessory: String,
    // Omission preserves the shape of pre-slot configurations. These values
    // remain stored when a sidekick is selected, even though it does not wear them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eyewear: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    /// New procedural 2D sidekicks use a family identifier. `model_id` remains
    /// readable for legacy 3D profiles so existing metadata can migrate forward
    /// without being discarded or rewritten on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_id: Option<String>,
    #[serde(default = "default_avatar_fiery", skip_serializing_if = "avatar_fiery_is_default")]
    pub fiery: bool,
    #[serde(default = "default_avatar_eye_style", skip_serializing_if = "avatar_eye_style_is_default")]
    pub eye_style: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<AgentSidekickModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_image_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub morph_id: Option<String>,
    /// A preset id, or `custom` with exactly three `gradient_stops`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient_stops: Option<Vec<String>>,
}

fn default_avatar_shape() -> String {
    "classic".to_string()
}

fn default_avatar_expression() -> String {
    "bright".to_string()
}

fn default_avatar_accessory() -> String {
    "none".to_string()
}

fn default_avatar_fiery() -> bool {
    true
}

fn avatar_fiery_is_default(value: &bool) -> bool {
    *value
}

fn default_avatar_eye_style() -> String {
    "round".to_string()
}

fn avatar_eye_style_is_default(value: &str) -> bool {
    value == "round"
}

impl Default for AgentAvatarConfig {
    fn default() -> Self {
        Self {
            mode: AgentAvatarMode::Flame,
            shape: default_avatar_shape(),
            expression: default_avatar_expression(),
            accessory: default_avatar_accessory(),
            hat: None,
            eyewear: None,
            extra: None,
            family_id: None,
            fiery: default_avatar_fiery(),
            eye_style: default_avatar_eye_style(),
            model_id: None,
            custom_image_id: None,
            morph_id: None,
            gradient: None,
            gradient_stops: None,
        }
    }
}

impl AgentAvatarConfig {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            ["classic", "soft", "wild", "tall"].contains(&self.shape.as_str()),
            "unknown flame shape `{}`",
            self.shape
        );
        anyhow::ensure!(
            [
                "bright",
                "joy",
                "curious",
                "focused",
                "mischief",
                "calm",
                "sleepy",
                "determined"
            ]
            .contains(&self.expression.as_str()),
            "unknown avatar expression `{}`",
            self.expression
        );
        anyhow::ensure!(
            [
                "none",
                "round_glasses",
                "square_glasses",
                "sunglasses",
                "headphones",
                "spark"
            ]
            .contains(&self.accessory.as_str()),
            "unknown flame accessory `{}`",
            self.accessory
        );
        for (name, selected, allowed) in [
            ("hat", self.hat.as_deref(), &["none", "top_hat", "beanie", "cap", "crown", "halo"][..]),
            ("eyewear", self.eyewear.as_deref(), &["none", "round_glasses", "square_glasses", "sunglasses", "monocle"][..]),
            ("extra", self.extra.as_deref(), &["none", "headphones", "spark", "bowtie", "scarf"][..]),
        ] {
            if let Some(value) = selected {
                anyhow::ensure!(allowed.contains(&value), "unknown flame {name} `{value}`");
            }
        }
        if let Some(family_id) = self.family_id.as_deref() {
            anyhow::ensure!(
                SIDEKICK_FAMILIES.contains(&family_id),
                "unknown sidekick family `{family_id}`"
            );
        }
        anyhow::ensure!(
            SIDEKICK_EYE_STYLES.contains(&self.eye_style.as_str()),
            "unknown sidekick eye style `{}`",
            self.eye_style
        );
        match self.mode {
            AgentAvatarMode::Flame => {}
            AgentAvatarMode::Morph => {
                let morph = self.morph_id.as_deref().context("choose an avatar mark")?;
                anyhow::ensure!(AVATAR_MORPHS.contains(&morph), "unknown avatar mark `{morph}`");
            }
            AgentAvatarMode::Sidekick => {
                anyhow::ensure!(
                    self.family_id.is_some() || self.model_id.is_some(),
                    "choose a family for this sidekick avatar"
                );
            }
            AgentAvatarMode::Custom => {
                let image_id = self
                    .custom_image_id
                    .as_deref()
                    .context("custom avatar requires an uploaded image")?;
                anyhow::ensure!(valid_avatar_image_id(image_id), "invalid custom avatar id");
            }
        }
        if let Some(image_id) = self.custom_image_id.as_deref() {
            anyhow::ensure!(valid_avatar_image_id(image_id), "invalid custom avatar id");
        }
        if let Some(morph) = self.morph_id.as_deref() {
            anyhow::ensure!(AVATAR_MORPHS.contains(&morph), "unknown avatar mark `{morph}`");
        }
        if let Some(gradient) = self.gradient.as_deref() {
            anyhow::ensure!(AVATAR_GRADIENTS.contains(&gradient), "unknown avatar gradient `{gradient}`");
        }
        if let Some(stops) = self.gradient_stops.as_deref() {
            anyhow::ensure!(
                stops.len() == 3 && stops.iter().all(|stop| valid_hex_color(stop)),
                "a custom avatar gradient needs three #rrggbb colors"
            );
        }
        if self.gradient.as_deref() == Some("custom") {
            anyhow::ensure!(self.gradient_stops.is_some(), "a custom avatar gradient needs its colors");
        }
        Ok(())
    }
}

fn valid_avatar_image_id(value: &str) -> bool {
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem.len() <= 96
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && ["png", "jpg", "jpeg", "webp", "avif"].contains(&extension.to_ascii_lowercase().as_str())
}

fn avatar_metadata_json(current: &str, avatar: AgentAvatarConfig) -> Result<String> {
    avatar.validate()?;
    let mut metadata = serde_json::from_str::<serde_json::Value>(current)
        .unwrap_or_else(|_| serde_json::json!({}));
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }
    metadata["avatar"] = serde_json::to_value(avatar)?;
    Ok(serde_json::to_string(&metadata)?)
}

fn default_discussion_rounds() -> u8 {
    1
}

fn default_read_full_transcript() -> bool {
    true
}

impl Default for GroupConversationSettings {
    fn default() -> Self {
        Self {
            discussion_rounds: default_discussion_rounds(),
            read_full_transcript: default_read_full_transcript(),
            routing_mode: GroupRoutingMode::MentionsOnly,
            structured_handoffs: false,
            visible_execution_queues: false,
            group_templates: false,
            granular_retention: false,
        }
    }
}

impl GroupConversationSettings {
    fn validated_json(&self) -> Result<String> {
        anyhow::ensure!(
            (1..=4).contains(&self.discussion_rounds),
            "group discussion rounds must be 1..=4"
        );
        anyhow::ensure!(
            self.routing_mode == GroupRoutingMode::MentionsOnly,
            "suggested and automatic group routing are not enabled"
        );
        anyhow::ensure!(
            !self.structured_handoffs,
            "structured group handoffs are not enabled"
        );
        anyhow::ensure!(
            !self.visible_execution_queues,
            "visible group execution queues are not enabled"
        );
        anyhow::ensure!(!self.group_templates, "group templates are not enabled");
        anyhow::ensure!(
            !self.granular_retention,
            "granular group retention is not enabled"
        );
        Ok(serde_json::to_string(self)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CompanyDirectoryCommand {
    Status,
    /// The entire normal + Agent popup. Phoenix derives the role title,
    /// identity prompt, collaboration boundaries, and initial knowledge in a
    /// hidden refinement turn after this synchronous request is committed.
    CreateAgent {
        description: String,
        #[serde(default)]
        preferred_name: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        avatar: Option<AgentAvatarConfig>,
    },
    /// Create a new responsibility owner from another coworker's role shape.
    /// Private transcript, memory, credentials, and browser state are never
    /// copied; Phoenix refines the new identity through the normal setup flow.
    CloneAgent {
        source_agent_id: String,
        #[serde(default)]
        preferred_name: Option<String>,
    },
    RetryAgentProvisioning {
        agent_id: String,
    },
    CreateGroup {
        name: String,
        description: String,
        color: String,
        icon_seed: String,
        members: Vec<String>,
        #[serde(default)]
        settings: GroupConversationSettings,
        /// Group leader; must be one of `members`. Defaults to the chief of
        /// staff (`phoenix`) when a member, else the first member.
        #[serde(default)]
        leader_agent_id: Option<String>,
    },
    UpdateAgent {
        agent_id: String,
        display_name: Option<String>,
        role_title: Option<String>,
        description: Option<String>,
        color: Option<String>,
        icon_seed: Option<String>,
        #[serde(default)]
        avatar: Option<AgentAvatarConfig>,
    },
    UpdateGroup {
        group_id: String,
        name: Option<String>,
        description: Option<String>,
        color: Option<String>,
        icon_seed: Option<String>,
        settings: Option<GroupConversationSettings>,
        /// Optional leader change; must be a current active member.
        #[serde(default)]
        leader_agent_id: Option<String>,
    },
    SetGroupMembers {
        group_id: String,
        members: Vec<GroupMemberInput>,
    },
    /// Change a group's leader (must be a current active member).
    SetGroupLeader {
        group_id: String,
        leader_agent_id: String,
    },
    SetPinned {
        item: SidebarItemKey,
        pinned: bool,
    },
    Reorder {
        items: Vec<SidebarItemKey>,
    },
    SetAgentLifecycle {
        agent_id: String,
        lifecycle: LifecycleState,
    },
    SetGroupLifecycle {
        group_id: String,
        lifecycle: LifecycleState,
    },
    ScheduleAgentDeletion {
        agent_id: String,
        confirmed_name: String,
    },
    ScheduleGroupDeletion {
        group_id: String,
        confirmed_name: String,
    },
    SetOutsideCallGrant {
        group_id: String,
        agent_id: String,
        granted: bool,
    },
    UpdateRelationship {
        from_agent_id: String,
        to_agent_id: String,
        relationship: String,
        trust_level: String,
        #[serde(default)]
        policy_json: Option<String>,
    },
    /// Start an empty canonical thread while retaining the old transcript and
    /// every other part of this coworker's state.
    ResetAgentConversation {
        agent_id: String,
        confirmed_name: String,
    },
    MarkRead {
        item: SidebarItemKey,
    },
}

pub fn execute(command: CompanyDirectoryCommand) -> Result<CompanyDirectoryView> {
    let store = crate::runtime::company::global()?;
    execute_with(&store, command)
}

fn execute_with(
    store: &CompanyStore,
    command: CompanyDirectoryCommand,
) -> Result<CompanyDirectoryView> {
    let mut mutation = None;
    match command {
        CompanyDirectoryCommand::Status => {}
        CompanyDirectoryCommand::CreateAgent {
            description,
            preferred_name,
            color,
            avatar,
        } => {
            validate_agent_request(&description, preferred_name.as_deref(), color.as_deref())?;
            if let Some(avatar) = avatar.as_ref() {
                avatar.validate()?;
            }
            let snapshot = store.directory_snapshot()?;
            let role = unique_agent_id(
                &snapshot,
                preferred_name.as_deref().unwrap_or(description.as_str()),
            )?;
            let persona = preferred_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| Some("New coworker".to_string()));
            let result = crate::tools::agent_forge::execute_user_approved_create_agent(
                crate::tools::agent_forge::CreateAgentInput {
                    role: role.clone(),
                    persona,
                    description: agent_request_preview(&description),
                    mission: Some(description.trim().to_string()),
                },
            );
            anyhow::ensure!(result.success, "agent request failed: {}", result.output);
            if let Some(color) = color {
                store.update_agent_profile("user", &role, None, None, None, Some(color), None)?;
            }
            if let Some(avatar) = avatar {
                let profile = store
                    .directory_snapshot()?
                    .agents
                    .into_iter()
                    .find(|agent| agent.profile.agent_id == role)
                    .with_context(|| format!("new agent `{role}` is missing from the directory"))?
                    .profile;
                store.update_agent_metadata(
                    "user",
                    &role,
                    avatar_metadata_json(&profile.metadata_json, avatar)?,
                )?;
            }
            mutation = Some(CompanyDirectoryMutation::AgentRequested { agent_id: role });
        }
        CompanyDirectoryCommand::CloneAgent {
            source_agent_id,
            preferred_name,
        } => {
            let snapshot = store.directory_snapshot()?;
            let source = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == source_agent_id)
                .with_context(|| format!("unknown source coworker `{source_agent_id}`"))?;
            anyhow::ensure!(
                source.profile.agent_id != "phoenix",
                "Phoenix itself cannot be cloned"
            );
            anyhow::ensure!(
                source.profile.lifecycle == LifecycleState::Active,
                "restore `{source_agent_id}` before cloning it"
            );
            let clone_name = preferred_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} Copy", source.profile.display_name));
            let role = unique_agent_id(&snapshot, &clone_name)?;
            let result=crate::tools::agent_forge::execute_user_approved_create_agent(crate::tools::agent_forge::CreateAgentInput{
                role:role.clone(),
                persona:Some(clone_name),
                description:source.profile.description.clone(),
                mission:Some(format!("Clone the role shape of `{}` ({}) into a distinct coworker. Preserve the responsibility style and collaboration boundaries, but never copy its private thread, memory, credentials, browser session, or identity-specific secrets.",source.profile.display_name,source.profile.internal_role)),
            });
            anyhow::ensure!(
                result.success,
                "agent clone request failed: {}",
                result.output
            );
            store.update_agent_profile(
                "user",
                &role,
                None,
                None,
                None,
                Some(source.profile.color.clone()),
                None,
            )?;
            let mut metadata = store
                .directory_snapshot()?
                .agents
                .into_iter()
                .find(|agent| agent.profile.agent_id == role)
                .and_then(|agent| {
                    serde_json::from_str::<serde_json::Value>(&agent.profile.metadata_json).ok()
                })
                .unwrap_or_else(|| serde_json::json!({}));
            let source_metadata =
                serde_json::from_str::<serde_json::Value>(&source.profile.metadata_json)
                    .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(avatar) = source_metadata.get("avatar") {
                metadata["avatar"] = avatar.clone();
            }
            metadata["cloned_from"] = serde_json::Value::String(source.profile.agent_id.clone());
            store.update_agent_metadata("user", &role, serde_json::to_string(&metadata)?)?;
            mutation = Some(CompanyDirectoryMutation::AgentRequested { agent_id: role });
        }
        CompanyDirectoryCommand::RetryAgentProvisioning { agent_id } => {
            anyhow::ensure!(
                crate::tools::agent_forge::pending_agent_provisioning_roles()?
                    .iter()
                    .any(|role| role == &agent_id),
                "agent `{agent_id}` is not waiting for Phoenix provisioning"
            );
            mutation = Some(CompanyDirectoryMutation::AgentRequested { agent_id });
        }
        CompanyDirectoryCommand::CreateGroup {
            name,
            description,
            color,
            icon_seed,
            members,
            settings,
            leader_agent_id,
        } => {
            let snapshot = store.directory_snapshot()?;
            let name = if name.trim().is_empty() {
                super::company::generated_group_name(&snapshot, &members)?
            } else {
                name.trim().to_string()
            };
            let group_id = unique_group_id(&snapshot, &name)?;
            let icon_seed = safe_id(&icon_seed).unwrap_or_else(|| group_id.clone());
            let sort_order = snapshot
                .agents
                .iter()
                .map(|agent| agent.profile.sort_order)
                .chain(snapshot.groups.iter().map(|group| group.profile.sort_order))
                .max()
                .unwrap_or(-1)
                .saturating_add(1);
            store.create_group(
                "user",
                GroupProfile {
                    group_id: group_id.clone(),
                    name,
                    description,
                    color,
                    icon_seed,
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order,
                    canonical_session_id: Some(format!("group-{group_id}")),
                    metadata_json: settings.validated_json()?,
                    leader_agent_id: leader_agent_id.filter(|id| !id.trim().is_empty()),
                },
                members,
            )?;
        }
        CompanyDirectoryCommand::UpdateAgent {
            agent_id,
            display_name,
            role_title,
            description,
            color,
            icon_seed,
            avatar,
        } => {
            if let Some(avatar) = avatar.as_ref() {
                avatar.validate()?;
            }
            store.update_agent_profile(
                "user",
                &agent_id,
                display_name,
                role_title,
                description,
                color,
                icon_seed,
            )?;
            if let Some(avatar) = avatar {
                let profile = store
                    .directory_snapshot()?
                    .agents
                    .into_iter()
                    .find(|agent| agent.profile.agent_id == agent_id)
                    .with_context(|| format!("updated agent `{agent_id}` disappeared"))?
                    .profile;
                store.update_agent_metadata(
                    "user",
                    &agent_id,
                    avatar_metadata_json(&profile.metadata_json, avatar)?,
                )?;
            }
        }
        CompanyDirectoryCommand::UpdateGroup {
            group_id,
            name,
            description,
            color,
            icon_seed,
            settings,
            leader_agent_id,
        } => {
            let metadata_json = settings
                .map(|settings| settings.validated_json())
                .transpose()?;
            store.update_group_profile(
                "user",
                &group_id,
                name,
                description,
                color,
                icon_seed.and_then(|seed| safe_id(&seed)),
                metadata_json,
            )?;
            if let Some(leader) = leader_agent_id.filter(|id| !id.trim().is_empty()) {
                store.set_group_leader("user", &group_id, &leader)?;
            }
        }
        CompanyDirectoryCommand::SetGroupMembers { group_id, members } => {
            store.set_group_members("user", &group_id, members)?;
        }
        CompanyDirectoryCommand::SetGroupLeader {
            group_id,
            leader_agent_id,
        } => {
            store.set_group_leader("user", &group_id, &leader_agent_id)?;
        }
        CompanyDirectoryCommand::SetPinned { item, pinned } => {
            store.set_sidebar_item_pinned("user", &item, pinned)?;
        }
        CompanyDirectoryCommand::Reorder { items } => {
            store.reorder_sidebar_items("user", items)?;
        }
        CompanyDirectoryCommand::SetAgentLifecycle {
            agent_id,
            lifecycle,
        } => {
            anyhow::ensure!(
                lifecycle != LifecycleState::PendingDeletion,
                "use schedule_agent_deletion with the exact current name"
            );
            store.set_agent_lifecycle("user", &agent_id, lifecycle)?;
        }
        CompanyDirectoryCommand::SetGroupLifecycle {
            group_id,
            lifecycle,
        } => {
            anyhow::ensure!(
                lifecycle != LifecycleState::PendingDeletion,
                "use schedule_group_deletion with the exact current name"
            );
            store.set_group_lifecycle("user", &group_id, lifecycle)?;
        }
        CompanyDirectoryCommand::ScheduleAgentDeletion {
            agent_id,
            confirmed_name,
        } => {
            store.schedule_agent_deletion("user", &agent_id, &confirmed_name)?;
        }
        CompanyDirectoryCommand::ScheduleGroupDeletion {
            group_id,
            confirmed_name,
        } => {
            store.schedule_group_deletion("user", &group_id, &confirmed_name)?;
        }
        CompanyDirectoryCommand::SetOutsideCallGrant {
            group_id,
            agent_id,
            granted,
        } => {
            store.apply_directory_change(
                "user",
                format!(
                    "outside-call-user:{group_id}:{agent_id}:{granted}:{}",
                    uuid::Uuid::new_v4()
                ),
                super::company_directory::DirectoryChange::OutsideCallGrantSet {
                    group_id,
                    agent_id,
                    granted,
                },
            )?;
        }
        CompanyDirectoryCommand::UpdateRelationship {
            from_agent_id,
            to_agent_id,
            relationship,
            trust_level,
            policy_json,
        } => {
            let snapshot = store.directory_snapshot()?;
            validate_relationship_update(
                &snapshot,
                &from_agent_id,
                &to_agent_id,
                &relationship,
                &trust_level,
                policy_json.as_deref(),
            )?;
            let policy_json = policy_json
                .or_else(|| {
                    snapshot
                        .relationships
                        .iter()
                        .find(|record| {
                            record.from_agent_id == from_agent_id
                                && record.to_agent_id == to_agent_id
                        })
                        .map(|record| record.policy_json.clone())
                })
                .unwrap_or_else(|| {
                    r#"{"shares_minimum_needed":true,"requires_task_scope":true}"#.to_string()
                });
            store.apply_directory_change(
                "user",
                format!(
                    "relationship-user:{from_agent_id}:{to_agent_id}:{}",
                    uuid::Uuid::new_v4()
                ),
                super::company_directory::DirectoryChange::RelationshipUpserted {
                    from_agent_id,
                    to_agent_id,
                    relationship: relationship.trim().to_string(),
                    trust_level,
                    policy_json,
                },
            )?;
        }
        CompanyDirectoryCommand::ResetAgentConversation {
            agent_id,
            confirmed_name,
        } => {
            let snapshot = store.directory_snapshot()?;
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == agent_id)
                .with_context(|| format!("unknown agent `{agent_id}`"))?;
            anyhow::ensure!(
                agent.profile.display_name == confirmed_name,
                "coworker name confirmation does not match"
            );
            store.rotate_agent_canonical_session("user", &agent_id)?;
        }
        CompanyDirectoryCommand::MarkRead { item } => {
            let revision = match canonical_session(store, &item)? {
                Some(session_id) => read_session(&session_id)?
                    .map(|session| session.transcript_revision)
                    .unwrap_or(0),
                None => 0,
            };
            store.mark_conversation_read(&item, revision)?;
        }
    }
    let mut result = view(store)?;
    result.mutation = mutation;
    Ok(result)
}

fn validate_relationship_update(
    snapshot: &DirectorySnapshot,
    from_agent_id: &str,
    to_agent_id: &str,
    relationship: &str,
    trust_level: &str,
    policy_json: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        from_agent_id != to_agent_id,
        "a coworker cannot have a relationship with itself"
    );
    for (label, agent_id) in [("source", from_agent_id), ("target", to_agent_id)] {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == agent_id)
            .with_context(|| format!("unknown {label} coworker `{agent_id}`"))?;
        anyhow::ensure!(
            agent.profile.kind == super::company_directory::AgentKind::ResponsibilityOwner,
            "{label} coworker `{agent_id}` is not a visible company coworker"
        );
        anyhow::ensure!(
            agent.profile.lifecycle != LifecycleState::PendingDeletion,
            "{label} coworker `{agent_id}` is pending deletion"
        );
    }
    let relationship = relationship.trim();
    anyhow::ensure!(
        !relationship.is_empty()
            && relationship.len() <= 256
            && !relationship.chars().any(char::is_control),
        "relationship guidance must be 1..=256 plain-text characters"
    );
    anyhow::ensure!(
        matches!(trust_level, "new" | "coworker" | "trusted"),
        "trust level must be `new`, `coworker`, or `trusted`"
    );
    if let Some(policy_json) = policy_json {
        let policy: serde_json::Value =
            serde_json::from_str(policy_json).context("relationship policy must be valid JSON")?;
        anyhow::ensure!(
            policy.is_object(),
            "relationship policy must be a JSON object"
        );
    }
    Ok(())
}

pub fn view(store: &CompanyStore) -> Result<CompanyDirectoryView> {
    let directory = store.directory_snapshot()?;
    let company = store.snapshot(None)?;
    let live = super::company_activity::snapshot();
    let markers = store
        .conversation_read_markers()?
        .into_iter()
        .map(|marker| (marker.item, marker.last_read_revision))
        .collect::<HashMap<_, _>>();
    let mut activities = Vec::with_capacity(directory.agents.len() + directory.groups.len());
    for agent in directory.agents.iter().filter(|agent| {
        agent.profile.kind == super::company_directory::AgentKind::ResponsibilityOwner
    }) {
        activities.push(agent_activity(
            agent, &company, &directory, &live, &markers,
        )?);
    }
    for group in &directory.groups {
        let item = SidebarItemKey::Group(group.profile.group_id.clone());
        activities.push(activity_for(
            item,
            group.profile.canonical_session_id.as_deref(),
            lifecycle_status(group.profile.lifecycle),
            company
                .jobs
                .iter()
                .filter(|job| {
                    group.profile.canonical_session_id.as_deref() == Some(job.session_id.as_str())
                        && is_live_job(&job.state)
                })
                .collect(),
            live.iter()
                .filter(|activity| {
                    group.profile.canonical_session_id.as_deref()
                        == Some(activity.session_id.as_str())
                })
                .collect(),
            &directory,
            &markers,
        )?);
    }
    activities.sort_by_key(|activity| sidebar_order_key(&directory, &activity.item));
    Ok(CompanyDirectoryView {
        directory,
        activities,
        mutation: None,
    })
}

fn validate_agent_request(
    description: &str,
    preferred_name: Option<&str>,
    color: Option<&str>,
) -> Result<()> {
    let description = description.trim();
    anyhow::ensure!(
        !description.is_empty(),
        "describe what the coworker should own"
    );
    anyhow::ensure!(
        description.len() <= 64 * 1024 && !description.contains('\0'),
        "agent description is too large or contains an invalid character"
    );
    if let Some(name) = preferred_name {
        let name = name.trim();
        anyhow::ensure!(
            !name.is_empty()
                && name.len() <= 256
                && !name.chars().any(char::is_control)
                && !name.contains(['/', '\\']),
            "agent name must be a short plain-text name"
        );
    }
    if let Some(color) = color {
        anyhow::ensure!(
            color.starts_with('#')
                && matches!(color.len(), 4 | 7 | 9)
                && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "agent color must be a CSS hex color"
        );
    }
    Ok(())
}

fn agent_request_preview(description: &str) -> String {
    let preview = description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect::<String>();
    if preview.is_empty() {
        "Owns the responsibility described by the user".to_string()
    } else {
        preview
    }
}

fn unique_agent_id(snapshot: &DirectorySnapshot, source: &str) -> Result<String> {
    let stopwords = [
        "a", "an", "and", "agent", "for", "guy", "i", "me", "my", "of", "person", "please",
        "someone", "that", "the", "to", "want", "who",
    ];
    let mut words = source
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|word| !stopwords.contains(&word.as_str()))
        .take(5)
        .collect::<Vec<_>>();
    if words.is_empty() {
        words.push("coworker".to_string());
    }
    let mut base = words.join("_");
    base.truncate(48);
    while base.ends_with('_') {
        base.pop();
    }
    if base.is_empty() || !base.as_bytes()[0].is_ascii_lowercase() {
        base.insert_str(0, "coworker_");
    }
    let occupied = |candidate: &str| {
        snapshot
            .agents
            .iter()
            .any(|agent| agent.profile.agent_id == candidate)
            || crate::sub_agents::registry::reserved_agent_role(candidate)
            || matches!(candidate, "orchestrator" | "user" | "librarian" | "lib")
    };
    if !occupied(&base) {
        return Ok(base);
    }
    for suffix in 2..=9_999 {
        let stem = base.chars().take(56).collect::<String>();
        let candidate = format!("{stem}_{suffix}");
        if !occupied(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not allocate a unique agent id")
}

fn agent_activity(
    agent: &AgentRecord,
    company: &super::company::CompanySnapshot,
    directory: &DirectorySnapshot,
    live: &[super::company_activity::LiveCoworkerActivity],
    markers: &HashMap<SidebarItemKey, u64>,
) -> Result<SidebarActivity> {
    let item = SidebarItemKey::Agent(agent.profile.agent_id.clone());
    let live_jobs = company
        .jobs
        .iter()
        .filter(|job| {
            job.role == agent.profile.internal_role
                && agent.profile.canonical_session_id.as_deref() == Some(job.session_id.as_str())
                && is_live_job(&job.state)
        })
        .collect();
    activity_for(
        item,
        agent.profile.canonical_session_id.as_deref(),
        lifecycle_status(agent.profile.lifecycle),
        live_jobs,
        live.iter()
            .filter(|activity| {
                activity.internal_role == agent.profile.internal_role
                    && agent.profile.canonical_session_id.as_deref()
                        == Some(activity.session_id.as_str())
            })
            .collect(),
        directory,
        markers,
    )
}

fn activity_for(
    item: SidebarItemKey,
    canonical_session_id: Option<&str>,
    lifecycle_status: &str,
    mut live_jobs: Vec<&super::company::JobProjection>,
    mut live_activity: Vec<&super::company_activity::LiveCoworkerActivity>,
    directory: &DirectorySnapshot,
    markers: &HashMap<SidebarItemKey, u64>,
) -> Result<SidebarActivity> {
    live_jobs.sort_by_key(|job| job.as_of_seq);
    live_activity.sort_by_key(|activity| activity.updated_at.as_str());
    let session = canonical_session_id
        .map(read_session)
        .transpose()?
        .flatten();
    let transcript_revision = session
        .as_ref()
        .map(|session| session.transcript_revision)
        .unwrap_or(0);
    let last_read_revision = markers.get(&item).copied().unwrap_or(0);
    let recent_prompts = session.as_ref().map(recent_prompts).unwrap_or_default();
    let title = recent_prompts
        .last()
        .map(|prompt| prompt.preview.clone())
        .or_else(|| session.as_ref().and_then(|session| session.title.clone()));
    let modified_at = canonical_session_id.and_then(session_modified_at);
    let activity_label = live_activity
        .last()
        .map(|activity| activity.activity_label.clone())
        .or_else(|| live_jobs.last().map(|job| job.subject.clone()));
    let mut active_agent_ids = live_activity
        .iter()
        .filter_map(|activity| {
            directory
                .agents
                .iter()
                .find(|agent| agent.profile.internal_role == activity.internal_role)
                .map(|agent| agent.profile.agent_id.clone())
        })
        .chain(live_jobs.iter().filter_map(|job| {
            directory
                .agents
                .iter()
                .find(|agent| agent.profile.internal_role == job.role)
                .map(|agent| agent.profile.agent_id.clone())
                .or_else(|| Some(job.role.clone()))
        }))
        .collect::<Vec<_>>();
    active_agent_ids.sort();
    active_agent_ids.dedup();
    let status = if lifecycle_status != "active" {
        lifecycle_status.to_string()
    } else if !live_activity.is_empty() {
        "working".to_string()
    } else {
        live_jobs
            .last()
            .map(|job| job.state.as_str().to_string())
            .unwrap_or_else(|| "idle".to_string())
    };
    let provider_id = live_activity
        .last()
        .map(|activity| activity.provider_id.clone())
        .or_else(|| configured_provider_for(&item));
    let model = live_activity
        .last()
        .map(|activity| activity.model.clone())
        .or_else(|| session.as_ref().map(|session| session.model.clone()));
    Ok(SidebarActivity {
        item,
        canonical_session_id: canonical_session_id.map(str::to_string),
        title,
        status,
        activity_label,
        active_agent_ids,
        provider_id,
        model,
        transcript_revision,
        last_read_revision,
        unread: transcript_revision > last_read_revision,
        modified_at,
        recent_prompts,
    })
}

fn configured_provider_for(item: &SidebarItemKey) -> Option<String> {
    let config = crate::config::PhoenixConfig::load().ok()?;
    match item {
        SidebarItemKey::Agent(agent_id) if agent_id != "phoenix" => config
            .profile
            .llm
            .specialist_provider
            .clone()
            .or_else(|| Some(config.profile.llm.provider.clone())),
        SidebarItemKey::Agent(_) => Some(config.profile.llm.provider),
        SidebarItemKey::Group(_) => None,
    }
}

fn read_session(session_id: &str) -> Result<Option<crate::session::Session>> {
    crate::session::SessionStore::read_one_from_disk(
        &crate::config::paths::phoenix_sessions_root(),
        session_id,
    )
}

fn canonical_session(store: &CompanyStore, item: &SidebarItemKey) -> Result<Option<String>> {
    let snapshot = store.directory_snapshot()?;
    Ok(match item {
        SidebarItemKey::Agent(agent_id) => snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == *agent_id)
            .with_context(|| format!("unknown agent `{agent_id}`"))?
            .profile
            .canonical_session_id
            .clone(),
        SidebarItemKey::Group(group_id) => snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == *group_id)
            .with_context(|| format!("unknown group `{group_id}`"))?
            .profile
            .canonical_session_id
            .clone(),
    })
}

fn recent_prompts(session: &crate::session::Session) -> Vec<PromptRailEntry> {
    let mut prompts = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(message_index, message)| match message {
            crate::session::Message::User { content } => Some(PromptRailEntry {
                message_index,
                preview: one_line_preview(content, MAX_PROMPT_PREVIEW_CHARS),
            }),
            _ => None,
        })
        .filter(|entry| !entry.preview.is_empty())
        .collect::<Vec<_>>();
    if prompts.len() > MAX_PROMPT_RAIL_ROWS {
        prompts.drain(..prompts.len() - MAX_PROMPT_RAIL_ROWS);
    }
    prompts
}

fn one_line_preview(value: &str, max_chars: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flattened.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn session_modified_at(session_id: &str) -> Option<String> {
    let path = crate::config::paths::phoenix_sessions_root().join(format!("{session_id}.json"));
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let modified: DateTime<Utc> = metadata.modified().ok()?.into();
    Some(modified.to_rfc3339())
}

fn is_live_job(state: &super::company::AgentState) -> bool {
    !matches!(
        state,
        super::company::AgentState::Available
            | super::company::AgentState::Failed
            | super::company::AgentState::CompletedVerified
            | super::company::AgentState::CompletedUnverified
            | super::company::AgentState::Superseded
            | super::company::AgentState::Stale
            | super::company::AgentState::Unavailable
    )
}

fn lifecycle_status(lifecycle: LifecycleState) -> &'static str {
    match lifecycle {
        LifecycleState::Active => "active",
        LifecycleState::Dormant => "setting_up",
        LifecycleState::Archived => "archived",
        LifecycleState::PendingDeletion => "pending_deletion",
    }
}

fn sidebar_order_key(directory: &DirectorySnapshot, item: &SidebarItemKey) -> (bool, i64, String) {
    match item {
        SidebarItemKey::Agent(agent_id) => directory
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == *agent_id)
            .map(|agent| {
                (
                    !agent.profile.pinned,
                    agent.profile.sort_order,
                    agent.profile.display_name.to_ascii_lowercase(),
                )
            })
            .unwrap_or((true, i64::MAX, agent_id.clone())),
        SidebarItemKey::Group(group_id) => directory
            .groups
            .iter()
            .find(|group| group.profile.group_id == *group_id)
            .map(|group| {
                (
                    !group.profile.pinned,
                    group.profile.sort_order,
                    group.profile.name.to_ascii_lowercase(),
                )
            })
            .unwrap_or((true, i64::MAX, group_id.clone())),
    }
}

fn safe_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 96 {
        return None;
    }
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        .then(|| value.to_string())
}

fn unique_group_id(snapshot: &DirectorySnapshot, name: &str) -> Result<String> {
    anyhow::ensure!(!name.trim().is_empty(), "group name is empty");
    let mut base = String::new();
    let mut separator = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if separator && !base.is_empty() {
                base.push('-');
            }
            separator = false;
            base.push(ch.to_ascii_lowercase());
        } else {
            separator = true;
        }
        if base.len() >= 48 {
            break;
        }
    }
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() {
        base.push_str("group");
    }
    for suffix in 1..=10_000u32 {
        let candidate = if suffix == 1 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        if !snapshot
            .groups
            .iter()
            .any(|group| group.profile.group_id == candidate)
        {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not allocate a unique group id")
}

#[cfg(test)]
mod tests {
    use super::super::company_directory::HistoryAccess;
    use super::*;

    fn store(root: &std::path::Path) -> CompanyStore {
        CompanyStore::open(root.join("company/company.sqlite")).unwrap()
    }

    #[test]
    fn add_agent_popup_needs_only_job_name_and_color() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        let result = execute_with(
            &store,
            CompanyDirectoryCommand::CreateAgent {
                description: "Manage rental properties, tenants, maintenance, and records."
                    .to_string(),
                preferred_name: Some("Robin".to_string()),
                color: Some("#6E7DE8".to_string()),
                avatar: Some(AgentAvatarConfig {
                    mode: AgentAvatarMode::Flame,
                    shape: "wild".to_string(),
                    expression: "focused".to_string(),
                    accessory: "round_glasses".to_string(),
                    custom_image_id: None,
                    ..AgentAvatarConfig::default()
                }),
            },
        )
        .unwrap();
        let agent_id = match result.mutation.as_ref().unwrap() {
            CompanyDirectoryMutation::AgentRequested { agent_id } => agent_id,
        };
        assert_eq!(agent_id, "robin");
        let agent = result
            .directory
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == *agent_id)
            .unwrap();
        assert_eq!(agent.profile.display_name, "Robin");
        assert_eq!(agent.profile.color, "#6E7DE8");
        assert_eq!(agent.profile.lifecycle, LifecycleState::Dormant);
        let metadata: serde_json::Value =
            serde_json::from_str(&agent.profile.metadata_json).unwrap();
        assert_eq!(metadata["avatar"]["shape"], "wild");
        assert_eq!(metadata["avatar"]["expression"], "focused");
        assert_eq!(metadata["avatar"]["accessory"], "round_glasses");
        assert_eq!(metadata["source"], "phoenix_agent_provisioning");
        assert_eq!(
            result
                .activities
                .iter()
                .find(|activity| { activity.item == SidebarItemKey::Agent(agent_id.to_string()) })
                .unwrap()
                .status,
            "setting_up"
        );
    }

    #[test]
    fn clone_agent_copies_role_shape_without_private_runtime_state() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let source = store
            .directory_snapshot()
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.profile.agent_id == "coder")
            .unwrap();
        let source_session = source.profile.canonical_session_id.clone().unwrap();
        let mut sessions =
            crate::session::SessionStore::new(crate::config::paths::phoenix_sessions_root());
        let mut private_thread = crate::session::Session::new_main_with_id(
            &source_session,
            "test-model",
            "source system",
        );
        private_thread.push_message(crate::session::Message::User {
            content: "SOURCE-ONLY-PRIVATE-CONTEXT".to_string(),
        });
        sessions.upsert(private_thread);
        sessions.save_one(&source_session).unwrap();

        let result = execute_with(
            &store,
            CompanyDirectoryCommand::CloneAgent {
                source_agent_id: "coder".to_string(),
                preferred_name: Some("Leo Copy Test".to_string()),
            },
        )
        .unwrap();
        let clone_id = match result.mutation.unwrap() {
            CompanyDirectoryMutation::AgentRequested { agent_id } => agent_id,
        };
        let clone = result
            .directory
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == clone_id)
            .unwrap();
        assert_ne!(clone.profile.agent_id, source.profile.agent_id);
        assert_ne!(
            clone.profile.canonical_session_id,
            source.profile.canonical_session_id
        );
        assert_eq!(clone.profile.color, source.profile.color);
        assert_eq!(clone.profile.description, source.profile.description);
        assert_eq!(clone.profile.lifecycle, LifecycleState::Dormant);
        let metadata: serde_json::Value =
            serde_json::from_str(&clone.profile.metadata_json).unwrap();
        assert_eq!(metadata["cloned_from"], "coder");
        assert!(metadata.get("credentials").is_none());
        let clone_session = clone.profile.canonical_session_id.as_deref().unwrap();
        let transcript = crate::session::SessionStore::read_one_from_disk(
            &crate::config::paths::phoenix_sessions_root(),
            clone_session,
        )
        .unwrap();
        assert!(transcript.as_ref().map_or(true, |session| session.messages.iter().all(|message| !matches!(message, crate::session::Message::User { content } if content.contains("SOURCE-ONLY-PRIVATE-CONTEXT")))));
    }

    #[test]
    fn avatar_validation_rejects_unknown_parts_and_unsafe_custom_ids() {
        let mut avatar = AgentAvatarConfig::default();
        avatar.expression = "generic-emoji".to_string();
        assert!(avatar.validate().is_err());
        avatar.expression = "bright".to_string();
        avatar.mode = AgentAvatarMode::Custom;
        avatar.custom_image_id = Some("../../secrets.png".to_string());
        assert!(avatar.validate().is_err());
        avatar.custom_image_id = Some("avatar-deadbeef.png".to_string());
        assert!(avatar.validate().is_ok());
    }

    #[test]
    fn morph_avatar_validates_mark_gradient_and_custom_stops() {
        let parse = |value: serde_json::Value| serde_json::from_value::<AgentAvatarConfig>(value).unwrap();
        parse(serde_json::json!({"mode":"morph","morph_id":"orbit","gradient":"06-ocean"})).validate().unwrap();
        parse(serde_json::json!({"mode":"morph","morph_id":"phoenix","gradient":"custom","gradient_stops":["#112233","#aabbcc","#FFEE00"]})).validate().unwrap();
        assert!(parse(serde_json::json!({"mode":"morph"})).validate().is_err(), "a mark is required");
        assert!(parse(serde_json::json!({"mode":"morph","morph_id":"flame"})).validate().is_err());
        assert!(parse(serde_json::json!({"mode":"morph","morph_id":"orbit","gradient":"neon"})).validate().is_err());
        assert!(parse(serde_json::json!({"mode":"morph","morph_id":"orbit","gradient":"custom"})).validate().is_err());
        assert!(parse(serde_json::json!({"mode":"morph","morph_id":"orbit","gradient":"custom","gradient_stops":["#112233","red","#000000"]})).validate().is_err());
        assert!(parse(serde_json::json!({"mode":"morph","morph_id":"orbit","gradient":"custom","gradient_stops":["#112233","#000000"]})).validate().is_err());
    }

    #[test]
    fn sidekick_avatar_preserves_legacy_serialization_and_optional_model() {
        let legacy = serde_json::json!({
            "mode":"flame", "shape":"wild", "expression":"focused",
            "accessory":"round_glasses"
        });
        let avatar: AgentAvatarConfig = serde_json::from_value(legacy.clone()).unwrap();
        avatar.validate().unwrap();
        assert_eq!(avatar.mode, AgentAvatarMode::Flame);
        assert_eq!(avatar.model_id, None);
        assert_eq!(serde_json::to_value(avatar).unwrap(), legacy);
        let default: AgentAvatarConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(default, AgentAvatarConfig::default());

        for mode in ["flame", "custom", "sidekick"] {
            assert!(serde_json::from_value::<AgentAvatarConfig>(serde_json::json!({
                "mode":mode, "model_id":"unknown-model", "custom_image_id":"avatar-old.png"
            })).is_err(), "unknown model IDs must never silently select Ember");
        }
        let missing: AgentAvatarConfig = serde_json::from_str(r#"{"mode":"sidekick"}"#).unwrap();
        assert!(missing.validate().is_err(), "sidekick mode requires a deliberate model choice");
    }

    #[test]
    fn sidekick_avatar_accepts_procedural_family_controls_without_rewriting_legacy_defaults() {
        for family_id in SIDEKICK_FAMILIES {
            for eye_style in SIDEKICK_EYE_STYLES {
                let value = serde_json::json!({
                    "mode":"sidekick",
                    "family_id":family_id,
                    "fiery":false,
                    "eye_style":eye_style,
                    "expression":"curious"
                });
                let avatar: AgentAvatarConfig = serde_json::from_value(value.clone()).unwrap();
                avatar.validate().unwrap();
                let encoded = serde_json::to_value(&avatar).unwrap();
                assert_eq!(encoded["family_id"], value["family_id"]);
                assert_eq!(encoded["fiery"], value["fiery"]);
                assert_eq!(encoded["expression"], value["expression"]);
                if *eye_style == "round" {
                    assert!(encoded.get("eye_style").is_none(), "default eye style stays omission-compatible");
                } else {
                    assert_eq!(encoded["eye_style"], value["eye_style"]);
                }
                let decoded: AgentAvatarConfig = serde_json::from_value(encoded).unwrap();
                assert_eq!(decoded, avatar);
            }
        }

        let bad_family: AgentAvatarConfig = serde_json::from_value(serde_json::json!({
            "mode":"sidekick", "family_id":"generic-mascot"
        })).unwrap();
        assert!(bad_family.validate().is_err());

        let bad_eyes: AgentAvatarConfig = serde_json::from_value(serde_json::json!({
            "mode":"sidekick", "family_id":"classic_flame", "eye_style":"emoji"
        })).unwrap();
        assert!(bad_eyes.validate().is_err());
    }

    #[test]
    fn sidekick_avatar_roundtrips_models_expressions_and_inactive_accessories() {
        for model_id in ["ember", "cinder", "kiln", "wisp"] {
            for expression in ["bright", "joy", "calm", "curious", "mischief", "sleepy", "focused", "determined"] {
                let value = serde_json::json!({
                    "mode":"sidekick", "model_id":model_id, "shape":"tall",
                    "expression":expression, "accessory":"round_glasses",
                    "hat":"top_hat", "eyewear":"monocle", "extra":"headphones",
                    "custom_image_id":"avatar-existing.png"
                });
                let mut avatar: AgentAvatarConfig = serde_json::from_value(value.clone()).unwrap();
                avatar.validate().unwrap();
                assert_eq!(serde_json::to_value(&avatar).unwrap(), value);
                for mode in [AgentAvatarMode::Flame, AgentAvatarMode::Custom] {
                    avatar.mode = mode;
                    avatar.validate().unwrap();
                    let encoded = serde_json::to_value(&avatar).unwrap();
                    for field in ["model_id", "shape", "expression", "accessory", "hat", "eyewear", "extra", "custom_image_id"] {
                        assert_eq!(encoded[field], value[field], "mode switch discarded {field}");
                    }
                }
            }
        }
        for field in ["hat", "eyewear", "extra"] {
            let mut value = serde_json::json!({"mode":"sidekick", "model_id":"ember"});
            value[field] = serde_json::json!("unsupported");
            assert!(serde_json::from_value::<AgentAvatarConfig>(value).unwrap().validate().is_err());
        }
    }

    #[test]
    fn sidekick_avatar_update_retains_metadata_and_uses_existing_directory_revision() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        store.update_agent_metadata("user", "coder", serde_json::json!({
            "source":"existing-profile", "unrelated":{"retain":true},
            "avatar":{"mode":"custom", "shape":"soft", "expression":"calm",
                "accessory":"spark", "hat":"cap", "eyewear":"square_glasses",
                "extra":"scarf", "custom_image_id":"avatar-kept.png"}
        }).to_string()).unwrap();
        let before = store.directory_snapshot().unwrap();
        let original = before.agents.iter().find(|a| a.profile.agent_id == "coder").unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&original.profile.metadata_json).unwrap();
        metadata["avatar"]["mode"] = serde_json::json!("sidekick");
        metadata["avatar"]["model_id"] = serde_json::json!("wisp");
        let result = execute_with(&store, serde_json::from_value(serde_json::json!({
            "action":"update_agent", "agent_id":"coder", "avatar":metadata["avatar"]
        })).unwrap()).unwrap();
        let updated = result.directory.agents.iter().find(|a| a.profile.agent_id == "coder").unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&updated.profile.metadata_json).unwrap(), metadata);
        assert!(updated.as_of_seq > original.as_of_seq);
        assert_eq!(updated.profile.canonical_session_id, original.profile.canonical_session_id);
        assert_eq!(updated.profile.browser_profile_id, original.profile.browser_profile_id);
        assert_eq!(updated.profile.color, original.profile.color);
        for agent in before.agents.iter().filter(|a| a.profile.agent_id != "coder") {
            assert_eq!(result.directory.agents.iter().find(|a| a.profile.agent_id == agent.profile.agent_id), Some(agent));
        }
    }

    #[test]
    fn sidekick_avatar_invalid_selection_fails_before_any_directory_mutation() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let before = serde_json::to_value(store.directory_snapshot().unwrap()).unwrap();
        for command in [
            serde_json::json!({"action":"update_agent", "agent_id":"coder", "display_name":"Must not change", "avatar":{"mode":"sidekick"}}),
            serde_json::json!({"action":"create_agent", "description":"Coordinate local fixture work", "preferred_name":"NotCreated", "avatar":{"mode":"sidekick"}}),
        ] {
            let error = execute_with(&store, serde_json::from_value(command).unwrap()).unwrap_err();
            assert!(error.to_string().contains("choose a family"));
            assert_eq!(serde_json::to_value(store.directory_snapshot().unwrap()).unwrap(), before);
        }
    }

    #[test]
    fn sidekick_avatar_all_models_persist_through_create_and_update() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        for (model_id, next_model) in [("ember", "cinder"), ("cinder", "kiln"), ("kiln", "wisp"), ("wisp", "ember")] {
            let mut avatar = serde_json::json!({
                "mode":"sidekick", "model_id":model_id, "shape":"wild", "expression":"joy",
                "accessory":"round_glasses", "hat":"beanie", "eyewear":"monocle", "extra":"headphones"
            });
            let created = execute_with(&store, serde_json::from_value(serde_json::json!({
                "action":"create_agent", "description":"Organize isolated fixture work and records.",
                "preferred_name":format!("Sidekick {model_id}"), "color":"#e55732", "avatar":avatar
            })).unwrap()).unwrap();
            let agent_id = match created.mutation.unwrap() {
                CompanyDirectoryMutation::AgentRequested { agent_id } => agent_id,
            };
            let reloaded = CompanyStore::open(root.path().join("company/company.sqlite")).unwrap();
            let first = reloaded.directory_snapshot().unwrap().agents.into_iter()
                .find(|agent| agent.profile.agent_id == agent_id).unwrap();
            let metadata: serde_json::Value = serde_json::from_str(&first.profile.metadata_json).unwrap();
            assert_eq!(metadata["avatar"], avatar, "{model_id} create must persist the full avatar");
            assert_eq!(metadata["source"], "phoenix_agent_provisioning");
            avatar["model_id"] = serde_json::json!(next_model);
            execute_with(&store, serde_json::from_value(serde_json::json!({
                "action":"update_agent", "agent_id":agent_id, "avatar":avatar
            })).unwrap()).unwrap();
            let second = reloaded.directory_snapshot().unwrap().agents.into_iter()
                .find(|agent| agent.profile.agent_id == agent_id).unwrap();
            let saved: serde_json::Value = serde_json::from_str(&second.profile.metadata_json).unwrap();
            assert_eq!(saved["avatar"], avatar, "{next_model} update must persist without dropping legacy slots");
            assert_eq!(saved["source"], metadata["source"]);
            assert!(second.as_of_seq > first.as_of_seq);
            assert_eq!(second.profile.canonical_session_id, first.profile.canonical_session_id);
            assert_eq!(second.profile.browser_profile_id, first.profile.browser_profile_id);
        }
    }

    #[test]
    fn blank_group_name_is_generated_from_the_authoritative_member_roster() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let members = vec!["planner".to_string(), "coder".to_string()];
        let expected = super::super::company::generated_group_name(
            &store.directory_snapshot().unwrap(),
            &members,
        )
        .unwrap();

        let created = execute_with(
            &store,
            CompanyDirectoryCommand::CreateGroup {
                name: "   ".to_string(),
                description: "Coordinate planning and implementation".to_string(),
                color: "#112233".to_string(),
                icon_seed: "generated-room".to_string(),
                members,
                settings: GroupConversationSettings::default(),
                leader_agent_id: None,
            },
        )
        .unwrap();
        assert_eq!(created.directory.groups.len(), 1);
        assert_eq!(created.directory.groups[0].profile.name, expected);
        assert!(!created.directory.groups[0].profile.group_id.is_empty());
    }

    #[test]
    fn legacy_group_settings_decode_with_all_p2_seams_disabled() {
        let settings: GroupConversationSettings =
            serde_json::from_str(r#"{"discussion_rounds":2,"read_full_transcript":false}"#)
                .unwrap();
        assert_eq!(settings.routing_mode, GroupRoutingMode::MentionsOnly);
        assert!(!settings.structured_handoffs);
        assert!(!settings.visible_execution_queues);
        assert!(!settings.group_templates);
        assert!(!settings.granular_retention);
        assert!(settings.validated_json().is_ok());

        let unsupported = [
            GroupConversationSettings {
                routing_mode: GroupRoutingMode::Suggested,
                ..settings.clone()
            },
            GroupConversationSettings {
                routing_mode: GroupRoutingMode::Automatic,
                ..settings.clone()
            },
            GroupConversationSettings {
                structured_handoffs: true,
                ..settings.clone()
            },
            GroupConversationSettings {
                visible_execution_queues: true,
                ..settings.clone()
            },
            GroupConversationSettings {
                group_templates: true,
                ..settings.clone()
            },
            GroupConversationSettings {
                granular_retention: true,
                ..settings
            },
        ];
        assert!(unsupported
            .iter()
            .all(|settings| settings.validated_json().is_err()));
    }

    #[test]
    fn group_control_is_atomic_editable_and_uses_one_endless_thread() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let created = execute_with(
            &store,
            CompanyDirectoryCommand::CreateGroup {
                name: "Launch Crew".to_string(),
                description: "Ship the launch".to_string(),
                color: "#112233".to_string(),
                icon_seed: "launch-eyes".to_string(),
                members: vec!["planner".to_string(), "coder".to_string()],
                settings: GroupConversationSettings::default(),
                leader_agent_id: None,
            },
        )
        .unwrap();
        let group = created
            .directory
            .groups
            .iter()
            .find(|group| group.profile.group_id == "launch-crew")
            .unwrap();
        assert_eq!(
            group.profile.canonical_session_id.as_deref(),
            Some("group-launch-crew")
        );
        assert_eq!(
            created
                .directory
                .members
                .iter()
                .filter(|member| member.group_id == "launch-crew")
                .count(),
            2
        );

        let updated = execute_with(
            &store,
            CompanyDirectoryCommand::UpdateGroup {
                group_id: "launch-crew".to_string(),
                name: Some("Release Room".to_string()),
                description: None,
                color: None,
                icon_seed: None,
                settings: Some(GroupConversationSettings {
                    discussion_rounds: 3,
                    read_full_transcript: false,
                    ..GroupConversationSettings::default()
                }),
                leader_agent_id: None,
            },
        )
        .unwrap();
        let group = updated
            .directory
            .groups
            .iter()
            .find(|group| group.profile.group_id == "launch-crew")
            .unwrap();
        assert_eq!(group.profile.name, "Release Room");
        assert_eq!(
            group.profile.canonical_session_id.as_deref(),
            Some("group-launch-crew")
        );
        assert_eq!(
            serde_json::from_str::<GroupConversationSettings>(&group.profile.metadata_json)
                .unwrap(),
            GroupConversationSettings {
                discussion_rounds: 3,
                read_full_transcript: false,
                ..GroupConversationSettings::default()
            }
        );
    }

    #[test]
    fn prompt_rail_contains_only_user_prompts_and_read_cursor_never_rolls_back() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        let session_id = store.ensure_agent_canonical_session("phoenix").unwrap();
        let mut session =
            crate::session::Session::new_main_with_id(&session_id, "test-model", "system");
        session.push_message(crate::session::Message::User {
            content: "First prompt\nwith details".to_string(),
        });
        session.push_message(crate::session::Message::Assistant {
            content: "not in the rail".to_string(),
        });
        session.push_message(crate::session::Message::User {
            content: "Second prompt".to_string(),
        });
        let mut sessions = crate::session::SessionStore::new(root.path().join("sessions"));
        sessions.upsert(session);
        sessions.save_one(&session_id).unwrap();

        let before = view(&store).unwrap();
        let activity = before
            .activities
            .iter()
            .find(|activity| activity.item == SidebarItemKey::Agent("phoenix".to_string()))
            .unwrap();
        assert_eq!(activity.recent_prompts.len(), 2);
        assert_eq!(
            activity.recent_prompts[0].preview,
            "First prompt with details"
        );
        assert!(activity.unread);

        execute_with(
            &store,
            CompanyDirectoryCommand::MarkRead {
                item: SidebarItemKey::Agent("phoenix".to_string()),
            },
        )
        .unwrap();
        store
            .mark_conversation_read(&SidebarItemKey::Agent("phoenix".to_string()), 1)
            .unwrap();
        let after = view(&store).unwrap();
        let activity = after
            .activities
            .iter()
            .find(|activity| activity.item == SidebarItemKey::Agent("phoenix".to_string()))
            .unwrap();
        assert!(!activity.unread);
        assert_eq!(activity.last_read_revision, activity.transcript_revision);
    }

    #[test]
    fn live_activity_projects_the_actual_provider_route_and_clears_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_founding_team(false).unwrap();
        let agent_id = format!("activity_test_{}", uuid::Uuid::new_v4().simple());
        let session_id = format!("agent-{agent_id}");
        store
            .apply_directory_change(
                "user",
                format!("test-create-{agent_id}"),
                super::super::company_directory::DirectoryChange::AgentUpserted {
                    profile: super::super::company_directory::AgentProfile {
                        agent_id: agent_id.clone(),
                        internal_role: agent_id.clone(),
                        display_name: "Runtime Test".to_string(),
                        role_title: "Runtime Test".to_string(),
                        description: "Tests exact live projection ownership".to_string(),
                        color: "#112233".to_string(),
                        icon_seed: "runtime-test".to_string(),
                        kind: super::super::company_directory::AgentKind::ResponsibilityOwner,
                        lifecycle: LifecycleState::Active,
                        pinned: false,
                        sort_order: 99,
                        canonical_session_id: Some(session_id.clone()),
                        browser_profile_id: format!("agent-{agent_id}"),
                        metadata_json: "{}".to_string(),
                    },
                },
            )
            .unwrap();
        let active = super::super::company_activity::begin(
            &session_id,
            &agent_id,
            "Exploring the codebase",
            "fallback-provider",
            "actual-model",
        );
        let during = view(&store).unwrap();
        let phoenix = during
            .activities
            .iter()
            .find(|activity| activity.item == SidebarItemKey::Agent(agent_id.clone()))
            .unwrap();
        assert_eq!(phoenix.status, "working");
        assert_eq!(
            phoenix.activity_label.as_deref(),
            Some("Exploring the codebase")
        );
        assert_eq!(phoenix.provider_id.as_deref(), Some("fallback-provider"));
        assert_eq!(phoenix.model.as_deref(), Some("actual-model"));

        drop(active);
        assert!(!super::super::company_activity::snapshot()
            .iter()
            .any(|activity| {
                activity.session_id == session_id && activity.internal_role == agent_id
            }));
    }

    #[test]
    fn group_activity_names_every_current_coworker_without_a_numeric_badge() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        execute_with(
            &store,
            CompanyDirectoryCommand::CreateGroup {
                name: "Launch".to_string(),
                description: "Launch together".to_string(),
                color: "#112233".to_string(),
                icon_seed: "launch".to_string(),
                members: vec!["coder".to_string(), "researcher".to_string()],
                settings: GroupConversationSettings::default(),
                leader_agent_id: None,
            },
        )
        .unwrap();
        let coder = super::super::company_activity::begin(
            "group-launch",
            "coder",
            "Building release",
            "provider-a",
            "model-a",
        );
        let researcher = super::super::company_activity::begin(
            "group-launch",
            "researcher",
            "Checking evidence",
            "provider-b",
            "model-b",
        );
        let current = view(&store).unwrap();
        let group = current
            .activities
            .iter()
            .find(|activity| activity.item == SidebarItemKey::Group("launch".to_string()))
            .unwrap();
        assert_eq!(group.status, "working");
        assert_eq!(
            group.active_agent_ids,
            vec!["coder".to_string(), "researcher".to_string()]
        );
        assert_eq!(group.transcript_revision, 0);
        assert!(!group.unread);
        for agent_id in ["coder", "researcher"] {
            let direct = current
                .activities
                .iter()
                .find(|activity| activity.item == SidebarItemKey::Agent(agent_id.to_string()))
                .unwrap();
            assert_eq!(
                direct.status, "idle",
                "group work must shine on the group row, not {agent_id}'s direct row"
            );
        }
        drop((coder, researcher));
    }

    #[test]
    fn invalid_group_settings_fail_without_publishing_a_group() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        assert!(execute_with(
            &store,
            CompanyDirectoryCommand::CreateGroup {
                name: "Bad".to_string(),
                description: "Bad".to_string(),
                color: "#112233".to_string(),
                icon_seed: "bad".to_string(),
                members: vec!["coder".to_string(), "planner".to_string()],
                settings: GroupConversationSettings {
                    discussion_rounds: 9,
                    read_full_transcript: true,
                    ..GroupConversationSettings::default()
                },
                leader_agent_id: None,
            },
        )
        .is_err());
        assert!(store.directory_snapshot().unwrap().groups.is_empty());
    }

    #[test]
    fn relationship_updates_are_validated_persisted_and_injected_into_runtime_identity() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();

        let updated = execute_with(
            &store,
            CompanyDirectoryCommand::UpdateRelationship {
                from_agent_id: "coder".to_string(),
                to_agent_id: "frontend".to_string(),
                relationship: "asks for interaction judgment before shipping UI".to_string(),
                trust_level: "trusted".to_string(),
                policy_json: None,
            },
        )
        .unwrap();
        let relationship = updated
            .directory
            .relationships
            .iter()
            .find(|record| record.from_agent_id == "coder" && record.to_agent_id == "frontend")
            .unwrap();
        assert_eq!(relationship.trust_level, "trusted");
        assert_eq!(
            relationship.relationship,
            "asks for interaction judgment before shipping UI"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&relationship.policy_json).unwrap(),
            serde_json::json!({
                "shares_minimum_needed": true,
                "requires_task_scope": true
            })
        );
        let identity =
            super::super::company_directory::runtime_identity_block(&updated.directory, "coder")
                .unwrap();
        assert!(identity.contains("Iris (frontend) · trust `trusted`"));
        assert!(identity.contains("asks for interaction judgment before shipping UI"));
        assert!(identity.contains("Ownership means execution, not routing"));
        assert!(identity.contains("An ordinary role-owned request does not need a `work` record"));

        for invalid in [
            CompanyDirectoryCommand::UpdateRelationship {
                from_agent_id: "coder".to_string(),
                to_agent_id: "coder".to_string(),
                relationship: "self".to_string(),
                trust_level: "trusted".to_string(),
                policy_json: None,
            },
            CompanyDirectoryCommand::UpdateRelationship {
                from_agent_id: "coder".to_string(),
                to_agent_id: "missing".to_string(),
                relationship: "unknown".to_string(),
                trust_level: "trusted".to_string(),
                policy_json: None,
            },
            CompanyDirectoryCommand::UpdateRelationship {
                from_agent_id: "coder".to_string(),
                to_agent_id: "frontend".to_string(),
                relationship: "bad trust".to_string(),
                trust_level: "unbounded".to_string(),
                policy_json: None,
            },
        ] {
            assert!(execute_with(&store, invalid).is_err());
        }
    }

    #[test]
    fn group_display_names_do_not_have_to_be_ids() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let store = store(root.path());
        store.ensure_full_catalog_team().unwrap();
        let created = execute_with(
            &store,
            CompanyDirectoryCommand::CreateGroup {
                name: "Design / QA 🚀".to_string(),
                description: "Ship the interface and the checks together.".to_string(),
                color: "#E06C52".to_string(),
                icon_seed: "group-Design / QA 🚀".to_string(),
                members: vec!["frontend".to_string(), "coder".to_string()],
                settings: GroupConversationSettings::default(),
                leader_agent_id: None,
            },
        )
        .unwrap();
        let group = created
            .directory
            .groups
            .iter()
            .find(|group| group.profile.group_id == "design-qa")
            .unwrap();
        assert_eq!(group.profile.name, "Design / QA 🚀");
        assert_eq!(group.profile.icon_seed, "design-qa");
        assert_eq!(
            group.profile.canonical_session_id.as_deref(),
            Some("group-design-qa")
        );
    }

    #[test]
    fn group_ids_are_stable_unique_and_path_safe() {
        let mut snapshot = DirectorySnapshot::default();
        assert_eq!(
            unique_group_id(&snapshot, "Design / QA 🚀").unwrap(),
            "design-qa"
        );
        snapshot
            .groups
            .push(super::super::company_directory::GroupRecord {
                profile: GroupProfile {
                    group_id: "design-qa".to_string(),
                    name: "one".to_string(),
                    description: "one".to_string(),
                    color: "#112233".to_string(),
                    icon_seed: "one".to_string(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 0,
                    canonical_session_id: None,
                    metadata_json: "{}".to_string(),
                    leader_agent_id: None,
                },
                archived_at: None,
                delete_after: None,
                created_at: "now".to_string(),
                updated_at: "now".to_string(),
                as_of_seq: 0,
            });
        assert_eq!(
            unique_group_id(&snapshot, "Design / QA 🚀").unwrap(),
            "design-qa-2"
        );
    }

    #[test]
    fn group_members_default_to_full_history() {
        let member: GroupMemberInput = serde_json::from_value(serde_json::json!({
            "agent_id": "coder"
        }))
        .unwrap();
        assert_eq!(member.history_access, HistoryAccess::Full);
    }
}
