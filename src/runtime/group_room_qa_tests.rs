//! Deterministic QA for the "Build Group" website mission: Tibo (leader),
//! Theo, Robin, Leon Lin, Remy and Rory. No network, no provider: these drive
//! the real routing, activation, delivery, board, claim, blindness and
//! convergence code with the mission brief Mike pastes into the room.

use chrono::{Duration, TimeZone, Utc};
use rusqlite::Connection;

use crate::runtime::company_directory::{
    AgentKind, AgentProfile, AgentRecord, DirectorySnapshot, GroupMemberRecord, GroupProfile,
    GroupRecord, HistoryAccess, LifecycleState,
};
use crate::runtime::group_conversation::{
    self as room, GroupActivationSelection, GroupDependency, GroupExecutionMode,
    GroupMemberActivationRecord, GroupMemberActivationState as State, GroupTurnContext,
    GroupTurnLedgerRecord, RoomDelivery,
};
use crate::runtime::group_coordination::{self as coord, GroupMode};
use crate::runtime::postbox::RoomSteerKind as Kind;

const GROUP: &str = "build-group";

/// Lines 3-57 of the approved Build Group prompt, verbatim.
fn mission() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/phoenix-website-mission.md");
    std::fs::read_to_string(path).expect("mission fixture")
}

/// (agent id, internal role, display name) in roster order.
const ROSTER: [(&str, &str, &str); 6] = [
    ("phoenix", "phoenix", "Tibo"),
    ("researcher", "researcher", "Theo"),
    ("coder", "coder", "Robin"),
    ("frontend", "frontend", "Leon Lin"),
    ("critic", "critic", "Remy"),
    ("marketing", "marketing", "Rory"),
];

fn directory() -> DirectorySnapshot {
    let agents = ROSTER.iter().enumerate().map(|(index, (id, role, name))| AgentRecord {
        profile: AgentProfile {
            agent_id: (*id).into(),
            internal_role: (*role).into(),
            display_name: (*name).into(),
            role_title: (*role).into(),
            description: "Build Group coworker".into(),
            color: "#112233".into(),
            icon_seed: (*id).into(),
            kind: AgentKind::CraftSpecialist,
            lifecycle: LifecycleState::Active,
            pinned: false,
            sort_order: index as i64,
            canonical_session_id: None,
            browser_profile_id: format!("agent-{id}"),
            metadata_json: "{}".into(),
        },
        archived_at: None,
        delete_after: None,
        created_at: "now".into(),
        updated_at: "now".into(),
        as_of_seq: 1,
    }).collect();
    let members = ROSTER.iter().enumerate().map(|(index, (id, _, _))| GroupMemberRecord {
        group_id: GROUP.into(),
        agent_id: (*id).into(),
        member_role: "member".into(),
        history_access: HistoryAccess::Full,
        history_start_message_index: 0,
        sort_order: index as i64,
        joined_at: "now".into(),
        as_of_seq: 1,
    }).collect();
    DirectorySnapshot {
        agents,
        groups: vec![GroupRecord {
            profile: GroupProfile {
                group_id: GROUP.into(),
                name: "Build Group".into(),
                description: "Phoenix website".into(),
                color: "#334455".into(),
                icon_seed: GROUP.into(),
                lifecycle: LifecycleState::Active,
                pinned: true,
                sort_order: 1,
                canonical_session_id: Some("group-build-group".into()),
                metadata_json: "{}".into(),
                leader_agent_id: Some("phoenix".into()),
            },
            archived_at: None,
            delete_after: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            as_of_seq: 1,
        }],
        members,
        ..DirectorySnapshot::default()
    }
}

fn context(message: &str) -> GroupTurnContext {
    room::resolve_group_turn(&directory(), GROUP, message).unwrap()
}

fn active(message: &str) -> Vec<String> {
    room::preview_group_activation(&directory(), GROUP, message).unwrap().active_agent_ids
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| id.to_string()).collect()
}

fn db() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    coord::migrate(&connection).unwrap();
    connection
}

// ── 1. The mission message ──────────────────────────────────────────────

#[test]
fn mission_fixture_is_the_approved_prompt() {
    let text = mission();
    assert!(text.starts_with("@everyone New mission"));
    assert!(text.trim_end().ends_with("screenshots plus a screen recording at desktop and mobile widths."));
}

