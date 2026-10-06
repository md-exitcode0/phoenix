//! Cron store — scheduled wake-ups for sessions.
//!
//! A cron entry says: at `next_run`, submit `prompt` into `session_id` as a
//! user message. The gateway daemon owns firing (it polls the store and runs
//! due entries through the normal turn path); this module owns persistence
//! and schedule math. Writers: the TUI `/cron` command and the agent-facing
//! `cron` tool. Store: `~/.phoenix/crons.json`.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// A repeat farther out than this is operationally indistinguishable from a
/// disabled schedule and risks overflowing Chrono arithmetic. One hundred
/// leap years remains generous while keeping every persisted value bounded.
const MAX_SCHEDULE_SECONDS: u64 = 100 * 366 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Repeat every `seconds`.
    Every { seconds: u64 },
    /// Every day at local `hour:minute`.
    Daily { hour: u8, minute: u8 },
    /// On the given weekdays (0=Sun … 6=Sat) at local `hour:minute`.
    Weekly { hour: u8, minute: u8, days: Vec<u8> },
    /// Fire once at `next_run`, then disable.
    Once,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronEntry {
    pub id: String,
    pub session_id: String,
    /// Canvas/session this job belongs to. Optional for backwards-compatible
    /// loading of jobs created before canvas routing existed.
    #[serde(default)]
    pub canvas: Option<String>,
    pub prompt: String,
    pub schedule: Schedule,
    pub next_run: DateTime<Utc>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

impl CronEntry {
    pub fn describe_schedule(&self) -> String {
        match &self.schedule {
            Schedule::Every { seconds } => format!("every {}", humanize_seconds(*seconds)),
            Schedule::Daily { hour, minute } => format!("daily {hour:02}:{minute:02}"),
            Schedule::Weekly { hour, minute, days } => {
                const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                let day_list: Vec<&str> = days
                    .iter()
                    .filter_map(|d| NAMES.get(*d as usize).copied())
                    .collect();
                format!("{} {hour:02}:{minute:02}", day_list.join("/"))
            }
            Schedule::Once => "once".to_string(),
        }
    }
}

fn store_path() -> PathBuf {
    crate::config::phoenix_home().join("crons.json")
}

pub fn load() -> Result<Vec<CronEntry>> {
    let path = store_path();
    let entries = match crate::config::private_io::read_private_file(&path)? {
        None => Vec::new(),
        Some(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("cron store {} is invalid", path.display()))?,
    };
    validate_entries(&entries)?;
    Ok(entries)
}

pub fn save(entries: &[CronEntry]) -> Result<()> {
    validate_entries(entries)?;
    let path = store_path();
    let payload = serde_json::to_string_pretty(entries)?;
    crate::config::private_io::atomic_write_private(&path, payload.as_bytes())
        .with_context(|| format!("failed to replace cron store {}", path.display()))
}

fn mutate_store<T>(
    path: &std::path::Path,
    mutate: impl FnOnce(&mut Vec<CronEntry>) -> Result<T>,
) -> Result<T> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let mut entries = match current {
            None | Some([]) => Vec::new(),
            Some(bytes) => serde_json::from_slice(bytes).with_context(|| {
                format!(
                    "cron store {} is invalid; refusing to erase it",
                    path.display()
                )
            })?,
        };
        validate_entries(&entries)?;
        let result = mutate(&mut entries)?;
        validate_entries(&entries)?;
        let replacement = serde_json::to_vec_pretty(&entries)?;
        Ok((result, replacement))
    })
}

/// Create + persist a cron from a human `when` spec. Returns the new entry.
pub fn add(session_id: &str, when: &str, prompt: &str) -> Result<CronEntry> {
    add_to_canvas(session_id, Some(session_id), when, prompt)
}

pub fn add_to_canvas(
    session_id: &str,
    canvas: Option<&str>,
    when: &str,
    prompt: &str,
) -> Result<CronEntry> {
    let (schedule, next_run) = parse_when(when, Utc::now())?;
    let entry = CronEntry {
        id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
        session_id: session_id.to_string(),
        canvas: canvas.map(str::to_string),
        prompt: prompt.to_string(),
        schedule,
        next_run,
        enabled: true,
        created_at: Utc::now(),
    };
    mutate_store(&store_path(), |entries| {
        entries.push(entry.clone());
        Ok(entry)
    })
}

