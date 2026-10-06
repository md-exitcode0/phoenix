//! Callable, resumable surface for the evidence-gated reverse-skill runtime.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::runtime::reverse_skill::{
    CanaryInput, ReverseSkillPipeline, ReverseSkillRequest, ReviewerReceipt,
};

use super::ToolOutput;

#[derive(Debug, Deserialize)]
pub struct ReverseSkillInput {
    pub action: String,
    #[serde(default)]
    pub candidate_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub run_ids: Vec<String>,
    #[serde(default)]
    pub explicit_request: bool,
    #[serde(default)]
    pub reviewers: Vec<ReviewerReceipt>,
    #[serde(default)]
    pub canary: Option<CanaryInput>,
}

pub async fn execute(
    workspace: &Path,
    actor: &str,
    input: ReverseSkillInput,
) -> Result<ToolOutput> {
    let store = crate::runtime::company::global()?;
    let pipeline =
        ReverseSkillPipeline::new(workspace, crate::config::phoenix_home().join("runs"), store)?;
    let action = input.action.trim().to_ascii_lowercase();
    let state = match action.as_str() {
        "observe" => pipeline.observe(ReverseSkillRequest {
            candidate_id: input.candidate_id,
            name: required(input.name, "name")?,
            description: required(input.description, "description")?,
            run_ids: input.run_ids,
            explicit_request: input.explicit_request,
            producer: actor.to_string(),
        })?,
        "status" => pipeline
            .load(&required(input.candidate_id, "candidate_id")?)?
            .context("reverse-skill candidate does not exist")?,
        "propose" | "review" | "canary" | "publish" | "rollback" => {
            let candidate_id = required(input.candidate_id, "candidate_id")?;
            let mut state = pipeline
                .load(&candidate_id)?
                .context("reverse-skill candidate does not exist")?;
            match action.as_str() {
                "propose" => pipeline.propose(&mut state)?,
                "review" => pipeline.review(&mut state, input.reviewers)?,
                "canary" => pipeline.canary(
                    &mut state,
                    input.canary.context("canary input is required")?,
                )?,
                "publish" => {
                    pipeline.publish(&mut state).await?;
                }
                "rollback" => pipeline.rollback(&mut state).await?,
                _ => unreachable!(),
            }
            state
        }
        other => bail!(
            "unknown reverse_skill action `{other}`; expected observe, status, propose, review, canary, publish, or rollback"
        ),
    };

    // The full body and artifact lists remain in the private durable state;
    // tool transcripts get a bounded, secret-safe projection.
    let projection = serde_json::json!({
        "candidate_id": state.candidate_id,
        "name": state.name,
        "stage": state.stage.as_str(),
        "producer": state.producer,
        "explicit_request": state.explicit_request,
        "pattern_key": state.pattern_key,
        "source_runs": state.source_runs.iter().map(|run| &run.run_id).collect::<Vec<_>>(),
        "reviewers": state.reviewers,
        "canary_runs": state.canary_runs.iter().map(|run| &run.run_id).collect::<Vec<_>>(),
        "bench": state.bench,
        "published_path": state.published_path,
        "rollback_path": state.rollback_path,
        "card_receipt": state.card_receipt,
        "updated_at": state.updated_at,
    });
    let content = serde_json::to_string_pretty(&projection)?;
    if content.len() > 256 * 1024 {
        bail!("reverse-skill status projection exceeded 256 KiB");
    }
    Ok(ToolOutput {
        summary: format!(
            "reverse skill {} is {}",
            state.candidate_id,
            state.stage.as_str()
        ),
        content,
    })
}

fn required(value: Option<String>, name: &str) -> Result<String> {
    let value = value.unwrap_or_default().trim().to_string();
    if value.is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}
