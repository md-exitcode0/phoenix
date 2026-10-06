//! Durable company directory: coworkers, responsibilities, groups, and threads.
//!
//! Tools are universal company infrastructure.  An agent is differentiated by
//! durable responsibility, expertise, memory, operating identity, and working
//! relationships — never by an arbitrary role-only tool silo.  This module is
//! a projection of the canonical company event stream in `company.sqlite`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

const MAX_ID_BYTES: usize = 96;
const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_DIRECTORY_ROWS: usize = 4_096;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    CraftSpecialist,
    ResponsibilityOwner,
}

impl AgentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::CraftSpecialist => "craft_specialist",
            Self::ResponsibilityOwner => "responsibility_owner",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "craft_specialist" => Ok(Self::CraftSpecialist),
            "responsibility_owner" => Ok(Self::ResponsibilityOwner),
            _ => anyhow::bail!("unknown agent kind `{value}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Active,
    Dormant,
    Archived,
    PendingDeletion,
}

impl LifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Dormant => "dormant",
            Self::Archived => "archived",
            Self::PendingDeletion => "pending_deletion",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "dormant" => Ok(Self::Dormant),
            "archived" => Ok(Self::Archived),
            "pending_deletion" => Ok(Self::PendingDeletion),
            _ => anyhow::bail!("unknown lifecycle state `{value}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAccess {
    Full,
    FromJoin,
}

