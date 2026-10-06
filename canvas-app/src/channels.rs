//! Thin desktop controls for the CLI channel worker. Tokens travel only over
//! stdin; optional remembered logins stay in the existing encrypted vault.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroizing;

static WORKERS: OnceLock<Mutex<HashMap<String, Child>>> = OnceLock::new();
fn workers() -> &'static Mutex<HashMap<String, Child>> {
    WORKERS.get_or_init(Default::default)
}
fn path(id: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid connection ID".into());
    }
    Ok(super::phoenix_home()
        .join("channels/connections")
        .join(format!("{id}.json")))
}
fn cli(args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let mut command = Command::new(super::phoenix_binary());
    command.args(args);
    let result = super::run_command_bounded(
        &mut command,
        input,
        std::time::Duration::from_secs(75),
        "channel settings",
    )?;
    if result.status.success() {
        Ok(String::from_utf8_lossy(&result.stdout).into())
    } else {
        Err(String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(2000)
            .collect())
    }
}
fn json_cli(args: &[&str], input: Option<&[u8]>) -> Result<Value, String> {
    serde_json::from_str(&cli(args, input)?)
        .map_err(|_| "Phoenix returned unreadable channel settings".into())
}
const SUMMARY_LIST_ARGS: &[&str] = &["channels", "list", "--summary"];
fn summary_status_args(path: &str) -> [&str; 4] { ["channels", "status", path, "--summary"] }
fn validate_snapshot(snapshot: Option<&str>, required: bool) -> Result<(), String> {
    if required && snapshot.is_none() {
        return Err("Refresh the recovery list before resolving this item".into());
    }
    if snapshot.is_some_and(|value| value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())) {
        return Err("Invalid recovery snapshot. Refresh the recovery list".into());
    }
    Ok(())
}
fn review_cli_args(path: &str, kind: &str, offset: usize, limit: usize, snapshot: Option<&str>) -> Result<Vec<String>, String> {
    if !matches!(kind, "turns" | "deliveries") || !(1..=100).contains(&limit) || offset > 64 * 1024 * 1024 {
        return Err("Choose a valid recovery page of 1–100 records".into());
    }
    validate_snapshot(snapshot, false)?;
    let mut args = vec!["channels".into(), "review-page".into(), path.into(), "--kind".into(), kind.into(),
        "--offset".into(), offset.to_string(), "--limit".into(), limit.to_string()];
    if let Some(snapshot) = snapshot { args.extend(["--snapshot".into(), snapshot.into()]); }
    Ok(args)
}
fn stop_child(mut child: Child) {
    // This PID is still owned by Child; it is not a persisted PID that could
    // belong to an unrelated process. Only workers started here are stopped.
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGINT);
    }
    std::thread::spawn(move || {
        for _ in 0..240 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let _ = child.kill();
        let _ = child.wait();
    });
}
pub fn shutdown() {
    if let Ok(mut map) = workers().lock() {
        for (_, child) in map.drain() {
            stop_child(child);
        }
    }
}
#[tauri::command]
pub async fn channels_command(
    action: String,
    id: Option<String>,
    config: Option<Value>,
    token: Option<String>,
    value: Option<String>,
    kind: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    snapshot: Option<String>,
) -> Result<Value, String> {
    // Wrap the incoming secret before any fallible operation; never echo it.
    let token = Zeroizing::new(token.unwrap_or_default());
    tauri::async_runtime::spawn_blocking(move || {
        if action == "list" {
            let mut result = json_cli(SUMMARY_LIST_ARGS, None)?;
            let mut map = workers()
                .lock()
                .map_err(|_| "Channel controls are unavailable")?;
            let mut ended = Vec::new();
            for (id, child) in map.iter_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    ended.push((id.clone(), status.success()));
                }
            }
            if let Some(rows) = result["connections"].as_array_mut() {
                for row in rows {
                    let id = row["config"]["id"].as_str().unwrap_or("").to_string();
                    row["desktop_worker"] =
                        json!(map.contains_key(&id) && !ended.iter().any(|(key, _)| key == &id));
                    if ended.iter().any(|(key, ok)| key == &id && !ok) {
                        row["worker_error"] = json!(
                            "Connection stopped. Check the gateway and bot login, then reconnect."
                        );
                    }
                }
            }
            return Ok(result);
        }
        if action == "save" {
            let data = serde_json::to_vec(&config.ok_or("Connection settings are missing")?)
                .map_err(|_| "Invalid settings")?;
            return json_cli(&["channels", "save"], Some(&data));
        }
        let id = id.ok_or("Choose a connection")?;
        let path = path(&id)?;
        let path = path.to_str().ok_or("Invalid connection path")?;
        match action.as_str() {
            "review" => {
                let args = review_cli_args(path, kind.as_deref().ok_or("Choose replies or turns")?,
                    offset.ok_or("Choose a page offset")?, limit.unwrap_or(10), snapshot.as_deref())?;
                json_cli(&args.iter().map(String::as_str).collect::<Vec<_>>(), None)
            }
            "check" | "remember" => {
                let mut args = vec!["channels", "check", path];
                let input = if token.trim().is_empty() {
                    None
                } else {
                    args.push("--token-stdin");
                    Some(token.as_bytes())
                };
                cli(&args, input)?;
                if action == "remember" {
                    if token.trim().is_empty() {
                        return Err("Enter the bot token to remember".into());
                    }
                    cli(
                        &["channels", "remember-token", path],
                        Some(token.as_bytes()),
                    )?;
                }
                Ok(json!({"ok":true}))
            }
            "forget" => {
                cli(&["channels", "forget-token", path], None)?;
                Ok(json!({"forgotten":true}))
            }
            "start" => {
                // Read-only credential verification happens off the UI thread.
                let mut args = vec!["channels", "check", path];
                let supplied = !token.trim().is_empty();
                if supplied {
                    args.push("--token-stdin");
                }
                cli(&args, supplied.then_some(token.as_bytes()))?;
                let mut map = workers()
                    .lock()
                    .map_err(|_| "Channel controls are unavailable")?;
                if let Some(child) = map.get_mut(&id) {
                    if child
                        .try_wait()
                        .map_err(|_| "Could not check this worker")?
                        .is_none()
                    {
                        return Err("This connection is already running".into());
                    }
                }
                map.remove(&id);
                let status = json_cli(&summary_status_args(path), None)?;
                if status["running"].as_bool() == Some(true) {
                    return Err(
                        "This connection is running outside the desktop. Stop that worker first."
                            .into(),
                    );
                }
                let mut command = Command::new(super::phoenix_binary());
                command.args(["channels", "run", path]);
                if supplied {
                    command.arg("--token-stdin");
                }
                command
                    .stdin(if supplied {
                        Stdio::piped()
                    } else {
                        Stdio::null()
                    })
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let mut child = command
                    .spawn()
                    .map_err(|_| "Could not start the channel worker")?;
                let written = if supplied {
                    child
                        .stdin
                        .take()
                        .ok_or("Could not open the worker input")
                        .and_then(|mut pipe| {
                            pipe.write_all(token.as_bytes())
                                .map_err(|_| "Could not send the token to the worker")
                        })
                } else {
                    Ok(())
                };
                if let Err(error) = written {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
                map.insert(id, child);
                Ok(json!({"started":true}))
            }
            "stop" => {
                let child = workers()
                    .lock()
                    .map_err(|_| "Channel controls are unavailable")?
                    .remove(&id)
                    .ok_or(
                        "This worker was not started by this desktop. Stop it in its terminal.",
                    )?;
                stop_child(child);
                Ok(json!({"stopping":true}))
            }
            "remove" => {
                cli(&["channels", "remove", &id], None)?;
                Ok(json!({"removed":true}))
            }
            "acknowledge" | "received" | "retry" => {
                validate_snapshot(snapshot.as_deref(), true)?;
                let value = value.ok_or("Choose the interrupted turn or reply")?;
                if !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || value.is_empty() || value.len() > 128
                {
                    return Err("Invalid recovery ID".into());
                }
                let mut args = if action == "acknowledge" {
                    vec!["channels", "acknowledge-turn", path, &value]
                } else {
                    vec!["channels", "resolve-delivery", path, &value]
                };
                if action == "retry" {
                    args.push("--retry");
                }
                if let Some(snapshot) = snapshot.as_deref() { args.extend(["--snapshot", snapshot]); }
                cli(&args, None)?;
                Ok(json!({"resolved":true}))
            }
            _ => Err("Unknown channel action".into()),
        }
    })
    .await
    .map_err(|_| "Channel operation was interrupted".to_string())?
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn channel_native_review_routes_only_bounded_reads() {
        assert_eq!(SUMMARY_LIST_ARGS, ["channels", "list", "--summary"]);
        assert_eq!(summary_status_args("connection.json"), ["channels", "status", "connection.json", "--summary"]);
        let snapshot = "a".repeat(64);
        let args = review_cli_args("connection.json", "deliveries", 155330, 10, Some(&snapshot)).unwrap();
        assert_eq!(args, ["channels", "review-page", "connection.json", "--kind", "deliveries", "--offset", "155330", "--limit", "10", "--snapshot", &snapshot]);
        for (kind, offset, limit, snapshot) in [("other",0,10,None),("turns",0,101,None),("deliveries",0,0,None),("turns",usize::MAX,10,None),("turns",0,10,Some("bad"))] {
            assert!(review_cli_args("connection.json",kind,offset,limit,snapshot).is_err());
        }
        assert!(validate_snapshot(None,true).is_err());
        assert!(validate_snapshot(Some(&"A".repeat(64)),true).is_err());
        assert!(validate_snapshot(Some(&snapshot),true).is_ok());
    }
}
