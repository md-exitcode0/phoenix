//! Fast-apply edit rescue (donor: morphllm Fast Apply — a small
//! purpose-trained merge model that applies "lazy edit" snippets to a file at
//! ~10k tok/s).
//!
//! Phoenix uses it in exactly one place: when `str_replace`'s anchored match
//! MISSES (the file drifted since the agent read it — the brittleness the
//! donor exists to solve), and a merge endpoint is configured, the intended
//! edit is merged by the model instead of failing the tool call. Absent
//! config = feature off, byte-identical behavior.
//!
//! Config lives in `~/.phoenix/fast_apply.toml` (same self-contained pattern
//! as the composio keys — no config.toml loader churn):
//!
//! ```toml
//! base_url = "https://api.morphllm.com/v1"   # any OpenAI-compatible host
//! api_key  = "sk-..."
//! model    = "morph-v3-fast"
//! ```

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

const FAST_APPLY_INPUT_MAX_BYTES: usize = 16 * 1024 * 1024;
const FAST_APPLY_RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;
const FAST_APPLY_CONFIG_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct FastApplyConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// The configured merge endpoint, or None (feature off).
pub fn load_config() -> Option<FastApplyConfig> {
    match load_config_result() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("fast-apply config is unusable: {error:#}");
            None
        }
    }
}

fn load_config_result() -> Result<Option<FastApplyConfig>> {
    // Tests must never pick up the LIVE machine's endpoint — an old_str-miss
    // test would silently turn into a real HTTP call.
    if crate::config::test_isolated_from_live_home() {
        return Ok(None);
    }
    let path = crate::config::phoenix_home().join("fast_apply.toml");
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&path, FAST_APPLY_CONFIG_MAX_BYTES)?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    let cfg: FastApplyConfig = toml::from_str(&text)
        .with_context(|| format!("{} is not valid fast-apply TOML", path.display()))?;
    if cfg.base_url.trim().is_empty() || cfg.model.trim().is_empty() {
        bail!("fast-apply base_url and model must be non-empty");
    }
    if cfg.base_url.len() > 2_048 || cfg.model.len() > 256 || cfg.api_key.len() > 16 * 1024 {
        bail!("fast-apply config contains an oversized field");
    }
    let url = reqwest::Url::parse(cfg.base_url.trim()).context("invalid fast-apply base_url")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("fast-apply base_url must use http or https");
    }
    if cfg.api_key.chars().any(|character| character.is_control()) {
        bail!("fast-apply api_key must not contain control characters");
    }
    Ok(Some(cfg))
}

/// The donor's wire format: one user message carrying instruction + original
/// code + the lazy update snippet, each in its own tag.
pub fn build_user_message(original: &str, old_str: &str, new_str: &str) -> String {
    format!(
        "<instruction>Apply one edit. The section that previously read exactly:\n{old_str}\n\
         must now read as in the update snippet. Every other byte of the file stays \
         identical. Output the complete merged file, nothing else.</instruction>\n\
         <code>{original}</code>\n\
         <update>// ... existing code ...\n{new_str}\n// ... existing code ...</update>"
    )
}

/// Pull the merged file out of an OpenAI-compatible chat response.
pub fn extract_merged(response: &serde_json::Value) -> Result<String> {
    response
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("fast-apply response carried no message content"))
}

/// Conservative acceptance gate — a merge model output that fails ANY of
/// these is discarded and the original str_replace error stands. Never let
/// a rescue lane become a corruption lane.
pub fn sanity_check(original: &str, merged: &str, new_str: &str) -> Result<()> {
    if merged.trim().is_empty() {
        bail!("merged file is empty");
    }
    if merged == original {
        bail!("merge produced no change");
    }
    if !new_str.trim().is_empty() && !merged.contains(new_str.trim()) {
        bail!("merged file does not contain the intended new text");
    }
    let (olen, mlen) = (original.len(), merged.len());
    if mlen < olen / 2 {
        bail!("merged file shrank by more than half ({olen} → {mlen} bytes)");
    }
    if mlen > olen.saturating_mul(2) + new_str.len() + 4096 {
        bail!("merged file more than doubled ({olen} → {mlen} bytes)");
    }
    Ok(())
}

