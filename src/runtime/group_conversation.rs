//! First-class group conversation planning.
//!
//! A group is a canonical endless thread plus an ordered set of active
//! coworkers with one leader. A user message that `@mentions` members wakes
//! exactly those members; an unaddressed message wakes only the group leader,
//! who coordinates the room (see `group_coordination`). Every awakened
//! coworker reads the same canonical room transcript before replying.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::company_directory::{DirectorySnapshot, HistoryAccess, LifecycleState};

const MAX_GROUP_PARTICIPANTS: usize = 32;

pub(crate) fn is_mention_only_followup(request: &str) -> bool {
    let mut tokens = request.split_whitespace().peekable();
    tokens.peek().is_some() && tokens.all(|token| {
        let token = token.trim_end_matches([',', '.', '!', '?']);
        token.strip_prefix('@').is_some_and(|name| !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-')))
    })
}

#[test]
fn mention_only_followup_keeps_substantive_requests_distinct() {
    for request in ["@researcher", " @Theo! ", "@researcher, @coder"] {
        assert!(is_mention_only_followup(request));
    }
    for request in ["", "@", "hello @Theo", "@Theo stop", "@Theo continue", "a@b.com"] {
        assert!(!is_mention_only_followup(request));
    }
}

/// The runtime lane a room participant's turns run in (and are steered
/// through): `orchestrator` for Phoenix, otherwise the specialist label.
pub fn participant_lane(participant: &GroupParticipant) -> Option<String> {
    if matches!(participant.internal_role.as_str(), "phoenix" | "orchestrator") {
        return Some("orchestrator".to_string());
    }
    crate::runtime::delegation::specialist_from_talk_name(&participant.internal_role)
        .map(|agent| crate::runtime::delegation::specialist_label(agent).to_string())
}

/// Separator of a race-fallback turn id: `<client turn id>.rf-<lane>`.
pub const ROOM_FALLBACK_SEPARATOR: &str = ".rf-";

/// Turn id of the normal turn started for `lane` when a room message could
/// not be steered into it because its turn had just finished.
pub fn room_fallback_turn_id(client_turn_id: &str, lane: &str) -> String {
    format!("{client_turn_id}{ROOM_FALLBACK_SEPARATOR}{}", crate::runtime::postbox::base_agent(lane))
}

/// The authored room message a turn id belongs to. A race-fallback turn
/// shares its message's single canonical transcript entry.
pub fn room_boundary_turn_id(turn_id: &str) -> &str {
    turn_id.split(ROOM_FALLBACK_SEPARATOR).next().unwrap_or(turn_id)
}

/// What one member gets when the user posts in a room that is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomDelivery {
    /// Running: steer the message into its turn with this framing.
    Steer(crate::runtime::postbox::RoomSteerKind),
    /// Idle but must act: start a turn for it now (concurrently).
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMemberDelivery {
    pub agent_id: String,
    pub lane: String,
    pub delivery: RoomDelivery,
}

/// "Everyone hears everything, only some act." `actors` are the members that
/// must act (the validated activation: the @mentioned members, everyone for
/// `@everyone`/`@all`, or the leader for an unaddressed message). Every
/// RUNNING member hears the message (actionable for actors, FYI for the
/// rest); an idle actor is started; an idle non-actor is left asleep — it
/// reads the message from the canonical transcript on its next turn.
pub fn plan_room_delivery(
    context: &GroupTurnContext,
    actors: &[String],
    everyone: bool,
    running: impl Fn(&str) -> bool,
) -> Vec<RoomMemberDelivery> {
    use crate::runtime::postbox::RoomSteerKind as Kind;
    let leader = context.leader_agent_id.as_deref();
    let mentioned_any = context.participants.iter().any(|p| p.explicitly_mentioned);
    let leader_solo = !everyone
        && mentioned_any
        && actors.len() == 1
        && leader.is_some_and(|leader| actors[0] == leader);
    let mut plan = Vec::new();
    for participant in &context.participants {
        let Some(lane) = participant_lane(participant) else { continue };
        let is_leader = leader == Some(participant.agent_id.as_str());
        let acts = actors.iter().any(|id| id == &participant.agent_id);
        let delivery = match (acts, running(&lane)) {
            (true, false) => RoomDelivery::Start,
            (true, true) => RoomDelivery::Steer(if everyone {
                if is_leader { Kind::EveryoneReplan } else { Kind::EveryoneAct }
            } else if leader_solo {
                Kind::LeaderSolo
            } else if is_leader && !mentioned_any {
                Kind::LeaderAct
            } else {
                Kind::MentionAct
            }),
            (false, true) => RoomDelivery::Steer(if is_leader && mentioned_any {
                Kind::LeaderFyi
            } else {
                Kind::Fyi
            }),
            (false, false) => continue,
        };
        plan.push(RoomMemberDelivery { agent_id: participant.agent_id.clone(), lane, delivery });
    }
    plan
}

/// Narrow a validated activation to the idle actors that must be started
/// while other members keep running. Dependencies among the started subset
/// are kept; edges to members that are not started (they are running and got
/// the message steered in) are dropped.
pub fn narrow_activation(
    intent: &GroupActivationIntent,
    start: &[String],
) -> GroupActivationIntent {
    let active = intent
        .active_agent_ids
        .iter()
        .filter(|id| start.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    if active == intent.active_agent_ids {
        return intent.clone();
    }
    let edges = intent
        .execution_dependencies
        .clone()
        .unwrap_or_else(|| legacy_wave_dependencies(&intent.execution_waves))
        .into_iter()
        .filter(|edge| active.contains(&edge.prerequisite) && active.contains(&edge.dependent))
        .collect::<Vec<_>>();
    let waves = dependency_waves(&active, &edges).unwrap_or_else(|_| vec![active.clone()]);
    let mut narrowed = GroupActivationIntent {
        inspection_participants: Default::default(),
        tool_constraints: Default::default(),
        group_id: intent.group_id.clone(),
        roster_fingerprint: intent.roster_fingerprint.clone(),
        selection: GroupActivationSelection::Explicit,
        execution_mode: if edges.is_empty() { GroupExecutionMode::Parallel } else { GroupExecutionMode::Ordered },
        execution_waves: if active.is_empty() { Vec::new() } else { waves },
        execution_dependencies: Some(edges),
        active_agent_ids: active,
    };
    narrowed.inherit_tool_constraints(intent);
    narrowed
}

/// A required result, not a global barrier between unrelated coworkers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupDependency {
    pub prerequisite: String,
    pub dependent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupParticipant {
    pub agent_id: String,
    pub internal_role: String,
    pub display_name: String,
    #[serde(default)]
    pub role_title: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub icon_seed: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<serde_json::Value>,
    pub member_role: String,
    pub history_access: HistoryAccess,
    pub history_start_message_index: usize,
    pub explicitly_mentioned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupTurnContext {
    #[serde(default, skip_serializing_if="std::collections::BTreeSet::is_empty")]
    pub inspection_participants: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if="std::collections::BTreeMap::is_empty")]
    pub tool_constraints: std::collections::BTreeMap<String,Vec<String>>,
    pub group_id: String,
    pub group_name: String,
    pub canonical_session_id: String,
    pub participants: Vec<GroupParticipant>,
    /// Retained for backwards-compatible metadata decoding. Group execution is
    /// one direct response per explicitly pinged coworker; there are no
    /// automatic discussion rounds.
    pub discussion_rounds: u8,
    pub read_full_transcript: bool,
    /// Stable-id preview layers. Legacy payloads without exact dependencies
    /// retain whole-wave ordering; new execution uses only prerequisite edges.
    #[serde(default)]
    pub execution_waves: Vec<Vec<String>>,
    /// None denotes a legacy wave plan; Some(empty) is explicitly independent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_dependencies: Option<Vec<GroupDependency>>,
    /// Effective group leader (agent id) when this context was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_agent_id: Option<String>,
}

impl GroupTurnContext {
    /// Resolve an actor label (agent id or internal role) to its agent id.
    pub fn agent_id_for(&self, actor: &str) -> Option<&str> {
        self.participants
            .iter()
            .find(|p| p.agent_id == actor || p.internal_role == actor
                || (actor == "orchestrator" && p.internal_role == "phoenix"))
            .map(|p| p.agent_id.as_str())
    }
    /// True when `actor` (agent id or internal role) leads this room.
    pub fn is_leader(&self, actor: &str) -> bool {
        match (self.leader_agent_id.as_deref(), self.agent_id_for(actor)) {
            (Some(leader), Some(id)) => leader == id,
            _ => false,
        }
    }
    pub fn leader(&self) -> Option<&GroupParticipant> {
        let leader = self.leader_agent_id.as_deref()?;
        self.participants.iter().find(|p| p.agent_id == leader)
    }
    /// Current scheduler facts must survive any transcript compaction or notes
    /// strategy. A prerequisite cannot wait for the work its return unlocks.
    pub fn publication_handoff_instruction(&self, actor: &str) -> Option<String> {
        let actor=self.participants.iter().find(|p|p.agent_id==actor||p.internal_role==actor)?;
        let edges=self.execution_dependencies.clone().unwrap_or_else(||legacy_wave_dependencies(&self.execution_waves));
        let waiting=self.participants.iter().filter(|p|edges.iter().any(|edge|
            edge.prerequisite==actor.agent_id&&edge.dependent==p.agent_id))
            .map(|p|p.display_name.as_str()).collect::<Vec<_>>();
        if waiting.is_empty() {return None;}
        Some(format!("CURRENT GROUP HANDOFF: Your saved contribution is a prerequisite for {}. Their assignment in this turn cannot start until you finish your own work and publish final_answer. Return your result when your part is ready; do not wait for their downstream review, mark their work complete, or search historical records for a response to this live turn. The group scheduler will deliver your contribution and start eligible downstream work.",waiting.join(", ")))
    }
    /// True when `name` (an agent id, internal role or display name, with or
    /// without a leading `@`) names someone in this room.
    pub fn names_member(&self, name: &str) -> bool {
        let wanted = normalized_mention(name.trim().trim_start_matches('@'));
        !wanted.is_empty() && self.participants.iter().filter(|p| {
            let first = p.display_name.split_whitespace().next().unwrap_or("");
            [p.agent_id.as_str(), p.internal_role.as_str(), p.display_name.as_str(), first]
                .into_iter().any(|alias| normalized_mention(alias) == wanted)
        }).count() == 1
    }
    /// Standing room etiquette for every awakened member: teammates are
    /// brought in by writing `@Name` in the reply, never by a private talk.
    pub fn room_instruction(&self, actor: &str) -> String {
        let me = self.participants.iter()
            .find(|p| p.agent_id == actor || p.internal_role == actor)
            .map(|p| p.agent_id.as_str()).unwrap_or(actor);
        let others = self.participants.iter().filter(|p| p.agent_id != me)
            .map(|p| format!("@{} ({})", p.display_name.split_whitespace().next().unwrap_or(&p.display_name), p.internal_role))
            .collect::<Vec<_>>().join(", ");
        let base = format!("GROUP ROOM \"{}\": you are speaking in a shared room the user can see. Teammates here: {}. To bring a teammate in, write @Name in your reply (for example \"@Leon can you take the visuals?\"); they wake up and answer in this same room under their own name. Never use the talk tool to reach someone in this room, and never claim a teammate joined unless they replied here. Do not repeat what a teammate already said in the room; build on it or stay brief.", self.group_name, if others.is_empty() { "none".to_string() } else { others });
        // Group leader architecture: leader protocol / member assignment and
        // the compact mission board ride on the same room block.
        match super::group_coordination::coordination_block(self, me) {
            Some(block) => format!("{base}\n\n{block}"),
            None => base,
        }
    }
    pub fn is_inspection(&self,actor:&str)->bool {
        let id=self.participants.iter().find(|p|p.agent_id==actor||p.internal_role==actor).map(|p|p.agent_id.as_str()).unwrap_or(actor);
        self.inspection_participants.contains(id)
    }
    pub fn permits_tool(&self,actor:&str,tool:&str)->bool {
        // Loading schemas grants nothing; every call stays gated.
        if tool=="final_answer" || tool==crate::tools::deferral::LOAD_TOOL {return true;}
        let id=self.participants.iter().find(|p|p.agent_id==actor||p.internal_role==actor).map(|p|p.agent_id.as_str()).unwrap_or(actor);
        self.tool_constraints.get(id).is_none_or(|allowed|allowed.iter().any(|pattern|
            pattern.strip_suffix('*').map_or(pattern==tool,|prefix|tool.starts_with(prefix))))
    }
    /// A working prerequisite cannot start its own downstream assignment by
    /// taking the peer-message path around normal group admission.
    pub fn downstream_of(&self, caller:&str,target:&str)->bool {
        let resolve=|name:&str|self.participants.iter().find(|p|p.agent_id==name||p.internal_role==name)
            .map(|p|p.agent_id.clone());
        let (Some(caller),Some(target))=(resolve(caller),resolve(target)) else {return false;};
        let edges=self.execution_dependencies.clone().unwrap_or_else(||legacy_wave_dependencies(&self.execution_waves));
        dependency_reachable(&edges,&caller,&target)
    }
}

fn dependency_reachable(edges:&[GroupDependency],caller:&str,target:&str)->bool {
    let mut pending=vec![caller];let mut seen=std::collections::HashSet::new();
    while let Some(current)=pending.pop() {
        if !seen.insert(current){continue;}
        for edge in edges.iter().filter(|edge|edge.prerequisite==current) {
            if edge.dependent==target{return true;}
            pending.push(&edge.dependent);
        }
    }
    false
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupActivationSelection {
    Explicit,
    Everyone,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupExecutionMode {
    Parallel,
    Ordered,
}

impl Default for GroupExecutionMode {
    fn default() -> Self {
        Self::Parallel
    }
}

impl Default for GroupActivationSelection {
    fn default() -> Self {
        Self::Explicit
    }
}

/// The durable, stable-id activation decision a later wire/storage slice can
/// carry. Display names deliberately stay out of the intent because they are
/// mutable presentation data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupActivationIntent {
    #[serde(default, skip_serializing_if="std::collections::BTreeSet::is_empty")]
    pub inspection_participants: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if="std::collections::BTreeMap::is_empty")]
    pub tool_constraints: std::collections::BTreeMap<String,Vec<String>>,
    pub group_id: String,
    pub roster_fingerprint: String,
    #[serde(default)]
    pub selection: GroupActivationSelection,
    #[serde(default)]
    pub active_agent_ids: Vec<String>,
    #[serde(default)]
    pub execution_mode: GroupExecutionMode,
    #[serde(default)]
    pub execution_waves: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_dependencies: Option<Vec<GroupDependency>>,
}

