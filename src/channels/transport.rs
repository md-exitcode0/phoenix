//! HTTP adapters use official bot APIs. Errors never include credential URLs,
//! response bodies, or authorization headers.
use super::*;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::time::Duration;
use zeroize::Zeroizing;

pub struct ChannelApi {
    client: Client,
    token: Zeroizing<String>,
    pub config: ChannelConfig,
    #[cfg(test)]
    test_origin: Option<String>,
}
/// One cancellable receive request, independent of final-reply delivery. The
/// worker admits and saves a result before starting another request/offset.
#[derive(Default)]
pub struct ChannelPoll {
    pending: Option<tokio::task::JoinHandle<Result<PollBatch>>>,
}
impl ChannelPoll {
    pub fn start(&mut self, api: std::sync::Arc<ChannelApi>, cursor: Option<String>) -> bool {
        if self.pending.is_some() { return false; }
        self.pending = Some(tokio::spawn(async move { api.poll(cursor.as_deref()).await }));
        true
    }
    pub async fn take_ready(&mut self) -> Option<Result<PollBatch>> {
        if !self.pending.as_ref().is_some_and(|task| task.is_finished()) { return None; }
        Some(match self.pending.take().unwrap().await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("Channel receiver stopped; polling will reconnect")),
        })
    }
}
impl Drop for ChannelPoll {
    fn drop(&mut self) {
        if let Some(task) = self.pending.take() { task.abort(); }
    }
}

/// One outgoing request at a time. Uploads do not block inbox admission or
/// question delivery; the owner persists `sending` before calling start.
#[derive(Default)]
pub struct ChannelSend {
    pending: Option<(String, tokio::task::JoinHandle<std::result::Result<String, SendFailure>>)>,
}
impl ChannelSend {
    pub fn start(&mut self, api: std::sync::Arc<ChannelApi>, id: String, item: Outbound) -> bool {
        if self.pending.is_some() { return false; }
        let nonce = id.clone();
        self.pending = Some((id, tokio::spawn(async move { api.send(&item, &nonce).await })));
        true
    }
    pub async fn take_ready(&mut self) -> Option<(String, std::result::Result<String, SendFailure>)> {
        if !self.pending.as_ref().is_some_and(|(_,task)| task.is_finished()) { return None; }
        let (id,task) = self.pending.take().unwrap();
        Some((id,task.await.unwrap_or(Err(SendFailure::Uncertain))))
    }
    /// Keep a completed acknowledgement when disconnect races the response.
    /// Otherwise cancel the HTTP future and retain an uncertain delivery.
    pub async fn finish_or_cancel(&mut self) -> Option<(String, std::result::Result<String, SendFailure>)> {
        let (id,task) = self.pending.take()?;
        task.abort();
        Some((id,task.await.unwrap_or(Err(SendFailure::Uncertain))))
    }
}
impl Drop for ChannelSend {
    fn drop(&mut self) { if let Some((_,task)) = self.pending.take() { task.abort(); } }
}

pub struct PollBatch {
    pub cursor: Option<String>,
    pub messages: Vec<InboundMessage>,
}
#[derive(Debug)]
pub enum SendFailure {
    RetryAfter(Duration),
    Rejected(String),
    Uncertain,
}

/// Markdown → Telegram's HTML subset (b, i, s, code, pre, a, blockquote), so
/// replies arrive formatted instead of showing `**` and `[text](url)`.
/// Returns None when there is nothing to format or it would not fit one message.
pub(crate) fn telegram_html(markdown: &str) -> Option<String> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let esc = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    for event in Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),
            Event::Start(Tag::Link { dest_url, .. }) => out.push_str(&format!("<a href=\"{}\">", esc(&dest_url).replace('"', "&quot;"))),
            Event::End(TagEnd::Link) => out.push_str("</a>"),
            Event::Start(Tag::Heading { .. }) => out.push_str("<b>"),
            Event::End(TagEnd::Heading(_)) => out.push_str("</b>\n\n"),
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str(if lists.is_empty() { "\n\n" } else { "\n" }),
            Event::Start(Tag::BlockQuote) => out.push_str("<blockquote>"),
            Event::End(TagEnd::BlockQuote) => { while out.ends_with('\n') { out.pop(); } out.push_str("</blockquote>\n\n"); }
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<pre>"),
            Event::End(TagEnd::CodeBlock) => { while out.ends_with('\n') { out.pop(); } out.push_str("</pre>\n\n"); }
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(TagEnd::List(_)) => { lists.pop(); if lists.is_empty() { out.push('\n'); } }
            Event::Start(Tag::Item) => {
                out.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(number)) => { out.push_str(&format!("{number}. ")); *number += 1; }
                    _ => out.push_str("• "),
                }
            }
            Event::End(TagEnd::Item) => { if !out.ends_with('\n') { out.push('\n'); } }
            Event::Code(code) => out.push_str(&format!("<code>{}</code>", esc(&code))),
            Event::Text(text) => out.push_str(&esc(&text)),
            Event::Html(html) | Event::InlineHtml(html) => out.push_str(&esc(&html)),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("──────\n\n"),
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }
    // Escaped text never contains '<', so no tag means nothing to format:
    // plain text is then sent exactly as written.
    let out = out.trim_end().to_string();
    (out.contains('<') && out.chars().count() <= 4096).then_some(out)
}

