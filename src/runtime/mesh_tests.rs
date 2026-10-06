use super::*;

#[test]
fn finalization_preserves_partial_reply_without_claiming_completion() {
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
    let runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![])), dir.path(), "partial-result");
    let mut store = SessionStore::new(dir.path().join("sessions"));
    let address = AgentAddress::Orchestrator;
    let (session, _) = runner.load_session(&mut store, &address, &orchestrator_spec()).unwrap();
    let incoming = AgentMessage::user_input(address.clone(), "Finish the requested artifact.");
    let response = FinalResponse { summary:"Useful draft saved".into(), final_markdown:"Your editable draft is saved.".into(),
        changes_made:vec![],verification:vec![],execution_mode:"incomplete_work".into(),tool_transcript:vec![] };
    let messages = runner.finalize(&address,&AgentAddress::User,&incoming,response,&mut store,session).unwrap();
    assert_eq!(messages[0].body,"Your editable draft is saved.");
    assert!(messages[0].is_failed_result(), "ordinary partial prose must retain runtime failure state");
    assert_eq!(AgentMessage::outcome_completion(&messages),crate::runtime::OutcomeCompletion::Incomplete);
    let mut reopened = SessionStore::new(store.root());
    let (session, _) = runner.load_session(&mut reopened, &address, &orchestrator_spec()).unwrap();
    assert!(session.messages.iter().any(|message| matches!(message,Message::Assistant{content} if content=="Your editable draft is saved.")));
}

#[test]
fn canonical_final_replaces_short_provider_summary() {
    let mut session = Session::new_main_with_id("final-history", "model", "system");
    session.push_message(Message::User {
        content: "Who am I?".into(),
    });
    session.push_message(Message::Assistant {
        content: "I remember you.".into(),
    });
    super::persist_canonical_final(
        &mut session,
        "I remember you.\n\n- The complete durable answer",
    );
    assert!(
        matches!(session.messages.last(), Some(Message::Assistant { content }) if content.contains("complete durable answer"))
    );
    assert_eq!(
        session.messages.len(),
        2,
        "the short summary must be replaced, not duplicated"
    );
}

#[test]
fn canonical_final_is_appended_after_tool_result() {
    let mut session = Session::new_main_with_id("final-after-tool", "model", "system");
    session.push_message(Message::ToolResult {
        tool_name: "recall".into(),
        input: "{}".into(),
        success: true,
        output: "memory".into(),
    });
    super::persist_canonical_final(&mut session, "The complete answer");
    assert!(
        matches!(session.messages.last(), Some(Message::Assistant { content }) if content == "The complete answer")
    );
}

#[test]
fn screenshot_paths_parse() {
    assert_eq!(
        screenshot_path("Screenshot saved: /tmp/a.png\nmore"),
        Some("/tmp/a.png".to_string())
    );
    // Executor output format: "{summary}\n{content}" — the marker is line 2.
    assert_eq!(
        screenshot_path("desktop screenshot\nScreenshot saved: /tmp/b.png"),
        Some("/tmp/b.png".to_string())
    );
    assert_eq!(screenshot_path("PDF saved: /tmp/a.pdf"), None);
    assert_eq!(screenshot_path(""), None);
}

use crate::providers::contracts::{
    AuthType as ContractAuthType, CompletionRequest, CompletionResponse, MessageRole, ModelInfo,
    StreamingResponse, TokenUsage,
};
use crate::runtime::gateway::Gateway;
use crate::runtime::{
    AgentTargetSpec, ExecutionStyle, OutputContract, PermissionProfile, WorkflowContract,
};
use crate::session::SubAgentType;
use std::collections::HashMap as StdHashMap;
use std::sync::Mutex;

fn orchestrator_spec() -> AgentSpec {
    AgentSpec {
        name: "Orchestrator".to_string(),
        target: AgentTargetSpec::Orchestrator,
        system_prompt: "You are Phoenix (test orchestrator).".to_string(),
        default_model: "mock-model".to_string(),
        tool_allowlist: vec![
            "read".to_string(),
            "design_reference".to_string(),
            "talk".to_string(),
            "final_answer".to_string(),
        ],
        permissions: PermissionProfile {
            can_delegate: true,
            can_use_shell: false,
            can_write_files: false,
            can_access_network: false,
        },
        workflow: WorkflowContract {
            execution_style: ExecutionStyle::RouteOnly,
            must_report_to_orchestrator: false,
            review_required_before_done: false,
            notes: vec![],
        },
        output: OutputContract {
            label: "reply".to_string(),
            required_artifacts: vec![],
            final_answer_style: "markdown".to_string(),
        },
    }
}

/// Scripted provider: per-agent queues of envelope responses, matched on the
/// `Agent: <name>` line the prompt assembly always includes.
struct ScriptedProvider {
    scripts: Mutex<StdHashMap<String, Vec<String>>>,
    request_texts: Mutex<Vec<String>>,
    native_images: bool,
    native_tools: bool,
    request_image_counts: Mutex<Vec<usize>>,
    request_image_messages: Mutex<Vec<Vec<ChatMessage>>>,
    request_image_wire: Mutex<Vec<Vec<serde_json::Value>>>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<(&str, Vec<String>)>) -> Self {
        Self {
            scripts: Mutex::new(
                scripts
                    .into_iter()
                    .map(|(agent, queue)| (agent.to_string(), queue))
                    .collect(),
            ),
            request_texts: Mutex::new(Vec::new()),
            native_images: false,
            native_tools: false,
            request_image_counts: Mutex::new(Vec::new()),
            request_image_messages: Mutex::new(Vec::new()),
            request_image_wire: Mutex::new(Vec::new()),
        }
    }

    fn agent_for(request: &CompletionRequest) -> String {
        request
            .messages
            .iter()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| {
                m.content
                    .lines()
                    .find_map(|line| line.strip_prefix("Agent: "))
            })
            .unwrap_or("unknown")
            .trim()
            .to_string()
    }
}

#[async_trait]
impl LLMProvider for ScriptedProvider {
    fn supports_native_images(&self) -> bool { self.native_images }
    fn name(&self) -> &str {
        "mock"
    }
    fn display_name(&self) -> &str {
        "Mock"
    }
    fn base_url(&self) -> &str {
        "mock://mesh"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> StdHashMap<String, String> {
        StdHashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        // History is append-only, so earlier image sets stay committed; what
        // a round "sends" is the newest set, if it arrived after the model's
        // latest reply.
        let newest = request.messages.iter().rposition(|m| !m.images.is_empty());
        let reply = request.messages.iter().rposition(|m| m.role == crate::providers::MessageRole::Assistant);
        let current = newest.filter(|&i| reply.is_none_or(|r| i > r)).map_or(0, |i| request.messages[i].images.len());
        self.request_image_counts.lock().unwrap().push(current);
        let newest_set: Vec<_> = newest.map(|i| request.messages[i].clone()).into_iter().collect();
        self.request_image_messages.lock().unwrap().push(newest_set.clone());
        self.request_image_wire.lock().unwrap().push(if self.native_images {
            let mut only_newest = request.clone();
            only_newest.messages = newest_set;
            crate::providers::openai_codex::OpenAICodexProvider::responses_input(&only_newest).into_iter()
                .flat_map(|item| item["content"].as_array().cloned().unwrap_or_default())
                .filter(|item| item["type"] == "input_image").collect()
        } else { Vec::new() });
        self.request_texts.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let agent = Self::agent_for(&request);
        let mut scripts = self.scripts.lock().unwrap();
        let queue = scripts
            .get_mut(&agent)
            .unwrap_or_else(|| panic!("no script for agent `{agent}`"));
        assert!(!queue.is_empty(), "script exhausted for agent `{agent}`");
        let mut content = queue.remove(0);
        let tool_calls = if self.native_tools {
            if let Ok(crate::runtime::AgentTurnResponse::ToolRequest { tool_calls, .. }) = serde_json::from_str(&content) {
                content.clear();
                tool_calls.into_iter().enumerate().map(|(index, call)| crate::providers::contracts::NativeToolCall {
                    id: format!("native-{}-{index}", queue.len()), tool_name: call.tool_name, arguments: call.input,
                }).collect()
            } else { Vec::new() }
        } else { Vec::new() };
        Ok(CompletionResponse {
            content,
            model: request.model,
            usage: TokenUsage::new(1, 1),
            reasoning: None,
            stop_reason: Some("stop".to_string()),
            tool_calls,
            provider_replay: None,
        })
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    async fn stream(&self, _request: CompletionRequest) -> Result<StreamingResponse> {
        unimplemented!("mesh tests do not stream")
    }
    async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        unimplemented!("mesh tests do not embed")
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
}

/// Provider that can answer forever with the same response. Runtime bound
/// tests use this instead of a finite script, so exhausting a test vector can
/// never masquerade as the safety mechanism under test.
struct RepeatingProvider {
    content: String,
    request_texts: Mutex<Vec<String>>,
    calls: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    peak_active: std::sync::atomic::AtomicUsize,
    delay: Option<std::time::Duration>,
}

impl RepeatingProvider {
    fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            request_texts: Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicUsize::new(0),
            active: std::sync::atomic::AtomicUsize::new(0),
            peak_active: std::sync::atomic::AtomicUsize::new(0),
            delay: None,
        }
    }

    fn delayed(content: impl Into<String>, delay: std::time::Duration) -> Self {
        Self {
            content: content.into(),
            request_texts: Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicUsize::new(0),
            active: std::sync::atomic::AtomicUsize::new(0),
            peak_active: std::sync::atomic::AtomicUsize::new(0),
            delay: Some(delay),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn peak_active(&self) -> usize {
        self.peak_active.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn request_texts(&self) -> Vec<String> {
        self.request_texts.lock().unwrap().clone()
    }
}

#[async_trait]
impl LLMProvider for RepeatingProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn display_name(&self) -> &str {
        "Repeating Mock"
    }
    fn base_url(&self) -> &str {
        "mock://mesh-repeat"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> StdHashMap<String, String> {
        StdHashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        self.request_texts.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let active = self
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.peak_active
            .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        Ok(CompletionResponse {
            content: self.content.clone(),
            model: request.model,
            usage: TokenUsage::new(1, 1),
            reasoning: None,
            stop_reason: Some("stop".to_string()),
            tool_calls: vec![],
            provider_replay: None,
        })
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    async fn stream(&self, _request: CompletionRequest) -> Result<StreamingResponse> {
        unimplemented!("mesh tests do not stream")
    }
    async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        unimplemented!("mesh tests do not embed")
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn volume_workers_run_independent_jobs_concurrently_and_return_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let state_root = dir.path().join(".phoenix");
    std::fs::create_dir(&state_root).unwrap();
    let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
    let provider = Arc::new(RepeatingProvider::delayed(
        final_envelope("item complete", "Independent item result."),
        std::time::Duration::from_millis(45),
    ));
    let recorder = Arc::clone(&provider);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let runner = mesh_runner(provider, dir.path()).with_event_channel(event_tx);
    let jobs = (0..5)
        .map(|index| crate::tools::VolumeJobInput {
            id: format!("item-{index}"),
            task: format!("Process independent item {index}"),
            context: None,
            expected_output: Some("One concise result".to_string()),
        })
        .collect();

    let result = runner
        .run_volume_batch(
            &AgentAddress::Orchestrator,
            crate::tools::VolumeWorkInput {
                objective: "Process a bounded batch".to_string(),
                jobs,
                max_concurrency: Some(3),
            },
            "",
        )
        .await
        .unwrap();

    assert_eq!(result.requested, 5);
    assert_eq!(result.completed, 5);
    assert_eq!(result.failed, 0);
    assert_eq!(
        result
            .results
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["item-0", "item-1", "item-2", "item-3", "item-4"]
    );
    assert!(recorder.peak_active() >= 2, "jobs did not overlap");
    assert!(
        recorder.peak_active() <= 3,
        "configured ceiling was exceeded"
    );
    let session_entries = std::fs::read_dir(state_root.join("sessions"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        session_entries
            .iter()
            .all(|name| !name.contains("volume_worker--volume-")),
        "ephemeral volume-worker sessions leaked: {session_entries:?}"
    );
    let profile_root = state_root.join("browser/profiles");
    let profile_entries = std::fs::read_dir(profile_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(profile_entries, 0, "ephemeral browser profiles leaked");
    drop(runner);
    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    assert!(
        events.iter().all(|event| !matches!(event, CliEvent::SpecialistDelegated { agent, .. } if agent.contains("volume_worker") || agent.starts_with("Worker"))),
        "anonymous batch items leaked into the durable coworker handoff lifecycle: {events:?}"
    );
    let started = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                CliEvent::VolumeWorkerLifecycle {
                    batch_id,
                    status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
                    ..
                } if !batch_id.is_empty()
            )
        })
        .count();
    let completed = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                CliEvent::VolumeWorkerLifecycle {
                    status: crate::runtime::VolumeWorkerLifecycleStatus::Completed,
                    ..
                }
            )
        })
        .count();
    assert_eq!(started, 5, "every item needs one live worker-start signal");
    assert_eq!(
        completed, 5,
        "every completed item must settle its live chip"
    );
}

#[tokio::test]
async fn volume_workers_receive_parent_completion_state_before_stale_item_context() {
    let dir = tempfile::tempdir().unwrap();
    let state_root = dir.path().join(".phoenix");
    std::fs::create_dir(&state_root).unwrap();
    let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
    let provider = Arc::new(RepeatingProvider::new(final_envelope(
        "state reconciled",
        "Section 2 is next.",
    )));
    let recorder = Arc::clone(&provider);
    let runner = mesh_runner(provider, dir.path());
    let jobs = ["science", "planner"]
        .into_iter()
        .map(|id| crate::tools::VolumeJobInput {
            id: id.to_string(),
            task: "Determine the next science work".to_string(),
            context: Some("Lesson 3 is available in Moodle.".to_string()),
            expected_output: None,
        })
        .collect();

    runner
        .run_volume_batch(
            &AgentAddress::Orchestrator,
            crate::tools::VolumeWorkInput {
                objective: "Reconcile the school plan".to_string(),
                jobs,
                max_concurrency: Some(2),
            },
            "=== AUTHORITATIVE STATE EVIDENCE — newest first ===\n- [user] I finished Science Lessons 3 and 4.\n- [verified tool evidence] Section 1 Assignment marked complete; next Science is Section 2.",
        )
        .await
        .unwrap();

    let requests = recorder.request_texts();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let state = request
            .find("PARENT AUTHORITATIVE STATE SNAPSHOT")
            .expect("worker prompt must contain parent state");
        let stale = request
            .find("ITEM CONTEXT")
            .expect("worker prompt must retain item evidence");
        assert!(
            state < stale,
            "authoritative state must precede stale item context"
        );
        assert!(request.contains("I finished Science Lessons 3 and 4"));
        assert!(request.contains("next Science is Section 2"));
        assert!(request.contains("Available/unlocked content is not evidence"));
    }
}

#[tokio::test]
async fn aborting_a_volume_batch_settles_every_ephemeral_worker_chip() {
    let dir = tempfile::tempdir().unwrap();
    let state_root = dir.path().join(".phoenix");
    std::fs::create_dir(&state_root).unwrap();
    let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
    let session_id = format!("volume-cancel-{}", uuid::Uuid::new_v4());
    let provider: Arc<dyn LLMProvider> = Arc::new(RepeatingProvider::delayed(
        final_envelope("late item", "This must be cancelled before completion."),
        std::time::Duration::from_secs(30),
    ));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let runner =
        mesh_runner_with_session(provider, dir.path(), &session_id).with_event_channel(event_tx);
    let jobs = ["first", "second"]
        .into_iter()
        .map(|id| crate::tools::VolumeJobInput {
            id: id.to_string(),
            task: format!("Run independent item {id}"),
            context: None,
            expected_output: None,
        })
        .collect::<Vec<_>>();
    let task = tokio::spawn(async move {
        runner
            .run_volume_batch(
                &AgentAddress::Orchestrator,
                crate::tools::VolumeWorkInput {
                    objective: "Exercise cancellation cleanup".to_string(),
                    jobs,
                    max_concurrency: Some(2),
                },
                "",
            )
            .await
    });

    let mut started = 0usize;
    while started < 2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv())
            .await
            .expect("worker start event timed out")
            .expect("volume event channel closed before starts");
        crate::runtime::postbox::forward(&session_id, event.clone());
        if matches!(
            event,
            CliEvent::VolumeWorkerLifecycle {
                status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
                ..
            }
        ) {
            started += 1;
        }
    }
    let (replay_tx, _replay_rx) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        crate::runtime::postbox::subscribe_journal(&session_id, replay_tx)
            .into_iter()
            .filter(|event| matches!(
                &event.event,
                CliEvent::VolumeWorkerLifecycle {
                    status: crate::runtime::VolumeWorkerLifecycleStatus::Started,
                    ..
                }
            ))
            .count(),
        2,
        "the reconnect state must show both live workers before cancellation"
    );

    task.abort();
    assert!(task.await.is_err(), "parent batch task should be aborted");

    let mut cancelled = 0usize;
    while cancelled < 2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv())
            .await
            .expect("worker cancellation event timed out")
            .expect("volume event channel closed before cancellation cleanup");
        crate::runtime::postbox::forward(&session_id, event.clone());
        if matches!(
            event,
            CliEvent::VolumeWorkerLifecycle {
                status: crate::runtime::VolumeWorkerLifecycleStatus::Cancelled,
                ..
            }
        ) {
            cancelled += 1;
        }
    }
    let (after_tx, _after_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        crate::runtime::postbox::subscribe_journal(&session_id, after_tx)
            .into_iter()
            .all(|event| !matches!(event.event, CliEvent::VolumeWorkerLifecycle { .. })),
        "cancelled workers must be removed from reconnect state"
    );
}

