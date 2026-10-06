//! Cross-turn background specialists (mode-2 `talk`): detached jobs that
//! outlive the spawning turn and deposit results in the session postbox.

use super::*;
use sha2::{Digest, Sha256};

pub(super) const MAX_BACKGROUND_TURNS: usize = 64;
const BACKGROUND_JOB_BUDGET: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

fn is_failed_result(message: &AgentMessage) -> bool {
    message.is_failed_result()
}

fn handoff_fingerprint(message: &AgentMessage) -> String {
    format!(
        "{}\0{}\0{}\0{}",
        message.from.label().to_ascii_lowercase(),
        message.to.label().to_ascii_lowercase(),
        message
            .subject
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        message
            .body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// Preserve useful evidence from successful stages while marking an incomplete
/// chain as failed. A partial result is available evidence, not job completion.
fn aggregate_background_results(
    results: &[AgentMessage],
    progress: &[String],
    agent_label: &str,
) -> (bool, String, String) {
    let successful = results
        .iter()
        .filter(|message| !is_failed_result(message))
        .count();
    let failed = results.len().saturating_sub(successful);
    let ok = successful > 0 && failed == 0;
    let mut body = if results.is_empty() {
        "(the background job produced no result message)".to_string()
    } else {
        results
            .iter()
            .map(|message| message.body.clone())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    };
    if successful > 0 && failed > 0 {
        body = format!(
            "PARTIAL COMPLETION — preserved {successful} usable result(s); {failed} downstream stage(s) failed. Use the completed evidence below and report the failed scope honestly.\n\n{body}"
        );
    }
    if !progress.is_empty() {
        body.push_str("\n\nChain receipts (stages the team completed and passed on):\n");
        for line in progress {
            body.push_str(&format!("- {line}\n"));
        }
    }
    let summary = results
        .iter()
        .find(|message| !is_failed_result(message))
        .or_else(|| results.first())
        .map(|message| cap_chars(&message.subject, 160))
        .unwrap_or_else(|| format!("{agent_label} returned nothing"));
    (ok, summary, body)
}

fn bounded_background_result(
    results: &[AgentMessage],
    progress: &[String],
    agent_label: &str,
    subject: &str,
    completed_turns: usize,
    reason: &str,
) -> crate::runtime::postbox::CompletedJob {
    let (_, partial_summary, partial_body) =
        aggregate_background_results(results, progress, agent_label);
    let preserved = if results.is_empty() && progress.is_empty() {
        "No terminal specialist result was available before the bound.".to_string()
    } else {
        format!(
            "Preserved partial evidence (not a completed job):\n\n{partial_body}\n\nPartial-result label: {partial_summary}"
        )
    };
    crate::runtime::postbox::CompletedJob {
        kind: crate::runtime::postbox::ReturnKind::Specialist,
        delivery_id: String::new(),
        causation_id: None,
        agent: agent_label.to_string(),
        subject: subject.to_string(),
        ok: false,
        summary: format!("{agent_label} background chain stopped at runtime bound"),
        body: format!(
            "BACKGROUND JOB INCOMPLETE — Phoenix stopped this specialist chain because {reason}. {completed_turns} agent turn(s) completed. Pending handoffs were not executed and must not be reported as finished.\n\n{preserved}"
        ),
        finished: chrono::Utc::now(),
    }
}

impl MeshRunner {
    /// An owned runner for a detached background job, sharing this runner's
    /// providers/config but with its own delegator map, its own token counter,
    /// and an event channel that outlives the current turn. Only lifecycle and
    /// user-interaction events cross back into the owning conversation; the
    /// specialist's private thinking and tool rows stay in its own durable
    /// session instead of being rendered as Phoenix's work.
    pub(super) fn background_clone(&self, job_scope: Option<String>) -> MeshRunner {
        let session_id = self.main_session_id.clone();
        let (tx, mut rx) = mpsc::channel::<CliEvent>(256);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match &event {
                    // Tool activity is still written to gateway.log for
                    // diagnostics, but it must not be published into the
                    // parent's story. Desktop has no per-lane visibility
                    // boundary, so forwarding these rows made a detached
                    // coworker look like Phoenix and mixed both transcripts.
                    CliEvent::ToolCallStarted {
                        agent,
                        tool_name,
                        input_summary,
                    } => crate::runtime::gwlog(&format!(
                        "  [bg] {agent}: {tool_name}({input_summary}) started"
                    )),
                    CliEvent::ToolCallCompleted {
                        agent,
                        tool_name,
                        success,
                        ..
                    } => crate::runtime::gwlog(&format!(
                        "  [bg] {agent}: {tool_name} {}",
                        if *success { "ok" } else { "FAILED" }
                    )),
                    CliEvent::AgentThinking { .. } | CliEvent::StreamDelta { .. } => {},
                    CliEvent::GatewayNotice(text) => {
                        crate::runtime::gwlog(&format!("  [bg] {text}"));
                    }
                    // SpecialistCompleted must pass: it is what stops
                    // a chain member's chip from spinning forever after its
                    // turn ends mid-chain (the job-level return only fires when
                    // the WHOLE chain finishes, and only for the first agent).
                    // AskUser MUST pass: a background specialist's ask_user
                    // registers the ask and blocks awaiting the answer — if
                    // the event doesn't reach the TUI there is no popup, the
                    // user only sees the "ask_user…" working row, and the
                    // agent hangs to the turn timeout with no visible popup.
                    CliEvent::AgentHandoff { .. }
                    | CliEvent::SpecialistCompleted { .. }
                    | CliEvent::AskUser { .. }
                    | CliEvent::WatcherCard { .. }
                    // The settle for a parked steer. A steer's whole point is
                    // reaching an agent that is ALREADY WORKING — which in this
                    // runtime means a background job — so if this variant is not
                    // in the allow-list the card the user just saw appear never
                    // resolves, in precisely the common case.
                    | CliEvent::SteerDelivered { .. } => {
                        // Background lanes bypass the daemon's turn logger, so
                        // mirror lifecycle/user-interaction beats here too.
                        match &event {
                            CliEvent::SpecialistCompleted { agent, ok, .. } => {
                                crate::runtime::gwlog(&format!(
                                    "  [bg] {agent} {}",
                                    if *ok { "completed" } else { "failed" }
                                ))
                            }
                            CliEvent::AskUser {
                                id,
                                agent,
                                questions,
                                ..
                            } => crate::runtime::gwlog(&format!(
                                "  [bg] ask {id} posted by {agent}: {}",
                                questions
                                    .first()
                                    .map(|q| q.question.as_str())
                                    .unwrap_or("")
                            )),
                            CliEvent::WatcherCard { from, subject, .. } => {
                                crate::runtime::gwlog(&format!("  [bg] {from}: {subject}"))
                            }
                            CliEvent::SteerDelivered { to, subject } => {
                                crate::runtime::gwlog(&format!(
                                    "  [bg] {to} read injected message: {subject}"
                                ))
                            }
                            _ => {}
                        }
                        crate::runtime::postbox::forward(&session_id, event.clone());
                    }
                    _ => {}
                }
            }
        });
        MeshRunner {
            provider: Arc::clone(&self.provider),
            workspace_root: self.workspace_root.clone(),
            state_root: self.state_root.clone(),
            main_session_id: self.main_session_id.clone(),
            orchestrator_spec: self.orchestrator_spec.clone(),
            specialist_model: self.specialist_model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            agent_models: self.agent_models.clone(),
            role_efforts: self.role_efforts.clone(),
            role_context_windows: self.role_context_windows.clone(),
            permission_mode: self.permission_mode,
            interaction_mode: self.interaction_mode,
            event_tx: Some(tx),
            group_status_tx: self.group_status_tx.clone(),
            vision: self.vision.clone(),
            native_vision: self.native_vision,
            // Composer image grants belong to the original authored turn,
            // not a new detached task's implicit input.
            design_reference_paths: Vec::new(),
            delegators: std::sync::Mutex::new(HashMap::new()),
            loaded_memories: None,
            tokens: Arc::new((AtomicU32::new(0), AtomicU32::new(0))),
            peak_input: Arc::new(AtomicU32::new(0)),
            context_window: self.context_window,
            specialist_provider: self.specialist_provider.clone(),
            agent_providers: self.agent_providers.clone(),
            compaction_provider: self.compaction_provider.clone(),
            compaction_model: self.compaction_model.clone(),
            job_scope,
            starting_turn: None,
            group_context: self.group_context.clone(),
            group_authored_turn_id: std::sync::Mutex::new(self.group_authored_turn_id.lock().unwrap_or_else(|p|p.into_inner()).clone()),
            direct_agent_context: self.direct_agent_context.clone(),
            #[cfg(test)]
            group_direct_context_sessions: self.group_direct_context_sessions.clone(),
        }
    }

    /// Detach a specialist onto a background task (mode-2 talk from the
    /// orchestrator). The job registers in the postbox immediately; its result
    /// is deposited there when done and injected into the orchestrator's
    /// context by the round-top drain.
    pub(super) fn spawn_background(&self, mut first: AgentMessage) {
        let session_id = self.main_session_id.clone();
        let base_label = first.to.label();
        let subject = first.subject.clone();
        // Parallel instances: the first job for a specialist runs on its
        // canonical durable session (continuity); a second+ concurrent job gets
        // an instance label ("coder#2") and a job scope that isolates every
        // specialist session its chain touches — fresh single-writer files.
        let agent_label = crate::runtime::postbox::next_instance_label(&session_id, &base_label);
        let (agent_label, job_scope) = if agent_label == base_label {
            (base_label.clone(), None)
        } else {
            let scope = uuid::Uuid::new_v4().simple().to_string()[..6].to_string();
            (agent_label, Some(scope))
        };
        let mut runner = self.background_clone(job_scope);
        let worker_agent = agent_label.clone();
        let worker_subject = subject.clone();
        // The detached return-delivery id is minted before the spawn becomes
        // observable. It is therefore the one handoff id shared by live spawn,
        // replayed spawn, durable return, and late/out-of-order return.
        let handoff_id = if first.handoff_id.starts_with("return_") {
            first.handoff_id.clone()
        } else {
            format!("return_{}", uuid::Uuid::new_v4().simple())
        };
        let causation_id = (!first.message_id.is_empty()).then(|| first.message_id.clone());
        first.handoff_id = handoff_id.clone();
        first.causation_id = causation_id.clone();
        // A futures AbortHandle exists before spawn, letting the postbox
        // publish the start marker + visible job + cancellation authority in
        // one transaction. `/stop` can therefore win even in the tiny window
        // before Tokio first polls the worker.
        let (abort_handle, abort_registration) = futures_util::future::AbortHandle::new_pair();
        runner.starting_turn = Some(crate::runtime::postbox::register_background_job(
            &session_id,
            &base_label,
            &agent_label,
            &subject,
            &handoff_id,
            causation_id.as_deref(),
            abort_handle,
        ));
        let worker = tokio::spawn(futures_util::future::Abortable::new(
            async move {
                runner
                    .drive_background(first, &worker_agent, &worker_subject)
                    .await
            },
            abort_registration,
        ));
        // A separate monitor is the liveness authority. It reports normal
        // completion, converts panics into a failed return, and stays alive
        // when `/stop` aborts the worker. Before this, an aborted/panicked
        // detached task skipped every line after `drive_background().await`,
        // leaving postbox.running and Canvas's spinner alive forever.
        tokio::spawn(async move {
            let result = worker.await;
            if !crate::runtime::postbox::claim_background_completion(&session_id, &agent_label) {
                return; // targeted /stop already settled this job
            }
            let mut job = match result {
                Ok(Ok(job)) => job,
                Ok(Err(_)) => crate::runtime::postbox::CompletedJob {
                    kind: crate::runtime::postbox::ReturnKind::Specialist,
                    delivery_id: String::new(),
                    causation_id: causation_id.clone(),
                    agent: agent_label.clone(),
                    subject: subject.clone(),
                    ok: false,
                    summary: "specialist task was cancelled".to_string(),
                    body: "The specialist runtime was cancelled before returning.".to_string(),
                    finished: chrono::Utc::now(),
                },
                Err(error) => crate::runtime::postbox::CompletedJob {
                    kind: crate::runtime::postbox::ReturnKind::Specialist,
                    delivery_id: String::new(),
                    causation_id: causation_id.clone(),
                    agent: agent_label.clone(),
                    subject: subject.clone(),
                    ok: false,
                    summary: if error.is_cancelled() {
                        "specialist task was cancelled".to_string()
                    } else {
                        "specialist task died unexpectedly".to_string()
                    },
                    body: format!("The specialist runtime ended before returning: {error}"),
                    finished: chrono::Utc::now(),
                },
            };
            // `delivery_id` is also the lifecycle correlation key; filling it
            // here preserves the identity even when the worker returned a
            // synthetic cancellation/panic receipt.
            job.delivery_id = handoff_id;
            if job.causation_id.is_none() {
                job.causation_id = causation_id;
            }
            // Lib beat: only successful/ordinary completions contain durable
            // work worth indexing; a panic receipt does not.
            if job.ok {
                crate::runtime::memory_beat::on_completion(&job.agent, &job.subject, &job.body);
            }
            crate::runtime::postbox::job_finished(&session_id, job);
        });
    }

    /// Drive a background specialist to completion: run its turn, follow any
    /// specialist→specialist handoffs it makes, and capture everything it
    /// addresses to the orchestrator or the user as the result. The
    /// orchestrator is a SINK here, never woken — the live orchestrator gets
    /// the result through the postbox, not a nested mesh turn.
    pub(super) async fn drive_background(
        &self,
        first: AgentMessage,
        agent_label: &str,
        subject: &str,
    ) -> crate::runtime::postbox::CompletedJob {
        self.drive_background_with_limits(
            first,
            agent_label,
            subject,
            MAX_BACKGROUND_TURNS,
            BACKGROUND_JOB_BUDGET,
        )
        .await
    }

    pub(super) async fn drive_background_with_limits(
        &self,
        first: AgentMessage,
        agent_label: &str,
        subject: &str,
        max_turns: usize,
        budget: std::time::Duration,
    ) -> crate::runtime::postbox::CompletedJob {
        if first.is_self_talk() {
            return crate::runtime::postbox::CompletedJob {
                kind: crate::runtime::postbox::ReturnKind::Specialist,
                delivery_id: String::new(),
                causation_id: None,
                agent: agent_label.to_string(),
                subject: subject.to_string(),
                ok: false,
                summary: "self-addressed coworker handoff rejected".to_string(),
                body: format!(
                    "The runtime rejected a self-addressed handoff ({} -> {}) before any agent turn ran. A coworker must continue its own work locally or return a final answer; it cannot call itself.",
                    first.from.label(),
                    first.to.label()
                ),
                finished: chrono::Utc::now(),
            };
        }
        let mut seen_handoffs = std::collections::HashSet::from([handoff_fingerprint(&first)]);
        let mut queue: std::collections::VecDeque<AgentMessage> =
            std::collections::VecDeque::from([first]);
        let mut results: Vec<AgentMessage> = Vec::new();
        // Team model: mid-chain stage finals become one-line receipts — the
        // orchestrator receives the finished result, not every intermediate
        // step the team already handled between themselves.
        let mut progress: Vec<String> = Vec::new();
        let mut started_turns = 0usize;
        let mut completed_turns = 0usize;
        let deadline = tokio::time::Instant::now() + budget;
        while !queue.is_empty() {
            if started_turns >= max_turns {
                return bounded_background_result(
                    &results,
                    &progress,
                    agent_label,
                    subject,
                    completed_turns,
                    &format!("it reached the hard limit of {max_turns} agent turns"),
                );
            }
            if tokio::time::Instant::now() >= deadline {
                return bounded_background_result(
                    &results,
                    &progress,
                    agent_label,
                    subject,
                    completed_turns,
                    &format!(
                        "it reached the {}-second whole-job deadline",
                        budget.as_secs()
                    ),
                );
            }
            // WAVE: every queued baton addressed to a DIFFERENT specialist runs
            // concurrently (the work-web's fan-out — a planner delegating
            // researcher + browser in one batch gets true parallelism, same as
            // the gateway's wave loop). Batons to the same specialist stay
            // queued: its sessions are single-writer.
            let mut wave: Vec<AgentMessage> = Vec::new();
            let mut wave_labels: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut rest: std::collections::VecDeque<AgentMessage> =
                std::collections::VecDeque::new();
            let available_turns = max_turns.saturating_sub(started_turns);
            while let Some(message) = queue.pop_front() {
                if wave.len() >= available_turns || !wave_labels.insert(message.to.label()) {
                    rest.push_back(message);
                } else {
                    wave.push(message);
                }
            }
            queue = rest;
            started_turns += wave.len();
            // `run_turn` (the gateway handler) already isolates panics and
            // converts turn errors into failure messages. Concurrent turns on
            // one runner are the gateway wave contract — each writes only its
            // own specialist session. STREAMED, not join_all'd: each member's
            // completion chip, hop rows, and follow-on batons surface the
            // moment ITS turn ends — a researcher that finishes in 1 minute
            // must not look frozen behind a browser that runs for 10.
            use futures_util::stream::{FuturesUnordered, StreamExt};
            let mut turns: FuturesUnordered<_> = wave
                .into_iter()
                .map(|message| {
                    let addr = message.to.clone();
                    let parent_handoff = message.correlation_id().to_string();
                    async move {
                        let outs = self.run_turn(&addr, message).await;
                        (addr, parent_handoff, outs)
                    }
                })
                .collect();
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                let next = match tokio::time::timeout(remaining, turns.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        return bounded_background_result(
                            &results,
                            &progress,
                            agent_label,
                            subject,
                            completed_turns,
                            &format!(
                                "it reached the {}-second whole-job deadline while specialists were still working",
                                budget.as_secs()
                            ),
                        );
                    }
                };
                let Some((addr, parent_handoff, outs)) = next else {
                    break;
                };
                completed_turns += 1;
                let passed_baton = outs
                    .iter()
                    .any(|m| matches!(m.to, AgentAddress::Specialist(_)));
                let failed_turn = outs.iter().any(is_failed_result);
                // The chain member's turn is over — tell the TUI so its chip
                // stops spinning (run_turn only emits this on failure; the
                // whole-JOB return event fires much later, when the entire
                // chain ends, and only for the job's first agent).
                if !failed_turn {
                    self.emit(CliEvent::SpecialistCompleted {
                        agent: crate::runtime::delegation::agent_display_name(&addr.label()),
                        ok: true,
                        summary: if passed_baton {
                            "stage done, baton passed".to_string()
                        } else {
                            "done".to_string()
                        },
                    });
                }
                for mut out in outs {
                    if out.is_self_talk() {
                        results.push(AgentMessage::talk(
                            out.from.clone(),
                            AgentAddress::Orchestrator,
                            "self-addressed coworker handoff rejected",
                            format!(
                                "The runtime stopped {} from handing work to itself before the baton could be queued.",
                                out.from.label()
                            ),
                            false,
                        ));
                        continue;
                    }
                    match &out.to {
                        AgentAddress::Orchestrator | AgentAddress::User => {
                            let failed = is_failed_result(&out);
                            if passed_baton && !failed {
                                // Stage receipt: the same turn handed the baton on,
                                // so this final is progress, not the result.
                                progress.push(format!(
                                    "{}: {}",
                                    crate::runtime::delegation::agent_display_name(
                                        &out.from.label()
                                    ),
                                    cap_chars(out.subject.trim(), 160)
                                ));
                            } else {
                                results.push(out);
                            }
                        }
                        AgentAddress::Specialist(_) => {
                            let fingerprint = handoff_fingerprint(&out);
                            if !seen_handoffs.insert(fingerprint.clone()) {
                                results.push(AgentMessage::talk(
                                    out.from.clone(),
                                    AgentAddress::Orchestrator,
                                    "background handoff turn failed",
                                    format!(
                                        "Phoenix stopped an exact repeated background handoff from {} to {}. The earlier delivery remains authoritative; the duplicate was not queued and no context was deleted.",
                                        out.from.label(),
                                        out.to.label()
                                    ),
                                    false,
                                ));
                                continue;
                            }
                            let producer_operation_id = format!(
                                "background-baton:{}:{:x}",
                                if parent_handoff.is_empty() {
                                    self.main_session_id.as_str()
                                } else {
                                    parent_handoff.as_str()
                                },
                                Sha256::digest(fingerprint.as_bytes())
                            );
                            let should_route = match self.prepare_company_handoff(
                                &mut out,
                                (!parent_handoff.is_empty()).then_some(parent_handoff.as_str()),
                                Some(&producer_operation_id),
                            ) {
                                Ok(should_route) => should_route,
                                Err(error) => {
                                    results.push(AgentMessage::talk(
                                    out.from.clone(),
                                    AgentAddress::Orchestrator,
                                    "company handoff could not be persisted",
                                    format!(
                                        "Phoenix stopped the handoff from {} to {} before delivery because its durable identity could not be saved: {error:#}",
                                        out.from.label(),
                                        out.to.label()
                                    ),
                                    false,
                                ));
                                    continue;
                                }
                            };
                            if !should_route {
                                // Recovery already owns this accepted row, or
                                // the exact baton reached its receiver before
                                // the producer replayed. Never emit another
                                // lifecycle row or enqueue another turn.
                                continue;
                            }
                            // Direct handoff — surface the hop so the user watches
                            // the team pass work between themselves. Journaled too:
                            // the gate's "recursive" view of the run reaches through
                            // chain handoffs, not just orchestrator-level spawns.
                            if !out.is_correlated_return() {
                            crate::runtime::journal::record_handoff(
                                &self.main_session_id,
                                out.correlation_id(),
                                &out.from.label(),
                                &out.to.label(),
                                out.subject.trim(),
                                "queued",
                                out.reply_to.as_deref(),
                                out.causation_id.as_deref(),
                            );
                            self.emit(CliEvent::AgentHandoff {
                                handoff_id: out.correlation_id().to_string(),
                                from: crate::runtime::delegation::agent_display_name(
                                    &out.from.label(),
                                ),
                                to: crate::runtime::delegation::agent_display_name(&out.to.label()),
                                subject: cap_chars(out.subject.trim(), 120),
                                background: true,
                                requester: out.from.label(),
                                receiver: out.to.label(),
                                status: "queued".to_string(),
                                causation_id: out.causation_id.clone(),
                                body: None,
                                reply_to: None,
                            });
                            }
                            queue.push_back(out);
                        }
                    }
                }
            }
        }
        let (ok, summary, body) = aggregate_background_results(&results, &progress, agent_label);
        crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(),
            causation_id: None,
            agent: agent_label.to_string(),
            subject: subject.to_string(),
            ok,
            summary,
            body,
            finished: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SubAgentType;

    #[test]
    fn downstream_failure_does_not_poison_completed_evidence() {
        let good = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Browser),
            AgentAddress::Orchestrator,
            "winner scan complete",
            "No winner announcement was found in the checked surfaces.",
            false,
        );
        let failed = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Tester),
            AgentAddress::Orchestrator,
            "tester turn failed",
            "Provider DNS failed before the final handoff.",
            false,
        );

        let (ok, summary, body) = aggregate_background_results(&[good, failed], &[], "planner");
        assert!(!ok, "a partial result must not claim that the whole job completed");
        assert_eq!(summary, "winner scan complete");
        assert!(body.contains("PARTIAL COMPLETION"));
        assert!(body.contains("No winner announcement"));
        assert!(body.contains("Provider DNS failed"));
    }

    #[test]
    fn all_failed_results_still_fail_the_job() {
        let failed = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Tester),
            AgentAddress::Orchestrator,
            "tester turn failed",
            "No usable result.",
            false,
        );
        let (ok, _, body) = aggregate_background_results(&[failed], &[], "planner");
        assert!(!ok);
        assert!(!body.contains("PARTIAL COMPLETION"));
    }

    #[test]
    fn runtime_bound_receipt_preserves_evidence_without_claiming_completion() {
        let good = AgentMessage::talk(
            AgentAddress::Specialist(SubAgentType::Coder),
            AgentAddress::Orchestrator,
            "focused check complete",
            "The focused regression passed before the later handoff looped.",
            false,
        );
        let job = bounded_background_result(
            &[good],
            &[],
            "coder",
            "fix backend",
            4,
            "it reached the hard limit of 4 agent turns",
        );

        assert!(!job.ok, "a bounded incomplete chain is never complete");
        assert!(job.body.contains("focused regression passed"));
        assert!(job.body.contains("Preserved partial evidence"));
        assert!(job.body.contains("must not be reported as finished"));
    }
}