#[test]
fn mission_wakes_the_whole_room_with_tibo_planning_first() {
    let directory = directory();
    let text = mission();
    let preview = room::preview_group_activation(&directory, GROUP, &text).unwrap();
    assert_eq!(preview.selection, GroupActivationSelection::Everyone);
    assert_eq!(preview.active_agent_ids, ids(&["phoenix", "researcher", "coder", "frontend", "critic", "marketing"]));
    // Leader first, then every member in ONE parallel wave: no accidental
    // serialisation from words like "before", "then" or "first" in the brief.
    assert_eq!(preview.execution_mode, GroupExecutionMode::Ordered);
    assert_eq!(preview.execution_waves, vec![
        ids(&["phoenix"]),
        ids(&["researcher", "coder", "frontend", "critic", "marketing"]),
    ]);
    let edges = preview.execution_dependencies.clone().unwrap();
    assert_eq!(edges.len(), 5);
    assert!(edges.iter().all(|edge| edge.prerequisite == "phoenix"));
    // What the desktop sends back is exactly what the daemon re-validates.
    let validated = room::validate_group_activation(&directory, &preview.intent()).unwrap();
    assert_eq!(validated.execution_waves, preview.execution_waves);
    assert_eq!(validated.active_agent_ids, preview.active_agent_ids);
    // Every member is explicitly addressed; Tibo leads.
    let context = context(&text);
    assert!(context.participants.iter().all(|p| p.explicitly_mentioned));
    assert!(context.is_leader("phoenix") && context.is_leader("orchestrator"));
    assert!(context.room_instruction("phoenix").contains("GROUP LEADER PROTOCOL"));
    for (id, ..) in &ROSTER[1..] {
        assert!(context.room_instruction(id).contains("GROUP MEMBER — Tibo leads"), "{id}");
        assert!(room::participant_lane(context.participants.iter().find(|p| p.agent_id == *id).unwrap()).is_some(), "{id} has a lane");
    }
}

#[test]
fn everyone_without_a_stored_leader_plans_with_the_first_member() {
    let mut directory = directory();
    directory.members.retain(|member| member.agent_id != "phoenix");
    directory.groups[0].profile.leader_agent_id = None;
    let preview = room::preview_group_activation(&directory, GROUP, "@everyone pitch a name").unwrap();
    // No stored leader: the first member leads, and still plans first.
    assert_eq!(preview.execution_waves.len(), 2);
    assert_eq!(preview.execution_waves[0], ids(&["researcher"]));
    // Two-person "@everyone" in a room whose leader alone is active is trivially fine.
    let solo = room::preview_group_activation(&directory, GROUP, "@Theo hi").unwrap();
    assert_eq!(solo.execution_mode, GroupExecutionMode::Parallel);
}

// ── 2. Ping parsing ─────────────────────────────────────────────────────

#[test]
fn user_pings_resolve_first_names_punctuation_and_multiple_names() {
    for (message, expected) in [
        ("@Leon take the hero", vec!["frontend"]),
        ("@Leon: you own the design", vec!["frontend"]),
        ("@leon, quick one", vec!["frontend"]),
        ("@Leon Lin please", vec!["frontend"]),
        ("@leon_lin please", vec!["frontend"]),
        ("(@Robin) wire D1", vec!["coder"]),
        ("**@Remy** review secrets", vec!["critic"]),
        ("@Theo and @Rory!", vec!["researcher", "marketing"]),
        ("@Robin and @Leon run the real app", vec!["coder", "frontend"]),
        ("@Tibo status?", vec!["phoenix"]),
        ("@frontend use @critic's notes", vec!["frontend", "critic"]),
    ] {
        assert_eq!(active(message), ids(&expected), "{message}");
    }
}

