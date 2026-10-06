use crate::runtime::runner::AgentRunner;
use crate::runtime::CliEvent;

pub(crate) fn preview_for_error(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 240 {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(240).collect::<String>())
    }
}

pub(crate) fn emit_agent_thinking(runner: &AgentRunner, agent: &str, rationale: &str) {
    let text = rationale.trim();
    if text.is_empty() || text == "Provider requested native tool call(s)." {
        return;
    }
    runner.emit(CliEvent::AgentMessage {
        agent: agent.to_string(),
        text: text.to_string(),
    });
}

pub(crate) fn sanitize_error(value: &str) -> String {
    preview_for_error(value)
}

pub(crate) fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or(value)
}

/// Format a summary line for tool result display in the CLI.
pub(crate) fn format_tool_result_summary(success: bool, output: &str) -> String {
    let status = if success { "ok" } else { "FAIL" };
    let sample = first_line(output);
    let char_len = output.chars().count();
    let suffix = if char_len > 4000 {
        let cut = char_len - 4000;
        format!(" [+{cut} more chars]")
    } else {
        String::new()
    };
    let sample_chars = sample.chars().count();
    if sample_chars > 120 {
        let short: String = sample.chars().take(120).collect();
        format!("[{status}] {short}...{suffix}")
    } else {
        format!("[{status}] {sample}{suffix}")
    }
}

/// Full specialist summary returned to orchestrator after `talk` (mode=1).
pub(crate) fn specialist_result_for_orchestrator(value: &str) -> String {
    const MAX_CHARS: usize = 12_000;
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(MAX_CHARS).collect();
    format!("{truncated}\n\n[truncated — specialist report exceeded {MAX_CHARS} chars]")
}

#[cfg(test)]
pub(crate) fn outcome_excerpt(value: &str) -> String {
    let cleaned = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("##"))
        .take(16)
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        first_line(value).to_string()
    } else if cleaned.chars().count() > 1200 {
        format!("{}...", cleaned.chars().take(1200).collect::<String>())
    } else {
        cleaned
    }
}
