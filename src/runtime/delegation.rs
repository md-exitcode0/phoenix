//! Generic orchestrator → specialist delegation (not coder-specific).

use crate::session::SubAgentType;

/// Specialists that can actually run. Custom coworkers are executable only
/// after Phoenix's receipt-gated provisioning has atomically published them;
/// a requested-but-incomplete hire is never a production capability.
pub fn specialist_is_executable(agent: SubAgentType) -> bool {
    // Volume workers are short-lived tool executors, not addressable company
    // coworkers. Their creation is gated by the volume-work runtime and they
    // never appear in `valid_talk_target_names`.
    if crate::sub_agents::volume_worker::is_agent(agent) {
        return true;
    }
    let compiled_capability = match agent {
        SubAgentType::Coder
        | SubAgentType::Researcher
        | SubAgentType::Frontend
        | SubAgentType::Presentation
        | SubAgentType::Finance
        | SubAgentType::Critic
        | SubAgentType::Planner
        | SubAgentType::Scribe
        | SubAgentType::Sales
        | SubAgentType::Marketing
        | SubAgentType::PersonalLogistics => true,
        // Historical implementation identities remain deserializable so old
        // data is not destroyed. They cannot receive new work: database work
        // belongs to Leo, security/testing to Remy, and browsing/desktop use
        // are universal tools on every visible coworker's own identity.
        SubAgentType::Browser
        | SubAgentType::ComputerUse
        | SubAgentType::Database
        | SubAgentType::Hacker
        | SubAgentType::Tester => false,
        SubAgentType::Custom(id) => {
            let label = crate::session::custom_agent_label(id);
            crate::sub_agents::registry::production_custom_roster()
                .iter()
                .any(|(role, _)| role == label)
        }
    };
    if !compiled_capability {
        return false;
    }

    // Unit tests share one process-global company store while independently
    // swapping PHOENIX_HOME. Letting whichever test initialized it first
    // become every other test's execution roster makes otherwise isolated
    // runner tests order-dependent. Production always takes the directory
    // branch below; pure snapshot tests cover the authority rule itself.
    if cfg!(test) {
        return true;
    }

    // Once the durable directory exists it is the execution authority, not a
    // compiled alias table.  A founding role removed from the user's visible
    // roster must not remain callable through a hidden backend route.  The
    // static capability table is retained only for bootstrap/legacy CLI use
    // before the company store has been initialized.
    let Some(company) = crate::runtime::company::global_if_initialized() else {
        return true;
    };
    let Ok(snapshot) = company.directory_snapshot() else {
        return false;
    };
    specialist_is_active_in_snapshot(agent, &snapshot)
}

fn specialist_is_active_in_snapshot(
    agent: SubAgentType,
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
) -> bool {
    let role = specialist_label(agent);
    snapshot.agents.iter().any(|record| {
        record.profile.internal_role.eq_ignore_ascii_case(role)
            && record.profile.lifecycle == crate::runtime::company_directory::LifecycleState::Active
    })
}