impl GroupActivationIntent {
    pub fn inherit_tool_constraints(&mut self,previous:&Self) {
        self.tool_constraints=previous.tool_constraints.iter()
            .filter(|(actor,_)|self.active_agent_ids.contains(actor))
            .map(|(actor,tools)|(actor.clone(),tools.clone())).collect();
        self.inspection_participants=previous.inspection_participants.iter()
            .filter(|actor|self.active_agent_ids.contains(actor)).cloned().collect();
    }
}

/// A user-facing preview resolved from the current authoritative directory.
/// IDs and names have the same canonical roster order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupActivationPreview {
    #[serde(default)]
    pub inspection_participants: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub tool_constraints: std::collections::BTreeMap<String,Vec<String>>,
    pub group_id: String,
    pub roster_fingerprint: String,
    pub selection: GroupActivationSelection,
    pub active_agent_ids: Vec<String>,
    pub active_display_names: Vec<String>,
    pub execution_mode: GroupExecutionMode,
    pub execution_waves: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_dependencies: Option<Vec<GroupDependency>>,
    pub execution_wave_display_names: Vec<Vec<String>>,
}

/// Durable execution state for one explicitly activated group member. These
/// states describe evidence, not UI guesses: in particular, a quiet gateway
/// run is never promoted to `done` without a persisted contribution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupMemberActivationState {
    Queued,
    Working,
    WaitingUser,
    Blocked,
    Done,
}

impl GroupMemberActivationState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Working => "working",
            Self::WaitingUser => "waiting_user",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    pub(crate) fn from_str(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "working" => Ok(Self::Working),
            "waiting_user" => Ok(Self::WaitingUser),
            "blocked" => Ok(Self::Blocked),
            "done" => Ok(Self::Done),
            _ => anyhow::bail!("invalid group member activation state `{value}`"),
        }
    }
}

/// Immutable member identity plus its current durable execution state. The
/// snapshot is copied at send time so later renames, avatar changes, or roster
/// removals cannot rewrite who was activated for an historical turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupMemberActivationRecord {
    pub activation_id: String,
    pub participant: GroupParticipant,
    pub state: GroupMemberActivationState,
    #[serde(default)]
    pub status_detail: String,
    /// Durable evidence for states that refer to another record: the pending
    /// ask id for `waiting_user`, or the persisted group message id for
    /// `done`. Quiet completion has no valid receipt and therefore cannot be
    /// represented as done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// Saved room contribution that explicitly pinged this additional member.
    /// None for the immutable set selected by the human prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt_id: Option<String>,
    pub updated_at: String,
}

impl GroupMemberActivationRecord {
    pub fn has_committed_result(&self) -> bool {
        self.state == GroupMemberActivationState::Done
            && self.receipt_id.as_deref().is_some_and(|id| !id.trim().is_empty())
    }
}

/// Authoritative durable receipt for one group-authored turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupTurnLedgerRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<GroupActivationIntent>,
    pub canonical_session_id: String,
    pub turn_id: String,
    pub prompt_hash: String,
    pub group_id: String,
    pub roster_fingerprint: String,
    pub selection: GroupActivationSelection,
    pub active_agent_ids: Vec<String>,
    pub members: Vec<GroupMemberActivationRecord>,
    pub created_at: String,
    pub updated_at: String,
}

impl GroupTurnLedgerRecord {
    /// Queued tasks whose transitive inputs are saved or included in this
    /// continuation. Unknown legacy plans are never resumed speculatively.
    pub fn ready_frontier(&self) -> Vec<String> {
        // Legacy waves can constrain an explicit answer continuation, but
        // cannot establish whether a live dispatcher still owns queued work.
        // Background transfer requires the exact-dependency plan boundary.
        if self.activation.as_ref().and_then(|plan| plan.execution_dependencies.as_ref()).is_none() {
            return Vec::new();
        }
        self.answer_frontier("")
    }

