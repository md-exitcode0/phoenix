//! Native attention/completion notifications controlled by Settings.
//!
//! Linux is the current production target. Commands are invoked directly
//! without a shell, and notification failure never affects the agent turn.

use crate::runtime::CliEvent;

fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

fn enabled(key: &str, scope: &crate::settings::SettingsScope) -> bool {
    crate::settings::effective_bool(key, scope).unwrap_or(true)
}

pub fn publish(title: &str, body: &str) {
    publish_scoped(title, body, &crate::settings::SettingsScope::Global)
}

fn event_enabled(key: &str, scope: &crate::settings::SettingsScope) -> bool {
    enabled("notifications.enabled", scope) && enabled(key, scope)
}

/// Clicking a notification opens Phoenix on that conversation: the shell
/// watches this file and focuses the window on the named chat.
fn open_target(scope: &crate::settings::SettingsScope) -> Option<(&'static str, String)> {
    match scope {
        crate::settings::SettingsScope::Agent { id } => Some(("agent", id.clone())),
        crate::settings::SettingsScope::Group { id } => Some(("group", id.clone())),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn request_open(kind: &str, id: &str) {
    let path = crate::config::phoenix_home().join("desktop-open.json");
    let body = serde_json::json!({"kind": kind, "id": id, "at": chrono::Utc::now().timestamp_millis()});
    let _ = std::fs::write(path, body.to_string());
}

fn publish_scoped(title: &str, body: &str, scope: &crate::settings::SettingsScope) {
    if cfg!(test) || !enabled("notifications.enabled", scope) {
        return;
    }
    let title = clean(title, 120);
    let body = clean(body, 500);
    if title.is_empty() {
        return;
    }
    if enabled("notifications.system", scope) {
        #[cfg(target_os = "linux")]
        {
            let target = open_target(scope);
            let mut command = std::process::Command::new("notify-send");
            command.args([
                "--app-name=Phoenix",
                "--expire-time=7000",
                "--urgency=normal",
                // Completion/attention cards are live status, not durable
                // mail. Without this hint GNOME retains every card on the
                // lock screen until the user explicitly clears it.
                "--transient",
            ]);
            if target.is_some() {
                // A click on the card reports the default action on stdout.
                command.args(["--action=default=Open", "--wait"]).stdout(std::process::Stdio::piped());
            }
            if let Ok(mut child) = command.arg(&title).arg(&body).spawn() {
                if let Some((kind, id)) = target {
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut clicked = String::new();
                        if let Some(mut out) = child.stdout.take() { let _ = out.read_to_string(&mut clicked); }
                        let _ = child.wait();
                        if clicked.lines().any(|line| line.trim() == "default") { request_open(kind, &id); }
                    });
                }
            }
        }
    }
    if crate::settings::effective_bool("notifications.sound", scope).unwrap_or(false) {
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("canberra-gtk-play")
                .args(["--id", "message", "--description", "Phoenix"])
                .spawn();
        }
    }
}

fn scope_for_session(session_id: &str) -> crate::settings::SettingsScope {
    let Some(company) = crate::runtime::company::global_if_initialized() else {
        return crate::settings::SettingsScope::Global;
    };
    let Ok(snapshot) = company.directory_snapshot() else {
        return crate::settings::SettingsScope::Global;
    };
    match crate::runtime::agent_conversation::resolve_canonical_owner(&snapshot, session_id) {
        Ok(Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Group {
            group_id,
        })) => return crate::settings::SettingsScope::Group { id: group_id },
        Ok(Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Agent(owner))) => {
            return crate::settings::SettingsScope::Agent { id: owner.agent_id }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::error!("notification owner for [{session_id}] is ambiguous: {error:#}");
            return crate::settings::SettingsScope::Global;
        }
    }
    crate::settings::SettingsScope::Global
}

/// Human identity that owns a canonical conversation. The wire event keeps
/// carrying only a session id, so native and in-app notifications resolve the
/// editable company name at the boundary instead of claiming every answer
/// came from Phoenix.
pub(crate) fn session_display_name(session_id: &str) -> String {
    let Some(company) = crate::runtime::company::global_if_initialized() else {
        return "Phoenix".to_string();
    };
    let Ok(snapshot) = company.directory_snapshot() else {
        return "Phoenix".to_string();
    };
    session_display_name_from_snapshot(session_id, &snapshot)
}