/// Map `talk` tool `to` field to a roster specialist. Built-in names first;
/// anything else resolves against the receipt-complete custom-agent registry.
pub fn specialist_from_talk_name(name: &str) -> Option<SubAgentType> {
    match specialist_from_live_company_name(name) {
        CompanyNameResolution::Resolved(agent) => return Some(agent),
        // Duplicate editable display names must not silently route private
        // work to whichever compiled alias happens to match.  The caller will
        // return an explicit unknown/ambiguous-target error and the model can
        // retry with the stable role or agent id.
        CompanyNameResolution::Ambiguous => return None,
        CompanyNameResolution::NotFound => {}
    }
    specialist_from_static_talk_name(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompanyNameResolution {
    Resolved(SubAgentType),
    Ambiguous,
    NotFound,
}

/// Resolve the user-editable identity in the live directory before falling
/// back to compiled historical aliases.  Stable ids/internal roles win over a
/// display-name collision.  This makes a rename immediately usable by every
/// coworker's `talk` tool without rewriting manifests or losing sessions.
fn specialist_from_live_company_name(name: &str) -> CompanyNameResolution {
    let Some(company) = crate::runtime::company::global_if_initialized() else {
        return CompanyNameResolution::NotFound;
    };
    let Ok(snapshot) = company.directory_snapshot() else {
        return CompanyNameResolution::NotFound;
    };
    specialist_from_directory_snapshot(name, &snapshot)
}

fn specialist_from_directory_snapshot(
    name: &str,
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
) -> CompanyNameResolution {
    let needle = name.trim();
    if needle.is_empty() {
        return CompanyNameResolution::NotFound;
    }

    // Agent ids and immutable runtime roles are unambiguous routing keys even
    // if a user deliberately gives another coworker the same visible name.
    if let Some(record) = snapshot.agents.iter().find(|record| {
        record.profile.agent_id.eq_ignore_ascii_case(needle)
            || record.profile.internal_role.eq_ignore_ascii_case(needle)
    }) {
        return specialist_from_static_talk_name(&record.profile.internal_role)
            .map(CompanyNameResolution::Resolved)
            .unwrap_or(CompanyNameResolution::NotFound);
    }

    let mut matches = snapshot.agents.iter().filter(|record| {
        record
            .profile
            .display_name
            .trim()
            .eq_ignore_ascii_case(needle)
    });
    let Some(first) = matches.next() else {
        return CompanyNameResolution::NotFound;
    };
    if matches.next().is_some() {
        return CompanyNameResolution::Ambiguous;
    }
    specialist_from_static_talk_name(&first.profile.internal_role)
        .map(CompanyNameResolution::Resolved)
        .unwrap_or(CompanyNameResolution::NotFound)
}

fn specialist_from_static_talk_name(name: &str) -> Option<SubAgentType> {
    // Accept BOTH the role key (coder, scribe…) AND the persona name Phoenix
    // actually uses when it hands off ("hand this to Quill"). Without the
    // persona aliases, `Phoenix can't hand off to Quill` — the name never
    // resolved, so the handoff silently failed.
    match name.trim().to_ascii_lowercase().as_str() {
        "coder" | "code" | "leo" | "spark" => Some(SubAgentType::Coder),
        "researcher" | "research" | "theo" | "scout" => Some(SubAgentType::Researcher),
        // Surf/browser remains a deserializable historical session kind, but
        // is not a coworker or talk destination in the rebuilt company. Every
        // real coworker receives the native browser tools directly.
        "browser" | "web" | "surf" => None,
        "frontend" | "design" | "ui" | "iris" => Some(SubAgentType::Frontend),
        "database" | "db" | "ada" | "vault" => Some(SubAgentType::Coder),
        "hacker" | "soren" | "cipher" => Some(SubAgentType::Critic),
        // "canvas" is Presentation's persona name. It was the one persona missing
        // from this table, so every `talk to: "Canvas"` handoff the orchestrator
        // prompt tells agents to make resolved to nothing and died silently.
        "presentation" | "slides" | "deck" | "report" | "documents" | "knowledge" | "elena"
        | "canvas" => Some(SubAgentType::Presentation),
        "finance" | "financial" | "money" | "purchasing" | "vera" | "felix" | "bart" => {
            Some(SubAgentType::Finance)
        }
        // ComputerUse remains deserializable for historical sessions, but is
        // no longer an executable coworker or talk destination.
        "computer_use" | "computer" | "desktop" | "ollie" | "pixel" => None,
        "critic" | "review" | "reviewer" | "systems" | "reliability" | "security" | "remy"
        | "hawk" => Some(SubAgentType::Critic),
        "tester" | "test" | "tests" | "qa" | "quinn" | "probe" => Some(SubAgentType::Critic),
        "planner" | "plan" | "planning" | "maya" | "compass" => Some(SubAgentType::Planner),
        // The communications coworker was removed: whoever holds the work
        // writes its own messages in the user's voice. These names resolve to
        // no one so a handoff cannot silently route to a retired lane.
        "scribe" | "communications" | "communication" | "inbox" | "email" | "writer"
        | "writing" | "copywriter" | "nora" | "quill" | "nico" => None,
        "sales" | "crm" | "revenue" | "owen" | "milo" => Some(SubAgentType::Sales),
        "marketing" | "campaigns" | "audience" | "june" => Some(SubAgentType::Marketing),
        "personal_logistics" | "operations" | "logistics" | "travel" | "appointments"
        | "errands" | "cleo" | "atlas" => Some(SubAgentType::PersonalLogistics),
        other => crate::sub_agents::registry::resolve_custom_talk_name(other),
    }
}

/// Current runtime-valid talk destinations. Pending custom hires deliberately
/// stay out until the evidence gates pass; Canvas/configuration use the disk
/// roster to show them and their next action.
pub fn valid_talk_target_names() -> Vec<String> {
    const FOUNDING_ROLES: [&str; 10] = [
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
    let mut names = Vec::new();
    if let Some(company) = crate::runtime::company::global_if_initialized() {
        if let Ok(snapshot) = company.directory_snapshot() {
            for record in snapshot.agents.into_iter().filter(|record| {
                record.profile.lifecycle
                    == crate::runtime::company_directory::LifecycleState::Active
            }) {
                names.push(record.profile.agent_id);
                names.push(record.profile.internal_role);
                names.push(record.profile.display_name);
            }
        }
    } else {
        names.extend(FOUNDING_ROLES.iter().map(|name| (*name).to_string()));
        names.extend(
            crate::sub_agents::registry::production_custom_roster()
                .into_iter()
                .map(|(role, _)| role),
        );
    }
    names.push("orchestrator".to_string());
    names.push("user".to_string());
    names.sort();
    names.dedup();
    names
}

pub fn specialist_label(agent: SubAgentType) -> &'static str {
    match agent {
        SubAgentType::Coder => "coder",
        SubAgentType::Researcher => "researcher",
        SubAgentType::Browser => "browser",
        SubAgentType::Frontend => "frontend",
        SubAgentType::Database => "database",
        SubAgentType::Hacker => "hacker",
        SubAgentType::Presentation => "presentation",
        SubAgentType::Finance => "finance",
        SubAgentType::ComputerUse => "computer_use",
        SubAgentType::Critic => "critic",
        SubAgentType::Tester => "tester",
        SubAgentType::Planner => "planner",
        SubAgentType::Scribe => "scribe",
        SubAgentType::Sales => "sales",
        SubAgentType::Marketing => "marketing",
        SubAgentType::PersonalLogistics => "personal_logistics",
        SubAgentType::Custom(id) => crate::session::custom_agent_label(id),
    }
}

/// Each agent's persona name — display identity only. Routing, sessions, and
/// `talk` targets always use the role label; personas exist so the user reads
/// "Leo (coder)" instead of a bare role string. Registry personas (custom
/// agents, built-in overrides) win over the compiled defaults.
pub fn agent_persona(role: &str) -> Option<&'static str> {
    let key = role.trim().to_ascii_lowercase();
    Some(match key.as_str() {
        "orchestrator" => "Phoenix",
        "planner" => "Maya",
        "coder" => "Leo",
        "researcher" => "Theo",
        // Historical browser sessions are folded into Phoenix during company
        // migration. Never reintroduce a thirteenth visible identity when an
        // old session label is rendered.
        "browser" => "Phoenix",
        "frontend" => "Iris",
        "presentation" => "Elena",
        "finance" => "Vera",
        "computer_use" | "computeruse" => "Computer Use",
        "database" => "Leo",
        "hacker" => "Remy",
        "critic" => "Remy",
        "tester" => "Remy",
        "scribe" => "Nico",
        "sales" => "Owen",
        "marketing" => "June",
        "personal_logistics" => "Cleo",
        "lib" | "librarian" => "Memory",
        "volume_worker" => "Worker",
        _ => return crate::sub_agents::registry::registry_persona(&key),
    })
}

/// User-facing agent name: `Leo (coder)`; parallel instances render as
/// `Leo (coder) #2`. Unknown roles pass through as-is.
/// The name the user gave this agent in the company directory ("Robin" for
/// the coder), cached briefly. The built-in personas ("Leo") are only a
/// fallback: labels built from them told coworkers the old names.
#[cfg(not(test))]
pub(crate) fn company_display_name(role: &str) -> Option<String> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, std::collections::HashMap<String, String>)>> = Mutex::new(None);
    let key = role.to_ascii_lowercase();
    {
        let cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some((_, names)) = cache.as_ref().filter(|(at, _)| at.elapsed() <= Duration::from_secs(10)) {
            return names.get(&key).cloned();
        }
    }
    // Read the directory without holding the lock: the store may itself ask
    // for an agent's display name.
    let names: std::collections::HashMap<String, String> = crate::runtime::company::global()
        .and_then(|store| store.directory_snapshot())
        .map(|snapshot| {
            snapshot.agents.into_iter().flat_map(|agent| {
                let name = agent.profile.display_name.trim().to_string();
                let mut keys = vec![agent.profile.agent_id.to_ascii_lowercase(), agent.profile.internal_role.to_ascii_lowercase()];
                if agent.profile.agent_id == "phoenix" { keys.push("orchestrator".into()); }
                if name.is_empty() { keys.clear(); }
                keys.into_iter().map(move |key| (key, name.clone())).collect::<Vec<_>>()
            }).collect()
        })
        .unwrap_or_default();
    let found = names.get(&key).cloned();
    *CACHE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some((Instant::now(), names));
    found
}
#[cfg(test)]
pub(crate) fn company_display_name(_role: &str) -> Option<String> { None }