    /// Work that one answered question can unlock. Other unanswered owners
    /// remain excluded, including joins that still require their results.
    pub fn answer_frontier(&self, asker: &str) -> Vec<String> {
        let Some(plan) = self.activation.as_ref() else {
            return self
                .members
                .iter()
                .filter(|member| {
                    member.participant.agent_id == asker
                })
                .map(|member| member.participant.agent_id.clone())
                .collect();
        };
        // Use the same legacy interpretation as transactional admission.
        // Absence of the newer edge field does not mean every queued member
        // belongs to this answer's continuation.
        let edges = plan.execution_dependencies.clone()
            .unwrap_or_else(|| legacy_wave_dependencies(&plan.execution_waves));
        let done = self
            .members
            .iter()
            .filter(|member| member.has_committed_result())
            .map(|member| member.participant.agent_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut included = std::collections::HashSet::from([asker.to_string()]);
        loop {
            let before = included.len();
            for member in &self.members {
                let id = &member.participant.agent_id;
                if member.state == GroupMemberActivationState::Queued
                    && edges
                        .iter()
                        .filter(|edge| &edge.dependent == id)
                        .all(|edge| {
                            done.contains(edge.prerequisite.as_str())
                                || included.contains(&edge.prerequisite)
                        })
                {
                    included.insert(id.clone());
                }
            }
            if included.len() == before {
                break;
            }
        }
        self.members.iter()
            .map(|member| &member.participant.agent_id)
            .filter(|id| included.contains(*id))
            .cloned().collect()
    }

    /// Empty, explicitly silent activations are complete by construction. A
    /// non-empty turn is complete only when every member has an explicit done
    /// receipt.
    pub fn is_done(&self) -> bool {
        self.members.is_empty()
            || self
                .members
                .iter()
                .all(GroupMemberActivationRecord::has_committed_result)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroupTurnLedgerReservation {
    New(GroupTurnLedgerRecord),
    Existing(GroupTurnLedgerRecord),
}

impl GroupTurnLedgerReservation {
    pub fn record(&self) -> &GroupTurnLedgerRecord {
        match self {
            Self::New(record) | Self::Existing(record) => record,
        }
    }
}

impl GroupActivationPreview {
    pub fn intent(&self) -> GroupActivationIntent {
        GroupActivationIntent {
            tool_constraints: self.tool_constraints.clone(),
            inspection_participants: self.inspection_participants.clone(),
            group_id: self.group_id.clone(),
            roster_fingerprint: self.roster_fingerprint.clone(),
            selection: self.selection,
            active_agent_ids: self.active_agent_ids.clone(),
            execution_mode: self.execution_mode,
            execution_waves: self.execution_waves.clone(),
            execution_dependencies: self.execution_dependencies.clone(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GroupMetadata {
    #[serde(default)]
    discussion_rounds: Option<u8>,
    #[serde(default)]
    read_full_transcript: Option<bool>,
}

pub fn resolve_group_turn(
    snapshot: &DirectorySnapshot,
    group_id: &str,
    user_message: &str,
) -> Result<GroupTurnContext> {
    let group = snapshot
        .groups
        .iter()
        .find(|group| group.profile.group_id == group_id)
        .with_context(|| format!("unknown group `{group_id}`"))?;
    anyhow::ensure!(
        group.profile.lifecycle == LifecycleState::Active,
        "group `{group_id}` is not active"
    );

    let metadata: GroupMetadata = if group.profile.metadata_json.trim().is_empty() {
        GroupMetadata::default()
    } else {
        serde_json::from_str(&group.profile.metadata_json)
            .with_context(|| format!("group `{group_id}` metadata is invalid"))?
    };
    let discussion_rounds = metadata.discussion_rounds.unwrap_or(1).clamp(1, 4);

    let mut members = snapshot
        .members
        .iter()
        .filter(|member| member.group_id == group_id)
        .collect::<Vec<_>>();
    members.sort_by_key(|member| {
        (
            member.sort_order,
            member.joined_at.as_str(),
            &member.agent_id,
        )
    });
    anyhow::ensure!(!members.is_empty(), "group `{group_id}` has no members");
    anyhow::ensure!(
        members.len() <= MAX_GROUP_PARTICIPANTS,
        "group `{group_id}` has too many members"
    );

    let mut participants = Vec::with_capacity(members.len());
    for member in members {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == member.agent_id)
            .with_context(|| {
                format!(
                    "group `{group_id}` references missing agent `{}`",
                    member.agent_id
                )
            })?;
        if agent.profile.lifecycle != LifecycleState::Active {
            continue;
        }
        let explicitly_mentioned = false;
        participants.push(GroupParticipant {
            agent_id: agent.profile.agent_id.clone(),
            internal_role: agent.profile.internal_role.clone(),
            display_name: agent.profile.display_name.clone(),
            role_title: agent.profile.role_title.clone(),
            color: agent.profile.color.clone(),
            icon_seed: agent.profile.icon_seed.clone(),
            avatar: serde_json::from_str::<serde_json::Value>(&agent.profile.metadata_json)
                .ok()
                .and_then(|metadata| metadata.get("avatar").cloned()),
            member_role: member.member_role.clone(),
            history_access: member.history_access,
            history_start_message_index: member.history_start_message_index,
            explicitly_mentioned,
        });
    }
    anyhow::ensure!(
        !participants.is_empty(),
        "group `{group_id}` has no active members"
    );
    // One parser for user and coworker messages: markdown-aware (code,
    // quotes and links are not pings), first-name aliases (`@Leon` for
    // "Leon Lin"), `@everyone`/`@all`, punctuation after names.
    let (mentions_everyone, explicit) = parse_room_pings(user_message, &participants);
    // Bare URLs, emails and handles are not someone's name either.
    let prose = authored_prose(user_message)
        .split_inclusive(char::is_whitespace)
        .map(|word| if word.contains("://") || word.contains('@') || word.starts_with("www.") { " " } else { word })
        .collect::<String>();
    let explicit = explicit.into_iter().map(|p| p.agent_id.clone()).collect::<std::collections::HashSet<_>>();
    for participant in &mut participants {
        participant.explicitly_mentioned = mentions_everyone
            || explicit.contains(&participant.agent_id)
            // Canvas renders an exact display-name occurrence as an inline
            // metal agent token and serializes it to @agent_id. Keep the
            // backend equally capable for other clients: `Theo` wakes Theo,
            // while `theo`, `THEO`, or `ThEo` remain ordinary prose.
            || (explicit.is_empty()
                && contains_exact_display_name(&prose, &participant.display_name));
    }

    let leader_agent_id = super::company_directory::effective_group_leader(snapshot, group_id)
        .filter(|leader| participants.iter().any(|participant| &participant.agent_id == leader));
    Ok(GroupTurnContext {
        leader_agent_id,
        tool_constraints: Default::default(),
        inspection_participants: Default::default(),
        group_id: group_id.to_string(),
        group_name: group.profile.name.clone(),
        canonical_session_id: group
            .profile
            .canonical_session_id
            .clone()
            .unwrap_or_else(|| format!("group-{group_id}")),
        participants,
        discussion_rounds,
        read_full_transcript: metadata.read_full_transcript.unwrap_or(true),
        execution_waves: Vec::new(),
        execution_dependencies: None,
    })
}

/// Resolve the exact active coworkers a message would wake, plus a stable
/// fingerprint of the whole membership/lifecycle roster used for stale-send
/// detection. A no-mention message intentionally returns empty activation
/// vectors while retaining the fingerprint.
pub fn preview_group_activation(
    snapshot: &DirectorySnapshot,
    group_id: &str,
    user_message: &str,
) -> Result<GroupActivationPreview> {
    let context = resolve_group_turn(snapshot, group_id, user_message)?;
    let selection = if addresses_everyone(user_message) {
        GroupActivationSelection::Everyone
    } else {
        GroupActivationSelection::Explicit
    };
    let mut active = context.explicitly_pinged().collect::<Vec<_>>();
    // Leader routing: an unaddressed message wakes only the group leader.
    // Explicit @mentions (and @everyone) keep waking exactly those members.
    if active.is_empty() {
        if let Some(leader) = context.leader() {
            active.push(leader);
        }
    }
    let active_agent_ids = active
        .iter()
        .map(|participant| participant.agent_id.clone())
        .collect::<Vec<_>>();
    let (mut execution_mode, mut execution_waves, mut execution_dependencies) =
        plan_execution_waves(user_message, &active_agent_ids, &context.participants)?;
    // `@everyone` in a led room: the leader re-plans and assigns FIRST, then
    // every member starts in parallel with the plan, the mode (blind diverge,
    // research, build claims) and its assignment already on the board.
    // Starting all members beside the leader made them work before any plan
    // existed, and the leader's assignment pings were no-ops (they were already
    // activated), so the mission never ran the leader's phases.
    if selection == GroupActivationSelection::Everyone && active_agent_ids.len() > 1 {
        if let Some(leader) = context.leader_agent_id.clone().filter(|id| active_agent_ids.contains(id)) {
            let mut edges = execution_dependencies
                .into_iter()
                .filter(|edge| edge.dependent != leader)
                .collect::<Vec<_>>();
            for member in active_agent_ids.iter().filter(|id| **id != leader) {
                edges.push(GroupDependency { prerequisite: leader.clone(), dependent: member.clone() });
            }
            edges.sort();
            edges.dedup();
            execution_waves = dependency_waves(&active_agent_ids, &edges)?;
            validate_execution_waves(GroupExecutionMode::Ordered, &execution_waves, &active_agent_ids)?;
            execution_mode = GroupExecutionMode::Ordered;
            execution_dependencies = edges;
        }
    }
    let execution_wave_display_names = execution_waves
        .iter()
        .map(|wave| {
            wave.iter()
                .filter_map(|agent_id| {
                    context
                        .participants
                        .iter()
                        .find(|participant| participant.agent_id == *agent_id)
                        .map(|participant| participant.display_name.clone())
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(GroupActivationPreview {
        inspection_participants: infer_inspection_assignments(user_message,&context.participants)
            .into_iter().filter(|id|active_agent_ids.contains(id)).collect(),
        tool_constraints: Default::default(),
        group_id: group_id.to_string(),
        roster_fingerprint: roster_fingerprint(snapshot, group_id)?,
        selection,
        active_agent_ids,
        active_display_names: active
            .iter()
            .map(|participant| participant.display_name.clone())
            .collect(),
        execution_mode,
        execution_waves,
        execution_dependencies: Some(execution_dependencies),
        execution_wave_display_names,
    })
}

/// Revalidate a previously previewed intent against the latest authoritative
/// roster. Any membership or lifecycle change invalidates the intent; callers
/// must refresh the preview instead of silently activating a different set.
pub fn validate_group_activation(
    snapshot: &DirectorySnapshot,
    intent: &GroupActivationIntent,
) -> Result<GroupActivationPreview> {
    anyhow::ensure!(intent.tool_constraints.len()<=MAX_GROUP_PARTICIPANTS,"too many assignment tool constraints");
    anyhow::ensure!(intent.inspection_participants.iter().all(|id|intent.active_agent_ids.contains(id)),
        "inspection assignments reference an inactive participant");
    for (actor,tools) in &intent.tool_constraints {
        anyhow::ensure!(intent.active_agent_ids.contains(actor),"tool constraints reference inactive group participant {actor}");
        anyhow::ensure!(tools.len()<=128,"too many allowed tools for {actor}");
        for tool in tools {
            let name=tool.strip_suffix('*').unwrap_or(tool);
            anyhow::ensure!(!name.is_empty()&&name.len()<=64&&name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'),"invalid tool constraint {tool}");
        }
    }
    let current_fingerprint = roster_fingerprint(snapshot, &intent.group_id)?;
    anyhow::ensure!(
        current_fingerprint == intent.roster_fingerprint,
        "group roster changed; refresh the activation preview"
    );
    let context = resolve_group_turn(snapshot, &intent.group_id, "")?;
    let active_roster = context
        .participants
        .iter()
        .map(|participant| participant.agent_id.as_str())
        .collect::<Vec<_>>();
    let requested = intent
        .active_agent_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let requested_set = requested
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        requested_set.len() == requested.len(),
        "group activation contains duplicate coworkers"
    );
    let canonical_requested = active_roster
        .iter()
        .copied()
        .filter(|agent_id| requested_set.contains(agent_id))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        canonical_requested == requested,
        "group activation contains a coworker who is not an active member or is out of roster order"
    );
    if intent.selection == GroupActivationSelection::Everyone {
        anyhow::ensure!(
            requested == active_roster,
            "@everyone activation no longer matches the current active roster"
        );
    }
    // Old clients and queued payloads did not carry a plan. Their historical
    // meaning was "all selected coworkers are independently eligible", which
    // is exactly one parallel wave—not an invalid activation.
    let execution_mode = if intent.execution_waves.is_empty() {
        GroupExecutionMode::Parallel
    } else {
        intent.execution_mode
    };
    let execution_waves =
        if intent.execution_waves.is_empty() && !intent.active_agent_ids.is_empty() {
            vec![intent.active_agent_ids.clone()]
        } else {
            intent.execution_waves.clone()
        };
    validate_execution_waves(execution_mode, &execution_waves, &intent.active_agent_ids)?;
    if let Some(dependencies) = &intent.execution_dependencies {
        let derived = dependency_waves(&intent.active_agent_ids, dependencies)?;
        anyhow::ensure!(
            derived == execution_waves,
            "group dependencies disagree with the execution preview"
        );
    }

    let active_display_names = requested
        .iter()
        .map(|agent_id| {
            context
                .participants
                .iter()
                .find(|participant| participant.agent_id == *agent_id)
                .map(|participant| participant.display_name.clone())
                .with_context(|| format!("active group member `{agent_id}` disappeared"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(GroupActivationPreview {
        inspection_participants: intent.inspection_participants.clone(),
        tool_constraints: intent.tool_constraints.clone(),
        group_id: intent.group_id.clone(),
        roster_fingerprint: current_fingerprint,
        selection: intent.selection,
        active_agent_ids: intent.active_agent_ids.clone(),
        active_display_names,
        execution_mode,
        execution_waves: execution_waves.clone(),
        execution_dependencies: intent.execution_dependencies.clone(),
        execution_wave_display_names: execution_waves
            .iter()
            .map(|wave| {
                wave.iter()
                    .filter_map(|agent_id| {
                        context
                            .participants
                            .iter()
                            .find(|participant| participant.agent_id == *agent_id)
                            .map(|participant| participant.display_name.clone())
                    })
                    .collect()
            })
            .collect(),
    })
}

fn infer_inspection_assignments(message:&str,participants:&[GroupParticipant])->std::collections::BTreeSet<String> {
    let mut result=std::collections::BTreeSet::new();
    let mut fenced=false;
    for line in message.lines() {
        let line=line.trim();
        if line.starts_with("```") {fenced=!fenced;continue;}
        if fenced||line.starts_with('>'){continue;}
        let line=line.strip_prefix("- ").unwrap_or(line);
        let Some((name,body))=line.split_once(':') else {continue;};
        let name=normalized_mention(name.trim().trim_start_matches('@'));
        let Some(participant)=participants.iter().find(|p|[&p.agent_id,&p.internal_role,&p.display_name]
            .iter().any(|candidate|normalized_mention(candidate)==name)) else {continue;};
        let first=body.split_once(". ").map_or(body,|(first,_)|first).to_ascii_lowercase();
        let words=first.split(|c:char|!c.is_ascii_alphanumeric()).filter(|word|!word.is_empty()).collect::<Vec<_>>();
        let inspect=words.iter().any(|word|["inspect","review","audit","measure","analyze","assess","compare","verify"].contains(word));
        let build=words.iter().any(|word|["edit","modify","change","create","build","repair","fix","implement","delete","write","replace","install","render","export","save"].contains(word));
        if inspect&&!build {result.insert(participant.agent_id.clone());}
        else {result.remove(&participant.agent_id);}
    }
    result
}

/// Build the executable room context from a previously validated, stable-id
/// activation decision. The roster fingerprint is checked again against this
/// final directory snapshot so a queued or delayed turn cannot silently wake
/// a different roster. Mutable prompt text and display names are deliberately
/// not consulted here.
pub fn resolve_group_turn_from_activation(
    snapshot: &DirectorySnapshot,
    intent: &GroupActivationIntent,
) -> Result<GroupTurnContext> {
    let validated = validate_group_activation(snapshot, intent)?;
    let active_agent_ids = validated
        .active_agent_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut context = resolve_group_turn(snapshot, &intent.group_id, "")?;
    for participant in &mut context.participants {
        participant.explicitly_mentioned = active_agent_ids.contains(participant.agent_id.as_str());
    }
    context.execution_waves = validated.execution_waves;
    context.execution_dependencies = validated.execution_dependencies;
    context.tool_constraints = intent.tool_constraints.clone();
    context.inspection_participants = intent.inspection_participants.clone();
    Ok(context)
}

/// Build only dependencies the user actually wrote. Mention order alone has
/// no scheduling meaning. `then`, `before`, `after`, `first`, `finally`, or an
/// explicit sequential/mention-order phrase create a small DAG; all other
/// activated coworkers share one parallel wave.
fn plan_execution_waves(
    message: &str,
    active_agent_ids: &[String],
    participants: &[GroupParticipant],
) -> Result<(GroupExecutionMode, Vec<Vec<String>>, Vec<GroupDependency>)> {
    if active_agent_ids.is_empty() {
        return Ok((GroupExecutionMode::Parallel, Vec::new(), Vec::new()));
    }
    if active_agent_ids.len() == 1 {
        return Ok((
            GroupExecutionMode::Parallel,
            vec![active_agent_ids.to_vec()],
            Vec::new(),
        ));
    }

    let lower = message.to_ascii_lowercase();
    let mut occurrences = Vec::<(usize, String)>::new();
    let mut all_occurrences = Vec::<(usize, String)>::new();
    let mut explicit_occurrences = Vec::<(usize, String)>::new();
    for agent_id in active_agent_ids {
        let Some(participant) = participants
            .iter()
            .find(|participant| participant.agent_id == *agent_id)
        else {
            continue;
        };
        let candidates = [
            format!("@{}", normalized_mention(&participant.agent_id)),
            format!("@{}", normalized_mention(&participant.internal_role)),
            format!("@{}", normalized_mention(&participant.display_name)),
            participant.display_name.to_ascii_lowercase(),
        ];
        for candidate in &candidates {
            for (position, _) in lower.match_indices(candidate) {
                all_occurrences.push((position, agent_id.clone()));
                if candidate.starts_with('@') {
                    explicit_occurrences.push((position, agent_id.clone()));
                }
            }
        }
        if let Some(position) = candidates
            .iter()
            .filter_map(|candidate| lower.find(candidate))
            .min()
        {
            occurrences.push((position, agent_id.clone()));
        }
    }
    occurrences.sort_by_key(|(position, _)| *position);
    occurrences.dedup_by(|left, right| left.1 == right.1);
    all_occurrences.sort_by_key(|(position, _)| *position);
    all_occurrences.dedup();
    explicit_occurrences.sort_by_key(|(position, _)| *position);
    explicit_occurrences.dedup();
    for agent_id in active_agent_ids {
        if !occurrences.iter().any(|(_, seen)| seen == agent_id) {
            occurrences.push((usize::MAX, agent_id.clone()));
        }
    }

    let mention_order = occurrences
        .iter()
        .map(|(_, agent_id)| agent_id.clone())
        .collect::<Vec<_>>();
    if [
        "in mention order",
        "mention order",
        "one at a time",
        "sequentially",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
    {
        let waves = mention_order
            .into_iter()
            .map(|agent_id| vec![agent_id])
            .collect::<Vec<_>>();
        let dependencies = legacy_wave_dependencies(&waves);
        return Ok((GroupExecutionMode::Ordered, waves, dependencies));
    }

    let mut edges = std::collections::HashSet::<(String, String)>::new();
    // A temporal word in task instructions is not a relation to a name in
    // another sentence/assignment. In particular, "Before deciding, ask ..."
    // must not serialize the independent coworker in the next paragraph.
    let same_clause = |left: usize, right: usize| {
        !lower[left..right].contains(['.', '!', '?', ';', '\n', '\r'])
    };
    let nearest_before = |position: usize| {
        all_occurrences
            .iter()
            .filter(|(at, _)| *at < position && same_clause(*at, position))
            .max_by_key(|(at, _)| *at)
            .map(|(_, agent_id)| agent_id.clone())
    };
    let nearest_after = |position: usize| {
        all_occurrences
            .iter()
            .filter(|(at, _)| *at > position && *at != usize::MAX && same_clause(position, *at))
            .min_by_key(|(at, _)| *at)
            .map(|(_, agent_id)| agent_id.clone())
    };
    for (keyword, reverse) in [(" then ", false), (" before ", false), (" after ", true)] {
        for (position, _) in lower.match_indices(keyword) {
            let pivot = position + keyword.len() / 2;
            if let (Some(left), Some(right)) = (nearest_before(pivot), nearest_after(pivot)) {
                let edge = if reverse {
                    (right, left)
                } else {
                    (left, right)
                };
                if edge.0 != edge.1 {
                    edges.insert(edge);
                }
            }
        }
    }
    // Natural group briefs often express a real dependency as "Theo will
    // research X so that you can build it" after directly addressing Iris.
    // The old parser ignored that coreference and launched both in one wave,
    // guaranteeing that the builder worked without the promised research.
    // Resolve "you" only to the most recently @mentioned *different* member;
    // without that explicit addressee the phrase remains parallel rather than
    // guessing at ownership.
    for (position, keyword) in lower.match_indices(" so that ") {
        let pivot = position + keyword.len() / 2;
        let source = all_occurrences
            .iter()
            .filter(|(at, _)| *at < pivot)
            .max_by_key(|(at, _)| *at)
            .cloned();
        let tail = lower[position + keyword.len()..].trim_start();
        let target = source.as_ref().and_then(|(source_at, source_id)| {
            all_occurrences
                .iter()
                .filter(|(at, agent_id)| *at > pivot && agent_id != source_id)
                .min_by_key(|(at, _)| *at)
                .map(|(_, agent_id)| agent_id.clone())
                .or_else(|| {
                    if !(tail.starts_with("you ") || tail.starts_with("you can ")) {
                        return None;
                    }
                    let (_, addressee) = explicit_occurrences
                        .iter()
                        .filter(|(at, _)| *at <= *source_at)
                        .max_by_key(|(at, _)| *at)?;
                    (addressee != source_id).then(|| addressee.clone())
                })
        });
        if let (Some((_, source)), Some(target)) = (source, target) {
            if source != target {
                edges.insert((source, target));
            }
        }
    }
    if let Some(position) = lower.find(" first") {
        if let Some(first) = nearest_before(position + 1).or_else(|| nearest_after(position)) {
            for other in active_agent_ids
                .iter()
                .filter(|agent_id| **agent_id != first)
            {
                edges.insert((first.clone(), other.clone()));
            }
        }
    }
    if let Some(position) = lower.find("finally") {
        if let Some(last) = nearest_after(position).or_else(|| nearest_before(position)) {
            for other in active_agent_ids
                .iter()
                .filter(|agent_id| **agent_id != last)
            {
                edges.insert((other.clone(), last.clone()));
            }
        }
    }
    if edges.is_empty() {
        return Ok((
            GroupExecutionMode::Parallel,
            vec![active_agent_ids.to_vec()],
            Vec::new(),
        ));
    }

    let mut dependencies = edges
        .into_iter()
        .map(|(prerequisite, dependent)| GroupDependency {
            prerequisite,
            dependent,
        })
        .collect::<Vec<_>>();
    dependencies.sort();
    let waves = dependency_waves(active_agent_ids, &dependencies)?;
    validate_execution_waves(GroupExecutionMode::Ordered, &waves, active_agent_ids)?;
    Ok((GroupExecutionMode::Ordered, waves, dependencies))
}

pub fn legacy_wave_dependencies(waves: &[Vec<String>]) -> Vec<GroupDependency> {
    waves
        .windows(2)
        .flat_map(|pair| {
            pair[0].iter().flat_map(|before| {
                pair[1].iter().map(move |after| GroupDependency {
                    prerequisite: before.clone(),
                    dependent: after.clone(),
                })
            })
        })
        .collect()
}

/// Deterministic preview only. Runtime readiness uses exact edges, not these layers.
pub fn dependency_waves(
    active_agent_ids: &[String],
    dependencies: &[GroupDependency],
) -> Result<Vec<Vec<String>>> {
    let active = active_agent_ids
        .iter()
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        active.len() == active_agent_ids.len(),
        "duplicate group task owner"
    );
    let mut unique = std::collections::HashSet::new();
    for edge in dependencies {
        anyhow::ensure!(
            edge.prerequisite != edge.dependent,
            "group task cannot depend on itself"
        );
        anyhow::ensure!(
            active.contains(&edge.prerequisite) && active.contains(&edge.dependent),
            "group dependency references an inactive member"
        );
        anyhow::ensure!(
            unique.insert((&edge.prerequisite, &edge.dependent)),
            "duplicate group dependency"
        );
    }
    let mut remaining = active_agent_ids
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let wave = active_agent_ids
            .iter()
            .filter(|agent_id| {
                remaining.contains(*agent_id)
                    && !dependencies.iter().any(|edge| {
                        &edge.dependent == *agent_id && remaining.contains(&edge.prerequisite)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !wave.is_empty(),
            "group dependency wording creates a cycle; remove the conflicting before/after order"
        );
        for agent_id in &wave {
            remaining.remove(agent_id);
        }
        waves.push(wave);
    }
    Ok(waves)
}

fn validate_execution_waves(
    mode: GroupExecutionMode,
    waves: &[Vec<String>],
    active_agent_ids: &[String],
) -> Result<()> {
    if active_agent_ids.is_empty() {
        anyhow::ensure!(
            waves.is_empty(),
            "silent group activation cannot contain execution waves"
        );
        return Ok(());
    }
    anyhow::ensure!(
        !waves.is_empty(),
        "group activation is missing its execution wave"
    );
    anyhow::ensure!(
        waves.iter().all(|wave| !wave.is_empty()),
        "group activation contains an empty execution wave"
    );
    let flattened = waves.iter().flatten().cloned().collect::<Vec<_>>();
    let expected = active_agent_ids
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let actual = flattened
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        flattened.len() == actual.len() && actual == expected,
        "group execution waves must contain every activated coworker exactly once"
    );
    if mode == GroupExecutionMode::Parallel {
        anyhow::ensure!(
            waves.len() == 1,
            "parallel group activation must use exactly one execution wave"
        );
    }
    Ok(())
}

fn roster_fingerprint(snapshot: &DirectorySnapshot, group_id: &str) -> Result<String> {
    let group = snapshot
        .groups
        .iter()
        .find(|group| group.profile.group_id == group_id)
        .with_context(|| format!("unknown group `{group_id}`"))?;
    anyhow::ensure!(
        group.profile.lifecycle == LifecycleState::Active,
        "group `{group_id}` is not active"
    );
    let mut members = snapshot
        .members
        .iter()
        .filter(|member| member.group_id == group_id)
        .collect::<Vec<_>>();
    members.sort_by_key(|member| {
        (
            member.sort_order,
            member.joined_at.as_str(),
            member.agent_id.as_str(),
        )
    });
    anyhow::ensure!(!members.is_empty(), "group `{group_id}` has no members");
    anyhow::ensure!(
        members.len() <= MAX_GROUP_PARTICIPANTS,
        "group `{group_id}` has too many members"
    );

    let mut hasher = Sha256::new();
    hasher.update(b"phoenix-group-roster-v1\0");
    hasher.update(group_id.as_bytes());
    hasher.update([0]);
    for member in members {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.profile.agent_id == member.agent_id)
            .with_context(|| {
                format!(
                    "group `{group_id}` references missing agent `{}`",
                    member.agent_id
                )
            })?;
        let lifecycle = match agent.profile.lifecycle {
            LifecycleState::Active => "active",
            LifecycleState::Dormant => "dormant",
            LifecycleState::Archived => "archived",
            LifecycleState::PendingDeletion => "pending_deletion",
        };
        hasher.update(member.sort_order.to_le_bytes());
        hasher.update(member.as_of_seq.to_le_bytes());
        hasher.update(member.agent_id.as_bytes());
        hasher.update([0]);
        hasher.update(lifecycle.as_bytes());
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

impl GroupTurnContext {
    pub fn dependencies(&self) -> Vec<GroupDependency> {
        self.execution_dependencies
            .clone()
            .unwrap_or_else(|| legacy_wave_dependencies(&self.execution_waves))
    }
    /// The user controls who speaks in a room. Other members remain visible in
    /// the roster and transcript, but stay asleep until the user or an active
    /// coworker explicitly pings them.
    pub fn explicitly_pinged(&self) -> impl Iterator<Item = &GroupParticipant> {
        self.participants
            .iter()
            .filter(|participant| participant.explicitly_mentioned)
    }

    /// Resolve the stable-id plan into immutable participant snapshots. Old
    /// queued payloads predate execution waves; they safely retain the new
    /// default by treating every explicitly pinged coworker as one parallel
    /// wave.
    pub fn executable_waves(&self) -> Vec<Vec<&GroupParticipant>> {
        let fallback;
        let waves = if self.execution_waves.is_empty() {
            fallback = vec![self
                .explicitly_pinged()
                .map(|participant| participant.agent_id.clone())
                .collect::<Vec<_>>()];
            &fallback
        } else {
            &self.execution_waves
        };
        waves
            .iter()
            .filter_map(|wave| {
                let participants = wave
                    .iter()
                    .filter_map(|agent_id| {
                        self.participants
                            .iter()
                            .find(|participant| participant.agent_id == *agent_id)
                    })
                    .collect::<Vec<_>>();
                (!participants.is_empty()).then_some(participants)
            })
            .collect()
    }
}

/// The authored prose of a markdown message: quotes, code, links, images and
/// HTML are blanked, so nothing inside them reads as a ping or a name.
fn authored_prose(body: &str) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut prose = String::new();
    let mut suppressed = 0usize;
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::BlockQuote | Tag::CodeBlock(_) | Tag::Link { .. } | Tag::Image { .. }) => {
                suppressed += 1; prose.push(' ');
            }
            Event::End(TagEnd::BlockQuote | TagEnd::CodeBlock | TagEnd::Link | TagEnd::Image) => {
                suppressed = suppressed.saturating_sub(1); prose.push(' ');
            }
            Event::Text(text) if suppressed == 0 => prose.push_str(&text),
            Event::SoftBreak | Event::HardBreak => prose.push('\n'),
            Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => prose.push(' '),
            Event::End(_) => prose.push(' '),
            _ => {}
        }
    }
    prose
}

/// Explicit `@` pings in authored prose: `(addresses_everyone, members)`.
/// Quoted examples, code, links, images and email addresses are never pings.
/// Names match the agent id, internal role, full display name, or the
/// display name's first word (`@Leon` for "Leon Lin"); the longest alias wins
/// and an alias shared by two members pings nobody. `@everyone`/`@all` set the
/// flag. This deliberately does not use the legacy display-name prose
/// heuristic.
pub(crate) fn parse_room_pings<'a>(
    body: &str, participants: &'a [GroupParticipant],
) -> (bool, Vec<&'a GroupParticipant>) {
    let prose = authored_prose(body).to_lowercase();
    let word = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '-');
    let mut everyone = false;
    let mut found = std::collections::HashSet::new();
    for (at, _) in prose.match_indices('@') {
        if prose[..at].chars().next_back().is_some_and(|ch| word(ch) || matches!(ch, '.' | '/' | '@')) {
            continue;
        }
        let tail = &prose[at + 1..];
        if ["everyone", "all"].iter().any(|alias| tail.starts_with(alias)
            && tail[alias.len()..].chars().next().is_none_or(|ch| !word(ch))) {
            everyone = true;
            continue;
        }
        let mut candidates = Vec::new();
        for member in participants {
            let first_name = member.display_name.split_whitespace().next().unwrap_or("").to_string();
            for name in [&member.agent_id, &member.internal_role, &member.display_name, &first_name] {
                for alias in [name.to_lowercase(), normalized_mention(name)] {
                    if !alias.is_empty() && tail.starts_with(&alias)
                        && tail[alias.len()..].chars().next().is_none_or(|ch| !word(ch)) {
                        candidates.push((alias.len(), member.agent_id.as_str()));
                    }
                }
            }
        }
        if let Some(longest) = candidates.iter().map(|(len, _)| *len).max() {
            let ids = candidates.iter().filter(|(len, _)| *len == longest)
                .map(|(_, id)| *id).collect::<std::collections::HashSet<_>>();
            // A duplicate display name is not permission to choose an identity.
            if ids.len() == 1 { found.extend(ids); }
        }
    }
    (everyone, participants.iter().filter(|member| found.contains(member.agent_id.as_str())).collect())
}

/// Members an authored room contribution pings (`@everyone` = all of them).
pub(crate) fn authored_ping_targets<'a>(
    body: &str, participants: &'a [GroupParticipant],
) -> Vec<&'a GroupParticipant> {
    match parse_room_pings(body, participants) {
        (true, _) => participants.iter().collect(),
        (false, explicit) => explicit,
    }
}

/// Does this room message address everyone (`@everyone` / `@all`)?
pub fn addresses_everyone(message: &str) -> bool {
    parse_room_pings(message, &[]).0
}

fn normalized_mention(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

fn contains_exact_display_name(message: &str, display_name: &str) -> bool {
    if display_name.is_empty() {
        return false;
    }
    message.match_indices(display_name).any(|(start, matched)| {
        let before = message[..start].chars().next_back();
        let end = start + matched.len();
        let after = message[end..].chars().next();
        before.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
            && after.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn authored_room_first_names_match_the_room_tool_guard_without_guessing() {
        let mut group = resolve_group_turn(&snapshot(), "launch", "@nico work").unwrap();
        let theo = group.participants.iter().position(|member| member.agent_id == "theo").unwrap();
        let nico = group.participants.iter().position(|member| member.agent_id == "nico").unwrap();
        group.participants[theo].display_name = "Leon Lin".into();
        assert_eq!(authored_ping_targets("@Leon join", &group.participants)[0].agent_id, "theo");
        assert!(group.names_member("@Leon"));
        assert!(group.names_member("leon_lin"));
        group.participants[nico].display_name = "Leon Smith".into();
        assert!(authored_ping_targets("@Leon join", &group.participants).is_empty());
        assert!(!group.names_member("@Leon"));
        assert!(group.names_member("researcher"));
    }
    #[test]
    fn authored_room_pings_resolve_display_name_id_and_internal_role() {
        let group=resolve_group_turn(&snapshot(),"launch", "@nico work").unwrap();
        for body in ["@Theo", "@researcher Theo, come join us here.", "(@THEO), hello", "**@Theo**", "@theo @researcher"] {
            assert_eq!(authored_ping_targets(body, &group.participants).iter().map(|m|m.agent_id.as_str()).collect::<Vec<_>>(), vec!["theo"], "{body}");
        }
        assert_eq!(authored_ping_targets("@everyone join", &group.participants).len(), 2);
    }

    #[test]
    fn authored_room_pings_ignore_quoted_code_links_emails_and_prose_names() {
        let group=resolve_group_turn(&snapshot(),"launch", "@nico work").unwrap();
        for body in ["Theo already finished", "`@Theo`", "```\n@Theo\n```", "> @Theo, come join us", "[profile @Theo](https://example.com)", "a@theo.com", "https://example.com/@Theo", "@Theo_extra", "@Theodore"] {
            assert!(authored_ping_targets(body, &group.participants).is_empty(), "{body}");
        }
        assert_eq!(authored_ping_targets("> @nico is a quote\n\n@Theo join", &group.participants).iter().map(|m|m.agent_id.as_str()).collect::<Vec<_>>(),vec!["theo"]);
    }

    #[test]
    fn authored_room_pings_use_longest_name_and_refuse_ambiguous_names() {
        let mut group=resolve_group_turn(&snapshot(),"launch", "@nico work").unwrap();
        let nico=group.participants.iter().position(|member|member.agent_id=="nico").unwrap();
        group.participants[nico].display_name="Theo Smith".into();
        assert_eq!(authored_ping_targets("@Theo Smith, join us", &group.participants)[0].agent_id,"nico");
        group.participants[nico].display_name="Theo".into();
        assert!(authored_ping_targets("@Theo join", &group.participants).is_empty());
        assert_eq!(authored_ping_targets("@researcher join", &group.participants)[0].agent_id,"theo");
    }

    #[test]
    fn explicit_pings_do_not_reactivate_completed_people_named_in_prose() {
        let snapshot=snapshot();
        let preview=preview_group_activation(&snapshot,"launch","@Theo inspect the output. Nico already completed the build; do not rerun it.").unwrap();
        assert_eq!(preview.active_agent_ids,vec!["theo"]);
        let legacy=preview_group_activation(&snapshot,"launch","Theo inspect the output").unwrap();
        assert_eq!(legacy.active_agent_ids,vec!["theo"]);
    }

    #[test]
    fn named_inspection_briefs_flow_through_normal_preview() {
        let snapshot=snapshot();
        let preview=preview_group_activation(&snapshot,"launch",
            "@Theo @Nico\nTheo: Independently inspect the saved scene. Do not edit files.\nNico: Review the findings and repair the scene.").unwrap();
        assert_eq!(preview.inspection_participants,std::collections::BTreeSet::from(["theo".into()]));
        let context=resolve_group_turn_from_activation(&snapshot,&preview.intent()).unwrap();
        assert!(context.is_inspection("researcher"));
        assert!(!context.is_inspection("coder"));
        let quoted=preview_group_activation(&snapshot,"launch",
            "@Theo @Nico\n> Theo: inspect this quoted example\n```\nNico: inspect a code example\n```").unwrap();
        assert!(quoted.inspection_participants.is_empty());
    }

    #[test]
    fn assignment_tool_constraints_survive_activation_roundtrip() {
        let snapshot=snapshot();
        let mut intent=preview_group_activation(&snapshot,"launch","@Theo then @Nico").unwrap().intent();
        intent.tool_constraints.insert("nico".into(),vec!["computer_*".into(),"image_analyze".into()]);
        intent.inspection_participants.insert("theo".into());
        let restored:GroupActivationIntent=serde_json::from_slice(&serde_json::to_vec(&intent).unwrap()).unwrap();
        let context=resolve_group_turn_from_activation(&snapshot,&restored).unwrap();
        assert!(context.permits_tool("coder","computer_act"));
        assert!(context.permits_tool("nico","image_analyze"));
        assert!(!context.permits_tool("coder","bash"));
        assert!(!context.permits_tool("coder","skill_install"));
        assert!(context.permits_tool("coder","final_answer"));
        assert!(context.permits_tool("researcher","bash"));
        assert!(context.is_inspection("researcher"));
        assert!(!context.is_inspection("coder"));
        let mut resumed=preview_group_activation(&snapshot,"launch","@Nico").unwrap().intent();
        resumed.inherit_tool_constraints(&restored);
        let resumed_context=resolve_group_turn_from_activation(&snapshot,&resumed).unwrap();
        assert!(!resumed_context.permits_tool("coder","skill_install"));
        assert!(resumed_context.permits_tool("coder","computer_act"));
        assert!(resumed.inspection_participants.is_empty());
        intent.tool_constraints.insert("outsider".into(),vec!["bash".into()]);
        assert!(validate_group_activation(&snapshot,&intent).is_err());
    }

    #[test]
    fn dependent_handoffs_follow_transitive_group_edges() {
        use super::*;
        let edges=vec![GroupDependency{prerequisite:"inspector".into(),dependent:"builder".into()},
            GroupDependency{prerequisite:"builder".into(),dependent:"reviewer".into()}];
        assert!(dependency_reachable(&edges,"inspector","builder"));
        assert!(dependency_reachable(&edges,"inspector","reviewer"));
        assert!(!dependency_reachable(&edges,"builder","inspector"));
        assert!(!dependency_reachable(&edges,"inspector","independent"));
    }

    #[test]
    fn handoff_instruction_comes_from_current_dependencies_not_history() {
        let snapshot=snapshot();
        let intent=preview_group_activation(&snapshot,"launch","@Theo then @Nico").unwrap().intent();
        let mut context=resolve_group_turn_from_activation(&snapshot,&intent).unwrap();
        let instruction=context.publication_handoff_instruction("researcher").unwrap();
        assert!(instruction.contains("Nico"));
        assert!(instruction.contains("publish final_answer"));
        assert_eq!(context.publication_handoff_instruction("theo"),Some(instruction.clone()));
        assert!(context.publication_handoff_instruction("coder").is_none());
        assert!(context.publication_handoff_instruction("outsider").is_none());
        context.execution_dependencies=None;
        assert_eq!(context.publication_handoff_instruction("researcher"),Some(instruction));
        context.execution_dependencies=Some(vec![]);
        assert!(context.publication_handoff_instruction("researcher").is_none());
    }

    use super::*;
    use crate::runtime::company_directory::{
        AgentKind, AgentProfile, AgentRecord, GroupMemberRecord, GroupProfile, GroupRecord,
    };

    fn snapshot() -> DirectorySnapshot {
        let agent = |id: &str, role: &str, name: &str, sort_order: i64| AgentRecord {
            profile: AgentProfile {
                agent_id: id.into(),
                internal_role: role.into(),
                display_name: name.into(),
                role_title: role.into(),
                description: "test coworker".into(),
                color: "#112233".into(),
                icon_seed: id.into(),
                kind: AgentKind::CraftSpecialist,
                lifecycle: LifecycleState::Active,
                pinned: false,
                sort_order,
                canonical_session_id: None,
                browser_profile_id: format!("agent-{id}"),
                metadata_json: "{}".into(),
            },
            archived_at: None,
            delete_after: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            as_of_seq: 1,
        };
        DirectorySnapshot {
            agents: vec![
                agent("nico", "coder", "Nico", 2),
                agent("theo", "researcher", "Theo", 1),
            ],
            groups: vec![GroupRecord {
                profile: GroupProfile {
                    group_id: "launch".into(),
                    name: "Launch".into(),
                    description: "Launch team".into(),
                    color: "#334455".into(),
                    icon_seed: "launch".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: true,
                    sort_order: 1,
                    canonical_session_id: Some("group-launch".into()),
                    metadata_json: r#"{"discussion_rounds":2}"#.into(),
                    leader_agent_id: None,
                },
                archived_at: None,
                delete_after: None,
                created_at: "now".into(),
                updated_at: "now".into(),
                as_of_seq: 1,
            }],
            members: vec![
                GroupMemberRecord {
                    group_id: "launch".into(),
                    agent_id: "nico".into(),
                    member_role: "builder".into(),
                    history_access: HistoryAccess::Full,
                    history_start_message_index: 0,
                    sort_order: 1,
                    joined_at: "now".into(),
                    as_of_seq: 1,
                },
                GroupMemberRecord {
                    group_id: "launch".into(),
                    agent_id: "theo".into(),
                    member_role: "research".into(),
                    history_access: HistoryAccess::Full,
                    history_start_message_index: 0,
                    sort_order: 0,
                    joined_at: "now".into(),
                    as_of_seq: 1,
                },
            ],
            ..DirectorySnapshot::default()
        }
    }

    #[test]
    fn roster_is_shared_but_only_mentions_wake_coworkers() {
        let plan =
            resolve_group_turn(&snapshot(), "launch", "@Nico please pressure-test it").unwrap();
        assert_eq!(plan.canonical_session_id, "group-launch");
        assert_eq!(plan.discussion_rounds, 2);
        assert_eq!(plan.participants.len(), 2);
        assert_eq!(plan.participants[0].display_name, "Theo");
        assert!(
            plan.participants
                .iter()
                .find(|participant| participant.display_name == "Nico")
                .unwrap()
                .explicitly_mentioned
        );
        assert_eq!(
            plan.explicitly_pinged()
                .map(|participant| participant.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Nico"]
        );
    }

    #[test]
    fn exact_display_name_wakes_without_at_but_wrong_case_does_not() {
        let exact = resolve_group_turn(&snapshot(), "launch", "Theo please inspect this").unwrap();
        assert_eq!(
            exact
                .explicitly_pinged()
                .map(|participant| participant.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Theo"]
        );
        for near_miss in [
            "theo please inspect this",
            "THEO please inspect this",
            "ThEo please inspect this",
            "Theodore can inspect this",
        ] {
            assert_eq!(
                resolve_group_turn(&snapshot(), "launch", near_miss)
                    .unwrap()
                    .explicitly_pinged()
                    .count(),
                0,
                "{near_miss:?} must remain ordinary prose"
            );
        }
    }

    #[test]
    fn archived_members_do_not_participate() {
        let mut snapshot = snapshot();
        snapshot.agents[0].profile.lifecycle = LifecycleState::Archived;
        let plan = resolve_group_turn(&snapshot, "launch", "hello").unwrap();
        assert_eq!(plan.participants.len(), 1);
        assert_eq!(plan.participants[0].display_name, "Theo");
    }

    #[test]
    fn everyone_wakes_exactly_the_current_active_roster() {
        let mut directory = snapshot();
        directory.agents[0].profile.lifecycle = LifecycleState::Archived;
        let plan = resolve_group_turn(&directory, "launch", "@Everyone, please check").unwrap();
        assert_eq!(
            plan.explicitly_pinged()
                .map(|participant| participant.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["theo"]
        );
        let near_miss = resolve_group_turn(&snapshot(), "launch", "@everyoneish hello").unwrap();
        assert_eq!(near_miss.explicitly_pinged().count(), 0);
    }

    #[test]
    fn multiple_mentions_share_one_parallel_wave_by_default() {
        let preview = preview_group_activation(
            &snapshot(),
            "launch",
            "@Theo research the behavior and @Nico inspect the implementation",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Parallel);
        assert_eq!(
            preview.execution_waves,
            vec![vec!["theo".to_string(), "nico".to_string()]]
        );
        assert_eq!(
            preview.execution_wave_display_names,
            vec![vec!["Theo".to_string(), "Nico".to_string()]]
        );
    }

    #[test]
    fn explicit_first_then_language_creates_ordered_waves() {
        let preview = preview_group_activation(
            &snapshot(),
            "launch",
            "@Theo first, then @Nico implements from Theo's result",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Ordered);
        assert_eq!(
            preview.execution_waves,
            vec![vec!["theo".to_string()], vec!["nico".to_string()]]
        );
    }

    #[test]
    fn explicit_after_language_reverses_written_subject_order() {
        let preview = preview_group_activation(
            &snapshot(),
            "launch",
            "@Nico works after @Theo finishes the research",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Ordered);
        assert_eq!(
            preview.execution_waves,
            vec![vec!["theo".to_string()], vec!["nico".to_string()]]
        );
    }

    #[test]
    fn so_that_you_can_uses_the_last_explicit_addressee_as_the_dependent() {
        let preview = preview_group_activation(
            &snapshot(),
            "launch",
            "@Theo research the reference. @Nico you design the cursor. Theo will also research how the cursor looks so that you can build something similar.",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Ordered);
        assert_eq!(
            preview.execution_waves,
            vec![vec!["theo".to_string()], vec!["nico".to_string()]]
        );
    }

    #[test]
    fn repeated_names_preserve_each_explicit_dependency() {
        let mut directory = snapshot();
        let mut iris = directory.agents[0].clone();
        iris.profile.agent_id = "iris".into();
        iris.profile.internal_role = "frontend".into();
        iris.profile.display_name = "Iris".into();
        iris.profile.sort_order = 3;
        directory.agents.push(iris);
        let mut member = directory.members[0].clone();
        member.agent_id = "iris".into();
        member.sort_order = 3;
        directory.members.push(member);
        let preview = preview_group_activation(&directory, "launch",
            "@Theo before @Nico; @Iris before @Nico. Each contributor owns its own task.").unwrap();
        assert_eq!(preview.execution_waves, vec![vec!["theo".to_string(), "iris".to_string()], vec!["nico".to_string()]]);
        assert_eq!(preview.execution_dependencies, Some(vec![
            GroupDependency { prerequisite: "iris".into(), dependent: "nico".into() },
            GroupDependency { prerequisite: "theo".into(), dependent: "nico".into() },
        ]));
    }

    #[test]
    fn temporal_words_in_separate_assignments_do_not_serialize_peers() {
        for instruction in [
            "Before deciding, ask a tolerance question.",
            "After receiving the answer, calculate the result.",
            "Read the report then decide.",
        ] {
            let prompt = format!("@Theo and @Nico work independently in parallel.\nTheo: {instruction}\nNico: independently audit the verifier.");
            let preview = preview_group_activation(&snapshot(), "launch", &prompt).unwrap();
            assert_eq!(preview.execution_mode, GroupExecutionMode::Parallel, "{prompt}");
            assert_eq!(preview.execution_dependencies, Some(Vec::new()), "{prompt}");
        }
    }

    #[test]
    fn real_three_person_brief_starts_independent_coder_with_research_then_frontend() {
        let mut directory = snapshot();
        let mut iris = directory.agents[0].clone();
        iris.profile.agent_id = "iris".into();
        iris.profile.internal_role = "frontend".into();
        iris.profile.display_name = "Iris".into();
        iris.profile.sort_order = 3;
        directory.agents.push(iris);
        let mut member = directory.members[0].clone();
        member.agent_id = "iris".into();
        member.member_role = "frontend".into();
        member.sort_order = 3;
        directory.members.push(member);

        let preview = preview_group_activation(
            &directory,
            "launch",
            "@Theo checks the web. @Nico inspect the codebase. @Iris design the cursor. Theo will also research exactly how the reference cursor looks so that you can build something similar.",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Ordered);
        assert_eq!(
            preview.execution_waves,
            vec![
                vec!["theo".to_string(), "nico".to_string()],
                vec!["iris".to_string()]
            ]
        );
        assert_eq!(
            preview.execution_dependencies,
            Some(vec![GroupDependency {
                prerequisite: "theo".into(),
                dependent: "iris".into(),
            }])
        );
        let encoded = serde_json::to_string(&preview.intent()).unwrap();
        let decoded: GroupActivationIntent = serde_json::from_str(&encoded).unwrap();
        let restored = resolve_group_turn_from_activation(&directory, &decoded).unwrap();
        assert_eq!(
            restored.dependencies(),
            preview.execution_dependencies.unwrap()
        );
    }

    #[test]
    fn so_that_you_without_a_distinct_explicit_addressee_does_not_guess() {
        let preview = preview_group_activation(
            &snapshot(),
            "launch",
            "@Nico inspect the code independently. @Theo research the reference so that you can explain it to me",
        )
        .unwrap();
        assert_eq!(preview.execution_mode, GroupExecutionMode::Parallel);
    }

    #[test]
    fn executable_context_keeps_the_previewed_wave_plan() {
        let directory = snapshot();
        let intent = preview_group_activation(&directory, "launch", "@Theo first, then @Nico")
            .unwrap()
            .intent();
        let context = resolve_group_turn_from_activation(&directory, &intent).unwrap();
        assert_eq!(
            context
                .executable_waves()
                .into_iter()
                .map(|wave| wave
                    .into_iter()
                    .map(|participant| participant.agent_id.as_str())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec!["theo"], vec!["nico"]]
        );
    }

    #[test]
    fn activation_preview_uses_stable_ids_and_revalidates_mutable_names() {
        let directory = snapshot();
        // Unaddressed messages wake only the leader (default: first member
        // by sort order when the chief of staff is not in the room).
        let unaddressed = preview_group_activation(&directory, "launch", "hello room").unwrap();
        assert_eq!(unaddressed.selection, GroupActivationSelection::Explicit);
        assert_eq!(unaddressed.active_agent_ids, vec!["theo"]);
        assert_eq!(unaddressed.active_display_names, vec!["Theo"]);

        let preview =
            preview_group_activation(&directory, "launch", "@nico please review").unwrap();
        assert_eq!(preview.active_agent_ids, vec!["nico"]);
        assert_eq!(preview.active_display_names, vec!["Nico"]);
        let intent = preview.intent();

        let mut renamed = directory.clone();
        renamed.agents[0].profile.display_name = "Nicholas".into();
        let validated = validate_group_activation(&renamed, &intent).unwrap();
        assert_eq!(validated.active_agent_ids, vec!["nico"]);
        assert_eq!(validated.active_display_names, vec!["Nicholas"]);
        assert_eq!(validated.roster_fingerprint, preview.roster_fingerprint);
    }

    #[test]
    fn executable_activation_uses_stable_ids_and_preserves_silence() {
        let directory = snapshot();
        let nico = preview_group_activation(&directory, "launch", "@nico review this")
            .unwrap()
            .intent();

        let mut renamed = directory.clone();
        renamed.agents[0].profile.display_name = "Nicholas".into();
        let executable = resolve_group_turn_from_activation(&renamed, &nico).unwrap();
        assert_eq!(
            executable
                .explicitly_pinged()
                .map(|participant| (
                    participant.agent_id.as_str(),
                    participant.display_name.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![("nico", "Nicholas")]
        );

        let unaddressed = preview_group_activation(&directory, "launch", "hello room")
            .unwrap()
            .intent();
        let executable = resolve_group_turn_from_activation(&directory, &unaddressed).unwrap();
        assert_eq!(
            executable.explicitly_pinged().map(|p| p.agent_id.as_str()).collect::<Vec<_>>(),
            vec!["theo"]
        );
        // A stored legacy silent intent (no members) still validates and stays silent.
        let mut legacy_silent = unaddressed.clone();
        legacy_silent.active_agent_ids.clear();
        legacy_silent.execution_waves.clear();
        legacy_silent.execution_dependencies = Some(Vec::new());
        let executable = resolve_group_turn_from_activation(&directory, &legacy_silent).unwrap();
        assert_eq!(executable.explicitly_pinged().count(), 0);
    }

    #[test]
    fn activation_validation_rejects_stale_membership_and_lifecycle() {
        let directory = snapshot();
        let preview = preview_group_activation(&directory, "launch", "@everyone").unwrap();
        assert_eq!(preview.selection, GroupActivationSelection::Everyone);
        assert_eq!(preview.active_agent_ids, vec!["theo", "nico"]);

        let mut lifecycle_changed = directory.clone();
        lifecycle_changed.agents[0].profile.lifecycle = LifecycleState::Dormant;
        let lifecycle_error =
            validate_group_activation(&lifecycle_changed, &preview.intent()).unwrap_err();
        assert!(lifecycle_error.to_string().contains("roster changed"));

        let mut membership_changed = directory;
        membership_changed
            .members
            .retain(|member| member.agent_id != "nico");
        let membership_error =
            validate_group_activation(&membership_changed, &preview.intent()).unwrap_err();
        assert!(membership_error.to_string().contains("roster changed"));
    }

    #[test]
    fn unaddressed_messages_wake_only_the_leader_and_mentions_wake_only_the_mentioned() {
        let mut directory = snapshot();
        directory.groups[0].profile.leader_agent_id = Some("nico".into());
        let unaddressed = preview_group_activation(&directory, "launch", "what should we ship?").unwrap();
        assert_eq!(unaddressed.active_agent_ids, vec!["nico"]);
        let context = resolve_group_turn(&directory, "launch", "").unwrap();
        assert!(context.is_leader("coder"));
        assert!(!context.is_leader("researcher"));
        // Explicit mention of a non-leader does not force the leader in.
        let mentioned = preview_group_activation(&directory, "launch", "@Theo dig into pricing").unwrap();
        assert_eq!(mentioned.active_agent_ids, vec!["theo"]);
        let everyone = preview_group_activation(&directory, "launch", "@everyone thoughts?").unwrap();
        assert_eq!(everyone.active_agent_ids, vec!["theo", "nico"]);
        // A stored leader that left the room falls back to the default rule.
        directory.groups[0].profile.leader_agent_id = Some("ghost".into());
        let fallback = preview_group_activation(&directory, "launch", "hello").unwrap();
        assert_eq!(fallback.active_agent_ids, vec!["theo"]);
    }

    #[test]
    fn chief_of_staff_leads_by_default_when_a_member() {
        let mut directory = snapshot();
        let mut phoenix = directory.agents[0].clone();
        phoenix.profile.agent_id = "phoenix".into();
        phoenix.profile.internal_role = "phoenix".into();
        phoenix.profile.display_name = "Tibo".into();
        directory.agents.push(phoenix);
        let mut member = directory.members[0].clone();
        member.agent_id = "phoenix".into();
        member.sort_order = 9;
        directory.members.push(member);
        let preview = preview_group_activation(&directory, "launch", "plan the launch").unwrap();
        assert_eq!(preview.active_agent_ids, vec!["phoenix"]);
        let context = resolve_group_turn(&directory, "launch", "").unwrap();
        assert!(context.is_leader("orchestrator"));
        assert!(context.room_instruction("phoenix").contains("GROUP LEADER PROTOCOL"));
        assert!(context.room_instruction("researcher").contains("GROUP MEMBER — Tibo leads"));
    }

    #[test]
    fn legacy_groups_larger_than_the_new_creation_cap_remain_readable() {
        let mut directory = snapshot();
        for index in 0..5 {
            let agent_id = format!("legacy-{index}");
            let mut agent = directory.agents[0].clone();
            agent.profile.agent_id = agent_id.clone();
            agent.profile.internal_role = format!("legacy_role_{index}");
            agent.profile.display_name = format!("Legacy {index}");
            agent.profile.browser_profile_id = format!("agent-{agent_id}");
            agent.profile.icon_seed = agent_id.clone();
            directory.agents.push(agent);

            let mut member = directory.members[0].clone();
            member.agent_id = agent_id;
            member.sort_order = (index + 2) as i64;
            directory.members.push(member);
        }
        let plan = resolve_group_turn(&directory, "launch", "@everyone").unwrap();
        assert_eq!(plan.participants.len(), 7);
        assert_eq!(plan.explicitly_pinged().count(), 7);
    }

    // ── Room delivery while working: everyone hears, only some act ────────

    /// Launch room plus a Phoenix leader (Tibo) and a frontend member (Leon).
    fn led_snapshot() -> DirectorySnapshot {
        let mut directory = snapshot();
        for (id, role, name, order) in [("tibo", "phoenix", "Tibo", 0), ("leon", "frontend", "Leon", 3)] {
            let mut agent = directory.agents[0].clone();
            agent.profile.agent_id = id.into();
            agent.profile.internal_role = role.into();
            agent.profile.display_name = name.into();
            agent.profile.icon_seed = id.into();
            agent.profile.sort_order = order;
            directory.agents.push(agent);
            let mut member = directory.members[0].clone();
            member.agent_id = id.into();
            member.sort_order = if id == "tibo" { -1 } else { 3 };
            directory.members.push(member);
        }
        directory.groups[0].profile.leader_agent_id = Some("tibo".into());
        directory
    }

    /// Plan delivery for `message` with the given lanes running.
    fn deliver(message: &str, running: &[&str]) -> Vec<(String, RoomDelivery)> {
        let directory = led_snapshot();
        let intent = preview_group_activation(&directory, "launch", message).unwrap().intent();
        let context = resolve_group_turn(&directory, "launch", message).unwrap();
        plan_room_delivery(&context, &intent.active_agent_ids, addresses_everyone(message), |lane| running.contains(&lane))
            .into_iter()
            .map(|member| (member.agent_id, member.delivery))
            .collect()
    }

    use crate::runtime::postbox::RoomSteerKind as Kind;

    #[test]
    fn unaddressed_message_goes_to_the_running_leader_and_everyone_else_hears_it() {
        let plan = deliver("make it dark mode", &["orchestrator", "coder", "frontend"]);
        assert_eq!(plan, vec![
            ("tibo".to_string(), RoomDelivery::Steer(Kind::LeaderAct)),
            ("nico".to_string(), RoomDelivery::Steer(Kind::Fyi)),
            ("leon".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ]);
        // Theo is idle and not addressed: not woken (reads the transcript later).
        assert!(!plan.iter().any(|(id, _)| id == "theo"));
    }

    #[test]
    fn unaddressed_message_starts_an_idle_leader() {
        let plan = deliver("what's the status?", &["coder"]);
        assert_eq!(plan, vec![
            ("tibo".to_string(), RoomDelivery::Start),
            ("nico".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ]);
        // Fully idle room: only the leader acts.
        assert_eq!(deliver("hello team", &[]), vec![("tibo".to_string(), RoomDelivery::Start)]);
    }

    #[test]
    fn mention_to_a_busy_member_is_top_priority_and_the_leader_only_hears_it() {
        let plan = deliver("@Nico switch the API to GraphQL", &["orchestrator", "coder"]);
        assert_eq!(plan, vec![
            ("tibo".to_string(), RoomDelivery::Steer(Kind::LeaderFyi)),
            ("nico".to_string(), RoomDelivery::Steer(Kind::MentionAct)),
        ]);
    }

    #[test]
    fn mention_to_an_idle_member_starts_it_while_another_member_keeps_running() {
        let plan = deliver("@Leon sketch the landing page", &["coder"]);
        assert_eq!(plan, vec![
            ("nico".to_string(), RoomDelivery::Steer(Kind::Fyi)),
            ("leon".to_string(), RoomDelivery::Start),
        ]);
        // Only Leon is started; the running member is not part of the new turn.
        let directory = led_snapshot();
        let intent = preview_group_activation(&directory, "launch", "@Leon sketch the landing page").unwrap().intent();
        let narrowed = narrow_activation(&intent, &["leon".to_string()]);
        assert_eq!(narrowed.active_agent_ids, vec!["leon".to_string()]);
        resolve_group_turn_from_activation(&directory, &narrowed).expect("a started subset is a valid activation");
    }

    #[test]
    fn several_mentions_each_act_and_the_leader_stays_out() {
        let plan = deliver("@Theo find prior art and @Nico prototype it", &["orchestrator", "researcher"]);
        assert_eq!(plan, vec![
            ("tibo".to_string(), RoomDelivery::Steer(Kind::LeaderFyi)),
            ("theo".to_string(), RoomDelivery::Steer(Kind::MentionAct)),
            ("nico".to_string(), RoomDelivery::Start),
        ]);
    }

    #[test]
    fn everyone_and_all_steer_running_members_wake_idle_ones_and_replan() {
        for message in ["@everyone switch to dark mode", "@all switch to dark mode"] {
            let plan = deliver(message, &["orchestrator", "coder"]);
            assert_eq!(plan, vec![
                ("tibo".to_string(), RoomDelivery::Steer(Kind::EveryoneReplan)),
                ("theo".to_string(), RoomDelivery::Start),
                ("nico".to_string(), RoomDelivery::Steer(Kind::EveryoneAct)),
                ("leon".to_string(), RoomDelivery::Start),
            ], "{message}");
        }
        assert_eq!(deliver("@everyone hi", &[]).iter().filter(|(_, d)| *d == RoomDelivery::Start).count(), 4);
    }

    #[test]
    fn leader_alone_answers_without_fan_out() {
        let plan = deliver("@Tibo quick question: which DB?", &["orchestrator", "coder"]);
        assert_eq!(plan, vec![
            ("tibo".to_string(), RoomDelivery::Steer(Kind::LeaderSolo)),
            ("nico".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ]);
        assert_eq!(deliver("@Tibo quick question", &[]), vec![("tibo".to_string(), RoomDelivery::Start)]);
    }

    #[test]
    fn narrowing_keeps_only_edges_inside_the_started_subset() {
        let directory = led_snapshot();
        let message = "@Theo then @Nico then @Leon";
        let intent = preview_group_activation(&directory, "launch", message).unwrap().intent();
        assert!(!intent.execution_dependencies.as_ref().unwrap().is_empty());
        let narrowed = narrow_activation(&intent, &["nico".to_string(), "leon".to_string()]);
        assert_eq!(narrowed.active_agent_ids, vec!["nico".to_string(), "leon".to_string()]);
        assert!(narrowed.execution_dependencies.as_ref().unwrap().iter()
            .all(|edge| edge.prerequisite != "theo" && edge.dependent != "theo"));
        resolve_group_turn_from_activation(&directory, &narrowed).unwrap();
        // Nothing narrowed: the user's own plan is kept as is.
        assert_eq!(narrow_activation(&intent, &intent.active_agent_ids.clone()), intent);
    }

    #[test]
    fn race_fallback_turns_share_the_single_transcript_entry() {
        let fallback = room_fallback_turn_id("turn_room_0003", "coder#2");
        assert_eq!(fallback, "turn_room_0003.rf-coder");
        assert_eq!(room_boundary_turn_id(&fallback), "turn_room_0003");
        assert_eq!(room_boundary_turn_id("turn_room_0003"), "turn_room_0003");
        // Still a valid client turn id (8..=128 of [A-Za-z0-9_.-]).
        assert!((8..=128).contains(&fallback.len()) && fallback.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')));
    }

}
