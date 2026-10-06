//! Session debug logging (NDJSON) for active debug investigations.
//!
//! Opt-in: set `PHOENIX_DEBUG_LOG=/path/to/file.ndjson` to capture. Without
//! the env var this is a no-op — no hardcoded paths, nothing written.

use std::io::Write;

pub fn log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    let Ok(path) = std::env::var("PHOENIX_DEBUG_LOG") else {
        return;
    };
    if path.trim().is_empty() {
        return;
    }
    let line = serde_json::json!({
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    });
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    if let Ok(mut file) = options.open(path.trim()) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        let _ = writeln!(file, "{line}");
    }
}
