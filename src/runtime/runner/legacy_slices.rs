//! Legacy provider/tool-backed slices kept for diagnostics + local coder
//! tool execution.

use super::*;

impl AgentRunner {
    pub async fn execute_provider_backed_slice(
        &self,
        orchestrator: &Orchestrator,
        task: &TaskEnvelope,
    ) -> Result<RuntimeExecution> {
        if !matches!(task.target_agent, AgentTarget::Orchestrator) {
            bail!("provider-backed runtime slice expects an orchestrator task");
        }

        let mut session_store = SessionStore::new(self.state_root.join("sessions"));
        session_store.load_from_disk()?;

        let main_session_id = task.session_id.clone();
        let main_session_status = if session_store.get(&main_session_id).is_some() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        self.emit(CliEvent::SessionResolved {
            scope: "main".to_string(),
            session_id: main_session_id.clone(),
            status: format!("{:?}", main_session_status),
        });
        let mut main_session = session_store.load_or_create_main(
            &main_session_id,
            &orchestrator.spec().default_model,
            &orchestrator.spec().system_prompt,
        )?;
        let main_session_path = session_store.session_path(&main_session_id);
        main_session.push_message(Message::User {
            content: task.user_request.clone(),
        });

        let cache_root = self.state_root.join("session_cache");
        let main_cache_path = SessionCache::path_for(&cache_root, &main_session_id);
        let main_cache_status = if main_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut session_cache = SessionCache::load_or_create(&cache_root, &main_session_id)?;
        let (main_bundle, main_preload_record, loaded_main) = memory_hooks::preload(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&orchestrator.spec().default_model),
            SessionScope::Main,
            task,
            &main_session,
            &mut session_cache,
            self.event_tx.clone(),
        )
        .await?;
        self.emit_librarian_pass(&main_preload_record);
        let mut librarian_passes = vec![main_preload_record];
        let mut coder_parse = ParseRecord::not_applicable("coder", "Coder did not run yet.");
        let mut specialist_prompt = PromptAssembly {
            system_prompt: String::new(),
            user_prompt: "Coder did not execute.".to_string(),
            tail: String::new(),
        };
        let specialist_session_id =
            crate::session::specialist_session_id(&main_session.id, SubAgentType::Coder);
        let mut specialist_session_status = if session_store.get(&specialist_session_id).is_some() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session_path = session_store.session_path(&specialist_session_id);
        let mut specialist_cache_path = SessionCache::path_for(&cache_root, &specialist_session_id);
        let mut specialist_cache_status = if specialist_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_bundle = MemoryBundle {
            session_scope: SessionScope::Specialist(SubAgentType::Coder),
            task_frame: task.title.clone(),
            loaded_memory_paths: vec![],
            loaded_knowledge_paths: vec![],
            ranked_context_items: vec![],
            omitted_items: vec![],
            grounding_receipts: vec![],
            completion_state: "Coder session did not load.".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: Some("coder".to_string()),
            context_budget_used: 0,
            summary: "Coder session did not load.".to_string(),
        };
        let mut decision: Option<OrchestratorDecision> = None;
        let mut specialist_outcome: Option<AgentOutcome> = None;

        let root_main_session_id = main_session.id.clone();
        let mut pending_specialists = PendingSpecialists::new();
        let orchestrator_loop = self
            .execute_provider_agent_loop(
                orchestrator.spec(),
                AgentTarget::Orchestrator,
                &root_main_session_id,
                task,
                &mut main_session,
                &loaded_main,
                &mut session_store,
                &cache_root,
                &mut librarian_passes,
                &mut decision,
                &mut specialist_session_status,
                &mut specialist_session_path,
                &mut specialist_cache_status,
                &mut specialist_cache_path,
                &mut specialist_bundle,
                &mut specialist_prompt,
                &mut specialist_outcome,
                &mut coder_parse,
                &mut pending_specialists,
            )
            .await;

        let loop_result = orchestrator_loop?;
        let parse_detail = loop_result.parse_detail;
        let parse_fallback_used = loop_result.parse_fallback_used;
        let orchestrator_final = loop_result.final_response;
        let orchestrator_provider_turn = loop_result.provider_turn;
        let orchestrator_tool_results = loop_result.tool_results;

        let orchestrator_parse = if parse_fallback_used {
            ParseRecord::fallback("orchestrator", parse_detail)
        } else {
            ParseRecord::parsed("orchestrator", parse_detail)
        };

