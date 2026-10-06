use crate::providers::contracts::NativeToolCall;
use crate::runtime::delegation::{agent_display_name, specialist_label};
use crate::runtime::{
    AgentArtifact, AgentOutcome, AgentTarget, ArtifactKind, FinalResponse, ProviderTurn,
    RequestedToolCall, ToolCallResult, TurnToolTranscriptEntry,
};

use super::text_utils::{
    first_line, format_tool_result_summary, preview_for_error, sanitize_error,
};

pub(crate) fn raw_contains_tool_intent(raw: &str) -> bool {
    raw.contains("\"tool_name\"") || raw.contains("\"tool_calls\"") || raw.contains("<tool")
}

pub(crate) fn agent_label_for_target(agent_target: &AgentTarget) -> &'static str {
    match agent_target {
        AgentTarget::Orchestrator => "orchestrator",
        AgentTarget::Specialist(agent_type) => specialist_label(*agent_type),
    }
}

/// User-facing display name for the agent behind a tool call, delegation, or
/// receipt: `Phoenix (orchestrator)`, `Compass (planner)`, `Leo (coder)`. Use
/// this for every CliEvent `agent` field so the user always reads the persona +
/// role, never a bare role string (a bare "planner" reads as "orchestrator" in
/// the feed and is the root of the planner-labeling bug). Parallel instances
/// render as `Leo (coder) #2` via `agent_display_name`'s instance suffix.
pub(crate) fn agent_display_name_for_target(agent_target: &AgentTarget) -> String {
    match agent_target {
        AgentTarget::Orchestrator => agent_display_name("orchestrator"),
        AgentTarget::Specialist(agent_type) => agent_display_name(specialist_label(*agent_type)),
    }
}

pub(crate) fn invalid_provider_json_feedback(
    agent_name: &str,
    content: &str,
    error: &str,
) -> String {
    let preview = preview_for_error(content);
    let retry_instruction = if content.trim().is_empty() {
        "The provider returned empty content. Retry now with exactly one Phoenix JSON object: either `tool_request` or `final`; do not return empty content."
            .to_string()
    } else {
        "Return the same substantive answer again, but as ONLY valid AgentTurnResponse JSON. Do not wrap it in Markdown. Do not include commentary outside JSON."
            .to_string()
    };

    format!(
        "{agent_name} provider output was not valid AgentTurnResponse JSON. Error: {}. Preview: {}. {}",
        sanitize_error(error),
        if preview.is_empty() {
            "(empty)"
        } else {
            &preview
        },
        retry_instruction
    )
}

pub(crate) fn invalid_provider_json_fallback(
    agent_name: &str,
    content: &str,
    error: &str,
) -> FinalResponse {
    let raw = content.trim();
    // Distinguish "model returned usable prose we should preserve" from "model
    // returned a broken/half-written JSON envelope" (e.g. a truncated
    // tool_request with a stray quote). Dumping a raw envelope leaks internals
    // to the user; salvage any human-readable text from it instead, and fall
    // back to a clean message when there is none.
    let final_markdown = if raw.is_empty() {
        format!(
            "## Result\n{agent_name} returned an empty or unparseable provider response, so Phoenix preserved the turn as a bounded fallback instead of crashing.\n\nThis needs a retry with a smaller scope or a stronger model if the same provider keeps returning empty output."
        )
    } else if looks_like_json_envelope(raw) {
        match salvage_envelope_text(raw) {
            Some(text) => text,
            None => format!(
                "## Result\n{agent_name} returned a malformed structured response (an incomplete tool/JSON envelope), so Phoenix did not surface its internals. The turn stayed alive.\n\nRetry — usually a smaller scope or a stronger model produces a clean result."
            ),
        }
    } else {
        raw.to_string()
    };

    FinalResponse {
        summary: first_line(&final_markdown).to_string(),
        final_markdown,
        changes_made: vec![],
        verification: vec![format!(
            "{agent_name} output failed AgentTurnResponse JSON parsing: {}",
            sanitize_error(error)
        )],
        execution_mode: "provider_unstructured_fallback".to_string(),
        tool_transcript: vec![],
    }
}

