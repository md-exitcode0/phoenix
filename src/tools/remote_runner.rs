//! User-owned remote execution targets.
//!
//! Runners are deliberately boring SSH endpoints: the user pre-provisions a
//! matching workspace, pins the exact host key, and chooses an optional
//! identity file. Phoenix never accepts trust-on-first-use, password prompts,
//! agent forwarding, or an agent-invented destination.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const RUNNERS_VERSION: u32 = 1;
const RUNNERS_MAX_BYTES: usize = 256 * 1024;
const RUNNERS_CAP: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRunner {
    pub id: String,
    pub label: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub workspace_root: String,
    /// OpenSSH known-host value: `<algorithm> <base64-key>`.
    pub host_key: String,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteRunnerStore {
    version: u32,
    runners: Vec<RemoteRunner>,
}

fn default_port() -> u16 {
    22
}

fn default_enabled() -> bool {
    true
}

fn runners_path() -> PathBuf {
    crate::config::phoenix_home().join("runners.json")
}

fn known_hosts_path(id: &str) -> PathBuf {
    crate::config::phoenix_home()
        .join("runners/known-hosts")
        .join(format!("{id}.known_hosts"))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_runner(runner: &RemoteRunner) -> Result<()> {
    anyhow::ensure!(safe_component(&runner.id), "invalid remote runner id");
    anyhow::ensure!(
        !runner.label.trim().is_empty() && runner.label.len() <= 120,
        "remote runner label must be 1..=120 bytes"
    );
    anyhow::ensure!(
        safe_component(&runner.host) && !runner.host.starts_with('.'),
        "remote runner host must be a DNS name or IPv4 address without shell syntax"
    );
    anyhow::ensure!(safe_component(&runner.user), "invalid remote runner user");
    anyhow::ensure!(runner.port > 0, "remote runner port must be non-zero");
    let workspace = Path::new(&runner.workspace_root);
    anyhow::ensure!(
        workspace.is_absolute()
            && !runner.workspace_root.chars().any(char::is_control)
            && !workspace
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
        "remote runner workspace_root must be a clean absolute path"
    );
    let mut host_key = runner.host_key.split_whitespace();
    let algorithm = host_key.next().unwrap_or_default();
    let encoded = host_key.next().unwrap_or_default();
    anyhow::ensure!(
        matches!(
            algorithm,
            "ssh-ed25519" | "ecdsa-sha2-nistp256" | "rsa-sha2-512" | "rsa-sha2-256" | "ssh-rsa"
        ) && encoded.len() >= 40
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
            && host_key.next().is_none(),
        "remote runner host_key must be one exact OpenSSH algorithm/key pair"
    );
    if let Some(identity) = runner.identity_file.as_deref() {
        validate_identity_file(Path::new(identity))?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_identity_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    anyhow::ensure!(path.is_absolute(), "runner identity_file must be absolute");
    crate::config::private_io::reject_symlink_components(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect runner identity file {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "runner identity_file must be a same-user, non-symlinked private file (0600)"
    );
    Ok(())
}

#[cfg(not(unix))]
fn validate_identity_file(_path: &Path) -> Result<()> {
    anyhow::bail!("remote runners are supported only on Unix")
}

pub fn list() -> Result<Vec<RemoteRunner>> {
    let Some(bytes) =
        crate::config::private_io::read_private_file_limited(&runners_path(), RUNNERS_MAX_BYTES)?
    else {
        return Ok(Vec::new());
    };
    let store: RemoteRunnerStore =
        serde_json::from_slice(&bytes).context("remote runner registry is invalid JSON")?;
    anyhow::ensure!(
        store.version == RUNNERS_VERSION,
        "remote runner registry version mismatch"
    );
    anyhow::ensure!(
        store.runners.len() <= RUNNERS_CAP,
        "remote runner registry exceeds {RUNNERS_CAP} targets"
    );
    let mut seen = std::collections::HashSet::new();
    for runner in &store.runners {
        validate_runner(runner)?;
        anyhow::ensure!(
            seen.insert(runner.id.as_str()),
            "duplicate remote runner id"
        );
    }
    Ok(store.runners)
}

pub fn find_enabled(id: &str) -> Result<RemoteRunner> {
    anyhow::ensure!(safe_component(id), "invalid remote runner id");
    let runner = list()?
        .into_iter()
        .find(|runner| runner.id == id)
        .with_context(|| format!("remote runner `{id}` is not configured"))?;
    anyhow::ensure!(runner.enabled, "remote runner `{id}` is disabled");
    Ok(runner)
}

fn root_owned_ssh() -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        for candidate in ["/usr/bin/ssh", "/bin/ssh", "/usr/local/bin/ssh"] {
            let path = PathBuf::from(candidate);
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_file()
                && metadata.uid() == 0
                && metadata.permissions().mode() & 0o111 != 0
            {
                return Ok(path);
            }
        }
    }
    anyhow::bail!("a root-owned OpenSSH client was not found")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_cwd(runner: &RemoteRunner, relative_cwd: &Path) -> Result<String> {
    anyhow::ensure!(
        !relative_cwd.is_absolute()
            && !relative_cwd
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
        "remote runner cwd escaped the workspace"
    );
    let path = Path::new(&runner.workspace_root).join(relative_cwd);
    Ok(path.to_string_lossy().into_owned())
}

pub fn ssh_command(
    runner: &RemoteRunner,
    relative_cwd: &Path,
    user_command: &str,
) -> Result<Command> {
    validate_runner(runner)?;
    let ssh = root_owned_ssh()?;
    let known_hosts = known_hosts_path(&runner.id);
    let host_pattern = if runner.port == 22 {
        runner.host.clone()
    } else {
        format!("[{}]:{}", runner.host, runner.port)
    };
    let known_host_line = format!("{host_pattern} {}\n", runner.host_key);
    crate::config::private_io::atomic_write_private(&known_hosts, known_host_line.as_bytes())?;
    let cwd = remote_cwd(runner, relative_cwd)?;
    let remote = format!(
        "cd -- {} && exec /usr/bin/env bash -lc {}",
        shell_quote(&cwd),
        shell_quote(user_command)
    );
    let mut command = Command::new(ssh);
    command
        .arg("-T")
        .args(["-o", "BatchMode=yes"])
        .args(["-o", "StrictHostKeyChecking=yes"])
        .args(["-o", "CheckHostIP=yes"])
        .args(["-o", "IdentitiesOnly=yes"])
        .args(["-o", "ForwardAgent=no"])
        .args(["-o", "ForwardX11=no"])
        .args(["-o", "PermitLocalCommand=no"])
        .args(["-o", "ClearAllForwardings=yes"])
        .args(["-o", "ConnectTimeout=12"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .arg("-p")
        .arg(runner.port.to_string());
    if let Some(identity) = runner.identity_file.as_deref() {
        command.arg("-i").arg(identity);
    }
    command
        .arg(format!("{}@{}", runner.user, runner.host))
        .arg(remote)
        .env("GIT_TERMINAL_PROMPT", "0");
    Ok(command)
}

pub fn summary_json() -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "runners": list()?.into_iter().map(|runner| serde_json::json!({
            "id": runner.id,
            "label": runner.label,
            "host": runner.host,
            "port": runner.port,
            "user": runner.user,
            "workspace_root": runner.workspace_root,
            "enabled": runner.enabled,
            "host_key_pinned": true,
            "identity_file_configured": runner.identity_file.is_some(),
        })).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_quotes_workspace_and_payload_without_trusting_shell_syntax() {
        let runner = RemoteRunner {
            id: "build-1".into(),
            label: "Build machine".into(),
            host: "build.example.com".into(),
            port: 2222,
            user: "phoenix".into(),
            workspace_root: "/srv/work trees/project".into(),
            host_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG4p5lO8wI6FhL9G5D8Rz4uHn8yY9eB1cQ2xV3zW4a5b"
                    .into(),
            identity_file: None,
            enabled: true,
        };
        validate_runner(&runner).unwrap();
        assert_eq!(
            remote_cwd(&runner, Path::new("packages/app")).unwrap(),
            "/srv/work trees/project/packages/app"
        );
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert!(remote_cwd(&runner, Path::new("../escape")).is_err());
    }
}