        let specialist_outcome = specialist_outcome.unwrap_or_else(|| AgentOutcome {
            completion: crate::runtime::OutcomeCompletion::Unknown,
            agent: AgentTarget::Orchestrator,
            summary: "No specialist executed; orchestrator answered directly.".to_string(),
            artifacts: vec![],
            tool_results: vec![],
            provider_response: None,
        });
        let decision = decision.unwrap_or_else(|| OrchestratorDecision {
            mode: DelegationMode::StayLocal,
            target: AgentTarget::Orchestrator,
            rationale: "Orchestrator answered directly without specialist handoff.".to_string(),
        });
        // A Handoff already streamed its delegation + specialist output inline during
        // `deliver_talk`; re-emitting the routing banner here lands AFTER the specialist
        // finished and reads as a confusing late/duplicate "→ {agent}". Only surface
        // routing for a direct (StayLocal) answer, where nothing else announced it.
        if !matches!(decision.mode, DelegationMode::Handoff) {
            self.emit(CliEvent::Routing {
                target: match &decision.target {
                    AgentTarget::Orchestrator => "Direct answer".to_string(),
                    AgentTarget::Specialist(st) => format!("{:?}", st),
                },
                rationale: decision.rationale.clone(),
            });
        }
        let mut outcome = agent_outcome_from_final(
            AgentTarget::Orchestrator,
            "Orchestrator reply",
            orchestrator_final,
            orchestrator_provider_turn,
        );
        outcome.tool_results = orchestrator_tool_results;
        main_session.push_message(Message::Assistant {
            content: outcome.summary.clone(),
        });
        librarian_passes.push(
            memory_hooks::prune_session(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                &orchestrator.spec().default_model,
                SessionScope::Main,
                task,
                &mut main_session,
                &main_bundle,
                &mut session_cache,
                self.event_tx.clone(),
            )
            .await,
        );