/// True when the raw content looks like a (possibly malformed) Phoenix/JSON
/// envelope rather than human-readable prose — so we must not dump it verbatim.
pub(crate) fn looks_like_json_envelope(raw: &str) -> bool {
    let trimmed = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim();
    trimmed.starts_with('{')
        && (trimmed.contains("\"type\"")
            || trimmed.contains("\"tool_calls\"")
            || trimmed.contains("\"tool_name\"")
            || trimmed.contains("\"final_markdown\""))
}

/// Best-effort recovery of a human-facing answer from a malformed envelope:
/// prefer a `final_markdown` or `summary` field if one is parseable; otherwise
/// return `None` so the caller shows a clean fallback rather than raw JSON.
pub(crate) fn salvage_envelope_text(raw: &str) -> Option<String> {
    for key in ["final_markdown", "summary"] {
        if let Some(value) = extract_json_string_field(raw, key) {
            if value.trim().len() > 1 {
                return Some(value);
            }
        }
    }
    None
}

/// Extract a single JSON string field's value by key from possibly-malformed
/// JSON, handling basic `\"`/`\n` escapes. Returns `None` if not found.
pub(crate) fn extract_json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = raw.find(&needle)?;
    let after = &raw[start + needle.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (_, ch) in chars {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    // Unterminated string (truncated envelope) — return what we recovered if useful.
    if out.trim().len() > 1 {
        Some(out)
    } else {
        None
    }
}

pub(crate) fn coerce_plain_text_final_response(
    _agent_target: &AgentTarget,
    content: &str,
    tool_results: &[ToolCallResult],
) -> Option<FinalResponse> {
    // Defense in depth: compatibility paths must never turn an inline
    // `<think>` or Phoenix `[Reasoning]` block into visible final output.
    let (dethought, _) = crate::runtime::turns::split_reasoning_tags(content);
    let visible = strip_visible_reasoning_blocks(&dethought);
    let trimmed = visible.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Native assistant text is a valid terminal response on every mainstream
    // agent harness (and on the Responses API itself). Phoenix used to reject
    // it unless the model called the Phoenix-only `final_answer` function. That
    // extra envelope turned healthy provider output into "unstructured mesh
    // response", then spent three repair calls asking the model to repeat the
    // same answer as JSON. Keep `final_answer` as the preferred rich receipt,
    // but accept provider-native text too.
    //
    // One narrow guard preserves the reason the old coercion was removed: a
    // no-tool promise such as "let me download it now" is not a result. Once a
    // tool ran, the runtime transcript is objective evidence and prose can
    // safely synthesize it. Answer-only turns (explanations, reviews, status)
    // remain valid with no tools.
    if tool_results.is_empty() && looks_like_unfinished_action(trimmed) {
        return None;
    }

    final_shape_from_provider_text(trimmed, tool_results)
}

fn looks_like_unfinished_action(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let opening = lower.lines().next().unwrap_or("").trim();
    [
        "i'll ",
        "i will ",
        "let me ",
        "next i'll ",
        "next i will ",
        "i'm going to ",
        "i am going to ",
    ]
    .iter()
    .any(|prefix| opening.starts_with(prefix))
        || (lower.contains("next steps") && !lower.contains("completed") && !lower.contains("done"))
}

fn final_shape_from_provider_text(
    content: &str,
    tool_results: &[ToolCallResult],
) -> Option<FinalResponse> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let verification = if tool_results.is_empty() {
        vec!["Provider returned plain text final output without recorded tool calls.".to_string()]
    } else {
        let mut items = vec![format!(
            "Provider returned plain text final output after {} recorded tool call(s); Phoenix preserved the runtime transcript.",
            tool_results.len()
        )];
        items.extend(
            tool_results
                .iter()
                .filter(|result| !result.success)
                .map(|result| {
                    format!(
                        "Tool `{}` failed during the turn: {}",
                        result.tool_name,
                        first_line(&result.output)
                    )
                }),
        );
        items
    };

    let execution_mode = if tool_results.is_empty() {
        "provider_text_final_wrapped"
    } else {
        "provider_native_tool_loop_text_final"
    };

    Some(FinalResponse {
        summary: first_line(trimmed).to_string(),
        final_markdown: trimmed.to_string(),
        changes_made: vec![],
        verification,
        execution_mode: execution_mode.to_string(),
        tool_transcript: tool_results
            .iter()
            .map(|result| TurnToolTranscriptEntry {
                name: result.tool_name.clone(),
                input_summary: result.input_summary.clone(),
                outcome: format_tool_result_summary(result.success, &result.output),
            })
            .collect(),
    })
}

