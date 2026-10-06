//! Per-round wall-clock telemetry — turns "the agent feels slow" into a
//! measured breakdown. One JSON line per provider round lands in
//! `<state_root>/runs/round_timings.jsonl`: how long the model call took, how
//! long each tool ran, and how much a sidecar (vision caption / grounding /
//! OCR / extraction) added on top.
//!
//! The logger is a drop guard: the turn loop creates one per round and every
//! exit path — final, talk-yield, provider error, even a mid-turn Esc abort —
//! flushes the record when the guard drops. Writing is best-effort; telemetry
//! must never fail or slow a turn.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

/// Rotate the JSONL once it outgrows this; the previous generation is kept.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOOLS_PER_ROUND: usize = 256;

#[derive(Debug, Serialize)]
pub struct ToolTiming {
    pub tool: String,
    pub ms: u64,
    pub ok: bool,
    /// Present only when the runtime suppressed an unchanged repeated call.
    /// `blocked` feeds a recovery instruction back to the model; `stopped`
    /// ends the stagnant turn. Older telemetry has no field and stays valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_guard: Option<String>,
}

#[derive(Debug, Serialize)]
struct RoundRecord {
    /// RFC 3339 timestamp of the round START.
    ts: String,
    session: String,
    agent: String,
    round: usize,
    model: String,
    /// Non-secret provider/account route receipt. The account scope is a
    /// Phoenix auth-profile label, never credential material.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_id: Option<String>,
    /// Total characters shipped to the provider (all request messages) — the
    /// context-growth curve that explains decelerating turns.
    request_chars: usize,
    provider_ms: u64,
    /// True when the provider call failed this round (transient retry or hard
    /// error) — provider_ms is then the time spent on the FAILED attempt.
    provider_error: bool,
    input_tokens: u32,
    /// chars/4 estimate, only when the provider omitted input usage. Keeping
    /// it separate prevents an estimate from masquerading as reported usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_input_tokens: Option<u32>,
    output_tokens: u32,
    /// Input not satisfied by a provider-reported prompt-cache read. None when
    /// that provider supplies no cache telemetry.
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_input_tokens: Option<u32>,
    /// Provider-reported cache-read share in basis points (10_000 = 100%).
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_ratio_bps: Option<u16>,
    /// Prompt-cache telemetry (providers that report it). Healthy long turns
    /// show cache_read_tokens rising with creation clustered at compaction
    /// boundaries; creation-only means the prefix is being rewritten every
    /// round — a silent cache leak.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_tokens: Option<u32>,
    /// Pi-style cache diagnosis for this exact round. `unknown` means the
    /// provider supplied no cache fields; it must never be rendered as 0%.
    cache_state: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    miss_reason: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boundary_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction_mode: Option<String>,
    tools: Vec<ToolTiming>,
    tool_calls: usize,
    /// A diagnostic marker for the quota-drain shape: a 100k+ prompt replayed
    /// to make one tool action. Cross-round streaks are analyzed offline.
    high_replay_risk: bool,
    tool_ms: u64,
    /// Sidecar model time riding on tool results: vision captions, grounding,
    /// OCR, structured extraction.
    sidecar_ms: u64,
    /// Wall-clock for the whole round (provider + tools + sidecars + runtime).
    round_ms: u64,
    /// Bytes the tool-output compressor shaved during this round (process-wide
    /// delta over the round's lifetime — concurrent lanes can bleed into each
    /// other's rounds, but per-session sums stay honest). Proves compression
    /// is alive per round, next to the request_chars it holds down.
    #[serde(skip_serializing_if = "is_zero")]
    compress_saved_bytes: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

pub struct RoundLogger {
    path: PathBuf,
    started: Instant,
    saved_bytes_at_start: u64,
    record: RoundRecord,
}

impl RoundLogger {
    pub fn new(state_root: &Path, session: &str, agent: &str, round: usize) -> Self {
        Self {
            path: state_root.join("runs").join("round_timings.jsonl"),
            started: Instant::now(),
            saved_bytes_at_start: crate::tools::compress::saved_bytes_total(),
            record: RoundRecord {
                ts: chrono::Utc::now().to_rfc3339(),
                session: session.chars().take(256).collect(),
                agent: agent.chars().take(256).collect(),
                round,
                model: String::new(),
                provider: None,
                account_scope: None,
                route_id: None,
                request_chars: 0,
                provider_ms: 0,
                provider_error: false,
                input_tokens: 0,
                estimated_input_tokens: None,
                output_tokens: 0,
                effective_input_tokens: None,
                cache_read_ratio_bps: None,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                cache_state: "unknown".to_string(),
                miss_reason: Vec::new(),
                boundary_id: None,
                compaction_mode: None,
                tools: Vec::new(),
                tool_calls: 0,
                high_replay_risk: false,
                tool_ms: 0,
                sidecar_ms: 0,
                round_ms: 0,
                compress_saved_bytes: 0,
            },
        }
    }