fn talk_envelope(to: &str, subject: &str, body: &str) -> String {
    serde_json::json!({
        "type": "tool_request",
        "rationale": format!("delegating to {to}"),
        "tool_calls": [{
            "tool_name": "talk",
            "input": { "to": to, "subject": subject, "body": body, "mode": 1 }
        }]
    })
    .to_string()
}

fn no_reply_talk_envelope(to: &str, subject: &str, body: &str) -> String {
    serde_json::json!({
        "type": "tool_request",
        "rationale": format!("reporting to {to}"),
        "tool_calls": [{
            "tool_name": "talk",
            "input": { "to": to, "subject": subject, "body": body, "mode": 2 }
        }]
    })
    .to_string()
}

#[tokio::test]
async fn talk_to_busy_specialist_injects_without_flag_or_duplicate_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = format!("mesh-universal-talk-{}", uuid::Uuid::new_v4());
    crate::runtime::postbox::job_started(&session_id, "coder", "existing implementation");
    let _live_coder = crate::runtime::postbox::active_turn_guard(&session_id, "coder");
    let brief = "Join this correction to the implementation already in progress. Preserve the active working set, stop if the user asks to stop, and report the partial state through the existing background return rather than starting another coder instance.";
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            talk_envelope("coder", "additional user direction", brief),
            final_envelope("message joined", "## Done\nThe running coder received it."),
        ],
    )]));
    let runner = MeshRunner::new(
        provider,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "add this direction to the coder already running",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(
        crate::runtime::postbox::active_count(&session_id, "coder"),
        1
    );
    let notes = crate::runtime::postbox::take_steer(&session_id, "coder");
    assert_eq!(notes.len(), 1, "the message joins the existing turn");
    assert_eq!(notes[0].subject, "additional user direction");
    assert_eq!(notes[0].body, brief);
}

#[test]
fn steer_routing_covers_spawn_start_without_treating_job_label_as_live_turn() {
    let session_id = format!("mesh-steer-truth-{}", uuid::Uuid::new_v4());
    crate::runtime::postbox::job_started(&session_id, "coder", "root coder job");

    assert!(
        !turn_loop::specialist_accepts_steer(&session_id, "coder"),
        "a persistent root-job label is not proof that coder owns the live turn"
    );
    let starting = crate::runtime::postbox::starting_turn_guard(&session_id, "coder", "coder");
    assert!(
        turn_loop::specialist_accepts_steer(&session_id, "coder"),
        "the durable start marker must bridge the pre-ActiveTurnGuard race"
    );
    {
        let _guard = crate::runtime::postbox::active_turn_guard_from_start(
            &session_id,
            "coder",
            Some(&starting),
        );
        assert!(turn_loop::specialist_accepts_steer(&session_id, "coder"));
        assert!(
            !crate::runtime::postbox::agent_turn_starting(&session_id, "coder"),
            "live registration atomically consumes the exact start marker"
        );
    }
    assert!(!turn_loop::specialist_accepts_steer(&session_id, "coder"));

    crate::runtime::postbox::job_finished(
        &session_id,
        crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: String::new(),
            causation_id: None,
            agent: "coder".to_string(),
            subject: "root coder job".to_string(),
            ok: true,
            summary: "done".to_string(),
            body: "done".to_string(),
            finished: chrono::Utc::now(),
        },
    );
    let _ = crate::runtime::postbox::take_ready(&session_id);
}

#[tokio::test]
async fn spawn_start_stays_steerable_across_parent_rounds_until_child_is_active() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = format!("mesh-durable-start-{}", uuid::Uuid::new_v4());
    // Keep the detached child before ActiveTurnGuard deterministically. This
    // lets the parent complete several provider rounds without relying on
    // scheduler timing to reproduce the spawn gap.
    let held_lane = lanes::agent_lane_lock(&session_id, "coder")
        .lock_owned()
        .await;
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                no_reply_talk_envelope(
                    "coder",
                    "initial implementation",
                    "Start the implementation.",
                ),
                talk_envelope("coder", "first correction", "Apply correction alpha."),
                talk_envelope("coder", "second correction", "Apply correction beta."),
                final_envelope(
                    "work continues",
                    "## Result\nCoder has the initial task and both corrections.",
                ),
            ],
        ),
        (
            "Coder",
            vec![final_envelope(
                "implementation complete",
                "## Result\nApplied alpha and beta.",
            )],
        ),
    ]));
    let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::subscribe(&session_id, sub_tx);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "implement this and incorporate my immediate corrections",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(
        crate::runtime::postbox::active_count(&session_id, "coder"),
        1,
        "later parent rounds must steer, never duplicate the pending spawn"
    );
    assert!(crate::runtime::postbox::agent_turn_starting(
        &session_id,
        "coder"
    ));
    assert!(!crate::runtime::postbox::agent_turn_active(
        &session_id,
        "coder"
    ));
    assert!(crate::runtime::postbox::has_pending_steer(
        &session_id,
        "coder"
    ));

    drop(held_lane);
    let (agent, ok, body) = await_background_return(&mut sub_rx).await;
    assert_eq!(agent, "coder");
    assert!(ok, "child should finish after acquiring the lane: {body}");
    assert!(!crate::runtime::postbox::agent_turn_starting(
        &session_id,
        "coder"
    ));
    assert!(!crate::runtime::postbox::agent_turn_active(
        &session_id,
        "coder"
    ));
    assert_eq!(
        crate::runtime::postbox::active_count(&session_id, "coder"),
        0
    );
    assert!(!crate::runtime::postbox::has_pending_steer(
        &session_id,
        "coder"
    ));

    let coder_raw = std::fs::read_to_string(
        dir.path()
            .join(".phoenix/sessions")
            .join(format!("{session_id}__coder.json")),
    )
    .unwrap();
    assert!(coder_raw.contains("Apply correction alpha."), "{coder_raw}");
    assert!(coder_raw.contains("Apply correction beta."), "{coder_raw}");
}

/// Subscribe to a session's postbox and block until the next mode-2
/// background return lands.
async fn await_background_return(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<CliEvent>,
) -> (String, bool, String) {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("background job never returned")
            .expect("subscription closed");
        if let CliEvent::BackgroundAgentReturned {
            agent, ok, body, ..
        } = event
        {
            return (agent, ok, body);
        }
    }
}

fn final_envelope(summary: &str, markdown: &str) -> String {
    serde_json::json!({
        "type": "final",
        "summary": summary,
        "final_markdown": markdown,
        "changes_made": [],
        "verification": ["test"],
        "execution_mode": "test"
    })
    .to_string()
}

fn tool_envelope(tool_name: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "type": "tool_request",
        "rationale": format!("running {tool_name} for the task"),
        "tool_calls": [{ "tool_name": tool_name, "input": input }]
    })
    .to_string()
}

fn multi_tool_envelope(calls: Vec<(&str, serde_json::Value)>) -> String {
    let tool_calls: Vec<serde_json::Value> = calls
        .into_iter()
        .map(|(name, input)| serde_json::json!({ "tool_name": name, "input": input }))
        .collect();
    serde_json::json!({
        "type": "tool_request",
        "rationale": "batched independent reads in one round",
        "tool_calls": tool_calls
    })
    .to_string()
}

#[tokio::test]
async fn native_file_inspection_reaches_next_request_before_finish() {
    for native_tools in [false, true] {
        for references in [vec![], vec!["reference.png"], vec!["reference.png","other.png"]] {
        for crop in [
            serde_json::Value::Null,
            serde_json::json!({"x":8,"y":3,"width":4,"height":6}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            image::RgbImage::from_pixel(32, 24, image::Rgb([25, 140, 80]))
                .save(dir.path().join("sample.png"))
                .unwrap();
            for (name,color) in [("reference.png",[220,10,10]),("other.png",[10,10,220])] {
                image::RgbImage::from_pixel(32,24,image::Rgb(color)).save(dir.path().join(name)).unwrap();
            }
            let batch = serde_json::json!({
        "type": "tool_request", "rationale": "inspect the supplied file",
        "tool_calls": [
            {"tool_name":"image_analyze", "input":{"path":"sample.png","question":"Describe the color","crop":crop.clone(),"reference_paths":references.clone()}},
            {"tool_name":"image_analyze", "input":{"path":"sample.png","question":"Duplicate capture"}},
            {"tool_name":"final_answer", "input":{"summary":"premature", "markdown":"premature"}}
        ]
    }).to_string();
            let mut scripted = ScriptedProvider::new(vec![(
                "Orchestrator",
                vec![batch, final_envelope("observed", "Image received.")],
            )]);
            scripted.native_images = true;
            scripted.native_tools = native_tools;
            let provider = Arc::new(scripted);
            let mut spec = orchestrator_spec();
            spec.tool_allowlist.push("image_analyze".into());
            let runner = MeshRunner::new(
                provider.clone(),
                dir.path().to_path_buf(),
                dir.path().join(".phoenix"),
                format!("native-file-test-{}", uuid::Uuid::new_v4()),
                spec,
            )
            .with_native_vision(true);
            let mut gateway = Gateway::new(runner);
            gateway.submit(AgentMessage::user_input(
                AgentAddress::Orchestrator,
                "Read sample.png and describe its color.",
            ));
            let outcome = gateway.run().await;
            assert_eq!(*provider.request_image_counts.lock().unwrap(), vec![0, 1 + references.len()]);
            let requests = provider.request_texts.lock().unwrap();
            assert!(
                requests[1].contains("This batch was generated before those pixels were delivered")
            );
            // A second file inspection in the same batch now runs; the first
            // set stays committed in the append-only history.
            assert!(!requests[1]
                .contains("This call was not executed; the first image remains attached"));
            assert_eq!(outcome.user_messages.len(), 1);
            assert!(requests[1].contains("Original image dimensions: 32x24 pixels."));
            for reference in &references { assert!(requests[1].contains(reference)); }
            assert!(requests[1].contains("Attached image order: 1 = primary image"));
            assert!(!requests[1].contains("VISUAL QUALITY CHECK:"), "read-only inspection must not become an artifact repair task");
            if !crop.is_null() {
                assert!(requests[1].contains("x=8, y=3, width=4, height=6"));
            }
        }
    }
    }
}

#[tokio::test]
async fn plain_creation_gets_visual_feedback_after_actual_artifact_mutation() {
    // The real plain-request run never received its existing review guidance:
    // the call site had tied it to the unrelated substantial-workflow classifier.
    for native_tools in [false, true] {
        for request in [
            "Make me a realistic banana in Blender using a reference. Save the editable project and a polished render.",
            "Make me a cover image.",
            "Continue the existing task using its saved work and original requirements.",
            "Review sample.png; save your findings to notes.md; do not modify the artifact.",
        ] {
            let substantial = crate::runtime::build_contract::BuildContractGuard::new(request).requires_durable_goal();
            // Direct "Make … in Blender" is now correctly recognized. Keep
            // the original real prompt in this integration test, while still
            // exercising authoring-activated feedback for unclassified cover,
            // continuation and scoped review requests.
            assert_eq!(substantial, request.contains("in Blender"));
            let dir = tempfile::tempdir().unwrap();
            image::RgbImage::from_pixel(32, 24, image::Rgb([25, 140, 80]))
                .save(dir.path().join("sample.png")).unwrap();
            let original_pixels = std::fs::read(dir.path().join("sample.png")).unwrap();
            let notes_only = request.starts_with("Review");
            let mut sequence = Vec::new();
            if substantial {
                sequence.push(tool_envelope("todo_write", serde_json::json!({"todos":[
                    {"task":"Prepare assignment output", "completed":false},
                    {"task":"Inspect actual output pixels", "completed":false},
                    {"task":"Report unfinished visual criteria", "completed":false}
                ]})));
            }
            sequence.extend([
                tool_envelope("write", serde_json::json!({"path":if notes_only {"notes.md"} else {"draft.txt"},"content":"saved assignment output"})),
                tool_envelope("image_analyze", serde_json::json!({"path":"sample.png","question":"Confirm the final output"})),
                final_envelope("observed", "The observed output still needs revision."),
            ]);
            let mut scripted = ScriptedProvider::new(vec![("Orchestrator", sequence)]);
            scripted.native_images = true;
            scripted.native_tools = native_tools;
            let provider = Arc::new(scripted);
            let mut spec = orchestrator_spec();
            spec.tool_allowlist.extend(["write", "image_analyze"].map(str::to_owned));
            if substantial { spec.tool_allowlist.push("todo_write".into()); }
            spec.permissions.can_write_files = true;
            let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), dir.path().join(".phoenix"),
                format!("plain-visual-{}", uuid::Uuid::new_v4()), spec).with_native_vision(true);
            let mut gateway = Gateway::new(runner);
            gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, request));
            let outcome = gateway.run().await;
            assert_eq!(outcome.user_messages.len(), 1);
            let review_index = 2 + usize::from(substantial);
            let image_counts = provider.request_image_counts.lock().unwrap();
            assert!(image_counts.len() > review_index);
            assert!(image_counts[..review_index].iter().all(|count| *count == 0));
            assert_eq!(image_counts[review_index], 1, "the guidance accompanies actual pixels");
            if !substantial { assert_eq!(*image_counts, vec![0, 0, 1]); }
            let requests = provider.request_texts.lock().unwrap();
            let review = &requests[review_index];
            assert!(review.contains("VISUAL QUALITY CHECK:"), "plain request must receive existing guidance with its actual pixels: {request}");
            assert!(review.contains("A filename or inspection question calling this final is not evidence of completion"));
            if notes_only {
                assert!(review.contains(request), "original review scope remains in the actual request");
                assert!(review.contains("writing notes or a report does not authorize changing the pictured artifact"));
                assert!(review.contains("For review/report work, describe the observed findings without performing repairs"));
                assert_eq!(std::fs::read(dir.path().join("sample.png")).unwrap(), original_pixels);
            }
        }
    }
}

#[tokio::test]
async fn failed_image_comparison_clears_the_previous_pixels() {
    let dir = tempfile::tempdir().unwrap();
    image::RgbImage::new(12,12).save(dir.path().join("sample.png")).unwrap();
    std::fs::write(dir.path().join("broken.png"),"broken image").unwrap();
    let mut scripted=ScriptedProvider::new(vec![("Orchestrator",vec![
        tool_envelope("image_analyze",serde_json::json!({"path":"sample.png"})),
        tool_envelope("image_analyze",serde_json::json!({"path":"sample.png","reference_paths":["broken.png"]})),
        final_envelope("inspection failed","The reference image could not be read."),
    ])]);
    scripted.native_images=true;scripted.native_tools=true;
    let provider=Arc::new(scripted);
    let mut spec=orchestrator_spec();spec.tool_allowlist.push("image_analyze".into());
    let runner=MeshRunner::new(provider.clone(),dir.path().to_path_buf(),dir.path().join(".phoenix"),format!("comparison-failure-{}",uuid::Uuid::new_v4()),spec).with_native_vision(true);
    let mut gateway=Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator,"Inspect the sample and then compare its reference."));
    let outcome=gateway.run().await;
    assert_eq!(outcome.user_messages.len(),1);
    assert_eq!(*provider.request_image_counts.lock().unwrap(),vec![0,1,0]);
    assert!(provider.request_texts.lock().unwrap()[2].contains("Visual inspection has not occurred"));
}

