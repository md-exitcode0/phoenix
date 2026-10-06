//! Ordinary remote questions. Approval decisions remain in the typed desktop flow.
use super::*;
use crate::channels::ChannelQuestion;
use crate::runtime::asks;

#[derive(Debug)]
pub(super) struct QuestionUpdate {
    pub job_id: String,
    pub ask_id: String,
    pub text: String,
}
pub(super) type Updates = tokio::sync::mpsc::Sender<QuestionUpdate>;

pub(super) async fn publish(
    config: &ChannelConfig,
    job_id: &str,
    ask_id: &str,
    updates: &Updates,
) -> Result<()> {
    let Some(text) = question_text(config, ask_id)? else { return Ok(()); };
    updates
        .send(QuestionUpdate {
            job_id: job_id.into(),
            ask_id: ask_id.into(),
            text,
        })
        .await
        .context("Channel question receiver stopped")
}

/// The chat text for one pending question: the question, its "• option"
/// lines (Telegram turns them into buttons) and how to answer. None when the
/// ask is not an open, ordinary question of this connection's conversation.
pub(super) fn question_text(config: &ChannelConfig, ask_id: &str) -> Result<Option<String>> {
    let Some(record) = asks::decision_record_for(ask_id)? else {
        return Ok(None);
    };
    if record.session_id != config.session_id
        || record.approval.is_some()
        || record.status != "pending"
    {
        return Ok(None);
    }
    let mut text = String::from("A question before I continue:\n\n");
    for (index, question) in record.questions.iter().enumerate() {
        if record.questions.len() > 1 {
            text.push_str(&format!("{}. ", index + 1));
        }
        text.push_str(&question.question);
        text.push('\n');
        for option in &question.options {
            text.push_str(&format!("• {option}\n"));
        }
        text.push('\n');
    }
    text.push_str(
        "Tap an option, or just type your answer: your next message answers this question.",
    );
    Ok(Some(text))
}

#[cfg(test)]
use super::journal::owned_story;
use super::journal::{frame, Journal};

fn question_is_open(config: &ChannelConfig, ask_id: &str) -> Result<bool> {
    let record =
        asks::decision_record_for(ask_id)?.context("This question is no longer available")?;
    anyhow::ensure!(
        record.session_id == config.session_id && record.approval.is_none(),
        "This question requires its original Phoenix conversation"
    );
    Ok(record.status == "pending")
}
const CLOSED_QUESTION: &str =
    "This question is already closed in Phoenix. Send a new message to continue.";

