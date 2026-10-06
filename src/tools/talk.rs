//! talk tool payloads and delivery metadata

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TalkInput {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub mode: u8,
}

impl TalkInput {
    pub fn reply_expected(&self) -> bool {
        self.mode == 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TalkReplyStatus {
    Expected,
    NotExpected,
    Pending,
}

impl TalkReplyStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Expected => "expected",
            Self::NotExpected => "not_expected",
            Self::Pending => "pending",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TalkResult {
    pub delivered_to: String,
    pub target_session_id: String,
    pub reply_status: TalkReplyStatus,
    pub executed_target: bool,
}
