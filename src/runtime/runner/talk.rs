//! Specialist delegation via `talk`: drive the target agent and hand the
//! result back to the delegator.

use super::*;

fn ensure_not_self_talk(from: &str, to: &str) -> Result<()> {
    anyhow::ensure!(
        !crate::runtime::mailbox::same_agent_identity(from, to),
        "coworker `{from}` cannot call itself through talk (`{to}`); continue the work locally or return a final answer"
    );
    Ok(())
}

impl AgentRunner {
    pub(super) async fn deliver_talk(
        &self,
        talk: &TalkInput,
        from_name: &str,
        main_session_id: &str,
        caller_model: &str,
        task: &TaskEnvelope,
        session_store: &mut SessionStore,
        cache_root: &std::path::Path,
        librarian_passes: &mut Vec<LibrarianPassRecord>,
        specialist_session_status: &mut PersistenceStatus,
        specialist_session_path: &mut PathBuf,
        specialist_cache_status: &mut PersistenceStatus,
        specialist_cache_path: &mut PathBuf,
        specialist_bundle: &mut MemoryBundle,
        specialist_prompt: &mut PromptAssembly,
        specialist_outcome: &mut Option<AgentOutcome>,
        coder_parse: &mut ParseRecord,
        _pending_specialists: &mut PendingSpecialists,
    ) -> Result<TalkResult> {
        if talk.mode != 1 && talk.mode != 2 {
            bail!("talk mode must be 1 or 2");
        }
        ensure_not_self_talk(from_name, &talk.to)?;

        if talk.to.eq_ignore_ascii_case("orchestrator") {
            let main_session = session_store
                .get_mut(main_session_id)
                .context("main session missing during talk delivery")?;
            main_session.push_message(Message::Talk {
                from: from_name.to_string(),
                to: "Orchestrator".to_string(),
                subject: talk.subject.clone(),
                body: talk.body.clone(),
                reply_expected: talk.reply_expected(),
                handoff_id: String::new(),
                reply_to: None,
                causation_id: None,
                status: "queued".to_string(),
            });
            self.emit(CliEvent::SpecialistQueued {
                agent: agent_display_name_for_target(&AgentTarget::Orchestrator),
                subject: talk.subject.clone(),
                status: if talk.reply_expected() {
                    "delivered to orchestrator inbox; reply expected on a later turn".to_string()
                } else {
                    "delivered to orchestrator inbox; no reply expected".to_string()
                },
            });
            return Ok(TalkResult {
                delivered_to: "orchestrator".to_string(),
                target_session_id: main_session_id.to_string(),
                reply_status: if talk.reply_expected() {
                    TalkReplyStatus::Pending
                } else {
                    TalkReplyStatus::NotExpected
                },
                executed_target: false,
            });
        }

        // Resolve which executable specialist this talk targets. Never return
        // a reassuring delivery receipt for an address that cannot run: that
        // loses work silently and lets the caller report a handoff that never
        // happened. The supported mesh already feeds this error back to the
        // model; the compatibility runner must preserve the same invariant.
        let Some(target) =
            specialist_from_talk_name(&talk.to).filter(|agent| specialist_is_executable(*agent))
        else {
            let valid = crate::runtime::delegation::valid_talk_target_names().join(", ");
            bail!(
                "unknown, inactive, or non-executable coworker `{}`; current talk targets: {valid}",
                talk.to
            );
        };
        let label = agent_display_name_for_target(&AgentTarget::Specialist(target));

        let target_session_id = crate::session::specialist_session_id(main_session_id, target);
        if talk.reply_expected() {
            if let Some(outcome) = specialist_outcome.as_ref().filter(|outcome| {
                matches!(outcome.agent, AgentTarget::Specialist(a) if a == target)
                    && !outcome.summary.trim().is_empty()
            }) {
                tracing::info!(
                    "Skipping duplicate talk to {label} in this orchestrator turn ({} chars already available).",
                    outcome.summary.chars().count()
                );
                return Ok(TalkResult {
                    delivered_to: label.clone(),
                    target_session_id,
                    reply_status: TalkReplyStatus::Expected,
                    executed_target: false,
                });
            }
        }

        let mut config = specialist_config(target);
        let specialist_model = self.specialist_model.as_deref().unwrap_or(caller_model);
        config.spec.default_model = specialist_model.to_string();
        *specialist_session_status = if session_store.get(&target_session_id).is_some() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        self.emit(CliEvent::SessionResolved {
            scope: label.clone(),
            session_id: target_session_id.clone(),
            status: format!("{:?}", *specialist_session_status),
        });
        let mut specialist_session = session_store.load_or_create_specialist(
            main_session_id,
            target,
            specialist_model,
            &config.spec.system_prompt,
        )?;
        *specialist_session_path = session_store.session_path(&target_session_id);
        specialist_session.push_message(Message::Talk {
            from: from_name.to_string(),
            to: target.to_string(),
            subject: talk.subject.clone(),
            body: talk.body.clone(),
            reply_expected: talk.reply_expected(),
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: "working".to_string(),
        });

        *specialist_cache_path = SessionCache::path_for(cache_root, &specialist_session.id);
        *specialist_cache_status = if specialist_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session_cache =
            SessionCache::load_or_create(cache_root, &specialist_session.id)?;
        let specialist_task = TaskEnvelope {
            id: task.id.clone(),
            session_id: main_session_id.to_string(),
            target_agent: AgentTarget::Specialist(target),
            title: talk.subject.clone(),
            user_request: talk.body.clone(),
            context: task.context.clone(),
            reply_expected: talk.reply_expected(),
            interaction_mode: task.interaction_mode,
            agent: None,
            group: None,
        };
        let (bundle, preload_record, loaded_specialist) = memory_hooks::preload(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&config.spec.default_model),
            SessionScope::Specialist(target),
            &specialist_task,
            &specialist_session,
            &mut specialist_session_cache,
            self.event_tx.clone(),
        )
        .await?;
        *specialist_bundle = bundle.clone();
        self.emit_librarian_pass(&preload_record);
        librarian_passes.push(preload_record);
        *specialist_prompt = assemble_prompt_with_context(
            &config.spec,
            &specialist_task,
            &loaded_specialist,
            &specialist_session,
            Some(&self.workspace_root),
            Some(self.provider.name()),
            None, // project brain injected on the mesh turn (turn_loop)
        );

