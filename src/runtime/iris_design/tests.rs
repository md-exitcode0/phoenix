use super::*;

fn scope() -> DesignScope {
    DesignScope { session_id: "iris-a".into(), turn_id: "turn-a".into(),
        workspace: PathBuf::from("/fixture/project"), browser_instance: "agent-frontend".into() }
}

fn saved() -> SavedDesign {
    SavedDesign { version: 1, scope: scope(), original_request: "Build a website".into(), reference_paths: Vec::new(),
        engine_hash: "engine-a".into(), state: json!({"phase":"page"}), attachments: BTreeMap::new(), pending_operation: None }
}

#[test]
fn foreign_and_stale_state_cannot_be_replayed() {
    let original = scope();
    let saved = saved();
    assert!(validate_owner(&saved, &original, "engine-a").is_ok());
    for changed in [DesignScope { session_id: "another".into(), ..original.clone() },
        DesignScope { workspace: PathBuf::from("/other"), ..original.clone() },
        DesignScope { browser_instance: "another-browser".into(), ..original.clone() }] {
        assert!(validate_owner(&saved, &changed, "engine-a").is_err());
    }
    assert!(validate_owner(&saved, &original, "engine-b").is_err());
}

#[test]
fn same_turn_cannot_change_request_or_reference_inputs() {
    let mut saved = saved();
    saved.reference_paths = vec![PathBuf::from("/fixture/reference.png")];
    assert!(validate_resume_identity(&saved, &scope(), "Build a website", &[], false).is_ok());
    assert!(validate_resume_identity(&saved, &scope(), "Build a different website", &[], false).is_err());
    let resumed = DesignScope { turn_id: "next-turn".into(), ..scope() };
    assert!(validate_resume_identity(&saved, &resumed, "continue", &[], true).is_ok());
    assert!(validate_resume_identity(&saved, &resumed, "continue", &saved.reference_paths, true).is_ok());
    assert!(validate_resume_identity(&saved, &resumed, "continue", &[PathBuf::from("/other.png")], true).is_err());
}

#[test]
fn workspace_binding_checks_exact_turn_and_path_before_engine_restore() {
    let saved = saved();
    let state_path = Path::new("/private/iris_design/a/active.json");
    let mut owner = WorkspaceOwner { session_id: "iris-a".into(), turn_id: "turn-a".into(),
        state_path: state_path.into(), terminal: false };
    assert!(validate_workspace_owner(Some(&owner), &saved, state_path).is_ok());
    assert!(validate_workspace_owner(None, &saved, state_path).is_err());
    owner.turn_id = "old-turn".into();
    assert!(validate_workspace_owner(Some(&owner), &saved, state_path).is_err());
    owner.turn_id = "turn-a".into();
    owner.state_path = PathBuf::from("/private/other/active.json");
    assert!(validate_workspace_owner(Some(&owner), &saved, state_path).is_err());
    owner.state_path = state_path.into();
    owner.session_id = "other-session".into();
    assert!(validate_workspace_owner(Some(&owner), &saved, state_path).is_err());
}

