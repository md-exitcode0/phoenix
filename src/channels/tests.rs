use super::*;
use serde_json::json;
fn config(root: &Path, platform: Platform) -> ChannelConfig {
    ChannelConfig {
        id: "personal".into(),
        name: "My agent".into(),
        platform,
        conversation_id: "123".into(),
        allowed_user_ids: vec!["456".into()],
        agent_id: "phoenix".into(),
        session_id: "session-one".into(),
        workspace: root.into(),
        token_env: "PHOENIX_BOT_TOKEN".into(),
        enabled: true,
        group_id: None,
    }
}
#[test]
fn inbound_restricts_people_conversations_and_bots() {
    let dir = tempfile::tempdir().unwrap();
    for platform in [Platform::Telegram, Platform::Discord] {
        let cfg = config(dir.path(), platform);
        let payload = if platform == Platform::Telegram {
            json!({"update_id":12,"message":{"message_id":1,"from":{"id":456,"is_bot":false},"chat":{"id":123},"text":"hello"}})
        } else {
            json!({"id":"1","channel_id":"123","author":{"id":"456","bot":false},"content":"hello"})
        };
        assert_eq!(inbound(&cfg, &payload).unwrap().text, "hello");
        let mut forbidden = cfg.clone();
        forbidden.allowed_user_ids = vec!["789".into()];
        assert!(inbound(&forbidden, &payload).is_none());
        forbidden = cfg.clone();
        forbidden.conversation_id = "999".into();
        assert!(inbound(&forbidden, &payload).is_none());
        let mut bot = payload.clone();
        if platform == Platform::Telegram {
            bot["message"]["from"]["is_bot"] = json!(true);
        } else {
            bot["author"]["bot"] = json!(true);
        }
        assert!(inbound(&cfg, &bot).is_none());
        if platform == Platform::Telegram {
            assert!(inbound(&cfg, &json!({"edited_message":payload["message"]})).is_none());
        }
    }
}
#[test]
fn splitting_preserves_every_character_and_utf16_limits() {
    let text = format!("{}\n{}", "🍌".repeat(5000), "e\u{301} ".repeat(2000));
    for limit in [2000, 4096] {
        let pieces = split_text(&text, limit);
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|p| p.encode_utf16().count() <= limit));
    }
}

#[test]
fn settled_channel_history_does_not_exhaust_active_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Telegram);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    // A legacy settled snapshot may already exceed today's admission budget.
    // Seed that old shape directly; new work must save/archive as it proceeds.
    for index in 0..1100 {
        let id = index.to_string();
        let turn_id = format!("channel_{}", digest(&format!("{}\0{id}", cfg.binding())));
        state.jobs.push(Job { input:InboundMessage {
            id:id.clone(), user_id:"456".into(), text:"x".repeat(62 * 1024), reply_to:None, sender:None 
        }, turn_id:turn_id.clone(), state:"done".into() });
        state.outbox.push(Delivery { id:digest(&format!("{turn_id}\00"))[..24].into(),
            item:Outbound::Text{text:"final result".repeat(250)}, state:"sent".into(),
            retry_at:0, remote_id:Some(format!("remote-{index}")) });
        state.cursor = Some(id);
    }
    state.save(&path).expect("completed history must not disable new channel work");
    assert!(std::fs::metadata(&path).unwrap().len() < 8 * 1024 * 1024);
    let mut reopened = ChannelState::load(&path, &cfg).unwrap();
    let before = reopened.jobs.len();
    reopened.admit(PollBatch { cursor:Some("1100".into()), messages:vec![InboundMessage {
        id:"0".into(), user_id:"456".into(), text:"old duplicate".into(), reply_to:None, sender:None 
    }] }).unwrap();
    assert_eq!(reopened.jobs.len(), before, "archiving must retain duplicate prevention");
    let old_turn = format!("channel_{}", digest(&format!("{}\0{}", cfg.binding(), "0")));
    reopened.complete(&old_turn, vec![Outbound::Text{text:"must never send again".into()}]).unwrap();
    assert!(!reopened.outbox.iter().any(|d|matches!(&d.item,Outbound::Text{text} if text=="must never send again")));
}

