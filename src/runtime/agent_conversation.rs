//! First-class direct coworker conversations.
//!
//! A sidebar coworker owns one canonical endless transcript. The immutable
//! agent id, editable display name, and legacy/runtime role can all address it;
//! only active coworkers may receive new turns.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::company_directory::{DirectorySnapshot, LifecycleState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTurnContext {
    pub agent_id: String,
    pub internal_role: String,
    pub display_name: String,
    pub role_title: String,
    pub canonical_session_id: String,
}

/// Canonical owner of one visible endless conversation. Routing derives from
/// this backend identity; callers are not allowed to turn an omitted target
/// into an implicit Phoenix turn inside somebody else's thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalConversationOwner {
    Agent(AgentTurnContext),
    Group { group_id: String },
}

/// Immutable owner assertion carried by conversation-scoped client requests.
///
/// The session id is still the storage/routing key, but it is not sufficient
/// authorization for a UI projection: a stale client must not be able to pair
/// a direct coworker session with a group view (or vice versa). Legacy clients
/// may omit this assertion; current Canvas sends it on every scoped read and
/// mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationOwnerRef {
    pub kind: ConversationOwnerKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConversationOwnerKind {
    Agent,
    Group,
}

/// Verify both sides of the canonical binding before a conversation-scoped
/// request reads, subscribes to, or mutates backend state. Inactive/archived
/// owners remain readable here; lifecycle policy belongs to turn routing, not
/// history authorization.
pub fn authorize_canonical_session(
    snapshot: &DirectorySnapshot,
    session_id: &str,
    expected: Option<&ConversationOwnerRef>,
) -> Result<()> {
    let Some(expected) = expected else {
        // Backward-compatible boundary for older CLI/TUI clients. New Canvas
        // always supplies an owner assertion.
        return Ok(());
    };
    anyhow::ensure!(
        !expected.id.trim().is_empty()
            && expected.id.len() <= 160
            && !expected.id.chars().any(char::is_control),
        "invalid conversation owner id"
    );

    let expected_session = match expected.kind {
        ConversationOwnerKind::Agent => {
            let matches = snapshot
                .agents
                .iter()
                .filter(|agent| agent.profile.agent_id == expected.id)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "unknown or ambiguous conversation agent `{}`",
                expected.id
            );
            matches[0].profile.canonical_session_id.as_deref()
        }
        ConversationOwnerKind::Group => {
            let matches = snapshot
                .groups
                .iter()
                .filter(|group| group.profile.group_id == expected.id)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "unknown or ambiguous conversation group `{}`",
                expected.id
            );
            matches[0].profile.canonical_session_id.as_deref()
        }
    }
    .with_context(|| {
        format!(
            "{} `{}` has no canonical conversation",
            match expected.kind {
                ConversationOwnerKind::Agent => "agent",
                ConversationOwnerKind::Group => "group",
            },
            expected.id
        )
    })?;
    anyhow::ensure!(
        expected_session == session_id,
        "conversation owner mismatch: {:?} `{}` belongs to `{expected_session}`, not `{session_id}`",
        expected.kind,
        expected.id
    );

    let session_agents = snapshot
        .agents
        .iter()
        .filter(|agent| agent.profile.canonical_session_id.as_deref() == Some(session_id))
        .map(|agent| agent.profile.agent_id.as_str())
        .collect::<Vec<_>>();
    let session_groups = snapshot
        .groups
        .iter()
        .filter(|group| group.profile.canonical_session_id.as_deref() == Some(session_id))
        .map(|group| group.profile.group_id.as_str())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        session_agents.len() + session_groups.len() == 1,
        "canonical session `{session_id}` must have exactly one owner"
    );
    match expected.kind {
        ConversationOwnerKind::Agent => anyhow::ensure!(
            session_agents.as_slice() == [expected.id.as_str()] && session_groups.is_empty(),
            "conversation owner mismatch: `{session_id}` is not agent `{}`",
            expected.id
        ),
        ConversationOwnerKind::Group => anyhow::ensure!(
            session_groups.as_slice() == [expected.id.as_str()] && session_agents.is_empty(),
            "conversation owner mismatch: `{session_id}` is not group `{}`",
            expected.id
        ),
    }
    Ok(())
}

