//! `/burn` — where the tokens and the minutes actually go (donor idea:
//! getagentseal/codeburn, a local token-spend dashboard over other tools'
//! session logs). Phoenix's version reads its OWN per-round telemetry
//! (`runs/round_timings.jsonl`), which codeburn can't have: per-AGENT lanes,
//! per-model provider wait, and what compression shaved. No proxy, no keys,
//! nothing leaves the machine.
//!
//! Dollars are deliberately absent until a price table exists — wrong cost
//! numbers are worse than none. Tokens, rounds, and wall-clock are ground
//! truth from the telemetry.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

const MAX_TIMING_LOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_TIMING_ROWS: usize = 100_000;
const MAX_TIMING_LINE_BYTES: usize = 64 * 1024;

/// One parsed telemetry row (subset of round_timing::RoundRecord — tolerant
/// of missing fields so old generations still parse).
#[derive(Debug, Clone, Deserialize)]
pub struct BurnRow {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub account_scope: Option<String>,
    #[serde(default)]
    pub route_id: Option<String>,
    #[serde(default)]
    pub request_chars: u64,
    #[serde(default)]
    pub provider_ms: u64,
    #[serde(default)]
    pub provider_error: bool,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub estimated_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub tool_ms: u64,
    #[serde(default)]
    pub round_ms: u64,
    #[serde(default)]
    pub compress_saved_bytes: u64,
    /// Provider-reported prompt tokens read from cache this round (absent on
    /// lanes that don't report caching).
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_tokens: Option<u64>,
    #[serde(default)]
    pub cache_state: Option<String>,
    #[serde(default)]
    pub miss_reason: Vec<String>,
    #[serde(default)]
    pub boundary_id: Option<String>,
    #[serde(default)]
    pub compaction_mode: Option<String>,
}

impl BurnRow {
    /// Provider-reported input tokens when present, else the request-size
    /// estimate (chars/4) — several lanes (ollama-cloud) report 0.
    pub fn tokens_in(&self) -> u64 {
        if self.input_tokens > 0 {
            self.input_tokens
        } else {
            self.estimated_input_tokens
                .unwrap_or(self.request_chars / 4)
        }
    }

    /// True when tokens_in comes from the chars/4 estimate.
    pub fn tokens_in_estimated(&self) -> bool {
        self.input_tokens == 0 && self.request_chars > 0
    }
}

#[derive(Debug, Default, Clone)]
pub struct Bucket {
    pub rounds: u64,
    pub provider_ms: u64,
    pub tool_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub estimated: bool,
    /// Cache telemetry over the REPORTED subset only: input tokens from rows
    /// that carried real usage, and how many of those read from cache.
    pub reported_in: u64,
    pub cached_in: u64,
    pub cache_reported_rounds: u64,
    pub cache_unknown_rounds: u64,
    /// Input for rounds whose provider did not expose cache-read fields. This
    /// stays separate so the report never pretends unknown cache behavior was
    /// a miss.
    pub cache_unknown_in: u64,
}

impl Bucket {
    fn add(&mut self, row: &BurnRow) {
        self.rounds = self.rounds.saturating_add(1);
        self.provider_ms = self.provider_ms.saturating_add(row.provider_ms);
        self.tool_ms = self.tool_ms.saturating_add(row.tool_ms);
        self.tokens_in = self.tokens_in.saturating_add(row.tokens_in());
        self.tokens_out = self.tokens_out.saturating_add(row.output_tokens);
        self.estimated |= row.tokens_in_estimated();
        if let Some(cache_read) = row.cache_read_tokens {
            let anthropic_shape = row
                .provider
                .as_deref()
                .is_some_and(|provider| provider.contains("anthropic"))
                || row.model.to_ascii_lowercase().contains("claude");
            let prompt_tokens = if anthropic_shape {
                row.input_tokens
                    .saturating_add(cache_read)
                    .saturating_add(row.cache_creation_tokens.unwrap_or(0))
            } else {
                row.input_tokens
            };
            self.reported_in = self.reported_in.saturating_add(prompt_tokens);
            self.cached_in = self.cached_in.saturating_add(cache_read);
            self.cache_reported_rounds = self.cache_reported_rounds.saturating_add(1);
        } else {
            self.cache_unknown_rounds = self.cache_unknown_rounds.saturating_add(1);
            self.cache_unknown_in = self.cache_unknown_in.saturating_add(row.tokens_in());
        }
    }

