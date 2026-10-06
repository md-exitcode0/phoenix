//! Structured login requests rendered by Phoenix clients as a purpose-built
//! block above the composer. The runtime still uses the proven `ask_user`
//! delivery/answer channel, but the typed approval payload lets the desktop UI
//! offer browser import, human login, and autonomous account creation without
//! trying to infer intent from prose.

use anyhow::Result;
use serde::Deserialize;

use crate::tools::ask_user::{ApprovalRequest, AskUserInput, AskUserQuestion};

#[derive(Debug, Clone, Deserialize)]
pub struct LoginRequestInput {
    pub site: String,
    pub reason: String,
    #[serde(default)]
    pub username_hint: Option<String>,
    #[serde(default = "default_methods")]
    pub methods: Vec<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_methods() -> Vec<String> {
    vec![
        "import_cookies".to_string(),
        "user_login".to_string(),
        "create_account".to_string(),
    ]
}

fn default_scope() -> String {
    "agent".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDecision {
    SavedCredential,
    PendingUser,
    ImportCookies,
    UserLogin,
    UserLoginComplete,
    CreateAccount,
    Cancel,
}

impl LoginDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SavedCredential => "saved_credential",
            Self::PendingUser => "pending_user",
            Self::ImportCookies => "import_cookies",
            Self::UserLogin => "user_login",
            Self::UserLoginComplete => "user_login_complete",
            Self::CreateAccount => "create_account",
            Self::Cancel => "cancel",
        }
    }

    pub fn next_step(self) -> &'static str {
        match self {
            Self::SavedCredential => {
                "A matching password is already available to this coworker. Inspect the current page, then call browser_input_credential with the returned credential_id for the password field. Do not ask the user to unlock or re-enter it."
            }
            Self::PendingUser => {
                "The login choice is pending in the conversation. Continue every independent action now; do not post another login request. The user's eventual choice will wake this same coworker and resume the blocked login lane."
            }
            Self::ImportCookies => {
                "Phoenix imports this site's portable cookies into the coworker's private browser before returning this decision. Verify once; if the site still asks for login, make one new ask_for_login request limited to user_login instead of retrying imports."
            }
            Self::UserLogin => {
                "Phoenix opens the coworker's exact private browser inside the conversation pane at the site's real origin. The user may complete a password, passkey, security-key, OAuth, or 2FA ceremony with the browser and operating system; Phoenix does not capture the secret. Wait for the user to finish there; the answer resumes as user_login_complete, then verify once."
            }
            Self::UserLoginComplete => {
                "The user completed the login inside Phoenix on this coworker's exact private browser profile. Do not open another login window; verify the signed-in page once and continue."
            }
            Self::CreateAccount => {
                "Generate a vault password, begin an account record linked to that credential, create the free account in your private browser, coordinate email verification with a coworker when needed, then mark the account ready. Never select a paid plan without fresh approval."
            }
            Self::Cancel => "Do not attempt another login route; continue only with genuinely usable alternatives.",
        }
    }
}

impl LoginRequestInput {
    pub fn validate_and_normalize(mut self) -> Result<Self> {
        self.site = crate::tools::browser_cookie_grants::normalize_site(&self.site)?;
        self.reason = self.reason.trim().to_string();
        anyhow::ensure!(
            !self.reason.is_empty() && self.reason.len() <= 1_000,
            "login reason must be 1..=1000 bytes"
        );
        if let Some(username) = self.username_hint.as_mut() {
            *username = username.trim().to_string();
            anyhow::ensure!(username.len() <= 512, "username_hint must be <=512 bytes");
            if username.is_empty() {
                self.username_hint = None;
            }
        }
        anyhow::ensure!(
            matches!(self.scope.as_str(), "agent" | "group" | "company"),
            "login scope must be agent, group, or company"
        );
        anyhow::ensure!(
            (1..=3).contains(&self.methods.len()),
            "login request needs 1..=3 methods"
        );
        anyhow::ensure!(
            self.methods.iter().all(|method| matches!(
                method.as_str(),
                "import_cookies" | "user_login" | "create_account"
            )),
            "unknown login method"
        );
        let mut seen = std::collections::HashSet::new();
        self.methods.retain(|method| seen.insert(method.clone()));
        Ok(self)
    }