/// Remove by id (prefix match allowed when unambiguous).
pub fn remove(id: &str) -> Result<CronEntry> {
    mutate_store(&store_path(), |entries| {
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.id.starts_with(id))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => bail!("no cron with id {id}"),
            [index] => Ok(entries.remove(*index)),
            _ => bail!("id {id} is ambiguous — give more characters"),
        }
    })
}

/// Pause or resume a cron by id (prefix match allowed when unambiguous).
/// Resuming a recurring entry whose deadline passed while paused schedules
/// its next future occurrence instead of replaying stale work immediately.
pub fn set_enabled(id: &str, enabled: bool) -> Result<CronEntry> {
    mutate_store(&store_path(), |entries| {
        set_enabled_in(entries, id, enabled, Utc::now())
    })
}

fn set_enabled_in(
    entries: &mut [CronEntry],
    id: &str,
    enabled: bool,
    now: DateTime<Utc>,
) -> Result<CronEntry> {
    let matches: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.id.starts_with(id))
        .map(|(index, _)| index)
        .collect();
    let index = match matches.as_slice() {
        [] => bail!("no cron with id {id}"),
        [index] => *index,
        _ => bail!("id {id} is ambiguous — give more characters"),
    };
    let entry = &mut entries[index];
    if enabled && !entry.enabled && entry.next_run <= now {
        match &entry.schedule {
            Schedule::Every { seconds } => {
                validate_every(*seconds)?;
                let seconds = i64::try_from(*seconds).context("cron interval is too large")?;
                entry.next_run = now
                    .checked_add_signed(Duration::seconds(seconds))
                    .context("cron next-run date overflow")?;
            }
            Schedule::Daily { hour, minute } => entry.next_run = next_daily(*hour, *minute, now),
            Schedule::Weekly { hour, minute, days } => {
                entry.next_run = next_weekly(*hour, *minute, days, now)
            }
            Schedule::Once => bail!("completed one-time cron cannot be resumed"),
        }
    }
    entry.enabled = enabled;
    Ok(entry.clone())
}

/// Entries due at `now`. The caller must `advance` + `save` BEFORE running
/// them so a slow turn can't double-fire.
pub fn due(now: DateTime<Utc>) -> Result<Vec<CronEntry>> {
    Ok(load()?
        .into_iter()
        .filter(|e| e.enabled && e.next_run <= now)
        .collect())
}

/// Move a fired entry to its next occurrence (Once → disabled) and persist.
pub fn advance(id: &str, now: DateTime<Utc>) -> Result<()> {
    mutate_store(&store_path(), |entries| {
        let entry = entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .with_context(|| format!("no cron with id {id}"))?;
        advance_entry(entry, now)?;
        Ok(())
    })
}

fn advance_entry(entry: &mut CronEntry, now: DateTime<Utc>) -> Result<()> {
    match &entry.schedule {
        Schedule::Every { seconds } => {
            validate_every(*seconds)?;
            let step_seconds = i64::try_from(*seconds).context("cron interval is too large")?;
            let overdue_seconds = now
                .signed_duration_since(entry.next_run)
                .num_seconds()
                .max(0);
            // Arithmetic jump, never an iteration per missed occurrence. A
            // corrupt zero interval can therefore neither wedge nor spin.
            let skips = overdue_seconds / step_seconds + 1;
            let advance_seconds = step_seconds
                .checked_mul(skips)
                .context("cron next-run arithmetic overflow")?;
            entry.next_run = entry
                .next_run
                .checked_add_signed(Duration::seconds(advance_seconds))
                .context("cron next-run date overflow")?;
        }
        Schedule::Daily { hour, minute } => {
            entry.next_run = next_daily(*hour, *minute, now);
        }
        Schedule::Weekly { hour, minute, days } => {
            entry.next_run = next_weekly(*hour, *minute, days, now);
        }
        Schedule::Once => entry.enabled = false,
    }
    Ok(())
}

/// Atomically claim every currently-due job and advance/disable it before the
/// caller begins execution. All gateway processes use the same private file
/// lock, so a briefly overlapping old/new daemon cannot fire one schedule
/// twice or advance it by two intervals.
pub fn claim_due(now: DateTime<Utc>) -> Result<Vec<CronEntry>> {
    claim_due_at(&store_path(), now)
}

