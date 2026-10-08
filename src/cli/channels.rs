//! Local channel worker. Connections are explicit and scoped; it never starts
//! a gateway or changes account permissions as a side effect of receiving text.
use crate::channels::{ChannelApi, ChannelCapacity, ChannelConfig, ChannelState, Outbound, Platform, SendFailure};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
#[path = "channel_questions.rs"]
mod questions;
#[path = "channel_journal.rs"]
mod journal;

pub fn load(path: &Path) -> Result<ChannelConfig> {
    let raw = crate::config::private_io::read_private_file_limited(path, 128 * 1024)?
        .context("Channel connection file not found")?;
    let config: ChannelConfig =
        serde_json::from_slice(&raw).context("Invalid channel connection JSON")?;
    config.validate()?;
    Ok(config)
}
fn root(config: &ChannelConfig) -> PathBuf {
    crate::config::phoenix_home()
        .join("channels")
        .join(&config.id)
}
pub fn status(path: &Path) -> Result<serde_json::Value> {
    status_view(path, false)
}
const CHANNEL_UI_RESPONSE_BYTES: usize = 256 * 1024;
fn bounded_ui_response(value: serde_json::Value) -> Result<serde_json::Value> {
    anyhow::ensure!(serde_json::to_vec(&value)?.len() < CHANNEL_UI_RESPONSE_BYTES,
        "Channel settings exceed the desktop response budget. Inspect channels with the CLI; all saved records were preserved");
    Ok(value)
}
pub fn status_summary(path: &Path) -> Result<serde_json::Value> {
    bounded_ui_response(status_view(path, true)?)
}
fn status_view(path: &Path, summary_only: bool) -> Result<serde_json::Value> {
    let config = load(path)?;
    let dir = root(&config);
    let state = ChannelState::load(&dir.join("state.json"), &config)?;
    let running =
        crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?.is_none();
    Ok(
        json!({"id":config.id,"name":config.name,"platform":config.platform,"enabled":config.enabled,"running":running,"agent_id":config.agent_id,"saved_login":crate::channels::credentials::saved(&crate::config::phoenix_home(), &config)?,"delivery":if summary_only {state.summary_counts()} else {state.summary()}}),
    )
}
fn valid_snapshot(snapshot: Option<&str>) -> Result<()> {
    anyhow::ensure!(snapshot.is_none_or(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
        "Invalid recovery snapshot. Refresh the recovery list");
    Ok(())
}
fn check_snapshot(state: &ChannelState, config: &ChannelConfig, expected: Option<&str>) -> Result<()> {
    valid_snapshot(expected)?;
    if let Some(expected) = expected {
        anyhow::ensure!(state.review_snapshot(config)? == expected,
            "Recovery snapshot changed. Refresh the recovery list and check the selected item again");
    }
    Ok(())
}
pub fn review_page(path: &Path, kind: &str, offset: usize, limit: usize, expected: Option<&str>) -> Result<serde_json::Value> {
    anyhow::ensure!(matches!(kind, "turns" | "deliveries") && (1..=100).contains(&limit), "Choose deliveries or turns and a page size of 1–100");
    valid_snapshot(expected)?;
    let config = load(path)?;
    let dir = root(&config);
    let state = ChannelState::load(&dir.join("state.json"), &config)?;
    let snapshot = state.review_snapshot(&config)?;
    anyhow::ensure!(expected.is_none_or(|expected| expected == snapshot),
        "Recovery snapshot changed. Refresh the recovery list and check the selected item again");
    let mut page = state.review_page(kind, offset, limit)?;
    page["id"] = json!(config.id);
    page["snapshot"] = json!(snapshot);
    page["delivery"] = state.summary_counts();
    page["running"] = json!(crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?.is_none());
    bounded_ui_response(page)
}
pub fn resolve_delivery(path: &Path, id: &str, retry: bool) -> Result<()> {
    resolve_delivery_checked(path, id, retry, None)
}
pub fn resolve_delivery_checked(path: &Path, id: &str, retry: bool, expected: Option<&str>) -> Result<()> {
    valid_snapshot(expected)?;
    let config = load(path)?;
    let dir = root(&config);
    let _claim = crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?
        .context("Disconnect this channel before resolving a delivery")?;
    let config = load(path)?;
    anyhow::ensure!(root(&config) == dir, "Connection changed. Refresh its settings");
    let state_path = dir.join("state.json");
    let mut state = ChannelState::load(&state_path, &config)?;
    check_snapshot(&state, &config, expected)?;
    let delivery = state
        .outbox
        .iter_mut()
        .find(|d| d.id == id)
        .context("Delivery not found")?;
    anyhow::ensure!(
        matches!(
            delivery.state.as_str(),
            "uncertain" | "rejected" | "sending"
        ),
        "Only an interrupted or rejected delivery needs resolution"
    );
    delivery.state = if retry { "pending" } else { "sent" }.into();
    delivery.retry_at = 0;
    // A user's confirmation supplies no remote message ID. Keep any real ID
    // already known; do not grow a full legacy file with a fabricated one.
    state.notice = None;
    state.save(&state_path)
}

pub fn acknowledge_turn(path: &Path, id: &str) -> Result<()> {
    acknowledge_turn_checked(path, id, None)
}
pub fn acknowledge_turn_checked(path: &Path, id: &str, expected: Option<&str>) -> Result<()> {
    valid_snapshot(expected)?;
    let config = load(path)?;
    let dir = root(&config);
    let _claim = crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?
        .context("Disconnect this channel before resolving a turn")?;
    let config = load(path)?;
    anyhow::ensure!(root(&config) == dir, "Connection changed. Refresh its settings");
    let state_path = dir.join("state.json");
    let mut state = ChannelState::load(&state_path, &config)?;
    check_snapshot(&state, &config, expected)?;
    let job = state
        .jobs
        .iter_mut()
        .find(|j| j.turn_id == id)
        .context("Turn not found")?;
    anyhow::ensure!(
        job.state == "needs_review" || job.state == "running",
        "This turn does not need interruption review"
    );
    job.state = "done".into();
    state.notice = state.can_start().then(||
        "Interrupted turn acknowledged in Phoenix. Queued channel messages can continue.".into());
    state.save(&state_path)
}

pub async fn check(path: &Path, token_stdin: bool) -> Result<()> {
    let config = load(path)?;
    let token = read_token(&config, token_stdin)?;
    ChannelApi::new(config, token.to_string())?.verify().await
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

type TurnObserver = Option<(String, tokio::task::JoinHandle<Result<String>>)>;
fn observer_count(active: &TurnObserver, answering: &[TurnObserver], retained_results: usize) -> usize {
    usize::from(active.is_some()) + answering.iter().filter(|lane| lane.is_some()).count() + retained_results
}
fn intake_ready(state: &ChannelState, blocked_at: Option<(usize,usize)>) -> bool {
    state.can_start() && blocked_at != Some(state.capacity_stamp())
}

pub async fn run(path: &Path, token_stdin: bool) -> Result<()> {
    let config = load(path)?;
    anyhow::ensure!(config.enabled, "Enable this connection before starting it");
    let dir = root(&config);
    let state_path = dir.join("state.json");
    let _claim = crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?
        .context("This channel is already running")?;
    let token = read_token(&config, token_stdin)?;
    // Telegram has one update cursor per bot, so two connection workers using
    // the same bot would silently acknowledge one another's messages.
    let _bot_claim = if config.platform == Platform::Telegram {
        Some(
            crate::config::private_io::try_execution_claim(
                &crate::config::phoenix_home()
                    .join("channels")
                    .join(format!("bot-{}.lock", crate::channels::digest(&token))),
            )?
            .context("This Telegram bot already has a channel receiver")?,
        )
    } else {
        None
    };
    let api = std::sync::Arc::new(ChannelApi::new(config.clone(), token.to_string())?);
    api.verify().await?;
    verify_gateway().await?;
    let mut state = ChannelState::load(&state_path, &config)?;
    state.recover();
    state.save(&state_path)?;
    // Establish the historical boundary before reporting connected. Waiting
    // for a first new message here would otherwise discard it as history.
    if state.cursor.is_none() && state.can_start() {
        state.admit(api.poll(None).await?)?;
        state.save(&state_path)?;
    }
    println!(
        "{} connected. Replies and images only. Ctrl-C disconnects the channel.",
        config.name
    );
    // Mirror: answers to conversation typed in Phoenix also reach the chat.
    let (mirror_tx, mut mirror_rx) = tokio::sync::mpsc::channel::<MirrorEvent>(64);
    let mut announced = std::collections::HashSet::<String>::new();
    let mut last_announced: Option<String> = None;
    let mirror = tokio::spawn(mirror_feed(config.session_id.clone(), mirror_tx));
    let mut typing_at = tokio::time::Instant::now();
    let mut typing: Option<tokio::task::JoinHandle<()>> = None;
    let mut active: TurnObserver = None;
    let mut answering: Vec<TurnObserver> = Vec::new();
    let (question_tx, mut question_rx) = tokio::sync::mpsc::channel::<questions::QuestionUpdate>(32);
    let mut poll_at = 0;
    let mut receiver = crate::channels::ChannelPoll::default();
    let mut sender = crate::channels::ChannelSend::default();
    let mut failures = 0u32;
    let mut recovery: Option<tokio::task::JoinHandle<Result<Vec<(String, Vec<Outbound>)>>>> = None;
    let mut recover_at = tokio::time::Instant::now();
    let mut submission_failures = 0u32;
    let mut submit_at = tokio::time::Instant::now();
    let mut pending_question = None;
    let mut completed = std::collections::VecDeque::<(String, Vec<Outbound>)>::new();
    let mut deferred_stamp = None;
    let mut intake_blocked_at = None;
    // Every exit, including a failed state write, reaches observer cleanup.
    // Dropping a JoinHandle alone would leave its observation task detached.
    let outcome: Result<()> = async {
    loop {
        tokio::select! { _=tokio::signal::ctrl_c()=>{break;}, _=tokio::time::sleep(Duration::from_millis(250))=>{} }
        let mut observers = observer_count(&active, &answering, completed.len());
        state.set_active_observers(observers);
        if pending_question.is_none() {
            pending_question = question_rx.try_recv().ok();
            if pending_question.is_some() { deferred_stamp = None; }
        }
        // Keep a blocked update, leaving the bounded mpsc queue to apply
        // backpressure. Retry only after capacity/ownership changes, not on
        // every idle tick. Live terminal outputs are held by exact turn ID too.
        if deferred_stamp != Some(state.capacity_stamp()) {
            while pending_question.is_some() {
                let published = publish_pending_question(&mut state, &mut pending_question)?;
                state.save(&state_path)?;
                if !published { break; }
                pending_question = question_rx.try_recv().ok();
            }
            if pending_question.is_none() {
                while let Some((id, output)) = completed.front() {
                    state.set_active_observers(observers - 1);
                    let published = publish_completion(&mut state, id, output)?;
                    if !published { state.set_active_observers(observers); }
                    state.save(&state_path)?;
                    if !published { break; }
                    completed.pop_front();
                    observers -= 1;
                }
            }
            deferred_stamp = Some(state.capacity_stamp());
        }
        if recovery.as_ref().is_some_and(|task| task.is_finished()) {
            match recovery.take().unwrap().await {
                Ok(Ok(results)) => {
                    let previous_notice = state.notice.clone();
                    if apply_recovered(&mut state, results)? > 0 || state.notice != previous_notice {
                        state.save(&state_path)?;
                    }
                }
                _ => {
                    state.notice = Some("A saved reply could not be recovered. Open Phoenix to review it.".into());
                    state.save(&state_path)?;
                }
            }
            recover_at = tokio::time::Instant::now() + Duration::from_secs(15);
        }
        if recovery.is_none() && completed.is_empty() && pending_question.is_none()
            && state.can_start() && tokio::time::Instant::now() >= recover_at {
            let jobs = state.jobs.iter().filter(|job| job.state == "needs_review"
                && config.allowed_user_ids.contains(&job.input.user_id))
                .cloned().collect::<Vec<_>>();
            if !jobs.is_empty() {
                let cfg = config.clone();
                let images = dir.join("images");
                let questions = state.questions.clone();
                recovery = Some(tokio::task::spawn_blocking(move || {
                    super::channel_receipts::recoverable(&cfg, &jobs, &questions, &images)
                }));
            }
        }
        // A reply must be deliverable even while the original turn is waiting.
        answering.retain(Option::is_some);
        if answering.len() < 16 && completed.is_empty() && pending_question.is_none() && state.can_start() {
            if let Some(index) = state.jobs.iter().position(|j| j.state == "queued" && !matches!(state.question_for(&j.input), Ok(None))) {
                let job = state.jobs[index].clone();
                let selection = state.question_for(&job.input).and_then(|question| {
                    anyhow::ensure!(config.allowed_user_ids.contains(&job.input.user_id), "This sender is no longer allowed.");
                    Ok(question)
                });
                match selection {
                    Ok(Some(question)) => {
                        state.questions.iter_mut().find(|q| q.ask_id == question.ask_id).unwrap().answered_by = Some(job.input.id.clone());
                        state.jobs[index].state = "running".into();
                        state.save(&state_path)?;
                        answering.push(Some((job.turn_id.clone(), tokio::spawn(questions::answer(config.clone(), job.turn_id, question, job.input.text, question_tx.clone())))));
                        observers += 1;
                        state.set_active_observers(observers);
                    }
                    Err(error) => {
                        state.jobs[index].state = "needs_review".into();
                        completed.push_back((job.turn_id, vec![Outbound::Text { text: error.to_string() }]));
                        observers += 1;
                        state.set_active_observers(observers);
                        deferred_stamp = None;
                        state.save(&state_path)?;
                    }
                    Ok(None) => {}
                }
            }
        }
        // Only the connection's allowed identities can leave queued state.
        if active.is_none() && answering.is_empty()
            && completed.is_empty() && pending_question.is_none() && state.can_start()
            && tokio::time::Instant::now() >= submit_at
        {
            // A job waiting for review is never resubmitted, so it must not
            // hold back newer messages: they used to queue behind it forever.
            if let Some(index) = state.jobs.iter().position(|j| j.state == "queued") {
                let job = state.jobs[index].clone();
                if !config.allowed_user_ids.contains(&job.input.user_id) {
                    state.jobs[index].state = "needs_review".into();
                    state.notice = Some("A queued sender is no longer allowed.".into());
                    state.save(&state_path)?;
                } else {
                    state.jobs[index].state = "running".into();
                    state.save(&state_path)?;
                    let cfg = config.clone();
                    let id = job.turn_id.clone();
                    let updates = question_tx.clone();
                    active = Some((
                        id,
                        tokio::spawn(
                            async move { let text = crate::channels::request_text(&cfg, &job.input); run_turn_with_questions(cfg, job.turn_id, text, Some(updates)).await },
                        ),
                    ));
                    observers += 1;
                    state.set_active_observers(observers);
                }
            }
        }
        for lane in std::iter::once(&mut active).chain(answering.iter_mut()) {
        if lane
            .as_ref()
            .is_some_and(|(_, handle)| handle.is_finished())
        {
            let (id, handle) = lane.take().unwrap();
            observers -= 1;
            state.set_active_observers(observers);
            match handle.await {
                Ok(Ok(markdown)) => {
                    let answered_foreground = markdown.is_empty() && state.jobs.iter().find(|job| job.turn_id == id).is_some_and(|job| state.questions.iter().any(|q| q.answered_by.as_deref() == Some(job.input.id.as_str())));
                    let output = if answered_foreground { Ok(Vec::new()) } else {
                        crate::channels::project_reply(&config, &markdown, &dir.join("images"))
                    };
                    state.jobs.iter_mut().find(|job| job.turn_id == id).context("Unknown completed channel turn")?.state = "needs_review".into();
                    match output {
                        Ok(output) => {
                            completed.push_back((id.clone(), output)); deferred_stamp = None;
                            // Transfer the finished observer's reserve to its
                            // retained result until that exact result is saved.
                            observers += 1;
                            state.set_active_observers(observers);
                        }
                        Err(_) => { state.notice = Some("The saved result could not be prepared for this channel. Review the original reply in Phoenix; it was not resubmitted.".into()); }
                    }
                    submission_failures = 0;
                    if state.notice.as_deref() == Some(WAITING_FOR_GATEWAY) {
                        state.notice = None;
                    }
                }
                Ok(Err(error)) if defer_unsubmitted_turn(&mut state, &id, &error) => {
                    submission_failures = submission_failures.saturating_add(1);
                    submit_at = tokio::time::Instant::now()
                        + Duration::from_secs((1u64 << submission_failures.min(5)).min(30));
                }
                _ => {
                    state
                        .jobs
                        .iter_mut()
                        .find(|j| j.turn_id == id)
                        .unwrap()
                        .state = "needs_review".into();
                    state.notice =
                        Some("A turn needs review in Phoenix; it was not resubmitted.".into());
                    // Useful compact failure for the channel, never a raw
                    // provider/tool diagnostic. The job remains reviewable.
                    let delivery_id =
                        crate::channels::digest(&format!("{id}-error"))[..24].to_string();
                    if state.can_start() && !state.outbox.iter().any(|delivery| delivery.id == delivery_id) {
                        state.outbox.push(crate::channels::Delivery { id:delivery_id,item:Outbound::Text{text:"I couldn’t finish this run. Open Phoenix to review it; your conversation is saved.".into()},state:"pending".into(),retry_at:0,remote_id:None });
                    }
                }
            }
            state.save(&state_path)?;
        }
        }
        // "typing…" in the chat while a coworker works on a chat message.
        if (active.is_some() || answering.iter().any(Option::is_some))
            && tokio::time::Instant::now() >= typing_at
            && typing.as_ref().is_none_or(|task| task.is_finished())
        {
            let api = api.clone();
            typing = Some(tokio::spawn(async move { api.typing().await }));
            typing_at = tokio::time::Instant::now() + Duration::from_secs(4);
        }
        while let Ok(event) = mirror_rx.try_recv() {
            if !state.can_start() { continue; }
            let own = state.jobs.iter().any(|job| job.turn_id == event.turn);
            let mut texts = Vec::new();
            if !own && !announced.contains(&event.turn) {
                // A turn typed in Phoenix: say what was asked, once. Wakes,
                // coworker messages and routines have no typed prompt but are
                // still mirrored.
                announced.insert(event.turn.clone());
                if announced.len() > 256 { announced.clear(); announced.insert(event.turn.clone()); }
                // Wakes and coworker returns start new turns under the same
                // typed prompt; announce each prompt once, not once per turn.
                if let Some(prompt) = mirrored_prompt(&config.session_id).filter(|prompt| last_announced.as_deref() != Some(prompt.as_str())) {
                    last_announced = Some(prompt.clone());
                    texts.push(format!("💬 In Phoenix: {prompt}"));
                }
            }
            // The chat's own final answer and questions are delivered by its job.
            if (event.fin || event.ask.is_some()) && own { continue; }
            if let Some(ask) = &event.ask {
                // A question from a task started in Phoenix: ask it here too,
                // with buttons; anyone allowed in this chat may answer.
                for text in texts {
                    if let Ok(items) = crate::channels::project_reply(&config, &text, &dir.join("images")) {
                        for (part, item) in items.into_iter().enumerate() {
                            let id = crate::channels::digest(&format!("mirror-ask-head-{ask}-{part}"))[..24].to_string();
                            if !state.outbox.iter().any(|delivery| delivery.id == id) { state.outbox.push(crate::channels::Delivery { id, item, state:"pending".into(), retry_at:0, remote_id:None }); }
                        }
                    }
                }
                if let Ok(Some(text)) = questions::question_text(&config, ask) {
                    if let Err(error) = state.queue_question_as(&event.turn, ask, &text, String::new()) { state.notice = Some(error.to_string()); }
                }
                state.save(&state_path)?;
                continue;
            }
            texts.push(event.text.clone());
            for (index, text) in texts.into_iter().enumerate() {
                let Ok(items) = crate::channels::project_reply(&config, &text, &dir.join("images")) else { continue };
                for (part, item) in items.into_iter().enumerate() {
                    let id = crate::channels::digest(&format!("mirror-{}-{}-{index}-{part}-{}", event.turn, event.fin, crate::channels::digest(&text)))[..24].to_string();
                    if !state.outbox.iter().any(|delivery| delivery.id == id) {
                        state.outbox.push(crate::channels::Delivery { id, item, state:"pending".into(), retry_at:0, remote_id:None });
                    }
                }
            }
            state.save(&state_path)?;
        }
        if let Some((id,result)) = sender.take_ready().await {
            settle_delivery(&mut state, &id, result)?;
            state.save(&state_path)?;
        }
        // Save before starting HTTP. One active request preserves multipart
        // reply ordering while the inbox and question handlers stay live.
        if let Some(item) = prepare_delivery(&mut state, &state_path)? {
            anyhow::ensure!(sender.start(api.clone(), item.id, item.item), "A channel delivery is already active");
        }
        if let Some(received) = receiver.take_ready().await {
            match received {
                Ok(batch) => {
                    let previous = (state.cursor.clone(), state.jobs.len(), state.notice.clone());
                    let mut capacity_rejected = false;
                    let admitted = match state.admit(batch) {
                        Ok(()) => { intake_blocked_at = None; true },
                        Err(error) => {
                            capacity_rejected = error.is::<ChannelCapacity>();
                            state.notice = Some(error.to_string()); false
                        }
                    };
                    if previous != (state.cursor.clone(), state.jobs.len(), state.notice.clone()) { state.save(&state_path)?; }
                    // A whole poll batch can exceed available room even while
                    // one queued job still fits. Do not re-encode that rejected
                    // batch every two seconds against an unchanged backlog.
                    if capacity_rejected { intake_blocked_at = Some(state.capacity_stamp()); }
                    failures = 0;
                    poll_at = now() + if admitted && config.platform == Platform::Telegram { 0 } else { 2 };
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    poll_at = now() + (2u64.saturating_pow(failures.min(6))).min(60);
                    state.notice = Some(error.to_string());
                    state.save(&state_path)?;
                }
            }
        }
        if poll_at <= now() && completed.is_empty() && pending_question.is_none() && intake_ready(&state, intake_blocked_at) {
            receiver.start(api.clone(), state.cursor.clone());
        }
    }
    Ok(())
    }.await;
    drop(receiver);
    mirror.abort();
    if let Some(task) = typing { task.abort(); }
    let send_cleanup = match sender.finish_or_cancel().await {
        Some((id,result)) => settle_delivery(&mut state, &id, result),
        None => Ok(()),
    };
    for lane in std::iter::once(active).chain(answering.into_iter()) {
    if let Some((id, handle)) = lane {
        handle.abort();
        let _ = handle.await;
        state
            .jobs
            .iter_mut()
            .find(|j| j.turn_id == id)
            .unwrap()
            .state = "needs_review".into();
    }
    }
    if let Some(task) = recovery { let _ = task.await; }
    state.set_active_observers(0);
    // Pending questions still have their exact durable ask and owning job.
    // Completion recovery uses the existing bounded receipt cache; shutdown
    // cannot promise indefinite retention of that cache or its frozen images.
    state.notice = state.can_start().then(||
        "Channel disconnected. Review unpublished questions or results in Phoenix before resubmitting work.".into());
    let saved = state.save(&state_path);
    outcome.and(send_cleanup).and(saved)
}

// Complete and queue recovered output in the same atomic state save. Existing
// deliveries retain their IDs/states, including uncertain HTTP outcomes.
fn apply_recovered(state: &mut ChannelState, results: Vec<(String, Vec<Outbound>)>) -> Result<usize> {
    let mut count = 0;
    for (id, output) in results {
        if !state.jobs.iter().any(|job| job.turn_id == id && job.state == "needs_review") { continue; }
        if !publish_completion(state, &id, &output)? { break; }
        let error_id = crate::channels::digest(&format!("{id}-error"))[..24].to_string();
        // A compact failure which has not left the process is superseded.
        // Never touch a sending/uncertain receipt or erase a sent message.
        state.outbox.retain(|d| d.id != error_id || d.state != "pending");
        count += 1;
    }
    if count > 0 && !state.jobs.iter().any(|j| j.state == "needs_review")
        && !state.outbox.iter().any(|d| matches!(d.state.as_str(), "uncertain" | "rejected")) {
        state.notice = Some("Recovered saved channel progress from Phoenix.".into());
    }
    Ok(count)
}

fn publish_completion(state: &mut ChannelState, id: &str, output: &[Outbound]) -> Result<bool> {
    match state.complete(id, output.to_vec()) {
        Ok(()) => Ok(true),
        Err(error) if error.is::<ChannelCapacity>() => {
            if let Some(job) = state.jobs.iter_mut().find(|job| job.turn_id == id) {
                job.state = "needs_review".into();
            }
            state.notice = Some(error.to_string());
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn publish_pending_question(state: &mut ChannelState, pending: &mut Option<questions::QuestionUpdate>) -> Result<bool> {
    let Some(update) = pending.as_ref() else { return Ok(true); };
    match state.queue_question(&update.job_id, &update.ask_id, &update.text) {
        Ok(()) => { *pending = None; Ok(true) },
        Err(error) if error.is::<ChannelCapacity>() => {
            state.notice = Some(error.to_string());
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn settle_delivery(state: &mut ChannelState, id: &str, result: std::result::Result<String, SendFailure>) -> Result<()> {
    let delivery = state.outbox.iter_mut().find(|d| d.id == id).context("Unknown channel delivery receipt")?;
    anyhow::ensure!(delivery.state == "sending", "Channel delivery receipt is not in flight");
    match result {
        Ok(remote_id) => { delivery.state = "sent".into(); delivery.remote_id = Some(remote_id); }
        Err(SendFailure::RetryAfter(delay)) => { delivery.state = "pending".into(); delivery.retry_at = now().saturating_add(delay.as_secs().saturating_add(2)); }
        Err(SendFailure::Rejected(reason)) => { delivery.state = "rejected".into(); state.notice = Some(reason); }
        Err(SendFailure::Uncertain) => { delivery.state = "uncertain".into(); state.notice = Some("A reply may have arrived. Check the channel before retrying it.".into()); }
    }
    Ok(())
}

fn prepare_delivery(state: &mut ChannelState, path: &Path) -> Result<Option<crate::channels::Delivery>> {
    let Some(index) = state.outbox.iter().position(|d| d.state != "sent") else { return Ok(None); };
    if state.outbox[index].state != "pending" || state.outbox[index].retry_at > now() { return Ok(None); }
    state.outbox[index].state = "sending".into();
    // save may archive earlier sent rows. Bind the payload before those indices
    // change; the caller sends only after this exact sending state is durable.
    let item = state.outbox[index].clone();
    state.save(path)?;
    Ok(Some(item))
}

async fn verify_gateway() -> Result<()> {
    let stream = UnixStream::connect(super::daemon::socket_path())
        .await
        .context("Start Phoenix before connecting a channel")?;
    let (read, mut write) = stream.into_split();
    write.write_all(b"\"ProtocolInfo\"\n").await?;
    let mut reader = BufReader::new(read);
    let mut bytes = Vec::new();
    let count = tokio::time::timeout(
        Duration::from_secs(3),
        (&mut reader).take(8193).read_until(b'\n', &mut bytes),
    )
    .await
    .context("Phoenix did not answer the channel handshake")??;
    anyhow::ensure!(count > 0 && count <= 8192, "Invalid Phoenix handshake");
    match serde_json::from_slice::<super::daemon::WireResponse>(&bytes)? {
        super::daemon::WireResponse::ProtocolInfo { protocol, .. }
            if protocol == super::daemon::GATEWAY_WIRE_PROTOCOL =>
        {
            Ok(())
        }
        _ => anyhow::bail!("Phoenix and this channel worker use different gateway protocols"),
    }
}

const WAITING_FOR_GATEWAY: &str =
    "Waiting for Phoenix to reconnect. Your message is queued.";

// Only attach this marker before attempting to write the Turn request. After
// that boundary a disconnect cannot prove whether agent work has started.
#[derive(Debug)]
struct TurnNotSubmitted;
impl std::fmt::Display for TurnNotSubmitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Channel turn has not been submitted")
    }
}
impl std::error::Error for TurnNotSubmitted {}

fn defer_unsubmitted_turn(state: &mut ChannelState, id: &str, error: &anyhow::Error) -> bool {
    if !error.is::<TurnNotSubmitted>() {
        return false;
    }
    let Some(job) = state.jobs.iter_mut().find(|j| j.turn_id == id) else {
        return false;
    };
    job.state = "queued".into();
    state.notice = Some(WAITING_FOR_GATEWAY.into());
    true
}

/// One update for the chat: the coworker's in-between progress (commentary,
/// handoffs, returns, failures) or a turn's final answer.
struct MirrorEvent { turn: String, text: String, fin: bool, ask: Option<String> }
/// Follows the coworker's conversation for the life of the worker so the chat
/// gets constant updates. Rows replayed before the subscription's Pong are
/// history and never mirrored.
async fn mirror_feed(session: String, tx: tokio::sync::mpsc::Sender<MirrorEvent>) {
    use serde_json::Value;
    while !tx.is_closed() {
        let _ = async {
            // Display names ("Tibo"), keyed by agent id and internal role.
            let names: std::collections::HashMap<String, String> = crate::runtime::company::open_current_home_store()
                .and_then(|store| store.directory_snapshot())
                .map(|snapshot| snapshot.agents.into_iter().flat_map(|agent| {
                    let name = agent.profile.display_name.clone();
                    let mut keys = vec![(agent.profile.agent_id.clone(), name.clone()), (agent.profile.internal_role.clone(), name.clone())];
                    if agent.profile.agent_id == "phoenix" { keys.push(("orchestrator".into(), name)); }
                    keys
                }).collect())
                .unwrap_or_default();
            // Stories label agents as "Rory (audience_growth)"; show "Rory".
            let name = |id: &str| {
                let (label, role) = id.trim().strip_suffix(')').and_then(|rest| rest.rsplit_once(" (")).unwrap_or((id.trim(), id.trim()));
                names.get(role).or_else(|| names.get(label)).cloned()
                    .unwrap_or_else(|| if label != role { label.to_string() } else { crate::runtime::delegation::agent_display_name(id) })
            };
            let clip = |text: &str, limit: usize| { let mut out: String = text.trim().chars().take(limit).collect(); if text.trim().chars().count() > limit { out.push('…'); } out };
            let stream = UnixStream::connect(super::daemon::socket_path()).await?;
            let (read, mut keep_open) = stream.into_split();
            keep_open.write_all(format!("{}\n", json!({"SubscribeJournal":{"session_id":session}})).as_bytes()).await?;
            let mut reader = BufReader::new(read);
            loop {
                let value = journal::frame(&mut reader).await?;
                if value == json!("Pong") { break; }
                anyhow::ensure!(value.get("Error").is_none(), "Mirror subscription rejected");
            }
            let mut answers = std::collections::HashMap::<String, String>::new();
            loop {
                let value = journal::frame(&mut reader).await?;
                let story = &value["Story"];
                let turn = story["execution"]["turn_id"].as_str().unwrap_or("").to_string();
                let text = |key: &str| story[key].as_str().unwrap_or("").trim().to_string();
                let progress = match story["kind"].as_str() {
                    Some("commentary") | Some("narration") if !text("text").is_empty() =>
                        Some(format!("💭 {}: {}", name(&text("agent")), clip(&text("text"), 1500))),
                    Some("handoff") => Some(format!("➡️ {} asked {}: {}", name(&text("from")), name(&text("to")), clip(&text("subject"), 400))),
                    Some("return") => Some(format!("{} {} {}: {}", if story["ok"].as_bool() == Some(false) { "⚠️" } else { "↩️" }, name(&text("agent")),
                        if story["ok"].as_bool() == Some(false) { "hit a problem" } else { "returned" }, clip(&if text("body").is_empty() { text("subject") } else { text("body") }, 600))),
                    Some("failure") => Some(format!("⚠️ {}: {}", name(&text("agent")), clip(&text("text"), 400))),
                    // A mid-task message between agents (not the chat's own).
                    Some("steer") if !text("body").contains("[via ") => Some(format!("➡️ {} to {}: {}", name(&text("from")), name(&text("to")),
                        clip(&if text("body").is_empty() { text("subject") } else { text("body") }, 600))),
                    _ => None,
                };
                if story["kind"].as_str() == Some("ask_pending") {
                    if let Some(ask) = story["id"].as_str() {
                        if tx.send(MirrorEvent { turn: turn.clone(), text: String::new(), fin: false, ask: Some(ask.to_string()) }).await.is_err() { return Ok::<(), anyhow::Error>(()); }
                    }
                    continue;
                }
                if let Some(progress) = progress {
                    if tx.send(MirrorEvent { turn: turn.clone(), text: progress, fin: false, ask: None }).await.is_err() { return Ok::<(), anyhow::Error>(()); }
                    continue;
                }
                if turn.is_empty() || story["execution"]["task_id"].as_str() != Some(turn.as_str()) { continue; }
                match story["kind"].as_str() {
                    Some("answer") => { if let Some(markdown) = story["markdown"].as_str() { answers.insert(turn, markdown.to_string()); } }
                    Some("execution_ended") => {
                        let answer = answers.remove(&turn);
                        if let (true, Some(answer)) = (story.get("error").is_none_or(Value::is_null), answer) {
                            if tx.send(MirrorEvent { turn, text: answer, fin: true, ask: None }).await.is_err() { return Ok(()); }
                        }
                    }
                    _ => {}
                }
                if answers.len() > 64 { answers.clear(); }
            }
        }.await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
/// The latest human prompt in the conversation, if the turn that just ended
/// was started by a person typing in Phoenix. Channel messages, routines,
/// wakes and other internal prompts return None, so they are not mirrored.
fn mirrored_prompt(session: &str) -> Option<String> {
    let path = crate::config::phoenix_home().join("sessions").join(format!("{session}.json"));
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let text = value["messages"].as_array()?.iter().rev()
        .find(|message| message["type"].as_str() == Some("User"))?["content"].as_str()?.trim().to_string();
    let internal = text.starts_with('[') || text.starts_with("GOAL HEARTBEAT")
        || text.strip_prefix('@').and_then(|rest| rest.split_once(char::is_whitespace)).is_some_and(|(_, rest)| rest.trim_start().starts_with('['));
    if text.is_empty() || internal { return None; }
    let mut short: String = text.chars().take(500).collect();
    if text.chars().count() > 500 { short.push('…'); }
    Some(short)
}
#[cfg(test)]
async fn run_turn(config: ChannelConfig, turn_id: String, text: String) -> Result<String> {
    run_turn_with_questions(config, turn_id, text, None).await
}
async fn run_turn_with_questions(config: ChannelConfig, turn_id: String, text: String, updates: Option<questions::Updates>) -> Result<String> {
    use super::daemon::{TurnCompletion, WireRequest, WireResponse};
    use crate::runtime::CliEvent;
    // The subscription barrier precedes submission, including a fast queued
    // execution. Only that execution's owned final + terminal boundary count.
    let mut journal = journal::Journal::connect(&config.session_id).await.context(TurnNotSubmitted)?;
    let stream = UnixStream::connect(super::daemon::socket_path())
        .await
        .context(TurnNotSubmitted)?;
    let (read, mut write) = stream.into_split();
    let request: WireRequest = serde_json::from_value(json!({"Turn":{
        "session_id":config.session_id,"turn_id":turn_id,"user_request":text,
        // No permission_mode: the daemon applies the coworker's own access
        // (its sticky choice, else Default access), same as the app.
        "workspace":config.workspace,"target_agent":crate::channels::target_agent(&config),
        "target_group":config.group_id,
        "journal":false,"delivery":"queue"
    }}))?;
    write
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;
    let mut reader = BufReader::new(read);
    let outcome = async {
        loop {
            let mut bytes = Vec::new();
            let count = (&mut reader)
                .take(4 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .await?;
            anyhow::ensure!(
                count > 0 && count <= 4 * 1024 * 1024,
                "Gateway stream ended or exceeded its frame limit"
            );
            match serde_json::from_slice::<WireResponse>(&bytes)? {
                WireResponse::Event(CliEvent::AskUser { id, .. }) => {
                    if let Some(updates) = &updates { questions::publish(&config, &turn_id, &id, updates).await?; }
                }
                WireResponse::Done(summary) => {
                    anyhow::ensure!(summary.main_session_id == config.session_id, "Gateway replied for another conversation");
                    return match summary.completion {
                        // Terminal interruption still deserves a concise user
                        // notice. The channel projection strips raw details.
                        TurnCompletion::Completed | TurnCompletion::Incomplete => Ok(summary.final_markdown),
                        TurnCompletion::Queued => journal.final_answer(&config, &turn_id, &turn_id, updates.as_ref()).await,
                        // Sent while the agent was already working: delivered
                        // into that running task, whose own answer follows.
                        TurnCompletion::Steered => Ok("Got it — I've added that to what I'm working on now.".to_string()),
                        _ => anyhow::bail!("Gateway did not confirm completion or a correlated queued request"),
                    };
                }
                WireResponse::Error { .. } => anyhow::bail!("Gateway rejected the channel turn"),
                _ => {} // no tool events, worker returns, or thought fragments
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(7200), outcome)
        .await
        .context("Channel observation timed out; agent work remains in Phoenix")?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity_config(home: &Path) -> ChannelConfig {
        ChannelConfig { id:"capacity".into(), name:"Capacity fixture".into(), platform:Platform::Telegram,
            conversation_id:"123".into(), allowed_user_ids:vec!["456".into()], agent_id:"phoenix".into(),
            session_id:"capacity-session".into(), workspace:home.into(), token_env:"UNUSED_CAPACITY_TOKEN".into(), enabled:false, group_id:None }
    }

    // Seeds only the disposable packaged-UI test home. It does not create a
    // bot client, read a token, submit a task or change the user's channel data.
    #[test]
    #[ignore]
    fn channel_review_packaged_fixture() {
        let home = crate::config::phoenix_home();
        assert!(home.to_string_lossy().starts_with("/tmp/phoenix-ui-acceptance-"));
        assert_eq!(std::env::var("PHOENIX_CHANNEL_UI_FIXTURE").as_deref(), Ok("1"));
        let cfg = ChannelConfig { id:"ui-review-fixture".into(), name:"Review lab (test data)".into(),
            platform:Platform::Telegram, conversation_id:"123".into(), allowed_user_ids:vec!["456".into()],
            agent_id:"frontend".into(), session_id:"agent-frontend".into(), workspace:home.clone(),
            token_env:"UNUSED_PACKAGED_FIXTURE_TOKEN".into(), enabled:true, group_id:None  };
        assert!(!saved_path(&cfg.id).unwrap().exists(), "never overwrite an existing connection");
        save_config(&cfg).unwrap();
        let path = root(&cfg).join("state.json");
        let mut state = ChannelState::load(&path, &cfg).unwrap();
        state.cursor = Some("fixture-cursor-preserved".into());
        state.outbox = (0..27).map(|i| crate::channels::Delivery {
            id:format!("{i:024x}"), item:Outbound::Text {
                text:format!("Review item {}: the revised project plan is ready. Check the saved reply before marking it received. Synthetic integration data, not a real message.",i+1),
            }, state:"uncertain".into(), retry_at:0, remote_id:None,
        }).collect();
        state.save(&path).unwrap();
        let summary = status_summary(&saved_path(&cfg.id).unwrap()).unwrap();
        assert_eq!(summary["delivery"]["review_counts"]["deliveries"],27);
        std::fs::write(home.join("channel-ui-fixture.json"),serde_json::to_vec_pretty(&json!({
            "id":cfg.id,"connection":saved_path(&cfg.id).unwrap(),"first_id":state.outbox[0].id,
            "cursor":state.cursor,"count":27,"provider_calls":0,"platform_calls":0
        })).unwrap()).unwrap();
    }

    fn review_fixture(home: &Path, count: usize) -> (ChannelConfig, PathBuf, PathBuf, ChannelState) {
        let cfg = capacity_config(home);
        let connection = saved_path(&cfg.id).unwrap();
        std::fs::create_dir_all(connection.parent().unwrap()).unwrap();
        std::fs::write(&connection, serde_json::to_vec(&cfg).unwrap()).unwrap();
        let path = root(&cfg).join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut state = ChannelState::load(&path, &cfg).unwrap();
        state.cursor = Some("retained-provider-cursor".into());
        state.outbox = (0..count).map(|i| crate::channels::Delivery { id:format!("{i:024x}"),
            item:Outbound::Text{text:"x".into()},state:"uncertain".into(),retry_at:0,remote_id:None }).collect();
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        (cfg, connection, path, state)
    }

    #[test]
    fn channel_review_near_cap_transport_is_bounded_and_keeps_final_page() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (_cfg, connection, path, mut state) = review_fixture(home.path(), 160_000);
        let target = 64 * 1024 * 1024 - 10;
        let mut padding = target - serde_json::to_vec(&state).unwrap().len();
        for record in &mut state.outbox {
            let count = padding.min(2999);
            if let Outbound::Text{text} = &mut record.item { text.push_str(&"x".repeat(count)); }
            padding -= count;
            if padding == 0 { break; }
        }
        assert_eq!(padding, 0);
        let raw = serde_json::to_vec(&state).unwrap();
        assert_eq!(raw.len(), target);
        std::fs::write(&path, &raw).unwrap(); drop(state);
        let legacy = status(&connection).unwrap();
        assert!(serde_json::to_vec(&legacy).unwrap().len() > 8 * 1024 * 1024, "actual legacy CLI projection breaches native stdout");
        assert_eq!(legacy["delivery"]["review_deliveries"].as_array().unwrap().len(), 160_000);
        drop(legacy);
        let summary = status_summary(&connection).unwrap();
        assert_eq!(summary["delivery"]["capacity"]["paused"], true);
        assert_eq!(summary["delivery"]["review_counts"]["deliveries"], 160_000);
        assert!(summary["delivery"].get("review_deliveries").is_none());
        assert!(summary["delivery"].get("review_turns").is_none());
        assert!(serde_json::to_vec(&summary).unwrap().len() < 4096);
        assert!(serde_json::to_vec(&list_saved_summary().unwrap()).unwrap().len() < 4096);
        let first = review_page(&connection, "deliveries", 0, 10, None).unwrap();
        let snapshot = first["snapshot"].as_str().unwrap();
        let last = review_page(&connection, "deliveries", 159_990, 10, Some(snapshot)).unwrap();
        assert_eq!(last["total"], 160_000);
        assert_eq!(last["items"].as_array().unwrap().len(), 10);
        assert_eq!(last["items"][9]["id"], format!("{:024x}",159_999));
        assert!(last["next_offset"].is_null());
        assert!(serde_json::to_vec(&last).unwrap().len() < 16 * 1024);
        assert!(first["items"][0]["preview"]["truncated"].as_bool().unwrap());
        assert_eq!(first["items"][0]["preview"]["text"].as_str().unwrap(), "x".repeat(200));
        assert_eq!(std::fs::read(&path).unwrap(), raw, "summary and pages cannot save/prune the legacy state");
    }

    #[test]
    fn channel_review_snapshot_rejects_reorder_and_guards_exact_resolution() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (cfg, connection, path, mut state) = review_fixture(home.path(), 11);
        let initial = review_page(&connection, "deliveries", 0, 10, None).unwrap();
        let old_snapshot = initial["snapshot"].as_str().unwrap();
        state.outbox.swap(0, 10);
        let reordered = serde_json::to_vec(&state).unwrap();
        std::fs::write(&path, &reordered).unwrap();
        assert!(review_page(&connection, "deliveries", 10, 10, Some(old_snapshot)).is_err());
        assert!(resolve_delivery_checked(&connection, &format!("{:024x}",0), false, Some(old_snapshot)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), reordered);
        let fresh = review_page(&connection, "deliveries", 10, 10, None).unwrap();
        assert_eq!(fresh["items"][0]["id"], format!("{:024x}",0));
        let fresh_snapshot = fresh["snapshot"].as_str().unwrap();
        let selected = fresh["items"][0]["id"].as_str().unwrap();
        let _claim = crate::config::private_io::try_execution_claim(&root(&cfg).join("worker.lock")).unwrap().unwrap();
        assert!(resolve_delivery_checked(&connection, selected, false, Some(fresh_snapshot)).is_err(), "a running worker still owns state");
        drop(_claim);
        resolve_delivery_checked(&connection, selected, false, Some(fresh_snapshot)).unwrap();
        let settled = ChannelState::load(&path, &cfg).unwrap();
        assert_eq!(settled.outbox.iter().filter(|r|r.state=="sent").count(),1);
        assert_eq!(settled.outbox.iter().find(|r|r.state=="sent").unwrap().id,selected);
        assert_eq!(settled.cursor, state.cursor);
        assert!(resolve_delivery_checked(&connection, selected, true, Some(fresh_snapshot)).is_err(), "old action cannot be replayed after success");
        assert!(review_page(&connection, "deliveries", 10, 10, None).is_err());
        let remaining = review_page(&connection, "deliveries", 0, 10, None).unwrap();
        assert_eq!(remaining["total"],10);
        assert!(remaining["items"].as_array().unwrap().iter().all(|r|r["id"]!=selected));
    }

    #[test]
    fn channel_review_snapshot_binds_config_and_rejects_malformed_pages() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (mut cfg, connection, path, mut state) = review_fixture(home.path(), 2);
        let page = review_page(&connection, "deliveries", 0, 10, None).unwrap();
        let snapshot = page["snapshot"].as_str().unwrap();
        cfg.allowed_user_ids = vec!["999".into()];
        std::fs::write(&connection,serde_json::to_vec(&cfg).unwrap()).unwrap();
        assert!(review_page(&connection,"deliveries",0,10,Some(snapshot)).is_err());
        assert!(resolve_delivery_checked(&connection,&state.outbox[0].id,false,Some(snapshot)).is_err());
        for (kind,offset,limit,snapshot) in [("other",0,10,None),("turns",0,0,None),("turns",0,101,None),("deliveries",usize::MAX,10,None),("deliveries",0,10,Some("bad"))] {
            assert!(review_page(&connection,kind,offset,limit,snapshot).is_err());
        }
        state.outbox[1].id = "x".repeat(129);
        let original=serde_json::to_vec(&state).unwrap();
        std::fs::write(&path,&original).unwrap();
        assert_eq!(status_summary(&connection).unwrap()["delivery"]["review_counts"]["deliveries"],2);
        assert!(review_page(&connection,"deliveries",0,10,None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),original,"oversized identifiers are preserved, never truncated");
    }

    #[test]
    fn channel_review_turn_acknowledgement_requires_its_snapshot() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (cfg, connection, path, mut state) = review_fixture(home.path(), 1);
        state.jobs.push(crate::channels::Job { input:crate::channels::InboundMessage {id:"1".into(),user_id:"456".into(),text:"Review me".into(),reply_to:None, sender:None },turn_id:"channel_exact_turn".into(),state:"needs_review".into() });
        std::fs::write(&path,serde_json::to_vec(&state).unwrap()).unwrap();
        let page=review_page(&connection,"turns",0,10,None).unwrap();
        assert_eq!(page["items"][0]["turn_id"],"channel_exact_turn");
        assert_eq!(page["items"][0]["preview"]["text"],"Review me");
        let snapshot=page["snapshot"].as_str().unwrap();
        acknowledge_turn_checked(&connection,"channel_exact_turn",Some(snapshot)).unwrap();
        assert!(acknowledge_turn_checked(&connection,"channel_exact_turn",Some(snapshot)).is_err());
        let saved=ChannelState::load(&path,&cfg).unwrap();
        assert_eq!(saved.jobs[0].state,"done");assert_eq!(saved.outbox[0].state,"uncertain");
    }

    #[test]
    fn channel_review_response_budget_errors_preserve_oversized_records() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let (cfg, connection, path, mut state) = review_fixture(home.path(), 1);
        state.notice = Some("x".repeat(CHANNEL_UI_RESPONSE_BYTES));
        let raw = serde_json::to_vec(&state).unwrap();
        std::fs::write(&path, &raw).unwrap();
        assert!(status_summary(&connection).is_err());
        assert!(review_page(&connection,"deliveries",0,10,None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),raw);
        for i in 0..2 {
            let mut large = cfg.clone();
            large.id = format!("large{i}");
            large.agent_id = "x".repeat(70 * 1024);
            let config_path = saved_path(&large.id).unwrap();
            std::fs::write(config_path,serde_json::to_vec(&large).unwrap()).unwrap();
        }
        assert!(list_saved_summary().is_err(),"a list budget error must not silently omit connections");
        let legacy = list_saved().unwrap();
        assert_eq!(legacy["connections"].as_array().unwrap().len(),3,"all legacy records remain readable through the explicit CLI");
        assert_eq!(std::fs::read(path).unwrap(),raw);
    }

    #[test]
    fn channel_capacity_question_is_retained_until_published_and_stays_manually_recoverable() {
        use crate::channels::{InboundMessage, PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let cfg = capacity_config(home.path());
        let path = home.path().join("state.json");
        let mut state = ChannelState::load(&path,&cfg).unwrap();
        state.admit(PollBatch { cursor:Some("2".into()), messages:vec![InboundMessage {
            id:"1".into(),user_id:"456".into(),text:"Choose the exact finish".into(),reply_to:None, sender:None 
        }] }).unwrap();
        let id = state.jobs[0].turn_id.clone();
        state.jobs[0].state = "running".into();
        crate::runtime::asks::register_detached_with_payload("capacity-ask",&cfg.session_id,"Phoenix",
            &[crate::tools::ask_user::AskUserQuestion { question:"Which finish: e\u{301} or 漢字?".into(),header:None,options:vec![],multi_select:false }],None);
        let update = || questions::QuestionUpdate { job_id:id.clone(),ask_id:"capacity-ask".into(),text:"Which finish: e\u{301} or 漢字?".into() };
        let mut pending = Some(update());
        state.set_active_observers(16); // existing producers still own their reservations
        assert!(!publish_pending_question(&mut state,&mut pending).unwrap());
        assert_eq!(pending.as_ref().unwrap().ask_id,"capacity-ask");
        assert_eq!(pending.as_ref().unwrap().text,update().text);
        assert!(state.questions.is_empty() && state.outbox.is_empty());
        state.save(&path).unwrap();
        let mut restarted = ChannelState::load(&path,&cfg).unwrap();
        restarted.recover();
        assert_eq!(restarted.jobs[0].state,"needs_review");
        assert_eq!(crate::runtime::asks::decision_record_for("capacity-ask").unwrap().unwrap().status,"pending",
            "the exact question remains reviewable in Phoenix after worker restart");
        state.set_active_observers(0);
        assert!(publish_pending_question(&mut state,&mut pending).unwrap());
        assert!(pending.is_none());
        state.save(&path).unwrap();
        assert_eq!(state.questions[0].turn_id,id);
        assert_eq!(state.questions[0].user_id,"456");
        let count = state.outbox.len();
        let mut duplicate = Some(update());
        assert!(publish_pending_question(&mut state,&mut duplicate).unwrap());
        assert_eq!(state.outbox.len(),count);
        assert!(matches!(&state.outbox[0].item,Outbound::Question{text} if text==&update().text));
        assert!(state.questions[0].answered_by.is_none());
    }

    #[tokio::test]
    async fn channel_capacity_counts_finished_handles_and_retained_results() {
        let home = tempfile::tempdir().unwrap();
        let cfg = capacity_config(home.path());
        let mut state = ChannelState::load(&home.path().join("state.json"),&cfg).unwrap();
        let active = Some(("owner".into(),tokio::spawn(async { std::future::pending::<Result<String>>().await })));
        let mut answers = vec![Some(("answer".into(),tokio::spawn(async { Ok("Ready".to_string()) }))),None];
        tokio::task::yield_now().await;
        assert!(answers[0].as_ref().unwrap().1.is_finished());
        assert_eq!(observer_count(&active,&answers,1),3);
        state.set_active_observers(observer_count(&active,&answers,1));
        assert!(!state.can_start(),"finished-but-uncollected handles still need room");
        let (_,ready) = answers[0].take().unwrap();
        assert_eq!(ready.await.unwrap().unwrap(),"Ready");
        state.set_active_observers(observer_count(&active,&answers,2));
        assert!(!state.can_start(),"collecting a handle transfers its reservation to the retained output");
        state.set_active_observers(observer_count(&active,&answers,1));
        assert!(state.can_start(),"only publishing that exact result releases its room");
        let rejected_batch_at = Some(state.capacity_stamp());
        for _ in 0..100 { assert!(!intake_ready(&state,rejected_batch_at)); }
        state.admit(crate::channels::PollBatch { cursor:Some("2".into()),messages:vec![] }).unwrap();
        assert!(intake_ready(&state,rejected_batch_at),"an actual state mutation permits a fresh capacity check");
        let (_,running) = active.unwrap(); running.abort(); let _ = running.await;
    }

    #[test]
    fn channel_capacity_delivery_identity_survives_archiving_before_send() {
        use crate::channels::Delivery;
        let home = tempfile::tempdir().unwrap();
        let cfg = capacity_config(home.path());
        let path = home.path().join("state.json");
        let mut state = ChannelState::load(&path,&cfg).unwrap();
        for index in 0..140 {
            state.outbox.push(Delivery { id:format!("old-{index}"),item:Outbound::Text{text:"already delivered".into()},
                state:"sent".into(),retry_at:0,remote_id:Some(index.to_string()) });
        }
        for id in ["selected","following"] {
            state.outbox.push(Delivery { id:id.into(),item:Outbound::Text{text:id.into()},state:"pending".into(),retry_at:0,remote_id:None });
        }
        let sent = prepare_delivery(&mut state,&path).unwrap().unwrap();
        assert_eq!(sent.id,"selected");
        assert!(matches!(&sent.item,Outbound::Text{text} if text=="selected"));
        assert_eq!(state.outbox.len(),66,"save really removed seventy-six prior sent rows");
        assert_eq!(state.outbox[64].id,"selected");
        let on_disk = ChannelState::load(&path,&cfg).unwrap();
        assert_eq!(on_disk.outbox[64].state,"sending");
        assert!(prepare_delivery(&mut state,&path).unwrap().is_none(),"no second send while receipt is unresolved");
        settle_delivery(&mut state,&sent.id,Ok("remote-selected".into())).unwrap();
        assert!(settle_delivery(&mut state,&sent.id,Ok("duplicate".into())).is_err());
        state.save(&path).unwrap();
        let next = prepare_delivery(&mut state,&path).unwrap().unwrap();
        assert_eq!(next.id,"following");
        settle_delivery(&mut state,&next.id,Err(SendFailure::Uncertain)).unwrap();
        state.save(&path).unwrap();
        let mut reopened = ChannelState::load(&path,&cfg).unwrap(); reopened.recover();
        assert!(prepare_delivery(&mut reopened,&path).unwrap().is_none());
        assert_eq!(reopened.outbox.last().unwrap().id,"following");
        assert_eq!(reopened.outbox.last().unwrap().state,"uncertain");
        assert_eq!(reopened.outbox.iter().find(|d|d.id=="selected").unwrap().remote_id.as_deref(),Some("remote-selected"));
    }

    #[test]
    fn channel_capacity_recovery_stops_at_full_result_without_settling_later_jobs() {
        use crate::channels::{InboundMessage,PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let cfg = capacity_config(home.path());
        let path = home.path().join("state.json");
        let mut state = ChannelState::load(&path,&cfg).unwrap();
        state.admit(PollBatch { cursor:Some("4".into()),messages:(1..=3).map(|i|InboundMessage {
            id:i.to_string(),user_id:"456".into(),text:format!("Work {i}"),reply_to:None, sender:None 
        }).collect() }).unwrap();
        for job in &mut state.jobs { job.state="needs_review".into(); }
        for (index,job) in state.jobs.iter().enumerate() {
            let key = super::super::channel_receipts::Key::for_job(&cfg,job).unwrap();
            let text = if index==1 { "\u{1}".repeat(crate::channels::MAX_REPLY_BYTES) } else { format!("Result {index}") };
            super::super::channel_receipts::save(&key,&text).unwrap();
        }
        let results = super::super::channel_receipts::recoverable(&cfg,&state.jobs,&state.questions,&home.path().join("images")).unwrap();
        assert_eq!(results.len(),3);
        state.set_active_observers(3);
        assert_eq!(apply_recovered(&mut state,results.clone()).unwrap(),1);
        assert_eq!(state.jobs.iter().map(|job|job.state.as_str()).collect::<Vec<_>>(),["done","needs_review","needs_review"]);
        assert_eq!(state.outbox.len(),1);
        state.save(&path).unwrap();
        let mut restarted = ChannelState::load(&path,&cfg).unwrap();
        let remaining = super::super::channel_receipts::recoverable(&cfg,&restarted.jobs,&restarted.questions,&home.path().join("images")).unwrap();
        assert_eq!(apply_recovered(&mut restarted,remaining).unwrap(),2);
        restarted.save(&path).unwrap();
        let count = restarted.outbox.len();
        assert_eq!(apply_recovered(&mut restarted,results).unwrap(),0);
        assert_eq!(restarted.outbox.len(),count);
        assert!(restarted.jobs.iter().all(|job|job.state=="done"));
        assert_eq!(restarted.outbox.iter().map(|d|&d.id).collect::<std::collections::HashSet<_>>().len(),count);
        assert_eq!(restarted.cursor.as_deref(),Some("4"));
    }

    #[test]
    fn channel_capacity_legacy_file_remains_readable_and_manual_controls_shrink_it() {
        use crate::channels::{Delivery,InboundMessage,PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let cfg = capacity_config(home.path());
        let connection = home.path().join("connection.json");
        std::fs::write(&connection,serde_json::to_vec(&cfg).unwrap()).unwrap();
        let path = root(&cfg).join("state.json");
        let mut state = ChannelState::load(&path,&cfg).unwrap();
        state.admit(PollBatch { cursor:Some("3".into()),messages:(1..=2).map(|id|InboundMessage {
            id:id.to_string(),user_id:"456".into(),text:"Retain this exact input".into(),reply_to:None, sender:None 
        }).collect() }).unwrap();
        state.jobs[0].state="running".into();
        let turn = state.jobs[0].turn_id.clone();
        let target = 64 * 1024 * 1024 - 10;
        let mut bytes = serde_json::to_vec(&state).unwrap().len();
        while target - bytes > 256 {
            let mut row = Delivery { id:format!("{:024x}",state.outbox.len()),item:Outbound::Text{text:String::new()},
                state:if state.outbox.is_empty(){"uncertain"}else{"pending"}.into(),retry_at:0,remote_id:None };
            let overhead = serde_json::to_vec(&row).unwrap().len()+usize::from(!state.outbox.is_empty());
            let n = 3000.min(target-bytes-overhead);
            if let Outbound::Text{text}=&mut row.item { *text="x".repeat(n); }
            bytes += overhead+n; state.outbox.push(row);
        }
        if let Outbound::Text{text}=&mut state.outbox.last_mut().unwrap().item { text.push_str(&"x".repeat(target-bytes)); }
        assert_eq!(serde_json::to_vec(&state).unwrap().len(),target);
        // Old supported files need not contain the defaulted questions field.
        // A no-op save must not reintroduce an empty optional field at the cap.
        let mut legacy = serde_json::to_value(&state).unwrap();
        legacy.as_object_mut().unwrap().remove("questions");
        let gap = target - serde_json::to_vec(&legacy).unwrap().len();
        let tail = legacy["outbox"].as_array_mut().unwrap().last_mut().unwrap();
        let text = tail["item"]["text"].as_str().unwrap().to_owned() + &"x".repeat(gap);
        tail["item"]["text"] = json!(text);
        let raw = serde_json::to_vec(&legacy).unwrap();
        assert_eq!(raw.len(),target);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path,raw).unwrap();
        let report = status(&connection).unwrap();
        assert_eq!(report["delivery"]["capacity"]["paused"],true);
        assert!(report["delivery"]["notice"].as_str().unwrap().contains("delivery needs review"));
        let count = state.outbox.len(); drop(state);
        acknowledge_turn(&connection,&turn).unwrap();
        let mut saved = ChannelState::load(&path,&cfg).unwrap();
        assert_eq!(saved.jobs.iter().find(|job| job.turn_id == turn).unwrap().state,"done");
        assert_eq!(saved.outbox.len(),count,"acknowledgement preserves all unresolved deliveries");
        // A subsequent genuine control update must still be able to archive
        // this newly settled job below the ordinary sixty-four-record window.
        saved.notice=Some("The interrupted turn was acknowledged. Pending deliveries retain their original identities.".into());
        saved.save(&path).unwrap();
        let mut saved = ChannelState::load(&path,&cfg).unwrap();
        assert_eq!(saved.jobs.len(),1); assert_eq!(saved.jobs[0].state,"queued");
        assert_eq!(saved.jobs[0].input.id,"2");
        assert!(path.with_extension("history.sqlite").exists());
        saved.admit(PollBatch { cursor:Some("3".into()),messages:vec![InboundMessage {
            id:"1".into(),user_id:"456".into(),text:"Retain this exact input".into(),reply_to:None, sender:None 
        }] }).unwrap();
        assert_eq!(saved.jobs.len(),1,"the acknowledged input still deduplicates after archival/reload");
        saved.complete(&turn,vec![Outbound::Text{text:"do not send again".into()}]).unwrap();
        assert_eq!(saved.cursor.as_deref(),Some("3"));
        assert_eq!(saved.outbox.len(),count);
        assert!(saved.outbox.iter().skip(1).all(|d|d.state=="pending"));
        let id = saved.outbox[0].id.clone(); drop(saved);
        resolve_delivery(&connection,&id,false).unwrap();
        let saved = ChannelState::load(&path,&cfg).unwrap();
        assert_eq!(saved.outbox[0].state,"sent"); assert!(saved.outbox[0].remote_id.is_none());
        assert_eq!(saved.outbox.len(),count);
        assert!(std::fs::metadata(path).unwrap().len() < target as u64);
    }

    #[test]
    fn channel_capacity_pending_only_legacy_backlog_can_connect_and_drain() {
        use crate::channels::Delivery;
        let home = tempfile::tempdir().unwrap();
        let cfg = capacity_config(home.path());
        let path = home.path().join("state.json");
        let mut state = ChannelState::load(&path, &cfg).unwrap();
        state.cursor = Some("retained-provider-cursor".into());
        let target = 64 * 1024 * 1024 - 10;
        let mut bytes = serde_json::to_vec(&state).unwrap().len();
        while target - bytes > 256 {
            let mut delivery = Delivery { id:format!("{:024x}", state.outbox.len()),
                item:Outbound::Text { text:String::new() }, state:"pending".into(),
                retry_at:0, remote_id:None };
            let overhead = serde_json::to_vec(&delivery).unwrap().len() + usize::from(!state.outbox.is_empty());
            let count = 3000.min(target - bytes - overhead);
            if let Outbound::Text { text } = &mut delivery.item { *text = "x".repeat(count); }
            bytes += overhead + count; state.outbox.push(delivery);
        }
        let mut legacy = serde_json::to_value(&state).unwrap();
        legacy.as_object_mut().unwrap().remove("questions");
        let padding = target - serde_json::to_vec(&legacy).unwrap().len();
        let tail = legacy["outbox"].as_array_mut().unwrap().last_mut().unwrap();
        tail["item"]["text"] = json!(tail["item"]["text"].as_str().unwrap().to_owned() + &"x".repeat(padding));
        assert!(legacy["outbox"].as_array().unwrap().iter().all(|d| d["item"]["text"].as_str().unwrap().len() <= 4096));
        let raw = serde_json::to_vec(&legacy).unwrap();
        assert_eq!(raw.len(), target);
        std::fs::write(&path, raw).unwrap();
        let mut restored = ChannelState::load(&path, &cfg).unwrap();
        let first = restored.outbox[0].id.clone();
        let next = restored.outbox[1].id.clone();
        restored.recover();
        restored.save(&path).expect("startup must preserve a readable pending-only legacy file so its sender can drain it");
        assert!(!restored.can_start(), "new model work remains paused");
        let selected = prepare_delivery(&mut restored, &path).unwrap().unwrap();
        assert_eq!(selected.id, first);
        settle_delivery(&mut restored, &first, Ok("12345678901234567890".into())).unwrap();
        restored.save(&path).unwrap();
        let mut reopened = ChannelState::load(&path, &cfg).unwrap();
        assert_eq!(reopened.cursor.as_deref(), Some("retained-provider-cursor"));
        assert_eq!(prepare_delivery(&mut reopened, &path).unwrap().unwrap().id, next);
    }

    #[test]
    fn completed_reply_recovers_text_and_image_once_without_replaying_uncertain_delivery() {
        use crate::channels::{Delivery, InboundMessage, PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = ChannelConfig { id:"test".into(), name:"Test".into(), platform:Platform::Telegram,
            conversation_id:"1".into(), allowed_user_ids:vec!["2".into()], agent_id:"phoenix".into(),
            session_id:"session".into(), workspace:home.path().into(), token_env:"BOT_TOKEN".into(), enabled:true, group_id:None  };
        let path = home.path().join("state.json");
        let mut state = ChannelState::load(&path,&config).unwrap();
        state.admit(PollBatch { cursor:Some("3".into()), messages:vec![
            InboundMessage { id:"1".into(), user_id:"2".into(), text:"Make the image".into(), reply_to:None, sender:None  },
            InboundMessage { id:"2".into(), user_id:"2".into(), text:"Unknown result".into(), reply_to:None, sender:None  },
        ] }).unwrap();
        for job in &mut state.jobs { job.state="running".into(); }
        let id=state.jobs[0].turn_id.clone();
        let key=super::super::channel_receipts::Key::for_job(&config,&state.jobs[0]).unwrap();
        let image_path=home.path().join("final result.png");
        image::RgbImage::new(8,8).save(&image_path).unwrap();
        super::super::channel_receipts::save(&key,"Finished. ![Result](final%20result.png)").unwrap();
        let failure=crate::channels::digest(&format!("{id}-error"))[..24].to_string();
        state.outbox.push(Delivery { id:failure.clone(),item:Outbound::Text{text:"failure".into()},state:"pending".into(),retry_at:0,remote_id:None });
        state.outbox.push(Delivery { id:"previous-unknown-send".into(),item:Outbound::Text{text:"previous".into()},state:"sending".into(),retry_at:0,remote_id:None });
        state.save(&path).unwrap();
        let mut restarted=ChannelState::load(&path,&config).unwrap();restarted.recover();
        let original_image=std::fs::read(&image_path).unwrap();
        std::fs::write(&image_path,b"source was overwritten after completion").unwrap();
        let results=super::super::channel_receipts::recoverable(&config,&restarted.jobs,&restarted.questions,&home.path().join("outbox-images")).unwrap();
        assert_eq!(results.len(),1);
        let saved_image=results[0].1.iter().find_map(|o|if let Outbound::Image{path,..}=o{Some(path)}else{None}).unwrap();
        assert_eq!(std::fs::read(saved_image).unwrap(),original_image);
        restarted.set_active_observers(16);
        let count = restarted.outbox.len();
        assert_eq!(apply_recovered(&mut restarted,results.clone()).unwrap(),0,"capacity rejection is not a fatal recovery error");
        assert_eq!(restarted.jobs[0].state,"needs_review");
        assert_eq!(restarted.outbox.len(),count);
        restarted.save(&path).unwrap();
        restarted.set_active_observers(0);
        assert_eq!(apply_recovered(&mut restarted,results).unwrap(),1);
        #[cfg(unix)] {
            // Simulate a failed publication after loading a valid snapshot.
            // The old owner remains reviewable; recovery must use frozen bytes,
            // even though the mutable source image was already overwritten.
            let original_state = std::fs::read(&path).unwrap();
            let backup = home.path().join("before-failed-save.json");
            std::fs::rename(&path,&backup).unwrap();
            std::os::unix::fs::symlink(&backup,&path).unwrap();
            assert!(restarted.save(&path).is_err());
            assert_eq!(std::fs::read(&backup).unwrap(),original_state);
            std::fs::remove_file(&path).unwrap(); std::fs::rename(&backup,&path).unwrap();
            restarted = ChannelState::load(&path,&config).unwrap(); restarted.recover();
            assert_eq!(restarted.jobs[0].state,"needs_review");
            let recovered = super::super::channel_receipts::recoverable(&config,&restarted.jobs,&restarted.questions,&home.path().join("outbox-images")).unwrap();
            assert_eq!(apply_recovered(&mut restarted,recovered).unwrap(),1);
        }
        restarted.save(&path).unwrap();
        let mut twice=ChannelState::load(&path,&config).unwrap();twice.recover();
        assert_eq!(twice.jobs[0].state,"done");assert_eq!(twice.jobs[1].state,"needs_review");
        assert!(!twice.outbox.iter().any(|d|d.id==failure));
        assert_eq!(twice.outbox[0].state,"uncertain");
        assert!(twice.outbox.iter().any(|d|matches!(&d.item,Outbound::Text{text} if text.contains("Finished"))));
        assert!(twice.outbox.iter().any(|d|matches!(&d.item,Outbound::Image{..})));
        let count=twice.outbox.len();
        assert_eq!(apply_recovered(&mut twice,vec![(id,vec![Outbound::Text{text:"duplicate".into()}])]).unwrap(),0);
        assert_eq!(twice.outbox.len(),count);
    }
    #[test]
    fn detached_question_reply_recovers_after_process_restart_without_resubmission() {
        use super::super::{channel_receipts as receipts, turn_queue as queue};
        use crate::channels::{InboundMessage, PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = ChannelConfig { id:"test".into(), name:"Test".into(), platform:Platform::Telegram,
            conversation_id:"1".into(), allowed_user_ids:vec!["2".into()], agent_id:"avery".into(),
            session_id:"question-recovery".into(), workspace:home.path().into(), token_env:"BOT_TOKEN".into(), enabled:true, group_id:None  };
        let state_path = home.path().join("channel-state.json");
        let mut state = ChannelState::load(&state_path, &config).unwrap();
        state.admit(PollBatch { cursor:None, messages:vec![
            InboundMessage { id:"request".into(), user_id:"2".into(), text:"Make an image".into(), reply_to:None, sender:None  },
            InboundMessage { id:"answer".into(), user_id:"2".into(), text:"Blue".into(), reply_to:Some("remote-question".into()), sender:None  },
        ] }).unwrap();
        let original = state.jobs[0].turn_id.clone();
        state.jobs[0].state = "done".into();
        state.jobs[1].state = "needs_review".into();
        state.queue_question(&original, "ask-channel-recovery", "Which color?").unwrap();
        state.questions[0].answered_by = Some("answer".into());
        let ask = &state.questions[0].ask_id;
        crate::runtime::asks::register_detached_with_payload(ask, &config.session_id, "Avery", &[], None);
        let spool = home.path().join("outbox-images");
        let recover = |cfg: &ChannelConfig, current: &ChannelState| receipts::recoverable(cfg, &current.jobs, &current.questions, &spool);
        assert!(recover(&config, &state).unwrap().is_empty()); // still pending
        crate::runtime::asks::archive_late_answer(ask, "Blue");
        assert!(recover(&config, &state).unwrap().is_empty()); // no frozen envelope
        assert!(queue::existing_answer_turn(&config.session_id, ask, "Blue").unwrap().is_none());
        let turn = queue::frozen_answer_turn(&config.session_id, ask, "Blue", || Ok(queue::QueuedUserTurn {
            turn_id:Some(format!("ask_answer_{}", "b".repeat(64))),
            user_request:"Resume the saved work using the user's blue selection".into(),
            origin:Some(crate::runtime::TurnOrigin::AskAnswer { ask_id:ask.clone(), agent_id:Some("avery".into()), display:"Blue".into() }),
            interaction_mode:crate::runtime::InteractionMode::Execute,
            permission_mode:Some(crate::tools::PermissionMode::Workspace), workspace:Some(home.path().into()),
            target_agent:Some("avery".into()), target_group:None, group_activation:None,
            yolo:None, sticky_notes:None, viewport:None, attachments:None,
        })).unwrap();
        let key = receipts::Key::new(&config.session_id, turn.turn_id.as_deref(), turn.target_agent.as_deref(), turn.workspace.as_deref(), &turn.user_request).unwrap();
        let source = home.path().join("result.png");
        image::RgbImage::new(8,8).save(&source).unwrap();
        let original_image = std::fs::read(&source).unwrap();
        receipts::save(&key, "Blue result. ![Result](result.png)").unwrap();
        assert!(recover(&config, &state).unwrap().is_empty()); // no submission receipt
        let queued = queue::enqueue(&config.session_id, &turn).unwrap();
        assert_eq!(queue::claim_next(&config.session_id).unwrap().unwrap().queue_id, queued);
        queue::complete(&queued).unwrap();
        assert!(queue::list(&config.session_id).unwrap().is_empty());
        std::fs::write(&source, b"source changed later").unwrap();
        let output = recover(&config, &state).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].0, state.jobs[1].turn_id);
        let image = output[0].1.iter().find_map(|o| if let Outbound::Image{path,..}=o { Some(path) } else { None }).unwrap();
        assert_eq!(std::fs::read(image).unwrap(), original_image);
        for field in ["sender", "answer", "question", "unlinked"] {
            let mut wrong = state.clone();
            match field {
                "sender" => wrong.questions[0].user_id = "another-user".into(),
                "answer" => wrong.jobs[1].input.text = "Red".into(),
                "question" => wrong.questions[0].ask_id = "another-question".into(),
                _ => wrong.questions[0].answered_by = None,
            }
            assert!(recover(&config, &wrong).map(|v| v.is_empty()).unwrap_or(true), "{field}");
        }
        for field in ["agent", "workspace", "session", "revoked"] {
            let mut wrong = config.clone();
            match field {
                "agent" => wrong.agent_id = "theo".into(),
                "workspace" => wrong.workspace = home.path().join("other"),
                "session" => wrong.session_id = "another-session".into(),
                _ => wrong.allowed_user_ids.clear(),
            }
            assert!(recover(&wrong, &state).unwrap().is_empty(), "{field}");
        }
        state.jobs[1].state = "running".into();
        state.save(&state_path).unwrap();
        crate::config::private_io::atomic_write_private(&home.path().join("fixture-config.json"), &serde_json::to_vec(&config).unwrap()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::channels::tests::question_recovery_child", "--ignored", "--nocapture"])
            .env("PHOENIX_CHANNEL_RECOVERY_FIXTURE", home.path()).output().unwrap();
        assert!(child.status.success(), "{}\n{}", String::from_utf8_lossy(&child.stdout), String::from_utf8_lossy(&child.stderr));
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        let saved = ChannelState::load(&state_path, &config).unwrap();
        assert_eq!(saved.jobs[1].state, "done");
        let image = saved.outbox.iter().find_map(|d| if let Outbound::Image{path,..}=&d.item { Some(path) } else { None }).unwrap();
        assert_eq!(std::fs::read(image).unwrap(), original_image);
        assert!(queue::list(&config.session_id).unwrap().is_empty());
    }

    #[test]
    fn foreground_question_ack_recovers_without_stealing_the_original_final() {
        use super::super::{channel_receipts as receipts, turn_queue as queue};
        use crate::channels::{InboundMessage, PollBatch};
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = ChannelConfig { id:"foreground".into(), name:"Test".into(), platform:Platform::Discord,
            conversation_id:"1".into(), allowed_user_ids:vec!["2".into()], agent_id:"avery".into(),
            session_id:"foreground-recovery".into(), workspace:home.path().into(), token_env:"BOT_TOKEN".into(), enabled:true, group_id:None  };
        let path = home.path().join("channel-state.json");
        let mut state = ChannelState::load(&path, &config).unwrap();
        state.admit(PollBatch { cursor:None, messages:vec![
            InboundMessage { id:"request".into(), user_id:"2".into(), text:"Make an image".into(), reply_to:None, sender:None  },
            InboundMessage { id:"answer".into(), user_id:"2".into(), text:"Blue".into(), reply_to:Some("remote-question".into()), sender:None  },
        ] }).unwrap();
        let original = state.jobs[0].turn_id.clone();
        let reply = state.jobs[1].turn_id.clone();
        state.queue_question(&original, "ask-foreground-recovery", "Which color?").unwrap();
        state.outbox[0].state = "sent".into();
        state.outbox[0].remote_id = Some("remote-question".into());
        state.questions[0].answered_by = Some("answer".into());
        for job in &mut state.jobs { job.state = "needs_review".into(); }
        let mut receiver = crate::runtime::asks::register_with_payload(
            "ask-foreground-recovery", &config.session_id, "Avery", &[], None);
        let spool = home.path().join("images");
        let recover = |cfg: &ChannelConfig, current: &ChannelState| receipts::recoverable(cfg, &current.jobs, &current.questions, &spool);
        assert!(recover(&config, &state).unwrap().is_empty());
        assert!(crate::runtime::asks::answer("ask-foreground-recovery", "Blue".into()));
        assert_eq!(receiver.try_recv().unwrap(), "Blue");
        let result = recover(&config, &state).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, reply);
        assert!(result[0].1.is_empty(), "the foreground answer job owns no final message");
        for field in ["sender", "answer", "question", "unlinked"] {
            let mut wrong = state.clone();
            match field {
                "sender" => wrong.questions[0].user_id = "another-user".into(),
                "answer" => wrong.jobs[1].input.text = "Red".into(),
                "question" => wrong.questions[0].ask_id = "missing-question".into(),
                _ => wrong.questions[0].answered_by = None,
            }
            assert!(recover(&config, &wrong).map(|v| v.is_empty()).unwrap_or(true), "{field}");
        }
        for field in ["agent", "workspace", "session", "destination", "platform", "revoked"] {
            let mut wrong = config.clone();
            match field {
                "agent" => wrong.agent_id = "theo".into(),
                "workspace" => wrong.workspace = home.path().join("other"),
                "session" => wrong.session_id = "another-session".into(),
                "destination" => wrong.conversation_id = "3".into(),
                "platform" => wrong.platform = Platform::Telegram,
                _ => wrong.allowed_user_ids.clear(),
            }
            assert!(recover(&wrong, &state).unwrap().is_empty(), "{field}");
        }
        // A fresh worker must reconcile the lost answer acknowledgement while
        // leaving the original request reviewable until its final is known.
        for job in &mut state.jobs { job.state = "running".into(); }
        state.save(&path).unwrap();
        crate::config::private_io::atomic_write_private(&home.path().join("fixture-config.json"), &serde_json::to_vec(&config).unwrap()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::channels::tests::question_recovery_child", "--ignored", "--nocapture"])
            .env("PHOENIX_CHANNEL_RECOVERY_FIXTURE", home.path()).output().unwrap();
        assert!(child.status.success(), "{}\n{}", String::from_utf8_lossy(&child.stdout), String::from_utf8_lossy(&child.stderr));
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        let mut restarted = ChannelState::load(&path, &config).unwrap();
        assert_eq!(restarted.jobs[0].state, "needs_review");
        assert_eq!(restarted.jobs[1].state, "done");
        assert_eq!(restarted.outbox.len(), 1); // only the already sent question
        assert!(!restarted.notice.as_deref().unwrap_or_default().contains("completed"));
        let key = receipts::Key::for_job(&config, &restarted.jobs[0]).unwrap();
        receipts::save(&key, "Here is the blue image.").unwrap();
        let result = recover(&config, &restarted).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, original);
        assert_eq!(apply_recovered(&mut restarted, result).unwrap(), 1);
        assert_eq!(restarted.outbox.len(), 2);
        assert!(matches!(&restarted.outbox[1].item, Outbound::Text{text} if text == "Here is the blue image."));
        assert!(recover(&config, &restarted).unwrap().is_empty());
        assert!(queue::existing_answer_turn(&config.session_id, "ask-foreground-recovery", "Blue").unwrap().is_none());
        assert!(queue::list(&config.session_id).unwrap().is_empty());
    }

    #[test]
    #[ignore = "fresh-process helper launched by question recovery tests"]
    fn question_recovery_child() {
        use super::super::{channel_receipts as receipts, turn_queue as queue};
        let home = PathBuf::from(std::env::var_os("PHOENIX_CHANNEL_RECOVERY_FIXTURE").expect("fixture"));
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(&home);
        let config: ChannelConfig = serde_json::from_slice(&std::fs::read(home.join("fixture-config.json")).unwrap()).unwrap();
        let path = home.join("channel-state.json");
        let mut state = ChannelState::load(&path, &config).unwrap();
        state.recover();
        assert_eq!(state.jobs[1].state, "needs_review");
        let result = receipts::recoverable(&config, &state.jobs, &state.questions, &home.join("restarted-images")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(apply_recovered(&mut state, result.clone()).unwrap(), 1);
        let count = state.outbox.len();
        assert_eq!(apply_recovered(&mut state, result).unwrap(), 0);
        assert_eq!(state.outbox.len(), count);
        state.save(&path).unwrap();
        assert!(queue::list(&config.session_id).unwrap().is_empty());
    }

    #[test]
    fn asynchronous_send_receipts_preserve_order_and_uncertainty_across_restart() {
        let root=tempfile::tempdir().unwrap();
        let config=ChannelConfig{id:"test".into(),name:"Test".into(),platform:Platform::Telegram,conversation_id:"1".into(),allowed_user_ids:vec!["2".into()],agent_id:"phoenix".into(),session_id:"session".into(),workspace:root.path().into(),token_env:"BOT_TOKEN".into(),enabled:true, group_id:None };
        let path=root.path().join("state.json");
        let mut state=ChannelState::load(&path,&config).unwrap();
        for (id,status) in [("previous","sent"),("image","sending"),("next","pending")] {
            state.outbox.push(crate::channels::Delivery{id:id.into(),item:Outbound::Text{text:id.into()},state:status.into(),retry_at:0,remote_id:None});
        }
        settle_delivery(&mut state,"image",Err(SendFailure::RetryAfter(Duration::from_secs(30)))).unwrap();
        assert_eq!(state.outbox[1].state,"pending");assert!(state.outbox[1].retry_at>=now()+30);
        assert_eq!(state.outbox.iter().position(|d|d.state!="sent"),Some(1));
        state.outbox[1].state="sending".into();
        settle_delivery(&mut state,"image",Err(SendFailure::Uncertain)).unwrap();
        state.save(&path).unwrap();let mut recovered=ChannelState::load(&path,&config).unwrap();recovered.recover();
        assert_eq!(recovered.outbox[1].state,"uncertain");assert_eq!(recovered.outbox[2].state,"pending");
        assert_eq!(recovered.outbox.iter().position(|d|d.state!="sent"),Some(1));
        assert!(settle_delivery(&mut recovered,"next",Ok("wrong".into())).is_err());
        recovered.outbox[1].state="sending".into();settle_delivery(&mut recovered,"image",Ok("remote-42".into())).unwrap();
        assert_eq!(recovered.outbox[1].remote_id.as_deref(),Some("remote-42"));
        assert_eq!(recovered.outbox.iter().position(|d|d.state!="sent"),Some(2));
    }

    #[test]
    fn managed_channel_reuses_saved_login_and_removal_forgets_it() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = ChannelConfig { id:"remembered".into(), name:"Saved bot".into(), platform:Platform::Discord,
            conversation_id:"123".into(), allowed_user_ids:vec!["456".into()], agent_id:"avery".into(), session_id:"agent-avery".into(),
            workspace:home.path().to_path_buf(), token_env:"PHOENIX_TEST_CHANNEL_TOKEN_NOT_SET".into(), enabled:true, group_id:None  };
        let vault = crate::security::vault::Vault::at(home.path());
        vault.initialize("a sufficiently long test master password").unwrap();
        save_config(&config).unwrap();
        crate::channels::credentials::save(home.path(), &config, "saved-discord-token").unwrap();
        assert_eq!(read_token(&config, false).unwrap().as_str(), "saved-discord-token");
        // A locked Passes store keeps the token sealed until one unlock.
        vault.lock();
        assert!(read_token(&config, false).is_err());
        assert_eq!(status(&saved_path(&config.id).unwrap()).unwrap()["saved_login"], true);
        assert!(!list_saved().unwrap().to_string().contains("saved-discord-token"));
        remove_saved(&config.id).unwrap();
        assert!(!crate::channels::credentials::saved(home.path(), &config).unwrap());
        assert!(vault.list_for_agent(&[crate::security::vault::CredentialScope::agent("avery")]).unwrap().is_empty());
    }

    #[test]
    fn managed_channel_edit_and_recovery_preserve_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
        let mut config = ChannelConfig {
            id: "personal".into(),
            name: "My chat".into(),
            platform: Platform::Telegram,
            conversation_id: "123".into(),
            allowed_user_ids: vec!["123".into()],
            agent_id: "avery".into(),
            session_id: "agent-avery".into(),
            workspace: dir.path().to_path_buf(),
            token_env: "BOT_TOKEN".into(),
            enabled: true, group_id:None 
        };
        save_config(&config).unwrap();
        let list = list_saved().unwrap();
        assert_eq!(list["connections"][0]["config"]["name"], "My chat");
        assert_eq!(list["connections"][0]["status"]["running"], false);
        let claim =
            crate::config::private_io::try_execution_claim(&root(&config).join("worker.lock"))
                .unwrap();
        assert!(save_config(&config).is_err());
        assert!(remove_saved(&config.id).is_err());
        drop(claim);
        config.name = "Work chat".into();
        save_config(&config).unwrap();
        config.conversation_id = "999".into();
        assert!(save_config(&config).is_err());
        config.conversation_id = "123".into();
        let mut state = ChannelState::load(&root(&config).join("state.json"), &config).unwrap();
        state.outbox.push(crate::channels::Delivery {
            id: "reply_1".into(),
            item: Outbound::Text {
                text: "result".into(),
            },
            state: "uncertain".into(),
            retry_at: 0,
            remote_id: None,
        });
        state.save(&root(&config).join("state.json")).unwrap();
        assert!(remove_saved(&config.id).is_err());
        resolve_delivery(&saved_path(&config.id).unwrap(), "reply_1", false).unwrap();
        remove_saved(&config.id).unwrap();
        assert!(root(&config).join("state.json").exists());
        assert_eq!(
            list_saved().unwrap()["connections"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(saved_path("../other").is_err());
    }

    #[tokio::test]
    async fn channel_retries_only_before_turn_submission_and_keeps_queued_identity() {
        for stage in ["offline", "subscribe", "connect", "submitted"] {
            let dir = tempfile::tempdir().unwrap();
            let _home = crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
            let config = ChannelConfig {
                id: "test".into(), name: "Test".into(), platform: Platform::Telegram,
                conversation_id: "123".into(), allowed_user_ids: vec!["456".into()],
                agent_id: "avery".into(), session_id: "session".into(),
                workspace: dir.path().into(), token_env: "BOT_TOKEN".into(), enabled: true, group_id:None 
            };
            let path = dir.path().join("state.json");
            let mut state = ChannelState::load(&path, &config).unwrap();
            state.admit(crate::channels::PollBatch {
                cursor: Some("101".into()),
                messages: vec![crate::channels::InboundMessage {
                    reply_to: None,
                    id: "100".into(), user_id: "456".into(), text: "Please help".into(), sender:None 
                }],
            }).unwrap();
            let id = state.jobs[0].turn_id.clone();
            state.jobs[0].state = "running".into();
            let server = if stage == "offline" { None } else {
                let listener = tokio::net::UnixListener::bind(super::super::daemon::socket_path()).unwrap();
                Some(tokio::spawn(async move {
                    let (subscription, _) = listener.accept().await.unwrap();
                    let (read, mut write) = subscription.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    assert!(line.contains("Subscribe"));
                    if stage == "subscribe" { return; }
                    // Remove the listening socket before acknowledging the
                    // subscription to force a failure at the Turn connection.
                    if stage == "connect" {
                        drop(listener);
                        write.write_all(b"\"Pong\"\n").await.unwrap();
                        return;
                    }
                    write.write_all(b"\"Pong\"\n").await.unwrap();
                    let (turn, _) = listener.accept().await.unwrap();
                    line.clear();
                    BufReader::new(turn).read_line(&mut line).await.unwrap();
                    assert!(line.contains("Turn"));
                    // Lost response after submission is intentionally ambiguous.
                }))
            };
            let error = run_turn(config.clone(), id.clone(), "Please help".into()).await.unwrap_err();
            if let Some(server) = server { server.await.unwrap(); }
            assert_eq!(error.is::<TurnNotSubmitted>(), stage != "submitted", "{stage}: {error:#}");
            assert_eq!(defer_unsubmitted_turn(&mut state, &id, &error), stage != "submitted");
            if stage == "submitted" {
                assert_eq!(state.jobs[0].state, "running");
                continue;
            }
            state.save(&path).unwrap();
            let mut recovered = ChannelState::load(&path, &config).unwrap();
            recovered.recover();
            assert_eq!(recovered.jobs.len(), 1);
            assert_eq!(recovered.jobs[0].turn_id, id);
            assert_eq!(recovered.jobs[0].state, "queued");
            assert_eq!(recovered.cursor.as_deref(), Some("101"));
            assert!(recovered.outbox.is_empty(), "No failure reply for an unsubmitted turn");
        }
    }

    #[tokio::test]
    async fn channel_gateway_boundary_returns_only_its_completed_request() {
        for (background, queued, incomplete) in [(false,false,false),(true,false,false),(true,true,false),(false,false,true)] {
            let dir=tempfile::tempdir().unwrap();
            let _home=crate::config::test_env::PhoenixHomeGuard::set_private(dir.path());
            if queued { crate::runtime::asks::register_detached_with_payload("queued-channel-question","session","avery",&[crate::tools::ask_user::AskUserQuestion{question:"Which label?".into(),header:None,options:vec!["Blue".into()],multi_select:false}],None); }
            let listener=tokio::net::UnixListener::bind(super::super::daemon::socket_path()).unwrap();
            let server=tokio::spawn(async move {
                let (stream,_)=listener.accept().await.unwrap();let (read,mut journal)=stream.into_split();
                let value=journal::frame(&mut BufReader::new(read)).await.unwrap();assert_eq!(value["SubscribeJournal"]["session_id"],"session");
                journal.write_all(b"\"Pong\"\n").await.unwrap();
                let (stream,_)=listener.accept().await.unwrap();let (read,mut reply)=stream.into_split();
                let request=journal::frame(&mut BufReader::new(read)).await.unwrap();
                assert_eq!(request["Turn"]["target_agent"],"avery");assert_eq!(request["Turn"]["turn_id"],"stable-turn");assert_eq!(request["Turn"]["delivery"],"queue");
                // Other work can finish first in exactly this same session.
                for (turn,task,kind,text) in [("other","other","answer","Wrong final"),("other","other","execution_ended",""),("stable-turn","worker","answer","Private worker result"),("stable-turn","stable-turn","reasoning","Private thought")] {
                    journal.write_all(format!("{}\n",json!({"Story":{"kind":kind,"markdown":text,"execution":{"turn_id":turn,"task_id":task}}})).as_bytes()).await.unwrap();
                }
                if queued {
                    journal.write_all(format!("{}\n",json!({"Story":{"kind":"ask_pending","id":"queued-channel-question","execution":{"turn_id":"stable-turn","task_id":"stable-turn"}}})).as_bytes()).await.unwrap();
                    for (kind,text) in [("answer","Early candidate"),("answer","Queued final answer"),("execution_ended","")] {
                        journal.write_all(format!("{}\n",json!({"Story":{"kind":kind,"markdown":text,"execution":{"turn_id":"stable-turn","task_id":"stable-turn"}}})).as_bytes()).await.unwrap();
                    }
                }
                for frame in [
                    json!({"Event":{"ToolCallStarted":{"agent":"avery","tool_name":"browser_screenshot","input_summary":"private-url"}}}),
                    json!({"Done":{"completion":if queued {"queued"} else if incomplete {"incomplete"} else {"completed"},"final_markdown":if queued {"message queued"} else if incomplete {"The provider became unavailable after this agent had already performed work. Provider failure: usage_limit_reached. Tool evidence: private-url"} else {"Here is your final answer."},"main_session_id":"session","run_id":"run","trace_path":"trace","route":"avery","total_tokens":20,"orchestrator_tokens":null,"coder_tokens":null,"background_work_pending":background}})
                ] {reply.write_all(format!("{frame}\n").as_bytes()).await.unwrap();}
            });
            let config=ChannelConfig{id:"test".into(),name:"Test".into(),platform:Platform::Telegram,conversation_id:"123".into(),allowed_user_ids:vec!["456".into()],agent_id:"avery".into(),session_id:"session".into(),workspace:dir.path().into(),token_env:"BOT_TOKEN".into(),enabled:true, group_id:None };
            let (updates,mut received)=tokio::sync::mpsc::channel(4);
            let result=tokio::time::timeout(Duration::from_secs(5),run_turn_with_questions(config.clone(),"stable-turn".into(),"Please help".into(),Some(updates))).await.unwrap().unwrap();
            if queued {let question=received.recv().await.unwrap();assert_eq!(question.ask_id,"queued-channel-question");assert!(question.text.contains("Which label?"));}
            if incomplete {
                let projected=crate::channels::project_reply(&config,&result,&dir.path().join("spool")).unwrap();
                assert_eq!(projected.len(),1);
                let crate::channels::Outbound::Text { text:notice } = &projected[0] else { panic!("failure should be one text notice") };
                assert!(notice.contains("usage limit"));
                assert!(!notice.contains("private-url"));
                assert!(!notice.contains("Tool evidence"));
            } else {
                assert_eq!(result,if queued {"Queued final answer"} else {"Here is your final answer."});
            }
            server.await.unwrap();
        }
    }

}

// Explicit stdin overrides a saved login; a saved, bound login precedes ambient environment tokens.
// Tokens never appear in argv, connection metadata, or status.
fn read_token(config: &ChannelConfig, stdin: bool) -> Result<zeroize::Zeroizing<String>> {
    if stdin {
        use std::io::Read;
        let mut raw = zeroize::Zeroizing::new(String::new());
        std::io::stdin().take(8193).read_to_string(&mut raw)?;
        anyhow::ensure!(raw.len() <= 8192, "Bot token is too long");
        let trimmed = zeroize::Zeroizing::new(raw.trim().to_string());
        anyhow::ensure!(!trimmed.is_empty(), "Enter a bot token");
        Ok(trimmed)
    } else {
        if let Some(token) = crate::channels::credentials::load(&crate::config::phoenix_home(), config)? {
            return Ok(token);
        }
        if let Ok(token) = std::env::var(&config.token_env) {
            anyhow::ensure!(!token.trim().is_empty() && token.len() <= 8192, "Invalid bot-token environment value");
            return Ok(zeroize::Zeroizing::new(token.trim().to_string()));
        }
        anyhow::bail!("Enter a bot token or save a login in Phoenix")
    }
}
fn saved_path(id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid connection ID"
    );
    Ok(crate::config::phoenix_home()
        .join("channels/connections")
        .join(format!("{id}.json")))
}
pub fn list_saved() -> Result<serde_json::Value> {
    list_saved_view(false)
}
pub fn list_saved_summary() -> Result<serde_json::Value> {
    bounded_ui_response(list_saved_view(true)?)
}
fn list_saved_view(summary_only: bool) -> Result<serde_json::Value> {
    let dir = crate::config::phoenix_home().join("channels/connections");
    let mut rows = Vec::new();
    let workspace = crate::config::phoenix_workspace_root();
    let mut response_bytes = serde_json::to_vec(&json!({"connections":[],"workspace":workspace}))?.len();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let path = entry.path();
            let row = match load(&path) {
                Ok(config) => {
                    let status = if summary_only {status_summary(&path)} else {status(&path)};
                    json!({"config":config, "status":status.unwrap_or_else(|_|json!({"error":"Connection state needs review in Phoenix"}))})
                }
                Err(_) => {
                    json!({"error":"A saved connection could not be read. Its file was preserved."})
                }
            };
            if summary_only {
                response_bytes = response_bytes.saturating_add(serde_json::to_vec(&row)?.len() + usize::from(!rows.is_empty()));
                anyhow::ensure!(response_bytes < CHANNEL_UI_RESPONSE_BYTES,
                    "Too many or oversized channel settings for the desktop list. Inspect channels with the CLI; all connections were preserved");
            }
            rows.push(row);
        }
    }
    rows.sort_by_key(|r| r["config"]["name"].as_str().unwrap_or("").to_lowercase());
    Ok(json!({"connections":rows, "workspace":workspace}))
}
pub fn save_stdin() -> Result<serde_json::Value> {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin()
        .take(128 * 1024 + 1)
        .read_to_string(&mut raw)?;
    anyhow::ensure!(raw.len() <= 128 * 1024, "Connection settings are too large");
    let config: ChannelConfig =
        serde_json::from_str(&raw).context("Invalid connection settings")?;
    save_config(&config)?;
    Ok(json!({"id":config.id}))
}
fn save_config(config: &ChannelConfig) -> Result<()> {
    config.validate()?;
    let path = saved_path(&config.id)?;
    let dir = root(config);
    let _claim = crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?
        .context("Disconnect this channel before editing it")?;
    if path.exists() {
        let previous = load(&path)?;
        anyhow::ensure!(
            previous.binding() == config.binding(),
            "Create a new connection to change its destination, agent or workspace"
        );
    }
    ChannelState::load(&dir.join("state.json"), config)?;
    crate::config::private_io::atomic_write_private(&path, &serde_json::to_vec_pretty(config)?)
}
pub fn remember_token(path: &Path) -> Result<()> {
    let config = load(path)?;
    let _claim = crate::config::private_io::try_execution_claim(&root(&config).join("worker.lock"))?
        .context("Disconnect this channel before replacing its login")?;
    let token = read_token(&config, true)?;
    crate::channels::credentials::save(&crate::config::phoenix_home(), &config, &token)
}
pub fn forget_token(path: &Path) -> Result<()> {
    let config = load(path)?;
    let _claim = crate::config::private_io::try_execution_claim(&root(&config).join("worker.lock"))?
        .context("Disconnect this channel before forgetting its login")?;
    crate::channels::credentials::forget(&crate::config::phoenix_home(), &config)
}
pub fn remove_saved(id: &str) -> Result<()> {
    let path = saved_path(id)?;
    let config = load(&path)?;
    let dir = root(&config);
    let _claim = crate::config::private_io::try_execution_claim(&dir.join("worker.lock"))?
        .context("Disconnect this channel before removing it")?;
    let state = ChannelState::load(&dir.join("state.json"), &config)?;
    anyhow::ensure!(
        state.jobs.iter().all(|j| j.state == "done")
            && state.outbox.iter().all(|d| d.state == "sent"),
        "Resolve unfinished turns and replies before removing this connection"
    );
    crate::channels::credentials::forget(&crate::config::phoenix_home(), &config)?;
    // Keep delivered history private for diagnosis. IDs are not reused by the UI.
    std::fs::remove_file(path)?;
    Ok(())
}