    pub fn to_ask(
        &self,
        agent_id: &str,
        group_id: Option<&str>,
    ) -> (AskUserInput, Vec<(String, LoginDecision)>) {
        let agent_name = crate::runtime::delegation::agent_display_name(agent_id).replace('_', " ");
        let mut options = Vec::new();
        for method in &self.methods {
            let (label, decision) = match method.as_str() {
                "import_cookies" => ("Import from my browser", LoginDecision::ImportCookies),
                "user_login" => ("I'll log in", LoginDecision::UserLogin),
                "create_account" => ("Create an account", LoginDecision::CreateAccount),
                _ => continue,
            };
            options.push((label.to_string(), decision));
        }
        options.push(("Not now".to_string(), LoginDecision::Cancel));

        let approved_option = options
            .first()
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| "Not now".to_string());
        let mut details = std::collections::BTreeMap::new();
        details.insert("site".to_string(), self.site.clone());
        details.insert("agent_id".to_string(), agent_id.to_string());
        details.insert("scope".to_string(), self.scope.clone());
        details.insert("methods".to_string(), self.methods.join(","));
        details.insert(
            "auth_surface".to_string(),
            "site_origin_in_private_browser".to_string(),
        );
        details.insert(
            "authenticators".to_string(),
            "passkey,security_key,password,oauth,2fa".to_string(),
        );
        details.insert("user_presence".to_string(), "required".to_string());
        details.insert("credential_capture".to_string(), "none".to_string());
        if let Some(group_id) = group_id {
            details.insert("group_id".to_string(), group_id.to_string());
        }
        if let Some(username) = &self.username_hint {
            details.insert("username_hint".to_string(), username.clone());
        }
        let approval = ApprovalRequest {
            action: "login_request".to_string(),
            subject: self.site.clone(),
            approved_option,
            details,
        };
        let question = AskUserQuestion {
            header: Some("Login needed".to_string()),
            question: format!(
                "{} needs access to {} to {}. How should Phoenix continue?",
                agent_name, self.site, self.reason
            ),
            options: options.iter().map(|(label, _)| label.clone()).collect(),
            multi_select: false,
        };
        (
            AskUserInput {
                questions: vec![question],
                approval: Some(approval),
            },
            options,
        )
    }
}

pub fn decision_from_answer(answer: &str, options: &[(String, LoginDecision)]) -> LoginDecision {
    for line in answer.lines() {
        let candidate = line.trim().strip_prefix("A: ").unwrap_or(line.trim());
        if candidate.eq_ignore_ascii_case("Embedded login complete") {
            return LoginDecision::UserLoginComplete;
        }
        if let Some((_, decision)) = options
            .iter()
            .find(|(label, _)| label.eq_ignore_ascii_case(candidate))
        {
            return *decision;
        }
    }
    LoginDecision::Cancel
}

/// One stable machine-readable result for mesh and one-shot runtimes. Clients
/// can change presentation without making agents infer the chosen login route
/// from prose.
pub fn decision_result(input: &LoginRequestInput, decision: LoginDecision) -> serde_json::Value {
    let configured_cookie_source = (decision == LoginDecision::ImportCookies)
        .then(resolve_cookie_source)
        .flatten();
    serde_json::json!({
        "decision": decision.as_str(),
        "site": input.site,
        "scope": input.scope,
        "username_hint": input.username_hint,
        "auth_surface": "site_origin_in_private_browser",
        "user_presence_verified_by": if decision == LoginDecision::UserLoginComplete {
            "explicit_user_completion"
        } else {
            "not_applicable"
        },
        "credential_capture": "none",
        "configured_cookie_source": configured_cookie_source,
        "next_step": decision.next_step()
    })
}