#[test]
fn channel_archival_retains_question_ownership_and_unresolved_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Telegram);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    for index in 0..70 {
        let request = format!("request-{index}"); let answer = format!("answer-{index}");
        state.admit(PollBatch {cursor:None,messages:vec![InboundMessage{id:request,user_id:"456".into(),text:"request".into(),reply_to:None, sender:None }]}).unwrap();
        let turn = state.jobs.last().unwrap().turn_id.clone();
        state.queue_question(&turn, &format!("ask-{index}"), "Which color?").unwrap();
        state.outbox.last_mut().unwrap().state = "sent".into();
        state.outbox.last_mut().unwrap().remote_id = Some(format!("question-{index}"));
        if index != 0 {
            state.admit(PollBatch{cursor:None,messages:vec![InboundMessage{id:answer.clone(),user_id:"456".into(),text:"Blue".into(),reply_to:Some(format!("question-{index}")), sender:None }]}).unwrap();
            let answer_turn = state.jobs.last().unwrap().turn_id.clone();
            state.complete(&answer_turn, vec![]).unwrap();
            state.questions.last_mut().unwrap().answered_by = Some(answer);
        }
        state.complete(&turn, vec![]).unwrap();
    }
    for status in ["pending","sending","uncertain","rejected"] {
        state.outbox.insert(0,Delivery{id:status.into(),item:Outbound::Text{text:status.into()},state:status.into(),retry_at:0,remote_id:None});
    }
    state.save(&path).unwrap();
    let mut restored = ChannelState::load(&path,&cfg).unwrap();
    let reply = |remote: &str,user: &str| InboundMessage{id:"new-answer".into(),user_id:user.into(),text:"Blue".into(),reply_to:Some(remote.into()), sender:None };
    assert_eq!(restored.question_for(&reply("question-0","456")).unwrap().unwrap().ask_id,"ask-0");
    assert!(restored.question_for(&reply("question-0","789")).is_err());
    assert!(restored.question_for(&reply("question-1","456")).unwrap_err().to_string().contains("already has an answer"));
    let count = restored.outbox.len();
    restored.queue_question("no-longer-active", "ask-1", "do not repeat").unwrap();
    assert_eq!(restored.outbox.len(),count);
    restored.recover();
    for status in ["pending","uncertain","rejected"] { assert!(restored.outbox.iter().any(|d|d.id==status&&d.state==status)); }
    assert!(restored.outbox.iter().any(|d|d.id=="sending"&&d.state=="uncertain"));
    std::fs::rename(path.with_extension("history.sqlite"),dir.path().join("saved-history.sqlite")).unwrap();
    assert!(ChannelState::load(&path,&cfg).is_err());
    assert!(restored.admit(PollBatch{cursor:None,messages:vec![reply("question-0","456")]}).is_err());
    assert!(restored.save(&path).is_err());
}

#[cfg(unix)]
#[test]
fn archived_history_survives_failure_before_active_snapshot_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(),Platform::Telegram);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path,&cfg).unwrap();
    for i in 0..70 {
        state.jobs.push(Job{input:InboundMessage{id:i.to_string(),user_id:"456".into(),text:"retained input".into(),reply_to:None, sender:None },turn_id:format!("turn-{i}"),state:"done".into()});
    }
    // Existing pre-migration snapshot remains the recovery authority until
    // atomic replacement; make that replacement fail after archive commit.
    let old = serde_json::to_vec(&state).unwrap();
    std::fs::write(&path,&old).unwrap();
    let backup = dir.path().join("old.json");
    std::fs::rename(&path,&backup).unwrap();
    std::os::unix::fs::symlink(&backup,&path).unwrap();
    assert!(state.save(&path).is_err());
    assert!(path.with_extension("history.sqlite").exists());
    assert_eq!(state.jobs.len(),70);
    assert_eq!(std::fs::read(&backup).unwrap(),old);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&backup,&path).unwrap();
    let mut recovered = ChannelState::load(&path,&cfg).unwrap();
    recovered.save(&path).unwrap();
    assert_eq!(recovered.jobs.len(),64);
    let archived: Job = archive::find(Some(&path.with_extension("history.sqlite")),true,&cfg.binding(),"job","id","turn-0").unwrap().unwrap();
    assert_eq!(archived.input.text,"retained input");
}