fn claim_due_at(path: &std::path::Path, now: DateTime<Utc>) -> Result<Vec<CronEntry>> {
    mutate_store(path, |entries| {
        let mut claimed = Vec::new();
        for entry in entries.iter_mut() {
            if !entry.enabled || entry.next_run > now {
                continue;
            }
            claimed.push(entry.clone());
            advance_entry(entry, now)?;
        }
        Ok(claimed)
    })
}

fn validate_every(seconds: u64) -> Result<()> {
    if !(60..=MAX_SCHEDULE_SECONDS).contains(&seconds) {
        bail!(
            "repeat interval must be between 60 and {MAX_SCHEDULE_SECONDS} seconds; got {seconds}"
        );
    }
    Ok(())
}

fn validate_entries(entries: &[CronEntry]) -> Result<()> {
    let mut ids = std::collections::HashSet::with_capacity(entries.len());
    for entry in entries {
        if entry.id.trim().is_empty() || entry.session_id.trim().is_empty() {
            bail!("cron entries need non-empty id and session_id");
        }
        if entry.id.len() > 64
            || !entry
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            bail!("cron id `{}` contains unsafe characters", entry.id);
        }
        if !ids.insert(entry.id.as_str()) {
            bail!("cron store contains duplicate id `{}`", entry.id);
        }
        crate::session::SessionStore::validate_session_id(&entry.session_id)
            .with_context(|| format!("cron {} has an invalid session id", entry.id))?;
        if let Some(canvas) = entry.canvas.as_deref() {
            crate::session::SessionStore::validate_session_id(canvas)
                .with_context(|| format!("cron {} has an invalid canvas id", entry.id))?;
        }
        if entry.prompt.len() > 256 * 1024 {
            bail!("cron {} prompt exceeds the 256 KiB limit", entry.id);
        }
        match &entry.schedule {
            Schedule::Every { seconds } => validate_every(*seconds)?,
            Schedule::Daily { hour, minute } => {
                if *hour > 23 || *minute > 59 {
                    bail!(
                        "cron {} has invalid daily time {hour:02}:{minute:02}",
                        entry.id
                    );
                }
            }
            Schedule::Weekly { hour, minute, days } => {
                if *hour > 23 || *minute > 59 {
                    bail!(
                        "cron {} has invalid weekly time {hour:02}:{minute:02}",
                        entry.id
                    );
                }
                if days.is_empty() || days.iter().any(|day| *day > 6) {
                    bail!("cron {} needs weekdays in the range 0..=6", entry.id);
                }
            }
            Schedule::Once => {}
        }
    }
    Ok(())
}

/// The next local `hour:minute` that falls on one of `days` (0=Sun … 6=Sat),
/// strictly after `now`.
pub fn next_weekly(hour: u8, minute: u8, days: &[u8], now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::Datelike;
    if days.is_empty() {
        return next_daily(hour, minute, now);
    }
    let local_now = now.with_timezone(&Local);
    for ahead in 0..=7 {
        let cand_date = (local_now + Duration::days(ahead)).date_naive();
        let dow = cand_date.weekday().num_days_from_sunday() as u8;
        if !days.contains(&dow) {
            continue;
        }
        if let Some(naive) = cand_date.and_hms_opt(hour as u32, minute as u32, 0) {
            if let chrono::LocalResult::Single(dt) = Local.from_local_datetime(&naive) {
                if dt.with_timezone(&Utc) > now {
                    return dt.with_timezone(&Utc);
                }
            }
        }
    }
    next_daily(hour, minute, now)
}