        self.emit(CliEvent::SpecialistDelegated {
            agent: label.clone(),
            subject: talk.subject.clone(),
        });
        if talk.mode == 2 {
            self.emit(CliEvent::GatewayNotice(format!(
                "talk mode=2 to {label} (internal compatibility harness): this specialist runs now and hands its result back; the caller pauses until it returns."
            )));
        }

        // --- INTERNAL COMPATIBILITY HARNESS NOTE ---
        // The product path is always the mesh gateway. This synchronous helper
        // remains for focused runtime tests and older persisted fixtures only;
        // it is not selected by config, environment, or user-facing controls.
        //
        // True orchestrator-continues-while-specialist-runs (tokio::spawn) needs
        // the captured AgentRunner/provider/session to be Send; that is the open
        // design (PendingSpecialists/SpecialistFinish in mod.rs are the scaffold).

        // --- SYNC PATH: block until coder completes ---
        let mut nested_decision = None;
        let mut specialist_pending = PendingSpecialists::new();
        let specialist_loop = Box::pin(self.execute_provider_agent_loop(
            &config.spec,
            AgentTarget::Specialist(target),
            main_session_id,
            &specialist_task,
            &mut specialist_session,
            &loaded_specialist,
            session_store,
            cache_root,
            librarian_passes,
            &mut nested_decision,
            specialist_session_status,
            specialist_session_path,
            specialist_cache_status,
            specialist_cache_path,
            specialist_bundle,
            specialist_prompt,
            specialist_outcome,
            coder_parse,
            &mut specialist_pending,
        ))
        .await?;
        *coder_parse = if specialist_loop.parse_fallback_used {
            ParseRecord::fallback(label.clone(), specialist_loop.parse_detail.clone())
        } else {
            ParseRecord::parsed(label.clone(), specialist_loop.parse_detail.clone())
        };