#[test]
fn channel_chunks_keep_readable_words_and_visible_characters_together() {
    for limit in [2000, 4096] {
        for glyph in ["e\u{301}", "👩🏽‍💻", "🇨🇦", "क्\u{200d}ष"] {
            let prefix = "x".repeat(limit - 1);
            let text = format!("{prefix}{glyph} done");
            let parts = split_text(&text, limit);
            assert_eq!(parts.concat(), text);
            assert_eq!(parts[0], prefix, "a visible character must move intact: {glyph}");
            assert!(parts[1].starts_with(glyph));
            assert!(parts.iter().all(|part| part.encode_utf16().count() <= limit));
        }
        let text = format!("{}remaining words", "word ".repeat(limit / 5 - 1));
        let parts = split_text(&text, limit);
        assert!(parts[0].ends_with(' '), "prefer a nearby word boundary");
        assert_eq!(parts.concat(), text);
    }
}

#[test]
fn channel_chunking_handles_oversized_graphemes_without_loss_or_looping() {
    let text = format!("a{} followed by normal text 👩🏽‍💻", "\u{301}".repeat(10_000));
    for limit in [2, 2000, 4096] {
        let pieces = split_text(&text, limit);
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|p| !p.is_empty() && p.encode_utf16().count() <= limit));
    }
    assert_eq!(split_text("", 2000), Vec::<String>::new());
    assert_eq!(split_text("  exact\r\ntext  ", 2000), ["  exact\r\ntext  "]);
}
#[test]
fn final_images_are_staged_and_outside_files_are_not_attached() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let image = dir.path().join("banana.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([240, 220, 20]))
        .save(&image)
        .unwrap();
    let forbidden = outside.path().join("private.png");
    std::fs::copy(&image, &forbidden).unwrap();
    let cfg = config(dir.path(), Platform::Telegram);
    let text = format!(
        "Finished. ![banana](banana.png) ![private]({})",
        forbidden.display()
    );
    let output = project_reply(&cfg, &text, &dir.path().join("outbox")).unwrap();
    assert_eq!(
        output
            .iter()
            .filter(|o| matches!(o, Outbound::Image { .. }))
            .count(),
        1
    );
    assert!(
        matches!(&output[0],Outbound::Text{text} if text.contains("Image attached") && text.contains("unavailable"))
    );
    let Outbound::Image { path, .. } = &output[1] else {
        panic!()
    };
    let saved = std::fs::read(path).unwrap();
    std::fs::write(image, b"changed after finalization").unwrap();
    assert_eq!(std::fs::read(path).unwrap(), saved);
    assert_eq!(
        project_reply(
            &cfg,
            "Tool screenshot at banana.png",
            &dir.path().join("outbox")
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn encoded_final_image_paths_resolve_once_and_keep_workspace_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Project Phoenix");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("résultat + 100%20.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([25, 40, 70])).save(&source).unwrap();
    let original = std::fs::read(&source).unwrap();
    let file_url = url::Url::from_file_path(&source).unwrap();
    let absolute = file_url.path();
    let relative = absolute.rsplit('/').next().unwrap();
    let outside = dir.path().join("private.png");
    std::fs::copy(&source, &outside).unwrap();
    for platform in [Platform::Telegram, Platform::Discord] {
        let cfg = config(&root, platform);
        for destination in [relative, absolute, file_url.as_str()] {
            let output = project_reply(&cfg, &format!("Finished. ![Proof]({destination})"), &root.join("outbox")).unwrap();
            let images: Vec<_> = output.iter().filter_map(|item| match item { Outbound::Image { path, .. } => Some(path), _ => None }).collect();
            assert_eq!(images.len(), 1, "{destination}");
            assert_eq!(std::fs::read(images[0]).unwrap(), original);
        }
        for destination in ["%2e%2e/private.png", "file://remote/private.png", "%FF.png", "%00.png"] {
            let output = project_reply(&cfg, &format!("![Private]({destination})"), &root.join("outbox")).unwrap();
            assert!(output.iter().all(|item| !matches!(item, Outbound::Image { .. })), "{destination}");
        }
    }
}
#[test]
fn durable_admission_deduplicates_and_preserves_uncertain_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Discord);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    let message = InboundMessage {
                    reply_to: None,
        id: "42".into(),
        user_id: "456".into(),
        text: "hello".into(), sender:None 
    };
    for _ in 0..2 {
        state
            .admit(PollBatch {
                cursor: Some("42".into()),
                messages: vec![message.clone()],
            })
            .unwrap();
    }
    assert_eq!(state.jobs.len(), 1);
    let turn = state.jobs[0].turn_id.clone();
    state
        .complete(
            &turn,
            vec![Outbound::Text {
                text: "Final answer".into(),
            }],
        )
        .unwrap();
    state
        .complete(
            &turn,
            vec![Outbound::Text {
                text: "Duplicate".into(),
            }],
        )
        .unwrap();
    assert_eq!(state.outbox.len(), 1);
    state.outbox[0].state = "sending".into();
    state.save(&path).unwrap();
    let mut restored = ChannelState::load(&path, &cfg).unwrap();
    restored.recover();
    assert_eq!(restored.outbox[0].state, "uncertain");
    let mut changed = cfg.clone();
    changed.conversation_id = "999".into();
    assert!(ChannelState::load(&path, &changed).is_err());
}
#[test]
fn rejects_unrestricted_or_malformed_connection_config() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), Platform::Telegram);
    cfg.validate().unwrap();
    cfg.allowed_user_ids.clear();
    assert!(cfg.validate().is_err());
    cfg.allowed_user_ids = vec!["456".into()];
    cfg.conversation_id = "123/messages?leak".into();
    assert!(cfg.validate().is_err());
}