pub(crate) fn extract_artifact_bullets(outcome: &AgentOutcome, title: &str) -> Vec<String> {
    outcome
        .artifacts
        .iter()
        .filter(|artifact| artifact.title == title)
        .flat_map(|artifact| artifact.body.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
                .trim()
                .to_string()
        })
        .filter(|line| {
            !matches!(
                line.as_str(),
                "No changes recorded." | "No verification recorded."
            )
        })
        .collect()
}

/// Reject structured final answers that are syntactically valid but visibly
/// incomplete. Provider-native prose remains valid; this only catches shapes
/// that would otherwise present an empty answer or an unresolved template.
pub(crate) fn final_response_validation_feedback(final_response: &FinalResponse) -> Option<String> {
    let markdown = final_response.final_markdown.trim();
    if markdown.is_empty() {
        return Some(
            "Final response rejected: `final_markdown` is empty. Return the actual result or an explicit blocker."
                .to_string(),
        );
    }
    if markdown.contains("REPLACE_ME") && markdown.len() < 200 {
        return Some(
            "Final response rejected: it still contains `REPLACE_ME` placeholder text. Use tools to gather the missing value, then return a real final answer."
                .to_string(),
        );
    }
    None
}

pub(crate) fn validation_fallback_response(
    agent_name: &str,
    final_response: &FinalResponse,
    feedback: &str,
    specialist_outcome: Option<&AgentOutcome>,
) -> FinalResponse {
    if let Some(outcome) = specialist_outcome.filter(|outcome| !outcome.summary.trim().is_empty()) {
        let changes_made = extract_artifact_bullets(outcome, "Changes made");
        let mut verification = extract_artifact_bullets(outcome, "Verification");
        verification.push(format!(
            "Phoenix preserved the delegated specialist result after repeated orchestrator validation failures: {}",
            first_line(feedback)
        ));
        return FinalResponse {
            summary: first_line(&outcome.summary).to_string(),
            final_markdown: outcome.summary.clone(),
            changes_made,
            verification,
            execution_mode: "provider_validation_preserved_specialist_result".to_string(),
            tool_transcript: vec![],
        };
    }

    FinalResponse {
        summary: format!("{agent_name} returned a response that failed runtime validation."),
        final_markdown: format!(
            "## Result\n{agent_name} returned structured output, but Phoenix rejected it during runtime validation.\n\nReason: {}\n\nLast reported summary: {}",
            first_line(feedback),
            first_line(&final_response.summary)
        ),
        changes_made: final_response.changes_made.clone(),
        verification: vec![format!(
            "Runtime validation fallback: {}",
            first_line(feedback)
        )],
        execution_mode: "provider_validation_fallback".to_string(),
        tool_transcript: vec![],
    }
}

/// Build a `FinalResponse` from the arguments of a `final_answer` tool call.
/// `execution_mode` and `tool_transcript` are runtime-owned, so the schema only
/// exposes the model-authored fields; `summary` falls back to the first line of
/// the answer when the model omits it.
pub(crate) fn final_response_from_tool_input(input: &serde_json::Value) -> FinalResponse {
    let str_field = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let arr_field = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let final_markdown = str_field("final_markdown");
    let mut summary = str_field("summary");
    if summary.is_empty() {
        summary = first_line(&final_markdown).to_string();
    }

    FinalResponse {
        summary,
        final_markdown,
        changes_made: arr_field("changes_made"),
        verification: arr_field("verification"),
        execution_mode: "provider_tools_final_answer".to_string(),
        tool_transcript: vec![],
    }
}

pub(crate) fn provider_error_fallback(agent_name: &str, error: &str) -> FinalResponse {
    FinalResponse {
        summary: format!(
            "{agent_name} provider call failed; Phoenix preserved the turn with a bounded fallback."
        ),
        final_markdown: format!(
            "## Result\n{agent_name} could not complete its provider-backed turn.\n\nPhoenix kept the session alive and returned a bounded fallback instead of failing the whole run.\n\nError: {}",
            sanitize_error(error)
        ),
        changes_made: vec![],
        verification: vec![format!(
            "Phoenix recorded a {agent_name} provider call failure: {}",
            sanitize_error(error)
        )],
        execution_mode: "provider_error_fallback".to_string(),
        tool_transcript: vec![],
    }
}