#[tokio::test]
async fn explicit_references_survive_native_captures_in_outgoing_requests() {
    for native_tools in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let reference_path = dir.path().join("input.png");
        image::RgbImage::from_pixel(32, 24, image::Rgb([10, 220, 10])).save(&reference_path).unwrap();
        let reference = crate::runtime::vision::screenshot_data_uri(&reference_path).await.unwrap();
        let mut sequence = vec![tool_envelope("image_analyze", serde_json::json!({
            "path":"input.png", "reference_paths":["input.png", "input.png"]
        }))];
        let mut receipts = Vec::new();
        let mut captures = Vec::new();
        for index in 0..6u8 {
            let path = dir.path().join(format!("capture-{index}.png"));
            image::RgbImage::from_pixel(32, 24, image::Rgb([30 + index * 25, 10, 180])).save(&path).unwrap();
            captures.push(crate::runtime::vision::native_screen_data_uri(&path).await.unwrap());
            sequence.push(tool_envelope("computer_act", serde_json::json!({"actions":[{"type":"move","x":10 + index,"y":10}]})));
            receipts.push(ToolCallResult { tool_name:"computer_act".into(), input_summary:String::new(), success:true,
                output:format!("Fixture input delivered.\nScreenshot saved: {}", path.display()) });
        }
        sequence.push(tool_envelope("computer_act", serde_json::json!({"actions":[{"type":"key","combo":"enter"}]})));
        receipts.push(ToolCallResult { tool_name:"computer_act".into(), input_summary:String::new(), success:false,
            output:"Fixture input partially delivered; capture failed. Current state is unknown.".into() });
        sequence.push(tool_envelope("image_analyze", serde_json::json!({"path":"capture-0.png"})));
        sequence.push(final_envelope("inspected", "The capture failure remains recorded."));
        // Reopening the same session and inspecting another private peer must
        // never recover image authority by parsing paths from old text.
        sequence.push(final_envelope("new turn", "No image selected in this turn."));
        let mut scripted = ScriptedProvider::new(vec![("Orchestrator", sequence),
            ("Researcher", vec![final_envelope("private peer", "No private reference supplied.")])]);
        scripted.native_images = true;
        scripted.native_tools = native_tools;
        let provider = Arc::new(scripted);
        let mut spec = orchestrator_spec();
        spec.tool_allowlist.extend(["image_analyze", "computer_act"].map(str::to_owned));
        let session_id = format!("reference-lifetime-{}", uuid::Uuid::new_v4());
        let _fixture = super::turn_loop::native_image_fixtures::install(dir.path(), receipts);
        // Native fixtures replace execution, not the permission boundary. This
        // task explicitly authorizes computer use; Workspace would correctly
        // wait for an approval that this unattended fixture cannot provide.
        let make_runner = || MeshRunner::new(provider.clone(), dir.path().to_path_buf(), dir.path().join(".phoenix"),
            session_id.clone(), spec.clone()).with_native_vision(true)
            .with_permission_mode(PermissionMode::FullAccess);
        let original_brief = "Inspect the supplied image and native observations. Report the observed evidence.";
        let mut gateway = Gateway::new(make_runner());
        gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, original_brief));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), gateway.run())
            .await.expect("authorized native reference fixture did not finish");
        assert_eq!(outcome.user_messages.len(), 1);
        assert_eq!(*provider.request_image_counts.lock().unwrap(), vec![0,1,2,2,2,2,2,2,1,2]);
        {
            let messages = provider.request_image_messages.lock().unwrap();
            let wire = provider.request_image_wire.lock().unwrap();
            assert_eq!(messages[1][0].images, vec![reference.clone()], "self-reference and duplicate refs share one payload");
            for (index, capture) in captures.iter().enumerate() {
                let message = &messages[index + 2][0];
                assert_eq!(message.images, vec![capture.clone(), reference.clone()]);
                assert!(message.content.contains("current observation from computer_act"));
                assert!(message.content.contains("saved pixels explicitly selected by reference_paths"));
                assert!(message.content.contains("even if their source file has since changed"));
                assert_eq!(wire[index + 2].iter().map(|v| v["image_url"].as_str().unwrap()).collect::<Vec<_>>(),
                    vec![capture.as_str(), reference.as_str()], "actual provider adapter keeps the same image order");
                assert!(message.images.iter().map(String::len).sum::<usize>() <= 46 * 1024 * 1024);
            }
            assert_eq!(messages[8][0].images, vec![reference.clone()]);
            assert!(messages[8][0].content.contains("No current observation is attached"));
            assert!(provider.request_texts.lock().unwrap()[8].contains("capture failed"));
            assert_eq!(messages[9][0].images, vec![captures[0].clone(), reference]);
            assert!(provider.request_texts.lock().unwrap().iter().all(|text| text.contains(original_brief)));
        }
        let mut gateway = Gateway::new(make_runner());
        gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator,
            "A new message: [The user attached images to THIS message. image: input.png]"));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), gateway.run())
            .await.expect("reference-reset fixture did not finish");
        assert_eq!(outcome.user_messages.len(), 1);
        let mut gateway = Gateway::new(make_runner());
        gateway.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Researcher), "A separate private question."));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), gateway.run())
            .await.expect("private-peer isolation fixture did not finish");
        assert_eq!(outcome.user_messages.len(), 1);
        assert_eq!(&provider.request_image_counts.lock().unwrap()[10..], &[0,0]);
    }
}

#[tokio::test]
async fn failed_reference_replacement_preserves_only_original_reference_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut encoded = Vec::new();
    for (index, name) in ["primary.png", "ref-a.png", "ref-b.png", "replacement.png"].iter().enumerate() {
        let path = dir.path().join(name);
        image::RgbImage::from_pixel(24, 16, image::Rgb([20 + index as u8 * 50, 10, 160])).save(&path).unwrap();
        encoded.push(crate::runtime::vision::screenshot_data_uri(&path).await.unwrap());
    }
    std::fs::write(dir.path().join("broken.png"), b"not an image").unwrap();
    std::fs::write(dir.path().join("unsupported.txt"), b"not an image").unwrap();
    let mut scripted = ScriptedProvider::new(vec![("Orchestrator", vec![
        tool_envelope("image_analyze", serde_json::json!({"path":"primary.png","reference_paths":["ref-a.png","ref-b.png"]})),
        tool_envelope("image_analyze", serde_json::json!({"path":"replacement.png","reference_paths":["replacement.png","broken.png"]})),
        tool_envelope("image_analyze", serde_json::json!({"path":"primary.png","reference_paths":["unsupported.txt"]})),
        tool_envelope("image_analyze", serde_json::json!({"path":"primary.png"})),
        tool_envelope("image_analyze", serde_json::json!({"path":"primary.png","reference_paths":["ref-b.png"]})),
        final_envelope("observed", "The failed references were not inspected."),
    ])]);
    scripted.native_images = true;
    scripted.native_tools = true;
    let provider = Arc::new(scripted);
    let mut spec = orchestrator_spec();
    spec.tool_allowlist.push("image_analyze".into());
    let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), dir.path().join(".phoenix"),
        format!("reference-failure-{}", uuid::Uuid::new_v4()), spec).with_native_vision(true);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Inspect the requested comparisons."));
    assert_eq!(gateway.run().await.user_messages.len(), 1);
    // Round 3's set is identical to round 2's, so the append-only history
    // does not re-send it (it remains committed and visible).
    assert_eq!(*provider.request_image_counts.lock().unwrap(), vec![0,3,2,0,3,2]);
    let messages = provider.request_image_messages.lock().unwrap();
    for round in [2,3] {
        assert_eq!(messages[round][0].images, encoded[1..3]);
        assert!(messages[round][0].content.contains("No current observation is attached"));
        assert!(!messages[round][0].images.contains(&encoded[3]));
    }
    assert_eq!(messages[4][0].images, encoded[..3]);
    assert_eq!(messages[5][0].images, vec![encoded[0].clone(), encoded[2].clone()]);
    assert!(provider.request_texts.lock().unwrap()[2].contains("Visual inspection has not occurred"));
    assert!(provider.request_texts.lock().unwrap()[3].contains("unsupported image type"));
}

#[tokio::test]
async fn explicit_references_require_native_delivery_on_the_acting_turn() {
    let dir = tempfile::tempdir().unwrap();
    image::RgbImage::new(8, 8).save(dir.path().join("sample.png")).unwrap();
    let mut scripted = ScriptedProvider::new(vec![("Orchestrator", vec![
        tool_envelope("image_analyze", serde_json::json!({"path":"sample.png","reference_paths":["sample.png"]})),
        final_envelope("unavailable", "Native comparison was unavailable."),
    ])]);
    scripted.native_images = true;
    let provider = Arc::new(scripted);
    let mut spec = orchestrator_spec(); spec.tool_allowlist.push("image_analyze".into());
    let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), dir.path().join(".phoenix"),
        format!("no-native-reference-{}", uuid::Uuid::new_v4()), spec).with_native_vision(false);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Inspect the explicit comparison."));
    assert_eq!(gateway.run().await.user_messages.len(), 1);
    assert_eq!(*provider.request_image_counts.lock().unwrap(), vec![0,0]);
    assert!(provider.request_texts.lock().unwrap()[1].contains("requires native vision"));
}

#[tokio::test]
async fn artifact_only_turn_does_not_emit_failed_code_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let batch = serde_json::json!({"type":"tool_request","rationale":"save requested helper",
        "tool_calls":[{"tool_name":"write","input":{"path":"helper.py","content":"print(42)\n"}}]}).to_string();
    let provider = Arc::new(ScriptedProvider::new(vec![("Orchestrator", vec![batch, final_envelope("saved", "Saved helper.py.")])]));
    let mut spec = orchestrator_spec();
    spec.tool_allowlist.push("write".into());
    spec.permissions.can_write_files = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let runner = MeshRunner::new(provider, dir.path().to_path_buf(), dir.path().join(".phoenix"),
        format!("artifact-index-test-{}",uuid::Uuid::new_v4()),spec).with_event_channel(tx);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator,"Save helper.py containing print(42)."));
    let result = gateway.run().await;
    assert_eq!(result.user_messages.len(),1);
    assert_eq!(std::fs::read_to_string(dir.path().join("helper.py")).unwrap(),"print(42)\n");
    let mut writes = 0;
    while let Ok(event) = rx.try_recv() {
        if let CliEvent::ToolCallCompleted {tool_name, success, ..} = event {
            assert_ne!(tool_name,"index_codebase","non-project artifacts must not trigger automatic indexing");
            if tool_name=="write" && success { writes+=1; }
        }
    }
    assert_eq!(writes,1);
}

fn mesh_runner(provider: Arc<dyn LLMProvider>, root: &std::path::Path) -> MeshRunner {
    let session_id = format!("mesh-test-{}", uuid::Uuid::new_v4());
    mesh_runner_with_session(provider, root, &session_id)
}

fn mesh_runner_with_session(
    provider: Arc<dyn LLMProvider>,
    root: &std::path::Path,
    session_id: &str,
) -> MeshRunner {
    MeshRunner::new(
        provider,
        root.to_path_buf(),
        root.join(".phoenix"),
        session_id,
        orchestrator_spec(),
    )
}

#[test]
fn root_orchestrator_preserves_explicit_session_identity() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![])), dir.path(), "fresh-root-task");
    let mut store = SessionStore::new(dir.path().join("sessions"));
    let spec = orchestrator_spec();
    let (session, _) = runner.load_session(&mut store, &AgentAddress::Orchestrator, &spec).unwrap();
    assert_eq!(session.id, "fresh-root-task");
    assert!(session.messages.is_empty());
}

#[test]
fn owned_session_is_loaded_without_a_global_scan_and_corruption_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![])), dir.path(), "owned-history");
    let root = dir.path().join("sessions");
    let mut seed = SessionStore::new(&root);
    let mut old = Session::new_main_with_id("owned-history", "old-model", "old-system");
    old.push_message(Message::User { content: "Existing decision must survive".into() });
    seed.upsert(old);
    seed.save_one("owned-history").unwrap();
    let mut store = SessionStore::new(&root);
    let (loaded, previous) = runner.load_session(&mut store, &AgentAddress::Orchestrator, &orchestrator_spec()).unwrap();
    assert_eq!(previous.as_deref(), Some("old-model"));
    assert_eq!(loaded.messages.len(), 1);

    // No CAS exists for this intentionally corrupt history: do not overwrite it.
    std::fs::write(root.join("broken-owned-history.json"), b"broken history").unwrap();
    let broken_runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![])), dir.path(), "broken-owned-history");
    assert!(broken_runner.load_session(&mut SessionStore::new(&root), &AgentAddress::Orchestrator, &orchestrator_spec()).is_err());
    assert_eq!(std::fs::read(root.join("broken-owned-history.json")).unwrap(), b"broken history");
}

#[test]
fn context_window_caps_follow_the_agent_lane_without_globally_shrinking_coworkers() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner(Arc::new(ScriptedProvider::new(vec![])), dir.path())
        .with_context_window(400_000)
        .with_role_context_windows(HashMap::from([
            ("orchestrator".to_string(), 320_000),
            ("specialist".to_string(), 240_000),
            ("coder".to_string(), 128_000),
        ]));
    assert_eq!(
        runner.context_window_cap_for_addr(&AgentAddress::Orchestrator),
        Some(320_000)
    );
    assert_eq!(
        runner.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Coder)),
        Some(128_000)
    );
    assert_eq!(
        runner.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Researcher)),
        Some(240_000)
    );
    let uncapped = mesh_runner(Arc::new(ScriptedProvider::new(vec![])), dir.path())
        .with_context_window(400_000);
    assert_eq!(
        uncapped.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Coder)),
        None,
        "a main-lane legacy cap must not silently shrink every coworker's model"
    );

    let group_context = crate::runtime::group_conversation::GroupTurnContext {
        tool_constraints: Default::default(),
        inspection_participants: Default::default(),
        group_id: "bounded-room".to_string(),
        group_name: "Bounded room".to_string(),
        canonical_session_id: "group-bounded-room".to_string(),
        participants: vec![],
        discussion_rounds: 1,
        read_full_transcript: true,
        execution_dependencies: None,
        execution_waves: vec![],
    };
    let grouped = mesh_runner(Arc::new(ScriptedProvider::new(vec![])), dir.path())
        .with_context_window(500_000)
        .with_group_context(Some(group_context.clone()));
    assert_eq!(
        grouped.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Coder)),
        None,
        "group membership must not invent a smaller model context window"
    );
    let explicit = mesh_runner(Arc::new(ScriptedProvider::new(vec![])), dir.path())
        .with_context_window(500_000)
        .with_role_context_windows(HashMap::from([
            ("coder".to_string(), 1_050_000),
            ("researcher".to_string(), 128_000),
        ]))
        .with_group_context(Some(group_context));
    assert_eq!(explicit.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Coder)), Some(1_050_000));
    assert_eq!(explicit.context_window_cap_for_addr(&AgentAddress::Specialist(SubAgentType::Researcher)), Some(128_000));
    assert_eq!(explicit.context_window_cap_for_addr(&AgentAddress::Orchestrator), Some(500_000));
}