impl ChannelApi {
    pub fn new(config: ChannelConfig, token: String) -> Result<Self> {
        config.validate()?;
        ensure!(
            !token.trim().is_empty() && !token.chars().any(char::is_whitespace),
            "Bot token is empty or malformed"
        );
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-' | b'.')),
            "Bot token contains invalid characters"
        );
        if config.platform == Platform::Telegram {
            ensure!(
                token.split_once(':').is_some_and(|(id, key)| !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
                    && !key.is_empty()),
                "Telegram bot token must contain its bot ID and key"
            );
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(35))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            token: Zeroizing::new(token),
            config,
            #[cfg(test)]
            test_origin: None,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        #[cfg(test)]
        if let Some(origin) = &self.test_origin {
            return self.client.request(method, format!("{origin}{path}"));
        }
        match self.config.platform {
            Platform::Telegram => self.client.request(
                method,
                format!("https://api.telegram.org/bot{}{path}", self.token.as_str()),
            ),
            Platform::Discord => self
                .client
                .request(method, format!("https://discord.com/api/v10{path}"))
                .header("Authorization", format!("Bot {}", self.token.as_str())),
        }
    }
    pub async fn verify(&self) -> Result<()> {
        let path = if self.config.platform == Platform::Telegram {
            "/getMe"
        } else {
            "/users/@me"
        };
        let response = self
            .request(reqwest::Method::GET, path)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Could not reach the channel provider"))?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "Channel sign-in failed (HTTP {})",
            status.as_u16()
        );
        let body = bounded_json(response).await?;
        if self.config.platform == Platform::Telegram {
            ensure!(body["ok"] == true, "Telegram rejected the bot token");
        } else {
            ensure!(body["bot"] == true, "Use a Discord bot token");
        }
        Ok(())
    }
/// Shows "typing…" in the chat. Best effort: Telegram keeps it about 5s,
    /// Discord about 10s, so the worker repeats it while a turn runs.
    pub async fn typing(&self) {
        let request = match self.config.platform {
            Platform::Telegram => self.request(reqwest::Method::POST, "/sendChatAction")
                .json(&json!({"chat_id":self.config.conversation_id,"action":"typing"})),
            Platform::Discord => self.request(reqwest::Method::POST, &format!("/channels/{}/typing", self.config.conversation_id)),
        };
        let _ = tokio::time::timeout(Duration::from_secs(8), request.send()).await;
    }
    pub async fn poll(&self, cursor: Option<&str>) -> Result<PollBatch> {
        let request = match self.config.platform {
            Platform::Telegram => {
                let offset = cursor.map(str::parse::<i64>).transpose()?.unwrap_or(-1);
                self.request(reqwest::Method::POST, "/getUpdates").json(
                    &json!({"offset":offset,"timeout":if cursor.is_some() {20} else {0},"limit":100,"allowed_updates":["message","callback_query"]}),
                )
            }
            Platform::Discord => {
                let request = self.request(
                    reqwest::Method::GET,
                    &format!("/channels/{}/messages", self.config.conversation_id),
                );
                if let Some(after) = cursor {
                    request.query(&[("after", after), ("limit", "100")])
                } else {
                    request.query(&[("limit", "1")])
                }
            }
        };
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Channel polling connection failed"))?;
        ensure!(
            response.status().is_success(),
            "Channel polling failed (HTTP {})",
            response.status().as_u16()
        );
        let body = bounded_json(response).await?;
        let rows = match self.config.platform {
            Platform::Telegram => {
                ensure!(
                    body["ok"] == true,
                    "Telegram rejected polling; check for another bot receiver or webhook"
                );
                body["result"].as_array()
            }
            Platform::Discord => body.as_array(),
        }
        .context("Channel returned an invalid message list")?;
        let mut messages = Vec::new();
        let mut next = Some(cursor.unwrap_or("0").to_owned());
        let mut ordered = rows.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|row| message_sequence(self.config.platform, row).unwrap_or_default());
        for row in ordered {
            let seq = message_sequence(self.config.platform, row)
                .context("Channel message omitted its cursor")?;
            next = Some(if self.config.platform == Platform::Telegram {
                seq.checked_add(1)
                    .context("Invalid Telegram cursor")?
                    .to_string()
            } else {
                seq.to_string()
            });
            // The first poll establishes a boundary without executing historical
            // messages. Telegram requests only its most recent update.
            if cursor.is_none() {
                continue;
            }
            if let Some(tap) = row.get("callback_query").and_then(|tap| tap.get("id")).and_then(|id| id.as_str()) {
                // Stop the button's spinner; the answer itself arrives as a message.
                let _ = tokio::time::timeout(Duration::from_secs(5), self.request(reqwest::Method::POST, "/answerCallbackQuery").json(&json!({"callback_query_id":tap,"text":"Answer sent"})).send()).await;
            }
            if let Some(message) = inbound(&self.config, row) {
                messages.push(message);
            } else if let Some(message) = self.voice_message(row).await {
                messages.push(message);
            }
        }
        Ok(PollBatch {
            cursor: next,
            messages,
        })
    }