        let mut specialist_result = agent_outcome_from_final(
            AgentTarget::Specialist(target),
            &format!("{target} reply"),
            specialist_loop.final_response,
            specialist_loop.provider_turn,
        );
        specialist_result.tool_results = specialist_loop.tool_results;
        specialist_session.push_message(Message::Assistant {
            content: specialist_result.summary.clone(),
        });
        // Specialist finished — expose outcome before librarian post-passes so talk
        // can return the specialist result block even when save/prune contracts fail.
        *specialist_outcome = Some(specialist_result.clone());

        // Render specialist completion before librarian maintenance. Prune/save
        // happen after the specialist loop, but emitting them first makes the CLI
        // look like memory ran before the specialist finished.
        self.emit(CliEvent::SpecialistOutput {
            agent: label.clone(),
            summary: specialist_result.summary.clone(),
        });
        self.emit(CliEvent::SpecialistCompleted {
            agent: label.clone(),
            ok: true,
            // Cap identically to the mesh finalize path so a long summary can't
            // bloat the event regardless of which runtime produced it.
            summary: crate::runtime::mesh::cap_chars(specialist_result.summary.trim(), 160),
        });

        let specialist_prune = memory_hooks::prune_session(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&config.spec.default_model),
            SessionScope::Specialist(target),
            &specialist_task,
            &mut specialist_session,
            &specialist_bundle,
            &mut specialist_session_cache,
            self.event_tx.clone(),
        )
        .await;
        self.emit_librarian_pass(&specialist_prune);
        librarian_passes.push(specialist_prune);
        let save_record_emitted = match memory_hooks::save_phase(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&config.spec.default_model),
            SessionScope::Specialist(target),
            &specialist_task,
            &specialist_session,
            &OrchestratorDecision {
                mode: DelegationMode::Handoff,
                target: AgentTarget::Specialist(target),
                rationale: format!("Talk from {from_name} to {label}."),
            },
            &specialist_result,
            &mut specialist_session_cache,
            self.event_tx.clone(),
        )
        .await
        {
            Ok(save_record) => with_preload_context(save_record, specialist_bundle),
            Err(error) => LibrarianPassRecord {
                session_scope: SessionScope::Specialist(target),
                phase: LibrarianPhase::Save,
                memory_paths: vec![],
                knowledge_paths: vec![],
                saved_memory_paths: vec![],
                omitted_items: vec![],
                receipts: vec![format!(
                    "Librarian save failed after {label} completed: {}",
                    sanitize_error(&error.to_string())
                )],
                context_budget_used: specialist_bundle.context_budget_used,
                pruned_message_count: 0,
                summary: format!(
                    "Specialist-session librarian save failed; {label} result was still delivered to the orchestrator."
                ),
            },
        };
        self.emit_librarian_pass(&save_record_emitted);
        librarian_passes.push(save_record_emitted);
        specialist_session_cache.save(cache_root)?;
        session_store.upsert(specialist_session);

        Ok(TalkResult {
            delivered_to: label.clone(),
            target_session_id,
            reply_status: if talk.reply_expected() {
                TalkReplyStatus::Expected
            } else {
                TalkReplyStatus::NotExpected
            },
            executed_target: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_not_self_talk;

    #[test]
    fn compatibility_runner_rejects_self_talk_aliases() {
        assert!(ensure_not_self_talk("coder", "Leo").is_err());
        assert!(ensure_not_self_talk("Phoenix", "orchestrator").is_err());
        assert!(ensure_not_self_talk("Avery (school_coach)", "school-coach").is_err());
    }

    #[test]
    fn compatibility_runner_allows_real_handoffs() {
        assert!(ensure_not_self_talk("coder", "researcher").is_ok());
        assert!(ensure_not_self_talk("Avery", "Nico").is_ok());
    }
}