#[test]
fn group_shared_workspace_uses_only_canonical_room_context() {
    let dir = tempfile::tempdir().unwrap();
    let shared_workspace = dir.path().join("shared-workspace");
    let workspace = shared_workspace.display().to_string();
    let current_id = "group-privacy-current";
    let canonical_id = "group-privacy-canonical";

    let mut current = Session::new_main_with_id(current_id, "model", "system");
    current.workspace = Some(workspace.clone());
    current.push_message(Message::User {
        content: "Current room participant session".to_string(),
    });

    let mut canonical = Session::new_main_with_id(canonical_id, "model", "system");
    canonical.workspace = Some(workspace.clone());
    canonical.push_message(Message::User {
        content: "PUBLIC GROUP FACT: the launch window is Tuesday.".to_string(),
    });
    canonical.push_message(Message::Assistant {
        content: "The room acknowledged the public launch window.".to_string(),
    });

    let mut private_direct =
        Session::new_main_with_id("private-direct-conversation", "model", "system");
    private_direct.workspace = Some(workspace);
    private_direct.push_message(Message::User {
        content: "PRIVATE DIRECT SECRET: never disclose this salary note.".to_string(),
    });
    private_direct.push_message(Message::Assistant {
        content: "PRIVATE DIRECT SECRET remains only in this salary-note conversation.".to_string(),
    });

    let phoenix_direct_id = "own-direct-phoenix";
    let mut participant_direct = Session::new_main_with_id(phoenix_direct_id, "model", "system");
    participant_direct.workspace = Some(shared_workspace.display().to_string());
    participant_direct.push_message(Message::User {
        content: "OWN DIRECT FACT: Phoenix promised to preserve the compact room UI.".to_string(),
    });
    participant_direct.push_message(Message::Assistant {
        content: "I will carry that promise into my own group work without exposing anybody else's private thread."
            .to_string(),
    });

    let mut store = SessionStore::new(dir.path().join("sessions"));
    store.upsert(current.clone());
    store.upsert(canonical);
    store.upsert(private_direct);
    store.upsert(participant_direct);

    // This proves the fixture reproduces the original hazard: project brain
    // would include the other direct conversation solely because the folders
    // match.
    let unsafe_project_context =
        crate::runtime::project_brain::project_context_block(&current, &store, current_id)
            .expect("the shared workspace should produce sibling project context");
    assert!(
        unsafe_project_context.contains("PRIVATE DIRECT SECRET"),
        "fixture must exercise the project-brain leak path: {unsafe_project_context}"
    );

    let group_id = format!("privacy-regression-{}", uuid::Uuid::new_v4());
    let group_context = crate::runtime::group_conversation::GroupTurnContext {
        tool_constraints: Default::default(),
        inspection_participants: Default::default(),
        group_id,
        group_name: "Privacy regression room".to_string(),
        canonical_session_id: canonical_id.to_string(),
        participants: vec![crate::runtime::group_conversation::GroupParticipant {
            agent_id: "phoenix".to_string(),
            internal_role: "orchestrator".to_string(),
            display_name: "Phoenix".to_string(),
            role_title: "Coordinator".to_string(),
            color: "#d46a43".to_string(),
            icon_seed: "phoenix".to_string(),
            avatar: None,
            member_role: "coordinator".to_string(),
            history_access: crate::runtime::company_directory::HistoryAccess::Full,
            history_start_message_index: 0,
            explicitly_mentioned: true,
        }],
        discussion_rounds: 1,
        read_full_transcript: true,
        execution_dependencies: None,
        execution_waves: vec![vec!["phoenix".to_string()]],
    };
    let provider: Arc<dyn LLMProvider> = Arc::new(ScriptedProvider::new(vec![]));
    let runner = MeshRunner::new(
        provider,
        shared_workspace,
        dir.path().join("state"),
        current_id,
        orchestrator_spec(),
    )
    .with_group_context(Some(group_context.clone()))
    .with_group_direct_context_session("orchestrator", phoenix_direct_id);

    store.save_to_disk().unwrap();
    let mut scoped_store = SessionStore::new(store.root());
    runner.preload_group_histories(&AgentAddress::Orchestrator, &mut scoped_store).unwrap();
    assert_eq!(scoped_store.all().count(), 2, "only room and this member's own direct history");
    assert!(scoped_store.get(canonical_id).is_some());
    assert!(scoped_store.get(phoenix_direct_id).is_some());

    // Phoenix used to mutate the public room as its working session. Save a
    // real private tool checkpoint and prove that neither the room bytes nor
    // Phoenix's personal conversation are rewritten by that actor save.
    // Use the production invariant main_session_id == canonical room here;
    // the surrounding context-scoping fixture deliberately uses different ids.
    let checkpoint_runner = MeshRunner::new(
        Arc::new(ScriptedProvider::new(vec![])), dir.path().join("shared-workspace"),
        dir.path().to_path_buf(), canonical_id, orchestrator_spec(),
    ).with_group_context(Some(group_context.clone()));
    let room_path = scoped_store.session_path(canonical_id);
    let room_before = std::fs::read(&room_path).unwrap();
    let personal_path = scoped_store.session_path(phoenix_direct_id);
    let personal_before = std::fs::read(&personal_path).unwrap();
    let (mut working, _) = checkpoint_runner.load_session(&mut scoped_store, &AgentAddress::Orchestrator, &orchestrator_spec()).unwrap();
    assert_ne!(working.id, canonical_id);
    assert_ne!(working.id, phoenix_direct_id);
    let working_id = working.id.clone();
    working.push_message(Message::ToolResult {
        tool_name: "private_checkpoint".to_string(), input: "owned work".to_string(),
        success: true, output: "PRIVATE WORKING STATE".to_string(),
    });
    checkpoint_runner.save_session(&mut scoped_store, working).unwrap();
    assert_eq!(std::fs::read(&room_path).unwrap(), room_before);
    assert_eq!(std::fs::read(&personal_path).unwrap(), personal_before);
    let (resumed, _) = checkpoint_runner.load_session(&mut SessionStore::new(store.root()), &AgentAddress::Orchestrator, &orchestrator_spec()).unwrap();
    assert_eq!(resumed.id, working_id);
    assert!(resumed.messages.iter().any(|message| matches!(message, Message::ToolResult { output, .. } if output == "PRIVATE WORKING STATE")));

    let context = runner
        .cross_conversation_context_for_turn(
            &AgentAddress::Orchestrator,
            &current,
            &scoped_store,
            current_id,
            &[],
        )
        .expect("an explicitly mentioned member should receive canonical room context");
    assert!(context.contains("Canonical group transcript:"), "{context}");
    assert!(context.contains("PUBLIC GROUP FACT"), "{context}");
    assert!(
        context.contains("YOUR PRIVATE DIRECT-CONVERSATION CONTINUITY (Phoenix)")
            && context.contains("OWN DIRECT FACT"),
        "the participant must keep its own direct-thread continuity in the room: {context}"
    );
    assert!(
        !context.contains("PRIVATE DIRECT SECRET") && !context.contains("YOUR PROJECT THREADS"),
        "group context must not contain project-brain or private direct-session text: {context}"
    );

    // Runtime-bound receipts suppress only their own duplicate body. Text
    // matching is insufficient: two different results may have identical text.
    let mut with_results = scoped_store.get(canonical_id).unwrap().clone();
    for receipt in ["input-supplied", "input-not-supplied"] {
        with_results.push_message(Message::GroupContribution {
            turn_id: "test-turn".into(), message_id: receipt.into(),
            group_id: group_context.group_id.clone(), agent_id: "phoenix".into(),
            internal_role: "orchestrator".into(), display_name: "Phoenix".into(),
            role_title: "Coordinator".into(), color: String::new(), icon_seed: String::new(),
            avatar: None, subject: "Saved result".into(), body: "EXACT-RESULT-é🦊".into(),
            reply_to: None, causation_id: None,
        });
    }
    scoped_store.upsert(with_results);
    let full = runner.cross_conversation_context_for_turn(&AgentAddress::Orchestrator,
        &current, &scoped_store, current_id, &[]).unwrap();
    let deduplicated = runner.cross_conversation_context_for_turn(&AgentAddress::Orchestrator,
        &current, &scoped_store, current_id, &["input-supplied".into()]).unwrap();
    assert_eq!(full.matches("EXACT-RESULT-é🦊").count(), 2);
    assert_eq!(deduplicated.matches("EXACT-RESULT-é🦊").count(), 1);
    assert!(deduplicated.contains("PUBLIC GROUP FACT"));
    assert!(deduplicated.contains("OWN DIRECT FACT"));
    assert!(deduplicated.len() < full.len());
    assert_eq!(scoped_store.get(canonical_id).unwrap().messages.iter().filter(|m|
        matches!(m, Message::GroupContribution { .. })).count(), 2, "stored history must remain intact");

    // Disabling transcript reads must remain fail-closed. It must not make a
    // group turn fall back to project brain.
    let mut history_disabled = group_context;
    history_disabled.read_full_transcript = false;
    let provider: Arc<dyn LLMProvider> = Arc::new(ScriptedProvider::new(vec![]));
    let runner = MeshRunner::new(
        provider,
        dir.path().join("shared-workspace"),
        dir.path().join("state-disabled"),
        current_id,
        orchestrator_spec(),
    )
    .with_group_context(Some(history_disabled))
    .with_group_direct_context_session("orchestrator", phoenix_direct_id);
    let mut disabled_store = SessionStore::new(store.root());
    runner.preload_group_histories(&AgentAddress::Orchestrator, &mut disabled_store).unwrap();
    assert_eq!(disabled_store.all().count(), 1);
    assert!(disabled_store.get(canonical_id).is_none());
    let disabled_context = runner
        .cross_conversation_context_for_turn(
            &AgentAddress::Orchestrator,
            &current,
            &disabled_store,
            current_id,
            &[],
        )
        .expect("the participant's own direct continuity remains available");
    assert!(
        disabled_context.contains("OWN DIRECT FACT")
            && !disabled_context.contains("PUBLIC GROUP FACT")
            && !disabled_context.contains("PRIVATE DIRECT SECRET")
            && !disabled_context.contains("YOUR PROJECT THREADS"),
        "disabling room transcript reads must retain only the participant's own direct continuity: {disabled_context}"
    );
}

#[tokio::test]
async fn fast_malformed_mesh_responses_stop_after_bounded_repairs() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(RepeatingProvider::new(
        r#"{"type":"tool_request","tool_calls":BROKEN}"#,
    ));
    let recorder = Arc::clone(&provider);
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "answer without looping forever",
    ));

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), gateway.run())
        .await
        .expect("fast malformed replies must terminate deterministically");
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(
        recorder.calls(),
        turn_loop::MAX_PARSE_REPAIR_ATTEMPTS as usize + 1,
        "the initial invalid reply gets exactly three repair calls"
    );
    assert!(
        !outcome.user_messages[0].body.contains("tool_calls"),
        "a broken internal envelope must not leak to the user: {}",
        outcome.user_messages[0].body
    );
}

#[tokio::test]
async fn mesh_rejects_empty_parsed_and_placeholder_final_answer_then_repairs() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            final_envelope("empty", ""),
            tool_envelope(
                "final_answer",
                serde_json::json!({
                    "summary": "placeholder",
                    "final_markdown": "REPLACE_ME",
                    "changes_made": [],
                    "verification": []
                }),
            ),
            final_envelope(
                "repaired",
                "## Result\nThe real answer survived validation.",
            ),
        ],
    )]));
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "return a complete answer",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0]
        .body
        .contains("The real answer survived validation"));
    assert!(!outcome.user_messages[0].body.contains("REPLACE_ME"));
}

#[tokio::test]
async fn valid_mesh_tool_work_resets_consecutive_final_rejections() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let empty = final_envelope("empty", "");
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            empty.clone(),
            empty.clone(),
            tool_envelope("read", serde_json::json!({"path": "Cargo.toml"})),
            empty.clone(),
            tool_envelope(
                "final_answer",
                serde_json::json!({
                    "summary": "empty",
                    "final_markdown": "",
                    "changes_made": [],
                    "verification": []
                }),
            ),
            final_envelope("done", "## Result\nTool work reset the rejection streak."),
        ],
    )]));
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "read first if that helps, then answer",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(
        outcome.user_messages[0]
            .body
            .contains("Tool work reset the rejection streak"),
        "a valid tool request must break the rejection run: {}",
        outcome.user_messages[0].body
    );
}

#[test]
fn configured_boundary_preserves_a_completed_specialist_result() {
    let specialist_body =
        "Coder completed the requested backend fix and verified the focused regression.";

    let mut seeded =
        crate::session::Session::new_main_with_id("round-limit-helper", "mock-model", "system");
    seeded.push_message(crate::session::Message::Talk {
        from: "coder".to_string(),
        to: "orchestrator".to_string(),
        subject: "backend fix".to_string(),
        body: specialist_body.to_string(),
        reply_expected: false,
        handoff_id: "message_backend_fix".to_string(),
        reply_to: None,
        causation_id: None,
        status: "done".to_string(),
    });
    let fallback = turn_loop::economy_boundary_response(
        "Orchestrator",
        &AgentAddress::Orchestrator,
        &seeded,
        "the user-configured weekly hard stop was reached",
        &[],
    );
    assert_eq!(
        fallback.execution_mode,
        "configured_boundary_preserved_specialist_result"
    );
    assert_eq!(fallback.final_markdown, specialist_body);
}

#[tokio::test]
async fn dreamina_one_tool_replay_stops_at_the_semantic_repeat_guard() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let provider = Arc::new(RepeatingProvider::new(tool_envelope(
        "read",
        serde_json::json!({"path": "Cargo.toml"}),
    )));
    let recorder = Arc::clone(&provider);
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "Generate a video in Dreamina and return the downloaded file",
    ));

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), gateway.run())
        .await
        .expect("the Dreamina replay guard must terminate deterministically");
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(
        (4..=6).contains(&recorder.calls()),
        "the test must exercise retries, correction, and semantic abandonment"
    );
    assert!(
        outcome.user_messages[0]
            .body
            .contains("repeated the already-blocked `read` lane"),
        "the finish must name the semantic loop without claiming a runtime boundary: {}",
        outcome.user_messages[0].body
    );
    assert!(!outcome.user_messages[0]
        .body
        .contains("hard runtime boundary"));
}

#[tokio::test]
async fn cyclic_background_batons_stop_on_the_first_exact_repeat() {
    let dir = tempfile::tempdir().unwrap();
    // Explicit nested questions still reach the exact-cycle backstop.
    let question = |to: &str, subject: &str, body: &str| {
        let mut value: serde_json::Value = serde_json::from_str(&talk_envelope(to, subject, body)).unwrap();
        value["tool_calls"][0]["input"]["intent"] = serde_json::json!("question");
        value.to_string()
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Coder",
            vec![
                question("researcher", "cyclic baton", "send this back to coder"),
                question("researcher", "cyclic baton", "send this back to coder"),
            ],
        ),
        (
            "Researcher",
            vec![question(
                "coder",
                "cyclic baton",
                "send this back to researcher",
            )],
        ),
    ]));
    let runner = mesh_runner(provider, dir.path());
    let first = AgentMessage::talk(
        AgentAddress::Orchestrator,
        AgentAddress::Specialist(SubAgentType::Coder),
        "cyclic background job",
        "start the deliberately cyclic baton",
        true,
    );

    let job = runner
        .drive_background_with_limits(
            first,
            "coder",
            "cyclic background job",
            8,
            std::time::Duration::from_secs(15),
        )
        .await;
    assert!(!job.ok);
    assert!(
        job.body.contains("exact repeated background handoff"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("duplicate was not queued"),
        "{}",
        job.body
    );
    assert!(!job.body.contains("hard limit"), "{}", job.body);
}

#[tokio::test]
async fn self_addressed_background_baton_is_rejected_before_provider_work() {
    let dir = tempfile::tempdir().unwrap();
    // No script exists: a provider call would panic the test, proving this
    // invalid route is stopped at ingress rather than by a later turn limit.
    let runner = mesh_runner(Arc::new(ScriptedProvider::new(vec![])), dir.path());
    let first = AgentMessage::talk(
        AgentAddress::Specialist(SubAgentType::Coder),
        AgentAddress::Specialist(SubAgentType::Coder),
        "self background job",
        "do not start this",
        true,
    );

    let job = runner
        .drive_background_with_limits(
            first,
            "coder",
            "self background job",
            8,
            std::time::Duration::from_secs(1),
        )
        .await;
    assert!(!job.ok);
    assert!(
        job.body.contains("before any agent turn ran"),
        "{}",
        job.body
    );
    assert!(job.body.contains("cannot call itself"), "{}", job.body);
    assert!(!job.body.contains("hard limit"), "{}", job.body);
}

#[tokio::test]
async fn background_job_deadline_cancels_inflight_specialist_work() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(RepeatingProvider::delayed(
        final_envelope(
            "too late",
            "## Result\nThis provider response arrived after the job deadline.",
        ),
        std::time::Duration::from_secs(1),
    ));
    let runner = mesh_runner(provider, dir.path());
    let first = AgentMessage::talk(
        AgentAddress::Orchestrator,
        AgentAddress::Specialist(SubAgentType::Coder),
        "deadline test",
        "do not start after the deadline",
        true,
    );

    let job = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runner.drive_background_with_limits(
            first,
            "coder",
            "deadline test",
            10,
            std::time::Duration::from_millis(500),
        ),
    )
    .await
    .expect("the background deadline must cancel in-flight provider work");
    assert!(!job.ok);
    assert!(job.body.contains("whole-job deadline"), "{}", job.body);
    assert!(
        job.body.contains("0 agent turn(s) completed"),
        "{}",
        job.body
    );
}

#[cfg(not(feature = "cognee"))]
#[tokio::test]
async fn mesh_memory_save_records_failure_when_persistence_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            tool_envelope(
                "memory_save",
                serde_json::json!({"note": "The durable route is alpha."}),
            ),
            final_envelope("done", "## Result\nContinued after the failed memory save."),
        ],
    )]));
    let mut spec = orchestrator_spec();
    spec.tool_allowlist.push("memory_save".to_string());
    let runner = MeshRunner::new(
        provider,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        "mesh-memory-failure",
        spec,
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "save this note, then continue",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    let raw = std::fs::read_to_string(
        dir.path()
            .join(".phoenix/sessions/mesh-memory-failure.json"),
    )
    .unwrap();
    let session: crate::session::Session = serde_json::from_str(&raw).unwrap();
    let failure = session.messages.iter().find_map(|message| match message {
        crate::session::Message::ToolResult {
            tool_name,
            success,
            output,
            ..
        } if tool_name == "memory_save" => Some((*success, output.as_str())),
        _ => None,
    });
    let (success, output) = failure.expect("memory_save result persisted");
    assert!(!success);
    assert!(
        output.contains("Memory durability was not confirmed"),
        "{output}"
    );
    assert!(!output.contains("Remembered (team-wide)"), "{output}");
}

/// A provider whose every call fails — stands in for a turn that never
/// reaches `finalize` (the same end state as an Esc/abort: no normal save).
struct ErroringProvider;

#[async_trait]
impl LLMProvider for ErroringProvider {
    fn name(&self) -> &str {
        "erroring"
    }
    fn display_name(&self) -> &str {
        "Erroring"
    }
    fn base_url(&self) -> &str {
        "mock://erroring"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> StdHashMap<String, String> {
        StdHashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }
    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
        anyhow::bail!("provider unavailable")
    }
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    async fn stream(&self, _request: CompletionRequest) -> Result<StreamingResponse> {
        unimplemented!("erroring provider does not stream")
    }
    async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        unimplemented!("erroring provider does not embed")
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_turn_that_never_finalizes_still_keeps_the_user_message_in_context() {
    // Esc/abort and a hard provider failure share an end state: the turn
    // never reaches finalize. The user's message must already be on disk.
    let dir = tempfile::tempdir().unwrap();
    let session_id = "mesh-user-persist-session";
    let mut gateway = Gateway::new(mesh_runner_with_session(
        Arc::new(ErroringProvider),
        dir.path(),
        session_id,
    ));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "remember this exact line please",
    ));
    let _ = gateway.run().await; // turn fails before finalize

    let mut store = SessionStore::new(dir.path().join(".phoenix").join("sessions"));
    store.load_from_disk().unwrap();
    let main = store
        .get(session_id)
        .expect("main session was snapshotted at turn start");
    assert!(
        main.messages.iter().any(|m| matches!(
            m,
            Message::User { content } if content.contains("remember this exact line")
        )),
        "the user message survives a turn that never reached finalize"
    );
}

#[test]
fn record_incoming_does_not_double_record_a_prepersisted_user_message() {
    // The runner persists the user message to the durable session BEFORE the
    // librarian preload (so an early Esc can't lose it). When the mesh turn
    // then loads that session and calls record_incoming, it must NOT add a
    // second identical copy — but a genuinely new message still appends.
    use crate::session::Session;
    let mut session = Session::new_main("m", "sys");
    let addr = AgentAddress::Orchestrator;
    let msg = AgentMessage::user_input(addr.clone(), "check my twitter posts");

    // Simulate the runner's pre-persist: the message is already the tail.
    session.push_message(Message::User {
        content: "check my twitter posts".to_string(),
    });
    MeshRunner::record_incoming(&mut session, &addr, &msg);
    let users = session
        .messages
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    assert_eq!(users, 1, "pre-persisted user message must not be doubled");

    // A different message still records.
    let other = AgentMessage::user_input(addr.clone(), "and my reddit too");
    MeshRunner::record_incoming(&mut session, &addr, &other);
    let users = session
        .messages
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    assert_eq!(users, 2, "a new user message still appends");
}

