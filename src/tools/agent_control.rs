//! Live controls for detached coworker jobs.

use serde::{Deserialize, Serialize};

use super::{MessageAttachment, MessagePriority};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentControlAction {
    List,
    Status,
    Message,
    Stop,
    Resume,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentControlInput {
    pub action: AgentControlAction,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub priority: MessagePriority,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}