/// Prefer a scoped vault credential before cookie import, account creation, or
/// a human login prompt. The human management UI may remain locked: this uses
/// the separately wrapped agent runtime key and returns metadata only.
pub fn saved_credential_for_site(
    agent_id: &str,
    group_id: Option<&str>,
    site: &str,
) -> Result<Option<crate::security::vault::CredentialMetadata>> {
    let scopes =
        crate::tools::browser_cookie_grants::visible_credential_scopes(agent_id, group_id)?;
    let mut credentials = crate::security::vault::Vault::open_default().list_for_agent(&scopes)?;
    credentials.retain(|credential| {
        credential.kind.eq_ignore_ascii_case("password")
            && crate::tools::browser_cookie_grants::domain_matches_site(&credential.site, site)
    });
    credentials.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(credentials.into_iter().next())
}

/// A human login is a one-use unblock within a turn. Once the matching typed
/// handoff returned `user_login_complete`, another `ask_for_login` call for the
/// same site is always a model retry, not a new user decision. Keep this check
/// on structured receipts so prose cannot spoof it.
pub fn completed_in_tool_results<'a>(
    site: &str,
    outputs: impl IntoIterator<Item = &'a str>,
) -> bool {
    decision_in_tool_results(site, "user_login_complete", outputs)
}

/// A durable login card may outlive the provider response that created it.
/// Once one card is pending for a site, later model rounds in the same turn
/// must reuse that receipt instead of creating duplicate cards.
pub fn pending_in_tool_results<'a>(site: &str, outputs: impl IntoIterator<Item = &'a str>) -> bool {
    decision_in_tool_results(site, "pending_user", outputs)
}

fn decision_in_tool_results<'a>(
    site: &str,
    decision: &str,
    outputs: impl IntoIterator<Item = &'a str>,
) -> bool {
    let Ok(expected) = crate::tools::browser_cookie_grants::normalize_site(site) else {
        return false;
    };
    outputs.into_iter().any(|output| {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
            return false;
        };
        value.get("decision").and_then(|value| value.as_str()) == Some(decision)
            && value
                .get("site")
                .and_then(|value| value.as_str())
                .and_then(|site| crate::tools::browser_cookie_grants::normalize_site(site).ok())
                .is_some_and(|site| site == expected)
    })
}

/// Pick the policy-owned route before rendering any chooser. Cookie import is
/// deliberately first: an existing portable session is cheaper, faster, and
/// less disruptive than creating another account. Free-account creation is the
/// automatic fallback only when both the account feature and policy allow it.
pub fn automatic_decision(
    input: &LoginRequestInput,
    import_policy: &str,
    account_policy: &str,
    account_enabled: bool,
) -> Option<LoginDecision> {
    if import_policy == "allow"
        && input
            .methods
            .iter()
            .any(|method| method == "import_cookies")
    {
        return Some(LoginDecision::ImportCookies);
    }
    if account_enabled
        && account_policy == "allow_free"
        && input
            .methods
            .iter()
            .any(|method| method == "create_account")
    {
        return Some(LoginDecision::CreateAccount);
    }
    None
}

/// Once automatic cookie import has genuinely failed, creating a free account
/// is the only silent fallback. Paid plans remain governed by the independent
/// purchase gate and never become an account-creation fallback.
pub fn fallback_after_import_failure(
    input: &LoginRequestInput,
    account_policy: &str,
    account_enabled: bool,
) -> Option<LoginDecision> {
    (account_enabled
        && account_policy == "allow_free"
        && input
            .methods
            .iter()
            .any(|method| method == "create_account"))
    .then_some(LoginDecision::CreateAccount)
}