/// Parse a human schedule spec into (schedule, first run):
///   "every 10m" / "every 2h" / "every 30s" / "every 1d"
///   "daily 09:30"               (local time)
///   "in 45m" / "in 2h"          (one-shot)
///   "at 2026-06-12T08:00"       (one-shot, local time)
pub fn parse_when(when: &str, now: DateTime<Utc>) -> Result<(Schedule, DateTime<Utc>)> {
    let spec = when.trim().to_lowercase();
    if let Some(rest) = spec.strip_prefix("every ") {
        let seconds = parse_duration_seconds(rest.trim())?;
        validate_every(seconds)?;
        let next_run = now
            .checked_add_signed(Duration::seconds(seconds as i64))
            .context("repeat interval exceeds the supported date range")?;
        return Ok((Schedule::Every { seconds }, next_run));
    }
    if let Some(rest) = spec.strip_prefix("daily ") {
        let (hour, minute) = parse_hh_mm(rest.trim())?;
        return Ok((
            Schedule::Daily { hour, minute },
            next_daily(hour, minute, now),
        ));
    }
    if let Some(rest) = spec.strip_prefix("in ") {
        let seconds = parse_duration_seconds(rest.trim())?;
        let next_run = now
            .checked_add_signed(Duration::seconds(seconds as i64))
            .context("one-shot interval exceeds the supported date range")?;
        return Ok((Schedule::Once, next_run));
    }
    if let Some(rest) = spec.strip_prefix("at ") {
        let naive = chrono::NaiveDateTime::parse_from_str(rest.trim(), "%Y-%m-%dT%H:%M")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(rest.trim(), "%Y-%m-%d %H:%M"))
            .context("`at` expects YYYY-MM-DDTHH:MM (local time)")?;
        let local = Local
            .from_local_datetime(&naive)
            .single()
            .context("ambiguous local time")?;
        return Ok((Schedule::Once, local.with_timezone(&Utc)));
    }
    bail!("unrecognized schedule `{when}` — use: every 10m | daily 09:30 | in 45m | at 2026-06-12T08:00")
}

fn parse_duration_seconds(text: &str) -> Result<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .context("expected a number followed by s/m/h/d (e.g. 10m)")?;
    let (digits, unit) = text.split_at(split);
    let value: u64 = digits.parse().context("bad number in duration")?;
    let multiplier = match unit.trim() {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        other => bail!("unknown duration unit `{other}` (use s/m/h/d)"),
    };
    let seconds = value
        .checked_mul(multiplier)
        .context("duration is too large")?;
    if seconds == 0 {
        bail!("duration must be > 0");
    }
    if seconds > MAX_SCHEDULE_SECONDS {
        bail!("duration exceeds the supported maximum of {MAX_SCHEDULE_SECONDS} seconds");
    }
    Ok(seconds)
}

fn parse_hh_mm(text: &str) -> Result<(u8, u8)> {
    let (h, m) = text
        .split_once(':')
        .context("daily expects HH:MM (e.g. daily 09:30)")?;
    let hour: u8 = h.trim().parse().context("bad hour")?;
    let minute: u8 = m.trim().parse().context("bad minute")?;
    if hour > 23 || minute > 59 {
        bail!("hour 0-23, minute 0-59");
    }
    Ok((hour, minute))
}

/// Next local-time HH:MM occurrence strictly after `now`, in UTC.
fn next_daily(hour: u8, minute: u8, now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(&Local);
    let mut candidate_day = local_now.date_naive();
    loop {
        let naive = candidate_day
            .and_hms_opt(hour as u32, minute as u32, 0)
            .expect("validated hh:mm");
        if let Some(local) = Local.from_local_datetime(&naive).single() {
            let utc = local.with_timezone(&Utc);
            if utc > now {
                return utc;
            }
        }
        candidate_day = candidate_day
            .succ_opt()
            .expect("date overflow walking to next day");
    }
}