fn event_setting_key(event: &CliEvent) -> Option<&'static str> {
    match event {
        CliEvent::AskUser { .. } => Some("notifications.attention"),
        CliEvent::FinalOutput(_) => Some("notifications.completions"),
        CliEvent::BackgroundAgentReturned { .. } => Some("notifications.coworkers"),
        // A specialist stage completion is internal lifecycle. Direct agent
        // turns also emit FinalOutput, while detached work has one canonical
        // BackgroundAgentReturned event. Notifying here produced two alerts
        // for one visible answer and one alert per internal chain stage.
        CliEvent::SpecialistCompleted { .. } => None,
        _ => None,
    }
}

fn session_display_name_from_snapshot(
    session_id: &str,
    snapshot: &crate::runtime::company_directory::DirectorySnapshot,
) -> String {
    match crate::runtime::agent_conversation::resolve_canonical_owner(snapshot, session_id) {
        Ok(Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Group {
            group_id,
        })) => snapshot
            .groups
            .iter()
            .find(|group| group.profile.group_id == group_id)
            .map(|group| group.profile.name.clone())
            .unwrap_or_else(|| "Phoenix".to_string()),
        Ok(Some(crate::runtime::agent_conversation::CanonicalConversationOwner::Agent(owner))) => {
            owner.display_name
        }
        Ok(None) | Err(_) => "Phoenix".to_string(),
    }
}

