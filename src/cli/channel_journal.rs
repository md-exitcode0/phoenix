//! Correlated, terminal-only channel observation. Drain unrelated UI activity
//! without letting it select an answer or block the gateway's journal writer.
use super::*;
use serde_json::Value;

pub(super) async fn frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<Value> {
    let mut bytes = Vec::new();
    let count = (&mut *reader)
        .take(4 * 1024 * 1024 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    anyhow::ensure!(
        count > 0 && count <= 4 * 1024 * 1024,
        "Channel journal ended or exceeded its frame limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) fn owned_story<'a>(value: &'a Value, turn: &str) -> Option<&'a Value> {
    let story = value.get("Story")?;
    (story["execution"]["turn_id"].as_str() == Some(turn)
        && story["execution"]["task_id"].as_str() == Some(turn))
    .then_some(story)
}

pub(super) struct Journal {
    frames: tokio::sync::mpsc::Receiver<Result<Value>>,
    reader: tokio::task::JoinHandle<()>,
}
impl Drop for Journal {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
impl Journal {
    pub async fn connect(session: &str) -> Result<Self> {
        let stream = UnixStream::connect(super::super::daemon::socket_path()).await?;
        let (read, mut keep_open) = stream.into_split();
        keep_open
            .write_all(
                format!("{}\n", json!({"SubscribeJournal":{"session_id":session}})).as_bytes(),
            )
            .await?;
        let mut journal = BufReader::new(read);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = frame(&mut journal).await?;
                if value == json!("Pong") {
                    return Ok::<_, anyhow::Error>(());
                }
                anyhow::ensure!(
                    value.get("Error").is_none(),
                    "Channel journal subscription rejected"
                );
            }
        })
        .await
        .context("Channel journal did not become ready")??;
        let (tx, frames) = tokio::sync::mpsc::channel(128);
        let reader = tokio::spawn(async move {
            loop {
                match frame(&mut journal).await {
                    Ok(value) => {
                        let relevant = matches!(
                            value["Story"]["kind"].as_str(),
                            Some("answer" | "ask_pending" | "execution_ended")
                        );
                        if relevant && tx.send(Ok(value)).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                }
            }
            drop(keep_open);
        });
        Ok(Self { frames, reader })
    }
    pub async fn final_answer(
        &mut self,
        config: &ChannelConfig,
        job_id: &str,
        turn: &str,
        updates: Option<&questions::Updates>,
    ) -> Result<String> {
        let mut candidate: Option<(Option<String>, String)> = None;
        while let Some(value) = self.frames.recv().await {
            let value = value?;
            let Some(story) = owned_story(&value, turn) else {
                continue;
            };
            match story["kind"].as_str() {
                Some("ask_pending") => {
                    if let (Some(id), Some(updates)) = (story["id"].as_str(), updates) {
                        questions::publish(config, job_id, id, updates).await?;
                    }
                }
                Some("answer") => {
                    candidate = story["markdown"].as_str().map(|text| {
                        (
                            story["execution"]["attempt_id"].as_str().map(str::to_owned),
                            text.to_owned(),
                        )
                    });
                }
                Some("execution_ended") => {
                    // A saved answer preview can precede a failed receipt or
                    // trace write. The owned failure boundary must terminate
                    // observation promptly, not deliver that preview or wait
                    // two hours for an end that has already happened.
                    anyhow::ensure!(
                        story.get("error").is_none_or(Value::is_null),
                        "Phoenix could not confirm this terminal result; review the original conversation before continuing, because work may already have taken effect"
                    );
                    let (attempt, text) = candidate.context(
                        "The channel turn ended without a final answer; review it in Phoenix",
                    )?;
                    anyhow::ensure!(
                        attempt.as_deref() == story["execution"]["attempt_id"].as_str(),
                        "The final answer and completion belong to different attempts"
                    );
                    return Ok(text);
                }
                _ => {}
            }
        }
        anyhow::bail!("Channel journal disconnected before this request ended")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_terminal_rejects_a_preview_without_waiting_for_disconnection() {
        let root = tempfile::tempdir().unwrap();
        let config = ChannelConfig {
            id: "test".into(), name: "Test".into(), platform: Platform::Telegram,
            conversation_id: "1".into(), allowed_user_ids: vec!["2".into()],
            agent_id: "phoenix".into(), session_id: "session".into(),
            workspace: root.path().into(), token_env: "UNUSED".into(), enabled: false, group_id:None 
        };
        for error in [json!("Receipt could not be confirmed"), json!(""), json!(true), json!({"malformed":true})] {
            for preview in [false, true] {
                let (tx, frames) = tokio::sync::mpsc::channel(4);
                if preview {
                    tx.send(Ok(json!({"Story":{"kind":"answer","markdown":"Do not deliver this preview",
                        "execution":{"turn_id":"turn","task_id":"turn","attempt_id":"a"}}}))).await.unwrap();
                }
                tx.send(Ok(json!({"Story":{"kind":"execution_ended","error":error,
                    "execution":{"turn_id":"turn","task_id":"turn","attempt_id":"a"}}}))).await.unwrap();
                let mut journal = Journal { frames, reader: tokio::spawn(std::future::pending()) };
                let result = tokio::time::timeout(Duration::from_millis(250),
                    journal.final_answer(&config, "job", "turn", None)).await
                    .expect("failure must be observed while the journal connection is still open");
                assert!(result.unwrap_err().to_string().contains("work may already have taken effect"));
                assert!(!tx.is_closed(), "the consumer did not rely on the producer disconnecting");
            }
        }
    }

    #[tokio::test]
    async fn foreign_failure_does_not_override_the_exact_successful_attempt() {
        let root = tempfile::tempdir().unwrap();
        let config = ChannelConfig {
            id: "test".into(), name: "Test".into(), platform: Platform::Telegram,
            conversation_id: "1".into(), allowed_user_ids: vec!["2".into()],
            agent_id: "phoenix".into(), session_id: "session".into(),
            workspace: root.path().into(), token_env: "UNUSED".into(), enabled: false, group_id:None 
        };
        let (tx, frames) = tokio::sync::mpsc::channel(8);
        for (turn, task) in [("foreign", "foreign"), ("turn", "private-coworker")] {
            tx.send(Ok(json!({"Story":{"kind":"execution_ended","error":"Foreign failure",
                "execution":{"turn_id":turn,"task_id":task,"attempt_id":"other"}}}))).await.unwrap();
        }
        tx.send(Ok(json!({"Story":{"kind":"answer","markdown":"Exact result",
            "execution":{"turn_id":"turn","task_id":"turn","attempt_id":"a"}}}))).await.unwrap();
        tx.send(Ok(json!({"Story":{"kind":"execution_ended",
            "execution":{"turn_id":"turn","task_id":"turn","attempt_id":"a"}}}))).await.unwrap();
        let mut journal = Journal { frames, reader: tokio::spawn(std::future::pending()) };
        assert_eq!(journal.final_answer(&config, "job", "turn", None).await.unwrap(), "Exact result");
    }

    #[tokio::test]
    async fn final_delivery_requires_an_end_from_the_same_attempt() {
        let root = tempfile::tempdir().unwrap();
        let config = ChannelConfig {
            id: "test".into(),
            name: "Test".into(),
            platform: Platform::Telegram,
            conversation_id: "1".into(),
            allowed_user_ids: vec!["2".into()],
            agent_id: "phoenix".into(),
            session_id: "session".into(),
            workspace: root.path().into(),
            token_env: "BOT_TOKEN".into(),
            enabled: true, group_id:None 
        };
        for (end_attempt, final_present, accepted) in [
            (None, true, false),
            (Some("b"), true, false),
            (Some("a"), false, false),
            (Some("a"), true, true),
        ] {
            let (tx, frames) = tokio::sync::mpsc::channel(4);
            if final_present {
                tx.send(Ok(json!({"Story":{"kind":"answer","markdown":"Final","execution":{"turn_id":"turn","task_id":"turn","attempt_id":"a"}}}))).await.unwrap();
            }
            if let Some(attempt) = end_attempt {
                tx.send(Ok(json!({"Story":{"kind":"execution_ended","execution":{"turn_id":"turn","task_id":"turn","attempt_id":attempt}}}))).await.unwrap();
            }
            drop(tx);
            let mut journal = Journal {
                frames,
                reader: tokio::spawn(async {}),
            };
            let result = journal.final_answer(&config, "job", "turn", None).await;
            assert_eq!(
                result.is_ok(),
                accepted,
                "end={end_attempt:?}, final={final_present}: {result:?}"
            );
            if accepted {
                assert_eq!(result.unwrap(), "Final");
            }
        }
    }
}
