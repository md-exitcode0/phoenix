//! PhoenixAgent Configuration System
//!
//! Config file location:
//! `~/.phoenix/config.toml`
//!
//! Profile system supports:
//! - Main model + fallback chain (e.g., glm1/glm-5.1 -> glm2/glm-5.1)
//! - Multiple provider types: LLM, search, crawl, scrape
//! - OAuth/API key authentication
//! - Per-agent overrides

pub mod auth_profile;
pub mod check;
pub mod efficiency;
pub mod loader;
pub mod paths;
pub(crate) mod private_io;
pub mod setup;
pub mod types;
pub mod web_auth;
pub mod web_providers_data;

pub use check::run_check;
pub use efficiency::{
    apply_codex_subscription_defaults, codex_subscription_defaults_active,
    codex_subscription_defaults_applicable, recommended_lane_effort, recommended_lane_model,
    CodexEfficiencyMigration, CODEX_PHOENIX_MODEL, CODEX_TEAM_EFFORT, CODEX_TEAM_MODEL,
};
pub use loader::ConfigLoader;
pub use paths::{
    ensure_phoenix_home, migrate_legacy_state, phoenix_cognee_root, phoenix_vitals_path,
    phoenix_workspace_root,
};
pub use setup::{run_configure, run_setup};
pub use types::*;
pub use web_auth::{format_web_auth_toml_block, resolve_web_api_key, store_web_api_key_profile};

/// One process-wide guard for tests that redirect Phoenix's state root.
/// Module-local mutexes do not protect against tests in other modules; without
/// this shared guard a config test can make a browser/auth test read or write
/// the wrong temporary home (or, worse, briefly fall back to the live home).
#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard};

    static PHOENIX_HOME_ENV: Mutex<()> = Mutex::new(());

    pub(crate) struct PhoenixHomeGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl PhoenixHomeGuard {
        pub(crate) fn set(path: &Path) -> Self {
            let lock = PHOENIX_HOME_ENV
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var_os("PHOENIX_HOME");
            std::env::set_var("PHOENIX_HOME", path);
            Self {
                previous,
                _lock: lock,
            }
        }

        /// Point PHOENIX_HOME at an existing test directory after making that
        /// directory satisfy the same owner-only invariant production custom
        /// homes require. `tempfile::tempdir()` follows the process umask and
        /// is commonly 0755, which must not make otherwise isolated tests fail
        /// (or tempt production code to weaken its root validation).
        pub(crate) fn set_private(path: &Path) -> Self {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .expect("make isolated PHOENIX_HOME owner-only");
            }
            Self::set(path)
        }

        pub(crate) fn unset() -> Self {
            let lock = PHOENIX_HOME_ENV
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var_os("PHOENIX_HOME");
            std::env::remove_var("PHOENIX_HOME");
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for PhoenixHomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("PHOENIX_HOME", value),
                None => std::env::remove_var("PHOENIX_HOME"),
            }
        }
    }
}

use std::path::PathBuf;

#[derive(Clone)]
pub struct PhoenixConfig {
    pub profile: Profile,
}

/// The unified Phoenix home directory — `~/.phoenix` by default.
///
/// All Phoenix-owned state lives here, independent of the current working
/// directory: `config.toml`, `auth-profiles.json`, `memory/`, `sessions/`,
/// `session_cache/`, `runs/`, and `history`. The cwd is the *target repo* the
/// agent operates on, not where Phoenix keeps its own state — so memory and
/// sessions persist no matter which directory you launch `phoenix` from.
///
/// Override with the `PHOENIX_HOME` environment variable (useful for tests and
/// isolated profiles).
pub fn phoenix_home() -> PathBuf {
    if let Ok(custom) = std::env::var("PHOENIX_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".phoenix"))
        .unwrap_or_else(|| PathBuf::from(".phoenix"))
}

/// True when a TEST build would otherwise touch the live `~/.phoenix` (no
/// `PHOENIX_HOME` override set): every durable-store writer — gateway.log
/// lines (`gwlog`/`blog`/`mlog`), receipts, the Cognee store — must no-op
/// instead of polluting real state. 222 fake "[bg] Spark (coder)" lines from
/// one `cargo test` run once landed in the live gateway.log, poisoning the
/// forensics that log exists for; the receipts store was 98% cargo-test
/// records before its guard. A test that wants the real machinery opts in by
/// pointing `PHOENIX_HOME` at an isolated tempdir.
pub fn test_isolated_from_live_home() -> bool {
    cfg!(test)
        && std::env::var("PHOENIX_HOME")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
}

/// Runtime prompt overlays (RSI tier 1): when `~/.phoenix/prompts/<name>.md`
/// exists and is non-empty, it replaces the embedded system prompt — prompts
/// become editable (by the user or, later, by Phoenix itself through the
/// approval board) without recompiling. Names mirror the repo `prompts/`
/// files: `orchestrator_system`, `coder_system`, `librarian_system`, …
pub fn prompt_overlay(name: &str, embedded: &str) -> String {
    if !crate::settings::effective_bool(
        "prompts.allow_overlays",
        &crate::settings::SettingsScope::Global,
    )
    .unwrap_or(true)
    {
        return embedded.to_string();
    }
    prompt_overlay_from(&phoenix_home(), name, embedded)
}

fn prompt_overlay_from(home: &std::path::Path, name: &str, embedded: &str) -> String {
    const MAX_PROMPT_OVERLAY_BYTES: usize = 4 * 1024 * 1024;
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        eprintln!("warning: invalid prompt overlay name ignored: {name:?}");
        return embedded.to_string();
    }
    let path = home.join("prompts").join(format!("{name}.md"));
    match private_io::read_private_file(&path) {
        Ok(Some(bytes)) if bytes.len() > MAX_PROMPT_OVERLAY_BYTES => {
            eprintln!(
                "warning: prompt overlay {} is too large ({} bytes; max {MAX_PROMPT_OVERLAY_BYTES}) — using the embedded prompt",
                path.display(),
                bytes.len()
            );
            embedded.to_string()
        }
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(text) if !text.trim().is_empty() => text,
            Ok(_) => embedded.to_string(),
            Err(_) => {
                eprintln!(
                    "warning: prompt overlay {} is not valid UTF-8 — using the embedded prompt",
                    path.display()
                );
                embedded.to_string()
            }
        },
        Ok(None) => embedded.to_string(),
        Err(error) => {
            eprintln!(
                "warning: prompt overlay {} is unreadable ({error:#}) — using the embedded prompt",
                path.display()
            );
            embedded.to_string()
        }
    }
}

