//! Tool result envelopes for provider-native tool loops (Hermes + OpenCode patterns).

use crate::providers::ChatMessage;
use crate::runtime::ToolCallResult;

/// Hermes-style error JSON returned as tool content so the model can self-correct.
pub fn tool_error_json(message: impl AsRef<str>) -> String {
    serde_json::json!({ "error": message.as_ref() }).to_string()
}

pub fn native_tool_result_content(result: &ToolCallResult) -> String {
    // Structured connected-app batches are intentionally larger than a shell
    // receipt: a useful inbox/calendar/database answer may contain 20 compact
    // records from one API call. The old universal 4K cut hid 16 of 20 Gmail
    // messages and made the agent issue more calls looking for data it already
    // had. Keep that lane bounded, but large enough for the complete batch.
    let max_chars = if result.tool_name == "composio_run" {
        16_000
    } else {
        4_000
    };
    // The read tool already limits each page to 4,000 lines / 256 KiB and
    // reports continuation offsets. A second head/tail cut destroys the
    // middle of that page while its receipt still says those lines were read.
    // Live group review showed repeated reads of the same 139-line file just
    // to recover geometry definitions removed by this adapter.
    let output = if result.tool_name == "read" {
        result.output.clone()
    } else {
        truncate_tool_output(&result.output, max_chars)
    };
    serde_json::json!({
        "tool_name": result.tool_name.as_str(),
        "input_summary": result.input_summary.as_str(),
        "success": result.success,
        "output": output,
    })
    .to_string()
}

pub fn native_tool_error_content(tool_name: &str, error: &str) -> String {
    let output = truncate_tool_output(error, 4_000);
    if output.trim_start().starts_with('{') && output.contains("\"error\"") {
        output
    } else {
        tool_error_json(format!("Tool '{tool_name}' failed: {output}"))
    }
}

/// Append a tool-result message for every pending native call id (Hermes: always reply).
pub fn push_native_errors_for_calls(
    native_tool_messages: &mut Vec<ChatMessage>,
    native_call_ids: &[Option<String>],
    tool_names: &[String],
    error: &str,
) {
    for (index, call_id) in native_call_ids.iter().enumerate() {
        let Some(call_id) = call_id else {
            continue;
        };
        let tool_name = tool_names
            .get(index)
            .map(String::as_str)
            .unwrap_or("unknown");
        native_tool_messages.push(ChatMessage::tool_result(
            call_id.clone(),
            native_tool_error_content(tool_name, error),
        ));
    }
}

fn truncate_tool_output(output: &str, max_chars: usize) -> String {
    let char_len = output.chars().count();
    if char_len <= max_chars {
        output.to_string()
    } else {
        // Keep the retrieval receipt at the tail (Headroom's FULL-original
        // marker, remote file metadata, final status) instead of cutting it
        // off with the middle of a huge first line.
        let tail_chars = (max_chars / 4).clamp(256, 2_000);
        let head_chars = max_chars.saturating_sub(tail_chars);
        let head: String = output.chars().take(head_chars).collect();
        let tail: String = output
            .chars()
            .rev()
            .take(tail_chars)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let cut = char_len.saturating_sub(head_chars + tail_chars);
        format!("{head}\n\n[truncated {cut} middle chars — full output saved to session]\n\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_file_read_reaches_the_provider_without_a_second_middle_cut() {
        let output = format!("Read lines 1-139 of 139 from model.py.\n{}\ncritical_surface_contact = 3.0\n{}\nlast_line", "a".repeat(4_500), "b".repeat(4_500));
        let result = ToolCallResult {
            tool_name: "read".into(), input_summary: "model.py".into(),
            success: true, output: output.clone(),
        };
        let decoded: serde_json::Value = serde_json::from_str(&native_tool_result_content(&result)).unwrap();
        assert_eq!(decoded["output"].as_str(), Some(output.as_str()));
        assert!(decoded["output"].as_str().unwrap().contains("critical_surface_contact = 3.0"));
    }

    #[test]
    fn connected_app_batches_get_a_complete_but_bounded_native_envelope() {
        let result = ToolCallResult {
            tool_name: "composio_run".to_string(),
            input_summary: "gmail batch".to_string(),
            success: true,
            output: "m".repeat(13_923),
        };
        let encoded = native_tool_result_content(&result);
        let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded["output"].as_str().unwrap().chars().count(), 13_923);
        assert!(!decoded["output"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn oversized_results_keep_the_tail_recovery_receipt() {
        let output = format!(
            "{}<<headroom: FULL original: /safe/ccr/result.txt>>",
            "x".repeat(10_000)
        );
        let truncated = truncate_tool_output(&output, 4_000);
        assert!(truncated.contains("truncated"));
        assert!(truncated.contains("FULL original: /safe/ccr/result.txt"));
        assert!(truncated.chars().count() < 4_200);
    }
}