#[test]
fn peer_handoff_mode_decides_where_the_reply_lands() {
    // Mode 1 is a real question and reports to its immediate sender. Mode 2 is
    // one-way: its recipient may work, but cannot create an acknowledgement
    // ping-pong by reporting back into another agent turn.
    let dir = tempfile::tempdir().unwrap();
    let coder = AgentAddress::Specialist(SubAgentType::Coder);
    let tester = AgentAddress::Specialist(SubAgentType::Tester);

    // mode 1 (reply expected): tester reports back to coder — chain stays local.
    let runner = mesh_runner(Arc::new(ErroringProvider), dir.path());
    let want_reply = AgentMessage::talk(coder.clone(), tester.clone(), "validate", "b", true);
    assert_eq!(runner.resolve_reply_to(&tester, &want_reply), coder);

    // mode 2 (no reply): closes once to the user/job sink, not the sender.
    let runner = mesh_runner(Arc::new(ErroringProvider), dir.path());
    let fire_forget = AgentMessage::talk(coder.clone(), tester.clone(), "validate", "b", false);
    assert_eq!(
        runner.resolve_reply_to(&tester, &fire_forget),
        AgentAddress::User
    );
}

#[test]
fn duplicate_reply_assignment_does_not_push_a_phantom_owner() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner(Arc::new(ErroringProvider), dir.path());
    let leo = AgentAddress::Specialist(SubAgentType::Coder);
    let theo = AgentAddress::Specialist(SubAgentType::Researcher);
    let mut incoming = AgentMessage::talk(leo.clone(), theo.clone(), "verify", "evidence", true);
    incoming.handoff_id = "one-assignment".into();
    assert_eq!(runner.resolve_reply_to(&theo, &incoming), leo);
    assert_eq!(runner.resolve_reply_to(&theo, &incoming), leo);
    runner.complete_reply_owner(&theo, &leo);
    assert!(runner.delegators.lock().unwrap().is_empty());
}

#[test]
fn nested_peer_request_restores_original_user_owner_after_reply() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner(Arc::new(ErroringProvider), dir.path());
    let leo = AgentAddress::Specialist(SubAgentType::Coder);
    let theo = AgentAddress::Specialist(SubAgentType::Researcher);
    assert_eq!(runner.resolve_reply_to(&leo, &AgentMessage::user_input(leo.clone(), "finish for me")), AgentAddress::User);
    assert_eq!(runner.resolve_reply_to(&theo, &AgentMessage::talk(leo.clone(), theo.clone(), "source check", "inspect", true)), leo);
    // A callee's mode-1 follow-up must not permanently replace the user's
    // ownership of the caller's original task.
    assert_eq!(runner.resolve_reply_to(&leo, &AgentMessage::talk(theo.clone(), leo.clone(), "follow-up", "confirm", true)), theo);
    runner.complete_reply_owner(&leo, &theo);
    let mut followup_answer = AgentMessage::talk(leo.clone(), theo.clone(), "answer", "confirmed", false);
    followup_answer.reply_to = Some("nested-question".into());
    assert_eq!(runner.resolve_reply_to(&theo, &followup_answer), leo);
    runner.complete_reply_owner(&theo, &leo);
    let mut source_answer = AgentMessage::talk(theo.clone(), leo.clone(), "source result", "verified", false);
    source_answer.reply_to = Some("source-check".into());
    assert_eq!(runner.resolve_reply_to(&leo, &source_answer), AgentAddress::User);
    runner.complete_reply_owner(&leo, &AgentAddress::User);
    assert!(runner.delegators.lock().unwrap().is_empty());
}

#[test]
fn private_runtime_role_prefix_is_removed_but_authored_brackets_survive() {
    assert_eq!(
        super::strip_private_speaker_tag("**[school_coach]** Yep.", "school_coach"),
        "Yep."
    );
    assert_eq!(
        super::strip_private_speaker_tag("[customer_id] stays", "school_coach"),
        "[customer_id] stays"
    );
}

#[tokio::test]
async fn a_read_only_round_runs_concurrently_and_all_results_land() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha contents").unwrap();
    std::fs::write(dir.path().join("b.txt"), "bravo contents").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                talk_envelope("coder", "read both files", "read a.txt and b.txt"),
                final_envelope("done", "## Result\ncoder read both"),
            ],
        ),
        (
            "Coder",
            vec![
                // Two independent reads in ONE round → concurrent fast-path.
                multi_tool_envelope(vec![
                    ("read", serde_json::json!({ "path": "a.txt" })),
                    ("read", serde_json::json!({ "path": "b.txt" })),
                ]),
                final_envelope("read both", "## Result\nread a.txt and b.txt"),
            ],
        ),
    ]));
    let session_id = format!("mesh-conc-test-{}", uuid::Uuid::new_v4());
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "read both files",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.steps, 3, "mode-1 work must finish before the owner");
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0].body.contains("coder read both"));

    // The concurrent batch must persist the same transcript the serial path
    // would: both reads, both successful, both outputs intact and in order.
    let mut store = SessionStore::new(dir.path().join(".phoenix").join("sessions"));
    store.load_from_disk().unwrap();
    let coder = store
        .get(&crate::session::specialist_session_id(
            &session_id,
            crate::session::SubAgentType::Coder,
        ))
        .expect("coder session persisted");
    let reads: Vec<&String> = coder
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult {
                tool_name,
                success,
                output,
                ..
            } if tool_name == "read" && *success => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(reads.len(), 2, "both reads landed: {reads:?}");
    assert!(reads[0].contains("alpha contents"), "{reads:?}");
    assert!(reads[1].contains("bravo contents"), "{reads:?}");
}

#[test]
fn group_ask_keeps_authored_turn_after_peer_return() {
    let dir = tempfile::tempdir().unwrap();
    let runner = mesh_runner(Arc::new(ErroringProvider), dir.path());
    let mut input = AgentMessage::user_input(AgentAddress::Orchestrator, "Coordinate this task");
    input.causation_id = Some("authored-room-turn".into());
    runner.remember_group_authored_turn(&input);
    let mut returned = AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Researcher), AgentAddress::Orchestrator, "Result", "Verified", false);
    returned.causation_id = Some("peer-result-message".into());
    runner.remember_group_authored_turn(&returned);
    assert_eq!(runner.group_authored_turn_id.lock().unwrap().as_deref(), Some("authored-room-turn"));
}

#[tokio::test]
async fn completed_report_talk_is_repaired_into_one_correlated_return() {
    let dir = tempfile::tempdir().unwrap();
    let sid = format!("mesh-return-intent-{}", uuid::Uuid::new_v4());
    let worker = AgentAddress::Specialist(SubAgentType::Researcher);
    let caller = AgentAddress::Specialist(SubAgentType::Coder);
    let runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![
        ("Researcher", vec![
            talk_envelope("coder", "Report complete", "The requested report is complete."),
            final_envelope("Report complete", "The requested report is complete."),
        ]),
    ])), dir.path(), &sid);
    let mut request = AgentMessage::talk(caller.clone(), worker.clone(), "Research", "Report the findings.", true);
    request.handoff_id = "original-report-request".into();
    let result = runner.run_turn(&worker, request).await;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].to, caller);
    assert!(matches!(result[0].kind, MessageKind::Talk { reply_expected: false }), "completion must not queue another question");
    assert_eq!(result[0].reply_to.as_deref(), Some("original-report-request"));
    assert_eq!(result[0].body, "The requested report is complete.");
    assert!(runner.delegators.lock().unwrap().is_empty());
}

#[tokio::test]
async fn suspended_reply_owner_survives_runner_recreation() {
    let dir = tempfile::tempdir().unwrap();
    let sid = format!("mesh-owner-recovery-{}", uuid::Uuid::new_v4());
    let researcher = AgentAddress::Specialist(SubAgentType::Researcher);
    let coder = AgentAddress::Specialist(SubAgentType::Coder);
    let runner = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![
        ("Researcher", vec![talk_envelope("coder", "verify", "Check the result and return it.")]),
    ])), dir.path(), &sid);
    let mut assignment = AgentMessage::talk(
        AgentAddress::Orchestrator, researcher.clone(), "research", "Get a code review, then report to me.", true,
    );
    assignment.handoff_id = "parent-research-assignment".into();
    let outgoing = runner.run_turn(&researcher, assignment).await;
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].to, coder);
    let handoff = outgoing[0].correlation_id().to_string();
    assert!(!handoff.is_empty());
    drop(runner);

    let resumed = mesh_runner_with_session(Arc::new(ScriptedProvider::new(vec![
        ("Researcher", vec![final_envelope("verified", "The reviewed result is ready.")]),
    ])), dir.path(), &sid);
    let mut answer = AgentMessage::talk(coder, researcher.clone(), "review complete", "Verified evidence.", false);
    answer.reply_to = Some(handoff.clone());
    answer.handoff_id = handoff;
    let result = resumed.run_turn(&researcher, answer).await;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].to, AgentAddress::Orchestrator,
        "a recreated runner must not bypass the original requester and answer the user");
    assert_eq!(result[0].body, "The reviewed result is ready.");
    assert_eq!(result[0].reply_to.as_deref(), Some("parent-research-assignment"),
        "the return must settle the parent's assignment, not its child code review");
    let mut disk = SessionStore::new(dir.path().join(".phoenix/sessions"));
    disk.load_from_disk().unwrap();
    let saved = disk.get(&crate::session::specialist_session_id(&sid, SubAgentType::Researcher)).unwrap();
    assert!(saved.reply_owners.is_empty(), "completion and owner pop share one durable save");
}

#[tokio::test]
async fn failed_resumed_assignment_retires_only_its_durable_reply_frame() {
    for panic_in_provider in [false, true] {
    let dir = tempfile::tempdir().unwrap();
    let sid = format!("mesh-terminal-failure-{}", uuid::Uuid::new_v4());
    let researcher = AgentAddress::Specialist(SubAgentType::Researcher);
    let provider: Arc<dyn LLMProvider> = if panic_in_provider {
        Arc::new(ScriptedProvider::new(vec![]))
    } else { Arc::new(ErroringProvider) };
    let runner = mesh_runner_with_session(provider, dir.path(), &sid);
    let mut store = SessionStore::new(dir.path().join(".phoenix/sessions"));
    let (spec, _) = runner.spec_for(&researcher).unwrap();
    let (mut session, _) = runner.load_session(&mut store, &researcher, &spec).unwrap();
    session.reply_owners = vec![
        crate::session::ReplyOwnerFrame { owner: "user".into(), handoff_id: "outer-user-task".into() },
        crate::session::ReplyOwnerFrame { owner: "orchestrator".into(), handoff_id: "failed-parent-task".into() },
    ];
    let session_id = session.id.clone();
    runner.save_session(&mut store, session).unwrap();
    let mut answer = AgentMessage::talk(AgentAddress::Specialist(SubAgentType::Coder), researcher.clone(), "result", "Child evidence", false);
    answer.reply_to = Some("child-review".into());
    answer.handoff_id = "child-review".into();
    let failed = runner.run_turn(&researcher, answer).await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].to, AgentAddress::Orchestrator);
    assert_eq!(failed[0].reply_to.as_deref(), Some("failed-parent-task"));
    let mut disk = SessionStore::new(dir.path().join(".phoenix/sessions"));
    disk.load_one_if_absent(&session_id).unwrap();
    let saved = disk.get(&session_id).unwrap();
    assert_eq!(saved.reply_owners.len(), 1);
    assert_eq!(saved.reply_owners[0].handoff_id, "outer-user-task");
    assert!(matches!(saved.messages.last(), Some(Message::Assistant { content }) if content == &failed[0].body));
    }
}

#[tokio::test]
async fn chain_routes_finals_back_up_to_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                talk_envelope(
                    "researcher",
                    "find X then hand to coder",
                    "research X; then talk to coder with findings",
                ),
                final_envelope("All done", "## Result\nfindings A,B integrated by coder"),
            ],
        ),
        (
            "Researcher",
            vec![
                talk_envelope("coder", "integrate findings", "wire up X using A, B"),
                final_envelope(
                    "research+integration done",
                    "findings A,B integrated by coder",
                ),
            ],
        ),
        (
            "Coder",
            vec![final_envelope(
                "integrated",
                "## Result\nintegrated A and B",
            )],
        ),
    ]));
    let session_id = format!("mesh-chain-test-{}", uuid::Uuid::new_v4());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    )
    .with_event_channel(event_tx);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "research X and integrate it",
    ));

    let outcome = gateway.run().await;

    // Every mode-1 leg is part of one foreground chain. The owner's final can
    // only happen after researcher and coder have returned.
    assert!(outcome.steps >= 3, "turns: {:?}", outcome.user_messages);
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(outcome.user_messages[0].from, AgentAddress::Orchestrator);
    assert!(outcome.user_messages[0]
        .body
        .contains("findings A,B integrated by coder"));
    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    let owner_settled = events
        .iter()
        .position(|event| {
            matches!(event,
                CliEvent::AgentHandoff { requester, receiver, status, .. }
                    if requester == "orchestrator" && receiver == "researcher" && status == "done"
            )
        })
        .expect("researcher return must settle the owner's handoff");
    assert!(
        owner_settled < events.len(),
        "the correlated settlement is emitted during owner resumption, before Gateway returns the final: {events:#?}"
    );

    // Every agent owns a durable session on disk.
    let sessions = dir.path().join(".phoenix").join("sessions");
    assert!(sessions.join(format!("{session_id}.json")).exists());
    assert!(sessions
        .join(format!("{session_id}__researcher.json"))
        .exists());
    assert!(sessions.join(format!("{session_id}__coder.json")).exists());

    // The researcher's transcript shows the full suspend/resume arc:
    // inbound talk, its own outbound talk, the coder's reply, its final.
    let researcher_raw =
        std::fs::read_to_string(sessions.join(format!("{session_id}__researcher.json"))).unwrap();
    assert!(researcher_raw.contains("find X then hand to coder"));
    assert!(researcher_raw.contains("integrate findings"));
    assert!(researcher_raw.contains("integrated A and B"));
}

#[tokio::test]
async fn root_coder_job_can_baton_to_researcher_and_back_to_coder() {
    // The postbox's RunningJob stays labelled `coder` for this whole chain.
    // That durable job label must not make the researcher's return baton look
    // like a steer to a live coder after the coder's first turn has ended.
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                no_reply_talk_envelope(
                    "coder",
                    "implement with research",
                    "Ask the researcher for one finding, then finish the implementation.",
                ),
                final_envelope(
                    "implementation started",
                    "## Result\nCoder is working in the background.",
                ),
            ],
        ),
        (
            "Coder",
            vec![
                talk_envelope(
                    "researcher",
                    "find the missing detail",
                    "Find the exact integration detail and send it back to coder.",
                ),
                final_envelope(
                    "implementation complete",
                    "## Result\nCoder integrated the research finding.",
                ),
            ],
        ),
        (
            "Researcher",
            vec![no_reply_talk_envelope(
                "coder",
                "research finding",
                "The exact integration detail is alpha-42.",
            )],
        ),
    ]));
    let session_id = format!("mesh-coder-baton-test-{}", uuid::Uuid::new_v4());
    let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::subscribe(&session_id, sub_tx);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "implement this after one research check",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.steps, 1);
    let (agent, ok, body) = await_background_return(&mut sub_rx).await;
    assert_eq!(agent, "coder");
    assert!(ok, "the coder-rooted baton chain should finish: {body}");
    assert!(body.contains("integrated the research finding"), "{body}");
    assert!(
        crate::runtime::postbox::take_steer(&session_id, "coder").is_empty(),
        "the researcher -> coder baton must not be orphaned as a steer"
    );

    let coder_raw = std::fs::read_to_string(
        dir.path()
            .join(".phoenix/sessions")
            .join(format!("{session_id}__coder.json")),
    )
    .unwrap();
    assert!(coder_raw.contains("alpha-42"));
}

#[tokio::test]
async fn orchestrator_no_reply_echo_to_reporting_specialist_finishes_to_user() {
    // The echo guard: an orchestrator woken by a specialist REPORT that
    // answers by talking BACK at the reporter gets converted to a user-facing
    // final instead of spawning a pointless new job at the specialist.
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![no_reply_talk_envelope(
            "coder",
            "final summary",
            "Keep the first-class tools; benchmark compression before redesign.",
        )],
    )]));
    let session_id = "mesh-echo-session";
    let mut gateway = Gateway::new(mesh_runner_with_session(provider, dir.path(), session_id));
    gateway.submit(AgentMessage::talk(
        AgentAddress::Specialist(SubAgentType::Coder),
        AgentAddress::Orchestrator,
        "coder findings",
        "The tools are first-class runtime tools.",
        false,
    ));

    let outcome = gateway.run().await;

    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(outcome.user_messages[0].from, AgentAddress::Orchestrator);
    assert!(outcome.user_messages[0]
        .body
        .contains("Keep the first-class tools"));

    let sessions = dir.path().join(".phoenix").join("sessions");
    let coder_session = sessions.join(format!("{session_id}__coder.json"));
    if coder_session.exists() {
        let coder_raw = std::fs::read_to_string(coder_session).unwrap();
        assert!(
            !coder_raw.contains("Keep the first-class tools"),
            "the orchestrator final must not be queued back into the coder session"
        );
    }
}

