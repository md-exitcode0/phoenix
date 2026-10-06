//! Provider tier-gate detection and friendly error mapping.
//!
//! Some providers hand out credentials that AUTHENTICATE fine but whose plan
//! tier has no API access at all — x.ai SuperGrok OAuth is the canonical case:
//! every chat call 403s with `{"code":"permission-denied", ...}`. Surfacing
//! that raw JSON in the chat pane reads like a broken login and sends the user
//! chasing the wrong fix (re-login), so turns that hit the signature fail with
//! ONE actionable sentence instead, and the raw body goes to the gateway log
//! for forensics — loud on the first hit per host, quiet after (the background
//! sweep re-hits a gated lane every cycle and must not fill the log with
//! identical 403 JSON).
//!
//! The signature mirrors `phoenix auth probe` (cli/headless_config.rs), which
//! detects the same condition at setup time: `permission-denied` in the error,
//! or any 403 from x.ai.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// True when a failed provider call matches a tier-gate signature: a 403 whose
/// body says `permission-denied` (any provider), any 403 from api.x.ai — same
/// verdict `phoenix auth probe` would reach for the lane — or NVIDIA NIM's
/// account-scoped 404.
pub fn is_tier_gate(status: u16, base_url: &str, body: &str) -> bool {
    if status == 403 && (body.contains("permission-denied") || is_xai(base_url)) {
        return true;
    }
    is_nim_account_gate(status, body)
}

fn is_xai(base_url: &str) -> bool {
    base_url.contains("x.ai")
}

/// NVIDIA NIM's tier gate wears a 404, not a 403. An org without the "Public
/// API Endpoints" permission lists all 118 models on `GET /v1/models` and then
/// answers EVERY completion with
/// `{"status":404,"title":"Not Found","detail":"Function '<uuid>': Not found
/// for account '<acct>'"}`. Left raw, that reads as "bad model id" and sends
/// the user editing their config forever — the model is fine, the account is
/// not entitled to run it.
fn is_nim_account_gate(status: u16, body: &str) -> bool {
    status == 404 && body.to_ascii_lowercase().contains("not found for account")
}

/// Bare host for log lines and the generic message ("https://api.x.ai/v1" →
/// "api.x.ai").
fn host_of(base_url: &str) -> &str {
    base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(base_url)
}

/// The user-facing replacement for the raw 403 JSON. MUST stay under the
/// 240-char cap in runtime/runner/text_utils.rs `sanitize_error`, or the
/// actionable tail (what to actually do) gets truncated off the chat message.
pub fn user_message(base_url: &str) -> String {
    user_message_for(base_url, 403, "")
}

/// The gate-specific message. NIM's 404 gate and xAI's 403 gate need different
/// instructions — one is an org permission, the other a plan tier — so the
/// status/body pick the wording.
pub fn user_message_for(base_url: &str, status: u16, body: &str) -> String {
    if is_nim_account_gate(status, body) {
        return "NVIDIA refused the call: this account lacks the \"Public API Endpoints\" \
                permission, so /v1/models works but every completion 404s. Enable it for \
                the org at build.nvidia.com, or point this lane at another NVIDIA account."
            .to_string();
    }
    if is_xai(base_url) {
        "xAI rejected the call: your SuperGrok plan authenticates but has no API access \
         (this is x.ai's tier gate, not a bad login). Paste a console.x.ai API key in \
         the dashboard (Providers → xAI) or switch this lane to another provider."
            .to_string()
    } else {
        format!(
            "{} rejected the call: the account authenticates but its plan tier has no \
             API access (a tier gate, not a bad login). Paste a console API key in the \
             dashboard (Providers) or switch this lane to another provider.",
            host_of(base_url)
        )
    }
}

/// Per-host tier-gate hit counts since the last successful call. Process-wide
/// because provider clients are rebuilt per turn — an instance flag would
/// forget the spam suppression on the sweep's very next cycle.
static HITS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
/// Fast-path guard so `note_success` costs one relaxed load per completion
/// until a tier gate has actually been seen this run.
static ANY_GATED: AtomicBool = AtomicBool::new(false);

fn record_hit(host: &str) -> u64 {
    ANY_GATED.store(true, Ordering::Relaxed);
    let mut hits = HITS
        .get_or_init(Default::default)
        .lock()
        .expect("tier-gate hit map lock");
    let count = hits.entry(host.to_string()).or_insert(0);
    *count += 1;
    *count
}

/// A successful call through the host clears its hit count, so a lane that
/// gets RE-gated later (key revoked, tier downgraded) logs loud again instead
/// of staying suppressed forever.
pub fn note_success(base_url: &str) {
    if !ANY_GATED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(hits) = HITS.get() {
        hits.lock()
            .expect("tier-gate hit map lock")
            .remove(host_of(base_url));
    }
}