/// Fallback context window when the configured provider/model is not in the
/// catalog and no `context_window` override is set in config.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 200_000;

/// Resolve the active model's context window: config override first, then the
/// provider catalog, then the conservative default. Used by the TUI gauge and
/// the runtime's auto-compaction ceiling — keep them on the same number.
pub fn resolve_context_window() -> u64 {
    let Ok(config) = PhoenixConfig::load() else {
        return DEFAULT_CONTEXT_WINDOW_TOKENS;
    };
    let llm = &config.profile.llm;
    llm.context_window
        .or_else(|| crate::providers::providers_data::context_window_for(&llm.provider, &llm.model))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

/// The active main lane's prompt-cache TTL in seconds — how long a warm cache
/// survives idle before the next turn re-reads the whole context at full price.
/// Drives the compact-on-cache-miss trigger (fold right before that re-read).
///
/// Resolution order:
///   1. `PHOENIX_CACHE_TTL_SECS` env override (0 disables cache-miss folding).
///   2. A per-provider default — providers whose cache/`ttl` we know differs
///      from the 5-minute norm are pinned here.
///   3. [`compaction::DEFAULT_CACHE_TTL_SECS`] (300s, the Anthropic ephemeral
///      default) for everything else.
///
/// It is NOT a hard-coded 5 minutes: the 5-minute figure is only the fallback,
/// and it's a sliding window (refreshed on each cache read), which is why the
/// idle-gap trigger it feeds is measured as "now − last turn".
pub fn resolve_cache_ttl_secs() -> u64 {
    use crate::runtime::compaction::DEFAULT_CACHE_TTL_SECS;
    if let Ok(raw) = std::env::var("PHOENIX_CACHE_TTL_SECS") {
        if let Ok(secs) = raw.trim().parse::<u64>() {
            return secs;
        }
    }
    let Ok(config) = PhoenixConfig::load() else {
        return DEFAULT_CACHE_TTL_SECS;
    };
    match config.profile.llm.provider.as_str() {
        // Local/served models keep the whole KV-cache resident between requests
        // for far longer than a hosted ephemeral cache — a short idle gap does
        // NOT invalidate them, so don't fold prematurely.
        "ollama" | "ollama-cloud" | "sglang" | "vllm" | "lm_studio" | "litellm" => 3_600,
        _ => DEFAULT_CACHE_TTL_SECS,
    }
}

impl PhoenixConfig {
    pub fn load() -> anyhow::Result<Self> {
        let loader = ConfigLoader::new();
        loader.load()
    }

    pub fn load_from_path(path: PathBuf) -> anyhow::Result<Self> {
        let loader = ConfigLoader::new().with_path(path);
        loader.load()
    }
}

#[cfg(test)]
mod state_read_tests {
    use super::*;

    #[test]
    fn prompt_overlay_rejects_path_shaped_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            prompt_overlay_from(dir.path(), "../outside", "embedded"),
            "embedded"
        );
        assert_eq!(
            prompt_overlay_from(dir.path(), "Agent-System", "embedded"),
            "embedded"
        );
    }

    #[test]
    fn corrupt_prompt_overlay_does_not_become_runtime_text() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        std::fs::create_dir(&prompts).unwrap();
        std::fs::write(prompts.join("coder_system.md"), [0xff, 0xfe]).unwrap();

        assert_eq!(
            prompt_overlay_from(dir.path(), "coder_system", "embedded"),
            "embedded"
        );
    }

    #[test]
    fn oversized_prompt_overlay_falls_back_to_embedded_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        std::fs::create_dir(&prompts).unwrap();
        let file = std::fs::File::create(prompts.join("coder_system.md")).unwrap();
        file.set_len(4 * 1024 * 1024 + 1).unwrap();

        assert_eq!(
            prompt_overlay_from(dir.path(), "coder_system", "embedded"),
            "embedded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_prompt_overlay_is_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        std::fs::create_dir(&prompts).unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "hostile replacement").unwrap();
        symlink(&outside, prompts.join("coder_system.md")).unwrap();

        assert_eq!(
            prompt_overlay_from(dir.path(), "coder_system", "embedded"),
            "embedded"
        );
    }
}