impl HistoryAccess {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::FromJoin => "from_join",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub agent_id: String,
    /// Immutable runtime/migration alias (`coder`, `researcher`, custom slug).
    pub internal_role: String,
    pub display_name: String,
    pub role_title: String,
    pub description: String,
    pub color: String,
    pub icon_seed: String,
    pub kind: AgentKind,
    pub lifecycle: LifecycleState,
    pub pinned: bool,
    pub sort_order: i64,
    pub canonical_session_id: Option<String>,
    pub browser_profile_id: String,
    #[serde(default)]
    pub metadata_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    #[serde(flatten)]
    pub profile: AgentProfile,
    pub archived_at: Option<String>,
    pub delete_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupProfile {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub icon_seed: String,
    pub lifecycle: LifecycleState,
    pub pinned: bool,
    pub sort_order: i64,
    pub canonical_session_id: Option<String>,
    #[serde(default)]
    pub metadata_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupRecord {
    #[serde(flatten)]
    pub profile: GroupProfile,
    pub archived_at: Option<String>,
    pub delete_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMemberRecord {
    pub group_id: String,
    pub agent_id: String,
    pub member_role: String,
    pub history_access: HistoryAccess,
    /// Exact canonical group-message offset visible to a FromJoin member.
    /// Full-history members always use zero.
    #[serde(default)]
    pub history_start_message_index: usize,
    /// Stable user-controlled order inside this group. This is independent of
    /// the coworker's position in the company sidebar.
    #[serde(default)]
    pub sort_order: i64,
    pub joined_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponsibilityRecord {
    pub responsibility_id: String,
    pub agent_id: String,
    pub title: String,
    pub scope: String,
    pub success_criteria: Vec<String>,
    pub approval_policy_json: String,
    pub escalation_policy_json: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipRecord {
    pub from_agent_id: String,
    pub to_agent_id: String,
    pub relationship: String,
    pub trust_level: String,
    pub policy_json: String,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationSourceRecord {
    pub session_id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub source_kind: String,
    pub canonical: bool,
    pub linked_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutsideCallGrantRecord {
    pub group_id: String,
    pub agent_id: String,
    pub granted: bool,
    pub updated_at: String,
    pub as_of_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DirectoryChange {
    AgentUpserted {
        profile: AgentProfile,
    },
    AgentLifecycleSet {
        agent_id: String,
        lifecycle: LifecycleState,
        delete_after: Option<String>,
    },
    AgentDeletionScheduled {
        agent_id: String,
        confirmed_name: String,
    },
    AgentPurged {
        agent_id: String,
    },
    GroupUpserted {
        profile: GroupProfile,
    },
    GroupLifecycleSet {
        group_id: String,
        lifecycle: LifecycleState,
        delete_after: Option<String>,
    },
    GroupDeletionScheduled {
        group_id: String,
        confirmed_name: String,
    },
    GroupPurged {
        group_id: String,
    },
    GroupMemberSet {
        group_id: String,
        agent_id: String,
        member_role: String,
        history_access: HistoryAccess,
        #[serde(default)]
        history_start_message_index: usize,
        #[serde(default)]
        sort_order: i64,
        present: bool,
    },
    ResponsibilityUpserted {
        responsibility_id: String,
        agent_id: String,
        title: String,
        scope: String,
        success_criteria: Vec<String>,
        approval_policy_json: String,
        escalation_policy_json: String,
        status: String,
    },
    RelationshipUpserted {
        from_agent_id: String,
        to_agent_id: String,
        relationship: String,
        trust_level: String,
        policy_json: String,
    },
    OutsideCallGrantSet {
        group_id: String,
        agent_id: String,
        granted: bool,
    },
    ConversationSourceLinked {
        session_id: String,
        owner_kind: String,
        owner_id: String,
        source_kind: String,
        canonical: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub as_of_seq: i64,
    pub agents: Vec<AgentRecord>,
    pub groups: Vec<GroupRecord>,
    pub members: Vec<GroupMemberRecord>,
    pub responsibilities: Vec<ResponsibilityRecord>,
    pub relationships: Vec<RelationshipRecord>,
    #[serde(default)]
    pub outside_call_grants: Vec<OutsideCallGrantRecord>,
    pub conversation_sources: Vec<ConversationSourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredSessionSource {
    pub session_id: String,
    pub owner_agent_id: String,
    pub source_kind: String,
    pub canonical: bool,
    pub modified_unix_millis: u128,
}

/// Discover every usable historical session without rewriting or concatenating
/// its transcript. The newest unscoped session becomes the canonical endless
/// thread for each coworker; all older sources remain searchable and available
/// to librarian/indexer context. Surf/browser history is retained under
/// Phoenix because Surf is no longer a visible coworker.
pub fn discover_session_history(root: &std::path::Path) -> Result<Vec<DiscoveredSessionSource>> {
    discover_session_history_excluding(root, |_| false)
}

pub(crate) fn discover_session_history_excluding(
    root: &std::path::Path, excluded: impl Fn(&str) -> bool,
) -> Result<Vec<DiscoveredSessionSource>> {
    use crate::session::SessionKind;

    let mut store = crate::session::SessionStore::new(root);
    store.load_from_disk()?;
    let mut discovered = Vec::new();
    for session in store.all() {
        if session.id.starts_with("bench-") || session.id.starts_with("test-") || excluded(&session.id) {
            continue;
        }
        let (owner_agent_id, source_kind) = match &session.kind {
            SessionKind::Main => ("phoenix".to_string(), "legacy_main".to_string()),
            SessionKind::SubAgent(agent) => {
                let role = crate::runtime::delegation::specialist_label(*agent);
                if role == "browser" {
                    ("phoenix".to_string(), "legacy_surf".to_string())
                } else {
                    (role.to_string(), "legacy_specialist".to_string())
                }
            }
        };
        let modified_unix_millis = std::fs::metadata(store.session_path(&session.id))
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        discovered.push(DiscoveredSessionSource {
            session_id: session.id.clone(),
            owner_agent_id,
            source_kind,
            canonical: false,
            modified_unix_millis,
        });
    }

    let mut newest: std::collections::HashMap<String, (usize, u8, bool, u128)> =
        std::collections::HashMap::new();
    for (index, source) in discovered.iter().enumerate() {
        // Leaf/scoped sessions are parallel work branches, not the coworker's
        // stable conversational home. They remain linked as history.
        let unscoped = !source.session_id.contains("--");
        // A retired Surf task must never replace Phoenix's own main thread,
        // even when the browser task happened more recently.
        let owner_priority = u8::from(source.source_kind != "legacy_surf");
        let candidate = (index, owner_priority, unscoped, source.modified_unix_millis);
        match newest.get(&source.owner_agent_id) {
            Some((_, current_priority, current_unscoped, current_mtime))
                if (*current_priority, *current_unscoped, *current_mtime)
                    >= (owner_priority, unscoped, source.modified_unix_millis) => {}
            _ => {
                newest.insert(source.owner_agent_id.clone(), candidate);
            }
        }
    }
    for (index, _, _, _) in newest.values() {
        if let Some(source) = discovered.get_mut(*index) {
            source.canonical = true;
        }
    }
    discovered.sort_by(|left, right| {
        left.owner_agent_id
            .cmp(&right.owner_agent_id)
            .then_with(|| right.canonical.cmp(&left.canonical))
            .then_with(|| right.modified_unix_millis.cmp(&left.modified_unix_millis))
    });
    Ok(discovered)
}

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS company_agents(
            agent_id TEXT PRIMARY KEY,
            internal_role TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            role_title TEXT NOT NULL,
            description TEXT NOT NULL,
            color TEXT NOT NULL,
            icon_seed TEXT NOT NULL,
            agent_kind TEXT NOT NULL,
            lifecycle TEXT NOT NULL,
            pinned INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            canonical_session_id TEXT,
            browser_profile_id TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            archived_at TEXT,
            delete_after TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_agents_lifecycle_order
            ON company_agents(lifecycle,pinned DESC,sort_order,display_name);
        CREATE UNIQUE INDEX IF NOT EXISTS company_agents_one_canonical_owner
            ON company_agents(canonical_session_id)
            WHERE canonical_session_id IS NOT NULL;
        CREATE TABLE IF NOT EXISTS company_groups(
            group_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL,
            color TEXT NOT NULL,
            icon_seed TEXT NOT NULL,
            lifecycle TEXT NOT NULL,
            pinned INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            canonical_session_id TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            archived_at TEXT,
            delete_after TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_groups_lifecycle_order
            ON company_groups(lifecycle,pinned DESC,sort_order,name);
        CREATE UNIQUE INDEX IF NOT EXISTS company_groups_one_canonical_owner
            ON company_groups(canonical_session_id)
            WHERE canonical_session_id IS NOT NULL;
        CREATE TABLE IF NOT EXISTS company_group_members(
            group_id TEXT NOT NULL REFERENCES company_groups(group_id) ON DELETE CASCADE,
            agent_id TEXT NOT NULL REFERENCES company_agents(agent_id) ON DELETE RESTRICT,
            member_role TEXT NOT NULL,
            history_access TEXT NOT NULL,
            history_start_message_index INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            joined_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL,
            PRIMARY KEY(group_id,agent_id)
        );
        CREATE TABLE IF NOT EXISTS company_responsibilities(
            responsibility_id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES company_agents(agent_id) ON DELETE RESTRICT,
            title TEXT NOT NULL,
            scope TEXT NOT NULL,
            success_criteria_json TEXT NOT NULL,
            approval_policy_json TEXT NOT NULL,
            escalation_policy_json TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_responsibilities_agent_status
            ON company_responsibilities(agent_id,status);
        CREATE TABLE IF NOT EXISTS company_relationships(
            from_agent_id TEXT NOT NULL REFERENCES company_agents(agent_id) ON DELETE CASCADE,
            to_agent_id TEXT NOT NULL REFERENCES company_agents(agent_id) ON DELETE CASCADE,
            relationship TEXT NOT NULL,
            trust_level TEXT NOT NULL,
            policy_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL,
            PRIMARY KEY(from_agent_id,to_agent_id)
        );
        CREATE TABLE IF NOT EXISTS company_outside_call_grants(
            group_id TEXT NOT NULL REFERENCES company_groups(group_id) ON DELETE CASCADE,
            agent_id TEXT NOT NULL REFERENCES company_agents(agent_id) ON DELETE CASCADE,
            granted INTEGER NOT NULL,
            updated_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL,
            PRIMARY KEY(group_id,agent_id)
        );
        CREATE TABLE IF NOT EXISTS company_conversation_sources(
            session_id TEXT PRIMARY KEY,
            owner_kind TEXT NOT NULL,
            owner_id TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            canonical INTEGER NOT NULL DEFAULT 0,
            linked_at TEXT NOT NULL,
            as_of_seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_conversation_owner
            ON company_conversation_sources(owner_kind,owner_id,canonical DESC,linked_at DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS company_conversation_one_canonical_source
            ON company_conversation_sources(owner_kind,owner_id)
            WHERE canonical=1;
        CREATE TABLE IF NOT EXISTS company_conversation_reads(
            owner_kind TEXT NOT NULL,
            owner_id TEXT NOT NULL,
            last_read_revision INTEGER NOT NULL DEFAULT 0,
            read_at TEXT NOT NULL,
            PRIMARY KEY(owner_kind,owner_id)
        );",
    )?;
    ensure_column(
        connection,
        "company_group_members",
        "sort_order",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "company_group_members",
        "history_start_message_index",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    anyhow::ensure!(
        table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && column
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "unsafe migration identifier"
    );
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(());
        }
    }
    connection.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
    ))?;
    Ok(())
}

pub(crate) fn project(
    tx: &Transaction<'_>,
    seq: i64,
    at: &DateTime<Utc>,
    change: &DirectoryChange,
) -> Result<()> {
    let timestamp = at.to_rfc3339();
    validate_change(change)?;
    match change {
        DirectoryChange::AgentUpserted { profile } => {
            if let Some(session_id) = profile.canonical_session_id.as_deref() {
                ensure_canonical_session_available(tx, session_id, "agent", &profile.agent_id)?;
            }
            tx.execute(
                "INSERT INTO company_agents(
                    agent_id,internal_role,display_name,role_title,description,color,icon_seed,
                    agent_kind,lifecycle,pinned,sort_order,canonical_session_id,browser_profile_id,
                    metadata_json,created_at,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?16)
                 ON CONFLICT(agent_id) DO UPDATE SET
                    internal_role=excluded.internal_role,display_name=excluded.display_name,
                    role_title=excluded.role_title,description=excluded.description,
                    color=excluded.color,icon_seed=excluded.icon_seed,agent_kind=excluded.agent_kind,
                    lifecycle=excluded.lifecycle,pinned=excluded.pinned,sort_order=excluded.sort_order,
                    canonical_session_id=COALESCE(excluded.canonical_session_id,company_agents.canonical_session_id),
                    browser_profile_id=excluded.browser_profile_id,metadata_json=excluded.metadata_json,
                    updated_at=excluded.updated_at,as_of_seq=excluded.as_of_seq",
                params![
                    profile.agent_id,
                    profile.internal_role,
                    profile.display_name,
                    profile.role_title,
                    profile.description,
                    profile.color,
                    profile.icon_seed,
                    profile.kind.as_str(),
                    profile.lifecycle.as_str(),
                    profile.pinned as i64,
                    profile.sort_order,
                    profile.canonical_session_id,
                    profile.browser_profile_id,
                    normalized_json(&profile.metadata_json),
                    timestamp,
                    seq,
                ],
            )?;
        }
        DirectoryChange::AgentLifecycleSet {
            agent_id,
            lifecycle,
            delete_after,
        } => {
            anyhow::ensure!(
                agent_id != "phoenix" || *lifecycle == LifecycleState::Active,
                "Phoenix is the required company coordinator and cannot be archived or disabled"
            );
            validate_lifecycle_deadline(*lifecycle, delete_after.as_deref())?;
            let archived_at = matches!(
                lifecycle,
                LifecycleState::Archived | LifecycleState::PendingDeletion
            )
            .then_some(timestamp.as_str());
            let changed = tx.execute(
                "UPDATE company_agents SET lifecycle=?1,archived_at=?2,delete_after=?3,
                    updated_at=?4,as_of_seq=?5 WHERE agent_id=?6",
                params![
                    lifecycle.as_str(),
                    archived_at,
                    delete_after,
                    timestamp,
                    seq,
                    agent_id
                ],
            )?;
            anyhow::ensure!(changed == 1, "agent `{agent_id}` does not exist");
        }
        DirectoryChange::AgentDeletionScheduled {
            agent_id,
            confirmed_name,
        } => {
            anyhow::ensure!(
                agent_id != "phoenix",
                "Phoenix is the required company coordinator and cannot be deleted"
            );
            let current_name = tx
                .query_row(
                    "SELECT display_name FROM company_agents WHERE agent_id=?1",
                    [agent_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .with_context(|| format!("agent `{agent_id}` does not exist"))?;
            anyhow::ensure!(
                &current_name == confirmed_name,
                "agent name confirmation does not match"
            );
            let delete_after = (*at + chrono::Duration::days(30)).to_rfc3339();
            tx.execute(
                "UPDATE company_agents SET lifecycle='pending_deletion',archived_at=?1,
                    delete_after=?2,updated_at=?1,as_of_seq=?3 WHERE agent_id=?4",
                params![timestamp, delete_after, seq, agent_id],
            )?;
        }
        DirectoryChange::AgentPurged { agent_id } => {
            anyhow::ensure!(
                agent_id != "phoenix",
                "Phoenix is the required company coordinator and cannot be deleted"
            );
            let delete_after = tx
                .query_row(
                    "SELECT delete_after FROM company_agents
                     WHERE agent_id=?1 AND lifecycle='pending_deletion'",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .with_context(|| format!("agent `{agent_id}` is not pending deletion"))?;
            let deadline = DateTime::parse_from_rfc3339(&delete_after)?;
            anyhow::ensure!(deadline <= *at, "agent recovery period has not expired");
            tx.execute(
                "DELETE FROM company_group_members WHERE agent_id=?1",
                [agent_id],
            )?;
            tx.execute(
                "DELETE FROM company_responsibilities WHERE agent_id=?1",
                [agent_id],
            )?;
            tx.execute(
                "DELETE FROM company_relationships WHERE from_agent_id=?1 OR to_agent_id=?1",
                [agent_id],
            )?;
            tx.execute(
                "DELETE FROM company_outside_call_grants WHERE agent_id=?1",
                [agent_id],
            )?;
            tx.execute(
                "DELETE FROM company_conversation_sources WHERE owner_kind='agent' AND owner_id=?1",
                [agent_id],
            )?;
            tx.execute(
                "DELETE FROM company_conversation_reads WHERE owner_kind='agent' AND owner_id=?1",
                [agent_id],
            )?;
            let changed = tx.execute("DELETE FROM company_agents WHERE agent_id=?1", [agent_id])?;
            anyhow::ensure!(changed == 1, "agent `{agent_id}` does not exist");
        }
        DirectoryChange::GroupUpserted { profile } => {
            if let Some(session_id) = profile.canonical_session_id.as_deref() {
                ensure_canonical_session_available(tx, session_id, "group", &profile.group_id)?;
            }
            tx.execute(
                "INSERT INTO company_groups(
                    group_id,name,description,color,icon_seed,lifecycle,pinned,sort_order,
                    canonical_session_id,metadata_json,created_at,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11,?12)
                 ON CONFLICT(group_id) DO UPDATE SET
                    name=excluded.name,description=excluded.description,color=excluded.color,
                    icon_seed=excluded.icon_seed,lifecycle=excluded.lifecycle,pinned=excluded.pinned,
                    sort_order=excluded.sort_order,
                    canonical_session_id=COALESCE(excluded.canonical_session_id,company_groups.canonical_session_id),
                    metadata_json=excluded.metadata_json,updated_at=excluded.updated_at,
                    as_of_seq=excluded.as_of_seq",
                params![
                    profile.group_id,
                    profile.name,
                    profile.description,
                    profile.color,
                    profile.icon_seed,
                    profile.lifecycle.as_str(),
                    profile.pinned as i64,
                    profile.sort_order,
                    profile.canonical_session_id,
                    normalized_json(&profile.metadata_json),
                    timestamp,
                    seq,
                ],
            )?;
        }
        DirectoryChange::GroupLifecycleSet {
            group_id,
            lifecycle,
            delete_after,
        } => {
            validate_lifecycle_deadline(*lifecycle, delete_after.as_deref())?;
            let archived_at = matches!(
                lifecycle,
                LifecycleState::Archived | LifecycleState::PendingDeletion
            )
            .then_some(timestamp.as_str());
            let changed = tx.execute(
                "UPDATE company_groups SET lifecycle=?1,archived_at=?2,delete_after=?3,
                    updated_at=?4,as_of_seq=?5 WHERE group_id=?6",
                params![
                    lifecycle.as_str(),
                    archived_at,
                    delete_after,
                    timestamp,
                    seq,
                    group_id
                ],
            )?;
            anyhow::ensure!(changed == 1, "group `{group_id}` does not exist");
        }
        DirectoryChange::GroupDeletionScheduled {
            group_id,
            confirmed_name,
        } => {
            let current_name = tx
                .query_row(
                    "SELECT name FROM company_groups WHERE group_id=?1",
                    [group_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .with_context(|| format!("group `{group_id}` does not exist"))?;
            anyhow::ensure!(
                &current_name == confirmed_name,
                "group name confirmation does not match"
            );
            let delete_after = (*at + chrono::Duration::days(30)).to_rfc3339();
            tx.execute(
                "UPDATE company_groups SET lifecycle='pending_deletion',archived_at=?1,
                    delete_after=?2,updated_at=?1,as_of_seq=?3 WHERE group_id=?4",
                params![timestamp, delete_after, seq, group_id],
            )?;
        }
        DirectoryChange::GroupPurged { group_id } => {
            let delete_after = tx
                .query_row(
                    "SELECT delete_after FROM company_groups
                     WHERE group_id=?1 AND lifecycle='pending_deletion'",
                    [group_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .with_context(|| format!("group `{group_id}` is not pending deletion"))?;
            let deadline = DateTime::parse_from_rfc3339(&delete_after)?;
            anyhow::ensure!(deadline <= *at, "group recovery period has not expired");
            tx.execute(
                "DELETE FROM company_conversation_sources WHERE owner_kind='group' AND owner_id=?1",
                [group_id],
            )?;
            tx.execute(
                "DELETE FROM company_conversation_reads WHERE owner_kind='group' AND owner_id=?1",
                [group_id],
            )?;
            let changed = tx.execute("DELETE FROM company_groups WHERE group_id=?1", [group_id])?;
            anyhow::ensure!(changed == 1, "group `{group_id}` does not exist");
        }
        DirectoryChange::GroupMemberSet {
            group_id,
            agent_id,
            member_role,
            history_access,
            history_start_message_index,
            sort_order,
            present,
        } => {
            if *present {
                tx.execute(
                    "INSERT INTO company_group_members(
                        group_id,agent_id,member_role,history_access,history_start_message_index,
                        sort_order,joined_at,as_of_seq)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT(group_id,agent_id) DO UPDATE SET
                        member_role=excluded.member_role,history_access=excluded.history_access,
                        history_start_message_index=excluded.history_start_message_index,
                        sort_order=excluded.sort_order,
                        as_of_seq=excluded.as_of_seq",
                    params![
                        group_id,
                        agent_id,
                        member_role,
                        history_access.as_str(),
                        i64::try_from(*history_start_message_index)
                            .context("group history offset is too large")?,
                        sort_order,
                        timestamp,
                        seq
                    ],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM company_group_members WHERE group_id=?1 AND agent_id=?2",
                    params![group_id, agent_id],
                )?;
            }
        }
        DirectoryChange::ResponsibilityUpserted {
            responsibility_id,
            agent_id,
            title,
            scope,
            success_criteria,
            approval_policy_json,
            escalation_policy_json,
            status,
        } => {
            tx.execute(
                "INSERT INTO company_responsibilities(
                    responsibility_id,agent_id,title,scope,success_criteria_json,
                    approval_policy_json,escalation_policy_json,status,created_at,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)
                 ON CONFLICT(responsibility_id) DO UPDATE SET
                    agent_id=excluded.agent_id,title=excluded.title,scope=excluded.scope,
                    success_criteria_json=excluded.success_criteria_json,
                    approval_policy_json=excluded.approval_policy_json,
                    escalation_policy_json=excluded.escalation_policy_json,status=excluded.status,
                    updated_at=excluded.updated_at,as_of_seq=excluded.as_of_seq",
                params![
                    responsibility_id,
                    agent_id,
                    title,
                    scope,
                    serde_json::to_string(success_criteria)?,
                    normalized_json(approval_policy_json),
                    normalized_json(escalation_policy_json),
                    status,
                    timestamp,
                    seq,
                ],
            )?;
        }
        DirectoryChange::RelationshipUpserted {
            from_agent_id,
            to_agent_id,
            relationship,
            trust_level,
            policy_json,
        } => {
            anyhow::ensure!(
                from_agent_id != to_agent_id,
                "an agent cannot have a relationship with itself"
            );
            tx.execute(
                "INSERT INTO company_relationships(
                    from_agent_id,to_agent_id,relationship,trust_level,policy_json,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(from_agent_id,to_agent_id) DO UPDATE SET
                    relationship=excluded.relationship,trust_level=excluded.trust_level,
                    policy_json=excluded.policy_json,updated_at=excluded.updated_at,
                    as_of_seq=excluded.as_of_seq",
                params![
                    from_agent_id,
                    to_agent_id,
                    relationship,
                    trust_level,
                    normalized_json(policy_json),
                    timestamp,
                    seq,
                ],
            )?;
        }
        DirectoryChange::OutsideCallGrantSet {
            group_id,
            agent_id,
            granted,
        } => {
            tx.execute(
                "INSERT INTO company_outside_call_grants(
                    group_id,agent_id,granted,updated_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(group_id,agent_id) DO UPDATE SET
                    granted=excluded.granted,updated_at=excluded.updated_at,as_of_seq=excluded.as_of_seq",
                params![group_id, agent_id, *granted as i64, timestamp, seq],
            )?;
        }
        DirectoryChange::ConversationSourceLinked {
            session_id,
            owner_kind,
            owner_id,
            source_kind,
            canonical,
        } => {
            if *canonical {
                ensure_canonical_session_available(tx, session_id, owner_kind, owner_id)?;
                if let Some((existing_kind, existing_id)) = tx
                    .query_row(
                        "SELECT owner_kind,owner_id FROM company_conversation_sources WHERE session_id=?1",
                        [session_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()?
                {
                    anyhow::ensure!(
                        existing_kind == *owner_kind && existing_id == *owner_id,
                        "canonical conversation `{session_id}` already belongs to {existing_kind} `{existing_id}`"
                    );
                }
                tx.execute(
                    "UPDATE company_conversation_sources SET canonical=0
                     WHERE owner_kind=?1 AND owner_id=?2",
                    params![owner_kind, owner_id],
                )?;
            }
            tx.execute(
                "INSERT INTO company_conversation_sources(
                    session_id,owner_kind,owner_id,source_kind,canonical,linked_at,as_of_seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(session_id) DO UPDATE SET
                    owner_kind=excluded.owner_kind,owner_id=excluded.owner_id,
                    source_kind=excluded.source_kind,canonical=excluded.canonical,
                    linked_at=excluded.linked_at,as_of_seq=excluded.as_of_seq",
                params![
                    session_id,
                    owner_kind,
                    owner_id,
                    source_kind,
                    *canonical as i64,
                    timestamp,
                    seq
                ],
            )?;
            if *canonical {
                let sql = match owner_kind.as_str() {
                    "agent" => "UPDATE company_agents SET canonical_session_id=?1,updated_at=?2,as_of_seq=?3 WHERE agent_id=?4",
                    "group" => "UPDATE company_groups SET canonical_session_id=?1,updated_at=?2,as_of_seq=?3 WHERE group_id=?4",
                    _ => unreachable!("validated owner kind"),
                };
                let changed = tx.execute(sql, params![session_id, timestamp, seq, owner_id])?;
                anyhow::ensure!(
                    changed == 1,
                    "conversation owner `{owner_id}` does not exist"
                );
            }
        }
    }
    Ok(())
}

fn ensure_canonical_session_available(
    tx: &Transaction<'_>,
    session_id: &str,
    owner_kind: &str,
    owner_id: &str,
) -> Result<()> {
    if let Some(agent_id) = tx
        .query_row(
            "SELECT agent_id FROM company_agents WHERE canonical_session_id=?1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        anyhow::ensure!(
            owner_kind == "agent" && agent_id == owner_id,
            "canonical conversation `{session_id}` already belongs to agent `{agent_id}`"
        );
    }
    if let Some(group_id) = tx
        .query_row(
            "SELECT group_id FROM company_groups WHERE canonical_session_id=?1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        anyhow::ensure!(
            owner_kind == "group" && group_id == owner_id,
            "canonical conversation `{session_id}` already belongs to group `{group_id}`"
        );
    }
    Ok(())
}

pub(crate) fn snapshot(connection: &Connection) -> Result<DirectorySnapshot> {
    let as_of_seq = connection.query_row(
        "SELECT COALESCE(MAX(company_seq),0) FROM company_events",
        [],
        |row| row.get(0),
    )?;
    let agents = collect_rows(
        connection,
        "SELECT agent_id,internal_role,display_name,role_title,description,color,icon_seed,
                agent_kind,lifecycle,pinned,sort_order,canonical_session_id,browser_profile_id,
                metadata_json,archived_at,delete_after,created_at,updated_at,as_of_seq
         FROM company_agents ORDER BY pinned DESC,sort_order,display_name",
        row_to_agent,
    )?;
    let groups = collect_rows(
        connection,
        "SELECT group_id,name,description,color,icon_seed,lifecycle,pinned,sort_order,
                canonical_session_id,metadata_json,archived_at,delete_after,created_at,updated_at,as_of_seq
         FROM company_groups ORDER BY pinned DESC,sort_order,name",
        row_to_group,
    )?;
    let members = collect_rows(
        connection,
        "SELECT group_id,agent_id,member_role,history_access,history_start_message_index,
                sort_order,joined_at,as_of_seq
         FROM company_group_members ORDER BY group_id,sort_order,joined_at,agent_id",
        row_to_member,
    )?;
    let responsibilities = collect_rows(
        connection,
        "SELECT responsibility_id,agent_id,title,scope,success_criteria_json,
                approval_policy_json,escalation_policy_json,status,created_at,updated_at,as_of_seq
         FROM company_responsibilities ORDER BY agent_id,created_at,responsibility_id",
        row_to_responsibility,
    )?;
    let relationships = collect_rows(
        connection,
        "SELECT from_agent_id,to_agent_id,relationship,trust_level,policy_json,updated_at,as_of_seq
         FROM company_relationships ORDER BY from_agent_id,to_agent_id",
        row_to_relationship,
    )?;
    let outside_call_grants = collect_rows(
        connection,
        "SELECT group_id,agent_id,granted,updated_at,as_of_seq
         FROM company_outside_call_grants ORDER BY group_id,agent_id",
        row_to_outside_call_grant,
    )?;
    let conversation_sources = collect_rows(
        connection,
        "SELECT session_id,owner_kind,owner_id,source_kind,canonical,linked_at,as_of_seq
         FROM company_conversation_sources ORDER BY owner_kind,owner_id,canonical DESC,linked_at DESC",
        row_to_conversation_source,
    )?;
    Ok(DirectorySnapshot {
        as_of_seq,
        agents,
        groups,
        members,
        responsibilities,
        relationships,
        outside_call_grants,
        conversation_sources,
    })
}

/// The team a new company starts with. Every other role in
/// `role_catalog_profiles` is only created when the user asks for it.
pub const DEFAULT_TEAM: &[&str] = &["phoenix", "coder", "frontend", "researcher", "critic"];

pub fn founding_team_profiles(active: bool) -> Vec<AgentProfile> {
    role_catalog_profiles(active)
        .into_iter()
        .filter(|profile| DEFAULT_TEAM.contains(&profile.agent_id.as_str()))
        .collect()
}

/// Every built-in role Phoenix knows, including ones no longer in the default
/// team. Migrations of older companies still rename and charter these.
pub fn role_catalog_profiles(active: bool) -> Vec<AgentProfile> {
    let lifecycle = if active {
        LifecycleState::Active
    } else {
        LifecycleState::Dormant
    };
    [
        ("phoenix", "phoenix", "Phoenix", "Chief of Staff", "Coordinates the company, owns cross-domain work and the user's own communication (replies, follow-ups, inbox), and resolves unclear responsibility.", "#F26B38"),
        ("planner", "planner", "Maya", "Calendar & Coordination", "Owns calendars, meetings, schedules, dependencies, reminders, and cross-company coordination that keeps work moving.", "#7C6BF2"),
        ("finance", "finance", "Vera", "Finance & Purchasing", "Owns bills, receipts, subscriptions, purchases, reconciliation, financial records, administration, and related accounts.", "#4D96A9"),
        ("coder", "coder", "Leo", "Engineering", "Owns software, technical automation, codebase understanding, maintenance, and reliable delivery.", "#2F7CF6"),
        ("frontend", "frontend", "Iris", "Product Design & Frontend", "Owns product experience, interaction design, accessibility, visual quality, and frontend execution.", "#D85AA6"),
        ("researcher", "researcher", "Theo", "Research & Intelligence", "Finds primary evidence, compares options, monitors important developments, and produces decision-ready research.", "#27A879"),
        ("presentation", "presentation", "Elena", "Knowledge & Documents", "Owns the company's durable knowledge and builds polished documents, reports, decks, and clear visual narratives.", "#E58A2B"),
        ("critic", "critic", "Remy", "Systems, Reliability & Security", "Owns system reliability, security, realistic verification, incident learning, and detection of unsupported or falsely complete work.", "#8E5BAF"),
        ("sales", "sales", "Owen", "Relationships & CRM", "Owns relationships, account research, CRM continuity, thoughtful outreach, follow-ups, and honest commitment state.", "#3D9C8D"),
        ("marketing", "marketing", "June", "Publishing & Content", "Owns publishing, content, positioning, distribution, audience growth, launches, and channel learning.", "#B64D55"),
        ("personal_logistics", "personal_logistics", "Cleo", "Operations", "Owns practical operations, recurring procedures, travel, appointments, reservations, forms, errands, deliveries, reminders, and real-life commitments.", "#5D73C9"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (agent_id, internal_role, display_name, role_title, description, color))| {
        let is_phoenix = agent_id == "phoenix";
        AgentProfile {
            agent_id: agent_id.to_string(),
            internal_role: internal_role.to_string(),
            display_name: display_name.to_string(),
            role_title: role_title.to_string(),
            description: description.to_string(),
            color: color.to_string(),
            icon_seed: format!("phoenix-flame-{agent_id}"),
            kind: AgentKind::ResponsibilityOwner,
            lifecycle: if is_phoenix { LifecycleState::Active } else { lifecycle },
            pinned: is_phoenix,
            sort_order: index as i64,
            canonical_session_id: None,
            browser_profile_id: format!("agent-{agent_id}"),
            metadata_json: "{}".to_string(),
        }
    })
    .filter(|profile| active || profile.agent_id == "phoenix")
    .collect()
}

/// Durable outcome charters for the founding company. These describe who is
/// accountable for the completed result. They never restrict tool access.
pub fn founding_responsibilities(active: bool) -> Vec<DirectoryChange> {
    responsibilities_for(founding_team_profiles(active))
}

/// Charters for every catalog role (older companies may still have them).
pub fn catalog_responsibilities(active: bool) -> Vec<DirectoryChange> {
    responsibilities_for(role_catalog_profiles(active))
}

fn responsibilities_for(profiles: Vec<AgentProfile>) -> Vec<DirectoryChange> {
    profiles
        .into_iter()
        .map(|profile| {
            let (scope, success_criteria): (&str, &[&str]) = match profile.agent_id.as_str() {
                "phoenix" => (
                    "Own ambiguous and company-wide outcomes, maintain the responsibility map, assemble the right coworkers, and close cross-domain work without becoming a mandatory relay.",
                    &["Every accepted task has one accountable owner", "Cross-domain work returns as one coherent outcome", "Missing or overlapping responsibility is surfaced and resolved"],
                ),
                "planner" => (
                    "Own calendars, meetings, schedules, dependencies, reminders, and the coordination of cross-company work. Maintain the operating rhythm without becoming a mandatory planning hop.",
                    &["Plans identify owners and evidence", "Dependencies and approvals are explicit", "The plan adapts when reality changes"],
                ),
                "coder" => (
                    "Own software implementation, technical automation, maintenance, codebase understanding, debugging, and reliable delivery across the tools needed to ship it.",
                    &["Changes address the actual failure or goal", "Relevant tests and runtime checks pass", "The codebase is reindexed after completed changes"],
                ),
                "researcher" => (
                    "Own decision-ready research grounded in current primary evidence, including synthesis across sources and explicit uncertainty.",
                    &["Claims trace to authoritative evidence", "Material alternatives and uncertainty are covered", "The answer supports the user's actual decision"],
                ),
                "frontend" => (
                    "Own product interface quality from interaction design through implemented, accessible, visually verified frontend behavior.",
                    &["The interface matches the approved design intent", "Interaction and accessibility work in the real app", "Visual regressions are screenshot-tested"],
                ),
                "presentation" => (
                    "Own durable company knowledge and polished documents, reports, decks, and visual narratives that communicate the company outcome clearly.",
                    &["The artifact is complete and audience-ready", "Sources and claims are preserved", "Rendered output is visually verified"],
                ),
                "finance" => (
                    "Own bills, receipts, subscriptions, purchases, reimbursements, reconciliation, financial reporting, tax and insurance records, administration, and related accounts across service boundaries.",
                    &["Account, period, currency, and source scope are explicit", "Totals and exceptions reconcile", "Monetary or binding actions carry the required approval receipt"],
                ),
                "critic" => (
                    "Own system reliability, security, independent review, regression reproduction, incident learning, realistic verification, and unsupported claims before they reach the user.",
                    &["Material reliability and security failure modes are tested", "Claims match authoritative evidence", "Verification scope matches the completion claim and residual risk is clear"],
                ),
                "sales" => (
                    "Own relationship outcomes: account research, CRM continuity, relevant outreach, follow-ups, handoffs, and an honest record of commitments and opportunities.",
                    &["Contacts, consent, stage, and commitments are source-backed", "External sends follow policy and the user's voice", "Pipeline changes are confirmed by the authoritative service"],
                ),
                "marketing" => (
                    "Own publishing and content outcomes: positioning, audience understanding, creation, distribution, channel operations, launches, and measurement of real audience behavior.",
                    &["Audience, offer, channel, and desired behavior are explicit", "Published state is distinguished from drafts and schedules", "Claims and measurement trace to current evidence"],
                ),
                "personal_logistics" => (
                    "Own practical operations, recurring procedures, travel, appointments, reservations, forms, errands, deliveries, reminders, and real-life commitments through verified completion.",
                    &["Dates, timezone, location, identity scope, and constraints are explicit", "Paid or binding actions carry the required approval", "Confirmation, cost, and cancellation evidence are preserved"],
                ),
                _ => unreachable!("founding profile without a responsibility charter"),
            };
            DirectoryChange::ResponsibilityUpserted {
                responsibility_id: format!("responsibility_{}", profile.agent_id),
                agent_id: profile.agent_id,
                title: profile.role_title,
                scope: scope.to_string(),
                success_criteria: success_criteria.iter().map(|value| (*value).to_string()).collect(),
                approval_policy_json: r#"{"inherits_company_policy":true}"#.to_string(),
                escalation_policy_json: r#"{"ambiguous_owner":"phoenix","blocked":"ask_user"}"#.to_string(),
                status: "active".to_string(),
            }
        })
        .collect()
}

/// Initial working relationships encode common secure handoffs. They are
/// suggestions and trust context, not hard routing or capability gates.
pub fn founding_relationships(active: bool) -> Vec<DirectoryChange> {
    catalog_relationships(active)
        .into_iter()
        .filter(|change| matches!(change, DirectoryChange::RelationshipUpserted { from_agent_id, to_agent_id, .. }
            if DEFAULT_TEAM.contains(&from_agent_id.as_str()) && DEFAULT_TEAM.contains(&to_agent_id.as_str())))
        .collect()
}

pub fn catalog_relationships(active: bool) -> Vec<DirectoryChange> {
    if !active {
        return Vec::new();
    }
    [
        (
            "finance",
            "phoenix",
            "requests receipts and one-time verification messages",
        ),
        (
            "coder",
            "critic",
            "requests independent regression and completion review",
        ),
        (
            "critic",
            "coder",
            "returns reproducible defects and verification receipts",
        ),
        (
            "frontend",
            "coder",
            "coordinates implemented product behavior",
        ),
        (
            "coder",
            "frontend",
            "requests interaction and visual judgment",
        ),
        (
            "planner",
            "phoenix",
            "escalates ownership and company-wide dependencies",
        ),
        (
            "researcher",
            "critic",
            "requests challenge of consequential synthesis",
        ),
        (
            "presentation",
            "phoenix",
            "coordinates audience-ready communication",
        ),
        (
            "sales",
            "phoenix",
            "requests relationship-aware communication and inbox continuity",
        ),
        (
            "marketing",
            "frontend",
            "coordinates brand and product presentation",
        ),
        (
            "marketing",
            "sales",
            "hands qualified audience response into commercial follow-up",
        ),
        (
            "personal_logistics",
            "phoenix",
            "requests calendar, inbox, and verification handoffs with least access",
        ),
        (
            "personal_logistics",
            "finance",
            "requests budget or payment judgment before binding actions",
        ),
    ]
    .into_iter()
    .map(
        |(from_agent_id, to_agent_id, relationship)| DirectoryChange::RelationshipUpserted {
            from_agent_id: from_agent_id.to_string(),
            to_agent_id: to_agent_id.to_string(),
            relationship: relationship.to_string(),
            trust_level: "coworker".to_string(),
            policy_json: r#"{"shares_minimum_needed":true,"requires_task_scope":true}"#.to_string(),
        },
    )
    .collect()
}

/// Render the durable identity and outcome charter injected into a coworker's
/// live system prompt. The directory remains authoritative when the user
/// renames or refines a coworker; compiled craft prompts provide expertise.
pub fn runtime_identity_block(snapshot: &DirectorySnapshot, agent_id: &str) -> Option<String> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.profile.agent_id == agent_id)?;
    let responsibilities = snapshot
        .responsibilities
        .iter()
        .filter(|responsibility| {
            responsibility.agent_id == agent_id && responsibility.status == "active"
        })
        .map(|responsibility| {
            let criteria = responsibility
                .success_criteria
                .iter()
                .map(|criterion| format!("  - {criterion}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "- {}: {}\n{}",
                responsibility.title, responsibility.scope, criteria
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let relationships = snapshot
        .relationships
        .iter()
        .filter(|relationship| relationship.from_agent_id == agent_id)
        .filter_map(|relationship| {
            let teammate = snapshot
                .agents
                .iter()
                .find(|candidate| candidate.profile.agent_id == relationship.to_agent_id)?;
            Some(format!(
                "- {} ({}) · trust `{}`: {}",
                teammate.profile.display_name,
                teammate.profile.agent_id,
                relationship.trust_level,
                relationship.relationship
            ))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let account_identity = crate::onboarding::default_account_email()
        .map(|email| {
            format!(
                "- The user-approved default email for NEW service accounts is `{email}`. Use it only when the task needs an account. Passwords, recovery material, and one-time codes stay in the vault/runtime broker and never enter conversation text."
            )
        })
        .unwrap_or_else(|| {
            "- No default account email is configured. Use ask_for_login instead of inventing one."
                .to_string()
        });
    Some(format!(
        "\n\n# Durable Phoenix Coworker Identity\n\
Your editable name is **{}**. Your company id is `{}` and your responsibility title is **{}**. {}\n\
You are a persistent first-class coworker speaking in your own canonical thread. Own the completed outcome for work inside your responsibility even when teammates contribute. Every coworker has every live tool; responsibility changes judgment, memory, account ownership, and who reports the final outcome, never the available capability set. Share only the minimum task-scoped information needed.\n\
Ownership means execution, not routing: when a request is inside your recorded responsibility, use your own browser, connected apps, schedules, files, calculations, and other tools to finish it end to end. Do not delegate its planning, review, arithmetic, app work, or final synthesis merely because another coworker has an adjacent title. Contact a peer only for private context or authority they uniquely own, or for materially independent expertise the outcome actually needs; keep yourself accountable and integrate any answer. Ownership never means repeating work or refusing to return verified partial progress when one precise user-only blocker remains. An ordinary role-owned request does not need a `work` record.\n\n\
## Outcome charter\n{}\n\n\
## Established working relationships\n{}\n\n\
## Shared operating identity\n{}\n",
        agent.profile.display_name,
        agent.profile.agent_id,
        agent.profile.role_title,
        agent.profile.description,
        if responsibilities.is_empty() {
            "- No refined charter is recorded yet; ask Phoenix to propose one after completing the current request."
        } else {
            &responsibilities
        },
        if relationships.is_empty() {
            "- No preferred handoffs are recorded. Contact any coworker whose judgment materially helps."
        } else {
            &relationships
        },
        account_identity
    ))
}

fn validate_change(change: &DirectoryChange) -> Result<()> {
    match change {
        DirectoryChange::AgentUpserted { profile } => validate_agent(profile),
        DirectoryChange::AgentLifecycleSet {
            agent_id,
            lifecycle,
            delete_after,
        } => {
            validate_id(agent_id, "agent id")?;
            anyhow::ensure!(
                *lifecycle != LifecycleState::PendingDeletion,
                "pending deletion requires typed-name confirmation"
            );
            validate_lifecycle_deadline(*lifecycle, delete_after.as_deref())
        }
        DirectoryChange::AgentDeletionScheduled {
            agent_id,
            confirmed_name,
        } => {
            validate_id(agent_id, "agent id")?;
            validate_text(confirmed_name, "confirmed agent name", MAX_SHORT_TEXT_BYTES)
        }
        DirectoryChange::AgentPurged { agent_id } => validate_id(agent_id, "agent id"),
        DirectoryChange::GroupUpserted { profile } => validate_group(profile),
        DirectoryChange::GroupLifecycleSet {
            group_id,
            lifecycle,
            delete_after,
        } => {
            validate_id(group_id, "group id")?;
            anyhow::ensure!(
                *lifecycle != LifecycleState::PendingDeletion,
                "pending deletion requires typed-name confirmation"
            );
            validate_lifecycle_deadline(*lifecycle, delete_after.as_deref())
        }
        DirectoryChange::GroupDeletionScheduled {
            group_id,
            confirmed_name,
        } => {
            validate_id(group_id, "group id")?;
            validate_text(confirmed_name, "confirmed group name", MAX_SHORT_TEXT_BYTES)
        }
        DirectoryChange::GroupPurged { group_id } => validate_id(group_id, "group id"),
        DirectoryChange::GroupMemberSet {
            group_id,
            agent_id,
            member_role,
            history_access,
            history_start_message_index,
            ..
        } => {
            validate_id(group_id, "group id")?;
            validate_id(agent_id, "agent id")?;
            validate_text(member_role, "member role", MAX_SHORT_TEXT_BYTES)?;
            anyhow::ensure!(
                *history_start_message_index <= 100_000_000,
                "group history message offset is implausibly large"
            );
            anyhow::ensure!(
                *history_access == HistoryAccess::FromJoin || *history_start_message_index == 0,
                "full group history access must start at message zero"
            );
            Ok(())
        }
        DirectoryChange::ResponsibilityUpserted {
            responsibility_id,
            agent_id,
            title,
            scope,
            success_criteria,
            approval_policy_json,
            escalation_policy_json,
            status,
        } => {
            validate_id(responsibility_id, "responsibility id")?;
            validate_id(agent_id, "agent id")?;
            validate_text(title, "responsibility title", MAX_SHORT_TEXT_BYTES)?;
            validate_text(scope, "responsibility scope", MAX_DESCRIPTION_BYTES)?;
            anyhow::ensure!(success_criteria.len() <= 256, "too many success criteria");
            for criterion in success_criteria {
                validate_text(criterion, "success criterion", MAX_SHORT_TEXT_BYTES)?;
            }
            validate_json(approval_policy_json, "approval policy")?;
            validate_json(escalation_policy_json, "escalation policy")?;
            validate_text(status, "responsibility status", MAX_SHORT_TEXT_BYTES)
        }
        DirectoryChange::RelationshipUpserted {
            from_agent_id,
            to_agent_id,
            relationship,
            trust_level,
            policy_json,
        } => {
            validate_id(from_agent_id, "source agent id")?;
            validate_id(to_agent_id, "target agent id")?;
            validate_text(relationship, "relationship", MAX_SHORT_TEXT_BYTES)?;
            validate_text(trust_level, "trust level", MAX_SHORT_TEXT_BYTES)?;
            validate_json(policy_json, "relationship policy")
        }
        DirectoryChange::OutsideCallGrantSet {
            group_id, agent_id, ..
        } => {
            validate_id(group_id, "group id")?;
            validate_id(agent_id, "agent id")
        }
        DirectoryChange::ConversationSourceLinked {
            session_id,
            owner_kind,
            owner_id,
            source_kind,
            ..
        } => {
            crate::session::SessionStore::validate_session_id(session_id)?;
            anyhow::ensure!(
                matches!(owner_kind.as_str(), "agent" | "group"),
                "conversation owner kind must be `agent` or `group`"
            );
            validate_id(owner_id, "conversation owner id")?;
            validate_text(
                source_kind,
                "conversation source kind",
                MAX_SHORT_TEXT_BYTES,
            )
        }
    }
}

fn validate_agent(profile: &AgentProfile) -> Result<()> {
    validate_id(&profile.agent_id, "agent id")?;
    validate_id(&profile.internal_role, "internal role")?;
    validate_text(&profile.display_name, "display name", MAX_SHORT_TEXT_BYTES)?;
    validate_text(&profile.role_title, "role title", MAX_SHORT_TEXT_BYTES)?;
    validate_text(
        &profile.description,
        "agent description",
        MAX_DESCRIPTION_BYTES,
    )?;
    validate_color(&profile.color)?;
    validate_id(&profile.icon_seed, "icon seed")?;
    validate_id(&profile.browser_profile_id, "browser profile id")?;
    if let Some(session_id) = profile.canonical_session_id.as_deref() {
        crate::session::SessionStore::validate_session_id(session_id)?;
    }
    validate_json(&profile.metadata_json, "agent metadata")
}

fn validate_group(profile: &GroupProfile) -> Result<()> {
    validate_id(&profile.group_id, "group id")?;
    validate_text(&profile.name, "group name", MAX_SHORT_TEXT_BYTES)?;
    validate_text(
        &profile.description,
        "group description",
        MAX_DESCRIPTION_BYTES,
    )?;
    validate_color(&profile.color)?;
    validate_id(&profile.icon_seed, "group icon seed")?;
    if let Some(session_id) = profile.canonical_session_id.as_deref() {
        crate::session::SessionStore::validate_session_id(session_id)?;
    }
    validate_json(&profile.metadata_json, "group metadata")
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{label} is empty");
    anyhow::ensure!(value.len() <= MAX_ID_BYTES, "{label} is too long");
    anyhow::ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "{label} contains unsafe characters"
    );
    Ok(())
}

fn validate_text(value: &str, label: &str, max_bytes: usize) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{label} is empty");
    anyhow::ensure!(value.len() <= max_bytes, "{label} is too large");
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "{label} contains control characters"
    );
    Ok(())
}

fn validate_color(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == 7 && value.starts_with('#'),
        "agent/group color must be #RRGGBB"
    );
    anyhow::ensure!(
        value[1..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "agent/group color must be hexadecimal"
    );
    Ok(())
}

fn validate_json(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(value.len() <= MAX_METADATA_BYTES, "{label} is too large");
    let value = if value.trim().is_empty() { "{}" } else { value };
    let _: serde_json::Value =
        serde_json::from_str(value).with_context(|| format!("{label} is not valid JSON"))?;
    Ok(())
}

fn normalized_json(value: &str) -> &str {
    if value.trim().is_empty() {
        "{}"
    } else {
        value
    }
}

fn validate_lifecycle_deadline(
    lifecycle: LifecycleState,
    delete_after: Option<&str>,
) -> Result<()> {
    match lifecycle {
        LifecycleState::PendingDeletion => {
            let deadline = delete_after.context("pending deletion requires a deletion deadline")?;
            DateTime::parse_from_rfc3339(deadline).context("deletion deadline is not RFC3339")?;
        }
        _ => anyhow::ensure!(
            delete_after.is_none(),
            "only pending deletion may have a deletion deadline"
        ),
    }
    Ok(())
}

fn collect_rows<T>(
    connection: &Connection,
    sql: &str,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], map)?;
    let mut values = Vec::new();
    for row in rows {
        anyhow::ensure!(
            values.len() < MAX_DIRECTORY_ROWS,
            "company directory exceeds {MAX_DIRECTORY_ROWS} rows in one projection"
        );
        values.push(row?);
    }
    Ok(values)
}

fn row_to_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    let kind: String = row.get(7)?;
    let lifecycle: String = row.get(8)?;
    Ok(AgentRecord {
        profile: AgentProfile {
            agent_id: row.get(0)?,
            internal_role: row.get(1)?,
            display_name: row.get(2)?,
            role_title: row.get(3)?,
            description: row.get(4)?,
            color: row.get(5)?,
            icon_seed: row.get(6)?,
            kind: AgentKind::parse(&kind).map_err(sql_conversion_error)?,
            lifecycle: LifecycleState::parse(&lifecycle).map_err(sql_conversion_error)?,
            pinned: row.get::<_, i64>(9)? != 0,
            sort_order: row.get(10)?,
            canonical_session_id: row.get(11)?,
            browser_profile_id: row.get(12)?,
            metadata_json: row.get(13)?,
        },
        archived_at: row.get(14)?,
        delete_after: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        as_of_seq: row.get(18)?,
    })
}

fn row_to_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupRecord> {
    let lifecycle: String = row.get(5)?;
    Ok(GroupRecord {
        profile: GroupProfile {
            group_id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            color: row.get(3)?,
            icon_seed: row.get(4)?,
            lifecycle: LifecycleState::parse(&lifecycle).map_err(sql_conversion_error)?,
            pinned: row.get::<_, i64>(6)? != 0,
            sort_order: row.get(7)?,
            canonical_session_id: row.get(8)?,
            metadata_json: row.get(9)?,
        },
        archived_at: row.get(10)?,
        delete_after: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        as_of_seq: row.get(14)?,
    })
}

fn row_to_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupMemberRecord> {
    let history: String = row.get(3)?;
    let history_access = match history.as_str() {
        "full" => HistoryAccess::Full,
        "from_join" => HistoryAccess::FromJoin,
        _ => {
            return Err(sql_conversion_error(anyhow::anyhow!(
                "unknown history access `{history}`"
            )))
        }
    };
    Ok(GroupMemberRecord {
        group_id: row.get(0)?,
        agent_id: row.get(1)?,
        member_role: row.get(2)?,
        history_access,
        history_start_message_index: usize::try_from(row.get::<_, i64>(4)?).map_err(|_| {
            sql_conversion_error(anyhow::anyhow!("negative group history message offset"))
        })?,
        sort_order: row.get(5)?,
        joined_at: row.get(6)?,
        as_of_seq: row.get(7)?,
    })
}

fn row_to_responsibility(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResponsibilityRecord> {
    let criteria_json: String = row.get(4)?;
    let success_criteria = serde_json::from_str(&criteria_json).map_err(sql_conversion_error)?;
    Ok(ResponsibilityRecord {
        responsibility_id: row.get(0)?,
        agent_id: row.get(1)?,
        title: row.get(2)?,
        scope: row.get(3)?,
        success_criteria,
        approval_policy_json: row.get(5)?,
        escalation_policy_json: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        as_of_seq: row.get(10)?,
    })
}

fn row_to_relationship(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelationshipRecord> {
    Ok(RelationshipRecord {
        from_agent_id: row.get(0)?,
        to_agent_id: row.get(1)?,
        relationship: row.get(2)?,
        trust_level: row.get(3)?,
        policy_json: row.get(4)?,
        updated_at: row.get(5)?,
        as_of_seq: row.get(6)?,
    })
}

fn row_to_conversation_source(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ConversationSourceRecord> {
    Ok(ConversationSourceRecord {
        session_id: row.get(0)?,
        owner_kind: row.get(1)?,
        owner_id: row.get(2)?,
        source_kind: row.get(3)?,
        canonical: row.get::<_, i64>(4)? != 0,
        linked_at: row.get(5)?,
        as_of_seq: row.get(6)?,
    })
}

fn row_to_outside_call_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutsideCallGrantRecord> {
    Ok(OutsideCallGrantRecord {
        group_id: row.get(0)?,
        agent_id: row.get(1)?,
        granted: row.get::<_, i64>(2)? != 0,
        updated_at: row.get(3)?,
        as_of_seq: row.get(4)?,
    })
}

fn sql_conversion_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

pub(crate) fn outside_call_granted(
    connection: &Connection,
    group_id: &str,
    agent_id: &str,
) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT granted FROM company_outside_call_grants WHERE group_id=?1 AND agent_id=?2",
            params![group_id, agent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some_and(|granted| granted != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn founding_team_is_phoenix_plus_four_default_coworkers() {
        let team = founding_team_profiles(true);
        assert_eq!(team[0].display_name, "Phoenix");
        assert!(team.iter().all(|agent| agent.lifecycle == LifecycleState::Active));
        assert!(!team.iter().any(|agent| agent.internal_role == "browser"));
        let names = team.iter().map(|agent| agent.display_name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["Phoenix", "Leo", "Iris", "Theo", "Remy"]);
        // The rest of the catalog stays known for older companies.
        let catalog = role_catalog_profiles(true);
        assert_eq!(catalog.len(), 11);
        assert!(catalog.iter().any(|agent| agent.agent_id == "personal_logistics" && agent.display_name == "Cleo"));
    }

    #[test]
    fn scratch_mode_keeps_only_phoenix_active() {
        let team = founding_team_profiles(false);
        assert_eq!(team.len(), 1);
        assert_eq!(
            team.iter()
                .filter(|agent| agent.lifecycle == LifecycleState::Active)
                .count(),
            1
        );
        assert_eq!(team[0].agent_id, "phoenix");
    }

    #[test]
    fn founding_company_uses_outcome_responsibilities_not_tool_agents() {
        let team = role_catalog_profiles(true);
        assert!(team
            .iter()
            .all(|agent| agent.kind == AgentKind::ResponsibilityOwner));
        assert!(team.iter().any(|agent| {
            agent.agent_id == "finance"
                && agent.display_name == "Vera"
                && agent.role_title == "Finance & Purchasing"
        }));
        assert!(team.iter().any(|agent| {
            agent.agent_id == "personal_logistics"
                && agent.display_name == "Cleo"
                && agent.role_title == "Operations"
        }));
        assert!(!team.iter().any(|agent| matches!(
            agent.internal_role.as_str(),
            "browser" | "computer_use" | "database" | "hacker" | "tester"
        )));
        assert_eq!(founding_responsibilities(true).len(), 5);
        assert_eq!(catalog_responsibilities(true).len(), 11);
        assert_eq!(founding_responsibilities(false).len(), 1);
    }

    #[test]
    fn pending_deletion_requires_a_recovery_deadline() {
        assert!(validate_lifecycle_deadline(LifecycleState::PendingDeletion, None).is_err());
        assert!(validate_lifecycle_deadline(
            LifecycleState::Archived,
            Some("2026-09-13T00:00:00Z")
        )
        .is_err());
        assert!(validate_lifecycle_deadline(
            LifecycleState::PendingDeletion,
            Some("2026-09-13T00:00:00Z")
        )
        .is_ok());
    }

    #[test]
    fn historical_sessions_choose_one_canonical_thread_without_merging_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::session::SessionStore::new(dir.path());
        store.upsert(crate::session::Session::new_main_with_id(
            "main-older",
            "model",
            "prompt",
        ));
        store.save_one("main-older").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.upsert(crate::session::Session::new_main_with_id(
            "main-newer",
            "model",
            "prompt",
        ));
        store.save_one("main-newer").unwrap();

        let discovered = discover_session_history(dir.path()).unwrap();
        assert_eq!(discovered.len(), 2);
        assert_eq!(
            discovered.iter().filter(|source| source.canonical).count(),
            1
        );
        assert_eq!(
            discovered
                .iter()
                .find(|source| source.canonical)
                .map(|source| source.session_id.as_str()),
            Some("main-newer")
        );
        assert!(dir.path().join("main-older.json").exists());
        assert!(dir.path().join("main-newer.json").exists());
    }
}