        let main_save = memory_hooks::save_phase(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&orchestrator.spec().default_model),
            SessionScope::Main,
            task,
            &main_session,
            &decision,
            &outcome,
            &mut session_cache,
            self.event_tx.clone(),
        )
        .await?;
        let main_save_record = with_preload_context(main_save, &main_bundle);
        self.emit_librarian_pass(&main_save_record);
        librarian_passes.push(main_save_record);
        session_cache.save(&cache_root)?;
        session_store.upsert(main_session.clone());
        session_store.save_to_disk()?;

        self.emit(CliEvent::FinalOutput(outcome.summary.clone()));
        self.emit(CliEvent::Done);

        Ok(RuntimeExecution {
            main_session_id,
            specialist_session_id,
            main_session_status,
            specialist_session_status,
            main_session_path,
            specialist_session_path,
            main_cache_status,
            specialist_cache_status,
            main_cache_path,
            specialist_cache_path,
            decision,
            main_bundle,
            specialist_bundle,
            librarian_passes,
            specialist_prompt,
            specialist_outcome,
            outcome,
            orchestrator_parse,
            coder_parse,
        })
    }

    pub async fn execute_tool_backed_slice(
        &self,
        orchestrator: &Orchestrator,
        task: &TaskEnvelope,
    ) -> Result<RuntimeExecution> {
        if !matches!(task.target_agent, AgentTarget::Orchestrator) {
            bail!("tool-backed runtime slice expects an orchestrator task");
        }

        let mut session_store = SessionStore::new(self.state_root.join("sessions"));
        session_store.load_from_disk()?;

        let main_session_id = task.session_id.clone();
        let main_session_status = if session_store.get(&main_session_id).is_some() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut main_session = session_store.load_or_create_main(
            &main_session_id,
            &orchestrator.spec().default_model,
            &orchestrator.spec().system_prompt,
        )?;
        let main_session_path = session_store.session_path(&main_session_id);
        main_session.push_message(Message::User {
            content: task.user_request.clone(),
        });

        let cache_root = self.state_root.join("session_cache");
        let main_cache_path = SessionCache::path_for(&cache_root, &main_session_id);
        let main_cache_status = if main_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut session_cache = SessionCache::load_or_create(&cache_root, &main_session_id)?;
        let (main_bundle, main_preload_record, _loaded_main) = memory_hooks::preload(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&orchestrator.spec().default_model),
            SessionScope::Main,
            task,
            &main_session,
            &mut session_cache,
            self.event_tx.clone(),
        )
        .await?;
        let mut librarian_passes = vec![main_preload_record];

        let decision = orchestrator.route_task(task)?;
        self.emit(CliEvent::Routing {
            target: match &decision.target {
                AgentTarget::Orchestrator => "Direct answer".to_string(),
                AgentTarget::Specialist(st) => format!("{:?}", st),
            },
            rationale: decision.rationale.clone(),
        });
        if !matches!(decision.mode, DelegationMode::Handoff)
            || !matches!(
                decision.target,
                AgentTarget::Specialist(SubAgentType::Coder)
            )
        {
            bail!("tool-backed runtime slice only supports orchestrator handoff to coder");
        }

        let coder = coder_config();
        let specialist_session_id =
            crate::session::specialist_session_id(&main_session.id, SubAgentType::Coder);
        let specialist_session_status = if session_store.get(&specialist_session_id).is_some() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session = session_store.load_or_create_specialist(
            &main_session.id,
            SubAgentType::Coder,
            &coder.spec.default_model,
            &coder.spec.system_prompt,
        )?;
        let specialist_session_path = session_store.session_path(&specialist_session_id);
        specialist_session.push_message(Message::Talk {
            from: "Orchestrator".to_string(),
            to: "Coder".to_string(),
            subject: task.title.clone(),
            body: task.user_request.clone(),
            reply_expected: true,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: "working".to_string(),
        });

        let specialist_session_id = specialist_session.id.clone();
        let specialist_cache_path = SessionCache::path_for(&cache_root, &specialist_session_id);
        let specialist_cache_status = if specialist_cache_path.exists() {
            PersistenceStatus::Resumed
        } else {
            PersistenceStatus::Created
        };
        let mut specialist_session_cache =
            SessionCache::load_or_create(&cache_root, &specialist_session_id)?;
        let (specialist_bundle, specialist_preload_record, loaded_specialist) =
            memory_hooks::preload(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                self.librarian_model(&coder.spec.default_model),
                SessionScope::Specialist(SubAgentType::Coder),
                task,
                &specialist_session,
                &mut specialist_session_cache,
                self.event_tx.clone(),
            )
            .await?;
        librarian_passes.push(specialist_preload_record);

        let specialist_prompt = assemble_prompt_with_context(
            &coder.spec,
            task,
            &loaded_specialist,
            &specialist_session,
            Some(&self.workspace_root),
            Some(self.provider.name()),
            None, // project brain injected on the mesh turn (turn_loop)
        );
        let (parsed_turn, tool_results) = self.execute_local_coder_tools(task)?;

        let specialist_outcome = AgentOutcome {
            completion: crate::runtime::OutcomeCompletion::Completed,
            agent: AgentTarget::Specialist(SubAgentType::Coder),
            summary: parsed_turn.final_markdown.clone(),
            artifacts: {
                let mut artifacts = parsed_turn.to_artifacts();
                artifacts.push(AgentArtifact {
                    kind: ArtifactKind::Plan,
                    title: "Execution contract".to_string(),
                    body: coder.spec.summary(),
                });
                artifacts
            },
            tool_results,
            provider_response: None,
        };
        specialist_session.push_message(Message::Assistant {
            content: specialist_outcome.summary.clone(),
        });
        librarian_passes.push(
            memory_hooks::prune_session(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                self.librarian_model(&coder.spec.default_model),
                SessionScope::Specialist(SubAgentType::Coder),
                task,
                &mut specialist_session,
                &specialist_bundle,
                &mut specialist_session_cache,
                self.event_tx.clone(),
            )
            .await,
        );

        let specialist_save = memory_hooks::save_phase(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&coder.spec.default_model),
            SessionScope::Specialist(SubAgentType::Coder),
            task,
            &specialist_session,
            &decision,
            &specialist_outcome,
            &mut specialist_session_cache,
            self.event_tx.clone(),
        )
        .await?;
        librarian_passes.push(with_preload_context(specialist_save, &specialist_bundle));
        specialist_session_cache.save(&cache_root)?;
        session_store.upsert(specialist_session.clone());

        let outcome = orchestrator.finalize_after_specialist(task, &decision, &specialist_outcome);
        main_session.push_message(Message::Talk {
            from: "Coder".to_string(),
            to: "Orchestrator".to_string(),
            subject: task.title.clone(),
            body: specialist_outcome.summary.clone(),
            reply_expected: false,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: "done".to_string(),
        });
        main_session.push_message(Message::Assistant {
            content: outcome.summary.clone(),
        });
        librarian_passes.push(
            memory_hooks::prune_session(
                &self.memory_root,
                &self.workspace_root,
                Arc::clone(&self.provider),
                &orchestrator.spec().default_model,
                SessionScope::Main,
                task,
                &mut main_session,
                &main_bundle,
                &mut session_cache,
                self.event_tx.clone(),
            )
            .await,
        );

        let main_save = memory_hooks::save_phase(
            &self.memory_root,
            &self.workspace_root,
            Arc::clone(&self.provider),
            self.librarian_model(&orchestrator.spec().default_model),
            SessionScope::Main,
            task,
            &main_session,
            &decision,
            &outcome,
            &mut session_cache,
            self.event_tx.clone(),
        )
        .await?;
        librarian_passes.push(with_preload_context(main_save, &main_bundle));
        session_cache.save(&cache_root)?;
        session_store.upsert(main_session.clone());
        session_store.save_to_disk()?;

        self.emit_librarian_passes(&librarian_passes);
        self.emit(CliEvent::FinalOutput(outcome.summary.clone()));
        self.emit(CliEvent::Done);

        Ok(RuntimeExecution {
            main_session_id,
            specialist_session_id,
            main_session_status,
            specialist_session_status,
            main_session_path,
            specialist_session_path,
            main_cache_status,
            specialist_cache_status,
            main_cache_path,
            specialist_cache_path,
            decision,
            main_bundle,
            specialist_bundle,
            librarian_passes,
            specialist_prompt,
            specialist_outcome,
            outcome,
            orchestrator_parse: ParseRecord::not_applicable(
                "orchestrator",
                "Local tool-backed slice used deterministic Rust routing.",
            ),
            coder_parse: ParseRecord::not_applicable(
                "coder",
                "Local tool-backed slice constructs coder result from local tool execution.",
            ),
        })
    }

    pub(super) fn execute_local_coder_tools(
        &self,
        task: &TaskEnvelope,
    ) -> Result<(CoderTurnResult, Vec<ToolCallResult>)> {
        let executor = ToolExecutor::new(&self.workspace_root)?;
        let calls = vec![
            crate::runtime::ToolCall {
                tool_name: "glob".to_string(),
                input: serde_json::json!({ "pattern": "src/**/*.rs" }),
            },
            crate::runtime::ToolCall {
                tool_name: "read".to_string(),
                input: serde_json::json!({ "path": "Cargo.toml" }),
            },
            crate::runtime::ToolCall {
                tool_name: "grep".to_string(),
                input: serde_json::json!({ "pattern": "TODO", "path": "src" }),
            },
            crate::runtime::ToolCall {
                tool_name: "bash".to_string(),
                input: serde_json::json!({ "command": "cargo test -q" }),
            },
        ];

        let mut transcript = Vec::new();
        let mut results = Vec::new();
        for call in calls {
            let input_summary = summarize_tool_input(&call.tool_name, &call.input);
            self.emit(CliEvent::ToolCallStarted {
                agent: "coder".to_string(),
                tool_name: call.tool_name.clone(),
                input_summary: input_summary.clone(),
            });
            let result = executor.execute(call);
            self.emit(CliEvent::ToolCallCompleted {
                agent: "coder".to_string(),
                tool_name: result.tool_name.clone(),
                input_summary: result.input_summary.clone(),
                success: result.success,
                output_summary: first_line(&result.output).to_string(),
                diff: None,
            });
            transcript.push(CoderToolCall {
                name: result.tool_name.clone(),
                input_summary,
                outcome: first_line(&result.output).to_string(),
            });
            results.push(result);
        }

        let tools_used = transcript
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let verification = results
            .iter()
            .find(|result| result.tool_name == "bash")
            .map(|result| {
                if result.success {
                    "cargo test -q completed through the Phoenix bash tool.".to_string()
                } else {
                    format!(
                        "cargo test -q failed through the Phoenix bash tool: {}",
                        result.output
                    )
                }
            })
            .into_iter()
            .collect::<Vec<_>>();

        let failed = results
            .iter()
            .filter(|result| !result.success)
            .map(|result| {
                format!(
                    "{} failed: {}",
                    result.tool_name,
                    first_line(&result.output)
                )
            })
            .collect::<Vec<_>>();
        let status = if failed.is_empty() {
            "The tool-backed coder inspection completed without tool failures.".to_string()
        } else {
            format!(
                "The tool-backed coder inspection completed with failures: {}",
                failed.join("; ")
            )
        };

        Ok((
            CoderTurnResult {
                summary: status.clone(),
                final_markdown: format!(
                    "## Result\n{status}\n\nNo source edits were attempted in this diagnostic real loop.\n\n### Tools used\n{tools_used}\n\n### Task\n{}",
                    task.user_request
                ),
                changes_made: vec![],
                verification,
                execution_mode: "real_tool_backed".to_string(),
                tool_transcript: transcript,
            },
            results,
        ))
    }
}