pub(crate) fn assistant_session_content(response: &crate::providers::CompletionResponse) -> String {
    let mut parts = Vec::new();

    let (dethought, _) = crate::runtime::turns::split_reasoning_tags(&response.content);
    let visible_content = strip_visible_reasoning_blocks(&dethought);
    if !visible_content.trim().is_empty() {
        parts.push(visible_content);
    }

    // DO NOT synthesize the tool call into the stored content.
    //
    // This used to append `Native tool request: read({...})`. Within a turn the
    // call already travels structurally (ChatMessage::assistant_tool_calls), so
    // the text was only ever for the transcript — but the session is replayed
    // into the next turn's prompt as plain text (prompt.rs: `Assistant\n{content}`),
    // and after a compaction fold that prose becomes the model's own history.
    // The model then reads a conversation in which every assistant turn writes
    // "Native tool request: ..." and correctly predicts one more. One session
    // accumulated 68 such messages and the replies degenerated into transcripts.
    //
    // The call is not lost: the following ToolResult message records tool_name
    // and input, which is what the transcript and UI read.
    let _ = &response.tool_calls;

    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    }
}

pub(crate) fn strip_visible_reasoning_blocks(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    loop {
        let Some(start) = rest.find("[Reasoning]") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after_start = &rest[start + "[Reasoning]".len()..];
        let Some(end) = after_start.find("[/Reasoning]") else {
            break;
        };
        rest = &after_start[end + "[/Reasoning]".len()..];
    }
    out.trim().to_string()
}

pub(crate) fn requested_tool_call_from_native(call: &NativeToolCall) -> RequestedToolCall {
    RequestedToolCall {
        tool_name: call.tool_name.clone(),
        input: call.arguments.clone(),
    }
}

pub(crate) struct ProviderLoopResult {
    pub(crate) final_response: FinalResponse,
    pub(crate) tool_results: Vec<ToolCallResult>,
    pub(crate) provider_turn: Option<ProviderTurn>,
    pub(crate) parse_fallback_used: bool,
    pub(crate) parse_detail: String,
}

pub(crate) fn agent_outcome_from_final(
    agent: AgentTarget,
    reply_title: &str,
    final_response: FinalResponse,
    provider_turn: Option<ProviderTurn>,
) -> AgentOutcome {
    let transcript = if final_response.tool_transcript.is_empty() {
        format!(
            "Execution mode: {}\n- No tool calls were recorded by the model.",
            final_response.execution_mode
        )
    } else {
        let entries = final_response
            .tool_transcript
            .iter()
            .map(|entry| {
                format!(
                    "- {} | input: {} | outcome: {}",
                    entry.name, entry.input_summary, entry.outcome
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Execution mode: {}\n{}",
            final_response.execution_mode, entries
        )
    };

    let user_markdown = crate::runtime::turns::user_facing_markdown(&final_response.final_markdown);

    AgentOutcome {
        completion: crate::runtime::OutcomeCompletion::from_execution_mode(&final_response.execution_mode),
        agent,
        summary: user_markdown.clone(),
        artifacts: vec![
            AgentArtifact {
                kind: ArtifactKind::Reply,
                title: reply_title.to_string(),
                body: user_markdown,
            },
            AgentArtifact {
                kind: ArtifactKind::Plan,
                title: "Internal summary".to_string(),
                body: final_response.summary.clone(),
            },
            AgentArtifact {
                kind: ArtifactKind::FilePatch,
                title: "Changes made".to_string(),
                body: if final_response.changes_made.is_empty() {
                    "No changes recorded.".to_string()
                } else {
                    final_response
                        .changes_made
                        .iter()
                        .map(|item| format!("- {item}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            },
            AgentArtifact {
                kind: ArtifactKind::Finding,
                title: "Verification".to_string(),
                body: if final_response.verification.is_empty() {
                    "No verification recorded.".to_string()
                } else {
                    final_response.verification.join("\n")
                },
            },
            AgentArtifact {
                kind: ArtifactKind::ToolTranscript,
                title: "Execution transcript".to_string(),
                body: transcript,
            },
        ],
        tool_results: vec![],
        provider_response: provider_turn,
    }
}