#[tokio::test]
async fn local_tools_execute_inline_and_land_in_the_session() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "phoenix-mesh-marker").unwrap();
    let read_then_final = vec![
        serde_json::json!({
            "type": "tool_request",
            "rationale": "read the file first",
            "tool_calls": [{ "tool_name": "read", "input": { "path": "notes.txt" } }]
        })
        .to_string(),
        final_envelope("read it", "## Result\nthe file says phoenix-mesh-marker"),
    ];
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        read_then_final,
    )]));
    let session_id = "mesh-local-tools-session";
    let mut gateway = Gateway::new(mesh_runner_with_session(provider, dir.path(), session_id));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "what does notes.txt say?",
    ));

    let outcome = gateway.run().await;

    // The read ran inline — one agent turn total, no extra wake.
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0]
        .body
        .contains("phoenix-mesh-marker"));

    let raw = std::fs::read_to_string(
        dir.path()
            .join(".phoenix")
            .join("sessions")
            .join(format!("{session_id}.json")),
    )
    .unwrap();
    assert!(raw.contains("phoenix-mesh-marker"));
    assert!(raw.contains("ToolResult"));
}

#[tokio::test]
async fn self_talk_is_rejected_with_feedback_and_the_turn_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            talk_envelope("orchestrator", "loop", "talking to myself"),
            final_envelope("recovered", "## Result\nanswered directly instead"),
        ],
    )]));
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "hello",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0].body.contains("answered directly"));
}

#[tokio::test]
async fn agent_can_message_the_user_directly() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
            (
                "Orchestrator",
                vec![
                    no_reply_talk_envelope(
                        "coder",
                        "fix it and tell the user yourself",
                        "fix X; report straight to the user when done",
                    ),
                    final_envelope("delegated", "## Result\ncoder is fixing X now"),
                ],
            ),
            (
                "Coder",
                vec![serde_json::json!({
                    "type": "tool_request",
                    "rationale": "done — telling the user directly",
                    "tool_calls": [{
                        "tool_name": "talk",
                        "input": { "to": "user", "subject": "fixed", "body": "X is fixed, tests green", "mode": 2 }
                    }]
                })
                .to_string()],
            ),
        ]));
    let session_id = format!("mesh-direct-test-{}", uuid::Uuid::new_v4());
    let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::subscribe(&session_id, sub_tx);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id,
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "fix X",
    ));

    let outcome = gateway.run().await;
    // The orchestrator answers immediately; the coder runs detached and its
    // user-addressed message rides the background return.
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0].body.contains("fixing X"));
    let (agent, ok, body) = await_background_return(&mut sub_rx).await;
    assert_eq!(agent, "coder");
    assert!(ok);
    assert!(body.contains("tests green"));
}

#[tokio::test]
async fn mode_two_orchestrator_talk_spawns_a_background_specialist() {
    // Mode-2 from the orchestrator = TRUE background spawn: the specialist
    // runs detached, the orchestrator's turn continues immediately, and the
    // result lands in the postbox (announced to subscribers) for injection
    // into a later orchestrator round/turn.
    let dir = tempfile::tempdir().unwrap();
    let session_id = format!("mesh-bg-test-{}", uuid::Uuid::new_v4());
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                serde_json::json!({
                    "type": "tool_request",
                    "rationale": "hand work to coder without waiting",
                    "tool_calls": [{
                        "tool_name": "talk",
                        "input": {
                            "to": "coder",
                            "subject": "background note",
                            "body": "Write a short background note and report back when done.",
                            "mode": 2
                        }
                    }]
                })
                .to_string(),
                final_envelope(
                    "spawned",
                    "## Result\ncoder is working on the note in the background",
                ),
            ],
        ),
        (
            "Coder",
            vec![final_envelope(
                "note done",
                "## Note\nbackground note completed",
            )],
        ),
    ]));
    let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::subscribe(&session_id, sub_tx);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "start background note",
    ));

    let outcome = gateway.run().await;
    // The gateway run is ONLY the orchestrator's turn — the coder is detached,
    // not a gateway step, and the user gets an immediate answer.
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(
        outcome.user_messages[0]
            .body
            .contains("working on the note"),
        "orchestrator must answer without waiting for the background job: {:?}",
        outcome.user_messages[0].body
    );

    // Subscribers see the spawn immediately and the colored return when the
    // detached task finishes.
    let spawned = tokio::time::timeout(std::time::Duration::from_secs(5), sub_rx.recv())
        .await
        .expect("no spawn event")
        .expect("subscription closed");
    assert!(
        matches!(&spawned, CliEvent::BackgroundAgentSpawned { agent, subject, .. }
            if agent == "coder" && subject == "background note"),
        "expected spawn event, got {spawned:?}"
    );
    let returned = loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), sub_rx.recv())
            .await
            .expect("background job never returned")
            .expect("subscription closed");
        if let CliEvent::BackgroundAgentReturned {
            agent, ok, body, ..
        } = event
        {
            break (agent, ok, body);
        }
    };
    assert_eq!(returned.0, "coder");
    assert!(returned.1, "background job should succeed");
    assert!(
        returned.2.contains("background note completed"),
        "return must carry the specialist's full result: {}",
        returned.2
    );
}

#[tokio::test]
async fn group_assignment_returns_to_owner_before_final_answer() {
    // CompanyStore's test singleton is process-wide. Seed this directory in
    // a child test process so legacy mesh fixtures keep their empty roster.
    if std::env::var_os("PHOENIX_GROUP_REPLY_FIXTURE").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::mesh::mesh_tests::group_assignment_returns_to_owner_before_final_answer", "--test-threads=1"])
            .env("PHOENIX_GROUP_REPLY_FIXTURE", "1")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
    for failed in [false, true] {
    let session_id = format!("group-reply-test-{}", uuid::Uuid::new_v4());
    let company = crate::runtime::company::global().unwrap();
    for profile in crate::runtime::company_directory::founding_team_profiles(true) {
        company.apply_directory_change("test", format!("seed-{}", profile.agent_id),
            crate::runtime::company_directory::DirectoryChange::AgentUpserted { profile }).unwrap();
    }
    let snapshot = crate::runtime::company::global().unwrap().directory_snapshot().unwrap();
    let participants = snapshot.agents.iter().filter(|agent|
        matches!(agent.profile.internal_role.as_str(), "phoenix" | "coder" | "critic")
    ).map(|agent| crate::runtime::group_conversation::GroupParticipant {
        agent_id: agent.profile.agent_id.clone(), internal_role: agent.profile.internal_role.clone(),
        display_name: agent.profile.display_name.clone(), role_title: "Coworker".into(),
        color: "#888888".into(), icon_seed: "test".into(), avatar: None,
        member_role: "member".into(), history_access: crate::runtime::company_directory::HistoryAccess::Full,
        history_start_message_index: 0, explicitly_mentioned: true,
    }).collect();
    let group = crate::runtime::group_conversation::GroupTurnContext {
        tool_constraints: Default::default(), inspection_participants: Default::default(),
        group_id: "reply-test".into(), group_name: "Reply test".into(),
        canonical_session_id: session_id.clone(), participants,
        discussion_rounds: 1, read_full_transcript: true,
        execution_dependencies: None, execution_waves: vec![],
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        ("Orchestrator", vec![
            serde_json::json!({"type":"tool_request","rationale":"delegate the report","tool_calls":[{"tool_name":"talk","input":{"to":"coder","subject":"report","body":"Return the report","mode":2}},{"tool_name":"talk","input":{"to":"critic","subject":"review","body":"Return the independent review","mode":2}}]}).to_string(),
            final_envelope("Report received", if failed { "The coworker could not finish the report." } else { "The coworker's report is ready." }),
        ]),
        ("Coder", vec![final_envelope("Report", "Verified report evidence.")]),
        ("Critic", vec![final_envelope("Review", "Independent review evidence.")]),
    ]));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(128);
    let runner = MeshRunner::new(provider.clone() as Arc<dyn LLMProvider>, dir.path().to_path_buf(),
        dir.path().join(".phoenix"), session_id.clone(), orchestrator_spec())
        .with_group_context(Some(group))
        .with_specialist_provider(if failed { Some(Arc::new(ErroringProvider)) } else { None })
        .with_event_channel(event_tx);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Get the report"));
    let outcome = gateway.run().await;
    assert_eq!(outcome.steps, 4, "both coworkers must return before one owner resumption: {:?}", outcome.user_messages);
    assert_eq!(outcome.user_messages.len(), 1);
    assert_eq!(outcome.user_messages[0].from, AgentAddress::Orchestrator);
    assert!(outcome.user_messages[0].body.contains(if failed { "could not finish" } else { "report is ready" }));
    let mut returned = false;
    while let Ok(event) = event_rx.try_recv() {
        if let CliEvent::AgentHandoff { body: Some(_), status, .. } = event {
            assert_eq!(status, if failed { "blocked" } else { "done" });
            returned = true;
        }
    }
    assert!(returned, "the UI must receive the correlated terminal receipt");
    if !failed {
        let requests = provider.request_texts.lock().unwrap();
        let last = requests.last().unwrap();
        assert!(last.contains("Verified report evidence."), "first return must reach the owner");
        assert!(last.contains("Independent review evidence."), "second return must reach the same owner request");
    }
    if failed {
        let mut store = SessionStore::new(dir.path().join(".phoenix/sessions"));
        store.load_from_disk().unwrap();
        assert!(store.all().any(|session| session.messages.iter().any(|message|
            matches!(message, Message::Talk { status, subject, .. } if status == "blocked" && subject.contains("turn failed"))
        )), "restart history must retain the blocked return");
    }
    assert!(!crate::runtime::postbox::has_background_work(&session_id));
    }
}

#[tokio::test]
async fn unavailable_memory_recall_fails_honestly_and_the_turn_continues() {
    // memory_recall is intercepted by the mesh (async graph search) — never
    // the sync executor. Unit tests are isolated from the live memory store,
    // so the tool must report an unavailable index (not a false zero-hit) and
    // the turn can still continue to a final from live sources.
    // Hold the crate-wide environment lease for the whole async turn. An
    // unrelated opt-in test must not redirect this query to its temporary
    // Cognee store between provider rounds.
    let _home_guard = crate::config::test_env::PhoenixHomeGuard::unset();
    let dir = tempfile::tempdir().unwrap();
    let session_id = format!("mesh-recall-test-{}", uuid::Uuid::new_v4());
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![
            serde_json::json!({
                "type": "tool_request",
                "rationale": "not sure — search memory first",
                "tool_calls": [{
                    "tool_name": "memory_recall",
                    "input": { "query": "how was the cookie bug fixed" }
                }]
            })
            .to_string(),
            final_envelope("done", "## Result\nanswered from live sources"),
        ],
    )]));
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "what fixed the cookie bug?",
    ));
    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);

    // The recall result was recorded in the transcript for the model.
    let session_path = dir
        .path()
        .join(".phoenix")
        .join("sessions")
        .join(format!("{session_id}.json"));
    let session: crate::session::Session =
        serde_json::from_str(&std::fs::read_to_string(&session_path).unwrap()).unwrap();
    let recorded = session.messages.iter().any(|m| matches!(
        m,
        crate::session::Message::ToolResult { tool_name, success, output, .. }
            if tool_name == "memory_recall" && !*success && output.contains("could not query the memory index")
    ));
    assert!(
        recorded,
        "memory_recall must record its result in the session: {:#?}",
        session.messages
    );
}

#[tokio::test]
async fn a_background_return_reaches_the_owner_once_in_a_direct_chat() {
    // The owner needs the result to answer the next user question. Showing
    // it in the worker's chat must not discard it from the owner's context.
    let dir = tempfile::tempdir().unwrap();
    let session_id = format!("mesh-inject-test-{}", uuid::Uuid::new_v4());
    let delivery_id =
        crate::runtime::postbox::job_started(&session_id, "researcher", "scan the market");
    crate::runtime::postbox::job_finished(
        &session_id,
        crate::runtime::postbox::CompletedJob {
            kind: crate::runtime::postbox::ReturnKind::Specialist,
            delivery_id: delivery_id.clone(),
            causation_id: Some("message-market-scan".to_string()),
            agent: "researcher".to_string(),
            subject: "scan the market".to_string(),
            ok: true,
            summary: "scan finished".to_string(),
            body: "three competitors found, details attached".to_string(),
            finished: chrono::Utc::now(),
        },
    );

    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![final_envelope("done", "## Result\nnothing new")],
    )]));
    let request_recorder = Arc::clone(&provider);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id.clone(),
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "anything new?"));
    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(
        request_recorder.request_texts.lock().unwrap().iter()
            .any(|request| request.contains("three competitors found, details attached")),
        "the owner must receive the completed result before answering"
    );
    assert!(!crate::runtime::postbox::has_ready(&session_id), "the receipt is settled, not left waiting");
}

#[tokio::test]
async fn provider_call_emits_a_named_progress_note() {
    // The silent stretch while a provider call is in flight must be named:
    // the working row shows "<agent> waiting on <model>" instead of a bare
    // spinner (the user-reported "40s where nothing seems to happen").
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![(
        "Orchestrator",
        vec![final_envelope("done", "## Result\nok")],
    )]));
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let runner = mesh_runner(provider, dir.path()).with_event_channel(tx);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "hi"));
    let _ = gateway.run().await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert!(
        events.iter().any(|event| matches!(
            event,
            CliEvent::StreamDelta { kind, text }
                if kind == "progress"
                    && text.contains("Orchestrator")
                    && text.contains("mock-model")
        )),
        "each provider call must announce who is waiting on which model: {events:#?}"
    );
}

#[tokio::test]
async fn frontend_specialist_runs_via_mesh() {
    // Frontend (with database and hacker) is executable since 2026-06-10 —
    // a talk to it must run a real specialist turn and report back.
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![
                tool_envelope(
                    "design_reference",
                    serde_json::json!({"path":"taste/SKILL.md"}),
                ),
                no_reply_talk_envelope("frontend", "style it", "make it pretty"),
                final_envelope("styled", "## Result\nfrontend delivered the new styles"),
            ],
        ),
        (
            "Frontend",
            vec![
                tool_envelope(
                    "design_reference",
                    serde_json::json!({"path":"taste/SKILL.md"}),
                ),
                final_envelope("styles done", "## Styles\nbutton component restyled"),
            ],
        ),
    ]));
    let session_id = format!("mesh-frontend-test-{}", uuid::Uuid::new_v4());
    let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::runtime::postbox::subscribe(&session_id, sub_tx);
    let runner = MeshRunner::new(
        Arc::clone(&provider) as Arc<dyn LLMProvider>,
        dir.path().to_path_buf(),
        dir.path().join(".phoenix"),
        session_id,
        orchestrator_spec(),
    );
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "style the page",
    ));

    let outcome = gateway.run().await;
    assert_eq!(outcome.steps, 1); // the frontend runs detached
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(outcome.user_messages[0].body.contains("new styles"));
    let (agent, ok, body) = await_background_return(&mut sub_rx).await;
    assert_eq!(agent, "frontend");
    assert!(ok);
    assert!(body.contains("button component restyled"));
}

/// Scripted provider that answers with NATIVE tool calls, recording the
/// message roles each request carried — proves every round resends the
/// full transcript (Phoenix runs stateless; no server-side stored state).
struct NativeStatefulProvider {
    call_index: std::sync::Mutex<usize>,
    /// Message roles per request.
    pub requests: std::sync::Mutex<Vec<Vec<MessageRole>>>,
}