#[test]
fn code_quotes_links_and_emails_are_not_pings() {
    for message in [
        "`@Rory` is how you would ping him",
        "```\n@Rory draft it\n```",
        "> @Rory said hi",
        "see [the post](https://x.com/@Rory)",
        "https://example.com/@Rory",
        "mail rory@usephoenix.dev about it",
        "@Leonard is not here",
        "@Roryx typo",
    ] {
        // Unaddressed: only the leader acts.
        assert_eq!(active(message), ids(&["phoenix"]), "{message}");
    }
    // A code-quoted @everyone is not a fan-out either.
    assert_eq!(active("use `@everyone` sparingly"), ids(&["phoenix"]));
    assert!(!room::addresses_everyone("```\n@everyone\n```"));
    assert!(room::addresses_everyone("@all. go"));
    assert!(room::addresses_everyone("ok @everyone, ship it"));
}

#[test]
fn bare_names_wake_only_with_exact_display_name_and_no_explicit_ping() {
    assert_eq!(active("Theo can you check this"), ids(&["researcher"]));
    assert_eq!(active("theo can you check this"), ids(&["phoenix"]));
    // An explicit ping wins over prose names.
    assert_eq!(active("@Robin build it, Theo already researched"), ids(&["coder"]));
}

#[test]
fn ambiguous_first_names_ping_nobody() {
    let mut directory = directory();
    directory.agents[2].profile.display_name = "Leon Park".into();
    let preview = room::preview_group_activation(&directory, GROUP, "@Leon go").unwrap();
    assert_eq!(preview.active_agent_ids, ids(&["phoenix"]), "ambiguous: the leader decides");
    let preview = room::preview_group_activation(&directory, GROUP, "@Leon Lin go").unwrap();
    assert_eq!(preview.active_agent_ids, ids(&["frontend"]));
}

// ── 3. Mid-run delivery: everyone hears, only some act, nothing queues ────

fn plan(message: &str, running: &[&str]) -> Vec<(String, RoomDelivery)> {
    let directory = directory();
    let intent = room::preview_group_activation(&directory, GROUP, message).unwrap().intent();
    let context = room::resolve_group_turn(&directory, GROUP, message).unwrap();
    room::plan_room_delivery(&context, &intent.active_agent_ids, room::addresses_everyone(message), |lane| running.contains(&lane))
        .into_iter().map(|member| (member.agent_id, member.delivery)).collect()
}

const ALL_LANES: [&str; 6] = ["orchestrator", "researcher", "coder", "frontend", "critic", "marketing"];