/// Map a failed provider response: `Some(message)` when it matches the
/// tier-gate signature (the turn should fail with exactly that message),
/// `None` when it doesn't (caller surfaces its usual error unchanged).
///
/// Logging side effect: the first hit per host warns with the full raw body
/// (forensics in gateway.log); repeats drop to debug so a tier-gated lane the
/// sweep keeps probing doesn't spam an identical 403 every cycle.
pub fn map_response(status: u16, base_url: &str, body: &str) -> Option<String> {
    if !is_tier_gate(status, base_url, body) {
        return None;
    }
    let host = host_of(base_url);
    let hit = record_hit(host);
    if hit == 1 {
        tracing::warn!("tier gate on {host}: {status} {body}");
    } else {
        tracing::debug!(
            "tier gate on {host} still active (hit #{hit}) — raw {status} suppressed until a call succeeds"
        );
    }
    Some(user_message_for(base_url, status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact body x.ai returns to a tier-gated SuperGrok OAuth token.
    const XAI_403: &str = r#"{"code":"permission-denied","error":"Access to the chat endpoint is denied. Please ensure you're using the correct credentials..."}"#;

    #[test]
    fn xai_403_maps_to_the_friendly_message() {
        let message = map_response(403, "https://api.x.ai/v1", XAI_403)
            .expect("xai 403 permission-denied is a tier gate");
        assert!(message.contains("tier gate"), "{message}");
        assert!(message.contains("console.x.ai"), "{message}");
        assert!(message.contains("Providers"), "{message}");
        assert!(
            !message.contains("permission-denied"),
            "raw provider JSON must not leak into the chat message: {message}"
        );
    }

    #[test]
    fn generalized_detector_catches_permission_denied_on_any_provider() {
        assert!(is_tier_gate(
            403,
            "https://api.example.com/v1",
            r#"{"code":"permission-denied","error":"no api access"}"#
        ));
        let message = map_response(
            403,
            "https://api.example.com/v1",
            r#"{"code":"permission-denied"}"#,
        )
        .expect("permission-denied 403 is a tier gate everywhere");
        assert!(message.contains("api.example.com"), "{message}");
    }

    #[test]
    fn unrelated_errors_pass_through_unchanged() {
        // A 403 that isn't the signature (different body, non-xai host).
        assert_eq!(
            map_response(
                403,
                "https://api.openai.com/v1",
                r#"{"error":{"message":"model not allowed for this org"}}"#
            ),
            None
        );
        // permission-denied text on a non-403 status is not the tier gate.
        assert!(!is_tier_gate(400, "https://api.openai.com/v1", XAI_403));
        // Any 403 on x.ai IS the gate (probe parity), even with another body.
        assert!(is_tier_gate(403, "https://api.x.ai/v1", "Forbidden"));
    }

    /// The body a NIM account without the "Public API Endpoints" permission
    /// returns for EVERY completion, while `GET /v1/models` still 200s.
    const NIM_404: &str = r#"{"status":404,"title":"Not Found","detail":"Function '23d4f03a-b8a6-4adb-a183-7daa083a09cc': Not found for account 'QPJ8xxpqmG-NHcVSEeCWOf1HbKLXvOy3sD7YVa5qIDI'"}"#;
    const NIM_BASE: &str = "https://integrate.api.nvidia.com/v1";

    #[test]
    fn nim_account_scoped_404_is_a_gate_and_names_the_real_fix() {
        assert!(is_tier_gate(404, NIM_BASE, NIM_404));
        let message = map_response(404, NIM_BASE, NIM_404).expect("NIM 404 is an account gate");
        assert!(message.contains("Public API Endpoints"), "{message}");
        assert!(message.contains("build.nvidia.com"), "{message}");
        // It must NOT read like a bad model id — that is the wrong fix and the
        // one the raw body sends people chasing.
        assert!(!message.contains("Function '"), "{message}");
        // The turn error must still classify as account-exhaustion, or the
        // fallback chain would abort on a gated account instead of rotating to
        // the next NVIDIA login.
        assert!(crate::providers::fallback::is_account_exhausted_error(
            &message
        ));
    }

    #[test]
    fn an_ordinary_404_is_not_an_account_gate() {
        // A genuinely unknown model id: every account would 404, so this must
        // surface unchanged rather than blaming the account.
        assert!(!is_tier_gate(404, NIM_BASE, "404 page not found"));
        assert_eq!(map_response(404, NIM_BASE, "404 page not found"), None);
        // A live NIM 403 (bad key) is not the permission gate either — it has
        // its own handling in the fallback layer.
        assert_eq!(
            map_response(
                403,
                NIM_BASE,
                r#"{"status":403,"title":"Forbidden","detail":"Authorization failed"}"#
            ),
            None
        );
    }

    #[test]
    fn messages_survive_the_240_char_sanitize_cap() {
        // runtime/runner/text_utils.rs `sanitize_error` truncates at 240 chars;
        // the actionable tail must not be what gets cut.
        assert!(user_message("https://api.x.ai/v1").chars().count() <= 240);
        assert!(user_message("https://api.example.com/v1").chars().count() <= 240);
        assert!(
            user_message_for(NIM_BASE, 404, NIM_404).chars().count() <= 240,
            "NIM gate message is {} chars",
            user_message_for(NIM_BASE, 404, NIM_404).chars().count()
        );
    }

    #[test]
    fn repeat_hits_count_up_and_success_resets() {
        // Unique host so parallel tests don't share the entry.
        let host = "tier-gate-test.invalid";
        assert_eq!(record_hit(host), 1, "first hit logs loud");
        assert_eq!(record_hit(host), 2, "repeats go quiet");
        note_success(&format!("https://{host}/v1"));
        assert_eq!(record_hit(host), 1, "a recovered lane logs loud again");
        note_success(&format!("https://{host}/v1"));
    }
}
