//! Bounded, ephemeral generic workers for independent volume batches.

use super::*;
use futures_util::stream::{self, StreamExt};

const MAX_RESULT_CHARS_PER_JOB: usize = 16_000;

/// Volume workers have the ordinary Phoenix catalog, subject to the caller's
/// inherited permission/approval gates. Their terminal, desktop, browser,
/// tabs, and profile are item-scoped, so those tools do not share live process
/// state with the caller or sibling workers. Only lifecycle/coordination tools
/// that could escape the item's ephemeral ownership are withheld.
pub(super) fn volume_worker_tool_allowed(tool_name: &str) -> bool {
    crate::tools::tool_spec(tool_name).is_some()
        && !matches!(
            tool_name,
            // A worker always completes into the parent `volume_work` receipt;
            // it must not create an unbounded hidden worker tree or control
            // another item's lifecycle.
            "volume_work" | "talk" | "message_agent" | "agent_control"
                // Durable identities and schedules outlive the disposable
                // worker, so require a named coworker to own them instead.
                | "create_agent" | "agent_provision" | "cron"
        )
}

fn discard_volume_worker_state_at(
    state_root: &std::path::Path,
    main_session_id: &str,
    scope: &str,
) {
    let session_id = crate::session::specialist_session_id_scoped(
        main_session_id,
        crate::sub_agents::volume_worker::agent_type(),
        Some(scope),
    );
    let store = crate::session::SessionStore::new(state_root.join("sessions"));
    let session_path = store.session_path(&session_id);
    match std::fs::remove_file(&session_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            "volume worker cleanup could not remove {}: {error}",
            session_path.display()
        ),
    }
    if let Some(name) = session_path.file_name().and_then(|name| name.to_str()) {
        let lock_path = session_path.with_file_name(format!(".{name}.lock"));
        match std::fs::remove_file(&lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                "volume worker cleanup could not remove {}: {error}",
                lock_path.display()
            ),
        }
    }
    let browser_profile = format!("volume-worker-{scope}");
    if let Err(error) =
        crate::tools::browser_native::discard_volume_worker_profile(&browser_profile)
    {
        tracing::warn!(
            "volume worker cleanup could not discard browser profile {browser_profile}: {error:#}"
        );
    }
}

/// Cancellation-safe cleanup. Dropping the parent `volume_work` future (user
/// Stop, panic, connection loss) drops every in-flight item future and
/// therefore runs this guard even though normal function epilogues are skipped.
struct VolumeWorkerCleanup {
    state_root: std::path::PathBuf,
    main_session_id: String,
    scope: String,
    event_tx: Option<tokio::sync::mpsc::Sender<CliEvent>>,
    batch_id: String,
    worker_id: String,
    label: String,
    item_id: String,
    desktop_scope: Option<crate::tools::isolated_desktop::DesktopScope>,
    terminal_status: Option<crate::runtime::VolumeWorkerLifecycleStatus>,
}

impl VolumeWorkerCleanup {
    fn new(
        runner: &MeshRunner,
        batch_id: String,
        scope: String,
        label: String,
        item_id: String,
        parent_agent_id: &str,
    ) -> Self {
        let desktop_scope = crate::tools::isolated_desktop::DesktopScope::worker(
            &runner.main_session_id,
            parent_agent_id,
            &scope,
        )
        .map_err(|error| {
            tracing::warn!(
                "volume worker desktop scope could not be derived for {scope}: {error:#}"
            );
            error
        })
        .ok();
        Self {
            state_root: runner.state_root.clone(),
            main_session_id: runner.main_session_id.clone(),
            event_tx: runner.event_tx.clone(),
            batch_id,
            worker_id: scope.clone(),
            scope,
            label,
            item_id,
            desktop_scope,
            terminal_status: None,
        }
    }

    fn settle(&mut self, status: crate::runtime::VolumeWorkerLifecycleStatus) {
        self.terminal_status = Some(status);
    }

    fn emit_lifecycle(&self, status: crate::runtime::VolumeWorkerLifecycleStatus) {
        let event = CliEvent::VolumeWorkerLifecycle {
            batch_id: self.batch_id.clone(),
            worker_id: self.worker_id.clone(),
            label: self.label.clone(),
            item_id: self.item_id.clone(),
            status,
        };
        // The ordinary turn channel is bounded so a storm of detailed tool
        // rows cannot make a worker's terminal chip disappear forever. When
        // it is full/closed, publish this compact lifecycle signal directly
        // to the session postbox; successful channel sends keep the normal
        // daemon ordering and do not double-publish.
        let delivered = self
            .event_tx
            .as_ref()
            .is_some_and(|event_tx| event_tx.try_send(event.clone()).is_ok());
        if !delivered {
            crate::runtime::postbox::forward(&self.main_session_id, event);
        }
    }
}

impl Drop for VolumeWorkerCleanup {
    fn drop(&mut self) {
        discard_volume_worker_state_at(&self.state_root, &self.main_session_id, &self.scope);
        if let Some(scope) = self.desktop_scope.as_ref() {
            crate::tools::isolated_desktop::discard_scope(scope);
        }
        self.emit_lifecycle(
            self.terminal_status
                .unwrap_or(crate::runtime::VolumeWorkerLifecycleStatus::Cancelled),
        );
    }
}

