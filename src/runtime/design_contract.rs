//! Mechanical admission gate for Phoenix's primary visual-design contract.
//!
//! Prompts tell coworkers to use Taste; this guard prevents a provider from
//! mutating a visual artifact, generating imagery, loading a supplementary
//! design library, or finishing a design turn before Taste was successfully
//! loaded. It is provider- and coworker-independent so Phoenix and every named
//! specialist follow the same contract.

use crate::runtime::RequestedToolCall;
use crate::session::Session;

pub const TASTE_REFERENCE_PATH: &str = "taste/SKILL.md";

const DESIGN_GATE_FEEDBACK: &str = "TASTE DESIGN GATE: load `design_reference` with path `taste/SKILL.md` in its own call before continuing. Taste is Phoenix's mandatory primary design contract. Apply its Design Read and craft rules first; task-specific libraries may supplement it afterward but cannot replace or precede it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignAdmission {
    Allow,
    Feedback(&'static str),
}

#[derive(Debug, Clone)]
pub struct DesignContractGuard {
    required: bool,
    taste_loaded: bool,
}

impl DesignContractGuard {
    pub fn new(request: &str, session: &Session) -> Self {
        let required = is_visual_design_request(request);
        Self {
            required,
            taste_loaded: should_render_visual_references(request)
                && crate::runtime::prompt::taste_reference_is_rendered(session),
        }
    }

    /// Refresh from the same renderability predicate used by prompt assembly.
    /// A tool receipt or pin name alone is insufficient: Taste must resolve to
    /// the material Phoenix will place in the provider context.
    pub fn observe_session(&mut self, session: &Session) {
        self.taste_loaded = crate::runtime::prompt::taste_reference_is_rendered(session);
    }

    pub fn admit_tool_batch(&mut self, calls: &[RequestedToolCall]) -> DesignAdmission {
        // An image-generation call is itself visual design even if the user's
        // phrasing did not contain a classifier keyword.
        if calls.iter().any(activates_design_contract) {
            self.required = true;
        }
        if !self.required || self.taste_loaded {
            return DesignAdmission::Allow;
        }

        // Loading a supplement before the primary contract violates the
        // ordering rule. Taste must be a successful, standalone first load;
        // edits in the same provider batch could execute concurrently.
        let only_taste = calls.len() == 1
            && calls[0].tool_name == "design_reference"
            && is_taste_reference(&calls[0].input.to_string());
        if only_taste {
            DesignAdmission::Allow
        } else if calls.iter().all(is_read_only_inspection) {
            DesignAdmission::Allow
        } else {
            DesignAdmission::Feedback(DESIGN_GATE_FEEDBACK)
        }
    }

    pub fn final_feedback(&self) -> Option<&'static str> {
        (self.required && !self.taste_loaded).then_some(DESIGN_GATE_FEEDBACK)
    }

    #[cfg(test)]
    fn is_satisfied(&self) -> bool {
        !self.required || self.taste_loaded
    }
}

fn is_taste_reference(input: &str) -> bool {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(path) = value.get("path").and_then(serde_json::Value::as_str) {
            return crate::tools::design_refs::canonical_path(path) == TASTE_REFERENCE_PATH;
        }
    }
    let trimmed = input
        .trim()
        .trim_matches(|ch| matches!(ch, '`' | '"' | '\''));
    crate::tools::design_refs::canonical_path(trimmed) == TASTE_REFERENCE_PATH
        || input.contains("taste/SKILL.md")
}