#[test]
fn channel_failures_never_export_diagnostic_receipts_or_their_images() {
    let dir=tempfile::tempdir().unwrap();
    let source=dir.path().join("internal.png");
    image::RgbImage::new(8,8).save(&source).unwrap();
    let raw=format!("**[coder]** The `coder` agent could not complete its turn: usage_limit_reached\n{}\n![internal tool image]({})", "diagnostic receipt\n".repeat(1000),source.display());
    for platform in [Platform::Telegram,Platform::Discord] {
        let output=project_reply(&config(dir.path(),platform),&raw,&dir.path().join("spool")).unwrap();
        assert_eq!(output.len(),1);
        let Outbound::Text{text}=&output[0] else {panic!("error must be text only")};
        assert!(text.contains("usage limit"));assert!(text.len()<200);
        assert!(!dir.path().join("spool").exists(),"diagnostic image must not be staged");
    }
}

#[test]
fn remote_question_reply_binding_survives_restart_and_rejects_other_senders() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Telegram);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    let input = InboundMessage { id:"1".into(), user_id:"456".into(), text:"Help me choose".into(), reply_to:None, sender:None  };
    state.admit(PollBatch { cursor:Some("2".into()), messages:vec![input.clone()] }).unwrap();
    let turn = state.jobs[0].turn_id.clone();
    let text = format!("{}\nReply to this message.", "Question details ".repeat(250));
    state.queue_question(&turn, "ask-a", &text).unwrap();
    state.queue_question(&turn, "ask-a", &text).unwrap();
    assert_eq!(state.questions.len(), 1);
    assert!(state.outbox.len() > 1);
    let joined:String = state.outbox.iter().map(|d| match &d.item { Outbound::Question{text} => text.as_str(), _ => panic!() }).collect();
    assert_eq!(joined,text);
    for (index, delivery) in state.outbox.iter_mut().enumerate() { delivery.state="sent".into(); delivery.remote_id=Some(format!("remote-{index}")); }
    state.save(&path).unwrap();
    let mut state = ChannelState::load(&path,&cfg).unwrap(); state.recover();
    for index in 0..state.outbox.len() {
        let reply=InboundMessage {id:"reply-a".into(),reply_to:Some(format!("remote-{index}")),..input.clone()};
        assert_eq!(state.question_for(&reply).unwrap().unwrap().ask_id,"ask-a");
        assert!(state.question_for(&InboundMessage{user_id:"999".into(),..reply.clone()}).is_err());
        assert!(state.question_for(&InboundMessage{reply_to:Some("unrelated".into()),..reply.clone()}).is_err());
        state.questions[0].answered_by=Some("reply-a".into());
        assert!(state.question_for(&reply).unwrap().is_some());
        assert!(state.question_for(&InboundMessage{id:"another-reply".into(),..reply}).is_err());
    }
    assert!(state.question_for(&input).unwrap().is_none(),"Unquoted text never resolves a card");
}

