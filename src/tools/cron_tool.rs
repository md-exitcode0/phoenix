//! Agent-facing `cron` tool — schedule wake-ups for this session.
//!
//! The gateway fires due crons by submitting the prompt as a user message
//! into the target session, so the agent literally "wakes up" to it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::ToolOutput;

#[derive(Debug, Deserialize)]
pub struct CronInput {
    /// "create" | "list" | "pause" | "resume" | "delete"
    pub action: String,
    /// For create: "every 10m" | "daily 09:30" | "in 45m" | "at 2026-06-12T08:00"
    #[serde(default)]
    pub when: Option<String>,
    /// For create: the message the session wakes up to.
    #[serde(default)]
    pub prompt: Option<String>,
    /// For delete: the cron id (prefix ok).
    #[serde(default)]
    pub id: Option<String>,
    /// Canvas to run in (its session id). Defaults to the current canvas.
    #[serde(default)]
    pub canvas: Option<String>,
}

pub fn execute(input: CronInput, session_id: &str) -> Result<ToolOutput> {
    match input.action.as_str() {
        "create" => {
            let when = input
                .when
                .as_deref()
                .filter(|w| !w.trim().is_empty())
                .context(
                    "create needs `when` (every 10m | daily 09:30 | in 45m | at 2026-06-12T08:00)",
                )?;
            let prompt = input
                .prompt
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .context("create needs `prompt` — what the session wakes up to")?;
            let target = input.canvas.as_deref().unwrap_or(session_id);
            let entry = crate::cron::add_to_canvas(target, Some(target), when, prompt)?;
            Ok(out(format!(
                "Schedule {} created: {} → owner conversation `{}` with: {}\nNext run: {}",
                entry.id,
                entry.describe_schedule(),
                entry.session_id,
                entry.prompt,
                entry
                    .next_run
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M %Z"),
            )))
        }
        "list" => {
            let entries = crate::cron::load()?;
            if entries.is_empty() {
                return Ok(out("No Phoenix schedules yet.".to_string()));
            }
            let mut listing =
                String::from("id · schedule · next run · owner conversation · prompt\n");
            for e in entries {
                listing.push_str(&format!(
                    "{} · {} · {} · {} · {}{}\n",
                    e.id,
                    e.describe_schedule(),
                    e.next_run
                        .with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M"),
                    e.canvas.as_deref().unwrap_or(&e.session_id),
                    preview(&e.prompt, 60),
                    if e.enabled { "" } else { " (disabled)" },
                ));
            }
            Ok(out(listing.trim_end().to_string()))
        }
        "delete" => {
            let id = input
                .id
                .as_deref()
                .filter(|i| !i.trim().is_empty())
                .context("delete needs `id`")?;
            let removed = crate::cron::remove(id)?;
            Ok(out(format!(
                "Cron {} deleted ({}).",
                removed.id,
                removed.describe_schedule()
            )))
        }
        "pause" | "resume" => {
            let id = input
                .id
                .as_deref()
                .filter(|i| !i.trim().is_empty())
                .with_context(|| format!("{} needs `id`", input.action))?;
            let enabled = input.action == "resume";
            let entry = crate::cron::set_enabled(id, enabled)?;
            Ok(out(format!(
                "Cron {} {} ({}).",
                entry.id,
                if enabled { "resumed" } else { "paused" },
                entry.describe_schedule()
            )))
        }
        other => bail!("unknown cron action `{other}` (create | list | pause | resume | delete)"),
    }
}

fn out(content: String) -> ToolOutput {
    ToolOutput {
        summary: content.lines().next().unwrap_or("cron").to_string(),
        content,
    }
}

fn preview(text: &str, max: usize) -> String {
    let clean = text.replace('\n', " ");
    if clean.chars().count() <= max {
        clean
    } else {
        format!("{}…", clean.chars().take(max).collect::<String>())
    }
}