    pub fn set_request(&mut self, model: &str, request_chars: usize) {
        self.record.model = model.chars().take(256).collect();
        self.record.request_chars = request_chars;
    }

    pub fn set_route(&mut self, provider: &str, model: &str, account_scope: Option<&str>) {
        let provider: String = provider.chars().take(128).collect();
        let model: String = model.chars().take(256).collect();
        let account_scope = account_scope.map(|value| value.chars().take(256).collect::<String>());
        self.record.provider = Some(provider.clone());
        self.record.model = model.clone();
        self.record.account_scope = account_scope.clone();
        self.record.route_id = Some(match account_scope {
            Some(account) => format!("{provider}:{model}:{account}"),
            None => format!("{provider}:{model}"),
        });
    }

    /// Mark a prompt-cache baseline reset. Multiple reasons can apply to one
    /// boundary (for example idle_gap + compaction) and are retained exactly.
    pub fn mark_cache_boundary(&mut self, reason: &str) {
        let reason: String = reason.chars().take(64).collect();
        if !reason.is_empty() && !self.record.miss_reason.contains(&reason) {
            self.record.miss_reason.push(reason);
        }
        if self.record.boundary_id.is_none() {
            self.record.boundary_id = Some(format!(
                "{}:{}",
                self.record.session.chars().take(192).collect::<String>(),
                self.record.round
            ));
        }
        self.record.cache_state = "reset".to_string();
    }

    pub fn mark_compaction(&mut self, mode: &str) {
        self.mark_cache_boundary("compaction");
        self.record.compaction_mode = Some(mode.chars().take(128).collect());
    }

    pub fn set_provider(&mut self, ms: u64, input_tokens: u32, output_tokens: u32) {
        self.record.provider_ms = ms;
        self.record.input_tokens = input_tokens;
        self.record.estimated_input_tokens = (input_tokens == 0 && self.record.request_chars > 0)
            .then(|| u32::try_from(self.record.request_chars / 4).unwrap_or(u32::MAX));
        self.record.output_tokens = output_tokens;
    }

    pub fn set_cache_usage(&mut self, creation_tokens: Option<u32>, read_tokens: Option<u32>) {
        self.record.cache_creation_tokens = creation_tokens;
        self.record.cache_read_tokens = read_tokens;
        if let Some(read) = read_tokens {
            self.record.effective_input_tokens =
                Some(self.record.input_tokens.saturating_sub(read));
            self.record.cache_read_ratio_bps = (self.record.input_tokens > 0).then(|| {
                let ratio =
                    u64::from(read).saturating_mul(10_000) / u64::from(self.record.input_tokens);
                ratio.min(10_000) as u16
            });
        }
        if self.record.boundary_id.is_none() {
            self.record.cache_state = match (creation_tokens, read_tokens) {
                (None, None) => "unknown",
                (_, Some(read)) if read > 0 => "hit",
                _ => "miss",
            }
            .to_string();
            if self.record.cache_state == "miss" {
                self.mark_miss_reason("provider_reported_zero_read");
            }
        }
    }

    fn mark_miss_reason(&mut self, reason: &str) {
        let reason = reason.to_string();
        if !self.record.miss_reason.contains(&reason) {
            self.record.miss_reason.push(reason);
        }
    }

    pub fn provider_failed(&mut self, ms: u64) {
        self.record.provider_ms = ms;
        self.record.provider_error = true;
        self.mark_cache_boundary("retry");
    }

    pub fn add_tool(&mut self, tool: &str, ms: u64, ok: bool) {
        if self.record.tools.len() >= MAX_TOOLS_PER_ROUND {
            return;
        }
        self.record.tools.push(ToolTiming {
            tool: tool.chars().take(256).collect(),
            ms,
            ok,
            loop_guard: None,
        });
    }

    pub fn add_loop_guard(&mut self, tool: &str, action: &str) {
        if self.record.tools.len() >= MAX_TOOLS_PER_ROUND {
            return;
        }
        self.record.tools.push(ToolTiming {
            tool: tool.chars().take(256).collect(),
            ms: 0,
            ok: false,
            loop_guard: Some(action.chars().take(32).collect()),
        });
    }

