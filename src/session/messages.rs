//! Message formatting for agent communication

use serde::{Deserialize, Serialize};

/// A message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// User message (from human or talk envelope)
    User { content: String },
    /// Talk message between agents
    Talk {
        from: String,
        to: String,
        subject: String,
        body: String,
        reply_expected: bool,
        /// Stable lifecycle identity. Empty on legacy rows.
        #[serde(default)]
        handoff_id: String,
        /// Exact handoff answered by this row, when it is a return.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        /// Durable delivery/event that caused this row, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        causation_id: Option<String>,
        /// Durable lifecycle state (`queued`, `working`, `done`, `blocked`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        status: String,
    },
    /// A contribution authored inside a first-class group conversation.
    ///
    /// Unlike legacy `Talk`, this record keeps immutable identity and
    /// presentation snapshots. A later rename, avatar edit, or roster removal
    /// must not rewrite who authored historical room messages.
    GroupContribution {
        turn_id: String,
        message_id: String,
        group_id: String,
        agent_id: String,
        internal_role: String,
        display_name: String,
        role_title: String,
        color: String,
        icon_seed: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        avatar: Option<serde_json::Value>,
        subject: String,
        body: String,
        /// Exact activation/handoff answered by this contribution when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        /// Durable delivery/event that caused the contribution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        causation_id: Option<String>,
    },
    /// Tool result captured in the session transcript
    ToolResult {
        tool_name: String,
        input: String,
        success: bool,
        output: String,
    },
    /// Assistant response
    Assistant { content: String },
}

impl Message {
    /// Format a talk message for delivery to a sub-agent.
    ///
    /// The envelope tells the receiver WHERE its result goes — not just that a
    /// message arrived. This is the runtime enforcement of the team / Work-Web
    /// model: the receiver never has to guess whether to reply to the sender,
    /// pass the baton forward to the next owner, or report up to the
    /// orchestrator. `reply_expected` is `mode == 1` (synchronous reply);
    /// `!reply_expected` is `mode == 2` (background baton).
    pub fn format_talk_envelope(
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        reply_expected: bool,
    ) -> String {
        let context_message = crate::tools::is_transport_message(body);
        let visible_body = crate::tools::visible_transport_body(body);
        let handoff_hint = if context_message {
            format!(
                "This is an asynchronous context message from {from}, not delegated work and not a request for acknowledgement. Incorporate it into your current turn if one is running, or your next turn otherwise. Do not claim new ownership, send a courtesy reply, or report completion merely because this message arrived."
            )
        } else if reply_expected {
            format!(
                "This is a direct request from {from}. Return your result TO {from} — do not loop it through the orchestrator. {from} needs your answer to finish its own deliverable."
            )
        } else if from == "orchestrator" {
            // A background spawn from the manager: usually a single-stage job.
            // Only baton onward when the brief actually names further stages.
            "This is a background job from the orchestrator. If the brief names further stages and their owners, execute your stage and hand the baton DIRECTLY to the next owner (finish your own turn with a one-line receipt — the baton carries the substance). Otherwise the job is yours end-to-end: report the finished result to the orchestrator."
                .to_string()
        } else {
            format!(
                "This is a background baton from {from}. Execute your stage, then hand the baton DIRECTLY to the next owner named in the plan — do not bounce back to {from} and do not route through the orchestrator between stages. Only if you are the LAST stage in the plan, report the finished result to the orchestrator. Finish your own turn with a one-line receipt; the baton carries the substance."
            )
        };

        format!(
            "Talk message.\nFrom: {}\nSubject: {}\nTo: {}\nReply: {}\n\n{}\n\n{}",
            from,
            subject,
            to,
            if reply_expected { "yes" } else { "no" },
            visible_body,
            handoff_hint
        )
    }

    pub fn format_tool_result(tool_name: &str, input: &str, success: bool, output: &str) -> String {
        let input = crate::runtime::efficiency::compact_tool_input(tool_name, input);
        format!(
            "Tool result.\nTool: {tool_name}\nSuccess: {}\nInput: {input}\n\n{}",
            if success { "yes" } else { "no" },
            output
        )
    }