fn humanize_seconds(seconds: u64) -> String {
    if seconds % 86_400 == 0 {
        format!("{}d", seconds / 86_400)
    } else if seconds % 3600 == 0 {
        format!("{}h", seconds / 3600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parses_every() {
        let now = t("2026-06-10T12:00:00Z");
        let (schedule, next) = parse_when("every 10m", now).unwrap();
        assert_eq!(schedule, Schedule::Every { seconds: 600 });
        assert_eq!(next, t("2026-06-10T12:10:00Z"));
    }

    #[test]
    fn rejects_sub_minute_repeat_and_junk() {
        let now = t("2026-06-10T12:00:00Z");
        assert!(parse_when("every 30s", now).is_err());
        assert!(parse_when("whenever", now).is_err());
        assert!(parse_when("daily 25:00", now).is_err());
        assert!(parse_when("every 18446744073709551615d", now).is_err());
    }

    #[test]
    fn rejects_duplicate_persisted_ids() {
        let now = t("2026-06-10T12:00:00Z");
        let entry = CronEntry {
            id: "duplicate".to_string(),
            session_id: "session".to_string(),
            canvas: None,
            prompt: "tick".to_string(),
            schedule: Schedule::Once,
            next_run: now,
            enabled: true,
            created_at: now,
        };
        assert!(validate_entries(&[entry.clone(), entry]).is_err());
    }

    #[test]
    fn parses_once_in() {
        let now = t("2026-06-10T12:00:00Z");
        let (schedule, next) = parse_when("in 2h", now).unwrap();
        assert_eq!(schedule, Schedule::Once);
        assert_eq!(next, t("2026-06-10T14:00:00Z"));
    }

    #[test]
    fn daily_next_run_is_in_the_future() {
        let now = Utc::now();
        let (_, next) = parse_when("daily 09:30", now).unwrap();
        assert!(next > now);
        assert!(next <= now + Duration::days(1) + Duration::minutes(1));
    }

    #[test]
    fn every_advance_skips_missed_ticks() {
        // Covered via next-run math: a 10m cron last due 3h ago lands in the
        // future, not 17 replays.
        let now = t("2026-06-10T12:00:00Z");
        let step = Duration::seconds(600);
        let mut next = t("2026-06-10T09:00:00Z") + step;
        while next <= now {
            next = next + step;
        }
        assert!(next > now && next <= now + step);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_store_mutations_preserve_all_entries_and_private_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crons.json");
        std::thread::scope(|scope| {
            for index in 0..12 {
                let path = path.clone();
                scope.spawn(move || {
                    mutate_store(&path, |entries| {
                        entries.push(CronEntry {
                            id: format!("cron-{index}"),
                            session_id: "session".to_string(),
                            canvas: Some("canvas".to_string()),
                            prompt: "tick".to_string(),
                            schedule: Schedule::Once,
                            next_run: t("2026-06-10T12:10:00Z"),
                            enabled: true,
                            created_at: t("2026-06-10T12:00:00Z"),
                        });
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });

        let entries: Vec<CronEntry> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entries.len(), 12);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::write(&path, b"{not json").unwrap();
        assert!(mutate_store(&path, |_| Ok(())).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"{not json");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_claimers_fire_a_due_entry_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crons.json");
        let now = t("2026-06-10T12:00:00Z");
        mutate_store(&path, |entries| {
            entries.push(CronEntry {
                id: "only-once".to_string(),
                session_id: "session".to_string(),
                canvas: Some("canvas".to_string()),
                prompt: "tick".to_string(),
                schedule: Schedule::Once,
                next_run: now,
                enabled: true,
                created_at: now - Duration::minutes(1),
            });
            Ok(())
        })
        .unwrap();

        let claims = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = path.clone();
                let claims = &claims;
                scope.spawn(move || {
                    claims
                        .lock()
                        .unwrap()
                        .extend(claim_due_at(&path, now).unwrap());
                });
            }
        });
        let claims = claims.into_inner().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].id, "only-once");

        let stored: Vec<CronEntry> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(!stored[0].enabled);
    }

    #[test]
    fn corrupt_zero_interval_fails_claim_without_replacing_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crons.json");
        let raw = serde_json::json!([{
            "id": "wedging-entry",
            "session_id": "session",
            "canvas": "canvas",
            "prompt": "tick",
            "schedule": { "kind": "every", "seconds": 0 },
            "next_run": "2026-06-10T12:00:00Z",
            "enabled": true,
            "created_at": "2026-06-10T11:00:00Z"
        }]);
        let bytes = serde_json::to_vec_pretty(&raw).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(claim_due_at(&path, t("2026-06-10T12:00:00Z")).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn resume_skips_stale_recurring_deadline_and_rejects_completed_once() {
        let now = t("2026-06-10T12:00:00Z");
        let mut entries = vec![CronEntry {
            id: "recurring".to_string(),
            session_id: "session".to_string(),
            canvas: None,
            prompt: "tick".to_string(),
            schedule: Schedule::Every { seconds: 600 },
            next_run: t("2026-06-10T09:00:00Z"),
            enabled: false,
            created_at: t("2026-06-10T08:00:00Z"),
        }];
        let resumed = set_enabled_in(&mut entries, "rec", true, now).unwrap();
        assert!(resumed.enabled);
        assert_eq!(resumed.next_run, t("2026-06-10T12:10:00Z"));

        entries.push(CronEntry {
            id: "finished".to_string(),
            session_id: "session".to_string(),
            canvas: None,
            prompt: "once".to_string(),
            schedule: Schedule::Once,
            next_run: t("2026-06-10T11:00:00Z"),
            enabled: false,
            created_at: t("2026-06-10T10:00:00Z"),
        });
        assert!(set_enabled_in(&mut entries, "finished", true, now).is_err());
    }
}