    pub fn add_sidecar_ms(&mut self, ms: u64) {
        self.record.sidecar_ms = self.record.sidecar_ms.saturating_add(ms);
    }
}

impl Drop for RoundLogger {
    fn drop(&mut self) {
        self.record.round_ms = self.started.elapsed().as_millis() as u64;
        self.record.tool_ms = self
            .record
            .tools
            .iter()
            .fold(0u64, |total, timing| total.saturating_add(timing.ms));
        self.record.tool_calls = self.record.tools.len();
        self.record.high_replay_risk =
            self.record.input_tokens >= 100_000 && self.record.tool_calls == 1;
        self.record.compress_saved_bytes =
            crate::tools::compress::saved_bytes_total().saturating_sub(self.saved_bytes_at_start);
        let Ok(line) = serde_json::to_string(&self.record) else {
            return;
        };
        append_line(&self.path, &line);
    }
}

fn append_line(path: &Path, line: &str) {
    use std::io::Write;
    let _ = crate::config::private_io::with_private_lock(path, || {
        // Size-bounded: keep exactly one previous generation, never grow
        // unbounded. The target lock serializes rotate+append across gateway
        // processes, and the private opener rejects symlinks/special files.
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_file() && meta.len() > MAX_LOG_BYTES => {
                let previous = path.with_extension("prev.jsonl");
                match std::fs::symlink_metadata(&previous) {
                    Ok(meta) if meta.file_type().is_file() => {
                        std::fs::remove_file(&previous)?;
                    }
                    Ok(_) => anyhow::bail!(
                        "refusing non-regular rotated timing log {}",
                        previous.display()
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                std::fs::rename(path, &previous)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            Ok(meta) if !meta.file_type().is_file() => {
                anyhow::bail!("refusing non-regular timing log {}", path.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = crate::runtime::open_private_log_append(path)?;
        writeln!(file, "{line}")?;
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_flushes_one_json_line_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut log = RoundLogger::new(dir.path(), "sess-1", "browser", 3);
            log.set_request("test-model", 12_345);
            log.set_provider(850, 1000, 50);
            log.set_cache_usage(Some(4096), Some(90_000));
            log.add_tool("browser_click", 420, true);
            log.add_tool("browser_state", 90, false);
            log.add_loop_guard("browser_state", "blocked");
            log.add_sidecar_ms(300);
        }
        let raw = std::fs::read_to_string(dir.path().join("runs/round_timings.jsonl")).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["session"], "sess-1");
        assert_eq!(v["agent"], "browser");
        assert_eq!(v["round"], 3);
        assert_eq!(v["model"], "test-model");
        assert_eq!(v["request_chars"], 12_345);
        assert_eq!(v["provider_ms"], 850);
        assert_eq!(v["provider_error"], false);
        assert_eq!(v["tools"].as_array().unwrap().len(), 3);
        assert_eq!(v["tools"][2]["loop_guard"], "blocked");
        assert_eq!(v["tool_ms"], 510);
        assert_eq!(v["sidecar_ms"], 300);
        assert_eq!(v["cache_creation_tokens"], 4096);
        assert_eq!(v["cache_read_tokens"], 90_000);
        assert_eq!(v["effective_input_tokens"], 0);
        assert_eq!(v["cache_read_ratio_bps"], 10_000);
        assert_eq!(v["tool_calls"], 3);
        assert_eq!(v["high_replay_risk"], false);
    }

    #[test]
    fn flags_one_action_replayed_against_a_huge_prompt() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut log = RoundLogger::new(dir.path(), "dreamina", "orchestrator", 17);
            log.set_request("gpt-5.6-sol", 488_958);
            log.set_provider(6_000, 146_048, 200);
            log.set_cache_usage(None, Some(140_672));
            log.add_tool("computer_click", 80, true);
        }
        let raw = std::fs::read_to_string(dir.path().join("runs/round_timings.jsonl")).unwrap();
        let value: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(value["effective_input_tokens"], 5_376);
        assert_eq!(value["tool_calls"], 1);
        assert_eq!(value["high_replay_risk"], true);
    }

    #[test]
    fn records_append_across_rounds() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..3 {
            let mut log = RoundLogger::new(dir.path(), "sess-2", "computer_use", round);
            log.set_provider(100 + round as u64, 10, 5);
        }
        let raw = std::fs::read_to_string(dir.path().join("runs/round_timings.jsonl")).unwrap();
        assert_eq!(raw.lines().count(), 3);
    }

    #[test]
    fn provider_failure_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut log = RoundLogger::new(dir.path(), "sess-3", "coder", 0);
            log.provider_failed(2500);
        }
        let raw = std::fs::read_to_string(dir.path().join("runs/round_timings.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(v["provider_error"], true);
        assert_eq!(v["provider_ms"], 2500);
    }

    #[cfg(unix)]
    #[test]
    fn timing_log_is_private_and_refuses_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timings.jsonl");
        append_line(&path, "{}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        let link = dir.path().join("linked.jsonl");
        symlink(&outside, &link).unwrap();
        append_line(&link, "changed");
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }
}