#[test]
fn cancelled_managed_design_is_persisted_as_terminal_failure() {
    let root = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let mut saved = saved();
    saved.scope.session_id = "agent-frontend".into();
    saved.scope.turn_id = "mesh-turn-cancelled".into();
    saved.scope.workspace = workspace.clone();
    saved.state = json!({
        "phase": "build",
        "status": "running",
        "revision": 7,
        "correcting": true,
        "pendingPrompt": "continue building",
        "correctionErrors": ["earlier correction"],
        "awaitingCapture": true
    });
    saved.pending_operation = Some("advance".into());

    let state_key = digest(
        &serde_json::to_vec(&(&saved.scope.session_id, &saved.scope.workspace)).unwrap(),
    );
    let state_path = root
        .path()
        .join("iris_design")
        .join(state_key)
        .join("active.json");
    let owner_path = root
        .path()
        .join("iris_design")
        .join("workspaces")
        .join(format!(
            "{}.json",
            digest(workspace.to_string_lossy().as_bytes())
        ));
    let owner = WorkspaceOwner {
        session_id: saved.scope.session_id.clone(),
        turn_id: saved.scope.turn_id.clone(),
        state_path: state_path.clone(),
        terminal: false,
    };
    private_io::atomic_write_private(&state_path, &serde_json::to_vec(&saved).unwrap()).unwrap();
    private_io::atomic_write_private(&owner_path, &serde_json::to_vec(&owner).unwrap()).unwrap();

    assert!(finalize_cancelled_turn(root.path(), "agent-frontend", &workspace).unwrap());

    let persisted: SavedDesign = serde_json::from_slice(
        &private_io::read_private_file_limited(&state_path, MAX_STATE_BYTES)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted.state["phase"], "build");
    assert_eq!(persisted.state["status"], "failed");
    assert_eq!(persisted.state["revision"], 8);
    assert_eq!(persisted.state["correcting"], false);
    assert_eq!(persisted.state["error"]["code"], "TURN_STOPPED");
    assert_eq!(persisted.state["error"]["phase"], "build");
    assert!(persisted.state.get("pendingPrompt").is_none());
    assert!(persisted.state.get("correctionErrors").is_none());
    assert!(persisted.state.get("awaitingCapture").is_none());
    assert!(persisted.pending_operation.is_none());

    let owner: WorkspaceOwner = serde_json::from_slice(
        &private_io::read_private_file_limited(&owner_path, 16_384)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(owner.terminal);
    assert!(
        !finalize_cancelled_turn(root.path(), "agent-frontend", &workspace).unwrap(),
        "terminal cancellation finalization must be idempotent"
    );
}

#[test]
fn authored_references_are_bounded_canonical_regular_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reference.png");
    std::fs::write(&path, b"raster validation is performed by the upstream parser").unwrap();
    assert_eq!(validate_reference_paths(&[path.clone()]).unwrap(), vec![path.canonicalize().unwrap()]);
    assert!(validate_reference_paths(&[path.clone(), path]).is_err());
    assert!(validate_reference_paths(&[PathBuf::from("relative.png")]).is_err());
    assert!(validate_reference_paths(&[dir.path().to_path_buf()]).is_err());
}

#[cfg(unix)]
#[test]
fn reference_reads_reject_symlinks_and_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    std::fs::write(&path, b"reference").unwrap();
    assert_eq!(read_regular(&path, 9).unwrap(), b"reference");
    assert!(read_regular(&path, 8).is_err());
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&path, &linked).unwrap();
    assert!(read_regular(&linked, 100).is_err());
    assert!(read_regular(dir.path(), 100).is_err());
}

#[test]
fn bridge_request_limit_matches_upstream_encoded_protocol_not_state_storage() {
    assert!(MAX_STATE_BYTES > MAX_BRIDGE_BYTES);
    assert_eq!(bridge_request_bytes(&json!("x".repeat(MAX_BRIDGE_BYTES - 2))).unwrap().len(), MAX_BRIDGE_BYTES);
    let error = bridge_request_bytes(&json!("x".repeat(MAX_BRIDGE_BYTES - 1))).unwrap_err().to_string();
    assert!(error.contains("8 MiB") && error.contains("no engine process was started"));
    let escaped_boundary = json!("\u{0}".repeat(MAX_BRIDGE_BYTES / 6));
    assert_eq!(bridge_request_bytes(&escaped_boundary).unwrap().len(), MAX_BRIDGE_BYTES,
        "six-byte escapes plus the two JSON quotes exactly reach this boundary");
    assert!(bridge_request_bytes(&json!("\u{0}".repeat(MAX_BRIDGE_BYTES / 6 + 1))).is_err(),
        "JSON escaping, not raw text length, determines protocol size");
}

#[cfg(unix)]
#[test]
fn node_discovery_finds_nvm_when_node_is_not_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    let node = home.path().join(".nvm/versions/node/v24.15.0/bin/node");
    std::fs::create_dir_all(node.parent().unwrap()).unwrap();
    std::fs::write(&node, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700)).unwrap();

    let discovered = discover_node(None, Some(home.path())).unwrap();
    // PATH wins by design; the version-manager fallback is only observable on
    // a machine without a usable Node on PATH.
    if discovered == std::path::Path::new("node") || discovered == std::path::Path::new("nodejs") {
        return;
    }
    assert_eq!(discovered, node);
}