pub fn publish_event(session_id: &str, event: &CliEvent) {
    let scope = scope_for_session(session_id);
    let Some(setting_key) = event_setting_key(event) else {
        return;
    };
    if !event_enabled(setting_key, &scope) {
        return;
    }
    match event {
        CliEvent::AskUser {
            agent, questions, ..
        } => publish_scoped(
            &format!("{agent} needs your attention"),
            questions
                .first()
                .map(|question| question.question.as_str())
                .unwrap_or(session_id),
            &scope,
        ),
        CliEvent::FinalOutput(text) => publish_scoped(
            &format!("{} finished", session_display_name(session_id)),
            text,
            &scope,
        ),
        CliEvent::BackgroundAgentReturned {
            agent, ok, summary, ..
        } => publish_scoped(
            &format!(
                "{} {}",
                crate::runtime::delegation::agent_display_name(agent),
                if *ok { "finished" } else { "needs attention" }
            ),
            summary,
            &scope,
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::config::test_env::PhoenixHomeGuard;
    use crate::runtime::company_directory::{
        AgentKind, AgentProfile, AgentRecord, DirectorySnapshot, GroupProfile, GroupRecord,
        LifecycleState,
    };

    #[test]
    fn notification_text_is_bounded_and_strips_control_bytes() {
        let raw = format!("a\0{}", "b".repeat(600));
        let clean = super::clean(&raw, 120);
        assert!(!clean.contains('\0'));
        assert_eq!(clean.chars().count(), 120);
    }

    #[test]
    fn event_category_settings_exist_and_default_on() {
        let home = tempfile::tempdir().unwrap();
        let _guard = PhoenixHomeGuard::set_private(home.path());
        let scope = crate::settings::SettingsScope::Global;
        assert!(super::event_enabled("notifications.attention", &scope));
        assert!(super::event_enabled("notifications.completions", &scope));
        assert!(super::event_enabled("notifications.coworkers", &scope));
    }

    #[test]
    fn one_completion_event_owns_each_notification() {
        let direct = crate::runtime::CliEvent::FinalOutput("done".into());
        let stage = crate::runtime::CliEvent::SpecialistCompleted {
            agent: "Avery".into(),
            ok: true,
            summary: "done".into(),
        };
        let background = crate::runtime::CliEvent::BackgroundAgentReturned {
            agent: "school_coach".into(),
            subject: "school check".into(),
            ok: true,
            summary: "done".into(),
            body: "done".into(),
            handoff_id: "return_test".into(),
            requester: "orchestrator".into(),
            receiver: "school_coach".into(),
            status: "done".into(),
            reply_to: Some("return_test".into()),
            causation_id: None,
        };
        assert_eq!(
            super::event_setting_key(&direct),
            Some("notifications.completions")
        );
        assert_eq!(super::event_setting_key(&stage), None);
        assert_eq!(
            super::event_setting_key(&background),
            Some("notifications.coworkers")
        );
    }

    #[test]
    fn global_and_scoped_notification_settings_gate_real_events() {
        let home = tempfile::tempdir().unwrap();
        let _home_guard = PhoenixHomeGuard::set_private(home.path());
        let mut revision = match crate::settings::execute(crate::settings::SettingsCommand::Set {
            key: "notifications.enabled".into(),
            value: serde_json::json!(false),
            scope: crate::settings::SettingsScope::Global,
            expected_revision: Some(0),
        })
        .unwrap()
        {
            crate::settings::SettingsReply::Snapshot { snapshot } => snapshot.revision,
            _ => unreachable!(),
        };
        assert!(!super::event_enabled(
            "notifications.attention",
            &crate::settings::SettingsScope::Global
        ));

        revision = match crate::settings::execute(crate::settings::SettingsCommand::Set {
            key: "notifications.enabled".into(),
            value: serde_json::json!(true),
            scope: crate::settings::SettingsScope::Global,
            expected_revision: Some(revision),
        })
        .unwrap()
        {
            crate::settings::SettingsReply::Snapshot { snapshot } => snapshot.revision,
            _ => unreachable!(),
        };
        crate::settings::execute(crate::settings::SettingsCommand::Set {
            key: "notifications.attention".into(),
            value: serde_json::json!(false),
            scope: crate::settings::SettingsScope::Agent { id: "iris".into() },
            expected_revision: Some(revision),
        })
        .unwrap();
        assert!(!super::event_enabled(
            "notifications.attention",
            &crate::settings::SettingsScope::Agent { id: "iris".into() }
        ));
        assert!(super::event_enabled(
            "notifications.completions",
            &crate::settings::SettingsScope::Agent { id: "iris".into() }
        ));
        assert!(super::event_enabled(
            "notifications.attention",
            &crate::settings::SettingsScope::Agent { id: "leo".into() }
        ));
    }

    #[test]
    fn conversation_owner_name_labels_agent_and_group_notifications() {
        let snapshot = DirectorySnapshot {
            agents: vec![AgentRecord {
                profile: AgentProfile {
                    agent_id: "coder".into(),
                    internal_role: "coder".into(),
                    display_name: "Leo".into(),
                    role_title: "Coder".into(),
                    description: String::new(),
                    color: "#334455".into(),
                    icon_seed: "leo".into(),
                    kind: AgentKind::ResponsibilityOwner,
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 0,
                    canonical_session_id: Some("agent-leo".into()),
                    browser_profile_id: "agent-coder".into(),
                    metadata_json: "{}".into(),
                },
                archived_at: None,
                delete_after: None,
                created_at: String::new(),
                updated_at: String::new(),
                as_of_seq: 1,
            }],
            groups: vec![GroupRecord {
                profile: GroupProfile {
                    group_id: "launch".into(),
                    name: "Launch Room".into(),
                    description: String::new(),
                    color: "#556677".into(),
                    icon_seed: "launch".into(),
                    lifecycle: LifecycleState::Active,
                    pinned: false,
                    sort_order: 0,
                    canonical_session_id: Some("group-launch".into()),
                    metadata_json: "{}".into(),
                    leader_agent_id: None,
                },
                archived_at: None,
                delete_after: None,
                created_at: String::new(),
                updated_at: String::new(),
                as_of_seq: 2,
            }],
            ..Default::default()
        };
        assert_eq!(
            super::session_display_name_from_snapshot("agent-leo", &snapshot),
            "Leo"
        );
        assert_eq!(
            super::session_display_name_from_snapshot("group-launch", &snapshot),
            "Launch Room"
        );
        assert_eq!(
            super::session_display_name_from_snapshot("unknown", &snapshot),
            "Phoenix"
        );
    }
}
