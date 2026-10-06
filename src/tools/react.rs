//! React to the user's message with a single emoji instead of writing a reply.
//!
//! Some messages are acknowledgements, not questions — "also add a button here,
//! thanks". Answering those in prose adds a paragraph the user has to read to
//! learn nothing. A reaction closes the loop at a glance.
//!
//! The receipt is deliberately NOT part of the visible trace: a reaction that
//! announces itself as a tool call is worse than no reaction at all. The desktop
//! filters the story row and renders only the emoji on the message it targets.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::ToolOutput;

/// Reactions are a nod, not a vocabulary. A closed set keeps the agent from
/// inventing decorative emoji spam and keeps the rendered pill predictable.
const ALLOWED: &[&str] = &["👍", "👀", "🔥", "✅", "🎉", "🙏", "😄", "🤔"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReactInput {
    pub emoji: String,
}

pub fn execute(input: ReactInput) -> Result<ToolOutput> {
    let emoji = input.emoji.trim();
    if emoji.is_empty() {
        bail!("react requires an emoji");
    }
    if !ALLOWED.contains(&emoji) {
        bail!(
            "react only accepts one of {} — got {emoji:?}",
            ALLOWED.join(" ")
        );
    }
    Ok(ToolOutput {
        summary: format!("reacted {emoji}"),
        content: format!("reacted {emoji}"),
    })
}

/// The desktop hides these receipts, and so does every transcript summary: the
/// reaction is the whole message, so echoing it as work would double it.
pub fn is_reaction_receipt(text: &str) -> bool {
    text.trim_start().starts_with("reacted ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_allowed_emoji() {
        let out = execute(ReactInput {
            emoji: "👍".into()
        })
        .expect("thumbs up is allowed");
        assert_eq!(out.summary, "reacted 👍");
        assert!(is_reaction_receipt(&out.summary));
    }

    #[test]
    fn rejects_anything_outside_the_set() {
        // Without this the agent drifts into decorating replies with novel
        // emoji, which is exactly the noise a reaction is meant to avoid.
        let error = execute(ReactInput {
            emoji: "🦄".into()
        })
        .expect_err("unicorn is not a reaction");
        assert!(error.to_string().contains("react only accepts"));
    }

    #[test]
    fn rejects_empty_input() {
        assert!(execute(ReactInput { emoji: "  ".into() }).is_err());
    }

    #[test]
    fn every_agent_is_offered_the_tool_and_it_needs_no_extra_access() {
        // A tool missing from the registry makes specialists panic, and a
        // reaction gated behind Full Access would never fire in practice.
        assert!(crate::tools::tool_spec("react").is_some());
        assert!(crate::tools::universal_agent_tool_names()
            .iter()
            .any(|name| name == "react"));
        assert!(
            crate::tools::merge_with_universal_tools(vec!["talk".into()])
                .iter()
                .any(|name| name == "react")
        );
        assert!(crate::tools::descriptions::tool_definition("react").is_some());
    }

    #[test]
    fn ordinary_prose_is_not_a_reaction_receipt() {
        assert!(!is_reaction_receipt(
            "I reacted to the change by rebuilding."
        ));
        assert!(is_reaction_receipt("reacted 🔥"));
    }
}