/// A voice note, audio file or round video from an allowed sender becomes
    /// a text message: its transcript, or a short note saying why not. The
    /// sender is authorized (by `inbound` on a placeholder text) BEFORE any
    /// audio is downloaded or sent for transcription.
    async fn voice_message(&self, row: &Value) -> Option<InboundMessage> {
        let (holder, audio) = match self.config.platform {
            Platform::Telegram => {
                let message = row.get("message")?;
                let audio = ["voice", "audio", "video_note", "video"].iter().find_map(|kind| message.get(*kind).map(|media| (*kind, media)))?;
                ("message", audio)
            }
            Platform::Discord => {
                let attachment = row["attachments"].as_array()?.iter().find(|a| {
                    a["content_type"].as_str().is_some_and(|t| t.starts_with("audio/") || t.starts_with("video/"))
                })?;
                ("", ("attachment", attachment))
            }
        };
        let mut probe = row.clone();
        let target = if holder.is_empty() { &mut probe } else { probe.get_mut(holder)? };
        let caption = target.get("caption").and_then(|v| v.as_str()).map(str::to_string);
        target[if self.config.platform == Platform::Telegram { "text" } else { "content" }] = json!("voice");
        let mut message = inbound(&self.config, &probe)?;
        let transcript = self.transcribe(audio.0, audio.1).await;
        let label = if audio.0 == "voice" || audio.0 == "attachment" { "Voice message" } else { "Audio" };
        message.text = match transcript {
            Ok(text) => format!("🎤 {label} (transcribed): {text}"),
            Err(error) => format!("🎤 {label} received, but it could not be transcribed: {error}"),
        };
        if let Some(caption) = caption.filter(|c| !c.trim().is_empty()) {
            message.text = format!("{}\n{}", caption.trim(), message.text);
        }
        Some(message)
    }
    async fn transcribe(&self, kind: &str, media: &Value) -> Result<String> {
        let size = media["file_size"].as_u64().or_else(|| media["size"].as_u64()).unwrap_or(0);
        ensure!(size as usize <= crate::voice::MAX_AUDIO_BYTES, "the recording is larger than 25 MB");
        let (url, name) = match self.config.platform {
            Platform::Telegram => {
                // Bot API downloads are capped at 20 MB.
                ensure!(size <= 20 * 1024 * 1024, "Telegram lets bots download files up to 20 MB");
                let id = media["file_id"].as_str().context("the recording has no file id")?;
                let response = self.request(reqwest::Method::POST, "/getFile").json(&json!({"file_id":id})).send().await
                    .map_err(|_| anyhow::anyhow!("could not reach Telegram"))?;
                let body = bounded_json(response).await?;
                let path = body["result"]["file_path"].as_str().context("Telegram did not return the file")?.to_string();
                // Telegram names voice notes *.oga; speech APIs only accept .ogg.
                let ext = match path.rsplit('.').next().filter(|e| e.len() <= 5) {
                    Some("oga") | Some("opus") => "ogg",
                    None => if kind == "voice" { "ogg" } else { "mp4" },
                    Some(ext) => ext,
                };
                (format!("https://api.telegram.org/file/bot{}/{path}", self.token.as_str()), format!("{kind}.{ext}"))
            }
            Platform::Discord => (
                media["url"].as_str().context("the attachment has no URL")?.to_string(),
                media["filename"].as_str().unwrap_or("voice-message.ogg").to_string(),
            ),
        };
        let response = self.client.get(url).send().await.map_err(|_| anyhow::anyhow!("could not download the recording"))?;
        ensure!(response.status().is_success(), "could not download the recording (HTTP {})", response.status().as_u16());
        let bytes = response.bytes().await.map_err(|_| anyhow::anyhow!("could not download the recording"))?;
        crate::voice::transcribe(bytes.to_vec(), &name).await
    }
    pub async fn send(
        &self,
        item: &Outbound,
        nonce: &str,
    ) -> std::result::Result<String, SendFailure> {
        // Formatted first; if Telegram rejects the markup, the plain text.
        match self.send_once(item, nonce, false).await {
            Err(SendFailure::Rejected(_)) if self.config.platform == Platform::Telegram
                && matches!(item, Outbound::Text { text } if telegram_html(text).is_some()) => self.send_once(item, nonce, true).await,
            result => result,
        }
    }
    async fn send_once(
        &self,
        item: &Outbound,
        nonce: &str,
        plain: bool,
    ) -> std::result::Result<String, SendFailure> {
        let request = match (self.config.platform, item) {
(Platform::Telegram, Outbound::Question { text }) => {
                // Options become tap-to-answer buttons; with none, Telegram
                // opens a reply to the question.
                let options = question_options(text);
                let markup = if options.is_empty() { json!({"force_reply":true}) } else {
                    json!({"inline_keyboard": options.iter().enumerate().map(|(i, option)| vec![json!({"text":option,"callback_data":format!("opt:{i}")})]).collect::<Vec<_>>()})
                };
                self.request(reqwest::Method::POST, "/sendMessage").json(&json!({"chat_id":self.config.conversation_id,"text":text,"reply_markup":markup,"link_preview_options":{"is_disabled":true}}))
            }
            (Platform::Telegram, Outbound::Text { text }) => match telegram_html(text) {
                Some(html) if !plain => self.request(reqwest::Method::POST, "/sendMessage").json(&json!({"chat_id":self.config.conversation_id,"text":html,"parse_mode":"HTML","link_preview_options":{"is_disabled":true}})),
                _ => self.request(reqwest::Method::POST, "/sendMessage").json(&json!({"chat_id":self.config.conversation_id,"text":text,"link_preview_options":{"is_disabled":true}})),
            },
            (Platform::Discord, Outbound::Text { text } | Outbound::Question { text }) => self.request(reqwest::Method::POST, &format!("/channels/{}/messages", self.config.conversation_id)).json(&json!({"content":text,"allowed_mentions":{"parse":[]},"flags":4,"nonce":nonce,"enforce_nonce":true})),
            (platform, Outbound::Image { path, name }) => {
                let bytes = crate::config::private_io::read_private_file_limited(path, 8 * 1024 * 1024).ok().flatten().filter(|b| b.len() <= 8 * 1024 * 1024).ok_or_else(|| SendFailure::Rejected("Saved image unavailable".into()))?;
                let expected_prefix = format!("image-{}.", &format!("{:x}", Sha256::digest(&bytes))[..16]);
                if !name.starts_with(&expected_prefix) { return Err(SendFailure::Rejected("Saved image changed; review it in Phoenix".into())); }
                let mime = if name.ends_with(".png") { "image/png" } else { "image/jpeg" };
                let telegram_photo = if platform == Platform::Telegram {
                    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()
                        .map_err(|_| SendFailure::Rejected("Invalid saved image".into()))?;
                    let (width, height) = reader.into_dimensions()
                        .map_err(|_| SendFailure::Rejected("Invalid saved image".into()))?;
                    telegram_photo_dimensions(width, height)
                } else { false };
                let part = reqwest::multipart::Part::bytes(bytes).file_name(name.clone()).mime_str(mime).map_err(|_| SendFailure::Rejected("Invalid image type".into()))?;
                if platform == Platform::Telegram {
                    let (method, field) = if telegram_photo { ("/sendPhoto", "photo") } else { ("/sendDocument", "document") };
                    self.request(reqwest::Method::POST, method).multipart(reqwest::multipart::Form::new().text("chat_id", self.config.conversation_id.clone()).part(field, part))
                } else {
                    self.request(reqwest::Method::POST, &format!("/channels/{}/messages", self.config.conversation_id)).multipart(reqwest::multipart::Form::new().text("payload_json", json!({"attachments":[{"id":0,"filename":name}],"allowed_mentions":{"parse":[]},"flags":4,"nonce":nonce,"enforce_nonce":true}).to_string()).part("files[0]", part))
                }
            }
        };
        let response = request.send().await.map_err(|_| SendFailure::Uncertain)?;
        let status = response.status();
        let body = bounded_json(response)
            .await
            .map_err(|_| SendFailure::Uncertain)?;
        if status == StatusCode::TOO_MANY_REQUESTS {
            let delay = body["parameters"]["retry_after"]
                .as_f64()
                .or_else(|| body["retry_after"].as_f64())
                .unwrap_or(30.0)
                .clamp(1.0, 86400.0);
            return Err(SendFailure::RetryAfter(Duration::from_secs_f64(delay)));
        }
        if status.is_server_error() {
            return Err(SendFailure::Uncertain);
        }
        if !status.is_success()
            || (self.config.platform == Platform::Telegram && body["ok"] != true)
        {
            return Err(SendFailure::Rejected(format!(
                "Message rejected (HTTP {})",
                status.as_u16()
            )));
        }
        let id = if self.config.platform == Platform::Telegram {
            &body["result"]["message_id"]
        } else {
            &body["id"]
        };
        id.as_str()
            .map(str::to_owned)
            .or_else(|| id.as_i64().map(|v| v.to_string()))
            .ok_or(SendFailure::Uncertain)
    }
}
// Telegram photo limits differ from its file-upload limits. Keep the original
// screenshot bytes and choose the appropriate endpoint before any request;
// do not retry a possibly delivered upload with a second representation.
fn telegram_photo_dimensions(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && u64::from(width) + u64::from(height) <= 10_000
        && u64::from(width.max(height)) <= 20 * u64::from(width.min(height))
}
fn message_sequence(platform: Platform, row: &Value) -> Option<u64> {
    if platform == Platform::Telegram {
        row["update_id"].as_u64()
    } else {
        row["id"].as_str()?.parse().ok()
    }
}
async fn bounded_json(response: reqwest::Response) -> Result<Value> {
    let text = crate::providers::read_response_text(response, 4 * 1024 * 1024, "Channel response")
        .await
        .map_err(|_| anyhow::anyhow!("Channel response could not be read"))?;
    serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("Channel returned an invalid response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telegram_html_formats_markdown() {
        let html = telegram_html("Here is **bold**, a [post](https://x.com/a?b=1&c=2) & `code`.\n\n> quote\n\n- one\n- two").unwrap();
        assert!(html.contains("<b>bold</b>"));
        assert!(html.contains(r#"<a href="https://x.com/a?b=1&amp;c=2">post</a>"#));
        assert!(html.contains("&amp; <code>code</code>"));
        assert!(html.contains("<blockquote>quote</blockquote>"));
        assert!(html.contains("• one\n• two"));
        assert!(telegram_html(&"a".repeat(5000)).is_none());
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn photo_endpoint_obeys_telegram_geometry_limits() {
        for (width, height, expected) in [(5000,5000,true),(5001,5000,false),(2000,100,true),(2001,100,false),(100,2001,false),(0,1,false),(u32::MAX,1,false)] {
            assert_eq!(telegram_photo_dimensions(width,height),expected,"{width}x{height}");
        }
    }

    #[tokio::test]
    async fn tall_screenshot_is_sent_once_as_original_file() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("long-page.png");
        image::RgbImage::from_pixel(10,300,image::Rgb([25,40,70])).save(&source).unwrap();
        let (origin, request) = server(200,json!({"ok":true,"result":{"message_id":42}})).await;
        let api = api(root.path(), Platform::Telegram, origin);
        let output = project_reply(&api.config,"![Full page](long-page.png)",&root.path().join("images")).unwrap();
        let image = output.iter().find(|item| matches!(item,Outbound::Image{..})).unwrap();
        let original = std::fs::read(&source).unwrap();
        std::fs::write(&source,b"later revision").unwrap();
        assert_eq!(api.send(image,"tall-screen").await.unwrap(),"42");
        let raw = request.await.unwrap();
        assert!(raw.starts_with("POST /sendDocument "));
        assert!(raw.contains("name=\"document\""));
        assert!(raw.contains(String::from_utf8_lossy(&original).as_ref()),"frozen original image must survive a later edit");
        assert!(!raw.contains("later revision"));
    }
    async fn server(status: u16, body: Value) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..n]);
                assert!(raw.len() < 1024 * 1024);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&raw[..end]);
                    let size = header
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            let body = body.to_string();
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            String::from_utf8_lossy(&raw).into_owned()
        });
        (address, task)
    }
    fn api(root: &Path, platform: Platform, origin: String) -> ChannelApi {
        let cfg = ChannelConfig {
            id: "fixture".into(),
            name: "Fixture".into(),
            platform,
            conversation_id: "123".into(),
            allowed_user_ids: vec!["456".into()],
            agent_id: "phoenix".into(),
            session_id: "fixture".into(),
            workspace: root.into(),
            token_env: "BOT_TOKEN".into(),
            enabled: true, group_id:None 
        };
        let mut api = ChannelApi::new(
            cfg,
            if platform == Platform::Telegram {
                "123:fixture-secret"
            } else {
                "fixture-secret"
            }
            .into(),
        )
        .unwrap();
        api.test_origin = Some(origin);
        api
    }
    async fn read_request_bytes(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut raw = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            raw.extend_from_slice(&buffer[..count]);
            assert!(raw.len() < 1024 * 1024);
            if let Some(end) = raw.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..end]);
                let size = headers.lines().find_map(|line| line.to_ascii_lowercase()
                    .strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok())).unwrap_or(0);
                if raw.len() >= end + 4 + size { return raw; }
            }
        }
    }
    async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
        String::from_utf8(read_request_bytes(stream).await).unwrap()
    }
    async fn respond(stream: &mut tokio::net::TcpStream, body: Value) {
        let body = body.to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn held_image_upload_allows_incoming_message_admission() {
        let root=tempfile::tempdir().unwrap();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api=std::sync::Arc::new(api(root.path(),Platform::Telegram,format!("http://{}",listener.local_addr().unwrap())));
        image::RgbImage::from_pixel(8,8,image::Rgb([230,210,20])).save(root.path().join("proof.png")).unwrap();
        let image=project_reply(&api.config,"![Proof](proof.png)",&root.path().join("images")).unwrap().into_iter().find(|item| matches!(item,Outbound::Image{..})).unwrap();
        let (seen_tx,seen_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=tokio::sync::oneshot::channel();
        let server=tokio::spawn(async move {
            let (mut upload,_)=listener.accept().await.unwrap();
            let request=read_request_bytes(&mut upload).await;
            assert!(request.starts_with(b"POST /sendPhoto "));
            assert!(request.windows(8).any(|part| part==b"\x89PNG\r\n\x1a\n"));
            seen_tx.send(()).unwrap();
            let (mut poll,_)=listener.accept().await.unwrap();
            assert!(read_request(&mut poll).await.starts_with("POST /getUpdates "));
            respond(&mut poll,json!({"ok":true,"result":[{"update_id":10,"message":{"message_id":11,"from":{"id":456},"chat":{"id":123},"text":"Blue","reply_to_message":{"message_id":7}}}]})).await;
            release_rx.await.unwrap();
            respond(&mut upload,json!({"ok":true,"result":{"message_id":42}})).await;
        });
        let path=root.path().join("state.json");
        let mut state=ChannelState::load(&path,&api.config).unwrap();
        state.outbox.push(Delivery{id:"image-1".into(),item:image.clone(),state:"sending".into(),retry_at:0,remote_id:None});
        state.save(&path).unwrap();
        let mut sender=ChannelSend::default();
        assert!(sender.start(api.clone(),"image-1".into(),image.clone()));
        tokio::time::timeout(Duration::from_secs(2),seen_rx).await.unwrap().unwrap();
        assert!(!sender.start(api.clone(),"second-copy".into(),image));
        let mut receiver=ChannelPoll::default();receiver.start(api.clone(),Some("10".into()));
        let batch=tokio::time::timeout(Duration::from_secs(2),async {loop {if let Some(batch)=receiver.take_ready().await {break batch.unwrap();} tokio::task::yield_now().await;}}).await.unwrap();
        state.admit(batch).unwrap();state.save(&path).unwrap();
        assert_eq!(state.jobs[0].input.text,"Blue");assert_eq!(state.jobs[0].input.reply_to.as_deref(),Some("7"));
        assert_eq!(state.cursor.as_deref(),Some("11"));
        assert!(sender.take_ready().await.is_none(),"Inbox persisted before upload acknowledgement");
        assert_eq!(state.summary()["sending_replies"],1);assert_eq!(state.summary()["sending_images"],1);
        assert_eq!(ChannelState::load(&path,&api.config).unwrap().outbox[0].state,"sending");
        release_tx.send(()).unwrap();
        let (id,result)=tokio::time::timeout(Duration::from_secs(2),async {loop {if let Some(receipt)=sender.take_ready().await {break receipt;}tokio::task::yield_now().await;}}).await.unwrap();
        assert_eq!(id,"image-1");assert_eq!(result.unwrap(),"42");server.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_cancels_outgoing_http_and_marks_its_receipt_uncertain() {
        let root=tempfile::tempdir().unwrap();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api=std::sync::Arc::new(api(root.path(),Platform::Telegram,format!("http://{}",listener.local_addr().unwrap())));
        let (seen_tx,seen_rx)=tokio::sync::oneshot::channel();
        let server=tokio::spawn(async move {
            let (mut stream,_)=listener.accept().await.unwrap();read_request(&mut stream).await;seen_tx.send(()).unwrap();
            let mut byte=[0];assert_eq!(stream.read(&mut byte).await.unwrap(),0);
        });
        let mut sender=ChannelSend::default();sender.start(api,"pending".into(),Outbound::Text{text:"Final".into()});
        tokio::time::timeout(Duration::from_secs(2),seen_rx).await.unwrap().unwrap();
        let (id,result)=sender.finish_or_cancel().await.unwrap();
        assert_eq!(id,"pending");assert!(matches!(result,Err(SendFailure::Uncertain)));
        assert!(sender.take_ready().await.is_none());
        tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn disconnect_preserves_an_already_completed_send_acknowledgement() {
        let root=tempfile::tempdir().unwrap();
        let (origin,request)=server(200,json!({"ok":true,"result":{"message_id":73}})).await;
        let api=std::sync::Arc::new(api(root.path(),Platform::Telegram,origin));
        let mut sender=ChannelSend::default();sender.start(api,"finished".into(),Outbound::Text{text:"Final".into()});
        request.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2),async {while !sender.pending.as_ref().unwrap().1.is_finished() {tokio::task::yield_now().await;}}).await.unwrap();
        let (id,result)=sender.finish_or_cancel().await.unwrap();
        assert_eq!(id,"finished");assert_eq!(result.unwrap(),"73");
    }

    #[tokio::test]
    async fn channel_poll_does_not_delay_replies_or_advance_an_inflight_cursor() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = std::sync::Arc::new(api(root.path(), Platform::Telegram, format!("http://{}",listener.local_addr().unwrap())));
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut poll, _) = listener.accept().await.unwrap();
            let request = read_request(&mut poll).await;
            assert!(request.starts_with("POST /getUpdates "));
            assert!(request.contains("\"offset\":10"));
            seen_tx.send(()).unwrap();
            let (mut reply, _) = listener.accept().await.unwrap();
            let request = read_request(&mut reply).await;
            assert!(request.starts_with("POST /sendMessage "));
            respond(&mut reply, json!({"ok":true,"result":{"message_id":42}})).await;
            release_rx.await.unwrap();
            respond(&mut poll, json!({"ok":true,"result":[{"update_id":10,"message":{"message_id":5,"from":{"id":456},"chat":{"id":123},"text":"next"}}]})).await;
        });
        let mut receiver = ChannelPoll::default();
        assert!(receiver.start(api.clone(),Some("10".into())));
        tokio::time::timeout(Duration::from_secs(2),seen_rx).await.unwrap().unwrap();
        assert!(!receiver.start(api.clone(),Some("99".into())), "only one receive request may own the cursor");
        assert!(receiver.take_ready().await.is_none());
        let sent = tokio::time::timeout(Duration::from_secs(2), api.send(&Outbound::Text{text:"Finished".into()},"finished-1")).await.unwrap().unwrap();
        assert_eq!(sent,"42");
        assert!(receiver.take_ready().await.is_none(), "reply arrived before the receive request completed");
        release_tx.send(()).unwrap();
        let batch = tokio::time::timeout(Duration::from_secs(2), async {
            loop { if let Some(result) = receiver.take_ready().await { break result.unwrap(); } tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(batch.cursor.as_deref(),Some("11"));
        assert_eq!(batch.messages.len(),1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn channel_poll_disconnect_cancels_the_pending_request() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = std::sync::Arc::new(api(root.path(), Platform::Telegram, format!("http://{}",listener.local_addr().unwrap())));
        let (seen_tx,seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            seen_tx.send(()).unwrap();
            let mut byte=[0];
            assert_eq!(stream.read(&mut byte).await.unwrap(),0);
        });
        let mut receiver=ChannelPoll::default();
        receiver.start(api,Some("10".into()));
        tokio::time::timeout(Duration::from_secs(2),seen_rx).await.unwrap().unwrap();
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn question_messages_use_native_reply_ui_without_pinging_discord_users() {
        let root=tempfile::tempdir().unwrap();
        for platform in [Platform::Telegram,Platform::Discord] {
            let body=if platform==Platform::Telegram { json!({"ok":true,"result":{"message_id":88}}) } else {json!({"id":"88"})};
            let (origin,request)=server(200,body).await;
            let api=api(root.path(),platform,origin);
            assert_eq!(api.send(&Outbound::Question{text:"Which option, @everyone? Reply to this message.".into()},"question-nonce").await.unwrap(),"88");
            let raw=request.await.unwrap();let payload:Value=serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            if platform==Platform::Telegram { assert_eq!(payload["reply_markup"]["force_reply"],true); }
            else { assert_eq!(payload["allowed_mentions"]["parse"],json!([])); assert_eq!(payload["enforce_nonce"],true); }
            assert!(!raw.contains("fixture-secret"));
        }
    }
    #[tokio::test]
    async fn long_final_chunks_reopen_and_send_exact_text_in_order() {
        use unicode_segmentation::UnicodeSegmentation;
        for platform in [Platform::Telegram, Platform::Discord] {
            let root = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let api = api(root.path(), platform, format!("http://{}", listener.local_addr().unwrap()));
            let limit = if platform == Platform::Telegram { 4096 } else { 2000 };
            let text = format!("{}👩🏽‍💻\n{}", "x".repeat(limit - 1), "Café e\u{301} 🇨🇦 useful result. ".repeat(240));
            let projected = project_reply(&api.config, &text, &root.path().join("outbox")).unwrap();
            let state_path = root.path().join("state.json");
            let mut state = ChannelState::load(&state_path, &api.config).unwrap();
            state.admit(PollBatch { cursor:Some("1".into()), messages:vec![InboundMessage {
                id:"1".into(), user_id:"456".into(), text:"Send the report".into(), reply_to:None, sender:None 
            }] }).unwrap();
            let turn = state.jobs[0].turn_id.clone();
            state.complete(&turn, projected.clone()).unwrap();
            state.save(&state_path).unwrap();
            let mut state = ChannelState::load(&state_path, &api.config).unwrap();
            state.recover();
            state.complete(&turn, projected).unwrap();
            let count = state.outbox.len();
            assert!(count > 1);
            let server = tokio::spawn(async move {
                let mut received = Vec::new();
                for index in 0..count {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let request = read_request(&mut stream).await;
                    let payload: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    let field = if platform == Platform::Telegram { "text" } else { "content" };
                    received.push(payload[field].as_str().unwrap().to_string());
                    respond(&mut stream, if platform == Platform::Telegram {
                        json!({"ok":true,"result":{"message_id":index+1}})
                    } else { json!({"id":(index+1).to_string()}) }).await;
                }
                received
            });
            for (index, delivery) in state.outbox.iter().enumerate() {
                assert_eq!(api.send(&delivery.item, &delivery.id).await.unwrap(), (index+1).to_string());
            }
            let received = server.await.unwrap();
            assert_eq!(received.concat(), text);
            let boundaries = text.grapheme_indices(true).map(|(i,_)|i).chain([text.len()]).collect::<std::collections::HashSet<_>>();
            let mut offset = 0;
            for piece in received {
                assert!(piece.encode_utf16().count() <= limit);
                offset += piece.len();
                assert!(boundaries.contains(&offset));
            }
        }
    }

    #[tokio::test]
    async fn channel_text_requests_disable_mentions_and_preserve_limits() {
        let root = tempfile::tempdir().unwrap();
        for platform in [Platform::Telegram, Platform::Discord] {
            let body = if platform == Platform::Telegram {
                json!({"ok":true,"result":{"message_id":123}})
            } else {
                json!({"id":"123"})
            };
            let (origin, request) = server(200, body).await;
            let api = api(root.path(), platform, origin);
            assert_eq!(
                api.send(
                    &Outbound::Text {
                        text: "Hello @everyone".into()
                    },
                    "nonce1"
                )
                .await
                .unwrap(),
                "123"
            );
            let raw = request.await.unwrap();
            assert!(!raw.contains("fixture-secret"));
            let payload: Value =
                serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            if platform == Platform::Discord {
                assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
                assert_eq!(payload["enforce_nonce"], true);
            } else {
                assert_eq!(payload["chat_id"], "123");
                assert!(payload.get("parse_mode").is_none());
            }
        }
    }
    #[tokio::test]
    async fn polling_bootstrap_skips_history_but_accepts_new_messages() {
        let root = tempfile::tempdir().unwrap();
        for platform in [Platform::Telegram, Platform::Discord] {
            let body = if platform == Platform::Telegram {
                json!({"ok":true,"result":[{"update_id":9,"message":{"message_id":1,"from":{"id":456},"chat":{"id":123},"text":"hello"}}]})
            } else {
                json!([{"id":"9","channel_id":"123","author":{"id":"456"},"content":"hello"}])
            };
            for cursor in [None, Some("8")] {
                let (origin, request) = server(200, body.clone()).await;
                let api = api(root.path(), platform, origin);
                let batch = api.poll(cursor).await.unwrap();
                assert_eq!(batch.messages.len(), usize::from(cursor.is_some()));
                assert_eq!(
                    batch.cursor.as_deref(),
                    Some(if platform == Platform::Telegram {
                        "10"
                    } else {
                        "9"
                    })
                );
                let request = request.await.unwrap();
                if platform == Platform::Telegram {
                    let payload: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                    assert_eq!(payload["timeout"], if cursor.is_some() {20} else {0});
                }
            }
        }
    }
    #[tokio::test]
    async fn upload_uses_actual_image_bytes_and_rate_limits_are_retryable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("final screenshot.png");
        image::RgbImage::from_pixel(4,4,image::Rgb([30,140,200])).save(&path).unwrap();
        let image_bytes = std::fs::read(&path).unwrap();
        for platform in [Platform::Telegram, Platform::Discord] {
            let body = if platform == Platform::Telegram {
                json!({"ok":true,"result":{"message_id":2}})
            } else {
                json!({"id":"2"})
            };
            let (origin, request) = server(200, body).await;
            let api = api(root.path(), platform, origin);
            let output = project_reply(&api.config, "Finished. ![Proof](final%20screenshot.png)", &root.path().join("outbox")).unwrap();
            let image = output.iter().find(|item| matches!(item, Outbound::Image { .. })).unwrap();
            api.send(
                image,
                "image-nonce",
            )
            .await
            .unwrap();
            let raw = request.await.unwrap();
            assert!(raw.contains("multipart/form-data"));
            assert!(raw.contains(String::from_utf8_lossy(&image_bytes).as_ref()));
            assert!(raw.contains(if platform == Platform::Telegram {
                "name=\"photo\""
            } else {
                "name=\"files[0]\""
            }));
        }
        let (origin, request) = server(429, json!({"retry_after":2.5})).await;
        let api = api(root.path(), Platform::Discord, origin);
        assert!(
            matches!(api.send(&Outbound::Text{text:"reply".into()},"rate").await,Err(SendFailure::RetryAfter(d)) if d.as_secs_f64()==2.5)
        );
        request.await.unwrap();
    }
}