#[test]
fn inbound_keeps_real_reply_ids_and_ignores_discord_forward_references() {
    let dir=tempfile::tempdir().unwrap();
    let telegram=json!({"message":{"message_id":9,"from":{"id":456},"chat":{"id":123},"text":"Blue","reply_to_message":{"message_id":7}}});
    assert_eq!(inbound(&config(dir.path(),Platform::Telegram),&telegram).unwrap().reply_to.as_deref(),Some("7"));
    let mut discord=json!({"id":"9","channel_id":"123","author":{"id":"456"},"content":"Blue","message_reference":{"message_id":"7","channel_id":"123","type":0}});
    let cfg=config(dir.path(),Platform::Discord);
    assert_eq!(inbound(&cfg,&discord).unwrap().reply_to.as_deref(),Some("7"));
    discord["message_reference"]["type"]=json!(1);
    assert!(inbound(&cfg,&discord).unwrap().reply_to.is_none());
    discord["message_reference"]["type"]=json!(0);discord["message_reference"]["channel_id"]=json!("999");
    assert!(inbound(&cfg,&discord).unwrap().reply_to.is_none());
}

// A valid ordered backlog with bounded text chunks. Arithmetic here only
// prepares independent test bytes; production does not maintain a delta ledger.
fn fill_capacity_backlog(state: &mut ChannelState, target: usize) {
    let mut size = serde_json::to_vec(state).unwrap().len();
    while target - size > 256 {
        let mut delivery = Delivery { id:format!("{:024x}", state.outbox.len()),
            item:Outbound::Text{text:String::new()}, state:if state.outbox.is_empty(){"uncertain"}else{"pending"}.into(),
            retry_at:0, remote_id:None };
        let overhead = serde_json::to_vec(&delivery).unwrap().len() + usize::from(!state.outbox.is_empty());
        let count = 3000.min(target - size - overhead);
        if let Outbound::Text { text } = &mut delivery.item { *text = "x".repeat(count); }
        size += overhead + count;
        state.outbox.push(delivery);
    }
    if let Some(Delivery { item:Outbound::Text{text}, .. }) = state.outbox.last_mut() {
        text.push_str(&"x".repeat(target - size));
        assert!(text.len() <= 4096);
    }
    assert_eq!(serde_json::to_vec(state).unwrap().len(), target);
}

