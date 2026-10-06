//! The legacy provider agent loop and its tool-call plumbing (validation
//! feedback, allowlist enforcement, provider tool execution).

use super::*;

const PROVIDER_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(45);
const MAX_TOOL_CALLS_PER_RESPONSE: usize = 16;

/// Legacy/direct turns follow the same policy as mesh turns: no hidden
/// wall-clock deadline for the whole task. Provider calls and tools keep their
/// independent operation budgets, and user cancellation remains authoritative.
fn whole_turn_deadline() -> Option<tokio::time::Instant> {
    None
}

fn admit_tool_calls(seen: &mut usize, requested: usize) -> std::result::Result<(), String> {
    if requested > MAX_TOOL_CALLS_PER_RESPONSE {
        return Err(format!(
            "provider requested {requested} tools in one response (limit {MAX_TOOL_CALLS_PER_RESPONSE})"
        ));
    }
    let next = seen
        .checked_add(requested)
        .ok_or_else(|| "tool-call counter overflowed while enforcing the turn limit".to_string())?;
    *seen = next;
    Ok(())
}

fn provider_tool_timeout(tool_name: &str) -> std::time::Duration {
    use std::time::Duration;
    match tool_name {
        "ask_user" | "teach_workflow" => Duration::from_secs(3600),
        "bash" => Duration::from_secs(900),
        "glob" | "grep" | "list_directory" | "read" => Duration::from_secs(20),
        name if name.starts_with("browser_") => Duration::from_secs(120),
        name if name.starts_with("computer_") => Duration::from_secs(180),
        _ => Duration::from_secs(300),
    }
}