    /// "cache 62%" when the lane reports usage; empty otherwise.
    fn cache_note(&self) -> String {
        if self.reported_in == 0 {
            return " · cache unavailable".to_string();
        }
        format!(
            " · cache {:.0}%",
            100.0 * self.cached_in as f64 / self.reported_in as f64
        )
    }

    fn non_cache_read_or_unknown_input(&self) -> u64 {
        self.reported_in
            .saturating_sub(self.cached_in)
            .saturating_add(self.cache_unknown_in)
    }
}

/// Load every telemetry row: current generation plus the rotated previous one.
pub fn load_rows(state_root: &Path) -> Vec<BurnRow> {
    match load_rows_result(state_root) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("burn telemetry is unreadable: {error:#}");
            Vec::new()
        }
    }
}

/// Truthful variant for callers that can surface an unreadable/corrupt log.
/// The legacy feed wrapper above remains infallible for API compatibility.
pub fn load_rows_result(state_root: &Path) -> Result<Vec<BurnRow>> {
    let runs = state_root.join("runs");
    let mut rows = Vec::new();
    for name in ["round_timings.jsonl", "round_timings.prev.jsonl"] {
        let path = runs.join(name);
        let Some(bytes) =
            crate::config::private_io::read_private_file_limited(&path, MAX_TIMING_LOG_BYTES)?
        else {
            continue;
        };
        let text =
            String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > MAX_TIMING_LINE_BYTES {
                anyhow::bail!(
                    "{} line {} is too large ({} bytes; max {MAX_TIMING_LINE_BYTES})",
                    path.display(),
                    line_no + 1,
                    line.len()
                );
            }
            let row = serde_json::from_str::<BurnRow>(line).with_context(|| {
                format!(
                    "{} has invalid JSON at line {}",
                    path.display(),
                    line_no + 1
                )
            })?;
            rows.push(row);
            if rows.len() > MAX_TIMING_ROWS {
                anyhow::bail!("burn telemetry has more than {MAX_TIMING_ROWS} rows");
            }
        }
    }
    rows.sort_by(|a, b| a.ts.cmp(&b.ts));
    derive_cache_boundaries(&mut rows);
    Ok(rows)
}