impl NativeStatefulProvider {
    fn new() -> Self {
        Self {
            call_index: std::sync::Mutex::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LLMProvider for NativeStatefulProvider {
    fn name(&self) -> &str {
        "openai-codex"
    }
    fn display_name(&self) -> &str {
        "Native Stateful Mock"
    }
    fn base_url(&self) -> &str {
        "mock://stateful"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> StdHashMap<String, String> {
        StdHashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        self.requests
            .lock()
            .unwrap()
            .push(request.messages.iter().map(|m| m.role).collect());
        let mut index = self.call_index.lock().unwrap();
        let call = *index;
        *index += 1;
        let tool_calls = match call {
            0 => vec![crate::providers::contracts::NativeToolCall {
                id: "call-1".to_string(),
                tool_name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
            }],
            _ => vec![crate::providers::contracts::NativeToolCall {
                id: "call-2".to_string(),
                tool_name: "final_answer".to_string(),
                arguments: serde_json::json!({
                    "summary": "done",
                    "final_markdown": "## Done\nread the manifest",
                    "changes_made": [],
                    "verification": ["read"],
                    "execution_mode": "test"
                }),
            }],
        };
        Ok(CompletionResponse {
            content: String::new(),
            model: request.model,
            usage: TokenUsage::new(1, 1),
            reasoning: None,
            stop_reason: Some("tool_calls".to_string()),
            tool_calls,
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> Result<StreamingResponse> {
        anyhow::bail!("not used")
    }
    async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!("not used")
    }
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
}

#[test]
fn ambient_watch_reports_outside_changes_once_and_drops_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watched.rs");
    std::fs::write(&file, "v1").unwrap();
    let mut watch: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();
    record_file_mtime(
        &mut watch,
        dir.path(),
        &serde_json::json!({"path": "watched.rs"}),
    );
    assert_eq!(watch.len(), 1);
    // Unchanged: silent.
    assert!(ambient_changed_files(&mut watch).is_empty());
    // Outside change: reported once, then silent again.
    std::fs::write(&file, "v2 from outside").unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let file_handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    file_handle.set_modified(future).unwrap();
    drop(file_handle);
    let changed = ambient_changed_files(&mut watch);
    assert_eq!(changed.len(), 1);
    assert!(changed[0].contains("watched.rs"));
    assert!(ambient_changed_files(&mut watch).is_empty());
    // Deletion: reported once, then the entry is gone.
    std::fs::remove_file(&file).unwrap();
    let deleted = ambient_changed_files(&mut watch);
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].contains("(deleted)"));
    assert!(watch.is_empty());
}

/// Provider that runs many read rounds, recording the Tool-role message
/// contents of every request — proves an active turn retains exact results.
struct WindowRecorderProvider {
    call_index: std::sync::Mutex<usize>,
    pub tool_contents: std::sync::Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl LLMProvider for WindowRecorderProvider {
    fn name(&self) -> &str {
        "openai-codex"
    }
    fn display_name(&self) -> &str {
        "Window Recorder Mock"
    }
    fn base_url(&self) -> &str {
        "mock://window"
    }
    fn auth_type(&self) -> ContractAuthType {
        ContractAuthType::None
    }
    fn env_vars(&self) -> Vec<&str> {
        vec![]
    }
    fn default_headers(&self) -> StdHashMap<String, String> {
        StdHashMap::new()
    }
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
    fn fallback_models(&self) -> Vec<&str> {
        vec![]
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        self.tool_contents.lock().unwrap().push(
            request
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Tool)
                .map(|m| m.content.clone())
                .collect(),
        );
        let mut index = self.call_index.lock().unwrap();
        let call = *index;
        *index += 1;
        let tool_calls = if call < 5 {
            vec![crate::providers::contracts::NativeToolCall {
                id: format!("call-{call}"),
                tool_name: "read".to_string(),
                // Distinct exact ranges are legitimate dependent progress;
                // the economy guard only suppresses replaying the same read.
                arguments: serde_json::json!({"path": "Cargo.toml", "offset": call}),
            }]
        } else {
            vec![crate::providers::contracts::NativeToolCall {
                id: "call-final".to_string(),
                tool_name: "final_answer".to_string(),
                arguments: serde_json::json!({
                    "summary": "done",
                    "final_markdown": "## Done\nread it five times",
                    "changes_made": [],
                    "verification": ["read"],
                    "execution_mode": "test"
                }),
            }]
        };
        Ok(CompletionResponse {
            content: String::new(),
            model: request.model,
            usage: TokenUsage::new(1, 1),
            reasoning: None,
            stop_reason: Some("tool_calls".to_string()),
            tool_calls,
            provider_replay: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> Result<StreamingResponse> {
        anyhow::bail!("not used")
    }
    async fn embeddings(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!("not used")
    }
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn current_turn_keeps_all_tool_results_verbatim_until_final() {
    let dir = tempfile::tempdir().unwrap();
    // Fat enough to clear MESH_COMPACT_MIN_CHARS once wrapped in JSON.
    let fat_lines = (0..7)
        .map(|index| format!("# line-{index} {}", "p".repeat(1500)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        format!("[package]\nname = \"x\"\n{fat_lines}\n"),
    )
    .unwrap();
    let provider = Arc::new(WindowRecorderProvider {
        call_index: std::sync::Mutex::new(0),
        tool_contents: std::sync::Mutex::new(Vec::new()),
    });
    let recorder = Arc::clone(&provider);
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "read the manifest five times",
    ));
    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);

    let requests = recorder.tool_contents.lock().unwrap();
    let last = requests.last().expect("final request recorded");
    assert_eq!(last.len(), 5, "five tool results in the final request");
    assert!(last
        .iter()
        .all(|result| !result.contains("[aged result compacted")));
    assert!(
        last.iter().all(|result| result.len() > 1_000),
        "unexpected short native result lengths: {:?}",
        last.iter().map(String::len).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn oversized_session_auto_compacts_before_the_round() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = "mesh-compact-session";
    // Seed a durable main session far beyond a tiny context window.
    let mut store = SessionStore::new(dir.path().join(".phoenix/sessions"));
    let mut session =
        crate::session::Session::new_main_with_id(session_id.to_string(), "mock-model", "system");
    let big = "h".repeat(8_000);
    for index in 0..20 {
        session.push_message(crate::session::Message::User {
            content: format!("older request {index}: {big}"),
        });
        session.push_message(crate::session::Message::Assistant {
            content: format!("older answer {index}: {big}"),
        });
    }
    store.upsert(session);
    store.save_to_disk().unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        (
            "Orchestrator",
            vec![final_envelope("done", "## Done\ncompact then answer")],
        ),
        // The compaction summarizer request carries no `Agent:` line, so
        // ScriptedProvider routes it to the `unknown` queue.
        (
            "unknown",
            vec!["Goals: older requests 0-19 answered.".to_string()],
        ),
    ]));
    let runner =
        mesh_runner_with_session(provider, dir.path(), session_id).with_context_window(10_000);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "answer briefly",
    ));
    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);

    // The DURABLE session must have been folded: continuation summary
    // first, message count collapsed, newest history kept.
    let mut reloaded = SessionStore::new(dir.path().join(".phoenix/sessions"));
    reloaded.load_from_disk().unwrap();
    let session = reloaded.get(session_id).expect("session persists");
    assert!(
        session.messages.len() < 20,
        "expected folded transcript, got {} messages",
        session.messages.len()
    );
    match &session.messages[0] {
        crate::session::Message::Assistant { content } => {
            assert!(content.contains("AUTO-COMPACTED HISTORY"));
        }
        other => panic!("expected continuation summary first, got {other:?}"),
    }
    // Archive of the folded half exists beside the session files.
    let archive = dir
        .path()
        .join(format!(".phoenix/sessions/{session_id}.archive.jsonl"));
    assert!(archive.exists(), "folded messages must be archived");
}

#[tokio::test]
async fn every_round_resends_the_full_transcript() {
    let dir = tempfile::tempdir().unwrap();
    // Give the workspace a real file for the `read` call.
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let provider = Arc::new(NativeStatefulProvider::new());
    let recorder = Arc::clone(&provider);
    let mut gateway = Gateway::new(mesh_runner(provider, dir.path()));
    gateway.submit(AgentMessage::user_input(
        AgentAddress::Orchestrator,
        "read the manifest",
    ));
    let outcome = gateway.run().await;
    assert_eq!(outcome.user_messages.len(), 1);

    let requests = recorder.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "two provider rounds expected");
    // Stateless contract: both rounds carry the full transcript, and the
    // follow-up round also carries the tool result.
    assert!(requests[0].contains(&MessageRole::User));
    assert!(requests[1].contains(&MessageRole::User));
    assert_eq!(
        requests[1]
            .iter()
            .filter(|r| **r == MessageRole::Tool)
            .count(),
        1,
    );
}


#[tokio::test]
async fn mesh_pending_work_survives_every_model_final_path_without_retry_loops() {
    for kind in ["parsed", "prose", "tool", "native"] {
        for complete_after_reminder in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let state_root = dir.path().join(".phoenix");
            std::fs::create_dir_all(&state_root).unwrap();
            let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
            let session_id = format!("pending-{kind}-{complete_after_reminder}");
            let plan = |completed| tool_envelope("todo_write", serde_json::json!({"todos":[
                {"task":"Proofread Social assignment","completed":true},
                {"task":"Finish activity hour plan","completed":completed}
            ]}));
            let early = match kind {
                "parsed" => final_envelope("proofread", "Proofreading is ready."),
                "prose" => "Proofreading is ready.".to_string(),
                _ => tool_envelope("final_answer", serde_json::json!({"summary":"proofread","final_markdown":"Proofreading is ready."})),
            };
            let mut steps = vec![plan(false), early];
            if complete_after_reminder { steps.push(plan(true)); }
            steps.push(final_envelope("result", if complete_after_reminder { "Both checklist items are finished." } else { "The activity plan is blocked on a missing template." }));
            let expected_calls = steps.len();
            let mut script = ScriptedProvider::new(vec![("Orchestrator", steps)]);
            script.native_tools = kind == "native";
            let provider = Arc::new(script);
            let mut spec = orchestrator_spec();
            spec.tool_allowlist.push("todo_write".into());
            let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), state_root.clone(), session_id.clone(), spec);
            let mut gateway = Gateway::new(runner);
            gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Handle both checklist items."));
            let outcome = gateway.run().await;
            assert_eq!(outcome.user_messages.len(), 1, "{kind}");
            let body = &outcome.user_messages[0].body;
            // The runtime no longer appends a task-list appendix to replies.
            assert!(!body.contains("Unfinished items in the saved task list"), "{kind}: {body}");
            let requests = provider.request_texts.lock().unwrap();
            assert_eq!(requests.len(), expected_calls, "one reminder, no unbounded retry: {kind}");
            assert!(requests[2].contains("UNFINISHED WORK"), "the next request must receive the outstanding scope: {kind}");
            let raw = std::fs::read_to_string(state_root.join("sessions").join(format!("{session_id}.json"))).unwrap();
            let saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let canonical = saved["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            assert!(!canonical.contains("Unfinished items in the saved task list"));
        }
    }
}

#[tokio::test]
async fn rejected_workflow_commit_cannot_be_hidden_by_completed_todos_or_final_format() {
    const ISOLATED: &str = "PHOENIX_REJECTED_COMMIT_TEST_CHILD";
    if std::env::var_os(ISOLATED).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::mesh::mesh_tests::rejected_workflow_commit_cannot_be_hidden_by_completed_todos_or_final_format", "--nocapture"])
            .env(ISOLATED, "1").output().unwrap();
        assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
        return;
    }
    for kind in ["parsed", "prose", "tool", "native"] {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join(".phoenix");
        std::fs::create_dir_all(&state_root).unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
        crate::runtime::company::global().unwrap().ensure_full_catalog_team().unwrap();
        let final_attempt = match kind {
            "parsed" => final_envelope("Done", "All work is done."),
            "prose" => "All work is done.".into(),
            _ => tool_envelope("final_answer", serde_json::json!({"summary":"Done", "final_markdown":"All work is done."})),
        };
        let steps = vec![
            tool_envelope("todo_write", serde_json::json!({"todos":[{"task":"Finish assignment", "completed":true}]})),
            tool_envelope("work", serde_json::json!({"action":"workflow", "workflow_action":"transition", "workflow_payload":{
                "node_id":"node_missing_assignment", "idempotency_key":"commit-fixture", "phase":"commit", "state":"succeeded",
                "restart_state":"terminal", "reason":"Completed"}})),
            final_attempt.clone(), final_attempt,
        ];
        let mut script = ScriptedProvider::new(vec![("Orchestrator", steps)]);
        script.native_tools = kind == "native";
        let provider = Arc::new(script);
        let mut spec = orchestrator_spec();
        spec.tool_allowlist.extend(["todo_write", "work"].map(str::to_owned));
        let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), state_root.clone(),
            format!("rejected-commit-{kind}"), spec);
        let mut gateway = Gateway::new(runner);
        gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Check the assignment completion."));
        let outcome = gateway.run().await;
        assert_eq!(outcome.user_messages.len(), 1, "{kind}");
        assert!(!outcome.user_messages[0].body.contains("Workflow completion is unverified"), "{kind}: {}", outcome.user_messages[0].body);
        let requests = provider.request_texts.lock().unwrap();
        assert_eq!(requests.len(), 4, "one bounded reminder, not an infinite retry: {kind}");
        assert!(requests[3].contains("UNFINISHED WORK"), "{kind}");
        assert!(requests[3].contains("node_missing_assignment"), "{kind}");
    }
}

#[tokio::test]
async fn completion_audit_reaches_all_final_paths_after_successful_observation() {
    // Founding-team initialization changes the process-global directory used
    // by unrelated routing fixtures. Exercise the real global store in its
    // own process instead of changing those fixtures' identity resolution.
    const ISOLATED: &str = "PHOENIX_COMPLETION_AUDIT_TEST_CHILD";
    if std::env::var_os(ISOLATED).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::mesh::mesh_tests::completion_audit_reaches_all_final_paths_after_successful_observation", "--nocapture"])
            .env(ISOLATED, "1")
            .output().unwrap();
        assert!(result.status.success(), "isolated completion audit failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
        return;
    }
    for kind in ["parsed", "prose", "tool", "native"] {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join(".phoenix");
        std::fs::create_dir_all(&state_root).unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&state_root);
        image::RgbImage::from_pixel(16, 16, image::Rgb([240, 200, 40]))
            .save(dir.path().join("sample.png")).unwrap();
        let company = crate::runtime::company::global().unwrap();
        company.ensure_full_catalog_team().unwrap();
        let request = "Create a detailed Blender scene and save the final evidence; preserve the organic stem requirement.";
        let early = match kind {
            "parsed" => final_envelope("accepted", "Everything is accepted."),
            "prose" => "Everything is accepted.".to_string(),
            _ => tool_envelope("final_answer", serde_json::json!({"summary":"accepted","final_markdown":"Everything is accepted."})),
        };
        // A post-write image observation plus all-complete todos must not skip
        // the outcome audit. This fixture tests protocol, not rendered quality.
        let steps = vec![
            tool_envelope("todo_write", serde_json::json!({"todos":[
                {"task":"Save artifact","completed":true},
                {"task":"Inspect output","completed":true},
                {"task":"Check required finish","completed":true}
            ]})),
            tool_envelope("write", serde_json::json!({"path":"evidence.txt","content":"saved fixture"})),
            tool_envelope("image_analyze", serde_json::json!({"path":"sample.png","question":"Confirm this final artifact is accepted"})),
            early,
            final_envelope("incomplete", "The organic stem is still missing; the image alone does not prove completion."),
            final_envelope("incomplete", "The organic stem is still missing; the saved workflow is unfinished."),
        ];
        let mut script = ScriptedProvider::new(vec![("Orchestrator", steps)]);
        script.native_tools = kind == "native";
        script.native_images = true;
        let provider = Arc::new(script);
        let mut spec = orchestrator_spec();
        spec.tool_allowlist.extend(["todo_write", "write", "image_analyze", "work"].map(str::to_owned));
        spec.permissions.can_write_files = true;
        let runner = MeshRunner::new(provider.clone(), dir.path().to_path_buf(), state_root.clone(),
            format!("completion-audit-{kind}"), spec).with_native_vision(true);
        let mut gateway = Gateway::new(runner);
        gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, request));
        let outcome = gateway.run().await;
        assert_eq!(outcome.user_messages.len(), 1, "{kind}");
        assert!(outcome.user_messages[0].body.contains("organic stem is still missing"), "{kind}");
        assert_eq!(std::fs::read_to_string(dir.path().join("evidence.txt")).unwrap(), "saved fixture");
        let requests = provider.request_texts.lock().unwrap();
        assert!(requests[0].contains("SAVED DURABLE GOAL"));
        let (_, goals) = company.workflow_context(&format!("completion-audit-{kind}"), "phoenix").unwrap();
        assert_eq!(goals.runs.len(), 1);
        assert_eq!(goals.goals[0].objective, request);
        assert_ne!(goals.nodes[0].state, "succeeded");
        assert_eq!(requests.len(), 6, "one visual audit and one durable-work reminder, without indefinite retries: {kind}");
        assert!(requests[5].contains("UNFINISHED WORK"));
        assert!(!outcome.user_messages[0].body.contains("Workflow completion is unverified"));
        assert!(requests[3].contains("VISUAL QUALITY CHECK:"), "creative review instructions reach the next actual provider request: {kind}");
        assert!(requests[3].contains("A filename or inspection question calling this final is not evidence of completion"));
        assert!(provider.request_image_counts.lock().unwrap()[3] > 0, "criticism is accompanied by actual image pixels: {kind}");
        assert!(requests[4].contains("COMPLETION AUDIT"), "{kind}");
        // Normal turns carry no hidden wall-clock deadline, so no remaining-time note.
        assert!(!requests[4].contains("RUNTIME TIME EVIDENCE"), "{kind}");
        assert!(requests[4].contains("the plan is not locked"), "{kind}");
        assert!(requests[4].contains(request), "original scope survives: {kind}");
        drop(requests);
        // Recreate the runner from its saved conversation. A short continuation
        // must recover the same workflow even without replaying the failed
        // call or reclassifying the original request as a new build.
        let resumed_provider = Arc::new(ScriptedProvider::new(vec![("Orchestrator", vec![
            final_envelope("Done", "All finished."), final_envelope("Done", "All finished.")
        ])]));
        let mut resumed_spec = orchestrator_spec();
        resumed_spec.tool_allowlist.push("work".into());
        let resumed_runner = MeshRunner::new(resumed_provider.clone(), dir.path().to_path_buf(), state_root,
            format!("completion-audit-{kind}"), resumed_spec);
        let mut resumed_gateway = Gateway::new(resumed_runner);
        resumed_gateway.submit(AgentMessage::user_input(AgentAddress::Orchestrator, "Continue!"));
        let resumed_outcome = resumed_gateway.run().await;
        assert!(!resumed_outcome.user_messages[0].body.contains("Workflow completion is unverified"), "resumed {kind}");
        let resumed_requests = resumed_provider.request_texts.lock().unwrap();
        assert_eq!(resumed_requests.len(), 2, "one reminder on restart: {kind}");
        assert!(resumed_requests[0].contains(&goals.runs[0].run_id));
        assert!(resumed_requests[1].contains("UNFINISHED WORK"));
    }
}

