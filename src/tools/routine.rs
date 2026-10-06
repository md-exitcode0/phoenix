//! Agent-facing access to human-taught reusable workflows.

use anyhow::Result;

use super::ToolOutput;
use crate::runtime::workflow_teaching::RoutineToolInput;

pub fn execute(
    input: RoutineToolInput,
    actor: Option<&str>,
    group_id: Option<&str>,
) -> Result<ToolOutput> {
    let actor = actor.unwrap_or("phoenix");
    let value = crate::runtime::workflow_teaching::execute_tool(input, actor, group_id)?;
    Ok(ToolOutput {
        summary: "taught workflow library updated".to_string(),
        content: serde_json::to_string_pretty(&value)?,
    })
}
