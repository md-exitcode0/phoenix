//! Durable terminal answers for channel-originated turns. A receipt proves a
//! finished response (including an interruption notice), never success of the
//! requested work and never merely a queued acknowledgement.
use crate::config::private_io;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECEIPTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Key {
    session: String,
    turn: String,
    request: String,
    workspace: PathBuf,
    agent: String,
}
impl Key {
    pub fn new(
        session: &str,
        turn: Option<&str>,
        agent: Option<&str>,
        workspace: Option<&Path>,
        prompt: &str,
    ) -> Option<Self> {
        let turn = turn?;
        let suffix = turn
            .strip_prefix("channel_")
            .or_else(|| turn.strip_prefix("ask_answer_"))?;
        if suffix.len() != 64 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let agent = agent?;
        let workspace = workspace?;
        let request = crate::channels::digest(
            &serde_json::to_string(&(session, turn, agent, workspace, prompt)).ok()?,
        );
        Some(Self {
            session: session.into(),
            turn: turn.into(),
            request,
            workspace: workspace.into(),
            agent: agent.into(),
        })
    }
    pub fn for_job(
        config: &crate::channels::ChannelConfig,
        job: &crate::channels::Job,
    ) -> Option<Self> {
        Self::new(
            &config.session_id,
            Some(&job.turn_id),
            crate::channels::target_agent(config),
            Some(&config.workspace),
            &crate::channels::request_text(config, &job.input),
        )
    }
}
enum RecoverySource {
    Final(Key),
    /// The answer reached the waiting request. That original request still
    /// owns its final delivery; completing the answer job emits nothing.
    AcceptedAnswer,
}

/// Resolve durable provenance without rebuilding or resubmitting work.
fn recovery_source(
    config: &crate::channels::ChannelConfig,
    job: &crate::channels::Job,
    questions: &[crate::channels::ChannelQuestion],
) -> Result<Option<RecoverySource>> {
    let mut matching = questions
        .iter()
        .filter(|q| q.answered_by.as_ref() == Some(&job.input.id));
    let Some(question) = matching.next() else {
        return Ok(Key::for_job(config, job).map(RecoverySource::Final));
    };
    ensure!(
        matching.next().is_none(),
        "Channel reply belongs to multiple questions"
    );
    ensure!(
        question.user_id == job.input.user_id,
        "Question reply sender mismatch"
    );
    let Some(record) = crate::runtime::asks::decision_record_for(&question.ask_id)? else {
        return Ok(None);
    };
    if record.session_id != config.session_id
        || record.approval.is_some()
        || !matches!(record.status.as_str(), "answered" | "answered_late")
        || record.resolved_at.is_none()
        || record.answer.as_deref() != Some(job.input.text.as_str())
    {
        return Ok(None);
    }
    if record.status == "answered" {
        return Ok(Some(RecoverySource::AcceptedAnswer));
    }
    let Some(turn) = super::turn_queue::existing_answer_turn(
        &config.session_id,
        &question.ask_id,
        &job.input.text,
    )?
    else {
        return Ok(None);
    };
    if turn.workspace.as_ref() != Some(&config.workspace)
        || turn.target_agent.as_ref() != Some(&config.agent_id)
        || turn.target_group.is_some()
        || !turn
            .turn_id
            .as_deref()
            .is_some_and(|id| id.starts_with("ask_answer_"))
    {
        return Ok(None);
    }
    if super::turn_queue::submitted_turn_receipt(
        &config.session_id,
        turn.turn_id.as_deref().unwrap(),
    )?
    .is_none()
    {
        return Ok(None);
    }
    Ok(Key::new(
        &config.session_id,
        turn.turn_id.as_deref(),
        turn.target_agent.as_deref(),
        turn.workspace.as_deref(),
        &turn.user_request,
    )
    .map(RecoverySource::Final))
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    key: Key,
    markdown: String,
    output: Vec<crate::channels::Outbound>,
}
#[derive(Serialize, Deserialize)]
struct Store {
    version: u8,
    session: String,
    receipts: Vec<Receipt>,
}
fn path(session: &str) -> PathBuf {
    crate::config::phoenix_home()
        .join("channels/completions")
        .join(format!("{}.json", crate::channels::digest(session)))
}
fn images(session: &str) -> PathBuf {
    path(session).with_extension("").join("images")
}
fn clean_images(session: &str, keep: &[Receipt]) -> Result<()> {
    let dir = images(session);
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let file = entry?.path();
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with("image-")
            && (name.ends_with(".png") || name.ends_with(".jpg"))
            && !keep
                .iter()
                .flat_map(|r| &r.output)
                .any(|o| matches!(o, crate::channels::Outbound::Image{path,..} if path == &file))
        {
            private_io::remove_private_file(&file)?;
        }
    }
    Ok(())
}
fn load(session: &str) -> Result<Store> {
    let store = match private_io::read_private_file_limited(&path(session), MAX_BYTES)? {
        Some(bytes) => serde_json::from_slice::<Store>(&bytes)
            .context("Channel completion receipts are unreadable")?,
        None => Store {
            version: 1,
            session: session.into(),
            receipts: vec![],
        },
    };
    ensure!(
        store.version == 1 && store.session == session && store.receipts.len() <= MAX_RECEIPTS,
        "Channel completion receipt identity or version mismatch"
    );
    Ok(store)
}
/// Image validation and durable I/O run outside the gateway executor.
pub(super) async fn record(key: &Key, markdown: &str) -> Result<()> {
    let key = key.clone();
    let markdown = markdown.to_owned();
    tokio::task::spawn_blocking(move || save(&key, &markdown)).await?
}
/// Called only after the runner returned Ok, before acknowledging completion.
/// Repeated writes cannot replace a receipt with a different request/result.
pub(super) fn save(key: &Key, markdown: &str) -> Result<()> {
    ensure!(
        markdown.len() <= 4 * 1024 * 1024,
        "Channel final exceeds receipt limit"
    );
    let target = path(&key.session);
    private_io::with_private_lock(&target, || {
        let mut store = load(&key.session)?;
        if let Some(previous) = store.receipts.iter().find(|r| r.key.turn == key.turn) {
            ensure!(
                previous.key == *key && previous.markdown == markdown,
                "Channel completion receipt changed"
            );
            return Ok(());
        }
        // Use the smaller platform text limit for portable recovery. Freeze
        // deliberate image bytes now; a later render may overwrite the source.
        let config = crate::channels::ChannelConfig {
            id: "completion".into(),
            name: "Completion".into(),
            platform: crate::channels::Platform::Discord,
            conversation_id: "1".into(),
            allowed_user_ids: vec![],
            agent_id: key.agent.clone(),
            session_id: key.session.clone(),
            workspace: key.workspace.clone(),
            token_env: "UNUSED".into(),
            enabled: false, group_id:None 
        };
        let output = crate::channels::project_reply(&config, markdown, &images(&key.session))?;
        store.receipts.push(Receipt {
            key: key.clone(),
            markdown: markdown.into(),
            output,
        });
        while store.receipts.len() > MAX_RECEIPTS {
            store.receipts.remove(0);
        }
        let bytes = loop {
            let bytes = serde_json::to_vec(&store)?;
            if bytes.len() <= MAX_BYTES {
                break bytes;
            }
            ensure!(
                store.receipts.len() > 1,
                "Channel receipt exceeds storage limit"
            );
            store.receipts.remove(0);
        };
        private_io::atomic_write_private_under_lock(&target, &bytes)?;
        clean_images(&key.session, &store.receipts)
    })
}
/// A terminal receipt prevents another execution, including when the saved
/// reply reports incomplete work. Image availability is a delivery concern;
/// losing a frozen image must never authorize repeating the work.
pub(super) fn terminal_exists(key: &Key) -> Result<bool> {
    private_io::with_private_lock(&path(&key.session), || {
        let store = load(&key.session)?;
        let mut matching = store.receipts.iter().filter(|r| r.key.turn == key.turn);
        let Some(receipt) = matching.next() else {
            return Ok(false);
        };
        ensure!(matching.next().is_none(), "Channel completion has duplicate terminal receipts");
        ensure!(
            receipt.key == *key,
            "Channel completion belongs to a different request"
        );
        Ok(true)
    })
}