// These are protocol fixtures, not creative or website acceptance evidence.
// The production controller still owns every parser, draw and phase transition.
const MANAGED_IRIS_REQUEST: &str = "Build a Phoenix website using Inter.";
const MANAGED_IRIS_BRIEF: &str = r#"{
  "status":"complete","message":"Brief fixture accepted.","questions":[],
  "brief":{
    "originalRequest":"Build a Phoenix website using Inter.",
    "subject":"Phoenix","pageType":"Website","scope":"One responsive page",
    "primaryGoal":"Explain the fixture","audience":"Test maintainers",
    "offer":"Deterministic protocol evidence","primaryAction":"Read the fixture",
    "requiredContent":["Introduction"],"constraints":[],"brandInputs":["Use Inter."],
    "creativeControl":"Choose unspecified details","explicitAnswers":[],
    "assumptions":[],"unresolved":[]
  }
}"#;
const MANAGED_IRIS_BRAND: &str = r##"{
  "version":1,
  "foundation":{"strategy":"create","existingAssets":[],"assetActions":[],"lockedDecisions":["Use Inter."],"assumptions":[]},
  "creativeDirection":{
    "summary":"Synthetic protocol fixture.",
    "traits":[{"quality":"precise","boundary":"not ornate"}],
    "productiveTension":"Large type with compact explanation.",
    "signatureDevice":{"description":"Offset title.","status":"candidate","invariants":["Left aligned title"]},
    "restraint":"One title.","avoid":["Decorative filler"]
  },
  "paletteRecipe":{"themes":{"light":{"accentSeed":"#C1492E","neutralSeed":"#665A50","surfaceContrast":"quiet"}}},
  "typefaces":[{"family":"Inter","source":"Explicit user fixture requirement","roles":["UI"],"weights":[500]}],
  "interfaceDirection":"Open type composition with direct controls.",
  "imageDirection":{"summary":"No illustration in this fixture.","subjects":[],"treatment":"None.","avoid":[]},
  "motionDirection":{"summary":"Visible entrance.","principles":["Explain title; load; title and body; 200ms; ease-out"],"avoid":["Repeated motion"]},
  "voice":{"summary":"Direct.","avoid":["Unproven claims"]}
}"##;

fn managed_iris_run_in_child(filter: &str) -> bool {
    const CHILD: &str = "PHOENIX_MANAGED_IRIS_TEST_CASE";
    if std::env::var(CHILD).as_deref() == Ok(filter) {
        return false;
    }
    // Company/settings are process-global. Reuse the existing isolation pattern
    // instead of changing another test's directory or installed prompt overlay.
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", filter, "--test-threads=1", "--nocapture"])
        .env(CHILD, filter)
        .env("PHOENIX_IRIS_DESIGN_ROOT", env!("CARGO_MANIFEST_DIR"))
        .output().expect("start isolated managed Iris fixture");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{filter}:\n{stdout}\n{stderr}");
    assert!(!stderr.contains("panicked at") && !stderr.contains("script exhausted"),
        "a caught runtime/provider panic cannot count as managed fixture success:\n{stderr}");
    assert!(stdout.contains("1 passed; 0 failed"), "the exact child test must run: {stdout}");
    print!("{stdout}");
    true
}

fn managed_iris_saved_state(root: &std::path::Path) -> serde_json::Value {
    let states: Vec<_> = std::fs::read_dir(root.join("iris_design")).unwrap()
        .map(|entry| entry.unwrap().path().join("active.json"))
        .filter(|path| path.is_file()).collect();
    assert_eq!(states.len(), 1, "one scoped controller must own this fixture");
    serde_json::from_slice(&std::fs::read(&states[0]).unwrap()).unwrap()
}

fn managed_iris_original_contexts(state: &serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    // Render expected contexts with the unchanged engine using the actual saved
    // draw. Context-only clones do not advance, redraw or start a preview.
    let script = r#"
import { dispatch } from './scripts/iris-design-runtime.mjs';
const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const saved = JSON.parse(Buffer.concat(chunks).toString('utf8'));
const contexts = {};
for (const phase of ['brief', 'brand', 'page']) {
  const state = structuredClone(saved);
  state.phase = phase; state.status = 'running'; state.correcting = false;
  delete state.pendingPrompt; delete state.error; delete state.correctionErrors;
  if (phase === 'brief') {
    delete state.referenceDeck; delete state.referenceDeckSnapshot;
    delete state.typographyCandidates;
  }
  contexts[phase] = await dispatch({ action: 'context', state });
}
process.stdout.write(JSON.stringify(contexts));
"#;
    let node = std::env::var_os("PHOENIX_NODE").unwrap_or_else(|| "node".into());
    let mut child = std::process::Command::new(node)
        .args(["--input-type=module", "--eval", script])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(state).unwrap()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "original context rendering failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn managed_iris_mesh_uses_original_phases_and_ordered_images_before_bounded_failure() {
    const FILTER: &str = "runtime::mesh::mesh_tests::managed_iris_mesh_uses_original_phases_and_ordered_images_before_bounded_failure";
    if managed_iris_run_in_child(FILTER) { return; }
    let home = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    let workspace = tempfile::tempdir().unwrap();
    let preserved = workspace.path().join("preserve.txt");
    std::fs::write(&preserved, "existing user material").unwrap();

    let mut script = ScriptedProvider::new(vec![("Frontend", vec![
        // Iris decides this is a site and starts the workflow herself.
        tool_envelope("design_website", serde_json::json!({"brief": MANAGED_IRIS_REQUEST, "folder": workspace.path().display().to_string()})),
        tool_envelope("final_answer", serde_json::json!({"final_markdown":"{invalid brief"})),
        tool_envelope("final_answer", serde_json::json!({"final_markdown":MANAGED_IRIS_BRIEF})),
        MANAGED_IRIS_BRAND.to_string(),
        "{invalid page".to_string(), "{invalid page".to_string(),
    ])]);
    script.native_images = true;
    script.native_tools = true;
    let provider = Arc::new(script);
    let session_id = format!("managed-iris-phases-{}", uuid::Uuid::new_v4());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);
    let runner = MeshRunner::new(provider.clone(), workspace.path(), home.path(), &session_id, orchestrator_spec())
        .with_native_vision(true).with_event_channel(event_tx);
    let address = AgentAddress::Specialist(SubAgentType::Frontend);
    let (spec, _) = runner.spec_for(&address).unwrap();
    let mut store = SessionStore::new(home.path().join("sessions"));
    let (mut session, _) = runner.load_session(&mut store, &address, &spec).unwrap();
    // A real old pin must not reintroduce the retired aesthetic contract into
    // the managed system prompt. Its historical receipt remains in the session.
    session.push_message(Message::ToolResult {
        tool_name: "design_reference".into(), input: r#"{"path":"taste/SKILL.md"}"#.into(),
        success: true, output: "Previously loaded legacy reference.".into(),
    });
    let owned_session_id = session.id.clone();
    store.upsert(session);
    store.save_one(&owned_session_id).unwrap();
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(address, MANAGED_IRIS_REQUEST));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(90), gateway.run())
        .await.expect("bounded managed phase fixture must finish without preview or provider work");

    assert_eq!(provider.request_texts.lock().unwrap().len(), 6,
        "malformed Brief is corrected once, native finish advances, and repeated malformed Page fails; no extra provider calls");
    assert!(provider.scripts.lock().unwrap()["Frontend"].is_empty());
    assert_eq!(outcome.user_messages.len(), 1, "intermediate phase JSON must never become user finals");
    assert!(outcome.user_messages[0].is_failed_result());
    assert!(outcome.user_messages[0].body.contains("Iris Design did not complete"));
    assert!(outcome.user_messages[0].body.contains("phase page"));
    assert!(!outcome.user_messages[0].body.contains("Brief fixture accepted"));
    assert!(!outcome.user_messages[0].body.contains("script exhausted"));
    let saved = managed_iris_saved_state(home.path());
    assert_eq!(saved["state"]["phase"], "page");
    assert_eq!(saved["state"]["status"], "failed");
    assert_eq!(saved["state"]["error"]["code"], "PHASE_FAILED");
    assert!(saved["pending_operation"].is_null());
    // Upstream's fourteen plus two extra hero options from the Phoenix adapter.
    assert_eq!(saved["state"]["referenceDeck"].as_array().unwrap().len(), 16);
    assert_eq!(saved["state"]["approvedBrief"]["originalRequest"], MANAGED_IRIS_REQUEST);
    assert_eq!(saved["state"]["approvedBrand"]["typefaces"][0]["family"], "Inter");
    assert!(saved["state"]["approvedPage"].is_null());

    let expected = managed_iris_original_contexts(&saved["state"]);
    let requests = provider.request_texts.lock().unwrap().clone();
    for (round, phase) in [(1, "brief"), (3, "brand"), (4, "page")] {
        assert!(requests[round].contains(expected[phase]["prompt"].as_str().unwrap()),
            "actual provider request omitted or altered the original {phase} prompt");
    }
    assert!(!requests[0].contains("=== CURRENT INTERNAL DESIGN PHASE ==="), "the workflow starts only when Iris calls design_website");
    assert!(requests[2].contains("Your previous Design Mode response failed validation."));
    assert!(requests[5].contains("Your previous Design Mode response failed validation."));
    assert!(requests[3].contains("Internal design controller consumed this response. Current phase: brand."));
    for request in requests.iter().skip(1) {
        let system = request.split_once("Phoenix task\n").expect("normal prompt assembly is retained").0;
        for excluded in ["Visual-design gate.", "PRIMARY VISUAL-DESIGN CONTRACT",
            "PINNED SUPPLEMENTARY REFERENCE MATERIAL", "Work every substantial deliverable in four passes:",
            "Ground the work before building.", "Plain meaning and usefulness", "fresh-visitor perspective",
            "Do not write riddles", "Accent should be scarce"] {
            assert!(!system.contains(excluded), "managed system prompt retained {excluded}");
        }
        assert!(system.contains("Permission settings remain authoritative"));
        assert!(system.contains("Do not fake tool results"));
        assert!(request.contains(MANAGED_IRIS_REQUEST));
    }

    let reference_paths = expected["brand"]["attachments"].as_array().unwrap();
    assert!(reference_paths.len() >= 14 && reference_paths.len() <= 32);
    assert_eq!(expected["page"]["attachments"], expected["brand"]["attachments"]);
    let mut expected_pixels = Vec::new();
    for path in reference_paths {
        expected_pixels.push(crate::runtime::vision::native_screen_data_uri(
            std::path::Path::new(path.as_str().unwrap())).await.unwrap());
    }
    {
        let images = provider.request_image_messages.lock().unwrap();
        let wire = provider.request_image_wire.lock().unwrap();
        assert!(images[0].is_empty() && images[1].is_empty() && images[2].is_empty());
        assert!(wire[0].is_empty() && wire[1].is_empty() && wire[2].is_empty());
        for round in [3, 4, 5] {
            assert_eq!(images[round].len(), 1, "one labeled selected-reference set per request");
            assert_eq!(images[round][0].images, expected_pixels, "selected pixels/order changed at round {round}");
            assert_eq!(wire[round].iter().map(|part| part["image_url"].as_str().unwrap()).collect::<Vec<_>>(),
                expected_pixels.iter().map(String::as_str).collect::<Vec<_>>(), "provider wire omitted or reordered reference pixels");
            for (index, path) in reference_paths.iter().enumerate() {
                assert!(images[round][0].content.contains(&format!("{}: {}", index + 1, path.as_str().unwrap())));
            }
        }
    }
    let mut reloaded = SessionStore::new(home.path().join("sessions"));
    reloaded.load_one_if_absent(&owned_session_id).unwrap();
    let messages = &reloaded.get(&owned_session_id).unwrap().messages;
    let users = messages.iter().filter_map(|message| match message {
        Message::User { content } => Some(content.as_str()), _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(users, vec![MANAGED_IRIS_REQUEST], "controller prompts never impersonate user messages");
    assert_eq!(std::fs::read_to_string(preserved).unwrap(), "existing user material");
    assert!(!workspace.path().join("index.html").exists());
    let mut transitions = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            CliEvent::GatewayNotice(note) if note.starts_with("Iris Design:") => transitions.push(note),
            CliEvent::ToolCallStarted { tool_name, .. } => panic!("no action tool was scripted: {tool_name}"),
            _ => {}
        }
    }
    assert_eq!(transitions.len(), 5);
    println!("MANAGED_IRIS_PHASES {}", serde_json::json!({"provider_rounds":5,"outcome":"expected_page_validation_failure",
        "selected_images":reference_paths.len(),"reference_snapshots":saved["state"]["referenceDeckSnapshot"],
        "transitions":transitions,"creative_acceptance":false}));
}

#[tokio::test]
async fn managed_iris_not_design_restores_normal_tools_and_final_without_project_edits() {
    const FILTER: &str = "runtime::mesh::mesh_tests::managed_iris_not_design_restores_normal_tools_and_final_without_project_edits";
    if managed_iris_run_in_child(FILTER) { return; }
    let home = tempfile::tempdir().unwrap();
    let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
    let workspace = tempfile::tempdir().unwrap();
    let source = "A URL identifies a resource. This glossary request needs no implementation.";
    std::fs::write(workspace.path().join("glossary.txt"), source).unwrap();
    let request = "Design a website glossary; investigate how URLs work.";
    assert!(!crate::runtime::build_contract::is_substantial_build_request(request));
    let mut script = ScriptedProvider::new(vec![("Frontend", vec![
        tool_envelope("design_website", serde_json::json!({"brief": request, "folder": workspace.path().display().to_string()})),
        serde_json::json!({"status":"not_design","message":"Answer the glossary question.","questions":[],"brief":null}).to_string(),
        // This tool is explicitly unavailable during managed phases. Successful
        // execution after qualification proves the ordinary tool path returned.
        tool_envelope("design_reference", serde_json::json!({"path":"taste/SKILL.md"})),
        tool_envelope("read", serde_json::json!({"path":"glossary.txt"})),
        final_envelope("Glossary answered", source),
    ])]);
    script.native_tools = true;
    script.native_images = true;
    let provider = Arc::new(script);
    let runner = MeshRunner::new(provider.clone(), workspace.path(), home.path(),
        format!("managed-iris-not-design-{}", uuid::Uuid::new_v4()), orchestrator_spec()).with_native_vision(true);
    let mut gateway = Gateway::new(runner);
    gateway.submit(AgentMessage::user_input(AgentAddress::Specialist(SubAgentType::Frontend), request));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(45), gateway.run())
        .await.expect("not_design must continue ordinary work without asking for another user turn");
    let requests = provider.request_texts.lock().unwrap();
    assert_eq!(requests.len(), 5, "no hidden retry or provider exhaustion can pass this fixture");
    assert!(provider.scripts.lock().unwrap()["Frontend"].is_empty());
    assert!(!requests[0].contains("=== CURRENT INTERNAL DESIGN PHASE ==="));
    assert!(requests[1].contains("=== CURRENT INTERNAL DESIGN PHASE ==="));
    assert!(requests[1].contains("TasteCode Design Briefing mode"));
    for request in requests.iter().skip(2) {
        assert!(!request.contains("=== CURRENT INTERNAL DESIGN PHASE ==="));
        assert!(request.contains("Website design qualification has ended."));
        assert!(request.split_once("Phoenix task\n").unwrap().0.contains("Visual-design gate."));
    }
    assert_eq!(*provider.request_image_counts.lock().unwrap(), vec![0, 0, 0, 0, 0]);
    assert_eq!(outcome.user_messages.len(), 1);
    assert!(!outcome.user_messages[0].is_failed_result());
    assert!(outcome.user_messages[0].body.contains(source));
    assert!(!outcome.user_messages[0].body.contains("not_design"));
    let saved = managed_iris_saved_state(home.path());
    assert_eq!(saved["state"]["outcome"], "not_design");
    assert_eq!(saved["state"]["status"], "complete");
    assert!(saved["state"]["referenceDeck"].is_null(), "unrelated task must not draw references");
    assert!(!workspace.path().join(".taste").exists());
    assert_eq!(std::fs::read_to_string(workspace.path().join("glossary.txt")).unwrap(), source);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 1);
    let owned_session_id = saved["scope"]["session_id"].as_str().unwrap();
    let mut store = SessionStore::new(home.path().join("sessions"));
    store.load_one_if_absent(owned_session_id).unwrap();
    let messages = &store.get(owned_session_id).unwrap().messages;
    for expected_tool in ["design_reference", "read"] {
        assert!(messages.iter().any(|message| matches!(message, Message::ToolResult { tool_name, success:true, .. } if tool_name == expected_tool)),
            "ordinary {expected_tool} tool did not execute successfully");
    }
    let users = messages.iter().filter_map(|message| match message {
        Message::User { content } => Some(content.as_str()), _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(users, vec![request]);
    println!("MANAGED_IRIS_NOT_DESIGN {}", serde_json::json!({"provider_rounds":4,"outcome":"not_design",
        "normal_tools_executed":["design_reference","read"],"project_unchanged":true,"user_finals":1}));
}