pub fn resolve_canonical_owner(
    snapshot: &DirectorySnapshot,
    session_id: &str,
) -> Result<Option<CanonicalConversationOwner>> {
    let agents = snapshot
        .agents
        .iter()
        .filter(|agent| agent.profile.canonical_session_id.as_deref() == Some(session_id))
        .collect::<Vec<_>>();
    let groups = snapshot
        .groups
        .iter()
        .filter(|group| group.profile.canonical_session_id.as_deref() == Some(session_id))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        agents.len() + groups.len() <= 1,
        "canonical session `{session_id}` has more than one owner"
    );
    if let Some(agent) = agents.first() {
        return resolve_agent_turn(snapshot, &agent.profile.agent_id)
            .map(CanonicalConversationOwner::Agent)
            .map(Some);
    }
    if let Some(group) = groups.first() {
        anyhow::ensure!(
            group.profile.lifecycle == LifecycleState::Active,
            "group `{}` is not active",
            group.profile.group_id
        );
        return Ok(Some(CanonicalConversationOwner::Group {
            group_id: group.profile.group_id.clone(),
        }));
    }
    Ok(None)
}

pub fn resolve_agent_turn(snapshot: &DirectorySnapshot, target: &str) -> Result<AgentTurnContext> {
    let requested = normalized(target);
    let matching = |needle: &str| {
        snapshot
            .agents
            .iter()
            .filter(|agent| {
                [
                    agent.profile.agent_id.as_str(),
                    agent.profile.internal_role.as_str(),
                    agent.profile.display_name.as_str(),
                ]
                .into_iter()
                .any(|candidate| normalized(candidate) == needle)
            })
            .collect::<Vec<_>>()
    };
    // Editable human names win. Responsibility aliases are a fallback, so a
    // user-created coworker actually named "Operations" is never shadowed.
    let mut matches = matching(&requested);
    if matches.is_empty() {
        let role_alias = match requested.as_str() {
            "operations" => Some("personal_logistics"),
            "systems" | "reliability" | "security" => Some("critic"),
            "documents" | "knowledge" => Some("presentation"),
            "purchasing" => Some("finance"),
            "crm" | "relationships" => Some("sales"),
            "publishing" | "content" => Some("marketing"),
            _ => None,
        };
        if let Some(role_alias) = role_alias {
            matches = matching(role_alias);
        }
    }
    anyhow::ensure!(!matches.is_empty(), "unknown coworker `{requested}`");
    anyhow::ensure!(
        matches.len() == 1,
        "coworker name `{requested}` is ambiguous; address the immutable agent id"
    );
    let agent = matches[0];
    anyhow::ensure!(
        agent.profile.lifecycle == LifecycleState::Active,
        "coworker `{}` is {} and cannot receive new work",
        agent.profile.display_name,
        lifecycle_label(agent.profile.lifecycle)
    );
    let canonical_session_id = agent
        .profile
        .canonical_session_id
        .clone()
        .with_context(|| {
            format!(
                "coworker `{}` has no canonical thread",
                agent.profile.agent_id
            )
        })?;
    Ok(AgentTurnContext {
        agent_id: agent.profile.agent_id.clone(),
        internal_role: agent.profile.internal_role.clone(),
        display_name: agent.profile.display_name.clone(),
        role_title: agent.profile.role_title.clone(),
        canonical_session_id,
    })
}

fn normalized(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
}