#[test]
fn channel_capacity_reserves_other_observers_and_rejects_intake_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Discord);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    state.admit(PollBatch { cursor:Some("old".into()), messages:(0..3).map(|i| InboundMessage {
        id:i.to_string(), user_id:"456".into(), text:"Existing queued work".into(), reply_to:None, sender:None 
    }).collect() }).unwrap();
    let reserve = store::result_reserve(&dir.path().join("images")).unwrap();
    let target = store::STATE_MAX_BYTES - store::CONTROL_BYTES - 3 * reserve - 1024;
    fill_capacity_backlog(&mut state, target);
    state.save(&path).unwrap();
    state.set_active_observers(2);
    assert!(state.can_start());
    let before = serde_json::to_vec(&state).unwrap();
    // Raw input is below its limit, but its escaped JSON would consume the
    // remaining result reservation. No prefix of the batch or cursor is kept.
    let batch = || PollBatch { cursor:Some("new".into()), messages:vec![InboundMessage {
        id:"new".into(), user_id:"456".into(), text:"\u{1}".repeat(64 * 1024), reply_to:None, sender:None 
    }] };
    assert!(state.admit(batch()).unwrap_err().is::<ChannelCapacity>());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert!(state.jobs.iter().all(|job| job.state == "queued"));
    state.set_active_observers(3); // the final allowed observer has now started
    assert!(!state.can_start());
    let output = project_reply(&cfg, &"\u{1}".repeat(MAX_REPLY_BYTES), &dir.path().join("images")).unwrap();
    let id = state.jobs[0].turn_id.clone();
    assert!(state.complete(&id, output.clone()).unwrap_err().is::<ChannelCapacity>(), "counting this result twice must not be needed");
    assert_eq!(state.jobs[0].state, "queued");
    state.set_active_observers(2); // release only this completed observer
    state.complete(&id, output.clone()).unwrap();
    state.save(&path).unwrap();
    let count = state.outbox.len();
    state.complete(&id, output).unwrap();
    assert_eq!(state.outbox.len(), count, "same result never creates a second delivery");
    assert_eq!(state.jobs[0].state, "done");
    assert_eq!(state.jobs[1].state, "queued");
    assert_eq!(state.outbox[0].state, "uncertain");
    assert_eq!(state.cursor.as_deref(), Some("old"));
    assert!(state.capacity_stamp().0 + 2 * reserve + store::CONTROL_BYTES <= store::STATE_MAX_BYTES);
}

#[test]
fn channel_capacity_empty_poll_and_queries_do_not_encode_or_mutate_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), Platform::Telegram);
    let path = dir.path().join("state.json");
    let mut state = ChannelState::load(&path, &cfg).unwrap();
    state.set_active_observers(16); // legacy producer count: no new payload fits
    let stamp = state.capacity_stamp();
    for _ in 0..100 {
        assert!(!state.can_start());
        state.admit(PollBatch { cursor:None, messages:vec![] }).unwrap();
        assert_eq!(state.capacity_stamp(), stamp);
    }
    assert!(!path.exists(), "idle observation must not write a file");
    state.set_active_observers(0);
    let input = InboundMessage { id:"1".into(), user_id:"456".into(), text:"One request".into(), reply_to:None, sender:None  };
    state.admit(PollBatch { cursor:Some("2".into()), messages:vec![input.clone(),input] }).unwrap();
    assert_eq!(state.jobs.len(), 1, "a repeated ID inside one poll is admitted once");
}

