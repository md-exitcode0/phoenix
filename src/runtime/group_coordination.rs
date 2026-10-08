//! Group leader coordination.
//!
//! Every group has one leader (`GroupProfile::leader_agent_id`). An
//! unaddressed user message wakes only the leader, who picks a working mode
//! (`diverge`, `research`, `converge`, `build`), keeps a per-group mission board (brief,
//! plan, results, append-only decision log), dispatches assignments that are
//! posted as visible room messages, sequences plan dependencies, and is woken
//! again to converge once the members it dispatched have reported.
//!
//! State lives in the company database next to the directory. All storage
//! helpers take a plain `&Connection` so they are unit-testable against an
//! in-memory database; runtime entry points go through
//! [`CompanyStore::with_coordination`](super::company::CompanyStore::with_coordination).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::group_conversation::GroupTurnContext;

/// Leader convergence turns chained from one user request before the leader
/// must answer with what it has.
pub const MAX_LEADER_CYCLES: i64 = 4;
/// Turn-id prefix of a queued leader convergence continuation.
pub const LEADER_CONVERGENCE_PREFIX: &str = "group_lead_";
const MAX_BOARD_TEXT_BYTES: usize = 16 * 1024;
const MAX_PLAN_ITEMS: usize = 64;
const MAX_SCOPE_BYTES: usize = 2 * 1024;
const DEFAULT_CLAIM_TTL_MINUTES: i64 = 60;
const MAX_CLAIM_TTL_MINUTES: i64 = 24 * 60;
const BOARD_RECENT_ENTRIES: usize = 6;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    #[default]
    Idle,
    Diverge,
    /// Fact-finding between the brainstorm and the decision: the leader
    /// assigns prior art / APIs / constraints, members post findings.
    /// Stored as plain text in the `mode` column, so adding it needs no
    /// schema migration; rows written before it existed still parse.
    Research,
    Converge,
    Build,
}

impl GroupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Diverge => "diverge",
            Self::Research => "research",
            Self::Converge => "converge",
            Self::Build => "build",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "idle" => Ok(Self::Idle),
            "diverge" => Ok(Self::Diverge),
            "research" => Ok(Self::Research),
            "converge" => Ok(Self::Converge),
            "build" => Ok(Self::Build),
            other => anyhow::bail!("unknown group mode `{other}`; use diverge, research, converge, or build"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// `todo` | `doing` | `done` | `blocked`
    #[serde(default = "default_item_status")]
    pub status: String,
}

fn default_item_status() -> String {
    "todo".to_string()
}