pub fn agent_display_name(role: &str) -> String {
    let raw = role.trim();
    let (base, instance) = match raw.split_once('#') {
        Some((base, instance)) => (base.trim(), Some(instance.trim())),
        None => (raw, None),
    };
    let key = base.to_ascii_lowercase();
    if let Some(name) = company_display_name(&key) {
        return match instance {
            Some(n) => format!("{name} ({key}) #{n}"),
            None => format!("{name} ({key})"),
        };
    }
    match agent_persona(&key) {
        Some(persona) => match instance {
            Some(n) => format!("{persona} ({key}) #{n}"),
            None => format!("{persona} ({key})"),
        },
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::company_directory::{
        AgentKind, AgentProfile, AgentRecord, DirectorySnapshot, LifecycleState,
    };

    fn record(agent_id: &str, internal_role: &str, display_name: &str) -> AgentRecord {
        AgentRecord {
            profile: AgentProfile {
                agent_id: agent_id.to_string(),
                internal_role: internal_role.to_string(),
                display_name: display_name.to_string(),
                role_title: "Coworker".to_string(),
                description: "Test coworker".to_string(),
                color: "#112233".to_string(),
                icon_seed: agent_id.to_string(),
                kind: AgentKind::ResponsibilityOwner,
                lifecycle: LifecycleState::Active,
                pinned: false,
                sort_order: 0,
                canonical_session_id: Some(format!("agent-{agent_id}")),
                browser_profile_id: format!("agent-{agent_id}"),
                metadata_json: "{}".to_string(),
            },
            archived_at: None,
            delete_after: None,
            created_at: String::new(),
            updated_at: String::new(),
            as_of_seq: 1,
        }
    }

    #[test]
    fn editable_company_name_routes_to_the_stable_specialist() {
        let snapshot = DirectorySnapshot {
            agents: vec![record("coder", "coder", "Robin")],
            ..Default::default()
        };
        assert_eq!(
            specialist_from_directory_snapshot("Robin", &snapshot),
            CompanyNameResolution::Resolved(SubAgentType::Coder)
        );
        assert_eq!(
            specialist_from_directory_snapshot("CODER", &snapshot),
            CompanyNameResolution::Resolved(SubAgentType::Coder)
        );
    }

    #[test]
    fn duplicate_human_names_never_guess_a_private_destination() {
        let snapshot = DirectorySnapshot {
            agents: vec![
                record("coder", "coder", "Robin"),
                record("researcher", "researcher", "Robin"),
            ],
            ..Default::default()
        };
        assert_eq!(
            specialist_from_directory_snapshot("Robin", &snapshot),
            CompanyNameResolution::Ambiguous
        );
        assert_eq!(
            specialist_from_directory_snapshot("researcher", &snapshot),
            CompanyNameResolution::Resolved(SubAgentType::Researcher)
        );
    }

    #[test]
    fn durable_directory_is_the_execution_authority() {
        let active = DirectorySnapshot {
            agents: vec![record("coder", "coder", "Leo")],
            ..Default::default()
        };
        assert!(specialist_is_active_in_snapshot(
            SubAgentType::Coder,
            &active
        ));
        assert!(!specialist_is_active_in_snapshot(
            SubAgentType::Researcher,
            &active
        ));

        let mut dormant_record = record("coder", "coder", "Leo");
        dormant_record.profile.lifecycle = LifecycleState::Dormant;
        let dormant = DirectorySnapshot {
            agents: vec![dormant_record],
            ..Default::default()
        };
        assert!(!specialist_is_active_in_snapshot(
            SubAgentType::Coder,
            &dormant
        ));
    }

    #[test]
    fn retired_implementation_roles_are_capabilities_not_hidden_coworkers() {
        for retired in [
            SubAgentType::Browser,
            SubAgentType::ComputerUse,
            SubAgentType::Database,
            SubAgentType::Hacker,
            SubAgentType::Tester,
        ] {
            assert!(!specialist_is_executable(retired));
        }
        assert_eq!(
            specialist_from_static_talk_name("database"),
            Some(SubAgentType::Coder)
        );
        assert_eq!(
            specialist_from_static_talk_name("hacker"),
            Some(SubAgentType::Critic)
        );
        assert_eq!(
            specialist_from_static_talk_name("tester"),
            Some(SubAgentType::Critic)
        );
        assert_eq!(specialist_from_static_talk_name("browser"), None);
        assert_eq!(specialist_from_static_talk_name("computer_use"), None);

        let names = valid_talk_target_names();
        for hidden in ["browser", "computer_use", "database", "hacker", "tester"] {
            assert!(!names.iter().any(|name| name == hidden));
        }
    }
}