fn lifecycle_label(lifecycle: LifecycleState) -> &'static str {
    match lifecycle {
        LifecycleState::Active => "active",
        LifecycleState::Dormant => "dormant",
        LifecycleState::Archived => "archived",
        LifecycleState::PendingDeletion => "pending deletion",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::company_directory::{
        role_catalog_profiles, AgentRecord, GroupProfile, GroupRecord,
    };

    fn snapshot() -> DirectorySnapshot {
        DirectorySnapshot {
            agents: role_catalog_profiles(true)
                .into_iter()
                .map(|mut profile| {
                    profile.canonical_session_id = Some(format!("agent-{}", profile.agent_id));
                    AgentRecord {
                        profile,
                        archived_at: None,
                        delete_after: None,
                        created_at: "now".into(),
                        updated_at: "now".into(),
                        as_of_seq: 1,
                    }
                })
                .collect(),
            ..DirectorySnapshot::default()
        }
    }

    #[test]
    fn immutable_id_role_and_editable_name_resolve_the_same_coworker() {
        let snapshot = snapshot();
        for target in ["finance", "Vera", "@vera", "purchasing"] {
            let turn = resolve_agent_turn(&snapshot, target).unwrap();
            assert_eq!(turn.agent_id, "finance");
            assert_eq!(turn.canonical_session_id, "agent-finance");
        }
    }

    #[test]
    fn archived_coworkers_keep_history_but_reject_new_work() {
        let mut snapshot = snapshot();
        snapshot
            .agents
            .iter_mut()
            .find(|agent| agent.profile.agent_id == "finance")
            .unwrap()
            .profile
            .lifecycle = LifecycleState::Archived;
        assert!(resolve_agent_turn(&snapshot, "Vera").is_err());
        assert_eq!(
            snapshot
                .agents
                .iter()
                .find(|agent| agent.profile.agent_id == "finance")
                .unwrap()
                .profile
                .canonical_session_id
                .as_deref(),
            Some("agent-finance")
        );
    }

    #[test]
    fn responsibility_aliases_reach_the_compatible_internal_roles() {
        let snapshot = snapshot();
        for (target, expected) in [
            ("operations", "personal_logistics"),
            ("security", "critic"),
            ("documents", "presentation"),
            ("crm", "sales"),
            ("publishing", "marketing"),
        ] {
            assert_eq!(
                resolve_agent_turn(&snapshot, target).unwrap().agent_id,
                expected
            );
        }
    }

    #[test]
    fn canonical_session_resolves_to_its_single_backend_owner() {
        let snapshot = snapshot();
        let owner = resolve_canonical_owner(&snapshot, "agent-finance").unwrap();
        assert!(matches!(
            owner,
            Some(CanonicalConversationOwner::Agent(AgentTurnContext { agent_id, .. }))
                if agent_id == "finance"
        ));
        assert_eq!(resolve_canonical_owner(&snapshot, "unknown").unwrap(), None);
    }

    #[test]
    fn scoped_reads_require_the_expected_canonical_owner() {
        let mut snapshot = snapshot();
        snapshot.groups.push(GroupRecord {
            profile: GroupProfile {
                group_id: "launch-room".into(),
                name: "Launch room".into(),
                description: "Coordinate launch work".into(),
                color: "#112233".into(),
                icon_seed: "launch-room".into(),
                // Archived rooms keep readable history; authorization is not
                // the same thing as permission to start a new group turn.
                lifecycle: LifecycleState::Archived,
                pinned: false,
                sort_order: 0,
                canonical_session_id: Some("group-launch-room".into()),
                metadata_json: "{}".into(),
            },
            archived_at: Some("now".into()),
            delete_after: None,
            created_at: "then".into(),
            updated_at: "now".into(),
            as_of_seq: 1,
        });

        let finance = ConversationOwnerRef {
            kind: ConversationOwnerKind::Agent,
            id: "finance".into(),
        };
        let launch = ConversationOwnerRef {
            kind: ConversationOwnerKind::Group,
            id: "launch-room".into(),
        };
        authorize_canonical_session(&snapshot, "agent-finance", Some(&finance)).unwrap();
        authorize_canonical_session(&snapshot, "group-launch-room", Some(&launch)).unwrap();
        assert!(authorize_canonical_session(&snapshot, "agent-finance", Some(&launch)).is_err());
        assert!(
            authorize_canonical_session(&snapshot, "group-launch-room", Some(&finance)).is_err()
        );
        assert!(authorize_canonical_session(&snapshot, "agent-finance", None).is_ok());
    }
}