pub(super) async fn answer(
    config: ChannelConfig,
    job_id: String,
    question: ChannelQuestion,
    text: String,
    updates: Updates,
) -> Result<String> {
    if !question_is_open(&config, &question.ask_id)? {
        return Ok(CLOSED_QUESTION.into());
    }
    // Subscribe and await the registration barrier BEFORE answering. A fast
    // successor can finish before the AnswerAsk acknowledgement is read.
    let mut journal = Journal::connect(&config.session_id).await?;
    // The desktop may have resolved the card while replay was draining.
    if !question_is_open(&config, &question.ask_id)? {
        return Ok(CLOSED_QUESTION.into());
    }
    let stream = UnixStream::connect(super::super::daemon::socket_path()).await?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{}\n", json!({"AnswerAsk":{"ask_id":question.ask_id,"answer":text,"session_id":config.session_id}})).as_bytes()).await?;
    let ack = tokio::time::timeout(Duration::from_secs(30), frame(&mut BufReader::new(read)))
        .await
        .context("Question acknowledgement timed out")??;
    let receipt = ack
        .get("AskAnswered")
        .context("The question could not be answered; review it in Phoenix")?;
    let Some(turn) = receipt["continuation_turn_id"].as_str() else {
        anyhow::ensure!(
            receipt["disposition"] == "delivered",
            "Question continuation was not confirmed"
        );
        // A still-running foreground request owns delivery of its own answer.
        return Ok(String::new());
    };
    tokio::time::timeout(
        Duration::from_secs(7200),
        journal.final_answer(&config, &job_id, turn, Some(&updates)),
    )
    .await
    .context("Question continuation needs review in Phoenix")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuation_ignores_replay_other_turns_and_worker_tasks() {
        for (key, turn, task, accepted) in [
            ("StoryReplay", "next", "next", false),
            ("Story", "old", "old", false),
            ("Story", "next", "worker", false),
            ("Story", "next", "next", true),
        ] {
            let value = json!({key:{"kind":"answer","markdown":"answer","execution":{"turn_id":turn,"task_id":task}}});
            assert_eq!(owned_story(&value, "next").is_some(), accepted);
        }
        assert!(owned_story(
            &json!({"Story":{"kind":"answer","markdown":"unowned"}}),
            "next"
        )
        .is_none());
    }
    #[tokio::test]
    async fn fast_continuation_is_captured_before_ack_and_only_exact_owner_answer_leaves() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let config = ChannelConfig {
            id: "test".into(),
            name: "Test".into(),
            platform: Platform::Telegram,
            conversation_id: "123".into(),
            allowed_user_ids: vec!["456".into()],
            agent_id: "phoenix".into(),
            session_id: "question-session".into(),
            workspace: home.path().into(),
            token_env: "BOT_TOKEN".into(),
            enabled: true, group_id:None 
        };
        let question = crate::tools::ask_user::AskUserQuestion {
            question: "Which color?".into(),
            header: None,
            options: vec!["Blue".into(), "Green".into()],
            multi_select: false,
        };
        asks::register_detached_with_payload(
            "channel-test-ask",
            &config.session_id,
            "phoenix",
            &[question],
            None,
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        publish(&config, "source-turn", "channel-test-ask", &tx)
            .await
            .unwrap();
        let update = rx.recv().await.unwrap();
        assert!(update.text.contains("Which color?"));
        assert!(update.text.contains("• Blue"));
        let mut foreign = config.clone();
        foreign.session_id = "wrong-session".into();
        publish(&foreign, "source-turn", "channel-test-ask", &tx)
            .await
            .unwrap();
        assert!(rx.try_recv().is_err());
        let approval = crate::tools::ask_user::ApprovalRequest {
            action: "tool_permission".into(),
            subject: "publish".into(),
            approved_option: "Approve".into(),
            details: Default::default(),
        };
        asks::register_detached_with_payload(
            "channel-protected-ask",
            &config.session_id,
            "phoenix",
            &[],
            Some(&approval),
        );
        publish(&config, "source-turn", "channel-protected-ask", &tx)
            .await
            .unwrap();
        assert!(
            rx.try_recv().is_err(),
            "Approval never becomes an ordinary remote question"
        );
        let listener =
            tokio::net::UnixListener::bind(super::super::super::daemon::socket_path()).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut journal) = stream.into_split();
            let mut read = BufReader::new(read);
            let request = frame(&mut read).await.unwrap();
            assert_eq!(
                request["SubscribeJournal"]["session_id"],
                "question-session"
            );
            journal.write_all(b"\"Pong\"\n").await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut response) = stream.into_split();
            let request = frame(&mut BufReader::new(read)).await.unwrap();
            assert_eq!(request["AnswerAsk"]["ask_id"], "channel-test-ask");
            assert_eq!(request["AnswerAsk"]["answer"], "Blue");
            assert_eq!(request["AnswerAsk"]["session_id"], "question-session");
            for (turn, task, kind, text) in [
                ("unrelated", "unrelated", "answer", "Wrong conversation"),
                ("next", "worker", "answer", "Internal worker result"),
                ("next", "next", "reasoning", "Private thought"),
                ("next", "next", "answer", "Blue it is."),
            ] {
                journal.write_all(format!("{}\n",json!({"Story":{"kind":kind,"markdown":text,"execution":{"turn_id":turn,"task_id":task}}})).as_bytes()).await.unwrap();
            }
            journal.write_all(format!("{}\n",json!({"Story":{"kind":"execution_ended","execution":{"turn_id":"next","task_id":"next"}}})).as_bytes()).await.unwrap();
            response.write_all(format!("{}\n",json!({"AskAnswered":{"disposition":"late_answer_queued","continuation_turn_id":"next"}})).as_bytes()).await.unwrap();
        });
        let result = answer(
            config.clone(),
            "reply-job".into(),
            ChannelQuestion {
                ask_id: "channel-test-ask".into(),
                turn_id: "source-turn".into(),
                user_id: "456".into(),
                delivery_ids: vec![],
                answered_by: Some("reply-1".into()),
            },
            "Blue".into(),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result, "Blue it is.");
        server.await.unwrap();
        asks::archive_late_answer("channel-test-ask", "Blue");
        let closed = answer(
            config,
            "late-job".into(),
            ChannelQuestion {
                ask_id: "channel-test-ask".into(),
                turn_id: "source-turn".into(),
                user_id: "456".into(),
                delivery_ids: vec![],
                answered_by: None,
            },
            "Blue".into(),
            tx,
        )
        .await
        .unwrap();
        assert_eq!(
            closed, CLOSED_QUESTION,
            "A resolved question must not connect or wait for an old final"
        );
    }
}
