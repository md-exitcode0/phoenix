//! ui_snap — self-serve visual verification for the design lanes.
//!
//! Root cause from the retired architecture: visual verification once depended
//! on a separate shared-browser worker. While it was busy, Iris restyled a
//! cloned codebase blind for 30+ minutes and shipped without a screenshot. A
//! design lane cannot pass its visual gate without an independent eye.
//!
//! This tool is that eye: a throwaway, ISOLATED headless chrome (temp
//! profile, no logins, no coworker profile lock) navigates a URL — typically the
//! local dev server — screenshots it into the workspace, and tears down.
//! Pair with `image_analyze` on the produced file to score it.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::ToolOutput;

#[derive(Debug, Deserialize)]
pub struct UiSnapInput {
    /// The page to look at — usually the dev server (`http://localhost:3001`).
    /// A bare `localhost:3001` or `localhost:3001/pricing` gets `http://`.
    pub url: String,
    /// Workspace-relative output path. Default: `artifacts/ui-snaps/<ts>.png`.
    #[serde(default)]
    pub path: Option<String>,
    /// Capture the full page height instead of the viewport. Default false.
    #[serde(default)]
    pub full_page: Option<bool>,
    /// Milliseconds to wait after navigation before capturing (client-side
    /// rendering + entry animations). Default 1500.
    #[serde(default)]
    pub wait_ms: Option<u64>,
    /// Viewport width. Default 1440. Use 390 for a phone-width check.
    #[serde(default)]
    pub width: Option<u32>,
    /// Viewport height. Default 900.
    #[serde(default)]
    pub height: Option<u32>,
}

const MAX_OUTPUT_PATH_BYTES: usize = 4096;
const MAX_OUTPUT_PATH_COMPONENTS: usize = 128;

fn output_relative_path(requested: Option<&str>) -> Result<PathBuf> {
    let rel = match requested.map(str::trim) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(format!(
            "artifacts/ui-snaps/snap-{}-{}.png",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"),
            uuid::Uuid::new_v4().simple()
        )),
    };
    if rel.as_os_str().len() > MAX_OUTPUT_PATH_BYTES {
        bail!(
            "ui_snap `path` is too long ({} bytes; max {MAX_OUTPUT_PATH_BYTES})",
            rel.as_os_str().len()
        );
    }
    let mut component_count = 0usize;
    for component in rel.components() {
        match component {
            Component::Normal(name) if !name.is_empty() => {
                component_count += 1;
                if component_count > MAX_OUTPUT_PATH_COMPONENTS {
                    bail!(
                        "ui_snap `path` has too many components (max {MAX_OUTPUT_PATH_COMPONENTS})"
                    );
                }
            }
            _ => {
                bail!(
                    "ui_snap `path` must be workspace-relative with no `.` or `..` (got `{}`)",
                    rel.display()
                );
            }
        }
    }
    if component_count == 0 || rel.file_name().is_none() {
        bail!("ui_snap `path` must name a workspace-relative file");
    }
    Ok(rel)
}

pub async fn execute(workspace_root: &Path, input: UiSnapInput) -> Result<ToolOutput> {
    let mut url = input.url.trim().to_string();
    if url.is_empty() {
        bail!("ui_snap needs a `url` — usually your dev server, e.g. http://localhost:3001");
    }
    if !url.contains("://") {
        url = format!("http://{url}");
    }
    let width = input.width.unwrap_or(1440).clamp(320, 3840);
    let height = input.height.unwrap_or(900).clamp(480, 2400);
    let full_page = input.full_page.unwrap_or(false);
    let wait_ms = input.wait_ms.unwrap_or(1500).min(15_000);

    // Validate the destination before paying for a Chrome launch. Canonicalize
    // only the trusted workspace root (which may itself be a user-selected
    // symlink); every path component below it is then checked without following
    // symlinks by the shared artifact writer.
    let rel = output_relative_path(input.path.as_deref())?;
    let canonical_root = std::fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to resolve workspace {}", workspace_root.display()))?;
    let target = canonical_root.join(&rel);
    crate::config::private_io::reject_symlink_components(&target)?;

    // Chrome launch + capture are blocking (std) — keep them off the runtime.
    let snap_target = url.clone();
    let png = tokio::task::spawn_blocking(move || {
        crate::tools::browser_native::snap_url(&snap_target, width, height, full_page, wait_ms)
    })
    .await
    .map_err(|e| anyhow::anyhow!("ui_snap worker panicked: {e}"))??;

    // Artifact publication must not use the config/state writer: that writer's
    // durable advisory lock is intentionally persistent, and a unique default
    // snap name would otherwise leak one `.snap-<uuid>.png.lock` per capture.
    crate::tools::browser_native::atomic_write_artifact_output(&target, &png)?;

    Ok(ToolOutput {
        summary: format!("ui_snap {url} → {}", rel.display()),
        content: format!(
            "Screenshot of {url} ({width}x{height}{}) saved to {} ({} bytes).\n\
             Now LOOK at it: call image_analyze on this file and judge it against the brief \
             and the slop test — a design final without this look-and-score step will bounce.",
            if full_page { ", full page" } else { "" },
            rel.display(),
            png.len()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_stays_strictly_below_workspace() {
        assert_eq!(
            output_relative_path(Some("artifacts/ui-snaps/home.png")).unwrap(),
            PathBuf::from("artifacts/ui-snaps/home.png")
        );
        for invalid in [
            "/tmp/out.png",
            "../out.png",
            "artifacts/../out.png",
            "./out.png",
            ".",
            "..",
        ] {
            assert!(output_relative_path(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn default_output_names_do_not_collide() {
        let first = output_relative_path(None).unwrap();
        let second = output_relative_path(None).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("png"));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_writer_is_private_and_leaves_no_lock_or_stage() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("artifacts/ui-snaps/check.png");
        crate::tools::browser_native::atomic_write_artifact_output(&target, b"png bytes").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"png bytes");
        assert_eq!(
            std::fs::metadata(target.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(".lock") || name.ends_with(".tmp")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "leftover artifact files: {leftovers:?}"
        );
    }
}