pub(crate) fn is_visual_design_request(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    // Talking about a UI-building capability is not the same as building UI.
    // Keep architecture/research turns lightweight; a later visual-file edit
    // or image_gen call activates the gate mechanically in admit_tool_batch.
    let advisory_or_research = [
        "what do you think",
        "think about how",
        "how would you",
        "architecture report",
        "architecture discussion",
        "architecture approach",
        "research",
        "investigate",
        "evaluate the approach",
        "integration approach",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let also_requests_visual_execution = [
        "go ahead and build",
        "build me",
        "create the ui",
        "create a ui",
        "redesign the",
        "then implement",
        "and implement the ui",
        "make the changes",
        "change the ui",
        "update the ui",
        "fix the ui",
    ]
    .iter()
    .any(|term| lower.contains(term));
    if advisory_or_research && !also_requests_visual_execution {
        return false;
    }
    // A bounded read-only audit of an existing conclusion can quote visual
    // work without asking the critic to perform that work. Treating every
    // occurrence of "image generation" inside supplied evidence as a design
    // command forced read-only reviewers through Taste before they could
    // return a verdict. Actual image/edit tool calls still activate the gate
    // mechanically in `admit_tool_batch`.
    let read_only_meta_review = (lower.contains("read-only") || lower.contains("read only"))
        && [
            "review",
            "evaluate",
            "audit",
            "sanity-check",
            "sanity check",
        ]
        .iter()
        .any(|term| lower.contains(term))
        && ["conclusion", "findings", "report", "evidence", "status"]
            .iter()
            .any(|term| lower.contains(term));
    if read_only_meta_review {
        return false;
    }
    let self_evident_visual_action = [
        "visual design",
        "ui design",
        "ux design",
        "ui/ux",
        "generate an image",
        "generate image",
        "image generation",
        "presentation design",
    ]
    .iter()
    .any(|term| lower.contains(term));
    if self_evident_visual_action {
        return true;
    }

    let words = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let has_visual_phrase = [
        "user interface",
        "front-end",
        "landing page",
        "hero section",
        "color palette",
        "brand identity",
        "slide deck",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let has_visual_artifact = has_visual_phrase
        || words.iter().any(|word| {
            matches!(
                *word,
                "ui" | "ux"
                    | "frontend"
                    | "webpage"
                    | "website"
                    | "page"
                    | "screen"
                    | "layout"
                    | "wireframe"
                    | "mockup"
                    | "prototype"
                    | "dashboard"
                    | "logo"
                    | "illustration"
                    | "artwork"
                    | "icon"
                    | "icons"
                    | "imagery"
                    | "slides"
                    | "deck"
                    | "branding"
            )
        });
    let visual_action = |word: &&str| {
        matches!(
            *word,
            "design"
                | "redesign"
                | "style"
                | "restyle"
                | "create"
                | "build"
                | "make"
                | "refresh"
                | "revamp"
                | "improve"
                | "polish"
                | "draw"
                | "illustrate"
                | "generate"
                | "edit"
                | "update"
                | "change"
                | "fix"
                | "repair"
                | "implement"
                | "refine"
                | "audit"
                | "critique"
        )
    };
    let visual_artifact = |word: &&str| {
        matches!(
            *word,
            "ui" | "ux"
                | "frontend"
                | "webpage"
                | "website"
                | "page"
                | "screen"
                | "layout"
                | "wireframe"
                | "mockup"
                | "prototype"
                | "dashboard"
                | "logo"
                | "illustration"
                | "artwork"
                | "icon"
                | "icons"
                | "imagery"
                | "slides"
                | "deck"
                | "branding"
                | "interface"
                | "front-end"
                | "hero"
                | "palette"
        )
    };
    // Pair generic mutation verbs with the visual artifact they modify. A
    // whole-message AND made “Google Slides are ready; update the cookie
    // code” look like a request to redesign Slides and loaded Taste in a
    // school/account turn. Four words covers normal “redesign the settings
    // dashboard” phrasing without joining unrelated clauses.
    let nearby_visual_action = words.iter().enumerate().any(|(index, word)| {
        visual_artifact(word)
            && words[index.saturating_sub(4)..=(index + 4).min(words.len() - 1)]
                .iter()
                .any(visual_action)
    });
    has_visual_artifact && nearby_visual_action
}

/// Whether a previously loaded visual contract belongs in this provider
/// round. Explicit visual work renders it; short continuation commands keep
/// it available for the endless UI coworker session. Ordinary discussion,
/// status, research, and architecture turns do not pay for it.
pub(crate) fn should_render_visual_references(request: &str) -> bool {
    if is_visual_design_request(request) {
        return true;
    }
    matches!(
        request.trim().to_ascii_lowercase().as_str(),
        "continue"
            | "continue please"
            | "keep going"
            | "go ahead"
            | "do it"
            | "finish it"
            | "finish the ui"
            | "finish the design"
    )
}

/// Conservative allowlist: after visual intent is established, an unknown or
/// dual-purpose tool is treated as potentially mutating. This covers shell,
/// generated-code, browser/computer actions, MCP, Composio execution, and new
/// tools by default instead of trying to maintain an incomplete mutation list.
fn is_read_only_inspection(call: &RequestedToolCall) -> bool {
    matches!(
        call.tool_name.as_str(),
        "read"
            | "grep"
            | "glob"
            | "codebase_search"
            | "list_directory"
            | "symbol_search"
            | "callers"
            | "callees"
            | "impact"
            | "call_path"
            | "file_symbols"
            | "web_search"
            | "web_fetch"
            | "web_scrape"
            | "web_crawl"
            | "ui_snap"
            | "image_analyze"
            | "design_studio"
            | "mcp_servers"
            | "composio_search"
            | "composio_schemas"
            | "skill_search"
            | "recall"
            | "memory_recall"
            | "computer_status"
            | "computer_screenshot"
            | "computer_read_text"
            | "computer_list_windows"
            | "computer_app_targets"
            | "computer_app_inspect"
            | "computer_app_locate"
            | "computer_app_read"
            | "computer_locate"
            | "browser_console"
            | "browser_dropdown_options"
            | "browser_extract"
            | "browser_find_elements"
            | "browser_find_text"
            | "browser_search_page"
            | "browser_screenshot"
            | "browser_status"
            | "browser_state"
    )
}

fn activates_design_contract(call: &RequestedToolCall) -> bool {
    match call.tool_name.as_str() {
        "image_gen" => true,
        "write" | "str_replace" => visual_file_input(&call.input),
        _ => false,
    }
}

fn visual_file_input(input: &serde_json::Value) -> bool {
    let Some(path) = ["path", "file", "file_path", "target"]
        .iter()
        .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
    else {
        return false;
    };
    std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "css"
                    | "scss"
                    | "sass"
                    | "less"
                    | "html"
                    | "htm"
                    | "jsx"
                    | "tsx"
                    | "vue"
                    | "svelte"
                    | "svg"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RequestedToolCall;
    use crate::session::{Message, Session};

    fn call(tool: &str, input: serde_json::Value) -> RequestedToolCall {
        RequestedToolCall {
            tool_name: tool.to_string(),
            input,
        }
    }

    #[test]
    fn design_mutation_cannot_skip_taste() {
        let mut session = Session::new_main("model", "system");
        let mut guard = DesignContractGuard::new("Redesign the settings UI", &session);
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "write",
                serde_json::json!({"path":"src/settings.tsx", "content":"..."})
            )]),
            DesignAdmission::Feedback(DESIGN_GATE_FEEDBACK)
        );
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "design_reference",
                serde_json::json!({"path": TASTE_REFERENCE_PATH})
            )]),
            DesignAdmission::Allow
        );
        assert!(
            !guard.is_satisfied(),
            "requesting is not successful loading"
        );
        session.push_message(Message::ToolResult {
            tool_name: "design_reference".into(),
            input: r#"{"path":"taste/SKILL.md"}"#.into(),
            success: true,
            output: "loaded".into(),
        });
        guard.observe_session(&session);
        assert!(guard.is_satisfied());
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "write",
                serde_json::json!({"path":"src/settings.tsx", "content":"..."})
            )]),
            DesignAdmission::Allow
        );
    }

    #[test]
    fn supplement_and_parallel_edit_cannot_precede_successful_taste() {
        let session = Session::new_main("model", "system");
        let mut guard = DesignContractGuard::new("Build a dashboard", &session);
        assert!(matches!(
            guard.admit_tool_batch(&[call(
                "design_reference",
                serde_json::json!({"path":"system/SKILL.md"})
            )]),
            DesignAdmission::Feedback(_)
        ));
        assert!(matches!(
            guard.admit_tool_batch(&[
                call(
                    "design_reference",
                    serde_json::json!({"path": TASTE_REFERENCE_PATH})
                ),
                call(
                    "str_replace",
                    serde_json::json!({"path":"app.css", "old":"a", "new":"b"})
                ),
            ]),
            DesignAdmission::Feedback(_)
        ));
    }

    #[test]
    fn image_generation_activates_gate_without_keyword_classifier() {
        let session = Session::new_main("model", "system");
        let mut guard = DesignContractGuard::new("Make me something beautiful", &session);
        assert!(matches!(
            guard.admit_tool_batch(&[call(
                "image_gen",
                serde_json::json!({"prompt":"an editorial mountain scene"})
            )]),
            DesignAdmission::Feedback(_)
        ));
        assert!(guard.final_feedback().is_some());
    }

    #[test]
    fn successful_pinned_taste_survives_later_design_turns() {
        let mut session = Session::new_main("model", "system");
        session.push_message(Message::ToolResult {
            tool_name: "design_reference".into(),
            input: r#"{"path":"craft/SKILL.md"}"#.into(),
            success: true,
            output: "loaded".into(),
        });
        let guard = DesignContractGuard::new("Design a new dashboard", &session);
        assert!(guard.is_satisfied());
    }

    #[test]
    fn ordinary_code_edits_do_not_trigger_visual_gate() {
        let session = Session::new_main("model", "system");
        let mut guard = DesignContractGuard::new("Fix the database query", &session);
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "str_replace",
                serde_json::json!({"path":"src/query.rs", "old":"a", "new":"b"})
            )]),
            DesignAdmission::Allow
        );
        assert!(guard.final_feedback().is_none());
    }

    #[test]
    fn ui_tool_architecture_discussion_does_not_require_taste() {
        let session = Session::new_main("model", "system");
        let guard = DesignContractGuard::new(
            "A visual tool framework can build custom UIs. What do you think? Check the repo and think about how to integrate it into Phoenix so you could use it to build beautiful UI.",
            &session,
        );
        assert!(guard.is_satisfied());
        assert!(guard.final_feedback().is_none());
    }

    #[test]
    fn connected_slides_status_does_not_turn_unrelated_cookie_update_into_design() {
        let session = Session::new_main("model", "system");
        let guard = DesignContractGuard::new(
            "Google Slides, Sheets, and Docs are ready. VVS cookies update often, so I will rewrite the Phoenix cookie code.",
            &session,
        );
        assert!(guard.is_satisfied());
        assert!(guard.final_feedback().is_none());
    }

    #[test]
    fn inspection_and_backend_component_names_do_not_false_positive() {
        let session = Session::new_main("model", "system");
        let mut guard = DesignContractGuard::new("Inspect the current application", &session);
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "ui_snap",
                serde_json::json!({"url":"http://localhost:3000"})
            )]),
            DesignAdmission::Allow
        );
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "str_replace",
                serde_json::json!({
                    "path":"src/component_registry.rs",
                    "old":"theme_token",
                    "new":"theme_key"
                })
            )]),
            DesignAdmission::Allow
        );
        assert!(guard.final_feedback().is_none());
    }

    #[test]
    fn database_api_and_system_design_do_not_trigger_visual_contract() {
        let session = Session::new_main("model", "system");
        for request in [
            "Design the PostgreSQL database schema",
            "Design a versioned API for account events",
            "Redesign the distributed job system",
            "Read the dashboard metrics and summarize them",
            "Inspect the user interface accessibility tree",
        ] {
            let mut guard = DesignContractGuard::new(request, &session);
            assert_eq!(
                guard.admit_tool_batch(&[call(
                    "write",
                    serde_json::json!({"path":"src/api.rs", "content":"..."})
                )]),
                DesignAdmission::Allow,
                "ordinary non-visual design must remain available: {request}"
            );
            assert!(guard.final_feedback().is_none());
        }
    }

    #[test]
    fn read_only_status_audit_may_quote_visual_work_without_loading_taste() {
        let session = Session::new_main("model", "system");
        let request = "Bounded review only. Evaluate this evidence-backed status conclusion: the remaining path includes image generation and UI review. Return a material correction. APPROVALS: read-only judgment only; no files, browser, external action, or generation.";
        let mut guard = DesignContractGuard::new(request, &session);
        assert_eq!(
            guard.admit_tool_batch(&[call(
                "final_answer",
                serde_json::json!({"final_markdown":"material correction"})
            )]),
            DesignAdmission::Allow
        );
        assert!(guard.final_feedback().is_none());
    }

    #[test]
    fn genuine_visual_turn_allows_inspection_but_blocks_every_mutation_surface() {
        let session = Session::new_main("model", "system");
        let read_only = [
            call("read", serde_json::json!({"path":"app.css"})),
            call("ui_snap", serde_json::json!({"url":"http://localhost"})),
            call("browser_state", serde_json::json!({})),
            call("computer_screenshot", serde_json::json!({})),
            call("mcp_servers", serde_json::json!({})),
            call("composio_schemas", serde_json::json!({"toolkit":"figma"})),
        ];
        for inspection in read_only {
            let mut guard = DesignContractGuard::new("Redesign the settings page", &session);
            assert_eq!(
                guard.admit_tool_batch(&[inspection]),
                DesignAdmission::Allow
            );
        }

        for mutation in [
            call(
                "bash",
                serde_json::json!({"command":"generate-ui > app.html"}),
            ),
            call(
                "browser_navigate",
                serde_json::json!({"url":"https://example.com"}),
            ),
            call("browser_click", serde_json::json!({"index":3})),
            call("computer_click", serde_json::json!({"x":1,"y":2})),
            call(
                "mcp_call",
                serde_json::json!({"server":"figma","tool":"create"}),
            ),
            call("composio_run", serde_json::json!({"tools":[]})),
            call("tools_create", serde_json::json!({"name":"generated_ui"})),
            call("unknown_future_tool", serde_json::json!({})),
            call("final_answer", serde_json::json!({"final_markdown":"done"})),
        ] {
            let mut guard = DesignContractGuard::new("Redesign the settings page", &session);
            assert!(
                matches!(
                    guard.admit_tool_batch(&[mutation]),
                    DesignAdmission::Feedback(_)
                ),
                "all non-read-only surfaces default closed until Taste renders"
            );
        }
    }
}