#[test]
fn user_follow_up_to_a_running_member_is_steered_and_everyone_else_hears_it() {
    let delivered = plan("@Leon make the hero darker", &ALL_LANES);
    assert_eq!(delivered, vec![
        ("phoenix".to_string(), RoomDelivery::Steer(Kind::LeaderFyi)),
        ("researcher".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ("coder".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ("frontend".to_string(), RoomDelivery::Steer(Kind::MentionAct)),
        ("critic".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ("marketing".to_string(), RoomDelivery::Steer(Kind::Fyi)),
    ]);
    // Exactly one actor; nobody is started twice.
    assert_eq!(delivered.iter().filter(|(_, d)| matches!(d, RoomDelivery::Steer(k) if k.is_actionable())).count(), 1);
}

#[test]
fn unaddressed_follow_up_goes_to_the_leader_and_idle_pinged_members_start() {
    assert_eq!(plan("also add a FAQ", &["orchestrator", "frontend"]), vec![
        ("phoenix".to_string(), RoomDelivery::Steer(Kind::LeaderAct)),
        ("frontend".to_string(), RoomDelivery::Steer(Kind::Fyi)),
    ]);
    // Remy idle + pinged starts concurrently; Robin keeps running and hears it.
    assert_eq!(plan("@Remy check the honeypot", &["coder"]), vec![
        ("coder".to_string(), RoomDelivery::Steer(Kind::Fyi)),
        ("critic".to_string(), RoomDelivery::Start),
    ]);
    // Two pings, one busy and one idle.
    assert_eq!(plan("@Theo @Rory swap the launch copy", &["researcher"]), vec![
        ("researcher".to_string(), RoomDelivery::Steer(Kind::MentionAct)),
        ("marketing".to_string(), RoomDelivery::Start),
    ]);
}

#[test]
fn everyone_mid_run_replans_with_the_leader_and_narrowing_keeps_leader_first() {
    let delivered = plan("@everyone switch to dark mode", &["orchestrator", "coder"]);
    assert_eq!(delivered[0], ("phoenix".to_string(), RoomDelivery::Steer(Kind::EveryoneReplan)));
    assert!(delivered.contains(&("coder".to_string(), RoomDelivery::Steer(Kind::EveryoneAct))));
    // Idle room: all start, and the narrowed activation keeps leader-first.
    let directory = directory();
    let intent = room::preview_group_activation(&directory, GROUP, "@everyone switch to dark mode").unwrap().intent();
    let narrowed = room::narrow_activation(&intent, &intent.active_agent_ids);
    assert_eq!(narrowed, intent);
    room::resolve_group_turn_from_activation(&directory, &narrowed).unwrap();
    // Leader already running: idle members start without a dead edge to it.
    let started = ids(&["researcher", "frontend", "critic", "marketing"]);
    let narrowed = room::narrow_activation(&intent, &started);
    assert!(narrowed.execution_dependencies.as_ref().unwrap().is_empty());
    room::resolve_group_turn_from_activation(&directory, &narrowed).unwrap();
}

// ── 4. Phases, board rights and diverge blindness ───────────────────────

fn board_call(connection: &mut Connection, group: &GroupTurnContext, actor: &str, turn: &str, input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    coord::execute_board_action(connection, group, actor, Some(turn), &input, Utc::now())
}

#[test]
fn leader_advances_diverge_research_converge_build_and_members_cannot() {
    let mut connection = db();
    let group = context(&mission());
    let turn = "turn_build_mission";
    board_call(&mut connection, &group, "phoenix", turn, serde_json::json!({"action":"set_brief","brief":"usephoenix.dev launch site"})).unwrap();
    board_call(&mut connection, &group, "phoenix", turn, serde_json::json!({"action":"set_plan","items":[
        {"id":"pitch-leon","title":"Direction pitch","owner_agent_id":"Leon"},
        {"id":"pitch-theo","title":"Direction pitch + launch-site study","owner_agent_id":"@Theo"},
        {"id":"pitch-robin","title":"Direction pitch","owner_agent_id":"Robin"},
    ]})).unwrap();
    for mode in ["diverge", "research", "converge", "build"] {
        assert!(board_call(&mut connection, &group, "frontend", turn, serde_json::json!({"action":"set_mode","mode":mode})).is_err(), "members cannot move phases");
        board_call(&mut connection, &group, "phoenix", turn, serde_json::json!({"action":"set_mode","mode":mode})).unwrap();
        assert_eq!(coord::load_board(&connection, GROUP).unwrap().mode, GroupMode::parse(mode).unwrap());
    }
    let board = coord::load_board(&connection, GROUP).unwrap();
    assert_eq!(board.plan.iter().map(|item| item.owner_agent_id.clone().unwrap()).collect::<Vec<_>>(), ids(&["frontend", "researcher", "coder"]));
    assert!(!board.diverge.open, "converge closed the blind round");
    assert!(board_call(&mut connection, &group, "coder", turn, serde_json::json!({"action":"decide","text":"use Astro"})).is_err());
    assert!(board_call(&mut connection, &group, "coder", turn, serde_json::json!({"action":"result","item_id":"pitch-leon","text":"mine now"})).is_err(), "cannot complete someone else's item");
}

#[test]
fn diverge_pitches_stay_blind_until_converge_including_follow_up_turns() {
    let connection = db();
    let now = Utc::now().to_rfc3339();
    let board = coord::set_mode(&connection, GROUP, GroupMode::Diverge, Some("turn_mission"), &now).unwrap();
    // A user follow-up to one member mid-round is bound to the round.
    coord::note_round_turn(&connection, GROUP, "turn_followup", &now).unwrap();
    let board = coord::load_board(&connection, GROUP).map(|b| { assert_eq!(b.diverge.round, board.diverge.round); b }).unwrap();
    let leon = coord::DivergeView::new(&board.diverge, "phoenix", "frontend").unwrap();
    assert!(leon.hides_contribution("researcher", "turn_mission"), "Theo's pitch is hidden from Leon");
    assert!(leon.hides_contribution("coder", "turn_followup"));
    assert!(leon.hides_contribution("coder", "turn_mission.rf-coder"), "race-fallback turns belong to their message");
    assert!(!leon.hides_contribution("frontend", "turn_mission"), "own pitch visible");
    assert!(!leon.hides_contribution("phoenix", "turn_mission"), "leader's plan visible");
    assert!(!leon.hides_contribution("researcher", "turn_older"), "pre-round history visible");
    assert!(coord::DivergeView::new(&board.diverge, "phoenix", "phoenix").is_none(), "the leader sees every pitch");
    // Converge opens everything.
    let converged = coord::set_mode(&connection, GROUP, GroupMode::Converge, Some("group_lead_x"), &now).unwrap();
    assert!(coord::DivergeView::new(&converged.diverge, "phoenix", "frontend").is_none());
}

#[test]
fn turns_after_the_round_closes_are_not_bound() {
    let connection = db();
    let now = Utc::now().to_rfc3339();
    coord::set_mode(&connection, GROUP, GroupMode::Diverge, Some("t1"), &now).unwrap();
    coord::set_mode(&connection, GROUP, GroupMode::Build, Some("t2"), &now).unwrap();
    coord::note_round_turn(&connection, GROUP, "t3", &now).unwrap();
    assert_eq!(coord::load_board(&connection, GROUP).unwrap().diverge.turn_ids, ids(&["t1"]));
}

// ── 5. Build claims prevent double work ─────────────────────────────────

#[test]
fn claims_stop_robin_and_leon_doing_the_same_files() {
    let mut connection = db();
    let group = context("");
    let turn = "turn_build";
    let robin = board_call(&mut connection, &group, "coder", turn, serde_json::json!({"action":"claim","scope":"site/functions, site/schema.sql","ttl_minutes":90})).unwrap();
    assert_eq!(robin["claim"]["owner_agent_id"], "coder");
    let refused = board_call(&mut connection, &group, "frontend", turn, serde_json::json!({"action":"claim","scope":"site/functions/waitlist.ts"})).unwrap_err();
    assert!(format!("{refused:#}").contains("overlaps"), "{refused:#}");
    board_call(&mut connection, &group, "frontend", turn, serde_json::json!({"action":"claim","scope":"site/src/hero, site/public/fluffies"})).unwrap();
    // Leon cannot take it by himself, and cannot release Robin's claim.
    assert!(board_call(&mut connection, &group, "frontend", turn, serde_json::json!({"action":"claim","scope":"site/functions","reassign":true})).is_err());
    let claim_id = robin["claim"]["claim_id"].as_str().unwrap().to_string();
    assert!(board_call(&mut connection, &group, "frontend", turn, serde_json::json!({"action":"release","claim_id":claim_id})).is_err());
    // Only Tibo reassigns.
    board_call(&mut connection, &group, "phoenix", turn, serde_json::json!({"action":"claim","scope":"site/functions","owner_agent_id":"Leon","reassign":true})).unwrap();
    let claims = coord::list_claims(&connection, GROUP, &Utc::now().to_rfc3339()).unwrap();
    assert!(claims.iter().all(|claim| claim.owner_agent_id == "frontend"), "{claims:?}");
}

#[test]
fn expired_claims_free_the_scope() {
    let mut connection = db();
    let start = Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap();
    coord::claim(&mut connection, GROUP, "coder", "site/functions", Some(5), false, false, start).unwrap();
    assert!(coord::claim(&mut connection, GROUP, "frontend", "site/functions", None, false, false, start + Duration::minutes(4)).is_err());
    coord::claim(&mut connection, GROUP, "frontend", "site/functions", None, false, false, start + Duration::minutes(6)).unwrap();
}

// ── 6. Convergence: one leader summary, robust to a failed member ───────

fn ledger(members: Vec<GroupMemberActivationRecord>) -> GroupTurnLedgerRecord {
    let all = ids(&["phoenix", "researcher", "coder", "frontend", "critic", "marketing"]);
    GroupTurnLedgerRecord {
        activation: Some(room::GroupActivationIntent {
            inspection_participants: Default::default(),
            tool_constraints: Default::default(),
            group_id: GROUP.into(),
            roster_fingerprint: "f".into(),
            selection: GroupActivationSelection::Everyone,
            active_agent_ids: all.clone(),
            execution_mode: GroupExecutionMode::Ordered,
            execution_waves: vec![ids(&["phoenix"]), all[1..].to_vec()],
            execution_dependencies: Some(all[1..].iter().map(|id| GroupDependency { prerequisite: "phoenix".into(), dependent: id.clone() }).collect()),
        }),
        canonical_session_id: "group-build-group".into(),
        turn_id: "turn_mission".into(),
        prompt_hash: "h".into(),
        group_id: GROUP.into(),
        roster_fingerprint: "f".into(),
        selection: GroupActivationSelection::Everyone,
        active_agent_ids: all,
        members,
        created_at: "now".into(),
        updated_at: "now".into(),
    }
}

fn member(id: &str, state: State, receipt: Option<&str>) -> GroupMemberActivationRecord {
    let context = context("");
    GroupMemberActivationRecord {
        activation_id: format!("a-{id}"),
        participant: context.participants.iter().find(|p| p.agent_id == id).unwrap().clone(),
        state,
        status_detail: String::new(),
        receipt_id: receipt.map(str::to_string),
        source_receipt_id: None,
        updated_at: "now".into(),
    }
}

#[test]
fn everyone_mission_converges_once_after_all_members_report() {
    let mut record = ledger(vec![
        member("phoenix", State::Done, Some("r-tibo")),
        member("researcher", State::Done, Some("r-theo")),
        member("coder", State::Working, None),
        member("frontend", State::Done, Some("r-leon")),
        member("critic", State::Done, Some("r-remy")),
        member("marketing", State::Done, Some("r-rory")),
    ]);
    // Only members the leader's posted plan woke owe it a result.
    for member in record.members.iter_mut().skip(1) {
        member.source_receipt_id = Some("r-tibo".into());
    }
    assert!(coord::convergence_inputs(&record, "phoenix").is_none(), "Robin still working");
    record.members[2].state = State::Done;
    record.members[2].receipt_id = Some("r-robin".into());
    assert_eq!(coord::convergence_inputs(&record, "phoenix").unwrap(), ids(&["r-theo", "r-robin", "r-leon", "r-remy", "r-rory"]));
}

#[test]
fn a_member_that_errors_or_times_out_does_not_wedge_the_room() {
    let mut record = ledger(vec![
        member("phoenix", State::Done, Some("r-tibo")),
        member("researcher", State::Done, Some("r-theo")),
        member("coder", State::Blocked, None),
        member("frontend", State::Done, Some("r-leon")),
        member("critic", State::Done, Some("r-remy")),
        member("marketing", State::Done, Some("r-rory")),
    ]);
    for member in record.members.iter_mut().skip(1) {
        member.source_receipt_id = Some("r-tibo".into());
    }
    // The leader converges with what it has (Robin's failure is on the ledger).
    assert_eq!(coord::convergence_inputs(&record, "phoenix").unwrap().len(), 4);
    // If the leader itself failed, nothing converges and nobody loops.
    record.members[0].state = State::Blocked;
    record.members[0].receipt_id = None;
    assert!(coord::convergence_inputs(&record, "phoenix").is_none());
    // Everybody failed: nothing to converge on.
    let record = ledger(vec![
        member("phoenix", State::Done, Some("r-tibo")),
        member("researcher", State::Blocked, None),
        member("coder", State::Blocked, None),
    ]);
    assert!(coord::convergence_inputs(&record, "phoenix").is_none());
}

#[test]
fn a_ping_only_turn_converges_but_a_user_pinged_pair_does_not() {
    // "@Theo @Leon …" from the user: no leader, no convergence turn.
    let record = GroupTurnLedgerRecord {
        activation: None,
        members: vec![member("researcher", State::Done, Some("r1")), member("frontend", State::Done, Some("r2"))],
        ..ledger(Vec::new())
    };
    assert!(coord::convergence_inputs(&record, "phoenix").is_none());
}

// ── 7. Steer framing ────────────────────────────────────────────────────

#[test]
fn every_steer_kind_has_a_distinct_marker_and_peer_mentions_are_actionable() {
    let markers = Kind::ALL.iter().map(|kind| kind.marker()).collect::<std::collections::HashSet<_>>();
    assert_eq!(markers.len(), Kind::ALL.len());
    assert!(Kind::PeerMention.is_actionable());
    assert!(Kind::PeerMention.marker().contains("teammate"));
    assert_eq!(Kind::parse("peer-mention"), Some(Kind::PeerMention));
}