#[cfg(unix)]
#[test]
fn explicit_node_stays_authoritative_and_must_be_usable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let node = dir.path().join("node");
    std::fs::write(&node, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(discover_node(Some(node.clone().into_os_string()), None).unwrap(), node);

    let missing = dir.path().join("missing-node");
    assert!(discover_node(Some(missing.into_os_string()), None).unwrap_err().to_string().contains("PHOENIX_NODE"));
}

#[test]
fn review_capture_capacity_preserves_the_complete_accepted_reference_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut references = Vec::new();
    for index in 0..MAX_ATTACHMENTS {
        let path = dir.path().join(format!("reference-{index}.png"));
        std::fs::write(&path, b"the upstream parser separately validates raster content").unwrap();
        references.push(path);
    }
    assert_eq!(validate_reference_paths(&references).unwrap().len(), MAX_ATTACHMENTS);
    for captures in 0..=4 { assert!(validate_phase_image_count(references.len() + captures).is_ok()); }
    assert!(validate_phase_image_count(MAX_PHASE_ATTACHMENTS + 1).is_err());
    references.push(dir.path().join("excess.png"));
    assert!(validate_reference_paths(&references).is_err(), "do not raise the authored-reference input limit");
}

#[tokio::test]
async fn full_document_review_image_preserves_pixels_beyond_desktop_edge_limit() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("full-document.png");
    let mut image = image::RgbImage::from_pixel(1440, 9000, image::Rgb([230, 220, 210]));
    for y in 8950..9000 { for x in 0..1440 { image.put_pixel(x, y, image::Rgb([20, 90, 160])); } }
    image.save(&path).unwrap();
    // The old image-message route rejects this legitimate upstream capture.
    // Ordinary desktop/reference inputs retain that stricter existing bound.
    assert!(crate::runtime::vision::native_screen_data_uri(&path).await.is_err());
    assert!(phase_image_data_uri(&json!({}), &path).await.is_err());
    assert!(phase_image_data_uri(&json!({"screenshots":[{"path":dir.path().join("other.png") }]}), &path).await.is_err());
    let state = json!({"phase":"review","screenshots":[{"path":path,"width":1440,"height":1000}]});
    let data = phase_image_data_uri(&state, &path).await.unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap()).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (1440, 9000), "review must receive the full page, not a tiny thumbnail or top crop");
    let bottom = decoded.get_pixel(720, 8990).0;
    for (actual, expected) in bottom.into_iter().zip([20_u8, 90, 160]) { assert!(actual.abs_diff(expected) < 8); }
}

#[tokio::test]
async fn managed_capture_keeps_format_dimension_and_file_guards() {
    let dir = tempfile::tempdir().unwrap();
    let too_tall = dir.path().join("too-tall.png");
    image::RgbImage::from_pixel(8, 12_001, image::Rgb([1, 2, 3])).save(&too_tall).unwrap();
    let state = json!({"screenshots":[{"path":too_tall}]});
    assert!(phase_image_data_uri(&state, &too_tall).await.is_err());
    let corrupt = dir.path().join("corrupt.png");
    std::fs::write(&corrupt, b"not PNG bytes").unwrap();
    assert!(phase_image_data_uri(&json!({"screenshots":[{"path":corrupt}]}), &corrupt).await.is_err());
    let jpeg = dir.path().join("capture.jpg");
    image::RgbImage::from_pixel(32, 32, image::Rgb([1, 2, 3])).save(&jpeg).unwrap();
    assert!(phase_image_data_uri(&json!({"screenshots":[{"path":jpeg}]}), &jpeg).await.is_err());
    assert!(phase_image_data_uri(&json!({"screenshots":[{"path":dir.path()}]}), dir.path()).await.is_err());
    #[cfg(unix)] {
        let link = dir.path().join("capture-link.png");
        std::os::unix::fs::symlink(&too_tall, &link).unwrap();
        assert!(phase_image_data_uri(&json!({"screenshots":[{"path":link}]}), &link).await.is_err());
    }
}