    pub fn format_group_contribution(
        agent_id: &str,
        display_name: &str,
        role_title: &str,
        subject: &str,
        body: &str,
    ) -> String {
        let title = if role_title.trim().is_empty() {
            String::new()
        } else {
            format!(" ({role_title})")
        };
        format!(
            "Group contribution.\nFrom: {display_name}{title}\nAgent id: {agent_id}\nSubject: {subject}\n\n{body}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode1_envelope_directs_the_reply_back_to_the_requester() {
        let env = Message::format_talk_envelope("coder", "database", "schema check", "body", true);
        // Mode 1 = synchronous reply: the result goes back to the sender, never
        // looped through the orchestrator.
        assert!(env.contains("From: coder"));
        assert!(env.contains("To: database"));
        assert!(env.contains("Reply: yes"));
        assert!(env.contains("Return your result TO coder"));
        assert!(env.contains("do not loop it through the orchestrator"));
    }

    #[test]
    fn mode2_envelope_from_the_orchestrator_fits_single_stage_jobs() {
        // The manager's background spawn is usually one stage with no plan —
        // the hint must not push the specialist to invent a baton target.
        let env = Message::format_talk_envelope(
            "orchestrator",
            "browser",
            "check notifications",
            "body",
            false,
        );
        assert!(env.contains("If the brief names further stages"));
        assert!(env.contains("Otherwise the job is yours end-to-end"));
    }

    #[test]
    fn mode2_envelope_directs_the_baton_forward_not_back() {
        let env =
            Message::format_talk_envelope("planner", "coder", "build the X feature", "body", false);
        // Mode 2 = background baton: pass forward to the next owner; only the
        // LAST stage reports to the orchestrator; never bounce back to sender
        // or route through the orchestrator between stages.
        assert!(env.contains("Reply: no"));
        assert!(env.contains("background baton from planner"));
        assert!(env.contains("hand the baton DIRECTLY to the next owner"));
        assert!(env.contains("do not bounce back to planner"));
        assert!(env.contains("do not route through the orchestrator between stages"));
        assert!(env.contains("LAST stage"));
    }

    #[test]
    fn context_message_does_not_become_a_work_handoff() {
        let body = "<!-- phoenix-message-priority:urgent -->\nUse the corrected screenshot.";
        let env = Message::format_talk_envelope("planner", "coder", "Correction", body, false);
        assert!(env.contains("asynchronous context message"));
        assert!(env.contains("not delegated work"));
        assert!(env.contains("Do not claim new ownership"));
        assert!(!env.contains("background baton"));
    }

    #[test]
    fn group_contribution_round_trips_stable_identity_and_visual_snapshot() {
        let message = Message::GroupContribution {
            turn_id: "turn-42".to_string(),
            message_id: "group-message-7".to_string(),
            group_id: "launch-room".to_string(),
            agent_id: "agent-iris".to_string(),
            internal_role: "frontend".to_string(),
            display_name: "Iris".to_string(),
            role_title: "Product Designer".to_string(),
            color: "#d46a43".to_string(),
            icon_seed: "iris-flame".to_string(),
            avatar: Some(serde_json::json!({
                "mode": "flame",
                "shape": "wild",
                "expression": "focused",
                "accessory": "round_glasses"
            })),
            subject: "UI findings".to_string(),
            body: "The composer needs a stable preview row.".to_string(),
            reply_to: Some("turn-42".to_string()),
            causation_id: Some("turn-42".to_string()),
        };

        let encoded = serde_json::to_string(&message).expect("serialize group contribution");
        let decoded: Message =
            serde_json::from_str(&encoded).expect("reload group contribution from JSON");
        match decoded {
            Message::GroupContribution {
                turn_id,
                message_id,
                group_id,
                agent_id,
                internal_role,
                display_name,
                role_title,
                color,
                icon_seed,
                avatar,
                subject,
                body,
                reply_to,
                causation_id,
            } => {
                assert_eq!(turn_id, "turn-42");
                assert_eq!(message_id, "group-message-7");
                assert_eq!(group_id, "launch-room");
                assert_eq!(agent_id, "agent-iris");
                assert_eq!(internal_role, "frontend");
                assert_eq!(display_name, "Iris");
                assert_eq!(role_title, "Product Designer");
                assert_eq!(color, "#d46a43");
                assert_eq!(icon_seed, "iris-flame");
                assert_eq!(avatar.unwrap()["shape"], "wild");
                assert_eq!(subject, "UI findings");
                assert_eq!(body, "The composer needs a stable preview row.");
                assert_eq!(reply_to.as_deref(), Some("turn-42"));
                assert_eq!(causation_id.as_deref(), Some("turn-42"));
            }
            other => panic!("expected group contribution, got {other:?}"),
        }
    }

    #[test]
    fn legacy_talk_json_still_decodes_unchanged() {
        let decoded: Message = serde_json::from_str(
            r#"{"type":"Talk","from":"Remy","to":"Phoenix","subject":"handoff","body":"Legacy body","reply_expected":false}"#,
        )
        .expect("legacy Talk rows must remain readable");

        match decoded {
            Message::Talk {
                from,
                to,
                subject,
                body,
                reply_expected,
                handoff_id,
                reply_to,
                causation_id,
                status,
            } => {
                assert_eq!(from, "Remy");
                assert_eq!(to, "Phoenix");
                assert_eq!(subject, "handoff");
                assert_eq!(body, "Legacy body");
                assert!(!reply_expected);
                assert!(handoff_id.is_empty());
                assert!(reply_to.is_none());
                assert!(causation_id.is_none());
                assert!(status.is_empty());
            }
            other => panic!("expected legacy Talk, got {other:?}"),
        }
    }
}
