//! Persisted CLI toggle defaults, stored in a `[cli]` table in
//! `~/.phoenix/config.toml`. Loaded at startup; written by `/defaults save`.
//!
//! We edit the config as a generic TOML table so the rest of the file
//! (provider/profile/auth) is preserved untouched.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Default, Clone)]
pub struct CliDefaults {
    pub yolo: Option<bool>,
    pub actions: Option<bool>,
    pub thinking: Option<bool>,
    pub debug: Option<bool>,
    pub subagents: Option<bool>,
    /// Feed verbosity: "compact" | "detail" | "verbose".
    pub display: Option<String>,
}

fn config_path() -> PathBuf {
    crate::config::phoenix_home().join("config.toml")
}

/// Private compare-and-swap for config.toml, shared by CLI config surfaces.
/// The lock serializes cooperating Phoenix writers; the expected-content check
/// also refuses to erase an edit from an older writer that does not yet lock.
pub(super) fn replace_config_atomic(
    path: &std::path::Path,
    expected: Option<&str>,
    new_content: &str,
) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    let expected = expected.map(str::as_bytes);
    crate::config::private_io::read_modify_write_private(path, |current| {
        if current != expected {
            anyhow::bail!("{} changed in another process; retry", path.display());
        }
        if let Some(old) = current {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let backup = parent.join(format!("config.toml.bak-{stamp}-{}", std::process::id()));
            crate::config::private_io::atomic_write_private(&backup, old)?;
        }
        Ok(((), new_content.as_bytes().to_vec()))
    })
}

/// Read persisted defaults; absent file/section yields all-`None`.
pub fn load() -> CliDefaults {
    let Ok(Some(text)) = crate::config::private_io::read_private_file(&config_path()) else {
        return CliDefaults::default();
    };
    let Ok(text) = String::from_utf8(text) else {
        return CliDefaults::default();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return CliDefaults::default();
    };
    let Some(cli) = table.get("cli").and_then(|v| v.as_table()) else {
        return CliDefaults::default();
    };
    let flag = |key: &str| cli.get(key).and_then(|v| v.as_bool());
    CliDefaults {
        yolo: flag("yolo"),
        actions: flag("actions"),
        thinking: flag("thinking"),
        debug: flag("debug"),
        subagents: flag("subagents"),
        display: cli
            .get("display")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Write the `[cli]` table, preserving the rest of the config file.
/// toml_edit, not a toml::Table round-trip: the old path re-serialized the
/// WHOLE file and stripped every comment the user (or configure) had written —
/// the same reason the canvas dashboard's config writers use toml_edit.
pub fn save(defaults: CliDefaults) -> Result<PathBuf> {
    let path = config_path();
    let (original, existed) = match crate::config::private_io::read_private_file(&path)? {
        Some(original) => (
            String::from_utf8(original)
                .with_context(|| format!("{} is not UTF-8", path.display()))?,
            true,
        ),
        None => (String::new(), false),
    };
    let mut doc: toml_edit::DocumentMut = if original.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        original.parse().with_context(|| {
            format!(
                "{} contains invalid TOML; refusing to replace it while saving CLI defaults",
                path.display()
            )
        })?
    };

    let mut cli = toml_edit::Table::new();
    if let Some(v) = defaults.yolo {
        cli["yolo"] = toml_edit::value(v);
    }
    if let Some(v) = defaults.actions {
        cli["actions"] = toml_edit::value(v);
    }
    if let Some(v) = defaults.thinking {
        cli["thinking"] = toml_edit::value(v);
    }
    if let Some(v) = defaults.debug {
        cli["debug"] = toml_edit::value(v);
    }
    if let Some(v) = defaults.subagents {
        cli["subagents"] = toml_edit::value(v);
    }
    if let Some(v) = defaults.display {
        cli["display"] = toml_edit::value(v);
    }
    doc["cli"] = toml_edit::Item::Table(cli);

    replace_config_atomic(
        &path,
        existed.then_some(original.as_str()),
        &doc.to_string(),
    )?;
    Ok(path)
}

#[cfg(all(test, unix))]
mod private_config_tests {
    use super::{replace_config_atomic, save, CliDefaults};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn config_replace_is_private_atomic_and_refuses_stale_writers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        replace_config_atomic(&path, None, "[cli]\nyolo = true\n").expect("first write");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        replace_config_atomic(&path, Some("[cli]\nyolo = true\n"), "[cli]\nyolo = false\n")
            .expect("second write");
        assert!(replace_config_atomic(&path, Some("stale"), "bad").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read final"),
            "[cli]\nyolo = false\n"
        );
        let backups: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.toml.bak-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(backups.iter().all(|entry| {
            entry
                .metadata()
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777
                == 0o600
        }));
    }

    #[test]
    fn save_preserves_existing_malformed_config_instead_of_replacing_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
            .expect("private test home");
        let _home = crate::config::test_env::PhoenixHomeGuard::set(temp.path());
        let path = temp.path().join("config.toml");
        crate::config::private_io::atomic_write_private(&path, b"[profile\nbroken = true\n")
            .expect("seed malformed config");
        let before = std::fs::read(&path).expect("read malformed config");

        let error = save(CliDefaults {
            yolo: Some(true),
            ..CliDefaults::default()
        })
        .expect_err("malformed config must fail closed");

        assert!(error.to_string().contains("invalid TOML"));
        assert_eq!(std::fs::read(&path).expect("read preserved config"), before);
    }
}