/// Resolve the cookie source without making the person hunt through Settings.
/// An explicit concrete setting wins, `none` remains a deliberate opt-out,
/// legacy config remains compatible, and otherwise Phoenix selects the browser
/// with the freshest cookie store.
pub fn resolve_cookie_source() -> Option<String> {
    if let Some(explicit) = crate::settings::explicit_string(
        "browser.cookie_import_source",
        &crate::settings::SettingsScope::Global,
    ) {
        let source = explicit.trim().to_ascii_lowercase();
        if source == "none" {
            return None;
        }
        if source == "auto" {
            return crate::tools::browser_cookies::detect_active_source().ok();
        }
        if source != "auto" && crate::tools::browser_cookies::is_supported_source(&source) {
            return Some(source);
        }
    }
    if let Some(configured) = crate::config::PhoenixConfig::load()
        .ok()
        .and_then(|config| config.profile.browser)
        .and_then(|browser| browser.login_source)
    {
        let source = configured.trim().to_ascii_lowercase();
        if !source.is_empty()
            && source != "none"
            && source != "auto"
            && crate::tools::browser_cookies::is_supported_source(&source)
        {
            return Some(source);
        }
    }
    crate::tools::browser_cookies::detect_active_source().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_login_request_has_typed_ui_details_and_exact_decisions() {
        let input = LoginRequestInput {
            site: "https://WWW.Example.com/login".into(),
            reason: "publish the approved update".into(),
            username_hint: Some("owner@example.com".into()),
            methods: default_methods(),
            scope: "company".into(),
        }
        .validate_and_normalize()
        .unwrap();
        assert_eq!(input.site, "example.com");
        let (ask, options) = input.to_ask("Nico", Some("operations"));
        let approval = ask.approval.unwrap();
        assert_eq!(approval.action, "login_request");
        assert_eq!(approval.details.get("site").unwrap(), "example.com");
        assert_eq!(approval.details.get("group_id").unwrap(), "operations");
        assert_eq!(
            approval.details.get("authenticators").map(String::as_str),
            Some("passkey,security_key,password,oauth,2fa")
        );
        assert_eq!(
            approval
                .details
                .get("credential_capture")
                .map(String::as_str),
            Some("none")
        );
        assert!(ask.questions[0].question.starts_with("Nico needs access"));
        assert_eq!(
            decision_from_answer("A: Create an account", &options),
            LoginDecision::CreateAccount
        );
        assert_eq!(
            decision_from_answer("something invented", &options),
            LoginDecision::Cancel
        );
        assert_eq!(
            decision_from_answer("A: Embedded login complete", &options),
            LoginDecision::UserLoginComplete
        );
        let result = decision_result(&input, LoginDecision::CreateAccount);
        assert_eq!(result["decision"], "create_account");
        assert_eq!(result["credential_capture"], "none");
        assert!(result["next_step"]
            .as_str()
            .unwrap()
            .contains("Generate a vault password"));
        let saved = decision_result(&input, LoginDecision::SavedCredential);
        assert_eq!(saved["decision"], "saved_credential");
        assert!(saved["next_step"]
            .as_str()
            .unwrap()
            .contains("browser_input_credential"));
    }

    #[test]
    fn blank_optional_username_hint_does_not_break_login_handoff() {
        let input = LoginRequestInput {
            site: "https://vvs-moodle.pembinahills.ca/login/index.php".into(),
            reason: "continue the approved school login".into(),
            username_hint: Some("   ".into()),
            methods: default_methods(),
            scope: "agent".into(),
        }
        .validate_and_normalize()
        .unwrap();
        assert_eq!(input.site, "vvs-moodle.pembinahills.ca");
        assert_eq!(input.username_hint, None);
        let (ask, _) = input.to_ask("Avery", None);
        assert!(!ask.approval.unwrap().details.contains_key("username_hint"));
    }

    #[test]
    fn internal_role_id_never_leaks_into_the_login_question() {
        let input = LoginRequestInput {
            site: "vvs-moodle.pembinahills.ca".into(),
            reason: "continue the approved verification".into(),
            username_hint: None,
            methods: vec!["user_login".into()],
            scope: "agent".into(),
        };
        // Use a compiled founding role rather than a custom coworker. The
        // custom-agent registry is process-global and other tests refresh it
        // from isolated PHOENIX_HOME directories, so relying on a live
        // `school_coach` manifest made this assertion test-order dependent.
        let (ask, _) = input.to_ask("personal_logistics", None);
        let question = &ask.questions[0].question;
        assert!(
            question.starts_with("Cleo (personal logistics) needs access"),
            "{question}"
        );
        assert!(!question.starts_with("personal_logistics"), "{question}");
        assert_eq!(
            ask.approval
                .unwrap()
                .details
                .get("agent_id")
                .map(String::as_str),
            Some("personal_logistics")
        );

        // A custom role may or may not be present in the process-global test
        // registry, but the raw routing id must stay out of user-facing prose
        // in either case. Its typed approval metadata still keeps the exact id.
        let (custom_ask, _) = input.to_ask("school_coach", None);
        let custom_question = &custom_ask.questions[0].question;
        assert!(
            !custom_question.contains("school_coach"),
            "{custom_question}"
        );
        assert_eq!(
            custom_ask
                .approval
                .unwrap()
                .details
                .get("agent_id")
                .map(String::as_str),
            Some("school_coach")
        );
    }

    #[test]
    fn automatic_login_policy_imports_before_creating_a_free_account() {
        let input = LoginRequestInput {
            site: "dreamina.example".into(),
            reason: "generate the requested video".into(),
            username_hint: None,
            methods: default_methods(),
            scope: "agent".into(),
        };
        assert_eq!(
            automatic_decision(&input, "allow", "allow_free", true),
            Some(LoginDecision::ImportCookies)
        );
        assert_eq!(
            automatic_decision(&input, "deny", "allow_free", true),
            Some(LoginDecision::CreateAccount)
        );
        assert_eq!(
            fallback_after_import_failure(&input, "allow_free", true),
            Some(LoginDecision::CreateAccount)
        );
    }

    #[test]
    fn automatic_login_policy_never_silently_creates_when_not_allowed() {
        let input = LoginRequestInput {
            site: "example.com".into(),
            reason: "sign in".into(),
            username_hint: None,
            methods: vec!["create_account".into()],
            scope: "agent".into(),
        };
        assert_eq!(automatic_decision(&input, "deny", "ask", true), None);
        assert_eq!(
            fallback_after_import_failure(&input, "always_ask", true),
            None
        );
        assert_eq!(
            automatic_decision(&input, "deny", "allow_free", false),
            None
        );
    }

    #[test]
    fn one_shot_and_mesh_share_the_automatic_login_fallback_contract() {
        let one_shot = include_str!("mod.rs");
        let mesh = include_str!("../runtime/mesh/turn_loop.rs");
        for (runtime, source) in [("one-shot", one_shot), ("mesh", mesh)] {
            assert!(
                source.contains("login_request::automatic_decision"),
                "{runtime} stopped importing portable cookies automatically"
            );
            assert!(
                source.contains("login_request::fallback_after_import_failure"),
                "{runtime} stopped falling back to allowed free-account creation"
            );
        }
    }

    #[test]
    fn completed_embedded_login_suppresses_same_site_reask_only() {
        let completed = serde_json::json!({
            "decision": "user_login_complete",
            "site": "vvs-moodle.pembinahills.ca",
            "scope": "agent"
        })
        .to_string();
        let imported = serde_json::json!({
            "decision": "import_cookies",
            "site": "vvs-moodle.pembinahills.ca"
        })
        .to_string();
        assert!(completed_in_tool_results(
            "https://vvs-moodle.pembinahills.ca/login/index.php",
            [&imported[..], &completed[..]]
        ));
        assert!(!completed_in_tool_results("notion.so", [&completed[..]]));
    }

    #[test]
    fn pending_login_suppresses_duplicate_same_site_card_only() {
        let pending = serde_json::json!({
            "decision": "pending_user",
            "site": "vvs-moodle.pembinahills.ca",
            "scope": "agent"
        })
        .to_string();
        assert!(pending_in_tool_results(
            "https://vvs-moodle.pembinahills.ca/login/index.php",
            [&pending[..]]
        ));
        assert!(!pending_in_tool_results("notion.so", [&pending[..]]));
    }
}
