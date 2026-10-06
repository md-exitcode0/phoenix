//! Project known runtime failures onto a public answer. Canonical receipts and
//! logs retain diagnostic detail; a chat channel must not export that detail
//! (or attachments mentioned inside a failed tool receipt) as a final result.
pub(crate) fn runtime_failure_summary(markdown: &str) -> Option<&'static str> {
    let mut text = markdown.trim();
    if let Some(wrapped) = text.strip_prefix("**[") {
        if let Some((label, body)) = wrapped.split_once("]**") {
            if !label.is_empty()
                && label.chars().count() <= 120
                && !label.contains(['\r', '\n', ']'])
            {
                text = body.trim_start();
            }
        }
    }
    let text = text.to_lowercase();
    let named_agent = text
        .strip_prefix("the `")
        .and_then(|s| s.split_once('`'))
        .is_some_and(|(name, body)| {
            !name.is_empty() && body.starts_with(" agent could not complete its turn")
        });
    let legacy = text
        .strip_prefix("## result")
        .map(str::trim_start)
        .is_some_and(|body| {
            body.lines()
                .next()
                .is_some_and(|line| line.ends_with(" could not complete its provider-backed turn."))
        });
    let failure = named_agent
        || legacy
        || [
            "the provider became unavailable after",
            "phoenix stopped this agent at a hard runtime boundary:",
            "this agent could not complete its turn",
            "the agent could not complete its turn",
            "phoenix could not complete its turn",
            "phoenix could not complete the turn",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix));
    if !failure {
        return None;
    }
    let quota = [
        "usage_limit_reached",
        "usage limit reached",
        "usage limit has been reached",
        "quota exceeded",
        "insufficient_quota",
    ]
    .iter()
    .any(|s| text.contains(s));
    let auth = crate::config::auth_profile::is_oauth_login_rejected(&text) || [
        "authentication rejected",
        "401",
        "token is expired",
        "refresh token",
        "sign-in expired",
    ]
    .iter()
    .any(|s| text.contains(s));
    Some(if quota && auth {
        "The available provider accounts could not continue. Open Models & Providers in Phoenix to check them."
    } else if quota {
        "The provider reported a usage limit. Wait for it to reset or choose another available account in Phoenix."
    } else if auth {
        "Provider sign-in needs attention. Reconnect the account in Phoenix, then retry."
    } else if text.contains("without confirmed external termination")
        || text.contains("termination was not confirmed")
    {
        "A browser action timed out. Check its result in Phoenix before retrying."
    } else if text.contains("whole-turn deadline") {
        "This run reached its time limit. Open Phoenix to review the unfinished work."
    } else {
        "This run could not finish. Open Phoenix for details and any saved work."
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_error_projection_is_anchored_and_keeps_failure_classes_distinct() {
        let captured="**[coder]** The `coder` agent could not complete its turn: OpenAI Codex API error (429 Too Many Requests): {\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\"}}";
        let summary = runtime_failure_summary(captured).unwrap();
        assert!(summary.contains("usage limit"));
        assert!(!summary.contains('{'));
        assert!(
            runtime_failure_summary("The `avery` agent could not complete its turn: 401")
                .unwrap()
                .contains("Reconnect")
        );
        assert!(runtime_failure_summary(
            "The provider became unavailable after work: OAuth token refresh returned HTTP 400: the provider rejected the saved login; sign in again"
        ).unwrap().contains("Reconnect"));
        assert!(runtime_failure_summary(
            "## Result\nAvery could not complete its provider-backed turn.\nError: 503"
        )
        .is_some());
        for text in [
            "Here is how to fix usage_limit_reached",
            "The report documents a 401 error",
            "```\nThe `coder` agent could not complete its turn: 401\n```",
            "My analysis: The provider became unavailable after a deploy.",
        ] {
            assert!(runtime_failure_summary(text).is_none(), "{text}");
        }
    }
}
