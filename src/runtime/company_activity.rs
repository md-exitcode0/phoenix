//! Ephemeral truth for the desktop's live coworker indicators.
//!
//! Durable company jobs explain background work and survive reconnects. The
//! foreground provider call is process-local by nature, so it lives here and
//! is removed by an ownership-token guard on every normal, error, panic, or
//! cancellation exit. Nothing in this registry is used to resume work.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};

const MAX_LIVE_ACTIVITIES: usize = 256;
const MAX_LABEL_CHARS: usize = 180;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveCoworkerActivity {
    pub session_id: String,
    pub internal_role: String,
    pub activity_label: String,
    pub provider_id: String,
    pub model: String,
    pub updated_at: String,
    #[serde(skip)]
    token: String,
}

#[derive(Debug)]
pub struct ActivityGuard {
    key: (String, String),
    token: String,
}

fn registry() -> &'static Mutex<HashMap<(String, String), LiveCoworkerActivity>> {
    static REGISTRY: OnceLock<Mutex<HashMap<(String, String), LiveCoworkerActivity>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn begin(
    session_id: &str,
    internal_role: &str,
    activity_label: &str,
    provider_id: &str,
    model: &str,
) -> ActivityGuard {
    let key = (session_id.to_string(), internal_role.to_string());
    let token = uuid::Uuid::new_v4().simple().to_string();
    let activity = LiveCoworkerActivity {
        session_id: session_id.to_string(),
        internal_role: internal_role.to_string(),
        activity_label: bounded_one_line(activity_label),
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        updated_at: Utc::now().to_rfc3339(),
        token: token.clone(),
    };
    let mut activities = registry().lock().unwrap_or_else(|p| p.into_inner());
    if activities.len() >= MAX_LIVE_ACTIVITIES && !activities.contains_key(&key) {
        if let Some(oldest) = activities
            .iter()
            .min_by_key(|(_, activity)| activity.updated_at.as_str())
            .map(|(key, _)| key.clone())
        {
            activities.remove(&oldest);
        }
    }
    activities.insert(key.clone(), activity);
    ActivityGuard { key, token }
}

/// Replace the configured route with the receipt from the provider that
/// actually succeeded. Fallback chains call this only after a response.
pub fn record_actual_route(session_id: &str, internal_role: &str, provider_id: &str, model: &str) {
    let key = (session_id.to_string(), internal_role.to_string());
    let mut activities = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(activity) = activities.get_mut(&key) {
        activity.provider_id = provider_id.to_string();
        activity.model = model.to_string();
        activity.updated_at = Utc::now().to_rfc3339();
    }
}

/// Replace the broad task subject with a short, truthful live action. The UI
/// animates this label; it is never inferred client-side from raw tool ids.
pub fn record_tool(session_id: &str, internal_role: &str, tool_name: &str) {
    let label = match tool_name {
        "index_codebase" => "Indexing codebase",
        "codebase_search" | "symbol_search" | "file_symbols" | "callers" | "callees" | "impact"
        | "call_path" | "grep" | "glob" | "list_directory" => "Exploring codebase",
        "read" => "Reading files",
        "write" | "str_replace" => "Editing files",
        "bash" => "Running a command",
        "memory_recall" | "recall" => "Searching memory",
        "memory_save" | "vital_memory_write" => "Saving memory",
        "talk" => "Talking to a coworker",
        "routine" => "Using a workflow",
        "ask_user" | "ask_for_login" => "Waiting for you",
        name if name.starts_with("browser_") => "Browsing",
        name if name.starts_with("computer_") => "Using the computer",
        name if name.starts_with("web_") => "Researching",
        name if name.starts_with("composio_") || name.starts_with("mcp_") => "Using an app",
        _ => "Working",
    };
    let key = (session_id.to_string(), internal_role.to_string());
    let mut activities = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(activity) = activities.get_mut(&key) {
        activity.activity_label = label.to_string();
        activity.updated_at = Utc::now().to_rfc3339();
    }
}

pub fn snapshot() -> Vec<LiveCoworkerActivity> {
    let mut activities = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    activities.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.internal_role.cmp(&b.internal_role))
    });
    activities
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut activities = registry().lock().unwrap_or_else(|p| p.into_inner());
        if activities
            .get(&self.key)
            .is_some_and(|activity| activity.token == self.token)
        {
            activities.remove(&self.key);
        }
    }
}

fn bounded_one_line(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LABEL_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_owner_guard_prevents_stale_drop_and_route_is_actual() {
        let session_id = format!("activity-test-{}", uuid::Uuid::new_v4().simple());
        let first = begin(&session_id, "coder", "first job", "configured", "m1");
        let second = begin(&session_id, "coder", "second job", "fallback", "m2");
        drop(first);
        record_actual_route(&session_id, "coder", "actual", "m3");
        let activity = snapshot()
            .into_iter()
            .find(|activity| activity.session_id == session_id && activity.internal_role == "coder")
            .unwrap();
        assert_eq!(activity.activity_label, "second job");
        assert_eq!(activity.provider_id, "actual");
        assert_eq!(activity.model, "m3");
        drop(second);
        assert!(!snapshot().iter().any(|activity| {
            activity.session_id == session_id && activity.internal_role == "coder"
        }));
    }

    #[test]
    fn tools_become_human_activity_labels() {
        let session_id = format!("activity-tool-test-{}", uuid::Uuid::new_v4().simple());
        let guard = begin(
            &session_id,
            "coder",
            "implement the thing",
            "provider",
            "model",
        );
        record_tool(&session_id, "coder", "symbol_search");
        assert_eq!(
            snapshot()
                .into_iter()
                .find(|activity| activity.session_id == session_id)
                .unwrap()
                .activity_label,
            "Exploring codebase"
        );
        record_tool(&session_id, "coder", "index_codebase");
        assert_eq!(
            snapshot()
                .into_iter()
                .find(|activity| activity.session_id == session_id)
                .unwrap()
                .activity_label,
            "Indexing codebase"
        );
        drop(guard);
    }
}
