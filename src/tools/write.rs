//! write tool - write workspace files

use std::path::Path;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{format_bytes, workspace_io, ToolOutput};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteInput {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub overwrite: bool,
    /// New Markdown defaults to a private working note under ~/.phoenix so a
    /// model's transient plan/research/handoff does not litter the user's
    /// repository. Existing Markdown is always edited in place. The provider
    /// must explicitly classify a genuinely requested project file or final
    /// deliverable.
    #[serde(default)]
    pub purpose: WritePurpose,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePurpose {
    #[default]
    WorkingNote,
    ProjectSource,
    Deliverable,
}

pub fn execute(workspace_root: &Path, confined: bool, input: WriteInput) -> Result<ToolOutput> {
    execute_for_session(workspace_root, confined, input, None)
}

pub(crate) fn execute_for_session(
    workspace_root: &Path,
    confined: bool,
    input: WriteInput,
    session_id: Option<&str>,
) -> Result<ToolOutput> {
    workspace_io::validate_path_input(&input.path)?;
    if input.content.len() > workspace_io::MAX_WORKSPACE_TEXT_BYTES {
        bail!(
            "write content is too large ({} bytes; max {})",
            input.content.len(),
            workspace_io::MAX_WORKSPACE_TEXT_BYTES
        );
    }
    if should_store_private_markdown(workspace_root, &input)? {
        return store_private_working_note(workspace_root, &input, session_id);
    }
    let target =
        workspace_io::LockedWorkspaceTarget::acquire(workspace_root, &input.path, confined, true)?;
    let exists = target.exists()?;
    let expected = if confined {
        match session_id {
            Some(session_id) => {
                let receipt =
                    crate::runtime::read_files::receipt(session_id, target.canonical_path());
                if receipt.is_none() && exists {
                    return Err(anyhow::anyhow!(
                        "file has no content-version read receipt; read it again before editing"
                    ));
                }
                if !exists && !input.overwrite {
                    None
                } else {
                    receipt
                }
            }
            None => None,
        }
    } else {
        None
    };
    let existed = target.publish_checked(
        input.content.as_bytes(),
        input.overwrite,
        expected
            .as_ref()
            .map(|receipt| (receipt.sha256.as_str(), receipt.bytes)),
    )?;
    if confined {
        if let Some(session_id) = session_id {
            let sha256 = format!("sha256:{:x}", Sha256::digest(input.content.as_bytes()));
            crate::runtime::read_files::record(
                session_id,
                target.canonical_path(),
                &sha256,
                input.content.len() as u64,
            );
        }
    }

    Ok(ToolOutput {
        summary: format!(
            "{} {} with {}.",
            if existed { "Updated" } else { "Created" },
            input.path,
            format_bytes(input.content.len() as u64)
        ),
        content: String::new(),
    })
}

fn should_store_private_markdown(workspace_root: &Path, input: &WriteInput) -> Result<bool> {
    if input.purpose != WritePurpose::WorkingNote
        || !Path::new(&input.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Ok(false);
    }
    let requested = Path::new(&input.path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };
    match std::fs::symlink_metadata(candidate) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn store_private_working_note(
    workspace_root: &Path,
    input: &WriteInput,
    session_id: Option<&str>,
) -> Result<ToolOutput> {
    let workspace_key = format!(
        "{:x}",
        Sha256::digest(workspace_root.to_string_lossy().as_bytes())
    );
    let workspace_key = &workspace_key[..16];
    let session = session_id
        .filter(|value| crate::session::SessionStore::validate_session_id(value).is_ok())
        .unwrap_or("unsessioned");
    let requested = Path::new(&input.path);
    let mut relative = std::path::PathBuf::new();
    for component in requested.components() {
        if let std::path::Component::Normal(component) = component {
            relative.push(component);
        }
    }
    if relative.as_os_str().is_empty() {
        relative.push("working-note.md");
    }
    let target = crate::config::phoenix_home()
        .join("artifacts")
        .join("workspaces")
        .join(workspace_key)
        .join("working")
        .join(session)
        .join(relative);
    let existed = crate::config::private_io::read_private_file_limited(
        &target,
        workspace_io::MAX_WORKSPACE_TEXT_BYTES,
    )?
    .is_some();
    if existed && !input.overwrite {
        bail!(
            "private working note already exists at {}; pass overwrite=true to replace it",
            target.display()
        );
    }
    crate::config::private_io::atomic_write_private(&target, input.content.as_bytes())?;
    Ok(ToolOutput {
        summary: format!(
            "{} private working note with {} (kept out of the workspace).",
            if existed { "Updated" } else { "Stored" },
            format_bytes(input.content.len() as u64)
        ),
        content: format!(
            "Phoenix artifact: {}\nUse purpose=project_source or purpose=deliverable only when the user actually needs a new Markdown file in the workspace.",
            target.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_creates_missing_parent_dirs() {
        // Regression: writing into a not-yet-existing subdir (accounts/creds.txt)
        // used to fail at path resolution before create_dir_all ran, so a
        // coworker could not save a newly created nested artifact.
        let dir = tempfile::tempdir().expect("tempdir");
        let out = execute(
            dir.path(),
            true,
            WriteInput {
                path: "accounts/dreamina/creds.txt".to_string(),
                content: "hello".to_string(),
                overwrite: false,
                purpose: WritePurpose::WorkingNote,
            },
        )
        .expect("write into a fresh nested dir must succeed");
        assert!(out.summary.contains("Created"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("accounts/dreamina/creds.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn write_still_blocks_escape_outside_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = execute(
            dir.path(),
            true,
            WriteInput {
                path: "../../etc/evil.txt".to_string(),
                content: "x".to_string(),
                overwrite: false,
                purpose: WritePurpose::WorkingNote,
            },
        );
        assert!(err.is_err(), "path escaping the workspace must be rejected");
    }

    #[test]
    fn new_markdown_defaults_to_private_phoenix_artifacts() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let output = execute_for_session(
            workspace.path(),
            true,
            WriteInput {
                path: "plans/temporary-analysis.md".to_string(),
                content: "working notes".to_string(),
                overwrite: false,
                purpose: WritePurpose::WorkingNote,
            },
            Some("agent-nico"),
        )
        .unwrap();
        assert!(!workspace
            .path()
            .join("plans/temporary-analysis.md")
            .exists());
        assert!(output.summary.contains("kept out of the workspace"));
        let private_path = output
            .content
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Phoenix artifact: ")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(private_path).unwrap(),
            "working notes"
        );
    }

    #[test]
    fn requested_markdown_deliverable_stays_in_workspace() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        execute_for_session(
            workspace.path(),
            true,
            WriteInput {
                path: "docs/guide.md".to_string(),
                content: "# User guide".to_string(),
                overwrite: false,
                purpose: WritePurpose::Deliverable,
            },
            Some("agent-cleo"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("docs/guide.md")).unwrap(),
            "# User guide"
        );
    }
}