fn legacy_activity_digest(results: &[ToolCallResult]) -> String {
    results
        .iter()
        .rev()
        .take(20)
        .map(|result| {
            let output: String = result.output.chars().take(240).collect();
            format!(
                "{} {}: {}",
                if result.success { "ok" } else { "failed" },
                result.tool_name,
                output
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn legacy_boundary_response(
    agent_name: &str,
    reason: &str,
    results: &[ToolCallResult],
) -> FinalResponse {
    let evidence = legacy_activity_digest(results);
    let evidence_block = if evidence.trim().is_empty() {
        "No completed tool result was available before the boundary.".to_string()
    } else {
        format!("Completed tool evidence (newest bounded receipt):\n{evidence}")
    };
    FinalResponse {
        summary: format!("{agent_name} stopped at a runtime safety boundary."),
        final_markdown: format!(
            "Phoenix stopped this agent at a hard runtime boundary: {reason}. Unfinished actions are not reported as completed.\n\n{evidence_block}"
        ),
        changes_made: vec![],
        verification: vec![
            "Runtime-generated bounded receipt; any action without a completed tool result remains unconfirmed."
                .to_string(),
        ],
        execution_mode: "legacy_runtime_boundary_with_preserved_evidence".to_string(),
        tool_transcript: vec![],
    }
}

/// Keep slow-provider progress observable. Normal production turns have no
/// total wall-clock cap; an explicit deadline is still accepted for bounded
/// helper/test paths and future user-authored deadline policies.
async fn await_provider_with_heartbeats<F, T, H>(
    future: F,
    heartbeat_interval: std::time::Duration,
    deadline: Option<tokio::time::Instant>,
    mut heartbeat: H,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
    H: FnMut(),
{
    tokio::pin!(future);
    loop {
        let wait = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            heartbeat_interval.min(remaining)
        } else {
            heartbeat_interval
        };
        match tokio::time::timeout(wait, future.as_mut()).await {
            Ok(result) => return Some(result),
            Err(_) if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) => {
                return None
            }
            Err(_) => heartbeat(),
        }
    }
}

async fn await_provider_until<F, T, H>(
    future: F,
    heartbeat_interval: std::time::Duration,
    deadline: tokio::time::Instant,
    heartbeat: H,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
    H: FnMut(),
{
    await_provider_with_heartbeats(future, heartbeat_interval, Some(deadline), heartbeat).await
}

fn provider_call_deadline(
    turn_deadline: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
    turn_deadline
}

fn reset_final_rejection_streak_for_tool_work(
    tool_calls: &[RequestedToolCall],
    consecutive_final_rejections: &mut u32,
    last_final_rejection: &mut String,
) {
    if !tool_calls.is_empty()
        && !tool_calls
            .iter()
            .any(|call| call.tool_name == "final_answer")
    {
        *consecutive_final_rejections = 0;
        last_final_rejection.clear();
    }
}

impl AgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_provider_agent_loop(
        &self,
        spec: &crate::runtime::AgentSpec,
        agent_target: AgentTarget,
        root_main_session_id: &str,
        task: &TaskEnvelope,
        session: &mut crate::session::Session,
        loaded: &crate::librarian::LoadedMemories,
        session_store: &mut SessionStore,
        cache_root: &std::path::Path,
        librarian_passes: &mut Vec<LibrarianPassRecord>,
        decision: &mut Option<OrchestratorDecision>,
        specialist_session_status: &mut PersistenceStatus,
        specialist_session_path: &mut PathBuf,
        specialist_cache_status: &mut PersistenceStatus,
        specialist_cache_path: &mut PathBuf,
        specialist_bundle: &mut MemoryBundle,
        specialist_prompt: &mut PromptAssembly,
        specialist_outcome: &mut Option<AgentOutcome>,
        coder_parse: &mut ParseRecord,
        pending_specialists: &mut PendingSpecialists,
    ) -> Result<ProviderLoopResult> {
        const MAX_PARSE_REPAIR_ATTEMPTS: u32 = 3;
        let mut tool_results: Vec<ToolCallResult> = Vec::new();
        let mut last_provider_turn = None;
        let mut prompt_emitted = false;
        let mut native_tool_messages: Vec<ChatMessage> = Vec::new();
        let native_vision_turn = self.native_vision && self.provider.supports_native_images();
        let mut native_images = crate::runtime::vision::NativeTurnImages::default();
        let mut parse_repair_attempts = 0u32;
        let mut consecutive_final_rejections = 0u32;
        let mut last_final_rejection = String::new();
        let mut oversized_tool_batch_rejections = 0u8;
        let provider_turn_deadline = whole_turn_deadline();
        let mut tool_calls_seen = 0usize;
        let mut economy_guard =
            crate::runtime::efficiency::guard_for_turn(&task.user_request, &self.state_root);
        let mut design_guard =
            crate::runtime::design_contract::DesignContractGuard::new(&task.user_request, session);
        let mut build_guard =
            crate::runtime::build_contract::BuildContractGuard::new(&task.user_request);

        let mut round_index = 0usize;
        loop {
            economy_guard.observe_results(tool_results.iter().map(|result| {
                (
                    result.tool_name.as_str(),
                    result.input_summary.as_str(),
                    result.success,
                    result.output.as_str(),
                )
            }));
            design_guard.observe_session(session);
            build_guard.observe_results(&tool_results);
            match economy_guard.before_provider_round() {
                crate::runtime::efficiency::EconomyAdmission::Allow => {}
                crate::runtime::efficiency::EconomyAdmission::Stop(reason) => {
                    return Ok(ProviderLoopResult {
                        final_response: legacy_boundary_response(
                            &spec.name,
                            &reason,
                            &tool_results,
                        ),
                        tool_results,
                        provider_turn: last_provider_turn,
                        parse_fallback_used: true,
                        parse_detail: format!(
                            "{} execution-economy boundary: {reason}",
                            spec.name.to_lowercase()
                        ),
                    });
                }
                crate::runtime::efficiency::EconomyAdmission::Feedback(_) => {
                    unreachable!("provider-round admission never emits tool feedback")
                }
            }
            if let Some(notice) = economy_guard.take_weekly_notice() {
                self.emit(CliEvent::GatewayNotice(notice.clone()));
                native_tool_messages.push(ChatMessage::system(format!(
                    "LOCAL WEEKLY EFFICIENCY NOTICE: {notice} Prefer an already-connected structured lane, batch same-state actions, and avoid optional re-checks."
                )));
            }
            if provider_turn_deadline
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                return Ok(ProviderLoopResult {
                    final_response: legacy_boundary_response(
                        &spec.name,
                        "the absolute whole-turn deadline expired",
                        &tool_results,
                    ),
                    tool_results,
                    provider_turn: last_provider_turn,
                    parse_fallback_used: true,
                    parse_detail: format!(
                        "{} reached the absolute whole-turn deadline; unfinished actions remain unconfirmed.",
                        spec.name.to_lowercase()
                    ),
                });
            }
            let current_round = round_index;
            round_index += 1;
            let prompt = assemble_prompt_with_context(
                spec,
                task,
                loaded,
                session,
                Some(&self.workspace_root),
                Some(self.provider.name()),
                None, // project brain injected on the mesh turn (turn_loop)
            );
            // Only emit PromptAssembled once per turn (first round), not every retry.
            // /prompt shows the initial prompt the user wants to inspect, not retry noise.
            if !prompt_emitted {
                let assembled_agent_name = spec.name.clone();
                self.emit(CliEvent::PromptAssembled {
                    agent: assembled_agent_name,
                    system_prompt: prompt.system_prompt.clone(),
                    user_prompt: prompt.user_prompt.clone(),
                });
                prompt_emitted = true;
            }
            let mut request =
                prompt.to_completion_request(&spec.default_model, Some(session.id.clone()));
            if let Some(tx) = self.event_tx.clone() {
                request.stream_observer = Some(crate::runtime::stream_observer(tx));
            }
            // Keep follow-up replies focused on the next action.
            if current_round > 0 {
                let repair_mode = tool_results
                    .iter()
                    .rev()
                    .any(|result| result.tool_name == "response_validation" && !result.success);
                let directive = if repair_mode {
                    "REPAIR MODE — ACTION ONLY. Your previous response failed runtime validation. Return only the corrected tool request or final; do not recap, reset, reassure, or make another plan."
                } else {
                    "FOLLOW-UP ROUND — continue the current goal from observed evidence. Revise an approach that is not meeting the requested outcome; the plan is not locked. Perform the next meaningful authorized action. Finish only with verified completion, an explicit stop or limit, or a concrete blocker after appropriate recovery. Avoid repeated recaps and promises. Do not infer an expired deadline from elapsed effort or an approximate target."
                };
                request.messages.push(ChatMessage::system(directive));
            }
            request.tools = tool_definitions_for_agent(&spec.tool_allowlist);
            let has_tools = !request.tools.is_empty() || !native_tool_messages.is_empty();
            prompt.append_tail(&mut request);
            crate::runtime::wire_history::commit_round(
                &mut request,
                &mut native_tool_messages,
                native_images.take_observations(),
            );
            request.temperature = Some(AGENT_TEMPERATURE);
            // Reasoning effort for providers that support it (Codex Responses API
            // takes `reasoning: {effort}`): this agent's own lane in
            // [profile.llm.efforts], then the global /reasoning setting.
            let lane_effort = match &agent_target {
                AgentTarget::Orchestrator => self.role_efforts.get("orchestrator"),
                AgentTarget::Specialist(agent) => {
                    let label = crate::runtime::delegation::specialist_label(*agent);
                    self.role_efforts
                        .get(label)
                        .or_else(|| self.role_efforts.get("specialist"))
                }
            }
            .cloned()
            .or_else(|| self.reasoning_effort.clone());
            if let Some(effort) = lane_effort {
                request.extra_body.insert(
                    "reasoning".to_string(),
                    serde_json::json!({ "effort": effort }),
                );
            }
            // Only request json_object when we are NOT using native tool calling —
            // json_object conflicts with native tools in every provider API.
            if !has_tools {
                request.extra_body.insert(
                    "response_format".to_string(),
                    serde_json::json!({ "type": "json_object" }),
                );
            }
            self.emit(CliEvent::Thinking);
            // Heartbeat while the provider hangs (mirrors the mesh turn loop):
            // silence during a minutes-long read wait reads as a dead session.
            let request_model = request.model.clone();
            let provider_call_started = std::time::Instant::now();
            let call_deadline = provider_call_deadline(provider_turn_deadline);
            let response = await_provider_with_heartbeats(
                self.provider.complete(request),
                PROVIDER_HEARTBEAT_INTERVAL,
                call_deadline,
                || {
                    self.emit(CliEvent::StreamDelta {
                        kind: "progress".to_string(),
                        text: format!(
                            "{} still waiting on {} — {}s (slow provider; generation remains active)",
                            spec.name,
                            request_model,
                            provider_call_started.elapsed().as_secs(),
                        ),
                    });
                },
            )
            .await
            .unwrap_or_else(|| {
                Err(anyhow::anyhow!(
                    "{} provider call stopped while waiting on {} after {}s because the explicit whole-turn deadline expired",
                    spec.name,
                    request_model,
                    provider_call_started.elapsed().as_secs(),
                ))
            });
            let mut response = match response {
                Ok(response) => response,
                Err(_error)
                    if provider_turn_deadline
                        .is_some_and(|deadline| tokio::time::Instant::now() >= deadline) =>
                {
                    return Ok(ProviderLoopResult {
                        final_response: legacy_boundary_response(
                            &spec.name,
                            "the absolute whole-turn deadline expired during a provider call",
                            &tool_results,
                        ),
                        tool_results,
                        provider_turn: last_provider_turn,
                        parse_fallback_used: true,
                        parse_detail: format!(
                            "{} reached the absolute whole-turn deadline during a provider call.",
                            spec.name.to_lowercase()
                        ),
                    });
                }
                Err(error) if !matches!(agent_target, AgentTarget::Orchestrator) => {
                    let detail = error.to_string();
                    self.record_response_validation_feedback(
                        &agent_target,
                        "provider call",
                        format!(
                            "{} provider call failed; Phoenix is returning a bounded specialist fallback instead of failing the whole turn. Error: {}",
                            spec.name,
                            sanitize_error(&detail)
                        ),
                        &mut tool_results,
                        session,
                    );
                    return Ok(ProviderLoopResult {
                        final_response: provider_error_fallback(&spec.name, &detail),
                        tool_results,
                        provider_turn: last_provider_turn,
                        parse_fallback_used: true,
                        parse_detail: format!(
                            "{} provider call failed: {}",
                            spec.name.to_lowercase(),
                            sanitize_error(&detail)
                        ),
                    });
                }
                Err(error) => return Err(error),
            };
            economy_guard.record_provider_usage(response.usage.input_tokens);
            // Some providers expose a bounded reasoning summary (or an inline
            // <think> summary) intended for the user. Phoenix treats that as
            // live work commentary; private chain-of-thought is never part of
            // CompletionResponse and must never be invented by the client.
            let (visible, inline_reasoning) =
                crate::runtime::turns::split_reasoning_tags(&response.content);
            if let Some(reasoning) = inline_reasoning {
                if response
                    .reasoning
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    response.reasoning = Some(reasoning);
                }
                response.content = visible;
            }
            if let Some(ref reasoning) = response.reasoning {
                if !reasoning.trim().is_empty() {
                    self.emit(CliEvent::AgentThinking {
                        agent: agent_display_name(&spec.name),
                        text: reasoning.clone(),
                    });
                }
            }
            // In native tool-calling the model writes its plan/rationale into the
            // message content ALONGSIDE the tool call (we instruct it to). That
            // content is visible work commentary, analogous to Codex's short
            // paragraphs between tool receipts. Skip it on a final (no tool
            // calls), where the content is the user-facing answer.
            if !response.tool_calls.is_empty() && !response.content.trim().is_empty() {
                let visible = strip_visible_reasoning_blocks(&response.content);
                if !visible.trim().is_empty() {
                    self.emit(CliEvent::AgentMessage {
                        agent: agent_display_name(&spec.name),
                        text: visible,
                    });
                }
            }
            let provider_turn = ProviderTurn::from(response.clone());
            let mut native_call_ids: Vec<Option<String>> = Vec::new();
            session.push_message(Message::Assistant {
                content: assistant_session_content(&response),
            });

            let turn = if response.tool_calls.is_empty() {
                match parse_agent_turn_response(&response.content) {
                    Ok(turn) => turn,
                    Err(error) => {
                        let contains_pending_tool_intent =
                            raw_contains_tool_intent(response.content.trim());
                        let allow_plain_text_wrap =
                            !matches!(agent_target, AgentTarget::Orchestrator)
                                || specialist_outcome
                                    .as_ref()
                                    .filter(|outcome| !outcome.summary.trim().is_empty())
                                    .is_none();

                        if allow_plain_text_wrap && !contains_pending_tool_intent {
                            if let Some(mut final_response) = coerce_plain_text_final_response(
                                &agent_target,
                                &response.content,
                                &tool_results,
                            ) {
                                if let Some(feedback) = design_guard.final_feedback() {
                                    self.record_response_validation_feedback(
                                        &agent_target,
                                        "Taste design gate",
                                        feedback.to_string(),
                                        &mut tool_results,
                                        session,
                                    );
                                    continue;
                                }
                                if let Some(feedback) = build_guard.final_feedback() {
                                    self.record_response_validation_feedback(
                                        &agent_target,
                                        "substantial build contract",
                                        feedback.to_string(),
                                        &mut tool_results,
                                        session,
                                    );
                                    continue;
                                }
                                if let Some(feedback) = build_guard.completion_audit_feedback()
                                    .or_else(|| build_guard.pending_work_feedback(&session.id)) {
                                    self.record_response_validation_feedback(&agent_target, "completion audit", feedback,
                                        &mut tool_results, session);
                                    continue;
                                }
                                build_guard.preserve_pending_work(&session.id, &mut final_response);
                                return Ok(ProviderLoopResult {
                                    final_response,
                                    tool_results,
                                    provider_turn: Some(provider_turn),
                                    parse_fallback_used: false,
                                    parse_detail: "Provider returned plain text after the native tool loop; Phoenix wrapped it into a runtime final response."
                                        .to_string(),
                                });
                            }
                        }

                        if matches!(agent_target, AgentTarget::Orchestrator) {
                            let raw_reply = response.content.trim();
                            if let Some(outcome) = specialist_outcome
                                .as_ref()
                                .filter(|outcome| !outcome.summary.trim().is_empty())
                            {
                                let changes_made =
                                    extract_artifact_bullets(outcome, "Changes made");
                                let mut verification =
                                    extract_artifact_bullets(outcome, "Verification");
                                verification.push(
                                    "Phoenix preserved the delegated specialist result because the orchestrator provider reply was not valid structured JSON."
                                        .to_string(),
                                );
                                return Ok(ProviderLoopResult {
                                    final_response: FinalResponse {
                                        summary: first_line(&outcome.summary).to_string(),
                                        final_markdown: outcome.summary.clone(),
                                        changes_made,
                                        verification,
                                        execution_mode:
                                            "provider_tools_readonly_orchestrator_fallback"
                                                .to_string(),
                                        tool_transcript: vec![],
                                    },
                                    tool_results,
                                    provider_turn: Some(provider_turn),
                                    parse_fallback_used: true,
                                    parse_detail: format!(
                                        "Provider output was not valid AgentTurnResponse JSON after delegation; preserved the delegated specialist result instead. Error: {}",
                                        sanitize_error(&error.to_string())
                                    ),
                                });
                            }

                            parse_repair_attempts += 1;
                            let feedback = if raw_reply.is_empty() && !tool_results.is_empty() {
                                format!(
                                    "Orchestrator returned empty content after {} recorded tool call(s). Use those results and call final_answer, or take the missing action with a tool.",
                                    tool_results.len()
                                )
                            } else {
                                format!(
                                    "Orchestrator returned an unusable structured response: {}. Correct the response by calling final_answer or the next required tool.",
                                    sanitize_error(&error.to_string())
                                )
                            };
                            self.record_response_validation_feedback(
                                &agent_target,
                                "orchestrator provider response",
                                feedback,
                                &mut tool_results,
                                session,
                            );
                            if parse_repair_attempts <= MAX_PARSE_REPAIR_ATTEMPTS {
                                continue;
                            }
                            return Ok(ProviderLoopResult {
                                final_response: invalid_provider_json_fallback(
                                    &spec.name,
                                    &response.content,
                                    &error.to_string(),
                                ),
                                tool_results,
                                provider_turn: Some(provider_turn),
                                parse_fallback_used: true,
                                parse_detail: format!(
                                    "Provider output was not valid AgentTurnResponse JSON without a delegated specialist result to preserve after {} repair attempt(s): {}",
                                    parse_repair_attempts.saturating_sub(1),
                                    sanitize_error(&error.to_string())
                                ),
                            });
                        }

                        parse_repair_attempts += 1;
                        // Empty content (no tool calls) on a reasoning model is the
                        // hidden-reasoning stall — nudge it explicitly to write the
                        // envelope into the message body instead of the generic
                        // "your JSON was malformed" feedback (there was no JSON).
                        let feedback = if response.content.trim().is_empty() {
                            format!(
                                "{} returned an empty message (no JSON, no tool call). Your reasoning went to the hidden channel but Phoenix needs the actual answer WRITTEN OUT: emit exactly one Phoenix JSON object now — a `tool_request` (with your full `rationale`) or a `final`. Put your thinking in the `rationale`/`final_markdown` text, not silent reasoning. Do not return empty content.",
                                spec.name
                            )
                        } else {
                            invalid_provider_json_feedback(
                                &spec.name,
                                &response.content,
                                &error.to_string(),
                            )
                        };
                        self.record_response_validation_feedback(
                            &agent_target,
                            "provider response",
                            feedback.clone(),
                            &mut tool_results,
                            session,
                        );

                        if parse_repair_attempts <= MAX_PARSE_REPAIR_ATTEMPTS {
                            continue;
                        }
                        return Ok(ProviderLoopResult {
                            final_response: invalid_provider_json_fallback(
                                &spec.name,
                                &response.content,
                                &error.to_string(),
                            ),
                            tool_results,
                            provider_turn: Some(provider_turn),
                            parse_fallback_used: true,
                            parse_detail: format!(
                                "{} provider output was not valid AgentTurnResponse JSON after {} repair attempt(s): {}",
                                spec.name.to_lowercase(),
                                parse_repair_attempts.saturating_sub(1),
                                sanitize_error(&error.to_string())
                            ),
                        });
                    }
                }
            } else {
                native_call_ids = response
                    .tool_calls
                    .iter()
                    .map(|call| Some(call.id.clone()))
                    .collect();
                native_tool_messages.push(ChatMessage::assistant_reply(&response));

                AgentTurnResponse::ToolRequest {
                    tool_calls: response
                        .tool_calls
                        .iter()
                        .map(requested_tool_call_from_native)
                        .collect(),
                    // No prose accompanied the native calls — leave the
                    // rationale empty so nothing renders as fake narration.
                    rationale: String::new(),
                }
            };
            // A successfully decoded envelope (including a native tool-call
            // turn) proves the provider recovered. Only consecutive malformed
            // or empty replies should consume the repair budget.
            parse_repair_attempts = 0;
            last_provider_turn = Some(provider_turn);

            match turn {
                AgentTurnResponse::Final(mut final_response) => {
                    if let Some(feedback) = design_guard.final_feedback() {
                        self.record_response_validation_feedback(
                            &agent_target,
                            "Taste design gate",
                            feedback.to_string(),
                            &mut tool_results,
                            session,
                        );
                        continue;
                    }
                    if let Some(feedback) = build_guard.final_feedback() {
                        self.record_response_validation_feedback(
                            &agent_target,
                            "substantial build contract",
                            feedback.to_string(),
                            &mut tool_results,
                            session,
                        );
                        continue;
                    }
                    if let Some(feedback) = build_guard.completion_audit_feedback()
                        .or_else(|| build_guard.pending_work_feedback(&session.id)) {
                        self.record_response_validation_feedback(&agent_target, "completion audit", feedback,
                            &mut tool_results, session);
                        continue;
                    }
                    if let Some(feedback) = final_response_validation_feedback(&final_response) {
                        let key = first_line(&feedback).to_string();
                        if key == last_final_rejection {
                            consecutive_final_rejections += 1;
                        } else {
                            consecutive_final_rejections = 1;
                            last_final_rejection = key;
                        }
                        if consecutive_final_rejections >= 3 {
                            return Ok(ProviderLoopResult {
                                final_response: validation_fallback_response(
                                    &spec.name,
                                    &final_response,
                                    &feedback,
                                    specialist_outcome.as_ref(),
                                ),
                                tool_results,
                                provider_turn: last_provider_turn,
                                parse_fallback_used: true,
                                parse_detail: format!(
                                    "Provider output parsed but failed runtime validation after {consecutive_final_rejections} consecutive rejection(s)."
                                ),
                            });
                        }
                        self.record_response_validation_feedback(
                            &agent_target,
                            "final response",
                            feedback,
                            &mut tool_results,
                            session,
                        );
                        continue;
                    }
                    build_guard.preserve_pending_work(&session.id, &mut final_response);
                    return Ok(ProviderLoopResult {
                        final_response,
                        tool_results,
                        provider_turn: last_provider_turn,
                        parse_fallback_used: false,
                        parse_detail: "Provider output parsed into AgentTurnResponse.".to_string(),
                    });
                }
                AgentTurnResponse::ToolRequest {
                    tool_calls,
                    rationale,
                } => {
                    emit_agent_thinking(self, &agent_display_name(&spec.name), &rationale);
                    match design_guard.admit_tool_batch(&tool_calls) {
                        crate::runtime::design_contract::DesignAdmission::Allow => {}
                        crate::runtime::design_contract::DesignAdmission::Feedback(feedback) => {
                            self.record_response_validation_feedback(
                                &agent_target,
                                "Taste design gate",
                                feedback.to_string(),
                                &mut tool_results,
                                session,
                            );
                            let pending_tool_names = tool_calls
                                .iter()
                                .map(|call| call.tool_name.clone())
                                .collect::<Vec<_>>();
                            push_native_errors_for_calls(
                                &mut native_tool_messages,
                                &native_call_ids,
                                &pending_tool_names,
                                feedback,
                            );
                            continue;
                        }
                    }
                    match build_guard.admit_tool_batch(&tool_calls) {
                        crate::runtime::build_contract::BuildAdmission::Allow => {}
                        crate::runtime::build_contract::BuildAdmission::Feedback(feedback) => {
                            self.record_response_validation_feedback(
                                &agent_target,
                                "substantial build contract",
                                feedback.to_string(),
                                &mut tool_results,
                                session,
                            );
                            let pending_tool_names = tool_calls
                                .iter()
                                .map(|call| call.tool_name.clone())
                                .collect::<Vec<_>>();
                            push_native_errors_for_calls(
                                &mut native_tool_messages,
                                &native_call_ids,
                                &pending_tool_names,
                                feedback,
                            );
                            continue;
                        }
                    }
                    if tool_calls.iter().any(|call| call.tool_name == "final_answer") {
                        if let Some(feedback) = build_guard.completion_audit_feedback()
                            .or_else(|| build_guard.pending_work_feedback(&session.id)) {
                            self.record_response_validation_feedback(&agent_target, "completion audit", feedback.clone(),
                                &mut tool_results, session);
                            let names = tool_calls.iter().map(|call| call.tool_name.clone()).collect::<Vec<_>>();
                            push_native_errors_for_calls(&mut native_tool_messages, &native_call_ids, &names, &feedback);
                            continue;
                        }
                    }
                    let economy_calls = tool_calls
                        .iter()
                        .map(|call| (call.tool_name.clone(), call.input.clone()))
                        .collect::<Vec<_>>();
                    match economy_guard.admit_tool_batch(&economy_calls) {
                        crate::runtime::efficiency::EconomyAdmission::Allow => {}
                        crate::runtime::efficiency::EconomyAdmission::Feedback(feedback) => {
                            self.record_response_validation_feedback(
                                &agent_target,
                                "execution economy",
                                feedback.clone(),
                                &mut tool_results,
                                session,
                            );
                            let pending_tool_names = tool_calls
                                .iter()
                                .map(|call| call.tool_name.clone())
                                .collect::<Vec<_>>();
                            push_native_errors_for_calls(
                                &mut native_tool_messages,
                                &native_call_ids,
                                &pending_tool_names,
                                &feedback,
                            );
                            continue;
                        }
                        crate::runtime::efficiency::EconomyAdmission::Stop(reason) => {
                            return Ok(ProviderLoopResult {
                                final_response: legacy_boundary_response(
                                    &spec.name,
                                    &reason,
                                    &tool_results,
                                ),
                                tool_results,
                                provider_turn: last_provider_turn,
                                parse_fallback_used: true,
                                parse_detail: format!(
                                    "{} execution-economy boundary: {reason}",
                                    spec.name.to_lowercase()
                                ),
                            });
                        }
                    }
                    if let Err(reason) = admit_tool_calls(&mut tool_calls_seen, tool_calls.len()) {
                        oversized_tool_batch_rejections =
                            oversized_tool_batch_rejections.saturating_add(1);
                        let feedback = format!(
                            "TOOL BATCH TOO LARGE: {reason}. This batch was not executed. Preserve the findings already in context, split any remaining work into at most {MAX_TOOL_CALLS_PER_RESPONSE} calls in one response, and then return a normal final answer."
                        );
                        if oversized_tool_batch_rejections >= 2 {
                            return Ok(ProviderLoopResult {
                                final_response: legacy_boundary_response(
                                    &spec.name,
                                    &format!(
                                        "the provider repeated an oversized tool batch after explicit rebatching feedback ({reason})"
                                    ),
                                    &tool_results,
                                ),
                                tool_results,
                                provider_turn: last_provider_turn,
                                parse_fallback_used: true,
                                parse_detail: format!(
                                    "{} repeated an oversized tool batch after feedback.",
                                    spec.name.to_lowercase()
                                ),
                            });
                        }
                        self.record_response_validation_feedback(
                            &agent_target,
                            "tool batch",
                            feedback.clone(),
                            &mut tool_results,
                            session,
                        );
                        let pending_tool_names = tool_calls
                            .iter()
                            .map(|call| call.tool_name.clone())
                            .collect::<Vec<_>>();
                        push_native_errors_for_calls(
                            &mut native_tool_messages,
                            &native_call_ids,
                            &pending_tool_names,
                            &feedback,
                        );
                        continue;
                    }
                    let pending_tool_names: Vec<String> =
                        tool_calls.iter().map(|c| c.tool_name.clone()).collect();

                    if tool_calls.is_empty() {
                        let feedback =
                            format!("{} requested tools but provided no tool calls", spec.name);
                        self.record_response_validation_feedback(
                            &agent_target,
                            "tool request",
                            feedback.clone(),
                            &mut tool_results,
                            session,
                        );
                        push_native_errors_for_calls(
                            &mut native_tool_messages,
                            &native_call_ids,
                            &pending_tool_names,
                            &feedback,
                        );
                        continue;
                    }
                    let tool_calls = tool_calls
                        .into_iter()
                        .map(|mut call| {
                            call.input = normalize_tool_input(&call.tool_name, call.input);
                            call
                        })
                        .collect::<Vec<_>>();

                    // No post-specialist guard (intentional): the orchestrator may
                    // keep using tools after a specialist returns — widen with web,
                    // then synthesize. `final_answer` is the normal terminator; the
                    // round and wall-clock caps above remain hard safety bounds.

                    // Structured finish: the model called `final_answer`. Route its
                    // provider-validated arguments straight to a FinalResponse instead of
                    // parsing a free-text JSON envelope — the native tool-call channel
                    // guarantees the shape, removing the brittle parse + repair path.
                    if let Some(final_call) =
                        tool_calls.iter().find(|c| c.tool_name == "final_answer")
                    {
                        if native_vision_turn && tool_calls.iter().any(|call| crate::runtime::vision::native_image_tool(&call.tool_name)) {
                            let feedback = "Inspect the requested image in a separate round before finishing. This batch was generated before those pixels were delivered; none of its calls executed.";
                            self.record_response_validation_feedback(&agent_target, "image inspection before final", feedback.into(), &mut tool_results, session);
                            push_native_errors_for_calls(&mut native_tool_messages, &native_call_ids, &pending_tool_names, feedback);
                            continue;
                        }
                        let mut final_response = final_response_from_tool_input(&final_call.input);
                        if let Some(feedback) = final_response_validation_feedback(&final_response)
                        {
                            let key = first_line(&feedback).to_string();
                            if key == last_final_rejection {
                                consecutive_final_rejections += 1;
                            } else {
                                consecutive_final_rejections = 1;
                                last_final_rejection = key;
                            }
                            if consecutive_final_rejections >= 3 {
                                return Ok(ProviderLoopResult {
                                    final_response: validation_fallback_response(
                                        &spec.name,
                                        &final_response,
                                        &feedback,
                                        specialist_outcome.as_ref(),
                                    ),
                                    tool_results,
                                    provider_turn: last_provider_turn,
                                    parse_fallback_used: true,
                                    parse_detail: format!(
                                        "final_answer tool call failed runtime validation after {consecutive_final_rejections} consecutive rejection(s)."
                                    ),
                                });
                            }
                            self.record_response_validation_feedback(
                                &agent_target,
                                "final_answer tool call",
                                feedback.clone(),
                                &mut tool_results,
                                session,
                            );
                            push_native_errors_for_calls(
                                &mut native_tool_messages,
                                &native_call_ids,
                                &pending_tool_names,
                                &feedback,
                            );
                            continue;
                        }
                        build_guard.preserve_pending_work(&session.id, &mut final_response);
                        return Ok(ProviderLoopResult {
                            final_response,
                            tool_results,
                            provider_turn: last_provider_turn,
                            parse_fallback_used: false,
                            parse_detail:
                                "Provider finished via the final_answer tool call (structured output)."
                                    .to_string(),
                        });
                    }

                    // A real intervening tool step breaks a streak of rejected
                    // finals. Do this only after the final_answer branch: an
                    // invalid final_answer is another final attempt, not a
                    // recovery action that should reset its own counter.
                    reset_final_rejection_streak_for_tool_work(
                        &tool_calls,
                        &mut consecutive_final_rejections,
                        &mut last_final_rejection,
                    );

                    let mut native_image_prepared_in_batch = false;
                    for (call_index, call) in tool_calls.into_iter().enumerate() {
                        let tool_input_json =
                            serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".to_string());
                        if let Err(error) = self.ensure_tool_allowed(spec, &call) {
                            let feedback = error.to_string();
                            self.record_response_validation_feedback(
                                &agent_target,
                                &summarize_tool_input(&call.tool_name, &call.input),
                                feedback.clone(),
                                &mut tool_results,
                                session,
                            );
                            if let Some(Some(call_id)) = native_call_ids.get(call_index) {
                                native_tool_messages.push(ChatMessage::tool_result(
                                    call_id.clone(),
                                    native_tool_error_content(&call.tool_name, &feedback),
                                ));
                            }
                            continue;
                        }
                        if native_image_prepared_in_batch && call.tool_name == "image_analyze" {
                            native_images.snapshot_current();
                            native_image_prepared_in_batch = false;
                        }
                        if native_image_prepared_in_batch && crate::runtime::vision::invalidates_native_observation(&call.tool_name) {
                            let feedback = "Inspect the image already prepared in this batch before changing the screen or requesting another image. This call was not executed; the first image remains attached.";
                            self.record_response_validation_feedback(&agent_target, &call.tool_name, feedback.into(), &mut tool_results, session);
                            if let Some(Some(call_id)) = native_call_ids.get(call_index) {
                                native_tool_messages.push(ChatMessage::tool_result(call_id.clone(), native_tool_error_content(&call.tool_name, feedback)));
                            }
                            continue;
                        }
                        if crate::runtime::vision::invalidates_native_observation(&call.tool_name) {
                            native_images.invalidate_current();
                        }
                        let outcome = self
                            .execute_provider_tool_call(
                                spec,
                                &agent_target,
                                root_main_session_id,
                                task,
                                session,
                                call.clone(),
                                session_store,
                                cache_root,
                                librarian_passes,
                                decision,
                                specialist_session_status,
                                specialist_session_path,
                                specialist_cache_status,
                                specialist_cache_path,
                                specialist_bundle,
                                specialist_prompt,
                                specialist_outcome,
                                coder_parse,
                                pending_specialists,
                                provider_turn_deadline,
                                native_vision_turn,
                                &mut native_images,
                            )
                            .await;
                        let outcome = match outcome {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                let feedback = format!(
                                    "Tool `{}` failed before it could complete: {}",
                                    call.tool_name,
                                    sanitize_error(&error.to_string())
                                );
                                self.record_response_validation_feedback(
                                    &agent_target,
                                    &summarize_tool_input(&call.tool_name, &call.input),
                                    feedback.clone(),
                                    &mut tool_results,
                                    session,
                                );
                                if let Some(Some(call_id)) = native_call_ids.get(call_index) {
                                    native_tool_messages.push(ChatMessage::tool_result(
                                        call_id.clone(),
                                        native_tool_error_content(&call.tool_name, &feedback),
                                    ));
                                }
                                continue;
                            }
                        };
                        let outcome_unconfirmed = outcome.is_unconfirmed();
                        let result = outcome.into_result();
                        native_image_prepared_in_batch |= native_vision_turn && result.success
                            && crate::runtime::vision::native_image_tool(&call.tool_name) && native_images.has_current();
                        build_guard.observe_tool_outcome(&call.tool_name, &call.input, tool_results.len(), &result);
                        if let Some(Some(call_id)) = native_call_ids.get(call_index) {
                            native_tool_messages.push(ChatMessage::tool_result(
                                call_id.clone(),
                                native_tool_result_content(&result),
                            ));
                        }
                        tool_results.push(result.clone());
                        session.push_message(Message::ToolResult {
                            tool_name: result.tool_name.clone(),
                            input: tool_input_json,
                            success: result.success,
                            output: result.output.clone(),
                        });
                        if outcome_unconfirmed {
                            return Ok(ProviderLoopResult {
                                final_response: legacy_boundary_response(
                                    &spec.name,
                                    &format!(
                                        "tool `{}` returned without confirmed external termination; Phoenix stopped before another action could overlap it",
                                        call.tool_name
                                    ),
                                    &tool_results,
                                ),
                                tool_results,
                                provider_turn: last_provider_turn,
                                parse_fallback_used: true,
                                parse_detail: format!(
                                    "{} stopped after unconfirmed termination of tool `{}`.",
                                    spec.name.to_lowercase(),
                                    call.tool_name
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    pub(super) fn record_response_validation_feedback(
        &self,
        _agent_target: &AgentTarget,
        input_summary: &str,
        feedback: String,
        tool_results: &mut Vec<ToolCallResult>,
        session: &mut crate::session::Session,
    ) {
        let result = ToolCallResult {
            tool_name: "response_validation".to_string(),
            input_summary: input_summary.to_string(),
            success: false,
            output: format!(
                "{feedback}\n\nREPAIR MODE: do not re-run the full gate or re-explain the plan. Emit only the corrected tool_request/final next, with a one-sentence rationale."
            ),
        };
        // Internal runtime guidance for the model only — do not surface as a CLI "tool" row.
        tool_results.push(result.clone());
        session.push_message(Message::ToolResult {
            tool_name: result.tool_name,
            input: "{}".to_string(),
            success: result.success,
            output: result.output,
        });
    }

    pub(super) fn ensure_tool_allowed(
        &self,
        spec: &crate::runtime::AgentSpec,
        call: &RequestedToolCall,
    ) -> Result<()> {
        if spec
            .tool_allowlist
            .iter()
            .any(|name| name == &call.tool_name)
        {
            Ok(())
        } else {
            bail!(
                "{} is not allowed to use tool `{}`",
                spec.name,
                call.tool_name
            )
        }
    }

    pub(super) async fn execute_provider_tool_call(
        &self,
        spec: &crate::runtime::AgentSpec,
        agent_target: &AgentTarget,
        root_main_session_id: &str,
        task: &TaskEnvelope,
        _session: &mut crate::session::Session,
        call: RequestedToolCall,
        session_store: &mut SessionStore,
        cache_root: &std::path::Path,
        librarian_passes: &mut Vec<LibrarianPassRecord>,
        decision: &mut Option<OrchestratorDecision>,
        specialist_session_status: &mut PersistenceStatus,
        specialist_session_path: &mut PathBuf,
        specialist_cache_status: &mut PersistenceStatus,
        specialist_cache_path: &mut PathBuf,
        specialist_bundle: &mut MemoryBundle,
        specialist_prompt: &mut PromptAssembly,
        specialist_outcome: &mut Option<AgentOutcome>,
        coder_parse: &mut ParseRecord,
        pending_specialists: &mut PendingSpecialists,
        turn_deadline: Option<tokio::time::Instant>,
        native_vision_turn: bool,
        native_images: &mut crate::runtime::vision::NativeTurnImages,
    ) -> Result<BoundedToolOutcome> {
        let normalized_input = normalize_tool_input(&call.tool_name, call.input);
        let input_summary = summarize_tool_input(&call.tool_name, &normalized_input);
        // Emit tool call started for real-time CLI rendering. Use the persona
        // display name (Compass (planner), Leo (coder), Phoenix (orchestrator))
        // so the user always sees WHO issued the call — never a bare role string.
        let agent_label = agent_display_name_for_target(agent_target);
        self.emit(CliEvent::ToolCallStarted {
            agent: agent_label.clone(),
            tool_name: call.tool_name.clone(),
            input_summary: input_summary.clone(),
        });
        match call.tool_name.as_str() {
            "talk" => {
                let talk: TalkInput =
                    serde_json::from_value(normalized_input).context("invalid talk input")?;

                let delivery = self
                    .deliver_talk(
                        &talk,
                        &spec.name,
                        root_main_session_id,
                        &spec.default_model,
                        task,
                        session_store,
                        cache_root,
                        librarian_passes,
                        specialist_session_status,
                        specialist_session_path,
                        specialist_cache_status,
                        specialist_cache_path,
                        specialist_bundle,
                        specialist_prompt,
                        specialist_outcome,
                        coder_parse,
                        pending_specialists,
                    )
                    .await?;

                if matches!(agent_target, AgentTarget::Orchestrator)
                    && talk.to.eq_ignore_ascii_case("coder")
                {
                    *decision = Some(OrchestratorDecision {
                        mode: DelegationMode::Handoff,
                        target: AgentTarget::Specialist(SubAgentType::Coder),
                        rationale: format!(
                            "Orchestrator routed work to coder with talk: {}",
                            talk.subject
                        ),
                    });
                }

                // Only emit the "talk delivered" banner when the talk did NOT run a
                // specialist inline. When a specialist executed (executed_target),
                // `deliver_talk` already streamed SpecialistDelegated → SpecialistOutput
                // → SpecialistCompleted around the run; emitting "Delivered to X" again
                // here lands AFTER the specialist's own output and reads as a confusing
                // second, late delegation. Skip it in that case.
                if !delivery.executed_target {
                    self.emit(CliEvent::ToolCallCompleted {
                        agent: agent_label.clone(),
                        tool_name: "talk".to_string(),
                        input_summary: input_summary.clone(),
                        success: true,
                        output_summary: format!("Delivered to {}", delivery.delivered_to),
                        diff: None,
                    });
                }

                let body_chars = talk.body.chars().count();
                let mode_note = if talk.mode == 2 && delivery.executed_target {
                    " Mode 2 ran this specialist in-process now (no gateway); the orchestrator paused until it returned, and its result handed back without a separate reply block."
                } else if talk.mode == 2 {
                    " Mode 2 delivery was queued/delivered only; no live specialist execution started."
                } else {
                    ""
                };
                let reply_block = if talk.reply_expected() {
                    specialist_outcome.as_ref().map(|outcome| {
                        let label = agent_display_name_for_target(&outcome.agent);
                        format!(
                            "\n\n=== {label} result (use this for your final answer) ===\n{}\n=== end {label} result ===\n\nNext step: synthesize the user-facing answer in final JSON. Do not call `talk` again unless you need a genuinely new specialist task.",
                            specialist_result_for_orchestrator(&outcome.summary)
                        )
                    })
                } else {
                    None
                };

                Ok(BoundedToolOutcome::Completed(ToolCallResult {
                    tool_name: "talk".to_string(),
                    input_summary,
                    success: true,
                    output: format!(
                        "Delivered talk to {} (mode={}, body: {body_chars} chars, executed={}). Reply status: {}.{}{}",
                        delivery.delivered_to,
                        talk.mode,
                        delivery.executed_target,
                        delivery.reply_status.label(),
                        mode_note,
                        reply_block.unwrap_or_default()
                    ),
                }))
            }
            other => {
                // Check permissions for risky bash commands
                if other == "bash" {
                    if !self.check_tool_permission(&call.tool_name, &input_summary) {
                        return Ok(BoundedToolOutcome::Completed(ToolCallResult {
                            tool_name: call.tool_name.clone(),
                            input_summary,
                            success: false,
                            output: "Permission denied; the command was not executed.".to_string(),
                        }));
                    }
                }
                let native_file_input = (native_vision_turn && other == "image_analyze").then(|| normalized_input.clone());
                let executor = std::sync::Arc::new(self.build_tool_executor()?.with_native_image_analysis(native_vision_turn));
                let outcome = executor
                    .execute_bounded(
                        crate::runtime::ToolCall {
                            tool_name: other.to_string(),
                            input: normalized_input,
                        },
                        provider_tool_timeout(other),
                        turn_deadline,
                    )
                    .await;
                let unconfirmed = outcome.is_unconfirmed();
                let mut result = outcome.into_result();
                if native_vision_turn && result.success {
                    let attachment = async {
                        if let Some(input) = native_file_input {
                            crate::tools::image_analyze::attach_native(&self.workspace_root, serde_json::from_value(input)?, native_images).await?;
                            return Ok(true);
                        }
                        if crate::runtime::vision::native_image_tool(other) {
                            if let Some(path) = result.output.lines().find_map(|line| line.strip_prefix("Screenshot saved: ")) {
                                native_images.capture(other, std::path::Path::new(path.trim())).await?;
                                return Ok(true);
                            }
                        }
                        // A downloaded image is looked at in the same step:
                        // no separate image_analyze call just to see it.
                        if other == "browser_download" {
                            if let Some(path) = result.output.lines()
                                .filter(|line| line.contains("(image/"))
                                .find_map(|line| line.split_once("→ ").map(|(_, rest)| rest.split(" (magic").next().unwrap_or(rest).trim().to_string()))
                            {
                                native_images.capture(other, std::path::Path::new(&path)).await?;
                                return Ok(true);
                            }
                        }
                        Ok::<_, anyhow::Error>(false)
                    }.await;
                    match attachment {
                        Ok(true) => result.output.push_str("\nImage pixels attached to your next model request. Inspect the labeled image set; no sidecar analysis was performed."),
                        Ok(false) => {}
                        Err(error) => {
                            native_images.invalidate_current();
                            result.success = false;
                            result.output.push_str(&format!("\nNative image delivery failed: {error:#}. Visual inspection has not occurred. Prior explicit references, if any, are not current screen evidence."));
                        }
                    }
                }
                let output_summary = format_tool_result_summary(result.success, &result.output);
                self.emit(CliEvent::ToolCallCompleted {
                    agent: agent_label.clone(),
                    tool_name: result.tool_name.clone(),
                    input_summary: result.input_summary.clone(),
                    success: result.success,
                    output_summary: output_summary.clone(),
                    diff: None,
                });
                Ok(if unconfirmed { BoundedToolOutcome::Unconfirmed(result) } else { BoundedToolOutcome::Completed(result) })
            }
        }
    }
}

#[cfg(test)]
mod provider_deadline_tests {
    #[test]
    fn valid_tool_batches_do_not_hit_an_arbitrary_turn_cap() {
        let mut seen = 0usize;
        for _ in 0..10_000 {
            super::admit_tool_calls(&mut seen, 1).unwrap();
        }
        assert_eq!(seen, 10_000);
    }

    #[test]
    fn legacy_provider_turn_has_no_hidden_wall_clock_deadline() {
        assert!(
            super::whole_turn_deadline().is_none(),
            "legacy/direct turns must rely on per-operation bounds and cancellation rather than a hidden one-hour cap"
        );
        assert!(
            super::provider_call_deadline(None).is_none(),
            "legacy/direct provider calls must not add a fixed per-call wall-clock cap"
        );
    }

    #[test]
    fn oversized_tool_response_does_not_consume_turn_budget() {
        let mut seen = 4usize;
        assert!(
            super::admit_tool_calls(&mut seen, super::MAX_TOOL_CALLS_PER_RESPONSE + 1).is_err()
        );
        assert_eq!(seen, 4);
    }

    #[test]
    fn intervening_tool_work_resets_but_final_answer_does_not() {
        let mut count = 2;
        let mut last = "same invalid final".to_string();
        super::reset_final_rejection_streak_for_tool_work(
            &[crate::runtime::RequestedToolCall {
                tool_name: "read".to_string(),
                input: serde_json::json!({"path": "Cargo.toml"}),
            }],
            &mut count,
            &mut last,
        );
        assert_eq!(count, 0);
        assert!(last.is_empty());

        count = 2;
        last = "same invalid final".to_string();
        super::reset_final_rejection_streak_for_tool_work(
            &[crate::runtime::RequestedToolCall {
                tool_name: "final_answer".to_string(),
                input: serde_json::json!({}),
            }],
            &mut count,
            &mut last,
        );
        assert_eq!(count, 2, "an invalid final remains consecutive");
        assert_eq!(last, "same invalid final");
    }

    #[tokio::test]
    async fn heartbeat_timeouts_do_not_extend_the_absolute_deadline() {
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            super::await_provider_until(
                std::future::pending::<()>(),
                std::time::Duration::from_millis(5),
                started + std::time::Duration::from_millis(30),
                || {},
            ),
        )
        .await
        .expect("provider deadline was not enforced");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn slow_provider_completion_still_emits_heartbeats() {
        let mut heartbeats = 0usize;
        let result = super::await_provider_until(
            async {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                42
            },
            std::time::Duration::from_millis(5),
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            || heartbeats += 1,
        )
        .await;

        assert_eq!(result, Some(42));
        assert!(heartbeats > 0, "slow waits must remain visibly alive");
    }
}