/// Merge the intended edit into `original` via the configured endpoint.
/// Errors when unconfigured, on transport failure, or when the sanity gate
/// rejects the output — callers keep their own failure path.
pub fn rescue_replace(original: &str, old_str: &str, new_str: &str) -> Result<String> {
    let input_bytes = original
        .len()
        .saturating_add(old_str.len())
        .saturating_add(new_str.len());
    if input_bytes > FAST_APPLY_INPUT_MAX_BYTES {
        bail!("fast-apply input is {input_bytes} bytes; maximum is {FAST_APPLY_INPUT_MAX_BYTES}");
    }
    let cfg = load_config_result()?.ok_or_else(|| anyhow!("fast-apply not configured"))?;
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [{"role": "user", "content": build_user_message(original, old_str, new_str)}],
        "temperature": 0.0,
    });
    // Sync tool context inside an async runtime: same scoped-thread block_on
    // shape as tools::run_async.
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow!("fast-apply requires a tokio runtime"))?;
    let response: serde_json::Value = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                handle.block_on(async {
                    let client = reqwest::Client::builder()
                        .connect_timeout(std::time::Duration::from_secs(10))
                        .timeout(std::time::Duration::from_secs(60))
                        .build()
                        .context("build fast-apply HTTP client")?;
                    let mut resp = client
                        .post(format!(
                            "{}/chat/completions",
                            cfg.base_url.trim_end_matches('/')
                        ))
                        .bearer_auth(&cfg.api_key)
                        .json(&body)
                        .send()
                        .await
                        .context("fast-apply request failed")?;
                    let status = resp.status();
                    if resp
                        .content_length()
                        .is_some_and(|length| length > FAST_APPLY_RESPONSE_MAX_BYTES as u64)
                    {
                        bail!(
                            "fast-apply response exceeds the {FAST_APPLY_RESPONSE_MAX_BYTES}-byte limit"
                        );
                    }
                    let mut bytes = Vec::new();
                    while let Some(chunk) = resp
                        .chunk()
                        .await
                        .context("read fast-apply response body")?
                    {
                        if bytes.len().saturating_add(chunk.len())
                            > FAST_APPLY_RESPONSE_MAX_BYTES
                        {
                            bail!(
                                "fast-apply response exceeds the {FAST_APPLY_RESPONSE_MAX_BYTES}-byte limit"
                            );
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    let text = String::from_utf8(bytes)
                        .context("fast-apply response was not valid UTF-8")?;
                    if !status.is_success() {
                        bail!(
                            "fast-apply endpoint returned {status}: {}",
                            text.chars().take(400).collect::<String>()
                        );
                    }
                    serde_json::from_str::<serde_json::Value>(&text)
                        .context("fast-apply response was not JSON")
                })
            })
            .join()
            .unwrap_or_else(|_| Err(anyhow!("fast-apply worker thread panicked")))
    })?;
    let merged = extract_merged(&response)?;
    sanity_check(original, &merged, new_str)?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_carries_all_three_tags() {
        let msg = build_user_message("fn a() {}", "fn a()", "fn b()");
        assert!(msg.contains("<instruction>"));
        assert!(msg.contains("<code>fn a() {}</code>"));
        assert!(msg.contains("<update>"));
        assert!(msg.contains("// ... existing code ..."));
        assert!(msg.contains("fn b()"));
    }

    #[test]
    fn extract_merged_reads_openai_shape() {
        let response = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "merged file"}}]
        });
        assert_eq!(extract_merged(&response).unwrap(), "merged file");
        assert!(extract_merged(&serde_json::json!({"choices": []})).is_err());
    }

    #[test]
    fn sanity_gate_rejects_corruption_shapes() {
        let original = "line one\nline two\nline three\n";
        // Good merge passes.
        let good = "line one\nline 2\nline three\n";
        assert!(sanity_check(original, good, "line 2").is_ok());
        // Empty, unchanged, missing-new-text, and gross size changes fail.
        assert!(sanity_check(original, "", "x").is_err());
        assert!(sanity_check(original, original, "x").is_err());
        assert!(sanity_check(original, "line one\n", "line 2").is_err());
        // The doubling gate carries a 4KB slack for small files — prove it
        // on a file where the slack can't mask a runaway merge.
        let big = "a reasonable line of code\n".repeat(1000);
        let runaway = big.repeat(4);
        assert!(sanity_check(&big, &runaway, "a reasonable line").is_err());
    }

    #[test]
    fn unconfigured_rescue_fails_closed() {
        // Test builds see no config even when the live machine has one — the
        // rescue lane refuses and callers keep their original error path.
        assert!(load_config().is_none());
        let err = rescue_replace("original", "old", "new").unwrap_err();
        assert!(err.to_string().contains("not configured"), "{err:#}");
    }

    #[test]
    fn oversized_merge_is_rejected_before_config_or_network() {
        let original = "x".repeat(FAST_APPLY_INPUT_MAX_BYTES + 1);
        let err = rescue_replace(&original, "x", "y").unwrap_err();
        assert!(err.to_string().contains("maximum"), "{err:#}");
    }
}