impl PlanItem {
    pub fn is_done(&self) -> bool {
        self.status == "done"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardEntry {
    pub entry_id: i64,
    /// `decision` | `result` | `escalation`
    pub kind: String,
    pub author_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub text: String,
    pub created_at: String,
}

/// Diverge round state. While `open`, a member's room contributions saved in
/// one of `turn_ids` are hidden from every other non-leader member.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DivergeRound {
    pub round: i64,
    pub open: bool,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub turn_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MissionBoard {
    pub group_id: String,
    pub brief: String,
    pub plan: Vec<PlanItem>,
    pub mode: GroupMode,
    pub diverge: DivergeRound,
    pub decisions: Vec<BoardEntry>,
    pub results: Vec<BoardEntry>,
    pub escalations: Vec<BoardEntry>,
    pub leader_cycles: i64,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildClaim {
    pub claim_id: String,
    pub group_id: String,
    pub owner_agent_id: String,
    pub scope: String,
    /// `active` | `released` | `reassigned`
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
}

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS company_group_missions(
            group_id TEXT PRIMARY KEY,
            brief TEXT NOT NULL DEFAULT '',
            plan_json TEXT NOT NULL DEFAULT '[]',
            mode TEXT NOT NULL DEFAULT 'idle',
            round INTEGER NOT NULL DEFAULT 0,
            round_open INTEGER NOT NULL DEFAULT 0,
            round_started_at TEXT,
            round_turn_ids_json TEXT NOT NULL DEFAULT '[]',
            leader_cycles INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS company_group_board_entries(
            entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
            group_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            author_agent_id TEXT NOT NULL,
            plan_item_id TEXT,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_group_board_entries_group
            ON company_group_board_entries(group_id,kind,entry_id);
        CREATE TRIGGER IF NOT EXISTS company_group_board_entries_append_only
            BEFORE UPDATE ON company_group_board_entries
            BEGIN SELECT RAISE(ABORT,'mission board entries are append-only'); END;
        CREATE TABLE IF NOT EXISTS company_group_claims(
            claim_id TEXT PRIMARY KEY,
            group_id TEXT NOT NULL,
            owner_agent_id TEXT NOT NULL,
            scope TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_group_claims_group_status
            ON company_group_claims(group_id,status,expires_at);
        CREATE TABLE IF NOT EXISTS company_group_dispatches(
            dispatch_id INTEGER PRIMARY KEY AUTOINCREMENT,
            group_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            target_agent_id TEXT NOT NULL,
            plan_item_id TEXT,
            text TEXT NOT NULL,
            posted INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS company_group_dispatches_turn
            ON company_group_dispatches(group_id,turn_id,posted);",
    )?;
    Ok(())
}

fn bounded(text: &str, label: &str) -> Result<String> {
    let text = text.trim();
    anyhow::ensure!(
        text.len() <= MAX_BOARD_TEXT_BYTES,
        "{label} exceeds {MAX_BOARD_TEXT_BYTES} bytes"
    );
    Ok(text.to_string())
}

fn ensure_mission_row(connection: &Connection, group_id: &str, now: &str) -> Result<()> {
    anyhow::ensure!(!group_id.trim().is_empty(), "group id is empty");
    connection.execute(
        "INSERT OR IGNORE INTO company_group_missions(group_id,updated_at) VALUES(?1,?2)",
        params![group_id, now],
    )?;
    Ok(())
}

fn load_entries(connection: &Connection, group_id: &str, kind: &str) -> Result<Vec<BoardEntry>> {
    let mut statement = connection.prepare(
        "SELECT entry_id,kind,author_agent_id,plan_item_id,text,created_at
         FROM company_group_board_entries WHERE group_id=?1 AND kind=?2 ORDER BY entry_id",
    )?;
    let rows = statement.query_map(params![group_id, kind], |row| {
        Ok(BoardEntry {
            entry_id: row.get(0)?,
            kind: row.get(1)?,
            author_agent_id: row.get(2)?,
            plan_item_id: row.get(3)?,
            text: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(collected)
}

pub fn load_board(connection: &Connection, group_id: &str) -> Result<MissionBoard> {
    let row = connection
        .query_row(
            "SELECT brief,plan_json,mode,round,round_open,round_started_at,round_turn_ids_json,
                    leader_cycles,updated_at
             FROM company_group_missions WHERE group_id=?1",
            [group_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?;
    let mut board = MissionBoard {
        group_id: group_id.to_string(),
        ..MissionBoard::default()
    };
    if let Some((brief, plan_json, mode, round, open, started_at, turn_ids, cycles, updated_at)) = row {
        board.brief = brief;
        board.plan = serde_json::from_str(&plan_json).context("mission plan is invalid")?;
        board.mode = GroupMode::parse(&mode)?;
        board.diverge = DivergeRound {
            round,
            open,
            started_at,
            turn_ids: serde_json::from_str(&turn_ids).context("diverge round turns are invalid")?,
        };
        board.leader_cycles = cycles;
        board.updated_at = Some(updated_at);
    }
    board.decisions = load_entries(connection, group_id, "decision")?;
    board.results = load_entries(connection, group_id, "result")?;
    board.escalations = load_entries(connection, group_id, "escalation")?;
    Ok(board)
}

pub fn set_brief(connection: &Connection, group_id: &str, brief: &str, now: &str) -> Result<()> {
    let brief = bounded(brief, "mission brief")?;
    ensure_mission_row(connection, group_id, now)?;
    connection.execute(
        "UPDATE company_group_missions SET brief=?1,updated_at=?2 WHERE group_id=?3",
        params![brief, now, group_id],
    )?;
    Ok(())
}

/// Validate and store a plan. Ids are unique, `depends_on` references
/// existing items, and dependencies are acyclic. Existing `done` statuses
/// survive a re-plan for items that keep their id.
pub fn set_plan(connection: &Connection, group_id: &str, mut items: Vec<PlanItem>, now: &str) -> Result<Vec<PlanItem>> {
    anyhow::ensure!(items.len() <= MAX_PLAN_ITEMS, "a plan holds at most {MAX_PLAN_ITEMS} items");
    let mut ids = std::collections::HashSet::new();
    for item in &items {
        anyhow::ensure!(
            !item.id.trim().is_empty() && item.id.len() <= 96,
            "plan item ids must be 1-96 bytes"
        );
        anyhow::ensure!(ids.insert(item.id.clone()), "duplicate plan item `{}`", item.id);
        bounded(&item.title, "plan item title")?;
    }
    for item in &items {
        for dependency in &item.depends_on {
            anyhow::ensure!(dependency != &item.id, "plan item `{}` cannot depend on itself", item.id);
            anyhow::ensure!(ids.contains(dependency), "plan item `{}` depends on unknown `{dependency}`", item.id);
        }
    }
    // Kahn: every item must become ready.
    let mut remaining = items.iter().map(|item| item.id.clone()).collect::<std::collections::HashSet<_>>();
    loop {
        let ready = items
            .iter()
            .filter(|item| remaining.contains(&item.id))
            .filter(|item| item.depends_on.iter().all(|dependency| !remaining.contains(dependency)))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for id in ready {
            remaining.remove(&id);
        }
    }
    anyhow::ensure!(remaining.is_empty(), "plan dependencies form a cycle");
    let previous = load_board(connection, group_id)?.plan;
    for item in &mut items {
        if !matches!(item.status.as_str(), "todo" | "doing" | "done" | "blocked") {
            item.status = default_item_status();
        }
        if let Some(old) = previous.iter().find(|old| old.id == item.id) {
            if old.is_done() {
                item.status = "done".to_string();
            }
        }
    }
    ensure_mission_row(connection, group_id, now)?;
    connection.execute(
        "UPDATE company_group_missions SET plan_json=?1,updated_at=?2 WHERE group_id=?3",
        params![serde_json::to_string(&items)?, now, group_id],
    )?;
    Ok(items)
}

pub fn set_item_status(connection: &Connection, group_id: &str, item_id: &str, status: &str, now: &str) -> Result<()> {
    anyhow::ensure!(
        matches!(status, "todo" | "doing" | "done" | "blocked"),
        "plan item status must be todo, doing, done, or blocked"
    );
    let mut plan = load_board(connection, group_id)?.plan;
    let item = plan
        .iter_mut()
        .find(|item| item.id == item_id)
        .with_context(|| format!("unknown plan item `{item_id}`"))?;
    item.status = status.to_string();
    connection.execute(
        "UPDATE company_group_missions SET plan_json=?1,updated_at=?2 WHERE group_id=?3",
        params![serde_json::to_string(&plan)?, now, group_id],
    )?;
    Ok(())
}

/// Switch the room mode. `diverge` opens a new blind round bound to the
/// current turn; any other mode closes the round (convergence).
pub fn set_mode(connection: &Connection, group_id: &str, mode: GroupMode, turn_id: Option<&str>, now: &str) -> Result<MissionBoard> {
    ensure_mission_row(connection, group_id, now)?;
    if mode == GroupMode::Diverge {
        let turns = turn_id.map(|turn| vec![turn.to_string()]).unwrap_or_default();
        connection.execute(
            "UPDATE company_group_missions SET mode=?1,round=round+1,round_open=1,round_started_at=?2,
                round_turn_ids_json=?3,updated_at=?2 WHERE group_id=?4",
            params![mode.as_str(), now, serde_json::to_string(&turns)?, group_id],
        )?;
    } else {
        connection.execute(
            "UPDATE company_group_missions SET mode=?1,round_open=0,updated_at=?2 WHERE group_id=?3",
            params![mode.as_str(), now, group_id],
        )?;
    }
    load_board(connection, group_id)
}

/// Bind another turn to the open diverge round (leader convergence chains).
pub fn note_round_turn(connection: &Connection, group_id: &str, turn_id: &str, now: &str) -> Result<()> {
    let board = load_board(connection, group_id)?;
    if !board.diverge.open || board.diverge.turn_ids.iter().any(|turn| turn == turn_id) {
        return Ok(());
    }
    let mut turns = board.diverge.turn_ids;
    turns.push(turn_id.to_string());
    connection.execute(
        "UPDATE company_group_missions SET round_turn_ids_json=?1,updated_at=?2 WHERE group_id=?3",
        params![serde_json::to_string(&turns)?, now, group_id],
    )?;
    Ok(())
}

/// A room turn that starts while a diverge round is open belongs to that
/// round: a member's pitch in a mid-round turn (a follow-up the user sent to
/// one member) stays blind to its peers too. Leader-only turns are harmless
/// to bind (the leader is never hidden).
pub fn bind_turn_to_open_round(store: &super::company::CompanyStore, group: &GroupTurnContext, turn_id: &str) -> Result<()> {
    if group.leader_agent_id.is_none() {
        return Ok(());
    }
    store.with_coordination(|connection| note_round_turn(connection, &group.group_id, turn_id, &Utc::now().to_rfc3339()))
}

/// Append-only board entry (`decision`, `result`, `escalation`).
pub fn append_entry(
    connection: &Connection,
    group_id: &str,
    kind: &str,
    author_agent_id: &str,
    plan_item_id: Option<&str>,
    text: &str,
    now: &str,
) -> Result<i64> {
    anyhow::ensure!(
        matches!(kind, "decision" | "result" | "escalation"),
        "unknown mission board entry kind `{kind}`"
    );
    let text = bounded(text, "mission board entry")?;
    anyhow::ensure!(!text.is_empty(), "mission board entry is empty");
    ensure_mission_row(connection, group_id, now)?;
    connection.execute(
        "INSERT INTO company_group_board_entries(group_id,kind,author_agent_id,plan_item_id,text,created_at)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![group_id, kind, author_agent_id, plan_item_id, text, now],
    )?;
    Ok(connection.last_insert_rowid())
}

// ── Build claims ─────────────────────────────────────────────────────────

fn scope_tokens(scope: &str) -> Vec<String> {
    scope
        .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .map(|token| {
            let token = token.trim().trim_start_matches("./");
            let token = token.trim_end_matches("/**").trim_end_matches("/*").trim_end_matches('/');
            token.to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Two scopes overlap when any path/area token is equal to, or a
/// directory prefix of, a token of the other (`src/ui` vs `src/ui/app.js`).
/// `*` (whole repository) overlaps everything.
pub fn scopes_overlap(left: &str, right: &str) -> bool {
    let left = scope_tokens(left);
    let right = scope_tokens(right);
    let covers = |outer: &str, inner: &str| {
        outer == "*" || outer == inner || inner.strip_prefix(outer).is_some_and(|rest| rest.starts_with('/'))
    };
    left.iter().any(|a| right.iter().any(|b| covers(a, b) || covers(b, a)))
}

fn expire_claims(connection: &Connection, group_id: &str, now: &str) -> Result<()> {
    connection.execute(
        "UPDATE company_group_claims SET status='expired',updated_at=?1
         WHERE group_id=?2 AND status='active' AND expires_at<=?1",
        params![now, group_id],
    )?;
    Ok(())
}

/// Active, unexpired claims. Expired claims are ignored (and marked so).
pub fn list_claims(connection: &Connection, group_id: &str, now: &str) -> Result<Vec<BuildClaim>> {
    expire_claims(connection, group_id, now)?;
    let mut statement = connection.prepare(
        "SELECT claim_id,group_id,owner_agent_id,scope,status,created_at,expires_at
         FROM company_group_claims WHERE group_id=?1 AND status='active' AND expires_at>?2
         ORDER BY created_at,claim_id",
    )?;
    let rows = statement.query_map(params![group_id, now], |row| {
        Ok(BuildClaim {
            claim_id: row.get(0)?,
            group_id: row.get(1)?,
            owner_agent_id: row.get(2)?,
            scope: row.get(3)?,
            status: row.get(4)?,
            created_at: row.get(5)?,
            expires_at: row.get(6)?,
        })
    })?;
    let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(collected)
}

/// Claim `scope` for `owner`. An overlapping active claim held by someone
/// else refuses the claim unless the leader passes `reassign`, which marks
/// the conflicting claims `reassigned`.
#[allow(clippy::too_many_arguments)]
pub fn claim(
    connection: &mut Connection,
    group_id: &str,
    owner_agent_id: &str,
    scope: &str,
    ttl_minutes: Option<i64>,
    actor_is_leader: bool,
    reassign: bool,
    now: DateTime<Utc>,
) -> Result<BuildClaim> {
    let scope = scope.trim();
    anyhow::ensure!(!scope_tokens(scope).is_empty(), "claim scope is empty");
    anyhow::ensure!(scope.len() <= MAX_SCOPE_BYTES, "claim scope is too long");
    let ttl = ttl_minutes.unwrap_or(DEFAULT_CLAIM_TTL_MINUTES).clamp(1, MAX_CLAIM_TTL_MINUTES);
    let now_text = now.to_rfc3339();
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    expire_claims(&tx, group_id, &now_text)?;
    let conflicts = {
        let mut statement = tx.prepare(
            "SELECT claim_id,owner_agent_id,scope FROM company_group_claims
             WHERE group_id=?1 AND status='active' AND expires_at>?2",
        )?;
        let rows = statement.query_map(params![group_id, now_text], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        let all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        all
    }
    .into_iter()
    .filter(|(_, owner, existing)| owner != owner_agent_id && scopes_overlap(existing, scope))
    .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        anyhow::ensure!(
            actor_is_leader && reassign,
            "scope overlaps active claim(s) {}; ask the group leader to reassign",
            conflicts
                .iter()
                .map(|(id, owner, existing)| format!("{id} by {owner} on `{existing}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (id, _, _) in &conflicts {
            tx.execute(
                "UPDATE company_group_claims SET status='reassigned',updated_at=?1 WHERE claim_id=?2",
                params![now_text, id],
            )?;
        }
    }
    let claim_id = format!("claim-{}", uuid::Uuid::new_v4().simple());
    let expires_at = (now + chrono::Duration::minutes(ttl)).to_rfc3339();
    tx.execute(
        "INSERT INTO company_group_claims(claim_id,group_id,owner_agent_id,scope,status,created_at,expires_at,updated_at)
         VALUES(?1,?2,?3,?4,'active',?5,?6,?5)",
        params![claim_id, group_id, owner_agent_id, scope, now_text, expires_at],
    )?;
    tx.commit()?;
    Ok(BuildClaim {
        claim_id,
        group_id: group_id.to_string(),
        owner_agent_id: owner_agent_id.to_string(),
        scope: scope.to_string(),
        status: "active".to_string(),
        created_at: now_text,
        expires_at,
    })
}

pub fn release_claim(connection: &Connection, group_id: &str, claim_id: &str, actor: &str, actor_is_leader: bool, now: &str) -> Result<()> {
    let owner = connection
        .query_row(
            "SELECT owner_agent_id FROM company_group_claims WHERE group_id=?1 AND claim_id=?2 AND status='active'",
            params![group_id, claim_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("no active claim `{claim_id}` in this group"))?;
    anyhow::ensure!(owner == actor || actor_is_leader, "only the claim owner or the group leader can release `{claim_id}`");
    connection.execute(
        "UPDATE company_group_claims SET status='released',updated_at=?1 WHERE claim_id=?2",
        params![now, claim_id],
    )?;
    Ok(())
}

// ── Dispatch, gating, diverge blindness (pure helpers) ───────────────────

/// A member is gated (not woken) when it owns at least one open plan item
/// and every open item it owns still waits on an unfinished dependency.
pub fn dependency_gated(plan: &[PlanItem], target_agent_id: &str) -> bool {
    let open = plan
        .iter()
        .filter(|item| item.owner_agent_id.as_deref() == Some(target_agent_id) && !item.is_done())
        .collect::<Vec<_>>();
    !open.is_empty()
        && open.iter().all(|item| {
            item.depends_on.iter().any(|dependency| {
                plan.iter().find(|other| &other.id == dependency).is_none_or(|other| !other.is_done())
            })
        })
}

/// Items of `owner` whose dependencies are all done and that are still open.
pub fn ready_items_for<'a>(plan: &'a [PlanItem], owner: &str) -> Vec<&'a PlanItem> {
    plan.iter()
        .filter(|item| item.owner_agent_id.as_deref() == Some(owner) && !item.is_done())
        .filter(|item| {
            item.depends_on.iter().all(|dependency| {
                plan.iter().find(|other| &other.id == dependency).is_some_and(PlanItem::is_done)
            })
        })
        .collect()
}

/// What one non-leader viewer may not see during an open diverge round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergeView {
    pub leader_agent_id: String,
    pub viewer_agent_id: String,
    pub turn_ids: Vec<String>,
    pub started_at: Option<String>,
}

impl DivergeView {
    pub fn new(round: &DivergeRound, leader: &str, viewer: &str) -> Option<Self> {
        (round.open && viewer != leader).then(|| Self {
            leader_agent_id: leader.to_string(),
            viewer_agent_id: viewer.to_string(),
            turn_ids: round.turn_ids.clone(),
            started_at: round.started_at.clone(),
        })
    }

    /// A room contribution is hidden when another non-leader member authored
    /// it in a turn bound to the open round.
    pub fn hides_contribution(&self, author_agent_id: &str, turn_id: &str) -> bool {
        author_agent_id != self.viewer_agent_id
            && author_agent_id != self.leader_agent_id
            && {
                // A race-fallback turn (`<turn>.rf-<lane>`) belongs to its
                // room message's turn.
                let boundary = super::group_conversation::room_boundary_turn_id(turn_id);
                self.turn_ids.iter().any(|turn| turn == turn_id || turn == boundary)
            }
    }

    /// Board results logged by peers after the round opened are hidden too.
    pub fn hides_entry(&self, entry: &BoardEntry) -> bool {
        entry.author_agent_id != self.viewer_agent_id
            && entry.author_agent_id != self.leader_agent_id
            && self.started_at.as_deref().is_some_and(|start| entry.created_at.as_str() >= start)
    }
}

/// Diverge filter for a member's room context, read from the live store.
pub fn diverge_view(group: &GroupTurnContext, viewer_agent_id: &str) -> Option<DivergeView> {
    let leader = group.leader_agent_id.as_deref()?;
    let store = super::company::global_if_initialized()?;
    let board = store
        .with_coordination(|connection| load_board(connection, &group.group_id))
        .ok()?;
    DivergeView::new(&board.diverge, leader, viewer_agent_id)
}

fn short(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        format!("{}…", flat.chars().take(max_chars).collect::<String>())
    }
}

fn name_of(group: &GroupTurnContext, agent_id: &str) -> String {
    group
        .participants
        .iter()
        .find(|p| p.agent_id == agent_id)
        .map(|p| p.display_name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}

/// Compact mission board injected into every member's group-turn context.
pub fn render_board(group: &GroupTurnContext, board: &MissionBoard, claims: &[BuildClaim], view: Option<&DivergeView>) -> String {
    let mut out = format!("MISSION BOARD — mode: {}", board.mode.as_str());
    if board.diverge.open {
        out.push_str(&format!(" (diverge round {} open: answers stay blind until the leader converges)", board.diverge.round));
    }
    out.push('\n');
    out.push_str(&format!(
        "Brief: {}\n",
        if board.brief.is_empty() { "(none yet)".to_string() } else { short(&board.brief, 600) }
    ));
    if board.plan.is_empty() {
        out.push_str("Plan: (none yet)\n");
    } else {
        out.push_str("Plan:\n");
        for item in &board.plan {
            let owner = item.owner_agent_id.as_deref().map(|id| name_of(group, id)).unwrap_or_else(|| "unassigned".into());
            let deps = if item.depends_on.is_empty() { String::new() } else { format!(" ← after {}", item.depends_on.join(", ")) };
            out.push_str(&format!("- [{}] {} · {} · {}{}\n", item.status, item.id, short(&item.title, 160), owner, deps));
        }
    }
    let recent = |entries: &[BoardEntry]| {
        entries
            .iter()
            .filter(|entry| view.is_none_or(|view| !view.hides_entry(entry)))
            .rev()
            .take(BOARD_RECENT_ENTRIES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|entry| {
                format!(
                    "- {} {}{}: {}\n",
                    entry.created_at.get(..16).unwrap_or(entry.created_at.as_str()),
                    name_of(group, &entry.author_agent_id),
                    entry.plan_item_id.as_deref().map(|id| format!(" [{id}]")).unwrap_or_default(),
                    short(&entry.text, 220)
                )
            })
            .collect::<String>()
    };
    let decisions = recent(&board.decisions);
    if !decisions.is_empty() {
        out.push_str("Decisions (append-only):\n");
        out.push_str(&decisions);
    }
    let results = recent(&board.results);
    if !results.is_empty() {
        out.push_str("Results:\n");
        out.push_str(&results);
    }
    if view.is_none() {
        let escalations = recent(&board.escalations);
        if !escalations.is_empty() {
            out.push_str("Escalations to the leader:\n");
            out.push_str(&escalations);
        }
    }
    if !claims.is_empty() {
        out.push_str("Active build claims:\n");
        for claim in claims {
            out.push_str(&format!(
                "- {} · {} · `{}` until {}\n",
                claim.claim_id,
                name_of(group, &claim.owner_agent_id),
                short(&claim.scope, 160),
                claim.expires_at.get(..16).unwrap_or(claim.expires_at.as_str())
            ));
        }
    }
    out.trim_end().to_string()
}

pub fn leader_protocol(group: &GroupTurnContext) -> String {
    let members = group
        .participants
        .iter()
        .filter(|p| !group.is_leader(&p.agent_id))
        .map(|p| format!("@{} ({}, {})", p.agent_id, p.display_name, if p.role_title.is_empty() { &p.internal_role } else { &p.role_title }))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "GROUP LEADER PROTOCOL — you lead \"{name}\". Members: {members}. Unaddressed room messages come only to you; you own the outcome. This protocol overrides the no-talk rule above for you.\n\
WORKFLOW — size the request first:\n\
- Trivial or quick ask (a fact, a small fix, a yes/no): answer it yourself, or make ONE assignment. Skip the phases.\n\
- Pure ideation (names, options, strategy, angles): DIVERGE → CONVERGE only.\n\
- Substantial build or creation request: run these phases IN ORDER, recording each with group_board {{\"action\":\"set_mode\"}}:\n\
  (a) DIVERGE — brainstorm: members generate approaches independently and blind (each sees the brief, never each other's ideas, until you converge).\n\
  (b) RESEARCH — assign fact-finding to the relevant members (prior art, APIs, libraries, constraints, numbers); they post findings with group_board result.\n\
  (c) CONVERGE — you pick the approach from the brainstorm plus the research, critique it (persona reactions or a short idea tournament when useful), and record the decision with group_board decide.\n\
  (d) BUILD — set_plan with one owner per item, depends_on for real handoffs, no overlapping scopes; builders claim files/areas (group_board claim) before editing; members execute; the critic/reviewer checks the work; you verify it, then give the user ONE answer.\n\
1. Record the brief (set_brief) and, for diverge/research/build, the plan (set_plan: items {{id,title,owner_agent_id,depends_on}}).\n\
2. Dispatch with talk {{\"to\":\"<member or everyone>\",\"room_mode\":\"assign\"}} or `@id` assignment lines in your reply. Assignments are posted in the room under your name and wake those members; a member whose items still wait on unfinished depends_on stays asleep until they are done.\n\
3. When dispatched members have reported you are woken again with their saved results. Compare, critique, record decisions (group_board decide), then move to the next phase or dispatch newly ready items. At most {cycles} convergence cycles per request.\n\
4. Everyone in the room hears every user message. A message the user @mentions to specific members is THEIRS: they act on it directly, even mid-build. Do not take it over, redo it, or reassign it — only record it on the board (decision / plan item / claims) so claims do not collide. If the user @mentions only you, answer it yourself with no dispatch or fan-out. @everyone / @all: you go FIRST — record the brief, pick the mode, set_plan with one owner per item and post the assignments; every member starts right after your reply with your plan on the board (members already working act on it in their own area while you re-plan).\n\
5. The user may steer mid-build (\"make it dark mode\"): the members working on it adapt immediately; you update the brief/plan and claims to match instead of restarting the work.\n\
6. Before you answer, run your own red-team pass: strongest objection, failure modes, unverified claims, missing pieces — fix or flag them.\n\
7. Deliver ONE coherent answer in the room, in your voice — not a relay of member messages. Escalate to the user only with ask_user.",
        name = group.group_name,
        members = if members.is_empty() { "none".to_string() } else { members },
        cycles = MAX_LEADER_CYCLES,
    )
}

pub fn member_block(group: &GroupTurnContext, me: &str, board: &MissionBoard) -> String {
    let leader = group.leader().map(|p| p.display_name.clone()).unwrap_or_else(|| "the leader".into());
    let mine = board
        .plan
        .iter()
        .filter(|item| item.owner_agent_id.as_deref() == Some(me))
        .map(|item| format!("{} [{}] {}", item.id, item.status, short(&item.title, 160)))
        .collect::<Vec<_>>();
    let mode_rule = match board.mode {
        GroupMode::Diverge if board.diverge.open => " Work independently: give your own take on the brief; peers' answers are hidden from you until the leader converges, so do not wait for or reference them.",
        GroupMode::Build => " Build mode: claim your files/areas with group_board claim before editing; an overlapping claim is refused unless the leader reassigns it. Release claims when done.",
        GroupMode::Research => " Research mode: do the fact-finding the leader assigned you (prior art, APIs, constraints, numbers) and post concrete findings with sources via group_board result; do not start building or pick the final approach.",
        GroupMode::Converge => " Converge mode: react to the options the leader presents; be specific and critical.",
        _ => "",
    };
    format!(
        "GROUP MEMBER — {leader} leads this room. Mode: {mode}. Your assignment: {assignment}.{mode_rule} Answer your assignment in this room; log your result with group_board {{\"action\":\"result\"}} when it maps to a plan item. Need a decision or blocked? talk with room_mode \"escalate\" reaches the leader. Do not re-plan the whole request or answer for the group. If the leader gave you nothing in the current phase, reply with one short line that you are standing by — do not start later-phase work early or repeat a teammate.",
        mode = board.mode.as_str(),
        assignment = if mine.is_empty() { "as stated in the leader's room message".to_string() } else { mine.join("; ") },
    )
}

/// Leader protocol (leader) or member block (member) plus the compact board.
/// `None` when the room has no leader (legacy contexts).
pub fn coordination_block(group: &GroupTurnContext, actor: &str) -> Option<String> {
    let leader = group.leader_agent_id.as_deref()?;
    let me = group.agent_id_for(actor).unwrap_or(actor);
    let (board, claims) = super::company::global_if_initialized()
        .and_then(|store| {
            store
                .with_coordination(|connection| {
                    let now = Utc::now().to_rfc3339();
                    Ok((load_board(connection, &group.group_id)?, list_claims(connection, &group.group_id, &now)?))
                })
                .ok()
        })
        .unwrap_or_else(|| (MissionBoard { group_id: group.group_id.clone(), ..MissionBoard::default() }, Vec::new()));
    let is_leader = group.is_leader(me);
    let view = if is_leader { None } else { DivergeView::new(&board.diverge, leader, me) };
    let head = if is_leader { leader_protocol(group) } else { member_block(group, me, &board) };
    Some(format!("{head}\n\n{}", render_board(group, &board, &claims, view.as_ref())))
}

// ── Tool surfaces ────────────────────────────────────────────────────────

pub fn group_board_tool_definition() -> crate::providers::contracts::ToolDefinition {
    crate::providers::contracts::ToolDefinition {
        name: "group_board".into(),
        description: "Read or update this group's mission board and build claims (group rooms only). Leader: set_brief, set_plan, set_mode (diverge|research|converge|build), decide (append-only decision log), item_status. Members: result (log your result, optionally for a plan item you own), item_status on your own items. Everyone: read, claim/release/claims for build scopes (paths or areas; overlapping active claims are refused unless the leader reassigns; claims expire).".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["read", "set_brief", "set_plan", "set_mode", "decide", "result", "item_status", "claim", "release", "claims"]},
                "brief": {"type": "string"},
                "items": {"type": "array", "maxItems": MAX_PLAN_ITEMS, "items": {"type": "object", "properties": {
                    "id": {"type": "string"}, "title": {"type": "string"},
                    "owner_agent_id": {"type": "string"},
                    "depends_on": {"type": "array", "items": {"type": "string"}}
                }, "required": ["id", "title"]}},
                "mode": {"type": "string", "enum": ["diverge", "research", "converge", "build"], "description": "Phases for a substantial build/creation request run in order: diverge (blind brainstorm) → research (assigned fact-finding) → converge (pick + record the decision) → build (claims + dependencies). Pure ideation: diverge → converge. Trivial asks skip phases."},
                "text": {"type": "string", "description": "Decision or result text."},
                "item_id": {"type": "string"},
                "status": {"type": "string", "enum": ["todo", "doing", "done", "blocked"]},
                "scope": {"type": "string", "description": "Paths/areas, comma separated (claim)."},
                "owner_agent_id": {"type": "string", "description": "Claim on behalf of a member (leader only)."},
                "ttl_minutes": {"type": "integer", "minimum": 1, "maximum": MAX_CLAIM_TTL_MINUTES},
                "reassign": {"type": "boolean", "description": "Leader only: take over overlapping active claims."},
                "claim_id": {"type": "string"}
            },
            "required": ["action"]
        }),
    }
}

fn resolve_member<'a>(group: &'a GroupTurnContext, name: &str) -> Option<&'a str> {
    let wanted = name.trim().trim_start_matches('@');
    if let Some(id) = group.agent_id_for(wanted) {
        return Some(id);
    }
    if !group.names_member(wanted) {
        return None;
    }
    let lower = wanted.to_lowercase().replace([' ', '-'], "_");
    group
        .participants
        .iter()
        .find(|p| {
            let first = p.display_name.split_whitespace().next().unwrap_or("");
            [p.display_name.as_str(), first]
                .into_iter()
                .any(|alias| alias.to_lowercase().replace([' ', '-'], "_") == lower)
        })
        .map(|p| p.agent_id.as_str())
}

/// Execute one `group_board` call for `actor` in `group`. Returns the tool
/// result text (JSON on success, `error: …` on refusal).
pub fn handle_group_board_call(group: &GroupTurnContext, actor: &str, turn_id: Option<&str>, input: &serde_json::Value) -> String {
    let Some(store) = super::company::global_if_initialized() else {
        return "error: the company store is not available".to_string();
    };
    match store.with_coordination(|connection| execute_board_action(connection, group, actor, turn_id, input, Utc::now())) {
        Ok(value) => value.to_string(),
        Err(error) => format!("error: {error:#}"),
    }
}

pub(crate) fn execute_board_action(
    connection: &mut Connection,
    group: &GroupTurnContext,
    actor: &str,
    turn_id: Option<&str>,
    input: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<serde_json::Value> {
    let me = group.agent_id_for(actor).context("you are not a member of this group")?.to_string();
    let is_leader = group.is_leader(&me);
    let now_text = now.to_rfc3339();
    let text = |key: &str| input.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let action = text("action");
    let leader_only = |what: &str| -> Result<()> {
        anyhow::ensure!(is_leader, "only the group leader can {what}");
        Ok(())
    };
    let group_id = group.group_id.as_str();
    match action.as_str() {
        "read" | "" => {}
        "set_brief" => {
            leader_only("set the brief")?;
            set_brief(connection, group_id, &text("brief"), &now_text)?;
        }
        "set_plan" => {
            leader_only("set the plan")?;
            let items: Vec<PlanItem> = serde_json::from_value(input.get("items").cloned().unwrap_or_else(|| serde_json::json!([])))
                .context("items must be [{id,title,owner_agent_id?,depends_on?}]")?;
            let items = items
                .into_iter()
                .map(|mut item| -> Result<PlanItem> {
                    if let Some(owner) = item.owner_agent_id.as_deref().filter(|owner| !owner.trim().is_empty()) {
                        item.owner_agent_id = Some(
                            resolve_member(group, owner)
                                .with_context(|| format!("plan owner `{owner}` is not a member of this room"))?
                                .to_string(),
                        );
                    } else {
                        item.owner_agent_id = None;
                    }
                    Ok(item)
                })
                .collect::<Result<Vec<_>>>()?;
            set_plan(connection, group_id, items, &now_text)?;
        }
        "set_mode" => {
            leader_only("change the mode")?;
            set_mode(connection, group_id, GroupMode::parse(&text("mode"))?, turn_id, &now_text)?;
        }
        "decide" => {
            leader_only("record decisions")?;
            append_entry(connection, group_id, "decision", &me, None, &text("text"), &now_text)?;
        }
        "result" => {
            let item_id = Some(text("item_id")).filter(|id| !id.is_empty());
            if let Some(item_id) = item_id.as_deref() {
                let board = load_board(connection, group_id)?;
                let item = board.plan.iter().find(|item| item.id == item_id).with_context(|| format!("unknown plan item `{item_id}`"))?;
                anyhow::ensure!(
                    is_leader || item.owner_agent_id.as_deref() == Some(me.as_str()),
                    "plan item `{item_id}` belongs to someone else"
                );
            }
            append_entry(connection, group_id, "result", &me, item_id.as_deref(), &text("text"), &now_text)?;
            if let Some(item_id) = item_id.as_deref() {
                if input.get("done").and_then(|v| v.as_bool()).unwrap_or(true) {
                    set_item_status(connection, group_id, item_id, "done", &now_text)?;
                }
            }
        }
        "item_status" => {
            let item_id = text("item_id");
            let board = load_board(connection, group_id)?;
            let item = board.plan.iter().find(|item| item.id == item_id).with_context(|| format!("unknown plan item `{item_id}`"))?;
            anyhow::ensure!(
                is_leader || item.owner_agent_id.as_deref() == Some(me.as_str()),
                "plan item `{item_id}` belongs to someone else"
            );
            set_item_status(connection, group_id, &item_id, &text("status"), &now_text)?;
        }
        "claim" => {
            let owner = match Some(text("owner_agent_id")).filter(|id| !id.is_empty()) {
                Some(owner) => {
                    let owner = resolve_member(group, &owner).with_context(|| format!("`{owner}` is not a member of this room"))?.to_string();
                    anyhow::ensure!(owner == me || is_leader, "only the leader can claim for someone else");
                    owner
                }
                None => me.clone(),
            };
            let claimed = claim(
                connection,
                group_id,
                &owner,
                &text("scope"),
                input.get("ttl_minutes").and_then(|v| v.as_i64()),
                is_leader,
                input.get("reassign").and_then(|v| v.as_bool()).unwrap_or(false),
                now,
            )?;
            return Ok(serde_json::json!({"ok": true, "claim": claimed, "claims": list_claims(connection, group_id, &now_text)?}));
        }
        "release" => {
            release_claim(connection, group_id, &text("claim_id"), &me, is_leader, &now_text)?;
        }
        "claims" => {
            return Ok(serde_json::json!({"ok": true, "claims": list_claims(connection, group_id, &now_text)?}));
        }
        other => anyhow::bail!("unknown group_board action `{other}`"),
    }
    let board = load_board(connection, group_id)?;
    let view = if is_leader { None } else { DivergeView::new(&board.diverge, group.leader_agent_id.as_deref().unwrap_or(""), &me) };
    let claims = list_claims(connection, group_id, &now_text)?;
    Ok(serde_json::json!({"ok": true, "board": render_board(group, &board, &claims, view.as_ref())}))
}

/// Room-scoped handling for the `talk` tool. `None` lets the normal talk path
/// run (a teammate outside this room). `Some(text)` is the tool result and
/// nothing else is executed.
///
/// `room_mode`: `request` (default; one teammate), `assign` / `broadcast`
/// (leader only: queued as a visible room message that wakes the named
/// member(s) or everyone), `escalate` (member → leader via the mission board;
/// leader → user via ask_user).
pub fn intercept_room_talk(group: &GroupTurnContext, actor: &str, turn_id: Option<&str>, input: &serde_json::Value) -> Option<String> {
    let mode = input.get("room_mode").and_then(|v| v.as_str()).unwrap_or("request").to_ascii_lowercase();
    let to = input.get("to").and_then(|v| v.as_str()).unwrap_or("").trim();
    let everyone = matches!(to.trim_start_matches('@').to_ascii_lowercase().as_str(), "everyone" | "room" | "all" | "group");
    let room_target = everyone || group.names_member(to) || group.agent_id_for(to.trim_start_matches('@')).is_some();
    let leader_led = group.leader_agent_id.is_some();
    let is_leader = leader_led && group.is_leader(actor);
    let subject = input.get("subject").and_then(|v| v.as_str()).unwrap_or("").trim();
    let body = input.get("body").and_then(|v| v.as_str()).unwrap_or("").trim();
    let message = if subject.is_empty() { body.to_string() } else if body.is_empty() { subject.to_string() } else { format!("{subject}: {body}") };
    const REJECT: &str = "That teammate is in this room. Do not use talk for them: write @Name in your reply instead, and they will answer here in the room. No message was sent.";
    if !leader_led {
        return room_target.then(|| REJECT.to_string());
    }
    let me = group.agent_id_for(actor)?.to_string();
    if !is_leader {
        return match mode.as_str() {
            "escalate" => Some(match super::company::global_if_initialized().map(|store| {
                store.with_coordination(|connection| {
                    append_entry(connection, &group.group_id, "escalation", &me, None, &message, &Utc::now().to_rfc3339())
                })
            }) {
                Some(Ok(_)) => format!(
                    "Escalation recorded on the mission board for {}; the leader reads it when converging. Continue your assignment, or finish with final_answer and state what is blocked.",
                    group.leader().map(|p| p.display_name.as_str()).unwrap_or("the leader")
                ),
                Some(Err(error)) => format!("error: escalation was not recorded: {error:#}"),
                None => "error: the company store is not available; state the blocker in your room reply instead.".to_string(),
            }),
            "broadcast" | "assign" => Some("Only the group leader can assign or broadcast in this room. Answer your own assignment, or use room_mode \"escalate\" to reach the leader. No message was sent.".to_string()),
            _ => room_target.then(|| format!("{REJECT} To reach the leader, use room_mode \"escalate\".")),
        };
    }
    // Leader.
    if mode == "escalate" {
        return Some("To escalate to the user, call ask_user with your question; it uses the normal question/approval path. No message was sent.".to_string());
    }
    if !room_target && mode != "broadcast" {
        return None;
    }
    let targets = if everyone || (mode == "broadcast" && to.is_empty()) {
        group.participants.iter().filter(|p| p.agent_id != me).map(|p| p.agent_id.clone()).collect::<Vec<_>>()
    } else {
        match resolve_member(group, to) {
            Some(id) if id != me => vec![id.to_string()],
            Some(_) => return Some("You lead this room; do the work yourself or assign it to a member. No message was sent.".to_string()),
            None => return Some(format!("`{to}` is not a unique member of this room. No message was sent.")),
        }
    };
    if message.is_empty() {
        return Some("error: an assignment needs a subject or body. No message was sent.".to_string());
    }
    let Some(turn_id) = turn_id else {
        return Some("Assignments need a live room turn. Write the assignment as @Name lines in your room reply instead. No message was sent.".to_string());
    };
    let plan_item = input.get("plan_item_id").and_then(|v| v.as_str()).map(str::to_string);
    let Some(store) = super::company::global_if_initialized() else {
        return Some("error: the company store is not available; write @Name assignment lines in your reply instead.".to_string());
    };
    let recorded = store.with_coordination(|connection| {
        let now = Utc::now().to_rfc3339();
        for target in &targets {
            connection.execute(
                "INSERT INTO company_group_dispatches(group_id,turn_id,target_agent_id,plan_item_id,text,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![group.group_id, turn_id, target, plan_item, bounded(&message, "assignment")?, now],
            )?;
        }
        Ok(())
    });
    Some(match recorded {
        Ok(()) => format!(
            "Assignment queued for {}. It is appended to your room reply as a visible assignment and wakes them once your reply is saved (members whose plan items still wait on unfinished depends_on stay asleep until those are done). Finish this turn with final_answer carrying your brief/plan; do not wait for their answers here — you are woken to converge when they report.",
            targets.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(", ")
        ),
        Err(error) => format!("error: assignment was not recorded: {error:#}"),
    })
}

/// Unposted leader dispatches for this turn, rendered as visible room
/// assignment lines. Returns the appended text and the dispatch ids to mark
/// posted once the reply is durably saved.
pub fn pending_dispatch_lines(connection: &Connection, group_id: &str, turn_id: &str) -> Result<(String, Vec<i64>)> {
    let mut statement = connection.prepare(
        "SELECT dispatch_id,target_agent_id,plan_item_id,text FROM company_group_dispatches
         WHERE group_id=?1 AND turn_id=?2 AND posted=0 ORDER BY dispatch_id",
    )?;
    let rows = statement.query_map(params![group_id, turn_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?))
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let mut text = String::from("\n\n**Assignments**\n");
    for (_, target, item, line) in &rows {
        let item = item.as_deref().map(|id| format!(" [{id}]")).unwrap_or_default();
        text.push_str(&format!("- @{target}{item} — {}\n", short(line, 1200)));
    }
    Ok((text.trim_end().to_string(), rows.into_iter().map(|(id, ..)| id).collect()))
}

pub fn mark_dispatches_posted(connection: &Connection, ids: &[i64]) -> Result<()> {
    for id in ids {
        connection.execute("UPDATE company_group_dispatches SET posted=1 WHERE dispatch_id=?1", [id])?;
    }
    Ok(())
}

/// Leader dispatch path: append queued assignments to the leader's room reply
/// before it is persisted. Returns the amended message and the ids to mark.
pub fn with_leader_dispatches(
    store: &super::company::CompanyStore,
    group: &GroupTurnContext,
    turn_id: &str,
    speaker_agent_id: &str,
    message: &super::mailbox::AgentMessage,
) -> Result<Option<(super::mailbox::AgentMessage, Vec<i64>)>> {
    if !group.is_leader(speaker_agent_id) || message.is_failed_result() {
        return Ok(None);
    }
    let (lines, ids) = store.with_coordination(|connection| pending_dispatch_lines(connection, &group.group_id, turn_id))?;
    if ids.is_empty() {
        return Ok(None);
    }
    let mut amended = message.clone();
    amended.body = format!("{}{}", amended.body.trim_end(), lines);
    Ok(Some((amended, ids)))
}

/// Dependency gating for a room ping: a leader's ping does not wake a member
/// whose plan items all wait on unfinished dependencies. Errors never gate.
pub fn ping_is_gated(store: &super::company::CompanyStore, group: &GroupTurnContext, author_agent_id: &str, target_agent_id: &str) -> bool {
    if !group.is_leader(author_agent_id) {
        return false;
    }
    store
        .with_coordination(|connection| load_board(connection, &group.group_id))
        .map(|board| dependency_gated(&board.plan, target_agent_id))
        .unwrap_or(false)
}

/// A member's saved room contribution completes its ready (unblocked) plan
/// items and logs a result receipt, which can release gated dependents.
pub fn complete_member_items(store: &super::company::CompanyStore, group: &GroupTurnContext, agent_id: &str, receipt_id: &str) -> Result<usize> {
    if group.leader_agent_id.is_none() || group.is_leader(agent_id) {
        return Ok(0);
    }
    store.with_coordination(|connection| {
        let now = Utc::now().to_rfc3339();
        let board = load_board(connection, &group.group_id)?;
        let ready = ready_items_for(&board.plan, agent_id).into_iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        for item in &ready {
            set_item_status(connection, &group.group_id, item, "done", &now)?;
            append_entry(connection, &group.group_id, "result", agent_id, Some(item), &format!("Reported in room contribution {receipt_id}"), &now)?;
        }
        Ok(ready.len())
    })
}

/// Deterministic id of the leader convergence continuation for one turn.
pub fn leader_convergence_turn_id(canonical_session_id: &str, turn_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"phoenix-group-leader-converge-v1\0");
    digest.update(canonical_session_id.as_bytes());
    digest.update([0]);
    digest.update(turn_id.as_bytes());
    format!("{LEADER_CONVERGENCE_PREFIX}{:x}", digest.finalize())
}

/// Receipts of members the leader dispatched in this turn, when all of them
/// have settled and at least one reported. `None` means "do not converge".
pub fn convergence_inputs(ledger: &super::group_conversation::GroupTurnLedgerRecord, leader: &str) -> Option<Vec<String>> {
    use super::group_conversation::GroupMemberActivationState as State;
    let leader_receipt = ledger
        .members
        .iter()
        .find(|member| member.participant.agent_id == leader && member.has_committed_result())?
        .receipt_id
        .clone()?;
    let dispatched = ledger
        .members
        .iter()
        // Broadcast ordering alone is not a delegation. Only members woken
        // by the leader's actual assignment owe a result back to the leader.
        .filter(|member| member.participant.agent_id != leader
            && member.source_receipt_id.as_deref() == Some(leader_receipt.as_str()))
        .collect::<Vec<_>>();
    if dispatched.is_empty()
        || dispatched.iter().any(|member| matches!(member.state, State::Queued | State::Working | State::WaitingUser))
    {
        return None;
    }
    let receipts = dispatched
        .iter()
        .filter(|member| member.has_committed_result())
        .filter_map(|member| member.receipt_id.clone())
        .collect::<Vec<_>>();
    (!receipts.is_empty()).then_some(receipts)
}

/// After a room turn: when the leader dispatched members and they have all
/// reported, queue one leader convergence continuation (bounded chain).
pub fn queue_leader_convergence(store: &super::company::CompanyStore, group: &GroupTurnContext, turn_id: &str) -> Result<bool> {
    let Some(leader) = group.leader_agent_id.clone() else { return Ok(false) };
    let Some(ledger) = store.group_turn(&group.canonical_session_id, turn_id)? else { return Ok(false) };
    let Some(receipts) = convergence_inputs(&ledger, &leader) else { return Ok(false) };
    let snapshot = store.directory_snapshot()?;
    let intent = super::group_conversation::preview_group_activation(&snapshot, &group.group_id, &format!("@{leader}"))?.intent();
    anyhow::ensure!(intent.active_agent_ids == vec![leader.clone()], "leader convergence could not target the leader alone");
    let continuation_turn = leader_convergence_turn_id(&group.canonical_session_id, turn_id);
    let chained = turn_id.starts_with(LEADER_CONVERGENCE_PREFIX);
    store.with_coordination(|connection| {
        let now = Utc::now().to_rfc3339();
        ensure_mission_row(connection, &group.group_id, &now)?;
        let board = load_board(connection, &group.group_id)?;
        let cycles = if chained { board.leader_cycles + 1 } else { 1 };
        if cycles > MAX_LEADER_CYCLES {
            append_entry(connection, &group.group_id, "decision", &leader,
                None, "Convergence cycle cap reached; the leader answers with the results in hand.", &now)?;
            return Ok(false);
        }
        let original_request = connection
            .query_row(
                "SELECT original_request FROM company_group_turns WHERE canonical_session_id=?1 AND turn_id=?2",
                params![group.canonical_session_id, turn_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let continuation = super::company::GroupReadyContinuation {
            canonical_session_id: group.canonical_session_id.clone(),
            original_turn_id: turn_id.to_string(),
            turn_id: continuation_turn.clone(),
            activation: intent,
            predecessor_receipts: receipts,
            original_request,
        };
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO company_group_continuations(canonical_session_id,turn_id,payload_json) VALUES(?1,?2,?3)",
            params![group.canonical_session_id, continuation_turn, serde_json::to_string(&continuation)?],
        )?;
        if inserted == 1 {
            connection.execute(
                "UPDATE company_group_missions SET leader_cycles=?1,updated_at=?2 WHERE group_id=?3",
                params![cycles, now, group.group_id],
            )?;
            note_round_turn(connection, &group.group_id, &continuation_turn, &now)?;
        }
        Ok(inserted == 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::company_directory::HistoryAccess;
    use crate::runtime::group_conversation::GroupParticipant;

    fn db() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
    }

    fn participant(id: &str, role: &str, name: &str) -> GroupParticipant {
        GroupParticipant {
            agent_id: id.into(),
            internal_role: role.into(),
            display_name: name.into(),
            role_title: role.into(),
            color: "#112233".into(),
            icon_seed: id.into(),
            avatar: None,
            member_role: "member".into(),
            history_access: HistoryAccess::Full,
            history_start_message_index: 0,
            explicitly_mentioned: false,
        }
    }

    fn room() -> GroupTurnContext {
        GroupTurnContext {
            inspection_participants: Default::default(),
            tool_constraints: Default::default(),
            group_id: "build".into(),
            group_name: "Build Group".into(),
            canonical_session_id: "group-build".into(),
            participants: vec![
                participant("phoenix", "phoenix", "Tibo"),
                participant("leo", "coder", "Leo"),
                participant("iris", "frontend", "Iris"),
            ],
            discussion_rounds: 1,
            read_full_transcript: true,
            execution_waves: Vec::new(),
            execution_dependencies: None,
            leader_agent_id: Some("phoenix".into()),
        }
    }

    fn item(id: &str, owner: &str, deps: &[&str]) -> PlanItem {
        PlanItem {
            id: id.into(),
            title: format!("do {id}"),
            owner_agent_id: Some(owner.into()),
            depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
            status: "todo".into(),
        }
    }

    #[test]
    fn overlapping_claims_are_refused_unless_the_leader_reassigns() {
        let mut connection = db();
        let now = Utc::now();
        claim(&mut connection, "build", "leo", "src/ui", None, false, false, now).unwrap();
        let refused = claim(&mut connection, "build", "iris", "src/ui/app.js", None, false, false, now).unwrap_err();
        assert!(refused.to_string().contains("overlaps"), "{refused}");
        // Leader asking without reassign is still refused.
        assert!(claim(&mut connection, "build", "iris", "src/ui/app.js", None, true, false, now).is_err());
        // Disjoint scopes coexist; the same owner may extend its own claim.
        claim(&mut connection, "build", "iris", "src/api", None, false, false, now).unwrap();
        claim(&mut connection, "build", "leo", "src/ui/components", None, false, false, now).unwrap();
        let taken = claim(&mut connection, "build", "iris", "src/ui/app.js", None, true, true, now).unwrap();
        let active = list_claims(&connection, "build", &now.to_rfc3339()).unwrap();
        // Only claims overlapping the reassigned scope move; leo keeps the
        // sibling `src/ui/components` claim.
        assert!(active
            .iter()
            .filter(|claim| scopes_overlap(&claim.scope, "src/ui/app.js"))
            .all(|claim| claim.owner_agent_id == "iris"));
        assert!(active
            .iter()
            .any(|claim| claim.owner_agent_id == "leo" && claim.scope == "src/ui/components"));
        assert!(active.iter().any(|claim| claim.claim_id == taken.claim_id));
    }

    #[test]
    fn expired_claims_are_ignored() {
        let mut connection = db();
        let start = Utc::now();
        claim(&mut connection, "build", "leo", "src/runtime", Some(5), false, false, start).unwrap();
        let later = start + chrono::Duration::minutes(6);
        assert!(list_claims(&connection, "build", &later.to_rfc3339()).unwrap().is_empty());
        claim(&mut connection, "build", "iris", "src/runtime/company.rs", None, false, false, later).unwrap();
    }

    #[test]
    fn scope_overlap_uses_path_boundaries() {
        assert!(scopes_overlap("src/ui", "src/ui/app.js"));
        assert!(scopes_overlap("./src/ui/**", "src/ui"));
        assert!(scopes_overlap("docs, src/api", "src/api/v1"));
        assert!(scopes_overlap("*", "anything"));
        assert!(!scopes_overlap("src/ui", "src/uikit"));
        assert!(!scopes_overlap("src/ui", "src/api"));
    }

    #[test]
    fn diverge_round_hides_peer_answers_until_convergence() {
        let connection = db();
        let now = Utc::now().to_rfc3339();
        let board = set_mode(&connection, "build", GroupMode::Diverge, Some("turn-1"), &now).unwrap();
        let view = DivergeView::new(&board.diverge, "phoenix", "leo").unwrap();
        assert!(view.hides_contribution("iris", "turn-1"));
        assert!(!view.hides_contribution("leo", "turn-1"), "own answer stays visible");
        assert!(!view.hides_contribution("phoenix", "turn-1"), "leader brief stays visible");
        assert!(!view.hides_contribution("iris", "turn-0"), "earlier turns stay visible");
        assert!(DivergeView::new(&board.diverge, "phoenix", "phoenix").is_none(), "leader sees all");
        note_round_turn(&connection, "build", "group_lead_x", &now).unwrap();
        let board = load_board(&connection, "build").unwrap();
        assert!(DivergeView::new(&board.diverge, "phoenix", "leo").unwrap().hides_contribution("iris", "group_lead_x"));
        let converged = set_mode(&connection, "build", GroupMode::Converge, Some("turn-2"), &now).unwrap();
        assert!(DivergeView::new(&converged.diverge, "phoenix", "leo").is_none());
    }

    #[test]
    fn diverge_board_hides_peer_results_from_members() {
        let connection = db();
        let start = "2026-01-01T00:00:00+00:00";
        append_entry(&connection, "build", "result", "iris", None, "old public result", "2025-12-31T00:00:00+00:00").unwrap();
        set_mode(&connection, "build", GroupMode::Diverge, Some("turn-1"), start).unwrap();
        append_entry(&connection, "build", "result", "iris", None, "secret idea", "2026-01-01T00:01:00+00:00").unwrap();
        let group = room();
        let board = load_board(&connection, "build").unwrap();
        let view = DivergeView::new(&board.diverge, "phoenix", "leo");
        let member = render_board(&group, &board, &[], view.as_ref());
        assert!(member.contains("old public result"));
        assert!(!member.contains("secret idea"));
        let leader = render_board(&group, &board, &[], None);
        assert!(leader.contains("secret idea"));
    }

    #[test]
    fn dependency_gating_holds_members_until_prerequisites_are_done() {
        let connection = db();
        let now = Utc::now().to_rfc3339();
        let plan = set_plan(&connection, "build", vec![item("api", "leo", &[]), item("ui", "iris", &["api"])], &now).unwrap();
        assert!(!dependency_gated(&plan, "leo"));
        assert!(dependency_gated(&plan, "iris"));
        assert!(!dependency_gated(&plan, "phoenix"), "no items, not gated");
        assert_eq!(ready_items_for(&plan, "iris").len(), 0);
        set_item_status(&connection, "build", "api", "done", &now).unwrap();
        let plan = load_board(&connection, "build").unwrap().plan;
        assert!(!dependency_gated(&plan, "iris"));
        assert_eq!(ready_items_for(&plan, "iris")[0].id, "ui");
        // Cycles and unknown dependencies are refused.
        assert!(set_plan(&connection, "build", vec![item("a", "leo", &["b"]), item("b", "iris", &["a"])], &now).is_err());
        assert!(set_plan(&connection, "build", vec![item("a", "leo", &["zzz"])], &now).is_err());
    }

    #[test]
    fn decision_log_is_append_only_with_author_and_time() {
        let connection = db();
        append_entry(&connection, "build", "decision", "phoenix", None, "Ship option B", "2026-01-01T00:00:00+00:00").unwrap();
        let board = load_board(&connection, "build").unwrap();
        assert_eq!(board.decisions.len(), 1);
        assert_eq!(board.decisions[0].author_agent_id, "phoenix");
        assert_eq!(board.decisions[0].created_at, "2026-01-01T00:00:00+00:00");
        assert!(connection.execute("UPDATE company_group_board_entries SET text='rewritten'", []).is_err());
    }

    #[test]
    fn board_tool_enforces_leader_and_owner_rights() {
        let mut connection = db();
        let group = room();
        let now = Utc::now();
        let refused = execute_board_action(&mut connection, &group, "coder", Some("t1"), &serde_json::json!({"action":"set_brief","brief":"x"}), now).unwrap_err();
        assert!(refused.to_string().contains("leader"));
        execute_board_action(&mut connection, &group, "orchestrator", Some("t1"), &serde_json::json!({
            "action":"set_plan","items":[{"id":"api","title":"API","owner_agent_id":"Leo"},{"id":"ui","title":"UI","owner_agent_id":"iris","depends_on":["api"]}]
        }), now).unwrap();
        assert!(execute_board_action(&mut connection, &group, "frontend", Some("t1"), &serde_json::json!({"action":"result","item_id":"api","text":"done"}), now).is_err());
        execute_board_action(&mut connection, &group, "coder", Some("t1"), &serde_json::json!({"action":"result","item_id":"api","text":"endpoint live"}), now).unwrap();
        let board = load_board(&connection, "build").unwrap();
        assert!(board.plan.iter().find(|item| item.id == "api").unwrap().is_done());
        assert_eq!(board.plan.iter().find(|item| item.id == "api").unwrap().owner_agent_id.as_deref(), Some("leo"));
    }

    #[test]
    fn dispatch_lines_render_visible_pings_once() {
        let connection = db();
        connection.execute(
            "INSERT INTO company_group_dispatches(group_id,turn_id,target_agent_id,plan_item_id,text,created_at) VALUES('build','t1','leo','api','Build the API','now')",
            [],
        ).unwrap();
        let (lines, ids) = pending_dispatch_lines(&connection, "build", "t1").unwrap();
        assert!(lines.contains("@leo [api] — Build the API"));
        let group = room();
        let pinged = crate::runtime::group_conversation::authored_ping_targets(&lines, &group.participants);
        assert_eq!(pinged.iter().map(|p| p.agent_id.as_str()).collect::<Vec<_>>(), vec!["leo"]);
        mark_dispatches_posted(&connection, &ids).unwrap();
        assert!(pending_dispatch_lines(&connection, "build", "t1").unwrap().1.is_empty());
    }

    #[test]
    fn room_talk_rejects_members_but_lets_the_leader_assign() {
        let group = room();
        let to_peer = serde_json::json!({"to":"Iris","subject":"x","body":"y","mode":2});
        assert!(intercept_room_talk(&group, "coder", Some("t1"), &to_peer).unwrap().contains("in this room"));
        let broadcast = serde_json::json!({"to":"everyone","subject":"x","body":"y","mode":2,"room_mode":"broadcast"});
        assert!(intercept_room_talk(&group, "coder", Some("t1"), &broadcast).unwrap().contains("Only the group leader"));
        let leader_escalate = serde_json::json!({"to":"user","subject":"x","body":"y","mode":1,"room_mode":"escalate"});
        assert!(intercept_room_talk(&group, "orchestrator", Some("t1"), &leader_escalate).unwrap().contains("ask_user"));
        // Outside coworkers keep the normal talk path.
        let outside = serde_json::json!({"to":"Theo","subject":"x","body":"y","mode":1});
        assert!(intercept_room_talk(&group, "coder", Some("t1"), &outside).is_none());
        assert!(intercept_room_talk(&group, "orchestrator", Some("t1"), &outside).is_none());
    }

    #[test]
    fn leader_and_member_blocks_name_roles_mode_and_assignment() {
        let group = room();
        let protocol = leader_protocol(&group);
        for needle in ["diverge", "converge", "build", "red-team", "ONE coherent answer", "@leo"] {
            assert!(protocol.contains(needle), "{needle}");
        }
        let board = MissionBoard {
            group_id: "build".into(),
            mode: GroupMode::Build,
            plan: vec![item("api", "leo", &[])],
            ..MissionBoard::default()
        };
        let member = member_block(&group, "leo", &board);
        assert!(member.contains("Tibo leads"));
        assert!(member.contains("Mode: build"));
        assert!(member.contains("api [todo]"));
    }

    #[test]
    fn leader_prompt_runs_diverge_research_converge_build_in_order() {
        let protocol = leader_protocol(&room());
        let at = |needle: &str| protocol.find(needle).unwrap_or_else(|| panic!("missing {needle}"));
        assert!(at("(a) DIVERGE") < at("(b) RESEARCH"));
        assert!(at("(b) RESEARCH") < at("(c) CONVERGE"));
        assert!(at("(c) CONVERGE") < at("(d) BUILD"));
        for needle in [
            "Pure ideation (names, options, strategy, angles): DIVERGE → CONVERGE only",
            "Trivial or quick ask",
            "prior art, APIs",
            "record the decision with group_board decide",
            "the critic/reviewer checks the work",
            "give the user ONE answer",
            "If the user @mentions only you, answer it yourself with no dispatch or fan-out",
            "Do not take it over, redo it, or reassign it",
            "make it dark mode",
        ] {
            assert!(protocol.contains(needle), "{needle}");
        }
        let tool = group_board_tool_definition();
        assert!(tool.description.contains("diverge|research|converge|build"));
        let modes = tool.parameters["properties"]["mode"]["enum"].as_array().unwrap();
        assert_eq!(modes.iter().map(|m| m.as_str().unwrap()).collect::<Vec<_>>(), vec!["diverge", "research", "converge", "build"]);
    }

    #[test]
    fn research_mode_round_trips_on_the_board_and_old_rows_still_load() {
        let connection = db();
        let now = Utc::now().to_rfc3339();
        // A board written before `research` existed loads unchanged.
        set_mode(&connection, "build", GroupMode::Diverge, Some("turn-1"), &now).unwrap();
        assert_eq!(load_board(&connection, "build").unwrap().mode, GroupMode::Diverge);
        let board = set_mode(&connection, "build", GroupMode::Research, Some("turn-2"), &now).unwrap();
        assert_eq!(board.mode, GroupMode::Research);
        assert!(!board.diverge.open, "research closes the blind brainstorm");
        assert_eq!(GroupMode::parse("research").unwrap(), GroupMode::Research);
        assert!(render_board(&room(), &board, &[], None).contains("mode: research"));
        let member = member_block(&room(), "leo", &board);
        assert!(member.contains("Research mode"));
        // The leader can set it through the tool.
        let mut connection = connection;
        let value = execute_board_action(&mut connection, &room(), "phoenix", Some("turn-3"),
            &serde_json::json!({"action":"set_mode","mode":"research"}), Utc::now()).unwrap();
        assert!(value["board"].as_str().unwrap().contains("mode: research"));
    }

    #[test]
    fn convergence_waits_for_every_dispatched_member() {
        use crate::runtime::group_conversation::{GroupMemberActivationRecord, GroupMemberActivationState as State, GroupTurnLedgerRecord, GroupActivationSelection};
        let member = |id: &str, state: State, receipt: Option<&str>, source: Option<&str>| GroupMemberActivationRecord {
            activation_id: format!("a-{id}"),
            participant: participant(id, id, id),
            state,
            status_detail: String::new(),
            receipt_id: receipt.map(str::to_string),
            source_receipt_id: source.map(str::to_string),
            updated_at: "now".into(),
        };
        let mut ledger = GroupTurnLedgerRecord {
            activation: None,
            canonical_session_id: "group-build".into(),
            turn_id: "t1".into(),
            prompt_hash: "h".into(),
            group_id: "build".into(),
            roster_fingerprint: "f".into(),
            selection: GroupActivationSelection::Explicit,
            active_agent_ids: vec!["phoenix".into()],
            members: vec![
                member("phoenix", State::Done, Some("r-lead"), None),
                member("leo", State::Done, Some("r-leo"), Some("r-lead")),
                member("iris", State::Working, None, Some("r-lead")),
            ],
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(convergence_inputs(&ledger, "phoenix").is_none());
        ledger.members[2].state = State::Done;
        ledger.members[2].receipt_id = Some("r-iris".into());
        assert_eq!(convergence_inputs(&ledger, "phoenix").unwrap(), vec!["r-leo", "r-iris"]);
        for member in &mut ledger.members { member.source_receipt_id = None; }
        ledger.activation = Some(super::super::group_conversation::GroupActivationIntent {
            group_id: "build".into(), roster_fingerprint: "f".into(),
            selection: GroupActivationSelection::Everyone,
            active_agent_ids: vec!["phoenix".into(), "leo".into(), "iris".into()],
            execution_dependencies: Some(vec![super::super::group_conversation::GroupDependency {
                prerequisite: "phoenix".into(), dependent: "leo".into(),
            }]),
            inspection_participants: Default::default(),
            tool_constraints: Default::default(),
            execution_mode: super::super::group_conversation::GroupExecutionMode::Ordered,
            execution_waves: vec![vec!["phoenix".into()], vec!["leo".into(), "iris".into()]],
        });
        assert!(convergence_inputs(&ledger, "phoenix").is_none(), "a user broadcast is not a leader assignment");
        ledger.members.truncate(1);
        assert!(convergence_inputs(&ledger, "phoenix").is_none(), "nothing dispatched, nothing to converge");
        assert!(leader_convergence_turn_id("group-build", "t1").starts_with(LEADER_CONVERGENCE_PREFIX));
        assert!(leader_convergence_turn_id("group-build", "t1").len() <= 128);
    }
}
