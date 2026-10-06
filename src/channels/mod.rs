//! User-facing channel boundary. Only deliberate final replies and their
//! referenced images cross this boundary; runtime events never do.
pub mod credentials;
mod archive;
mod store;
mod transport;
pub use store::{ChannelCapacity, ChannelQuestion, ChannelState, Delivery, Job};
pub use transport::{ChannelApi, ChannelPoll, ChannelSend, PollBatch, SendFailure};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Telegram,
    Discord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    /// Exact chat/channel id; never inferred from an incoming message.
    pub conversation_id: String,
    pub allowed_user_ids: Vec<String>,
    pub agent_id: String,
    pub session_id: String,
    pub workspace: PathBuf,
    /// Secret stays outside settings/status JSON and process arguments.
    pub token_env: String,
    #[serde(default)]
    pub enabled: bool,
    /// When set, messages go to this Phoenix group (its conversation is
    /// `session_id`) instead of one coworker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}
impl ChannelConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 64
                && self
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Choose a channel ID using letters, numbers, '-' or '_'"
        );
        ensure!(
            !self.name.trim().is_empty() && self.name.chars().count() <= 80,
            "Name must contain 1–80 characters"
        );
        ensure!(
            numeric_id(&self.conversation_id, self.platform == Platform::Telegram),
            "Invalid conversation ID"
        );
        ensure!(
            !self.allowed_user_ids.is_empty()
                && self.allowed_user_ids.iter().all(|id| numeric_id(id, false)),
            "Choose at least one allowed user ID"
        );
        ensure!(
            !self.agent_id.trim().is_empty() && !self.session_id.trim().is_empty(),
            "Choose an agent and its conversation"
        );
        ensure!(
            self.workspace.is_absolute() && self.workspace.is_dir(),
            "Workspace must be an existing absolute directory"
        );
        ensure!(
            !self.token_env.is_empty()
                && self.token_env.len() <= 128
                && self
                    .token_env
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                && !self.token_env.as_bytes()[0].is_ascii_digit(),
            "Invalid token environment variable name"
        );
        Ok(())
    }
    pub fn binding(&self) -> String {
        digest(&format!(
            "{:?}\0{}\0{}\0{}\0{}",
            self.platform,
            self.conversation_id,
            self.agent_id,
            self.session_id,
            self.workspace.display()
        ))
    }
}
fn numeric_id(id: &str, negative: bool) -> bool {
    let digits = if negative {
        id.strip_prefix('-').unwrap_or(id)
    } else {
        id
    };
    !digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit())
}
fn sender_name(user: &serde_json::Value) -> Option<String> {
    let text = |key: &str| user.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty());
    let name = match (text("first_name"), text("last_name")) {
        (Some(first), Some(last)) => Some(format!("{first} {last}")),
        (Some(first), None) => Some(first.to_string()),
        _ => text("global_name").or_else(|| text("username")).map(str::to_string),
    }?;
    let clean: String = name.chars().filter(|c| !c.is_control() && *c != '[' && *c != ']').take(60).collect();
    (!clean.trim().is_empty()).then(|| clean.trim().to_string())
}
/// The text a channel message becomes in Phoenix. The leading marker tells
/// the coworker where the message came from and lets Canvas label the bubble
/// ("via Telegram · Mik"). Receipts key on this exact text, so every path that
/// submits or recovers a channel turn must build it here.
pub fn request_text(config: &ChannelConfig, input: &InboundMessage) -> String {
    let platform = match config.platform {
        Platform::Telegram => "Telegram",
        Platform::Discord => "Discord",
    };
    let who = input.sender.clone().unwrap_or_else(|| format!("user {}", input.user_id));
    format!("[via {platform} · {who}]\n{}", input.text)
}
/// The target agent recorded for this connection's turns; group connections
/// address the group, not one coworker.
pub fn target_agent(config: &ChannelConfig) -> Option<&str> {
    config.group_id.is_none().then_some(config.agent_id.as_str())
}
pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboundMessage {
    pub id: String,
    pub user_id: String,
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Display name of the sender, for the "via Telegram · Name" label.
    #[serde(default)]
    pub sender: Option<String>,
}
/// Authorization precedes text extraction. Bots, edits and unrelated chats
/// cannot create turns or prompt-inject their way onto another agent route.
/// The "• option" lines of a question, in order. Telegram shows them as
/// buttons; a tap sends back only the option's index.
pub fn question_options(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("• "))
        .map(|option| option.trim().to_string())
        .filter(|option| !option.is_empty())
        .take(12)
        .collect()
}
pub fn inbound(config: &ChannelConfig, payload: &serde_json::Value) -> Option<InboundMessage> {
    // A tapped question button is the same as replying with that option.
    if config.platform == Platform::Telegram {
        if let Some(tap) = payload.get("callback_query") {
            let question = tap.get("message")?;
            let index: usize = tap.get("data")?.as_str()?.strip_prefix("opt:")?.parse().ok()?;
            let option = question_options(question.get("text")?.as_str()?).into_iter().nth(index)?;
            let update = payload.get("update_id").and_then(|v| v.as_i64()).unwrap_or_default();
            let synthetic = serde_json::json!({"message":{
                "message_id": format!("tap-{update}"), "from": tap.get("from")?, "chat": question.get("chat")?,
                "text": option, "reply_to_message": {"message_id": question.get("message_id")?}
            }});
            return inbound(config, &synthetic);
        }
    }
    let (message, user, conversation, id) = match config.platform {
        Platform::Telegram => {
            let m = payload.get("message")?;
            (
                m,
                m.get("from")?,
                m.get("chat")?.get("id")?,
                m.get("message_id")?,
            )
        }
        Platform::Discord => (
            payload,
            payload.get("author")?,
            payload.get("channel_id")?,
            payload.get("id")?,
        ),
    };
    let string_id = |v: &serde_json::Value| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    };
    let user_id = string_id(user.get("id")?)?;
    if string_id(conversation)? != config.conversation_id
        || !config.allowed_user_ids.contains(&user_id)
        || user
            .get("is_bot")
            .or_else(|| user.get("bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        || message.get("webhook_id").is_some()
    {
        return None;
    }
    let text = message
        .get(if config.platform == Platform::Telegram {
            "text"
        } else {
            "content"
        })?
        .as_str()?
        .trim();
    if text.is_empty() || text.len() > 64 * 1024 {
        return None;
    }
    let reply_to = match config.platform {
        Platform::Telegram => message.get("reply_to_message").and_then(|m| m.get("message_id")).and_then(string_id),
        Platform::Discord => message.get("message_reference").filter(|r| r.get("type").and_then(|v| v.as_u64()).unwrap_or(0) == 0 && r.get("channel_id").and_then(string_id).is_none_or(|id| id == config.conversation_id)).and_then(|r| r.get("message_id")).and_then(string_id),
    };
    Some(InboundMessage {
        id: string_id(id)?,
        user_id,
        text: text.to_owned(),
        reply_to,
        sender: sender_name(user),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    Text { text: String },
    Question { text: String },
    Image { path: PathBuf, name: String },
}

// Shared with the store's conservative reservation for one completed result.
pub(crate) const MAX_REPLY_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_REPLY_IMAGES: usize = 10;
pub(crate) const MIN_TEXT_UNITS: usize = 2000;
pub(crate) const IMAGE_ATTACHED: &str = "[Image attached]";
pub(crate) const IMAGE_UNAVAILABLE: &str = "[Image unavailable in this channel; open Phoenix to view it.]";

/// UTF-16 limits are conservative for Telegram and match Discord's string
/// accounting. Keep visible characters intact and prefer nearby line/word
/// boundaries. A pathological grapheme larger than a whole message is split
/// only at scalar boundaries, so arbitrary input still makes bounded progress.
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    assert!(limit >= 2);
    let mut result = Vec::new();
    let mut rest = text;
    'chunks: while !rest.is_empty() {
        let mut units = 0;
        let mut end = 0;
        let mut newline = None;
        let mut whitespace = None;
        for (index, grapheme) in rest.grapheme_indices(true) {
            let length = grapheme.encode_utf16().count();
            if end == 0 && length > limit {
                let mut start = 0;
                let mut count = 0;
                for (offset, ch) in grapheme.char_indices() {
                    if count + ch.len_utf16() > limit {
                        result.push(grapheme[start..offset].to_owned());
                        start = offset;
                        count = 0;
                    }
                    count += ch.len_utf16();
                }
                result.push(grapheme[start..].to_owned());
                rest = &rest[grapheme.len()..];
                continue 'chunks;
            }
            if units + length > limit {
                break;
            }
            units += length;
            end = index + grapheme.len();
            if units >= limit / 2 && grapheme.contains('\n') {
                newline = Some(end);
            }
            if units >= limit / 2 && grapheme.chars().all(char::is_whitespace) {
                whitespace = Some(end);
            }
        }
        if end < rest.len() {
            if let Some(boundary) = newline.or(whitespace) {
                end = boundary;
            }
        }
        result.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    result
}

/// Images must be explicitly embedded in the final answer. Never attach the
/// latest tool screenshot automatically: it may contain a login or private UI.
/// Stage their bytes under the channel's private outbox before delivery.
pub fn project_reply(
    config: &ChannelConfig,
    markdown: &str,
    spool: &Path,
) -> Result<Vec<Outbound>> {
    ensure!(
        markdown.len() <= MAX_REPLY_BYTES,
        "Reply exceeds the channel limit"
    );
    if let Some(summary) = crate::runtime::user_error::runtime_failure_summary(markdown) {
        return Ok(vec![Outbound::Text { text:summary.into() }]);
    }
    let root = config.workspace.canonicalize()?;
    let mut text = markdown.to_owned();
    let mut images = Vec::new();
    let mut edits = Vec::new();
    let mut image_start = None;
    for (event, range) in pulldown_cmark::Parser::new(markdown).into_offset_iter() {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) => {
                image_start = Some((range.start, dest_url.to_string()));
            }
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Image) => {
                let Some((start, destination)) = image_start.take() else {
                    continue;
                };
                // External images remain links. Do not fetch arbitrary URLs.
                if destination.starts_with("https://") || destination.starts_with("http://") {
                    continue;
                }
                let result = local_image_path(&root, &destination)
                    .and_then(|candidate| stage_image(&root, &candidate, spool));
                match result {
                    Ok(image) if images.len() < MAX_REPLY_IMAGES => {
                        images.push(image);
                        edits.push((start..range.end, IMAGE_ATTACHED));
                    }
                    _ => edits.push((
                        start..range.end,
                        IMAGE_UNAVAILABLE,
                    )),
                }
            }
            _ => {}
        }
    }
    for (range, replacement) in edits.into_iter().rev() {
        text.replace_range(range, replacement);
    }
    let limit = if config.platform == Platform::Telegram {
        4096
    } else {
        MIN_TEXT_UNITS
    };
    let mut output: Vec<_> = split_text(&text, limit)
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .map(|text| Outbound::Text { text })
        .collect();
    output.extend(images);
    if output.is_empty() {
        output.push(Outbound::Text {
            text: "The agent finished without a reply. Open Phoenix to review the run.".into(),
        });
    }
    Ok(output)
}
fn local_image_path(root: &Path, destination: &str) -> Result<PathBuf> {
    let destination = destination.trim_matches(['<', '>']);
    let candidate = if destination.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:")) {
        // to_file_path decodes once and rejects non-local authorities. Never
        // feed its result through the percent decoder a second time.
        url::Url::parse(destination)?.to_file_path()
            .map_err(|_| anyhow::anyhow!("Image URL is not a local file"))?
    } else {
        // Markdown destinations may encode spaces, Unicode, or literal '%'.
        // This decoder preserves '+'; form decoding would change filenames.
        PathBuf::from(urlencoding::decode(destination)?.as_ref())
    };
    Ok(if candidate.is_absolute() { candidate } else { root.join(candidate) })
}
fn stage_image(root: &Path, candidate: &Path, spool: &Path) -> Result<Outbound> {
    let path = candidate.canonicalize().context("Image not found")?;
    ensure!(
        path.starts_with(root),
        "Image is outside the channel workspace"
    );
    // Bound reads, reject links, and bind the loaded bytes to the staged file.
    let raw = crate::config::private_io::read_private_file_limited(&path, 8 * 1024 * 1024)?
        .context("Image missing")?;
    ensure!(
        !raw.is_empty() && raw.len() <= 8 * 1024 * 1024,
        "Image exceeds upload limit"
    );
    let format = image::guess_format(&raw)?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        _ => bail!("Unsupported image format"),
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&raw), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().context("Invalid image")?;
    let name = format!(
        "image-{}.{}",
        &format!("{:x}", Sha256::digest(&raw))[..16],
        extension
    );
    let staged = spool.join(&name);
    crate::config::private_io::atomic_write_private(&staged, &raw)?;
    Ok(Outbound::Image { path: staged, name })
}

#[cfg(test)]
mod tests;