#[cfg(test)]
pub(super) fn find(key: &Key) -> Result<Option<String>> {
    let store = load(&key.session)?;
    let Some(receipt) = store.receipts.into_iter().find(|r| r.key.turn == key.turn) else {
        return Ok(None);
    };
    ensure!(
        receipt.key == *key,
        "Channel completion belongs to a different request"
    );
    Ok(Some(receipt.markdown))
}
/// Read the bounded store once for a recovery pass, not once per job.
pub(super) fn recoverable(
    config: &crate::channels::ChannelConfig,
    jobs: &[crate::channels::Job],
    questions: &[crate::channels::ChannelQuestion],
    spool: &Path,
) -> Result<Vec<(String, Vec<crate::channels::Outbound>)>> {
    let target = path(&config.session_id);
    private_io::with_private_lock(&target, || {
        let store = load(&config.session_id)?;
        let mut output = Vec::new();
        for job in jobs {
            if job.state != "needs_review" || !config.allowed_user_ids.contains(&job.input.user_id)
            {
                continue;
            }
            // The inbox identity also binds agent/workspace/destination for
            // foreground answers, which have no separate frozen continuation.
            let expected = format!(
                "channel_{}",
                crate::channels::digest(&format!("{}\0{}", config.binding(), job.input.id))
            );
            if job.turn_id != expected {
                continue;
            }
            let key = match recovery_source(config, job, questions)? {
                Some(RecoverySource::Final(key)) => key,
                Some(RecoverySource::AcceptedAnswer) => {
                    output.push((job.turn_id.clone(), Vec::new()));
                    continue;
                }
                None => continue,
            };
            if let Some(receipt) = store.receipts.iter().find(|r| r.key.turn == key.turn) {
                ensure!(
                    receipt.key == key,
                    "Channel completion belongs to a different request"
                );
                let mut restored = receipt.output.clone();
                for item in &mut restored {
                    if let crate::channels::Outbound::Image { path, name } = item {
                        ensure!(
                            path.parent() == Some(images(&config.session_id).as_path())
                                && path.file_name().and_then(|n| n.to_str()) == Some(name.as_str()),
                            "Channel image receipt path mismatch"
                        );
                        let bytes = private_io::read_private_file_limited(path, 8 * 1024 * 1024)?
                            .context("Saved channel image is missing")?;
                        let destination = spool.join(name);
                        private_io::atomic_write_private(&destination, &bytes)?;
                        *path = destination;
                    }
                }
                output.push((job.turn_id.clone(), restored));
            }
        }
        Ok(output)
    })
}
/// Transcript deletion must not resurrect an answer through recovery. Clear
/// the session's bounded receipt cache as part of staged deletion recovery.
pub(super) fn purge_session(session: &str) -> Result<()> {
    let target = path(session);
    private_io::with_private_lock(&target, || {
        private_io::remove_private_file_under_lock(&target)?;
        clean_images(session, &[])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_lookup_preserves_receipt_when_frozen_image_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let turn = format!("channel_{}", "b".repeat(64));
        let key = Key::new("terminal", Some(&turn), Some("avery"), Some(home.path()), "Render").unwrap();
        assert!(!terminal_exists(&key).unwrap());
        image::RgbImage::new(8, 8).save(home.path().join("result.png")).unwrap();
        save(&key, "Unfinished result. ![Result](result.png)").unwrap();
        let bytes = std::fs::read(path("terminal")).unwrap();
        let store = load("terminal").unwrap();
        let frozen = store.receipts[0].output.iter().find_map(|item| match item {
            crate::channels::Outbound::Image { path, .. } => Some(path),
            _ => None,
        }).unwrap();
        std::fs::remove_file(frozen).unwrap();
        assert!(terminal_exists(&key).unwrap(), "delivery damage never permits another execution");
        assert_eq!(std::fs::read(path("terminal")).unwrap(), bytes);
    }

    #[test]
    fn terminal_lookup_rejects_corruption_duplicate_and_full_key_mismatch() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let turn = format!("ask_answer_{}", "c".repeat(64));
        let key = Key::new("terminal", Some(&turn), Some("avery"), Some(home.path()), "Frozen request").unwrap();
        save(&key, "Saved result").unwrap();
        let original: serde_json::Value = serde_json::from_slice(&std::fs::read(path("terminal")).unwrap()).unwrap();
        for field in ["session", "agent", "workspace", "request", "duplicate", "version", "json"] {
            let mut changed = original.clone();
            match field {
                "duplicate" => {
                    let duplicate = changed["receipts"][0].clone();
                    changed["receipts"].as_array_mut().unwrap().push(duplicate);
                }
                "version" => changed["version"] = serde_json::json!(2),
                "json" => {},
                field => changed["receipts"][0]["key"][field] = serde_json::json!("different"),
            }
            let bytes = if field == "json" { b"{broken".to_vec() } else { serde_json::to_vec(&changed).unwrap() };
            private_io::atomic_write_private(&path("terminal"), &bytes).unwrap();
            assert!(terminal_exists(&key).is_err(), "{field}");
            assert_eq!(std::fs::read(path("terminal")).unwrap(), bytes, "{field}");
        }
    }

    #[test]
    fn receipt_is_durable_payload_bound_immutable_and_deleted_with_transcript() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let turn = format!("channel_{}", "a".repeat(64));
        let key = Key::new(
            "session",
            Some(&turn),
            Some("avery"),
            Some(home.path()),
            "Draw a banana",
        )
        .unwrap();
        assert!(find(&key).unwrap().is_none());
        save(&key, "Finished ![Banana](banana.png)").unwrap();
        save(&key, "Finished ![Banana](banana.png)").unwrap();
        assert_eq!(
            find(&key).unwrap().as_deref(),
            Some("Finished ![Banana](banana.png)")
        );
        assert!(save(&key, "Replacement").is_err());
        for (session, agent, workspace, prompt) in [
            ("session", "theo", home.path(), "Draw a banana"),
            ("session", "avery", Path::new("/other"), "Draw a banana"),
            ("session", "avery", home.path(), "Delete a banana"),
        ] {
            let wrong =
                Key::new(session, Some(&turn), Some(agent), Some(workspace), prompt).unwrap();
            assert!(find(&wrong).is_err());
        }
        let other = Key::new(
            "other",
            Some(&turn),
            Some("avery"),
            Some(home.path()),
            "Draw a banana",
        )
        .unwrap();
        assert!(find(&other).unwrap().is_none());
        assert!(Key::new(
            "session",
            Some("ordinary-turn"),
            Some("avery"),
            Some(home.path()),
            "hello"
        )
        .is_none());
        purge_session("other").unwrap();
        assert!(find(&key).unwrap().is_some());
        purge_session("session").unwrap();
        assert!(find(&key).unwrap().is_none());
    }
}
