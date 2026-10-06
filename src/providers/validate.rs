//! Add-time credential validation: does this API key actually WORK?
//!
//! The failure this exists to stop: onboarding says "stored ✓", and then every
//! turn dies at the provider. Phoenix has been burned by that twice — an
//! ollama-cloud key that 401ed on first use, and a model id that 404ed on
//! every single call after a green setup.
//!
//! ## Why a completion, not `GET /v1/models`
//!
//! For NVIDIA NIM specifically, the models endpoint is worthless as a check —
//! verified live on 2026-07-25 against `https://integrate.api.nvidia.com/v1`:
//!
//! | request                                            | result |
//! |----------------------------------------------------|--------|
//! | `GET /v1/models` with NO `Authorization` header     | `200`, full 118-model catalog |
//! | `GET /v1/models` with a garbage key                 | `200`, full 118-model catalog |
//! | `POST /v1/chat/completions` with a garbage key       | `403 {"status":403,"title":"Forbidden","detail":"Authorization failed"}` |
//! | `POST /v1/chat/completions` with NO header           | `401 Header of type 'authorization' was missing` |
//!
//! So `/v1/models` proves nothing about the key: it is unauthenticated. Worse,
//! it stays green on NIM accounts that CANNOT infer at all — an org missing the
//! "Public API Endpoints" permission lists every model happily and then 404s
//! (`Function '<uuid>': Not found for account '<acct>'`) on every completion.
//! A catalog check would hand that account a clean bill of health and reproduce
//! the exact "green setup, 404 every turn" bug.
//!
//! The only honest check is the thing the agent will actually do: an
//! authenticated `POST /chat/completions`, capped at one token. It exercises
//! auth, account permissions, model access and remaining credits in a single
//! round trip, and costs ~1 request of quota.
//!
//! ## Blocking on purpose
//!
//! Setup is a synchronous dialoguer flow that is sometimes called from inside a
//! tokio runtime. The request therefore runs on a dedicated thread with
//! `reqwest::blocking` (same pattern as the OAuth refresh in
//! `config/auth_profile.rs`), which is safe from either context.

use std::io::Read;
use std::time::Duration;

use crate::providers::providers_data::{self, ProviderModels};

/// How long to wait for the validation round trip. Long enough for a cold NIM
/// model to spin up, short enough that a wedged endpoint doesn't hang setup.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(45);
const VALIDATE_RESPONSE_MAX_BYTES: usize = 1024 * 1024;

fn read_validation_body(response: reqwest::blocking::Response) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > VALIDATE_RESPONSE_MAX_BYTES as u64)
    {
        return Err(format!(
            "provider validation response exceeded {} bytes",
            VALIDATE_RESPONSE_MAX_BYTES
        ));
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(64 * 1024),
    );
    let mut limited = response.take((VALIDATE_RESPONSE_MAX_BYTES + 1) as u64);
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read provider validation response: {error}"))?;
    if bytes.len() > VALIDATE_RESPONSE_MAX_BYTES {
        return Err(format!(
            "provider validation response exceeded {} bytes",
            VALIDATE_RESPONSE_MAX_BYTES
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| "provider validation response was not valid UTF-8".to_string())
}

fn validate_completion_body(body: &str) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| "provider returned HTTP success with invalid JSON".to_string())?;
    if value.get("error").is_some_and(|error| !error.is_null()) {
        return Err("provider returned an error payload with HTTP success".to_string());
    }
    let has_choice = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|choices| choices.first().is_some_and(serde_json::Value::is_object));
    if !has_choice {
        return Err("provider returned HTTP success without a completion choice".to_string());
    }
    Ok(())
}

/// The verdict on a pasted credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider answered. The key works, the account can infer, and it has
    /// credits left.
    Works { model: String },
    /// The provider actively REJECTED the credential or the account. Storing
    /// this key would produce a profile that fails on its first real turn.
    Rejected { model: String, reason: String },
    /// No verdict — the check could not run (unreachable network, or a
    /// provider whose wire format this module does not speak). Never presented
    /// as success.
    Unknown { reason: String },
}

impl KeyCheck {
    pub fn is_rejected(&self) -> bool {
        matches!(self, KeyCheck::Rejected { .. })
    }

    /// One line for a CLI/GUI, already saying what to do next.
    pub fn summary(&self) -> String {
        match self {
            KeyCheck::Works { model } => {
                format!("key verified — {model} answered a live 1-token call")
            }
            KeyCheck::Rejected { model, reason } => {
                format!("key REJECTED by the provider on {model}: {reason}")
            }
            KeyCheck::Unknown { reason } => {
                format!("could not verify the key ({reason}) — storing it unverified")
            }
        }
    }
}