#[test]
fn channel_capacity_reserve_covers_adversarial_real_projection() {
    let dir = tempfile::tempdir().unwrap();
    // Escaped paths are real filesystem paths, not guessed envelope lengths.
    let spool = dir.path().join("quoted-\"-\\-e\u{301}-漢字".repeat(5)).join("images");
    let reserve = store::result_reserve(&spool).unwrap();
    let repeat = |pattern: &str| pattern.repeat(MAX_REPLY_BYTES / pattern.len());
    let patterns = [repeat("\u{1}"),repeat("x\r\n\"\\\u{2}"),repeat("👩🏽‍🔧🇨🇦e\u{301}"),
        repeat(&format!("xa{}z", "\u{301}".repeat(2001))), "![]()".repeat(512),
        format!("{}\n\n[x]: missing.png\n", "![x]".repeat(512))];
    for platform in [Platform::Telegram,Platform::Discord] {
        let cfg = config(dir.path(), platform);
        for (case, text) in patterns.iter().enumerate() {
            let projected = project_reply(&cfg,text,&spool).unwrap();
            let deliveries = projected.into_iter().map(|item| Delivery {
                id:"f".repeat(24), item, state:"pending".into(), retry_at:0, remote_id:None,
            }).collect::<Vec<_>>();
            let bytes = serde_json::to_vec(&deliveries).unwrap().len();
            assert!(bytes < reserve, "{platform:?} case {case}: {bytes} > {reserve}");
            if case == 0 { assert!(bytes > 6 * (MAX_REPLY_BYTES - 1), "exercise actual JSON escaping"); }
            if case >= 4 {
                let text = deliveries.iter().map(|d| match &d.item { Outbound::Text{text}=>text.as_str(), _=>panic!() }).collect::<String>();
                assert_eq!(text.matches(IMAGE_UNAVAILABLE).count(),512);
                assert!(text.len() > patterns[case].len() * 10, "inline and shortcut image markup expand in production projection");
            }
            for mut delivery in deliveries {
                if let Outbound::Text { text } = &mut delivery.item {
                    text.clear(); delivery.state="uncertain".into(); delivery.retry_at=u64::MAX;
                    delivery.remote_id=Some("9".repeat(20));
                    assert!(serde_json::to_vec(&delivery).unwrap().len() + 1 <= 256);
                }
            }
        }
        let mut markdown = String::new();
        for index in 0..MAX_REPLY_IMAGES {
            let name = format!("image-{index}.png");
            image::RgbImage::from_pixel(2,2,image::Rgb([index as u8,2,3])).save(dir.path().join(&name)).unwrap();
            markdown.push_str(&format!("![proof]({name}) "));
        }
        let output = project_reply(&cfg,&markdown,&spool).unwrap();
        assert_eq!(output.iter().filter(|o|matches!(o,Outbound::Image{..})).count(),MAX_REPLY_IMAGES);
        for item in &output {
            if let Outbound::Image { path, name } = item {
                assert_eq!(name.len(),"image-0000000000000000.png".len());
                assert_eq!(path.parent(),Some(spool.as_path()));
                assert!(path.is_file());
            }
        }
        let deliveries = output.into_iter().map(|item| Delivery { id:"f".repeat(24),item,state:"pending".into(),retry_at:0,remote_id:None }).collect::<Vec<_>>();
        assert!(serde_json::to_vec(&deliveries).unwrap().len() < reserve);
        assert!(project_reply(&cfg,&"x".repeat(MAX_REPLY_BYTES+1),&spool).is_err());
    }
}

#[test]
fn channel_turns_carry_the_sender_label_and_group_target() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(root.path(), Platform::Telegram);
    let payload = json!({"message":{"message_id":7,"text":"status?","chat":{"id":123},"from":{"id":456,"first_name":"Ada","last_name":"L]ov"}}});
    let input = inbound(&cfg, &payload).unwrap();
    assert_eq!(input.sender.as_deref(), Some("Ada Lov"), "brackets never break the marker");
    assert_eq!(request_text(&cfg, &input), "[via Telegram · Ada Lov]\nstatus?");
    assert_eq!(target_agent(&cfg), Some("phoenix"));
    cfg.group_id = Some("launch-room".into());
    assert_eq!(target_agent(&cfg), None, "group connections address the group");
    let unnamed = InboundMessage { sender: None, ..input };
    assert_eq!(request_text(&cfg, &unnamed), "[via Telegram · user 456]\nstatus?");
}