/// Backfill Pi-style boundaries for older telemetry and for fallback route
/// changes that can only be known after two concrete response receipts exist.
fn derive_cache_boundaries(rows: &mut [BurnRow]) {
    fn push_reason(reasons: &mut Vec<String>, reason: &str) {
        if !reasons.iter().any(|value| value == reason) {
            reasons.push(reason.to_string());
        }
    }

    let mut previous_by_session: BTreeMap<String, BurnRow> = BTreeMap::new();
    for row in rows {
        if row.cache_state.is_none() {
            row.cache_state = Some(
                match row.cache_read_tokens {
                    Some(read) if read > 0 => "hit",
                    Some(_) => "miss",
                    None => "unknown",
                }
                .to_string(),
            );
        }
        if let Some(previous) = previous_by_session.get(&row.session) {
            if previous.provider != row.provider
                && previous.provider.is_some()
                && row.provider.is_some()
            {
                push_reason(&mut row.miss_reason, "provider_change");
            }
            if previous.model != row.model && !previous.model.is_empty() && !row.model.is_empty() {
                push_reason(&mut row.miss_reason, "model_change");
            }
            if previous.account_scope != row.account_scope
                && previous.account_scope.is_some()
                && row.account_scope.is_some()
            {
                push_reason(&mut row.miss_reason, "account_change");
            }
            if previous.cache_read_tokens.is_some() && row.cache_read_tokens.is_none() {
                push_reason(&mut row.miss_reason, "fields_unavailable");
            }
        }
        if row.provider_ms > 0 && row.cache_state.as_deref() == Some("unknown") {
            push_reason(&mut row.miss_reason, "fields_unavailable");
        }
        if row.provider_error {
            push_reason(&mut row.miss_reason, "retry");
        }
        if !row.miss_reason.is_empty() && row.boundary_id.is_none() {
            row.boundary_id = Some(format!("{}:{}", row.session, row.ts));
            row.cache_state = Some("reset".to_string());
        }
        previous_by_session.insert(row.session.clone(), row.clone());
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_mins(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Render the burn report as plain feed lines. Pure over its input — the
/// tests feed synthetic rows.
pub fn render(rows: &[BurnRow]) -> Vec<String> {
    if rows.is_empty() {
        return vec![
            "burn — no telemetry yet (runs/round_timings.jsonl is empty); numbers appear after the first turns".to_string(),
        ];
    }
    let mut by_agent: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_model: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_day: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    let mut saved_bytes: u64 = 0;
    let boundaries: Vec<_> = rows
        .iter()
        .filter(|row| row.boundary_id.is_some())
        .collect();
    for row in rows {
        total.add(row);
        saved_bytes = saved_bytes.saturating_add(row.compress_saved_bytes);
        if !row.agent.is_empty() {
            by_agent.entry(row.agent.clone()).or_default().add(row);
        }
        if !row.model.is_empty() {
            by_model.entry(row.model.clone()).or_default().add(row);
        }
        if row.ts.len() >= 10 {
            by_day.entry(row.ts[..10].to_string()).or_default().add(row);
        }
    }
    let est_mark = if total.estimated { "≈" } else { "" };
    let mut out = Vec::new();
    out.push(format!(
        "burn · {} rounds · provider wait {} · tools {} · {est_mark}{} tok in / {} tok out · compression shaved ~{} tok",
        total.rounds,
        fmt_mins(total.provider_ms),
        fmt_mins(total.tool_ms),
        fmt_tokens(total.tokens_in),
        fmt_tokens(total.tokens_out),
        fmt_tokens(saved_bytes / 4),
    ));
    let weekly_lanes = weekly_lane_buckets(rows, chrono::Utc::now());
    if !weekly_lanes.is_empty() {
        out.push(
            "7-day lanes (agent · provider/model · input · non-cache-read or unknown):".to_string(),
        );
        let mut lanes = weekly_lanes.into_iter().collect::<Vec<_>>();
        lanes.sort_by_key(|(_, bucket)| std::cmp::Reverse(bucket.tokens_in));
        for (lane, bucket) in lanes.into_iter().take(12) {
            out.push(format!(
                "  {lane} · {} rounds · {} in · {} non-cache-read/unknown{}",
                bucket.rounds,
                fmt_tokens(bucket.tokens_in),
                fmt_tokens(bucket.non_cache_read_or_unknown_input()),
                if bucket.estimated {
                    " · includes estimates"
                } else {
                    ""
                },
            ));
        }
    }
    let week = weekly_bucket(rows, chrono::Utc::now());
    let planning_budget = crate::settings::effective_u64(
        "efficiency.weekly_input_budget_millions",
        &crate::settings::SettingsScope::Global,
    )
    .unwrap_or(20)
    .saturating_mul(1_000_000);
    out.push(format!(
        "7-day planning ledger · {} rounds · {} tok in / {} out · {:.1}% of local {}M input budget{}",
        week.rounds,
        fmt_tokens(week.tokens_in),
        fmt_tokens(week.tokens_out),
        100.0 * week.tokens_in as f64 / planning_budget.max(1) as f64,
        planning_budget / 1_000_000,
        if week.estimated { " · includes estimates" } else { "" },
    ));
    if let Some(latest) = boundaries.last() {
        let reasons = if latest.miss_reason.is_empty() {
            "unspecified reset".to_string()
        } else {
            latest.miss_reason.join(" + ")
        };
        let mode = latest
            .compaction_mode
            .as_deref()
            .map(|value| format!(" · {value}"))
            .unwrap_or_default();
        out.push(format!(
            "cache boundaries · {} total · latest {reasons}{mode} (round-local cache comparisons restart here)",
            boundaries.len(),
        ));
    }
    let mut agents: Vec<_> = by_agent.into_iter().collect();
    agents.sort_by_key(|(_, b)| std::cmp::Reverse(b.provider_ms));
    out.push("by agent (provider wait · rounds · tok in/out):".to_string());
    for (agent, b) in agents.iter().take(8) {
        out.push(format!(
            "  {agent:<14} {} · {} rounds · {est_mark}{}/{}",
            fmt_mins(b.provider_ms),
            b.rounds,
            fmt_tokens(b.tokens_in),
            fmt_tokens(b.tokens_out),
        ));
    }
    let mut models: Vec<_> = by_model.into_iter().collect();
    models.sort_by_key(|(_, b)| std::cmp::Reverse(b.provider_ms));
    out.push(
        "by model (cache % = prompt tokens read from cache, reported lanes only):".to_string(),
    );
    for (model, b) in models.iter().take(6) {
        out.push(format!(
            "  {model:<14} {} · {} rounds · avg {}s/round{}",
            fmt_mins(b.provider_ms),
            b.rounds,
            (b.provider_ms / b.rounds.max(1)) / 1000,
            b.cache_note(),
        ));
    }
    out.push("by day (last 7):".to_string());
    for (day, b) in by_day.iter().rev().take(7) {
        out.push(format!(
            "  {day} {} provider wait · {} rounds · {est_mark}{} tok in",
            fmt_mins(b.provider_ms),
            b.rounds,
            fmt_tokens(b.tokens_in),
        ));
    }
    out.push(
        "(tokens marked ≈ are request-size estimates; the 7-day percentage is Phoenix's configurable replay budget, NOT a provider subscription-quota percentage; dollars omitted until a price table exists)"
            .to_string(),
    );
    out
}

pub fn weekly_bucket(rows: &[BurnRow], now: chrono::DateTime<chrono::Utc>) -> Bucket {
    let cutoff = now - chrono::Duration::days(7);
    let mut bucket = Bucket::default();
    for row in rows {
        let Some(ts) = chrono::DateTime::parse_from_rfc3339(&row.ts)
            .ok()
            .map(|ts| ts.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        if ts >= cutoff && ts <= now + chrono::Duration::minutes(5) {
            bucket.add(row);
        }
    }
    bucket
}

/// Rolling seven-day usage split by the concrete agent/provider/model route.
/// This is the actionable view for Phoenix/Sol versus coworker/Luna drain;
/// grouping only by model would hide which worker consumed it.
pub fn weekly_lane_buckets(
    rows: &[BurnRow],
    now: chrono::DateTime<chrono::Utc>,
) -> BTreeMap<String, Bucket> {
    let cutoff = now - chrono::Duration::days(7);
    let mut lanes: BTreeMap<String, Bucket> = BTreeMap::new();
    for row in rows {
        let Some(ts) = chrono::DateTime::parse_from_rfc3339(&row.ts)
            .ok()
            .map(|ts| ts.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        if ts < cutoff || ts > now + chrono::Duration::minutes(5) {
            continue;
        }
        let agent = if row.agent.trim().is_empty() {
            "unknown-agent"
        } else {
            row.agent.trim()
        };
        let provider = row
            .provider
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown-provider");
        let model = if row.model.trim().is_empty() {
            "unknown-model"
        } else {
            row.model.trim()
        };
        lanes
            .entry(format!("{agent} · {provider}/{model}"))
            .or_default()
            .add(row);
    }
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        ts: &str,
        agent: &str,
        model: &str,
        provider_ms: u64,
        in_tok: u64,
        chars: u64,
    ) -> BurnRow {
        BurnRow {
            ts: ts.to_string(),
            session: "s".to_string(),
            agent: agent.to_string(),
            model: model.to_string(),
            provider: None,
            account_scope: None,
            route_id: None,
            request_chars: chars,
            provider_ms,
            provider_error: false,
            input_tokens: in_tok,
            estimated_input_tokens: None,
            output_tokens: 40,
            tool_ms: 10,
            round_ms: provider_ms + 20,
            compress_saved_bytes: 4000,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            cache_state: None,
            miss_reason: Vec::new(),
            boundary_id: None,
            compaction_mode: None,
        }
    }

    #[test]
    fn cache_hit_rate_shows_for_reported_lanes_only() {
        let mut cached = row(
            "2026-07-09T01:00:00Z",
            "Coder",
            "gpt-5.5",
            5000,
            40_000,
            150_000,
        );
        cached.cache_read_tokens = Some(30_000);
        let uncached = row(
            "2026-07-09T01:01:00Z",
            "Coder",
            "gpt-5.5",
            5000,
            40_000,
            150_000,
        );
        // glm reports nothing → label it unavailable, never as a 0% miss.
        let blind = row(
            "2026-07-09T01:02:00Z",
            "Browser",
            "glm-5.2",
            5000,
            0,
            150_000,
        );
        let all = render(&[cached, uncached, blind]).join("\n");
        // Only the row carrying cache fields participates: the second row is
        // unknown, not a reported zero. 30k cached over 40k prompt = 75%.
        assert!(all.contains("cache 75%"), "{all}");
        let glm_line = all.lines().find(|l| l.contains("glm-5.2")).unwrap();
        assert!(glm_line.contains("cache unavailable"), "{glm_line}");
    }

    #[test]
    fn renders_totals_by_agent_model_and_day() {
        let rows = vec![
            row(
                "2026-07-08T10:00:00Z",
                "Browser",
                "glm-5.2",
                8000,
                0,
                120_000,
            ),
            row(
                "2026-07-08T10:01:00Z",
                "Browser",
                "glm-5.2",
                9000,
                0,
                130_000,
            ),
            row(
                "2026-07-09T02:00:00Z",
                "Orchestrator",
                "gpt-5.5",
                12000,
                30_000,
                150_000,
            ),
        ];
        let lines = render(&rows);
        let all = lines.join("\n");
        assert!(all.contains("3 rounds"), "{all}");
        assert!(all.contains("Browser"), "{all}");
        assert!(all.contains("glm-5.2"), "{all}");
        assert!(all.contains("2026-07-09"), "{all}");
        // The glm rows report no usage → the estimate marker shows.
        assert!(all.contains('≈'), "{all}");
        // compression line present (3 rows × 4000 bytes = 12000 bytes ≈ 3k tok)
        assert!(all.contains("compression shaved"), "{all}");
    }

    #[test]
    fn estimates_input_tokens_from_request_chars_when_unreported() {
        let r = row(
            "2026-07-08T10:00:00Z",
            "Browser",
            "glm-5.2",
            1000,
            0,
            100_000,
        );
        assert_eq!(r.tokens_in(), 25_000);
        assert!(r.tokens_in_estimated());
        let r2 = row(
            "2026-07-08T10:00:00Z",
            "Coder",
            "gpt-5.5",
            1000,
            9000,
            100_000,
        );
        assert_eq!(r2.tokens_in(), 9000);
        assert!(!r2.tokens_in_estimated());
    }

    #[test]
    fn weekly_lanes_separate_phoenix_sol_from_coworker_luna() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-24T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut sol = row(
            "2026-08-23T10:00:00Z",
            "Phoenix",
            "gpt-5.6-sol",
            1000,
            100_000,
            400_000,
        );
        sol.provider = Some("openai-codex".to_string());
        sol.cache_read_tokens = Some(70_000);
        let mut luna = row(
            "2026-08-23T11:00:00Z",
            "Nico",
            "luna-max",
            1000,
            40_000,
            160_000,
        );
        luna.provider = Some("moonshot".to_string());
        let lanes = weekly_lane_buckets(&[sol, luna], now);
        let phoenix = lanes
            .get("Phoenix · openai-codex/gpt-5.6-sol")
            .expect("Phoenix/Sol lane");
        assert_eq!(phoenix.tokens_in, 100_000);
        assert_eq!(phoenix.non_cache_read_or_unknown_input(), 30_000);
        let nico = lanes
            .get("Nico · moonshot/luna-max")
            .expect("coworker/Luna lane");
        assert_eq!(nico.tokens_in, 40_000);
        assert_eq!(nico.non_cache_read_or_unknown_input(), 40_000);
    }

    #[test]
    fn empty_telemetry_renders_a_pointer_not_a_crash() {
        let lines = render(&[]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no telemetry yet"));
    }

    #[test]
    fn telemetry_reader_rejects_corruption_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        std::fs::create_dir(&runs).unwrap();
        std::fs::write(runs.join("round_timings.jsonl"), b"{not-json}\n").unwrap();
        assert!(load_rows_result(dir.path()).is_err());

        let file = std::fs::File::create(runs.join("round_timings.jsonl")).unwrap();
        file.set_len(MAX_TIMING_LOG_BYTES as u64 + 1).unwrap();
        assert!(load_rows_result(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn telemetry_reader_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        std::fs::create_dir(&runs).unwrap();
        let outside = dir.path().join("outside.jsonl");
        std::fs::write(&outside, b"{}\n").unwrap();
        symlink(&outside, runs.join("round_timings.jsonl")).unwrap();
        assert!(load_rows_result(dir.path()).is_err());
    }
}
