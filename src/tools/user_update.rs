//! Explicit, infrequent user-facing updates, separate from model narration.
use anyhow::{bail, Result};
use serde::Deserialize;
use super::ToolOutput;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserUpdateInput { pub message: String }

pub fn execute(input: UserUpdateInput) -> Result<ToolOutput> {
    let message = input.message.trim();
    if message.is_empty() || message.chars().count() > 1200 {
        bail!("user_update needs a non-empty message of at most 1200 characters");
    }
    Ok(ToolOutput {
        summary: message.to_string(),
        // format_tool_output joins summary and content. Returning the message
        // in both made the published update contain the same prose twice.
        content: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_preserves_content_and_is_available_in_talk_mode() {
        let message = "The draft is ready.\nOne detail still needs checking.";
        let output = execute(UserUpdateInput { message: message.into() }).unwrap();
        assert_eq!(output.summary, message);
        assert!(output.content.is_empty());
        assert_eq!(super::super::format_tool_output("user_update", output), message);
        assert_eq!(crate::tools::required_permission_for_tool("user_update"), crate::tools::PermissionMode::Talk);
        assert!(crate::tools::universal_agent_tool_names().iter().any(|name| name == "user_update"));
        assert!(crate::tools::tool_definitions_for_agent(&["user_update".into()]).iter().any(|tool| tool.name == "user_update"));
        assert!(execute(UserUpdateInput { message: " ".into() }).is_err());
        assert!(execute(UserUpdateInput { message: "x".repeat(1201) }).is_err());
    }
}
