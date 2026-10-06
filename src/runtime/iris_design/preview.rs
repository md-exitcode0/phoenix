//! Own the upstream local preview service; no model or browser is created here.

use super::*;
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};

const START_BUDGET: Duration = Duration::from_secs(35);
const STOP_BUDGET: Duration = Duration::from_secs(35);

#[derive(Debug)]
pub(super) struct StartFailure {
    detail: String,
    pub(super) cleanup_confirmed: bool,
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} (owned preview cleanup confirmed: {})", self.detail, self.cleanup_confirmed)
    }
}
impl std::error::Error for StartFailure {}

pub(crate) fn preview_permitted(mode: crate::tools::PermissionMode, kind: &str) -> bool {
    match kind {
        "static" => mode.allows(crate::tools::PermissionMode::Workspace),
        // Upstream command validation confines paths, but does not replace
        // Phoenix's Workspace shell sandbox. Never silently omit that boundary.
        "command" => mode.is_full_access(),
        _ => false,
    }
}

pub(super) struct OwnedPreview {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    plan: Value,
    pub(super) url: String,
}

impl OwnedPreview {
    pub(super) async fn start(bridge: &DesignBridge, workspace: &Path, plan: &Value, state: &Value) -> Result<Self> {
        ensure!(engine_hash(&bridge.root)? == bridge.hash, "Iris preview engine changed");
        let mut command = tokio::process::Command::new(&bridge.node);
        command.arg(bridge.root.join("scripts/iris-design-preview.mjs"))
            .current_dir(&bridge.root).stdin(Stdio::piped()).stdout(Stdio::piped())
            .stderr(Stdio::null()).kill_on_drop(false);
        let mut child = command.spawn().context("could not start the owned Iris preview service")?;
        let stdin = child.stdin.take().context("Iris preview service has no stdin")?;
        let stdout = child.stdout.take().context("Iris preview service has no stdout")?;
        // Install the lifetime guard before any cancellable I/O. EOF requests
        // upstream's exact server shutdown even if startup was interrupted.
        let mut service = Self { child: Some(child), stdin: Some(stdin), plan: plan.clone(), url: String::new() };
        let mut input = serde_json::to_vec(&json!({"workspace":workspace, "plan":plan,
            "expectedManifestSha256":state.get("manifestSha256")}))?;
        input.push(b'\n');
        let ready = async {
            service.stdin.as_mut().unwrap().write_all(&input).await?;
            service.stdin.as_mut().unwrap().flush().await?;
            let mut line = String::new();
            BufReader::new(stdout.take(65_537)).read_line(&mut line).await?;
            ensure!(line.ends_with('\n') && line.len() <= 65_536, "Iris preview returned no bounded readiness record");
            let ready: Value = serde_json::from_str(&line)?;
            ensure!(ready.get("ready").and_then(Value::as_bool) == Some(true),
                "Iris preview did not start: {}", ready.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown startup failure"));
            let url = ready.get("url").and_then(Value::as_str).context("Iris preview has no actual URL")?;
            let parsed = url::Url::parse(url)?;
            ensure!(parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1") && parsed.port().is_some()
                && parsed.username().is_empty() && parsed.password().is_none(), "Iris preview returned a nonlocal URL");
            ensure!(ready.get("viewports") == plan.get("viewports"), "Iris preview changed the validated viewport plan");
            Ok::<_, anyhow::Error>(url.to_string())
        };
        let readiness = tokio::time::timeout(START_BUDGET, ready).await
            .context("Iris preview startup did not confirm readiness within its budget")
            .and_then(|result| result);
        service.url = match readiness {
            Ok(url) => url,
            Err(error) => {
                service.stdin.take();
                // A startup error is resumable only after the helper's EOF
                // handler finishes upstream cleanup. An interrupted wait keeps
                // the native pending marker and cannot spawn a replacement.
                let cleanup_confirmed = match service.child.as_mut() {
                    Some(child) => matches!(tokio::time::timeout(STOP_BUDGET, child.wait()).await, Ok(Ok(_))),
                    None => false,
                };
                if cleanup_confirmed { service.child.take(); }
                return Err(StartFailure { detail: format!("{error:#}"), cleanup_confirmed }.into());
            }
        };
        Ok(service)
    }

    pub(super) fn matches(&mut self, plan: &Value) -> Result<bool> {
        Ok(self.plan == *plan && self.child.as_mut().context("Iris preview process is missing")?.try_wait()?.is_none())
    }

    pub(super) async fn stop(mut self) -> Result<()> {
        self.stdin.take();
        let mut child = self.child.take().context("Iris preview process is missing")?;
        match tokio::time::timeout(STOP_BUDGET, child.wait()).await {
            Ok(status) => { ensure!(status?.success(), "Iris preview shutdown reported a failure"); Ok(()) }
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                bail!("Iris preview shutdown exceeded its budget; descendant termination is unconfirmed")
            }
        }
    }
}

impl Drop for OwnedPreview {
    fn drop(&mut self) {
        self.stdin.take();
        let Some(mut child) = self.child.take() else { return; };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // Resource cleanup only. The flow has already stopped and this
            // task never advances a model, captures another viewport or retries.
            runtime.spawn(async move {
                match tokio::time::timeout(STOP_BUDGET, child.wait()).await {
                    Ok(Ok(status)) if status.success() => {}
                    other => {
                        tracing::warn!("Iris preview cleanup was not confirmed: {other:?}");
                        let _ = child.start_kill();
                        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                    }
                }
            });
        } else {
            // Closing the last stdin handle requests the upstream shutdown.
            // Do not SIGKILL its supervisor and strand its owned child group.
            tracing::warn!("Iris preview stdin closed after runtime shutdown; cleanup confirmation is unavailable");
        }
    }
}

fn retained() -> &'static Mutex<BTreeMap<String, OwnedPreview>> {
    static PREVIEWS: OnceLock<Mutex<BTreeMap<String, OwnedPreview>>> = OnceLock::new();
    PREVIEWS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn take_retained(key: &str) -> Option<OwnedPreview> {
    retained().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(key)
}

pub(super) fn retain(key: String, preview: OwnedPreview) -> Result<()> {
    let mut retained = retained().lock().map_err(|_| anyhow::anyhow!("Iris preview ownership registry is unavailable"))?;
    ensure!(retained.contains_key(&key) || retained.len() < 8, "Iris already owns eight retained previews; this preview was not retained");
    retained.insert(key, preview);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::PermissionMode::{FullAccess, Talk, Workspace};

    #[test]
    fn preview_posture_never_skips_workspace_command_isolation() {
        assert!(!preview_permitted(Talk, "static"));
        assert!(!preview_permitted(Talk, "command"));
        assert!(preview_permitted(Workspace, "static"));
        assert!(!preview_permitted(Workspace, "command"));
        assert!(preview_permitted(FullAccess, "static"));
        assert!(preview_permitted(FullAccess, "command"));
        assert!(!preview_permitted(FullAccess, "unvalidated"));
    }
}
