use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tiny_http::{Response, Server};
use url::Url;

const MAX_REDIRECT_URL_BYTES: usize = 8 * 1024;
const MAX_CALLBACK_TARGET_BYTES: usize = 16 * 1024;
const MAX_OAUTH_VALUE_BYTES: usize = 64 * 1024;
const MAX_CALLBACK_WAIT: Duration = Duration::from_secs(15 * 60);

fn parse_loopback_redirect(url: &str) -> Result<(String, u16, String)> {
    if url.len() > MAX_REDIRECT_URL_BYTES {
        anyhow::bail!("OAuth redirect URL is oversized");
    }
    let parsed = Url::parse(url).context("Invalid redirect URL")?;
    if parsed.scheme() != "http"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        anyhow::bail!("OAuth redirect must be a plain HTTP loopback URL");
    }
    let host = parsed.host_str().context("Redirect URL missing host")?;
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        anyhow::bail!("OAuth redirect host must be localhost or a loopback address");
    }
    let port = parsed
        .port()
        .context("Redirect URL needs an explicit port")?;
    if port < 1024 {
        anyhow::bail!("OAuth redirect port must be unprivileged");
    }
    let expected_path = parsed.path();
    if expected_path.is_empty() || expected_path.len() > 1024 {
        anyhow::bail!("OAuth redirect path is empty or oversized");
    }
    Ok((host.to_string(), port, expected_path.to_string()))
}

pub fn wait_for_oauth_code_on(url: &str, timeout: Duration) -> Result<(String, String)> {
    let (host, port, expected_path) = parse_loopback_redirect(url)?;
    let timeout = timeout.min(MAX_CALLBACK_WAIT);
    let listener = TcpListener::bind((host.as_str(), port))
        .context("Failed to bind localhost callback listener")?;
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("Failed to start callback server: {e}"))?;
    let started = Instant::now();

    loop {
        if started.elapsed() > timeout {
            anyhow::bail!("OAuth callback timed out");
        }
        if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(250)) {
            if req.url().len() > MAX_CALLBACK_TARGET_BYTES {
                let _ = req.respond(
                    Response::from_string("Request target too long").with_status_code(414),
                );
                continue;
            }
            let authority = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.clone()
            };
            let full_url = format!("http://{authority}:{port}{}", req.url());
            let parsed = Url::parse(&full_url).context("Invalid callback URL")?;
            if parsed.path() != expected_path {
                let _ = req.respond(Response::from_string("Not found").with_status_code(404));
                continue;
            }
            let code = parsed
                .query_pairs()
                .find_map(|(k, v)| (k == "code").then(|| v.to_string()));
            let state = parsed
                .query_pairs()
                .find_map(|(k, v)| (k == "state").then(|| v.to_string()));
            let msg = if code.is_some() {
                "Authorization received. You can close this tab."
            } else {
                "Missing authorization code."
            };
            let _ = req.respond(Response::from_string(msg));
            if let (Some(c), Some(s)) = (code, state) {
                if c.is_empty()
                    || s.is_empty()
                    || c.len() > MAX_OAUTH_VALUE_BYTES
                    || s.len() > MAX_OAUTH_VALUE_BYTES
                {
                    anyhow::bail!("OAuth callback code/state is empty or oversized");
                }
                return Ok((c, s));
            }
            anyhow::bail!("OAuth callback missing code/state");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_listener_accepts_only_explicit_unprivileged_loopback_urls() {
        assert!(parse_loopback_redirect("http://127.0.0.1:1455/callback").is_ok());
        assert!(parse_loopback_redirect("http://localhost:56121/callback").is_ok());
        assert!(parse_loopback_redirect("https://localhost:1455/callback").is_err());
        assert!(parse_loopback_redirect("http://0.0.0.0:1455/callback").is_err());
        assert!(parse_loopback_redirect("http://example.com:1455/callback").is_err());
        assert!(parse_loopback_redirect("http://localhost/callback").is_err());
        assert!(parse_loopback_redirect("http://localhost:80/callback").is_err());
        assert!(parse_loopback_redirect("http://localhost:1455/callback?prebound=1").is_err());
    }
}

pub fn wait_for_oauth_code(timeout: Duration) -> Result<(String, String)> {
    wait_for_oauth_code_on("http://localhost:1455/auth/callback", timeout)
}

pub fn random_localhost_redirect() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").context("Failed to reserve localhost port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://localhost:{port}/callback"))
}
