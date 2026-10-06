//! Shared transient-error retry helper for provider HTTP calls.
//!
//! Free/shared model pools (OpenRouter, OpenCode Zen, etc.) frequently return
//! 429 / 503 / "overloaded" responses under load. A single blip should not kill
//! the whole Phoenix turn, so retryable failures are retried with exponential
//! backoff plus jitter before the error is surfaced.

use std::future::Future;
use std::time::Duration;

/// Total attempts (1 initial + retries). Backoff between retries grows
/// exponentially: ~0.7s, ~1.4s, ~2.8s (plus jitter).
const MAX_ATTEMPTS: u32 = 4;
const BASE_BACKOFF_MS: u64 = 700;
const MAX_JITTER_MS: u64 = 300;

/// Returns true when a provider error string indicates a transient condition
/// that is worth retrying (rate limits, temporary upstream unavailability).
pub fn is_retryable_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("429")
        || m.contains("too many requests")
        || m.contains("rate limit")
        || m.contains("rate-limit")
        || m.contains("503")
        || m.contains("service unavailable")
        || m.contains("overloaded")
        || m.contains("temporarily unavailable")
}

/// Cheap, dependency-free jitter derived from the system clock nanos.
fn jitter_ms() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (MAX_JITTER_MS + 1)
}

/// Run an async provider operation, retrying transient failures with backoff.
///
/// `op` is re-invoked on each attempt, so it must be cheap to reconstruct the
/// request (e.g. borrow the request and clone the body internally).
pub async fn with_retry<T, F, Fut>(mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS || !is_retryable_error(&err.to_string()) {
                    return Err(err);
                }
                let backoff = BASE_BACKOFF_MS * 2u64.pow(attempt - 1) + jitter_ms();
                tokio::time::sleep(Duration::from_millis(backoff)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn detects_retryable_errors() {
        assert!(is_retryable_error(
            "OpenCode chat API error (429 Too Many Requests)"
        ));
        assert!(is_retryable_error("Rate limited by Xiaomi (shared pool)"));
        assert!(is_retryable_error("upstream 503 Service Unavailable"));
        assert!(is_retryable_error("Model overloaded, try again"));
        assert!(!is_retryable_error("OpenCode API error (400): bad request"));
        assert!(!is_retryable_error("invalid api key (401)"));
    }

    #[tokio::test]
    async fn retries_until_success() {
        let calls = AtomicU32::new(0);
        let result: anyhow::Result<u32> = with_retry(|| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n < 2 {
                    anyhow::bail!("429 Too Many Requests");
                }
                Ok(n)
            }
        })
        .await;
        assert_eq!(result.unwrap(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn does_not_retry_non_retryable() {
        let calls = AtomicU32::new(0);
        let result: anyhow::Result<u32> = with_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { anyhow::bail!("API error (400): bad request") }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
