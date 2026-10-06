//! Non-blocking coworker messages.
//!
//! `talk` is the work-handoff primitive. `message_agent` is deliberately a
//! different contract: it delivers context, a correction, or a notice and the
//! sender keeps working. The receiver still gets a normal durable inbox turn.

use serde::{Deserialize, Serialize};

pub const MESSAGE_PRIORITY_MARKER: &str = "<!-- phoenix-message-priority:";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl MessagePriority {
    pub fn is_interrupting(self) -> bool {
        matches!(self, Self::High | Self::Urgent)
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
            Self::Urgent => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageAttachment {
    /// Workspace path, file URL, or already-authorized remote URL.
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageAgentInput {
    /// Individual agent ids/names. May be empty when `group` is supplied.
    #[serde(default)]
    pub to: Vec<String>,
    /// Optional company group id/name; active members are expanded in their
    /// durable directory sort order and each receives an individual receipt.
    #[serde(default)]
    pub group: Option<String>,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub priority: MessagePriority,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}

impl MessageAgentInput {
    /// Transport attachments inside the already-durable message body. The
    /// HTML comment is machine-readable after gateway recovery; Markdown
    /// keeps images/files useful to both models and the conversation UI.
    pub fn transport_body(&self) -> String {
        let mut body = format!(
            "<!-- phoenix-message-priority:{} -->\n{}",
            self.priority.label(),
            self.body.trim()
        );
        if !self.attachments.is_empty() {
            body.push_str("\n\nAttachments:\n");
            for attachment in &self.attachments {
                let name = if attachment.name.trim().is_empty() {
                    attachment.uri.as_str()
                } else {
                    attachment.name.as_str()
                };
                let image = attachment.media_type.starts_with("image/");
                body.push_str(&format!(
                    "- {}[{}]({}){}\n",
                    if image { "!" } else { "" },
                    name.replace([']', '\n'], " "),
                    attachment.uri.replace([')', '\n'], ""),
                    if attachment.media_type.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", attachment.media_type.trim())
                    }
                ));
            }
        }
        body
    }
}

pub fn priority_from_transport_body(body: &str) -> MessagePriority {
    let Some(rest) = body.trim_start().strip_prefix(MESSAGE_PRIORITY_MARKER) else {
        return MessagePriority::Normal;
    };
    match rest.split_once(" -->").map(|(value, _)| value.trim()) {
        Some("low") => MessagePriority::Low,
        Some("high") => MessagePriority::High,
        Some("urgent") => MessagePriority::Urgent,
        _ => MessagePriority::Normal,
    }
}

/// True only for the dedicated context-message transport. Keeping this
/// marker in the durable body lets recovery preserve the distinction without
/// adding a second, migration-sensitive session message variant.
pub fn is_transport_message(body: &str) -> bool {
    body.trim_start().starts_with(MESSAGE_PRIORITY_MARKER)
}

/// Remove Phoenix's routing header while retaining the authored message and
/// attachment Markdown for presentation or model context.
pub fn visible_transport_body(body: &str) -> &str {
    let trimmed = body.trim_start();
    if !trimmed.starts_with(MESSAGE_PRIORITY_MARKER) {
        return body;
    }
    trimmed
        .split_once(" -->")
        .map(|(_, visible)| visible.trim_start_matches(['\r', '\n']))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachments_and_priority_survive_the_durable_body() {
        let input = MessageAgentInput {
            to: vec!["coder".into()],
            group: None,
            subject: "Look here".into(),
            body: "Use the annotated reference.".into(),
            priority: MessagePriority::Urgent,
            attachments: vec![MessageAttachment {
                uri: "/workspace/reference.png".into(),
                name: "reference".into(),
                media_type: "image/png".into(),
            }],
        };
        let body = input.transport_body();
        assert_eq!(priority_from_transport_body(&body), MessagePriority::Urgent);
        assert!(is_transport_message(&body));
        assert!(visible_transport_body(&body).starts_with("Use the annotated reference."));
        assert!(body.contains("![reference](/workspace/reference.png)"));
    }
}
