//! ask_user tool - prompt the user with questions and collect responses

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::runtime::AskUserHandler;

use super::ToolOutput;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestion {
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default, alias = "choices")]
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserInput {
    #[serde(alias = "num_questions")]
    pub questions: Vec<AskUserQuestion>,
    /// Optional machine-readable approval. The runtime only grants it when
    /// the user's actual answer exactly selects `approved_option`.
    #[serde(default)]
    pub approval: Option<ApprovalRequest>,
}

/// A generic question cannot open an authenticated browser handoff or check
/// saved credentials. Route explicit sign-in requests through that tool.
pub fn validate_login_routing(input: &AskUserInput) -> Result<()> {
    if input.approval.is_some() {
        return Ok(());
    }
    for question in &input.questions {
        let text = question.question.to_ascii_lowercase();
        let sign_in = text.contains("login") || text.contains("log in") || text.contains("sign in");
        let request = text.contains("please complete")
            || text.contains("please log in")
            || text.contains("please sign in")
            || question.options.iter().any(|option| {
                let option = option.to_ascii_lowercase();
                option.starts_with("open ")
                    && (option.contains("login") || option.contains("sign in"))
            });
        if sign_in && request {
            bail!("Use ask_for_login with the exact site for a sign-in handoff, not a generic ask_user card. It checks this coworker's saved credentials and pending login flow first. Use an available credential through browser_input_credential and verify submission before claiming it failed. A disconnected browser is a runtime problem, not a request for the user to log in again.");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeachWorkflowInput {
    /// The repeatable outcome the user will demonstrate, in plain language.
    #[serde(alias = "goal", alias = "task")]
    pub workflow_goal: String,
    /// Optional page to open when the embedded teaching browser starts.
    #[serde(default)]
    pub start_url: Option<String>,
    /// Who may reuse the finished workflow: this coworker, the current group,
    /// or the whole company.
    #[serde(default = "default_teaching_scope")]
    pub scope: String,
}

fn default_teaching_scope() -> String {
    "agent".to_string()
}

impl TeachWorkflowInput {
    /// Turn the agent-facing one-purpose tool into the typed question contract
    /// understood by Canvas. The runtime binds the real coworker/group ids
    /// afterward, so models cannot accidentally or deliberately choose another
    /// coworker's private browser profile.
    pub fn into_ask(mut self) -> Result<AskUserInput> {
        self.workflow_goal = self.workflow_goal.trim().to_string();
        if self.workflow_goal.is_empty() {
            bail!("teach_workflow requires a concrete workflow_goal");
        }
        if self.workflow_goal.chars().count() > 500 {
            bail!("teach_workflow workflow_goal is too long (maximum 500 characters)");
        }
        self.scope = self.scope.trim().to_ascii_lowercase();
        if !matches!(self.scope.as_str(), "agent" | "group" | "company") {
            bail!("teach_workflow scope must be agent, group, or company");
        }
        let start_url = self
            .start_url
            .take()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty());
        if let Some(url) = start_url.as_deref() {
            if url.chars().count() > 2_048 {
                bail!("teach_workflow start_url is too long");
            }
            if !(url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("about:"))
            {
                bail!("teach_workflow start_url must be an http(s) or about: URL");
            }
        }

        let mut details = std::collections::BTreeMap::from([
            ("workflow_goal".to_string(), self.workflow_goal.clone()),
            ("scope".to_string(), self.scope),
        ]);
        if let Some(url) = start_url {
            details.insert("start_url".to_string(), url);
        }
        Ok(AskUserInput {
            questions: vec![AskUserQuestion {
                header: Some("Teach workflow".to_string()),
                question: format!(
                    "Show me how you {} in the browser? I’ll follow along and save it as a reusable workflow.",
                    self.workflow_goal
                ),
                options: vec!["Teach now".to_string(), "Not now".to_string()],
                multi_select: false,
            }],
            approval: Some(ApprovalRequest {
                action: "teach_workflow".to_string(),
                subject: self.workflow_goal,
                approved_option: "Teach now".to_string(),
                details,
            }),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub action: String,
    pub subject: String,
    pub approved_option: String,
    /// Typed, forward-compatible UI/runtime context (for example `tool_name`,
    /// `current_mode`, `required_mode`, and `scope`).
    #[serde(default)]
    pub details: std::collections::BTreeMap<String, String>,
}

/// Bind client-facing flows to the coworker that actually made the call.
/// Models may know a display name or runtime lane, but the canvas needs the
/// durable directory id to open the correct private browser profile. Never
/// trust a model-supplied owner id for this routing decision.
pub fn bind_runtime_context(input: &mut AskUserInput, runtime_role: &str, group_id: Option<&str>) {
    // Teaching is a distinct UI action, never a generic approval category.
    // Preserve the authored question, but strip a mismatched routing hint.
    if input.approval.as_ref().is_some_and(|a| a.action == "teach_workflow" && a.approved_option != "Teach now") {
        input.approval = None;
    }

    // Models used to ask a plain prose "Unlock vault" question. Canvas could
    // only submit that sentence back to the model, so nothing unlocked and the
    // model repeated credential_list/login requests. Promote the well-known
    // single vault unblock into the typed local form. The master password then
    // travels only through the Vault IPC command and never through the model.
    if input.approval.is_none() && input.questions.len() == 1 {
        let question = &mut input.questions[0];
        let visible = format!(
            "{} {}",
            question.header.as_deref().unwrap_or_default(),
            question.question
        )
        .to_ascii_lowercase();
        if visible.contains("vault") && (visible.contains("unlock") || visible.contains("locked")) {
            let approved_option = question
                .options
                .iter()
                .find(|option| option.to_ascii_lowercase().contains("unlock"))
                .cloned()
                .unwrap_or_else(|| {
                    question.options.insert(0, "Unlock here".to_string());
                    "Unlock here".to_string()
                });
            input.approval = Some(ApprovalRequest {
                action: "vault_unlock".to_string(),
                subject: "credential vault".to_string(),
                approved_option,
                details: std::collections::BTreeMap::from([(
                    "reason".to_string(),
                    question.question.clone(),
                )]),
            });
        }
    }

    let Some(approval) = input
        .approval
        .as_mut()
        .filter(|approval| approval.action == "teach_workflow")
    else {
        return;
    };

    let normalized = runtime_role.trim().to_ascii_lowercase();
    let agent_id = crate::runtime::company::global()
        .ok()
        .and_then(|company| company.directory_snapshot().ok())
        .and_then(|snapshot| {
            snapshot.agents.into_iter().find(|agent| {
                agent.profile.agent_id.eq_ignore_ascii_case(&normalized)
                    || agent
                        .profile
                        .internal_role
                        .eq_ignore_ascii_case(&normalized)
                    || agent.profile.display_name.eq_ignore_ascii_case(&normalized)
            })
        })
        .map(|agent| agent.profile.agent_id)
        .unwrap_or_else(|| {
            if matches!(normalized.as_str(), "orchestrator" | "phoenix") {
                "phoenix".to_string()
            } else {
                normalized
            }
        });
    approval
        .details
        .insert("owner_agent_id".to_string(), agent_id);
    if let Some(group_id) = group_id.filter(|id| !id.trim().is_empty()) {
        approval
            .details
            .insert("group_id".to_string(), group_id.to_string());
    }
}

impl ApprovalRequest {
    pub fn is_presented_in(&self, questions: &[AskUserQuestion]) -> bool {
        if self.action == "teach_workflow" && self.approved_option != "Teach now" { return false; }
        let subject = self.subject.trim().to_ascii_lowercase();
        let human_subject = subject.replace(['_', '-'], " ");
        let expected = self.approved_option.trim();
        matches!(
            self.action.as_str(),
            "permanent_agent"
                | "tool_permission"
                | "governed_effect"
                | "outside_group_call"
                | "login_request"
                | "teach_workflow"
                | "vault_unlock"
        ) && !subject.is_empty()
            && !expected.is_empty()
            && questions.iter().any(|question| {
                let visible_question = question.question.to_ascii_lowercase();
                (visible_question.contains(&subject) || visible_question.contains(&human_subject))
                    && question
                        .options
                        .iter()
                        .any(|option| option.trim() == expected)
            })
    }

    pub fn confirmed_by(&self, answer: &str) -> bool {
        if self.action == "login_request" {
            return answer.lines().any(|line| {
                matches!(
                    line.trim()
                        .strip_prefix("A: ")
                        .unwrap_or(line.trim())
                        .to_ascii_lowercase()
                        .as_str(),
                    "import from my browser"
                        | "i'll log in"
                        | "create an account"
                        | "embedded login complete"
                )
            });
        }
        let expected = self.approved_option.trim();
        !expected.is_empty()
            && answer.lines().any(|line| {
                line.trim()
                    .strip_prefix("A: ")
                    .unwrap_or(line.trim())
                    .eq_ignore_ascii_case(expected)
            })
    }
}

pub fn execute(input: AskUserInput, handler: Option<&AskUserHandler>) -> Result<ToolOutput> {
    validate_login_routing(&input)?;
    if input.questions.is_empty() {
        bail!("ask_user requires at least one question");
    }

    if let Some(collect) = handler {
        let content = collect(&input)?;
        return Ok(ToolOutput {
            summary: format!("{} question(s) answered by user.", input.questions.len()),
            content,
        });
    }

    let questions = input.questions;
    let mut lines = vec![format!(
        "Asked {} question(s) — user response pending:",
        questions.len()
    )];

    for q in &questions {
        let header = q.header.as_deref().unwrap_or("Question");
        lines.push(format!("\n  [{header}] {}", q.question));

        if q.options.is_empty() {
            lines.push("    (free-text response expected)".to_string());
        } else {
            let mode = if q.multi_select {
                "multi-select"
            } else {
                "single-select"
            };
            lines.push(format!("    Mode: {mode}"));
            for (j, opt) in q.options.iter().enumerate() {
                lines.push(format!("      {}. {}", j + 1, opt));
            }
        }
    }

    lines.push(
        "\n  ⚠ ask_user requires interactive CLI (phoenix start) to collect responses.".to_string(),
    );

    Ok(ToolOutput {
        summary: format!(
            "{} question(s) asked — awaiting user response.",
            questions.len()
        ),
        content: lines.join("\n"),
    })
}

/// Format ask_user questions for CLI display (used by the interactive runner)
pub fn format_for_cli(input: &AskUserInput) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (i, q) in input.questions.iter().enumerate() {
        let header = q.header.as_deref().unwrap_or("Question");
        if i > 0 {
            lines.push(String::new());
        }
        lines.push(format!("\x1b[1m[{header}]\x1b[0m {}", q.question));

        for (j, opt) in q.options.iter().enumerate() {
            lines.push(format!("  {}. {}", j + 1, opt));
        }

        if q.options.is_empty() {
            lines.push("  (enter your response)".to_string());
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_login_card_must_use_credential_aware_flow() {
        let input: AskUserInput = serde_json::from_value(serde_json::json!({
            "questions": [{"question": "Moodle is returning to login. Please complete the VVS login in Avery’s embedded browser.", "options": ["Open Moodle login"]}]
        })).unwrap();
        let error = execute(input, None).unwrap_err().to_string();
        assert!(error.contains("ask_for_login"));
        assert!(error.contains("browser_input_credential"));
    }

    #[test]
    fn login_design_question_is_not_a_sign_in_request() {
        let input: AskUserInput = serde_json::from_value(serde_json::json!({
            "questions": [{"question": "Should the login page use a dark background?", "options": ["Dark", "Light"]}]
        })).unwrap();
        assert!(validate_login_routing(&input).is_ok());
        let typed = crate::tools::login_request::LoginRequestInput {
            site: "https://example.com".into(), reason: "Complete MFA".into(), scope: "agent".into(), methods: vec!["user_login".into()], username_hint: None,
        }.to_ask("school_coach", None).0;
        assert!(validate_login_routing(&typed).is_ok());
    }


    #[test]
    fn posting_approval_cannot_open_teaching() {
        let mut input: AskUserInput = serde_json::from_value(serde_json::json!({
            "questions":[{"question":"Exact draft: Hello. Approve posting?","options":["Approve posting exactly as written","Not now"]}],
            "approval":{"action":"teach_workflow","subject":"posting","approved_option":"Approve posting exactly as written","details":{"workflow_goal":"Post the draft"}}
        })).unwrap();
        assert!(!input.approval.as_ref().unwrap().is_presented_in(&input.questions));
        bind_runtime_context(&mut input,"phoenix",None);
        assert!(input.approval.is_none());
        assert_eq!(input.questions[0].question,"Exact draft: Hello. Approve posting?");
        assert_eq!(input.questions[0].options[0],"Approve posting exactly as written");
    }

    #[test]
    fn formats_single_question_without_handler() {
        let result = execute(
            AskUserInput {
                questions: vec![AskUserQuestion {
                    question: "Which approach should we use?".to_string(),
                    header: Some("Approach".to_string()),
                    options: vec!["Option A".to_string(), "Option B".to_string()],
                    multi_select: false,
                }],
                approval: None,
            },
            None,
        )
        .unwrap();

        assert!(result.summary.contains("1 question"));
        assert!(result.content.contains("interactive CLI"));
    }

    #[test]
    fn rejects_empty_questions() {
        let result = execute(
            AskUserInput {
                questions: vec![],
                approval: None,
            },
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn plain_vault_unblock_becomes_typed_local_unlock() {
        let mut input = AskUserInput {
            questions: vec![AskUserQuestion {
                header: Some("Unlock vault".to_string()),
                question: "The Moodle login is complete, but the credential vault is locked."
                    .to_string(),
                options: vec!["Unlock vault now".to_string(), "Not now".to_string()],
                multi_select: false,
            }],
            approval: None,
        };

        bind_runtime_context(&mut input, "school_coach", None);

        let approval = input.approval.expect("vault question should be typed");
        assert_eq!(approval.action, "vault_unlock");
        assert_eq!(approval.approved_option, "Unlock vault now");
        assert!(approval.is_presented_in(&input.questions));
    }

    #[test]
    fn approval_requires_the_exact_user_choice() {
        let approval = ApprovalRequest {
            action: "permanent_agent".into(),
            subject: "trader".into(),
            approved_option: "Approve permanent hire".into(),
            details: Default::default(),
        };
        assert!(approval.confirmed_by("Approve permanent hire"));
        assert!(approval.confirmed_by("  A: Approve permanent hire"));
        assert!(!approval.confirmed_by("Do not hire"));
        assert!(!approval.confirmed_by("Maybe approve permanent hire later"));
        assert!(approval.is_presented_in(&[AskUserQuestion {
            question: "Make trader a permanent teammate?".into(),
            header: None,
            options: vec!["Approve permanent hire".into(), "Do not hire".into()],
            multi_select: false,
        }]));
        assert!(!approval.is_presented_in(&[AskUserQuestion {
            question: "Do you like this unrelated design?".into(),
            header: None,
            options: vec!["Approve permanent hire".into()],
            multi_select: false,
        }]));
    }

    #[test]
    fn tool_permission_approval_is_typed_and_exact() {
        let approval = ApprovalRequest {
            action: "tool_permission".into(),
            subject: "browser_click".into(),
            approved_option: "Allow once".into(),
            details: [
                ("required_mode".into(), "full_access".into()),
                ("scope".into(), "single_call".into()),
            ]
            .into_iter()
            .collect(),
        };
        let questions = [AskUserQuestion {
            question: "Nico wants to use `browser_click`. Allow Full Access?".into(),
            header: Some("Permission".into()),
            options: vec!["Allow once".into(), "Keep current access".into()],
            multi_select: false,
        }];
        assert!(approval.is_presented_in(&questions));
        assert!(approval.confirmed_by("A: Allow once"));
        assert!(!approval.confirmed_by("Keep current access"));
        let json = serde_json::to_value(&approval).unwrap();
        assert_eq!(json["details"]["scope"], "single_call");
    }

    #[test]
    fn teaching_flow_is_a_typed_presented_action() {
        let approval = ApprovalRequest {
            action: "teach_workflow".into(),
            subject: "publish the weekly update".into(),
            approved_option: "Teach now".into(),
            details: [
                ("owner_agent_id".into(), "nico".into()),
                ("start_url".into(), "https://example.com".into()),
                ("scope".into(), "agent".into()),
            ]
            .into_iter()
            .collect(),
        };
        let questions = [AskUserQuestion {
            question: "Show Nico how to publish the weekly update?".into(),
            header: Some("Teach workflow".into()),
            options: vec!["Teach now".into(), "Not now".into()],
            multi_select: false,
        }];
        assert!(approval.is_presented_in(&questions));
        assert!(approval.confirmed_by("A: Teach now"));
        assert!(!approval.confirmed_by("Not now"));
    }

    #[test]
    fn teaching_owner_is_bound_by_runtime_not_model_input() {
        let mut input = AskUserInput {
            questions: vec![AskUserQuestion {
                question: "Show Phoenix how to file the report?".into(),
                header: None,
                options: vec!["Teach now".into(), "Not now".into()],
                multi_select: false,
            }],
            approval: Some(ApprovalRequest {
                action: "teach_workflow".into(),
                subject: "file the report".into(),
                approved_option: "Teach now".into(),
                details: [("owner_agent_id".into(), "invented-owner".into())]
                    .into_iter()
                    .collect(),
            }),
        };
        bind_runtime_context(&mut input, "orchestrator", Some("ops"));
        let details = &input.approval.unwrap().details;
        assert_eq!(details["owner_agent_id"], "phoenix");
        assert_eq!(details["group_id"], "ops");
    }

    #[test]
    fn login_receipt_accepts_only_presented_routes_or_explicit_surface_completion() {
        let approval = ApprovalRequest {
            action: "login_request".into(),
            subject: "example.com".into(),
            approved_option: "Import from my browser".into(),
            details: Default::default(),
        };
        assert!(approval.confirmed_by("A: Import from my browser"));
        assert!(approval.confirmed_by("A: I'll log in"));
        assert!(approval.confirmed_by("A: Create an account"));
        assert!(approval.confirmed_by("A: Embedded login complete"));
        assert!(!approval.confirmed_by("A: Not now"));
        assert!(!approval.confirmed_by("Maybe log in later"));
    }

    #[test]
    fn first_class_teaching_request_builds_the_typed_canvas_flow() {
        let mut ask = TeachWorkflowInput {
            workflow_goal: " generate an image in ChatGPT ".into(),
            start_url: Some("https://chatgpt.com".into()),
            scope: "agent".into(),
        }
        .into_ask()
        .unwrap();
        bind_runtime_context(&mut ask, "orchestrator", None);

        let approval = ask.approval.as_ref().unwrap();
        assert_eq!(approval.action, "teach_workflow");
        assert_eq!(approval.subject, "generate an image in ChatGPT");
        assert_eq!(approval.details["owner_agent_id"], "phoenix");
        assert_eq!(approval.details["start_url"], "https://chatgpt.com");
        assert_eq!(ask.questions[0].options, ["Teach now", "Not now"]);
        assert!(approval.is_presented_in(&ask.questions));
    }

    #[test]
    fn teaching_request_rejects_unsafe_or_ambiguous_inputs() {
        assert!(TeachWorkflowInput {
            workflow_goal: " ".into(),
            start_url: None,
            scope: "agent".into(),
        }
        .into_ask()
        .is_err());
        assert!(TeachWorkflowInput {
            workflow_goal: "publish the update".into(),
            start_url: Some("file:///etc/passwd".into()),
            scope: "agent".into(),
        }
        .into_ask()
        .is_err());
    }
}