impl MeshRunner {
    fn volume_worker_clone(&self, scope: String) -> MeshRunner {
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
            event_tx: self.event_tx.clone(),
            group_status_tx: self.group_status_tx.clone(),
            vision: self.vision.clone(),
            native_vision: self.native_vision,
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
            job_scope: Some(scope),
            starting_turn: None,
            group_context: self.group_context.clone(),
            group_authored_turn_id: std::sync::Mutex::new(self.group_authored_turn_id.lock().unwrap_or_else(|p|p.into_inner()).clone()),
            direct_agent_context: None,
            #[cfg(test)]
            group_direct_context_sessions: self.group_direct_context_sessions.clone(),
        }
    }

    fn volume_settings_scope(&self, caller: &AgentAddress) -> crate::settings::SettingsScope {
        self.group_context
            .as_ref()
            .map(|group| crate::settings::SettingsScope::Group {
                id: group.group_id.clone(),
            })
            .unwrap_or_else(|| crate::settings::SettingsScope::Agent {
                id: match caller {
                    AgentAddress::Orchestrator => "phoenix".to_string(),
                    _ => crate::runtime::postbox::base_agent(&caller.label()).to_string(),
                },
            })
    }

    pub(super) async fn run_volume_batch(
        &self,
        caller: &AgentAddress,
        input: crate::tools::VolumeWorkInput,
        authoritative_state: &str,
    ) -> Result<crate::tools::VolumeWorkResult> {
        let settings_scope = self.volume_settings_scope(caller);
        if !crate::settings::effective_bool("agents.volume_workers_enabled", &settings_scope)
            .unwrap_or(true)
        {
            bail!("Generic volume workers are disabled in Settings → Agents & Groups");
        }
        let configured_limit =
            crate::settings::effective_u64("agents.volume_worker_limit", &settings_scope)
                .unwrap_or(8) as usize;
        let concurrency = input.validate(configured_limit)?;
        let objective = input.objective.trim().to_string();
        let authoritative_state = authoritative_state.trim().to_string();
        let requested = input.jobs.len();
        let caller_address = caller.clone();
        let caller_desktop_role = match caller {
            AgentAddress::Orchestrator => "phoenix".to_string(),
            AgentAddress::Specialist(_) => {
                crate::runtime::postbox::base_agent(&caller.label()).to_string()
            }
            AgentAddress::User => "phoenix".to_string(),
        };
        let batch_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();

        self.emit(CliEvent::GatewayNotice(format!(
            "Volume batch `{batch_id}` started: {requested} independent jobs, up to {concurrency} at once. Workers inherit the caller's permission mode, are independently cancellable through the parent Stop action, and have no per-item wall-clock deadline."
        )));

        let completed = stream::iter(input.jobs.into_iter().enumerate().map(|(index, job)| {
            let worker_number = index + 1;
            let scope = format!("volume-{batch_id}-{worker_number}");
            let worker_label = format!("Worker {worker_number}");
            let runner = self.volume_worker_clone(scope.clone());
            let caller_address = caller_address.clone();
            let caller_desktop_role = caller_desktop_role.clone();
            let objective = objective.clone();
            let authoritative_state = authoritative_state.clone();
            let batch_id = batch_id.clone();
            async move {
                let mut cleanup = VolumeWorkerCleanup::new(
                    &runner,
                    batch_id.clone(),
                    scope.clone(),
                    worker_label.clone(),
                    job.id.clone(),
                    &caller_desktop_role,
                );
                cleanup.emit_lifecycle(crate::runtime::VolumeWorkerLifecycleStatus::Started);
                // These anonymous, short-lived workers are implementation
                // details of one volume_work call, not durable coworker
                // handoffs. The parent tool receipt and batch notices
                // are the complete user-facing lifecycle.
                let mut body = format!(
                    "BATCH OBJECTIVE:\n{objective}\n\nYOUR ITEM [{}]:\n{}",
                    job.id,
                    job.task.trim()
                );
                if !authoritative_state.is_empty() {
                    body.push_str("\n\nPARENT AUTHORITATIVE STATE SNAPSHOT:\n");
                    body.push_str(&authoritative_state);
                    body.push_str(
                        "\n\nSTATE RECONCILIATION CONTRACT:\nThe parent state snapshot outranks item context and planning metadata. Available/unlocked content is not evidence that it is unfinished. A verified downstream completion protects its prerequisites. If live evidence truly conflicts, report the conflict; never silently reopen completed work.",
                    );
                }
                if let Some(context) = job
                    .context
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    body.push_str("\n\nITEM CONTEXT:\n");
                    body.push_str(context);
                }
                if let Some(expected) = job
                    .expected_output
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    body.push_str("\n\nEXPECTED OUTPUT / ACCEPTANCE:\n");
                    body.push_str(expected);
                }
                let worker = AgentAddress::Specialist(
                    crate::sub_agents::volume_worker::agent_type(),
                );
                let incoming = AgentMessage::talk(
                    caller_address.clone(),
                    worker.clone(),
                    format!("volume item: {}", job.id),
                    body,
                    true,
                );
                let outbound = runner.run_turn(&worker, incoming).await;
                let accepted = outbound.iter().find(|message| message.to == caller_address);
                let (ok, summary, output) = match accepted {
                    Some(message) => (
                        !message.is_failed_result(),
                        cap_chars(&message.subject, 300),
                        cap_chars(&message.body, MAX_RESULT_CHARS_PER_JOB),
                    ),
                    None if outbound.is_empty() => (
                        false,
                        "worker returned no result".to_string(),
                        "The ephemeral worker finished without a result message.".to_string(),
                    ),
                    None => (
                        false,
                        "worker attempted an unsupported handoff".to_string(),
                        "The ephemeral worker tried to delegate instead of completing its independent item. Route this item to a named coworker or make it self-contained."
                            .to_string(),
                    ),
                };
                cleanup.settle(if ok {
                    crate::runtime::VolumeWorkerLifecycleStatus::Completed
                } else {
                    crate::runtime::VolumeWorkerLifecycleStatus::Failed
                });
                (
                    index,
                    crate::tools::VolumeJobResult {
                        id: job.id,
                        ok,
                        summary,
                        output,
                    },
                )
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

        let mut completed = completed;
        completed.sort_by_key(|(index, _)| *index);
        let results = completed
            .into_iter()
            .map(|(_, result)| result)
            .collect::<Vec<_>>();
        let failed = results.iter().filter(|result| !result.ok).count();
        self.emit(CliEvent::GatewayNotice(format!(
            "Volume batch `{batch_id}` finished: {} completed, {failed} failed.",
            requested.saturating_sub(failed)
        )));
        Ok(crate::tools::VolumeWorkResult {
            objective,
            requested,
            completed: requested.saturating_sub(failed),
            failed,
            results,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_workers_receive_item_scoped_interactive_and_mutating_tools() {
        for denied in [
            "volume_work",
            "talk",
            "message_agent",
            "agent_control",
            "create_agent",
            "agent_provision",
            "cron",
        ] {
            assert!(
                !volume_worker_tool_allowed(denied),
                "{denied} must stay parent-owned"
            );
        }
        for allowed in [
            "read",
            "grep",
            "list_directory",
            "web_fetch",
            "image_analyze",
            "write",
            "str_replace",
            "bash",
            "browser_navigate",
            "browser_state",
            "computer_status",
            "computer_screenshot",
            "composio_run",
            "final_answer",
        ] {
            assert!(volume_worker_tool_allowed(allowed), "{allowed} missing");
        }
    }

    #[test]
    fn cancellation_guard_removes_ephemeral_worker_session() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let state_root = home.path().join("state");
        let sessions = state_root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let main_session_id = "agent-school-coach";
        let scope = "volume-cancel-proof-1";
        let session_id = crate::session::specialist_session_id_scoped(
            main_session_id,
            crate::sub_agents::volume_worker::agent_type(),
            Some(scope),
        );
        let store = crate::session::SessionStore::new(&sessions);
        let path = store.session_path(&session_id);
        std::fs::write(&path, b"ephemeral").unwrap();
        let guard = VolumeWorkerCleanup {
            state_root,
            main_session_id: main_session_id.to_string(),
            scope: scope.to_string(),
            event_tx: None,
            batch_id: "volume-test".to_string(),
            worker_id: scope.to_string(),
            label: "Worker 1".to_string(),
            item_id: "item-1".to_string(),
            desktop_scope: None,
            terminal_status: None,
        };
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists(), "cancelled worker session leaked");
    }

    #[test]
    fn lifecycle_terminal_signal_falls_back_when_the_turn_channel_is_full() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let session_id = format!("volume-lifecycle-fallback-{}", uuid::Uuid::new_v4());
        let (postbox_tx, mut postbox_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::runtime::postbox::subscribe(&session_id, postbox_tx);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(CliEvent::GatewayNotice(
                "channel intentionally full".to_string(),
            ))
            .unwrap();
        let cleanup = VolumeWorkerCleanup {
            state_root: home.path().join("state"),
            main_session_id: session_id,
            scope: "volume-fallback-1".to_string(),
            event_tx: Some(event_tx),
            batch_id: "batch-fallback".to_string(),
            worker_id: "volume-fallback-1".to_string(),
            label: "Worker 1".to_string(),
            item_id: "item-1".to_string(),
            desktop_scope: None,
            terminal_status: None,
        };
        cleanup.emit_lifecycle(crate::runtime::VolumeWorkerLifecycleStatus::Started);
        assert!(matches!(
            postbox_rx.try_recv(),
            Ok(CliEvent::VolumeWorkerLifecycle {
                status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
                ..
            })
        ));
        drop(cleanup);
        assert!(matches!(
            postbox_rx.try_recv(),
            Ok(CliEvent::VolumeWorkerLifecycle {
                status: crate::runtime::VolumeWorkerLifecycleStatus::Cancelled,
                ..
            })
        ));
    }
}
