//! Durable inbox/outbox. A lost network response is retained for review,
//! rather than blindly sending a second copy.
use super::*;
use serde::{Deserialize, Serialize};

// Keep the private-file ceiling unchanged. Control space is unavailable to
// payload admission, so settling observers/sends and manual review can persist.
pub(crate) const STATE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const CONTROL_BYTES: usize = 1024 * 1024;
pub(crate) const CAPACITY_NOTICE: &str = "Channel reply capacity is in use. New messages and work are paused; existing work is retained. Keep this channel connected to send saved replies. Do not resubmit pending work.";

#[derive(Debug)]
pub struct ChannelCapacity;
impl std::fmt::Display for ChannelCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(CAPACITY_NOTICE) }
}
impl std::error::Error for ChannelCapacity {}

pub(super) fn result_reserve(spool: &Path) -> Result<usize> {
    // JSON escapes cost at most six bytes per input byte. Image replacement
    // can expand a shortcut reference image, ![x] (four bytes), to a longer
    // ASCII notice. These spend disjoint input ranges, so use the larger ratio.
    let expansion = 6usize.max(IMAGE_UNAVAILABLE.len().max(IMAGE_ATTACHED.len()).div_ceil(4));
    let text = MAX_REPLY_BYTES * expansion;
    // Normal chunks consume >= half the UTF-16 limit, except the final chunk.
    // Oversized graphemes may add a short tail and a short preceding chunk;
    // charge both to the >=limit grapheme. UTF-16 units never exceed UTF-8 bytes.
    let chunks = 4 * text / (MIN_TEXT_UNITS - 2) + 4;
    let image = Delivery { id:"0".repeat(24), item:Outbound::Image {
        path:spool.join("image-0000000000000000.png"), name:"image-0000000000000000.png".into(),
    }, state:"pending".into(), retry_at:0, remote_id:None };
    let empty_text = Delivery { item:Outbound::Text{text:String::new()}, ..image.clone() };
    // Derive both pending envelopes with serde, including their array commas.
    // Later send-state/remote-ID updates use control space; settled history is
    // archived at each save. Image paths use the actual connection spool path.
    Ok(text + chunks * (serde_json::to_vec(&empty_text)?.len() + 1)
        + MAX_REPLY_IMAGES * (serde_json::to_vec(&image)?.len() + 1))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub input: InboundMessage,
    pub turn_id: String,
    pub state: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub item: Outbound,
    pub state: String,
    pub retry_at: u64,
    pub remote_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelQuestion {
    pub ask_id: String,
    pub turn_id: String,
    pub user_id: String,
    pub delivery_ids: Vec<String>,
    pub answered_by: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub binding: String,
    pub cursor: Option<String>,
    pub jobs: Vec<Job>,
    pub outbox: Vec<Delivery>,
    pub notice: Option<String>,
    // Legacy states may omit this field. Do not make a no-op startup save
    // larger just by reintroducing an empty default at the file-size ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<ChannelQuestion>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    history_archived: bool,
    #[serde(skip)]
    archive_path: Option<PathBuf>,
    #[serde(skip)]
    encoded_bytes: usize,
    #[serde(skip)]
    result_reserve: usize,
    #[serde(skip)]
    active_observers: usize,
}
impl ChannelState {
    pub fn load(path: &Path, config: &ChannelConfig) -> Result<Self> {
        let mut state: Self = match crate::config::private_io::read_private_file(path)? {
            Some(raw) => {
                let mut state: Self = serde_json::from_slice(&raw)
                    .context("Channel state is unreadable; it was not replaced")?;
                state.encoded_bytes = raw.len();
                state
            },
            None => Self {
                binding: config.binding(),
                cursor: None,
                jobs: vec![],
                outbox: vec![],
                notice: None,
                questions: vec![],
                history_archived: false,
                archive_path: None,
                encoded_bytes: 0,
                result_reserve: 0,
                active_observers: 0,
            },
        };
        ensure!(state.binding == config.binding(), "Channel destination or agent changed. Use a new connection ID to keep previous deliveries with their original destination.");
        state.archive_path = Some(path.with_extension("history.sqlite"));
        state.result_reserve = result_reserve(&path.parent().unwrap_or(Path::new(".")).join("images"))?;
        if state.encoded_bytes == 0 { state.encoded_bytes = serde_json::to_vec(&state)?.len(); }
        if state.history_archived { archive::validate(state.archive_path.as_deref().unwrap(), &state.binding)?; }
        Ok(state)
    }
    /// Only the worker knows which observers are still producing results.
    /// Count finished-but-not-collected handles and retained terminal outputs
    /// too. Release only the result currently being published before checking it.
    pub fn set_active_observers(&mut self, count: usize) { self.active_observers = count; }
    pub fn capacity_stamp(&self) -> (usize, usize) { (self.encoded_bytes, self.active_observers) }
    pub fn can_start(&self) -> bool { self.fits(self.encoded_bytes, 1) }
    fn fits(&self, bytes: usize, additional_results: usize) -> bool {
        bytes.saturating_add(CONTROL_BYTES).saturating_add(
            self.result_reserve.saturating_mul(self.active_observers.saturating_add(additional_results))
        ) <= STATE_MAX_BYTES
    }
    fn accept_mutation(&mut self, mut candidate: Self, additional_results: usize) -> Result<()> {
        // Exact encoding is only needed on a real mutation. Idle gates use the
        // last load/save/mutation size; there is no parallel delta ledger.
        if candidate.notice.as_deref() == Some(CAPACITY_NOTICE) { candidate.notice = None; }
        let bytes = serde_json::to_vec(&candidate)?.len();
        // An empty foreground acknowledgement (or an already-present result)
        // can shrink a legacy full snapshot without consuming anyone's reserve.
        let shrinking_completion = additional_results == 0 && candidate.jobs.iter().zip(&self.jobs)
            .any(|(after,before)| after.state == "done" && before.state != "done")
            && bytes <= self.encoded_bytes && bytes <= STATE_MAX_BYTES;
        if !self.fits(bytes, additional_results) && !shrinking_completion { return Err(ChannelCapacity.into()); }
        candidate.encoded_bytes = bytes;
        *self = candidate;
        Ok(())
    }
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let archive_path = path.with_extension("history.sqlite");
        ensure!(!self.history_archived || self.archive_path.as_ref() == Some(&archive_path), "Move channel state together with its history");
        if self.history_archived { archive::validate(&archive_path, &self.binding)?; }
        // Retain a small recent window plus every unresolved record. Never
        // archive a pending question or a possibly delivered HTTP request.
        fn split<T: Clone>(rows: &[T], settled: impl Fn(&T) -> bool) -> (Vec<T>, Vec<T>) {
            let mut remaining = rows.iter().filter(|row| settled(row)).count().saturating_sub(64);
            let mut active = Vec::new(); let mut cold = Vec::new();
            for row in rows {
                if remaining > 0 && settled(row) { cold.push(row.clone()); remaining -= 1; }
                else { active.push(row.clone()); }
            }
            (active, cold)
        }
        let (jobs, mut cold_jobs) = split(&self.jobs, |job| job.state == "done");
        let (outbox, mut cold_deliveries) = split(&self.outbox, |delivery| delivery.state == "sent");
        // A foreground answer job may still need the question for recovery.
        let mut settled_questions = std::collections::HashSet::new();
        for question in &self.questions {
            if let Some(id) = &question.answered_by {
                let settled = match self.jobs.iter().find(|job| &job.input.id == id) {
                    Some(job) => job.state == "done",
                    None => archive::find::<Job>(self.archive_path.as_deref(), self.history_archived, &self.binding, "job", "lookup", id)?.is_some(),
                };
                if settled { settled_questions.insert(question.ask_id.as_str()); }
            }
        }
        let (questions, mut cold_questions) = split(&self.questions, |question| settled_questions.contains(question.ask_id.as_str()));
        let mut snapshot = self.clone();
        if !cold_jobs.is_empty() || !cold_deliveries.is_empty() || !cold_questions.is_empty() {
            snapshot.history_archived = true;
        }
        snapshot.jobs = jobs; snapshot.outbox = outbox; snapshot.questions = questions;
        snapshot.archive_path = Some(archive_path);
        let mut bytes = serde_json::to_vec(&snapshot)?;
        if bytes.len() > STATE_MAX_BYTES {
            // A legacy file may have no control headroom (or omit defaulted
            // fields). One pressure pass may archive even the newest settled
            // rows, including a just-acknowledged job. Never prune unresolved
            // rows or loop over successively smaller windows and re-encode.
            let (jobs, extra): (Vec<_>, Vec<_>) = std::mem::take(&mut snapshot.jobs)
                .into_iter().partition(|job| job.state != "done");
            snapshot.jobs = jobs; cold_jobs.extend(extra);
            let (outbox, extra): (Vec<_>, Vec<_>) = std::mem::take(&mut snapshot.outbox)
                .into_iter().partition(|delivery| delivery.state != "sent");
            snapshot.outbox = outbox; cold_deliveries.extend(extra);
            let (questions, extra): (Vec<_>, Vec<_>) = std::mem::take(&mut snapshot.questions)
                .into_iter().partition(|question| !settled_questions.contains(question.ask_id.as_str()));
            snapshot.questions = questions; cold_questions.extend(extra);
            if !cold_jobs.is_empty() || !cold_deliveries.is_empty() || !cold_questions.is_empty() {
                snapshot.history_archived = true;
            }
            bytes = serde_json::to_vec(&snapshot)?;
        }
        if bytes.len() > STATE_MAX_BYTES { return Err(ChannelCapacity.into()); }
        snapshot.encoded_bytes = bytes.len();
        if !cold_jobs.is_empty() || !cold_deliveries.is_empty() || !cold_questions.is_empty() {
            archive::write(snapshot.archive_path.as_deref().unwrap(), &self.binding, &cold_jobs, &cold_deliveries, &cold_questions)?;
        }
        crate::config::private_io::atomic_write_private(path, &bytes)?;
        *self = snapshot;
        Ok(())
    }
    pub fn admit(&mut self, batch: PollBatch) -> Result<()> {
        let mut fresh = Vec::new();
        for message in batch.messages {
            if !self.jobs.iter().any(|job| job.input.id == message.id)
                && !fresh.iter().any(|input: &InboundMessage| input.id == message.id)
                && archive::find::<Job>(self.archive_path.as_deref(), self.history_archived, &self.binding, "job", "lookup", &message.id)?.is_none() {
                fresh.push(message);
            }
        }
        if fresh.is_empty() && self.cursor == batch.cursor { return Ok(()); }
        ensure!(
            self.jobs.iter().filter(|j| j.state != "done").count() + fresh.len() <= 1000,
            "Channel queue is full; messages remain at the provider until space is available"
        );
        ensure!(
            self.jobs
                .iter()
                .filter(|j| j.state != "done")
                .map(|j| j.input.text.len())
                .sum::<usize>()
                + fresh.iter().map(|m| m.text.len()).sum::<usize>()
                <= 8 * 1024 * 1024,
            "Channel queue is full; messages remain at the provider"
        );
        let mut candidate = self.clone();
        for input in fresh {
            let turn_id = format!(
                "channel_{}",
                digest(&format!("{}\0{}", self.binding, input.id))
            );
            if !self.jobs.iter().any(|j| j.turn_id == turn_id) {
                candidate.jobs.push(Job {
                    input,
                    turn_id,
                    state: "queued".into(),
                });
            }
        }
        candidate.cursor = batch.cursor;
        // Leave space to process intake, in addition to every current observer.
        self.accept_mutation(candidate, 1)
    }
    pub fn complete(&mut self, turn_id: &str, output: Vec<Outbound>) -> Result<()> {
        if !self.jobs.iter().any(|job| job.turn_id == turn_id)
            && archive::find::<Job>(self.archive_path.as_deref(), self.history_archived, &self.binding, "job", "id", turn_id)?.is_some() { return Ok(()); }
        let index = self
            .jobs
            .iter()
            .position(|j| j.turn_id == turn_id)
            .context("Unknown channel turn")?;
        if self.jobs[index].state == "done" {
            return Ok(());
        }
        let mut candidate = self.clone();
        for (index, item) in output.into_iter().enumerate() {
            let id = digest(&format!("{turn_id}\0{index}"))[..24].to_string();
            if !self.outbox.iter().any(|d| d.id == id) {
                candidate.outbox.push(Delivery {
                    id,
                    item,
                    state: "pending".into(),
                    retry_at: 0,
                    remote_id: None,
                });
            }
        }
        candidate.jobs[index].state = "done".into();
        // Never mark done or expose any partial multipart output before it fits.
        self.accept_mutation(candidate, 0)
    }
    pub fn queue_question(&mut self, turn_id: &str, ask_id: &str, text: &str) -> Result<()> {
        if self.questions.iter().any(|q| q.ask_id == ask_id) { return Ok(()); }
        if archive::find::<ChannelQuestion>(self.archive_path.as_deref(), self.history_archived, &self.binding, "question", "id", ask_id)?.is_some() { return Ok(()); }
        let user = self.jobs.iter().find(|j| j.turn_id == turn_id).context("Unknown question owner")?.input.user_id.clone();
        self.queue_question_as(turn_id, ask_id, text, user)
    }
    /// `user_id` empty: the question came from a task started in Phoenix, so
    /// any allowed person in the chat may answer it.
    pub fn queue_question_as(&mut self, turn_id: &str, ask_id: &str, text: &str, user_id: String) -> Result<()> {
        if self.questions.iter().any(|q| q.ask_id == ask_id) { return Ok(()); }
        if archive::find::<ChannelQuestion>(self.archive_path.as_deref(), self.history_archived, &self.binding, "question", "id", ask_id)?.is_some() { return Ok(()); }
        let mut question = ChannelQuestion { ask_id: ask_id.into(), turn_id: turn_id.into(), user_id, delivery_ids: vec![], answered_by: None };
        let mut candidate = self.clone();
        for (index, text) in split_text(text, MIN_TEXT_UNITS).into_iter().enumerate() {
            let id = digest(&format!("question\0{ask_id}\0{index}"))[..24].to_owned();
            question.delivery_ids.push(id.clone());
            candidate.outbox.push(Delivery { id, item: Outbound::Question { text }, state: "pending".into(), retry_at: 0, remote_id: None });
        }
        candidate.questions.push(question);
        self.accept_mutation(candidate, 0)
    }
    /// Only an exact reply to a successfully sent question can resolve it.
    /// Never infer the question from free text or whichever card is newest.
    pub fn question_for(&self, input: &InboundMessage) -> Result<Option<ChannelQuestion>> {
        let Some(remote) = &input.reply_to else {
            // No reply: while this person has an open question, their next
            // message is the answer (the question tells them so).
            return Ok(self.questions.iter().rev().find(|q| (q.user_id == input.user_id || q.user_id.is_empty()) && q.answered_by.is_none()
                && crate::runtime::asks::decision_record_for(&q.ask_id).ok().flatten()
                    .is_some_and(|record| record.status == "pending" && record.approval.is_none())).cloned());
        };
        let delivery = self.outbox.iter().find(|d| d.state == "sent" && d.remote_id.as_ref() == Some(remote)).cloned()
            .map(Ok).unwrap_or_else(|| archive::find::<Delivery>(self.archive_path.as_deref(), self.history_archived, &self.binding, "delivery", "lookup", remote)
                .and_then(|row| row.context("I couldn’t match that reply. Reply to a question from this connection, or send a new message.")))?;
        let question = self.questions.iter().find(|q| q.delivery_ids.contains(&delivery.id)).cloned()
            .map(|q| Ok(Some(q))).unwrap_or_else(|| archive::find::<ChannelQuestion>(self.archive_path.as_deref(), self.history_archived, &self.binding, "question_delivery", "id", &delivery.id))?;
        let Some(question) = question else { return Ok(None); };
        ensure!(question.user_id.is_empty() || question.user_id == input.user_id, "Only the person who started this request can answer its question.");
        ensure!(question.answered_by.as_ref().is_none_or(|id| id == &input.id), "This question already has an answer. Start a new message to continue.");
        Ok(Some(question.clone()))
    }
    pub fn recover(&mut self) {
        for job in &mut self.jobs {
            if job.state == "running" {
                job.state = "needs_review".into();
            }
        }
        for delivery in &mut self.outbox {
            if delivery.state == "sending" {
                delivery.state = "uncertain".into();
            }
        }
    }
    pub fn summary(&self) -> serde_json::Value {
        let mut summary = self.summary_counts();
        summary.as_object_mut().unwrap().remove("review_paging");
        summary.as_object_mut().unwrap().remove("review_counts");
        summary["review_turns"] = serde_json::json!(self.jobs.iter().filter(|j|matches!(j.state.as_str(),"needs_review"|"running")).map(|j|serde_json::json!({"turn_id":j.turn_id,"state":j.state})).collect::<Vec<_>>());
        summary["review_deliveries"] = serde_json::json!(self.outbox.iter().filter(|d|matches!(d.state.as_str(),"uncertain"|"rejected"|"sending")).map(|d|serde_json::json!({"id":d.id,"state":d.state})).collect::<Vec<_>>());
        summary
    }
    /// Desktop status needs counts without allocating every recovery identity.
    pub fn summary_counts(&self) -> serde_json::Value {
        let mut summary = serde_json::json!({"sending_replies":self.outbox.iter().filter(|d|d.state=="sending").count(),"sending_images":self.outbox.iter().filter(|d|d.state=="sending"&&matches!(d.item,Outbound::Image{..})).count(),"queued":self.jobs.iter().filter(|j|j.state=="queued").count(),"running":self.jobs.iter().filter(|j|j.state=="running").count(),"needs_review":self.jobs.iter().filter(|j|j.state=="needs_review").count(),"pending_replies":self.outbox.iter().filter(|d|d.state=="pending").count(),"uncertain_replies":self.outbox.iter().filter(|d|d.state=="uncertain").count(),"rejected_replies":self.outbox.iter().filter(|d|d.state=="rejected").count(),"notice":self.notice,"review_paging":true,"review_counts":{"turns":self.jobs.iter().filter(|j|matches!(j.state.as_str(),"needs_review"|"running")).count(),"deliveries":self.outbox.iter().filter(|d|matches!(d.state.as_str(),"uncertain"|"rejected"|"sending")).count()}});
        let paused = !self.can_start() || self.notice.as_deref() == Some(CAPACITY_NOTICE);
        summary["capacity"] = serde_json::json!({"paused":paused,"serialized_bytes":self.encoded_bytes,
            "limit_bytes":STATE_MAX_BYTES,"active_observers":self.active_observers,"result_reserve_bytes":self.result_reserve});
        // A near-cap legacy file remains readable. Status requires no growth or
        // ownership mutation merely to explain how to recover it.
        if paused {
            let review = if self.outbox.iter().any(|d| matches!(d.state.as_str(), "uncertain" | "rejected")) {
                " A delivery needs review: disconnect, check it in the channel, and resolve it in Phoenix before reconnecting."
            } else { "" };
            summary["notice"] = serde_json::json!(format!("{CAPACITY_NOTICE}{review}"));
        }
        summary
    }
    /// A request-scoped semantic snapshot. Stream the already-loaded state into
    /// its digest; no second file read, cached snapshot, or durable schema field.
    /// This is only used by explicit review reads/actions, never idle admission.
    pub fn review_snapshot(&self, config: &ChannelConfig) -> Result<String> {
        struct HashWriter(Sha256);
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes); Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, config)?;
        writer.0.update(b"\0");
        serde_json::to_writer(&mut writer, self)?;
        Ok(format!("{:x}", writer.0.finalize()))
    }
    pub fn review_page(&self, kind: &str, offset: usize, limit: usize) -> Result<serde_json::Value> {
        ensure!((1..=100).contains(&limit), "Review page size must be 1–100");
        let records: Box<dyn Iterator<Item = (&str, &str, &str, &str)> + '_> = match kind {
            "turns" => Box::new(self.jobs.iter().filter(|j|matches!(j.state.as_str(),"needs_review"|"running")).map(|j|(j.turn_id.as_str(),j.state.as_str(),"message",j.input.text.as_str()))),
            "deliveries" => Box::new(self.outbox.iter().filter(|d|matches!(d.state.as_str(),"uncertain"|"rejected"|"sending")).map(|d| {
                let (kind, text) = match &d.item {
                    Outbound::Text{text} => ("text",text.as_str()),
                    Outbound::Question{text} => ("question",text.as_str()),
                    Outbound::Image{name,..} => ("image",name.as_str()),
                };
                (d.id.as_str(),d.state.as_str(),kind,text)
            })),
            _ => bail!("Choose deliveries or turns for review"),
        };
        let mut items = Vec::new();
        let mut total = 0usize;
        for (id, state, preview_kind, text) in records {
            if total >= offset && items.len() < limit {
                ensure!(!id.is_empty() && id.len() <= 128 && id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'),
                    "A recovery ID cannot be displayed safely. Inspect this connection with the channels status CLI; its records were preserved");
                // Excerpts come only from this connection's actual message or
                // prepared reply. Image filesystem paths are never projected.
                let preview = serde_json::json!({"kind":preview_kind,"text":text.chars().take(200).collect::<String>(),
                    "truncated":text.chars().nth(200).is_some()});
                items.push(if kind == "turns" { serde_json::json!({"turn_id":id,"state":state,"preview":preview}) }
                    else { serde_json::json!({"id":id,"state":state,"preview":preview}) });
            }
            total += 1;
        }
        ensure!(offset < total || (offset == 0 && total == 0), "Review page is out of range. Refresh the recovery list");
        Ok(serde_json::json!({"kind":kind,"offset":offset,"limit":limit,"total":total,
            "next_offset":(offset + items.len() < total).then_some(offset + items.len()),"items":items}))
    }
}