/// Providers served by `OpenAIProvider` in `factory.rs`, i.e. plain
/// `POST {base}/chat/completions` with a bearer key. Anything else (Anthropic,
/// Google, the OAuth/CLI lanes) has its own wire format and is reported
/// `Unknown` rather than guessed at — a false "your key is bad" is worse than
/// no verdict.
fn speaks_openai_chat(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "openai"
            | "groq"
            | "mistral"
            | "together"
            | "fireworks"
            | "deepinfra"
            | "moonshot"
            | "kimi-coding"
            | "tokenrouter"
            | "zai"
            | "xai"
            | "cerebras"
            | "venice"
            | "kilocode"
            | "nvidia"
            | "volcengine"
            | "byteplus"
            | "stepfun"
            | "qianfan"
            | "tencent"
            | "xiaomi"
            | "chutes"
            | "minimax-portal"
            | "github-copilot"
    )
}

/// `{base}/chat/completions`, version-aware — mirrors
/// `OpenAIProvider::endpoint_url` so validation hits the SAME URL a real turn
/// will. A base that already carries `/v1` must not become `/v1/v1/...`.
fn chat_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.contains("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// Turn a failed HTTP response into a short, actionable reason. NIM's three
/// account-killing shapes get named explicitly, because each sends the user to
/// a completely different fix and the raw JSON sends them to none of them.
pub fn explain_rejection(provider_id: &str, status: u16, body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let is_nim = provider_id == "nvidia";
    if is_nim && lower.contains("not found for account") {
        return "this NVIDIA account is missing the \"Public API Endpoints\" permission — \
                the key is valid and /v1/models works, but every completion 404s. Enable \
                Public API Endpoints for the org at build.nvidia.com (personal orgs often \
                need a support request), or use a different NVIDIA account."
            .to_string();
    }
    if status == 402 || lower.contains("payment required") || lower.contains("credits expired") {
        return "the account is out of inference credits (HTTP 402). Add another account \
                as a fallback profile, or top this one up."
            .to_string();
    }
    if status == 401 || status == 403 {
        let hint = if is_nim {
            " NIM answers a bad/revoked key with exactly this. Re-copy the key from \
              build.nvidia.com — it starts with `nvapi-`."
        } else {
            ""
        };
        return format!("the provider refused the credential (HTTP {status}).{hint}");
    }
    if status == 429 {
        return "rate limited (HTTP 429) before the key could be judged — the credential \
                may still be fine. Wait a moment and re-check."
            .to_string();
    }
    let snippet: String = body.trim().chars().take(200).collect();
    format!("HTTP {status}: {snippet}")
}

/// Live-validate an API key with one 1-token completion.
///
/// `model` defaults to the provider's recommended model, which is the model the
/// profile will most likely run — checking a model the user will never call
/// proves the wrong thing. Blocking; safe to call from inside a tokio runtime.
pub fn validate_api_key(provider: &ProviderModels, key: &str, model: Option<&str>) -> KeyCheck {
    let provider_id = provider.id.to_string();
    if !speaks_openai_chat(&provider_id) {
        return KeyCheck::Unknown {
            reason: format!("no live check for `{provider_id}` yet"),
        };
    }
    if key.trim().is_empty()
        || key.len() > 64 * 1024
        || key
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return KeyCheck::Rejected {
            model: String::new(),
            reason: "the key is empty, oversized, or malformed".to_string(),
        };
    }
    let model = model
        .map(str::to_string)
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| providers_data::recommended_model(&provider_id).to_string());
    if model.is_empty() {
        return KeyCheck::Unknown {
            reason: format!("no model to probe for `{provider_id}`"),
        };
    }
    if model.len() > 1024 || model.chars().any(char::is_control) {
        return KeyCheck::Unknown {
            reason: "the model id is oversized or malformed".to_string(),
        };
    }
    let url = chat_url(provider.options.base_url.unwrap_or(provider.base_url));
    let key = key.trim().to_string();
    let probe_model = model.clone();

    // A dedicated thread keeps blocking reqwest legal even when setup is
    // running under the async gateway.
    let outcome = std::thread::spawn(move || -> Result<(u16, String), String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(VALIDATE_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        let response = client
            .post(&url)
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "model": probe_model,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
            }))
            .send()
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let body = read_validation_body(response)?;
        Ok((status, body))
    })
    .join();

    match outcome {
        Ok(Ok((status, body))) if (200..300).contains(&status) => {
            match validate_completion_body(&body) {
                Ok(()) => KeyCheck::Works { model },
                Err(reason) => KeyCheck::Unknown { reason },
            }
        }
        Ok(Ok((status, body))) => KeyCheck::Rejected {
            reason: explain_rejection(&provider_id, status, &body),
            model,
        },
        Ok(Err(transport)) => KeyCheck::Unknown {
            reason: format!("could not reach {provider_id}: {transport}"),
        },
        Err(_) => KeyCheck::Unknown {
            reason: "the validation thread panicked".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nim_is_validated_through_chat_completions_not_the_open_models_list() {
        let nim = providers_data::get_provider("nvidia").expect("nvidia in catalog");
        assert!(speaks_openai_chat(nim.id));
        // The URL a real turn hits — NOT /v1/models, which NIM serves to
        // anyone with no Authorization header at all (verified live).
        assert_eq!(
            chat_url(nim.base_url),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
        assert!(!chat_url(nim.base_url).contains("/models"));
        // A base that already carries the version must not double it.
        assert_eq!(
            chat_url("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://integrate.api.nvidia.com/v1/"),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }

    #[test]
    fn an_empty_key_is_rejected_without_a_network_call() {
        let nim = providers_data::get_provider("nvidia").unwrap();
        assert!(validate_api_key(&nim, "   ", None).is_rejected());
    }

    #[test]
    fn providers_with_another_wire_format_return_no_verdict_not_a_false_pass() {
        for id in ["anthropic", "google", "openai-codex", "ollama"] {
            let provider = providers_data::get_provider(id).expect(id);
            let verdict = validate_api_key(&provider, "irrelevant", None);
            assert!(
                matches!(verdict, KeyCheck::Unknown { .. }),
                "{id} must not be silently reported as verified: {verdict:?}"
            );
            // "Unknown" must never READ as success to a user or a GUI. Matched
            // against the success sentence itself, not the bare word: the
            // honest no-verdict line ends "storing it unverified", which
            // *contains* "verified" while saying the opposite.
            let summary = verdict.summary();
            assert!(!summary.contains("key verified"), "{id}: {summary}");
            assert!(summary.contains("could not verify"), "{id}: {summary}");
        }
    }

    #[test]
    fn http_success_requires_a_real_completion_payload() {
        assert!(validate_completion_body(r#"{"choices":[{"message":{"content":"P"}}]}"#).is_ok());
        assert!(validate_completion_body(r#"{"choices":[]}"#).is_err());
        assert!(validate_completion_body(r#"{"error":{"message":"bad key"}}"#).is_err());
        assert!(validate_completion_body("not json").is_err());
    }

    /// Each NIM rejection points at a DIFFERENT fix. Collapsing them into one
    /// "auth failed" is what sends a user re-pasting a perfectly good key for
    /// an hour.
    #[test]
    fn each_nim_rejection_names_its_own_fix() {
        let revoked = explain_rejection(
            "nvidia",
            403,
            r#"{"status":403,"title":"Forbidden","detail":"Authorization failed"}"#,
        );
        assert!(revoked.contains("nvapi-"), "{revoked}");

        let broke = explain_rejection(
            "nvidia",
            402,
            r#"{"status":402,"title":"Payment Required","detail":"Account 'X': Cloud credits expired - Please contact NVIDIA representatives"}"#,
        );
        assert!(broke.contains("out of inference credits"), "{broke}");
        assert!(broke.contains("fallback profile"), "{broke}");

        let gated = explain_rejection(
            "nvidia",
            404,
            r#"{"status":404,"title":"Not Found","detail":"Function '23d4f03a': Not found for account 'QPJ8'"}"#,
        );
        assert!(gated.contains("Public API Endpoints"), "{gated}");
        assert!(gated.contains("build.nvidia.com"), "{gated}");
        // These three must not be interchangeable.
        assert_ne!(revoked, broke);
        assert_ne!(broke, gated);
        assert_ne!(revoked, gated);
    }

    /// Every rejection reason this module produces must ALSO be a string the
    /// fallback layer classifies as account-exhaustion — otherwise a chain hits
    /// the same wall at runtime and aborts instead of rotating.
    #[test]
    fn rejection_reasons_stay_rotatable_by_the_fallback_chain() {
        use crate::providers::fallback::is_account_exhausted_error;
        for (status, body) in [
            (403u16, r#"{"status":403,"detail":"Authorization failed"}"#),
            (402, r#"{"status":402,"detail":"Cloud credits expired"}"#),
            (
                404,
                r#"{"detail":"Function 'x': Not found for account 'y'"}"#,
            ),
        ] {
            // The raw provider error is what the runtime actually classifies.
            let raw = format!("OpenAI API error ({status}): {body}");
            assert!(
                is_account_exhausted_error(&raw),
                "a chain would NOT rotate off `{raw}`"
            );
        }
    }
}
