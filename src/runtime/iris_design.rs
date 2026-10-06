//! Runtime adapter for the vendored TasteCode Design controller.
//!
//! The JavaScript package owns prompts, selection and validation. This module
//! owns only process transport, exact conversation ownership and durable replay.
//! It never selects a model or executes a model-authored command.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::private_io;

mod preview;
pub(crate) use preview::preview_permitted;

const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;
// Match the unmodified JavaScript bridge protocol, independently of the
// larger on-disk envelope that also carries native ownership metadata.
const MAX_BRIDGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 46 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 32;
// Upstream permits four preview viewports. Captures must not displace an
// already accepted supplied reference set when the flow enters Review.
const MAX_PHASE_ATTACHMENTS: usize = MAX_ATTACHMENTS + 4;
const BRIDGE_BUDGET: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesignScope {
    pub session_id: String,
    pub turn_id: String,
    pub workspace: PathBuf,
    pub browser_instance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedDesign {
    version: u32,
    scope: DesignScope,
    original_request: String,
    #[serde(default)]
    reference_paths: Vec<PathBuf>,
    engine_hash: String,
    state: Value,
    /// Exact bytes previously exposed to the model, including review captures.
    attachments: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pending_operation: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesignContext {
    pub phase: String,
    pub status: String,
    pub kind: String,
    pub prompt: Option<String>,
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
    pub preview_plan: Option<Value>,
    pub preview_url: Option<String>,
    pub outcome: Option<String>,
    pub error: Option<Value>,
}

pub(crate) struct IrisDesignController {
    bridge: DesignBridge,
    saved: SavedDesign,
    path: PathBuf,
    committed: Vec<u8>,
    context: DesignContext,
    // A second gateway process must not advance the same flow concurrently.
    _claim: private_io::PrivateExecutionClaim,
    _workspace_claim: private_io::PrivateExecutionClaim,
    owner_path: PathBuf,
    preview: Option<preview::OwnedPreview>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceOwner {
    session_id: String,
    turn_id: String,
    state_path: PathBuf,
    terminal: bool,
}

#[derive(Clone)]
struct DesignBridge {
    root: PathBuf,
    entry: PathBuf,
    node: PathBuf,
    hash: String,
}

impl DesignBridge {
    fn locate() -> Result<Self> {
        let mut roots = Vec::new();
        if let Some(root) = std::env::var_os("PHOENIX_IRIS_DESIGN_ROOT") {
            roots.push(PathBuf::from(root));
        } else {
            roots.push(crate::config::phoenix_home().join("design-runtime"));
            if let Ok(executable) = std::env::current_exe() {
                for parent in executable.ancestors().skip(1).take(3) {
                    roots.push(parent.to_path_buf());
                    roots.push(parent.join("share/phoenix"));
                }
            }
            roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        }
        let root = roots.into_iter().find(|root| root.join("scripts/iris-design-runtime.mjs").is_file())
            .context("Iris Design runtime is missing: install scripts/iris-design-runtime.mjs and its vendor/tastecode-design package")?
            .canonicalize()?;
        let entry = root.join("scripts/iris-design-runtime.mjs");
        private_io::reject_symlink_components(&entry)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let node = discover_node(std::env::var_os("PHOENIX_NODE"), home.as_deref())?;
        let hash = engine_hash(&root)?;
        Ok(Self { root, entry, node, hash })
    }

    async fn call(&self, request: Value) -> Result<Value> {
        ensure!(engine_hash(&self.root)? == self.hash,
            "Iris Design engine changed during this run; preserved state cannot be advanced by another engine");
        let input = bridge_request_bytes(&request)?;
        let mut child = tokio::process::Command::new(&self.node)
            .arg(&self.entry)
            .current_dir(&self.root)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn().context("could not start the installed Iris Design JavaScript engine")?;
        let mut stdin = child.stdin.take().context("Iris Design bridge has no stdin")?;
        let stdout = child.stdout.take().context("Iris Design bridge has no stdout")?;
        let stderr = child.stderr.take().context("Iris Design bridge has no stderr")?;
        let operation = async {
            let write = async move {
                stdin.write_all(&input).await?;
                stdin.shutdown().await?;
                // The one-request JavaScript bridge reads until EOF. Shutdown
                // flushes the async pipe but its handle must also be dropped
                // before waiting for the child's response, not after it.
                drop(stdin);
                Ok::<_, std::io::Error>(())
            };
            let read_out = async {
                let mut bytes = Vec::new();
                stdout.take((MAX_BRIDGE_BYTES + 1) as u64).read_to_end(&mut bytes).await?;
                ensure!(bytes.len() <= MAX_BRIDGE_BYTES, "Iris Design bridge output exceeds its 8 MiB byte limit");
                Ok::<_, anyhow::Error>(bytes)
            };
            let read_err = async {
                let mut bytes = Vec::new();
                stderr.take(16_385).read_to_end(&mut bytes).await?;
                ensure!(bytes.len() <= 16_384, "Iris Design bridge diagnostics exceed their byte limit");
                Ok::<_, anyhow::Error>(bytes)
            };
            let (_, output, diagnostics) = tokio::try_join!(async { write.await.map_err(anyhow::Error::from) }, read_out, read_err)?;
            let status = child.wait().await?;
            let parsed: Value = serde_json::from_slice(&output)
                .context("Iris Design bridge returned invalid JSON; no phase was accepted")?;
            if !status.success() {
                let detail = parsed.pointer("/error/message").and_then(Value::as_str)
                    .unwrap_or("controller transport or state validation failed");
                bail!("Iris Design bridge rejected this operation: {}{}", detail,
                    if diagnostics.is_empty() { String::new() } else { format!(" ({})", String::from_utf8_lossy(&diagnostics)) });
            }
            Ok(parsed)
        };
        tokio::time::timeout(BRIDGE_BUDGET, operation).await
            .context("Iris Design engine timed out; the phase remains unaccepted")?
    }
}

fn discover_node(explicit: Option<std::ffi::OsString>, home: Option<&Path>) -> Result<PathBuf> {
    if let Some(explicit) = explicit {
        let node = PathBuf::from(explicit);
        ensure!(node_supports_design_runtime(&node),
            "PHOENIX_NODE must name a working Node.js 22 or newer executable");
        return Ok(node);
    }

    let mut candidates = vec![PathBuf::from("node"), PathBuf::from("nodejs")];
    if let Some(home) = home {
        candidates.push(home.join(".volta/bin/node"));
        append_versioned_node_candidates(&mut candidates, &home.join(".nvm/versions/node"), "bin/node");
        append_versioned_node_candidates(&mut candidates, &home.join(".local/share/mise/installs/node"), "bin/node");
        append_versioned_node_candidates(&mut candidates, &home.join(".asdf/installs/nodejs"), "bin/node");
        append_versioned_node_candidates(&mut candidates, &home.join(".local/share/fnm/node-versions"), "installation/bin/node");
    }
    candidates.into_iter().find(|candidate| node_supports_design_runtime(candidate)).with_context(|| {
        "Iris Design requires Node.js 22 or newer. Set PHOENIX_NODE to the executable if Node is installed outside PATH or a standard user version-manager location"
    })
}

fn append_versioned_node_candidates(candidates: &mut Vec<PathBuf>, root: &Path, suffix: &str) {
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    let mut versions = entries.filter_map(|entry| entry.ok()).map(|entry| entry.path()).collect::<Vec<_>>();
    versions.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    candidates.extend(versions.into_iter().map(|version| version.join(suffix)));
}

fn node_supports_design_runtime(candidate: &Path) -> bool {
    std::process::Command::new(candidate)
        .args(["-e", "if(Number(process.versions.node.split('.')[0])<22)process.exit(1)"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().is_ok_and(|status| status.success())
}

impl IrisDesignController {
    /// Only the caller decides whether this authored turn is a build or an
    /// exact continuation. A chat/status turn never opens the design state.
    pub(crate) async fn open(
        state_root: &Path,
        mut scope: DesignScope,
        request: &str,
        continuation: bool,
        references: &[PathBuf],
    ) -> Result<Option<Self>> {
        scope.workspace = scope.workspace.canonicalize().context("Iris workspace is unavailable")?;
        let references = validate_reference_paths(references)?;
        let key = digest(&serde_json::to_vec(&(&scope.session_id, &scope.workspace))?);
        let path = state_root.join("iris_design").join(key).join("active.json");
        if continuation && !path.try_exists()? { return Ok(None); }
        let owner_path = state_root.join("iris_design").join("workspaces")
            .join(format!("{}.json", digest(scope.workspace.to_string_lossy().as_bytes())));
        let workspace_claim = private_io::try_execution_claim(&owner_path.with_extension("execution"))?
            .context("another Iris execution owns this workspace's design artifacts")?;
        let workspace_owner = private_io::read_private_file_limited(&owner_path, 16_384)?
            .map(|bytes| serde_json::from_slice::<WorkspaceOwner>(&bytes)
                .context("Iris workspace ownership record is corrupt")).transpose()?;
        if let Some(owner) = workspace_owner.as_ref() {
            ensure!(owner.session_id == scope.session_id || (owner.terminal && !continuation),
                "this workspace's approved design belongs to another conversation; foreign state cannot be resumed or overwritten");
        }
        let claim = private_io::try_execution_claim(&path.with_extension("execution"))?
            .context("this exact Iris design flow is already owned by another execution")?;
        let existing = private_io::read_private_file_limited(&path, MAX_STATE_BYTES)?;
        if continuation && existing.is_none() { return Ok(None); }
        let bridge = DesignBridge::locate()?;
        let mut committed = existing.clone().unwrap_or_default();
        let saved = if let Some(bytes) = existing {
            let previous: SavedDesign = serde_json::from_slice(&bytes)
                .context("Iris Design state is corrupt; refusing to replace it with a new reference draw")?;
            validate_owner(&previous, &scope, &bridge.hash)?;
            validate_resume_identity(&previous, &scope, request, &references, continuation)?;
            validate_workspace_owner(workspace_owner.as_ref(), &previous, &path)?;
            let context = bridge_context(&bridge, &previous.state).await?;
            let same_turn = previous.scope.turn_id == scope.turn_id;
            if same_turn || (continuation && context.kind != "terminal") {
                ensure!(previous.pending_operation.is_none(),
                    "Iris Design was interrupted during an uncommitted phase operation; its exact saved evidence requires review before any rerun or new reference draw");
                previous
            } else if continuation {
                return Ok(None);
            } else {
                // A new request supersedes whatever flow came before it. An
                // unfinished flow (a reboot, a stop, a failed turn) used to
                // refuse every later build in this conversation, so Iris could
                // never work again and the orchestrator did her job itself.
                // Keep the old flow's exact state as an archive and start fresh.
                let archive = path.parent().unwrap().join(format!("{}{}.json",
                    digest(previous.scope.turn_id.as_bytes()),
                    if context.kind == "terminal" && previous.pending_operation.is_none() { "" } else { ".interrupted" }));
                if !private_io::atomic_write_private_if_missing(&archive, &bytes)? {
                    ensure!(private_io::read_private_file_limited(&archive, MAX_STATE_BYTES)?.as_deref() == Some(bytes.as_slice()),
                        "Iris Design archive conflicts with the completed run");
                }
                Self::fresh(&bridge, scope, request, references).await?
            }
        } else {
            ensure!(workspace_owner.as_ref().is_none_or(|owner| owner.terminal),
                "Iris workspace ownership still names an unfinished design whose state is missing; refusing a new reference draw");
            Self::fresh(&bridge, scope, request, references).await?
        };
        let context = bridge_context(&bridge, &saved.state).await?;
        let replacement = serde_json::to_vec(&saved)?;
        if committed != replacement {
            private_io::compare_and_swap_private(&path,
                (!committed.is_empty()).then_some(committed.as_slice()), &replacement)?;
            committed = replacement;
        }
        let mut controller = Self { bridge, saved, path, committed, context, _claim: claim,
            _workspace_claim: workspace_claim, owner_path, preview: None };
        controller.save_owner()?;
        controller.check_attachments()?;
        Ok(Some(controller))
    }

    async fn fresh(bridge: &DesignBridge, scope: DesignScope, request: &str, references: Vec<PathBuf>) -> Result<SavedDesign> {
        let state = bridge.call(json!({
            "action": "start", "workspace": scope.workspace, "request": request,
            "references": references,
        })).await?;
        Ok(SavedDesign { version: 1, scope, original_request: request.into(), reference_paths: references,
            engine_hash: bridge.hash.clone(), state, attachments: BTreeMap::new(), pending_operation: None })
    }

    pub(crate) fn context(&self) -> &DesignContext { &self.context }
    pub(crate) fn browser_instance(&self) -> &str { &self.saved.scope.browser_instance }
    pub(crate) fn workspace(&self) -> &Path { &self.saved.scope.workspace }
    pub(crate) fn bridge_root(&self) -> &Path { &self.bridge.root }
    pub(crate) fn state_path(&self) -> &Path { &self.path }

    pub(crate) async fn start_preview(&mut self) -> Result<String> {
        let plan = self.context.preview_plan.as_ref().context("Iris Design capture has no validated preview plan")?.clone();
        let key = digest(&serde_json::to_vec(&(&self.saved.scope.session_id, &self.saved.scope.workspace))?);
        if self.preview.is_none() { self.preview = preview::take_retained(&key); }
        if let Some(service) = self.preview.as_mut() {
            if service.matches(&plan)? { return Ok(service.url.clone()); }
            self.preview.take().unwrap().stop().await?;
        }
        self.begin_operation("preview_start")?;
        let service = match preview::OwnedPreview::start(&self.bridge, self.workspace(), &plan, &self.saved.state).await {
            Ok(service) => service,
            Err(error) => {
                if error.downcast_ref::<preview::StartFailure>().is_some_and(|failure| failure.cleanup_confirmed) {
                    self.clear_operation()?;
                }
                return Err(error);
            }
        };
        let url = service.url.clone();
        self.preview = Some(service);
        self.clear_operation()?;
        Ok(url)
    }

    pub(crate) fn retain_preview(&mut self) -> Result<()> {
        if self.context.outcome.as_deref() == Some("passed") {
            if let Some(service) = self.preview.take() {
                let key = digest(&serde_json::to_vec(&(&self.saved.scope.session_id, &self.saved.scope.workspace))?);
                preview::retain(key, service)?;
            }
        }
        Ok(())
    }

    pub(crate) fn begin_capture(&mut self) -> Result<()> { self.begin_operation("native_capture") }
    pub(crate) fn finish_capture(&mut self) -> Result<()> { self.clear_operation() }

    pub(crate) async fn advance(&mut self, response: &str) -> Result<()> {
        self.check_attachments()?;
        let request = json!({"action":"advance", "state": self.saved.state, "output": response});
        self.begin_bridge_operation("advance", &request)?;
        let state = self.bridge.call(request).await?;
        self.accept_state(state).await
    }

    pub(crate) async fn capture(&mut self, preview_url: &str, screenshots: Vec<Value>) -> Result<()> {
        let request = json!({"action":"capture", "state": self.saved.state,
            "previewUrl": preview_url, "screenshots": screenshots});
        self.begin_bridge_operation("capture", &request)?;
        let state = self.bridge.call(request).await?;
        self.accept_state(state).await
    }

    pub(crate) async fn capture_unavailable(&mut self, reason: &str) -> Result<()> {
        let request = json!({"action":"capture_unavailable", "state": self.saved.state, "reason": reason});
        self.begin_bridge_operation("capture_unavailable", &request)?;
        let state = self.bridge.call(request).await?;
        self.accept_state(state).await
    }

    async fn accept_state(&mut self, state: Value) -> Result<()> {
        // Validate before committing. A transport error cannot advance the
        // durable phase or manufacture a successful terminal state.
        let context = bridge_context(&self.bridge, &state).await?;
        validate_phase_image_count(context.attachments.len())?;
        let mut replacement = self.saved.clone();
        replacement.state = state;
        replacement.pending_operation = None;
        let bytes = serde_json::to_vec(&replacement)?;
        ensure!(bytes.len() <= MAX_STATE_BYTES, "Iris Design state exceeds its byte limit");
        private_io::compare_and_swap_private(&self.path, Some(&self.committed), &bytes)?;
        self.saved = replacement;
        self.committed = bytes;
        self.context = context;
        self.save_owner()?;
        self.check_attachments()
    }

    fn begin_bridge_operation(&mut self, operation: &str, request: &Value) -> Result<()> {
        // A known protocol-size rejection is not an ambiguous in-flight write.
        // Refuse it before adding the durable pending-operation marker.
        bridge_request_bytes(request)?;
        self.begin_operation(operation)
    }

    fn begin_operation(&mut self, operation: &str) -> Result<()> {
        ensure!(self.saved.pending_operation.is_none(), "an earlier Iris Design phase operation remains uncommitted");
        let mut pending = self.saved.clone();
        pending.pending_operation = Some(operation.into());
        let bytes = serde_json::to_vec(&pending)?;
        private_io::compare_and_swap_private(&self.path, Some(&self.committed), &bytes)?;
        self.saved = pending;
        self.committed = bytes;
        Ok(())
    }

    fn clear_operation(&mut self) -> Result<()> {
        let mut saved = self.saved.clone();
        saved.pending_operation = None;
        let bytes = serde_json::to_vec(&saved)?;
        private_io::compare_and_swap_private(&self.path, Some(&self.committed), &bytes)?;
        self.saved = saved;
        self.committed = bytes;
        Ok(())
    }

    fn save_owner(&self) -> Result<()> {
        let owner = WorkspaceOwner { session_id: self.saved.scope.session_id.clone(),
            turn_id: self.saved.scope.turn_id.clone(), state_path: self.path.clone(),
            terminal: self.context.kind == "terminal" };
        private_io::atomic_write_private(&self.owner_path, &serde_json::to_vec(&owner)?)
    }

    fn check_attachments(&mut self) -> Result<()> {
        validate_phase_image_count(self.context.attachments.len())?;
        // Saved evidence is checked even when the next phase does not attach
        // it: deleting an earlier reference must not silently weaken review.
        for (path, expected) in &self.saved.attachments {
            ensure!(digest(&read_regular(path, MAX_IMAGE_BYTES)?) == *expected,
                "Iris Design reference or capture changed: {}; saved selection remains authoritative", path.display());
        }
        let mut changed = false;
        for path in &self.context.attachments {
            ensure!(path.is_absolute(), "Iris Design returned a relative image path");
            let hash = digest(&read_regular(path, MAX_IMAGE_BYTES)?);
            if let Some(expected) = self.saved.attachments.get(path) {
                ensure!(expected == &hash, "Iris Design image changed after selection: {}", path.display());
            } else {
                self.saved.attachments.insert(path.clone(), hash);
                changed = true;
            }
        }
        if changed {
            let bytes = serde_json::to_vec(&self.saved)?;
            private_io::compare_and_swap_private(&self.path, Some(&self.committed), &bytes)?;
            self.committed = bytes;
        }
        Ok(())
    }

    pub(crate) async fn image_message(&mut self, native_images: bool) -> Result<Option<crate::providers::ChatMessage>> {
        self.check_attachments()?;
        if self.context.attachments.is_empty() { return Ok(None); }
        ensure!(native_images, "Iris Design requires actual native image input for its selected references; this provider lane cannot inspect them");
        let mut images = Vec::new();
        let mut labels = Vec::new();
        let mut encoded_size = 0usize;
        for path in &self.context.attachments {
            let data = phase_image_data_uri(&self.saved.state, path).await?;
            // Detect replacement while pixels were encoded as well as before.
            ensure!(self.saved.attachments.get(path) == Some(&digest(&read_regular(path, MAX_IMAGE_BYTES)?)),
                "Iris Design image changed during native image preparation");
            encoded_size = encoded_size.checked_add(data.len()).context("Iris Design image budget overflow")?;
            ensure!(encoded_size <= MAX_ATTACHMENT_BYTES, "Iris Design reference images exceed the native image byte limit");
            labels.push(format!("{}: {}", images.len() + 1, path.display()));
            images.push(data);
        }
        Ok(Some(crate::providers::ChatMessage::user_with_images(
            format!("TasteCode {} phase image inputs. Exact selected files, in order:\n{}", self.context.phase, labels.join("\n")), images)))
    }
}

/// Settle the persisted Iris Design flow after the foreground runtime task has
/// been explicitly aborted. JoinHandle::abort drops the agent future before
/// its ordinary finalization code can run, so cancellation must close the
/// design state from the surviving gateway task after the aborted handle has
/// joined.
///
/// This is deliberately synchronous and does not invoke the JavaScript design
/// engine. The engine only returns successor state; it has no cancellation
/// action. A stopped turn keeps its last accepted phase/artifacts, changes the
/// status to the engine-supported terminal failed state, and records why the
/// flow ended.
pub(crate) fn finalize_cancelled_turn(
    state_root: &Path,
    session_id: &str,
    workspace: &Path,
) -> Result<bool> {
    let workspace = workspace
        .canonicalize()
        .context("Iris cancellation workspace is unavailable")?;
    let state_key = digest(&serde_json::to_vec(&(session_id, &workspace))?);
    let path = state_root
        .join("iris_design")
        .join(state_key)
        .join("active.json");
    if !path.try_exists()? {
        return Ok(false);
    }
    let owner_path = state_root
        .join("iris_design")
        .join("workspaces")
        .join(format!(
            "{}.json",
            digest(workspace.to_string_lossy().as_bytes())
        ));

    // Match open's claim order so a new turn cannot race cancellation
    // finalization and inherit a half-settled state/owner pair.
    let _workspace_claim =
        private_io::try_execution_claim(&owner_path.with_extension("execution"))?
            .context("another Iris execution owns this workspace while cancellation is settling")?;
    let _claim = private_io::try_execution_claim(&path.with_extension("execution"))?
        .context("this Iris design flow is still owned while cancellation is settling")?;

    let Some(committed) = private_io::read_private_file_limited(&path, MAX_STATE_BYTES)? else {
        return Ok(false);
    };
    let mut saved: SavedDesign = serde_json::from_slice(&committed)
        .context("Iris Design state is corrupt while cancellation is settling")?;
    ensure!(
        saved.scope.session_id == session_id && saved.scope.workspace == workspace,
        "Iris cancellation state does not belong to this conversation/workspace"
    );

    let state = saved
        .state
        .as_object_mut()
        .context("Iris Design state is not an object while cancellation is settling")?;
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .context("Iris Design state has no status while cancellation is settling")?;
    if matches!(status, "complete" | "failed") {
        return Ok(false);
    }
    ensure!(
        matches!(status, "running" | "waiting"),
        "Iris Design state has an unsupported active status"
    );
    let phase = state
        .get("phase")
        .and_then(Value::as_str)
        .context("Iris Design state has no phase while cancellation is settling")?
        .to_string();
    let revision = state
        .get("revision")
        .and_then(Value::as_u64)
        .context("Iris Design state has no revision while cancellation is settling")?;
    ensure!(
        revision < 1000,
        "Iris Design cancellation cannot advance an exhausted state revision"
    );

    state.insert("revision".into(), Value::from(revision + 1));
    state.insert("status".into(), Value::String("failed".into()));
    state.insert("correcting".into(), Value::Bool(false));
    state.insert(
        "error".into(),
        json!({
            "code": "TURN_STOPPED",
            "phase": phase,
            "message": "Managed Iris turn stopped before the staged design workflow completed."
        }),
    );
    for key in [
        "awaitingCapture",
        "pendingPrompt",
        "correctionErrors",
        "outcome",
        "completion",
    ] {
        state.remove(key);
    }
    // Explicit cancellation has joined the aborted task, so any bridge child
    // was killed on drop and cannot later commit output. Do not leave the
    // crash-recovery marker blocking a future authored run.
    saved.pending_operation = None;
    let replacement = serde_json::to_vec(&saved)?;
    ensure!(
        replacement.len() <= MAX_STATE_BYTES,
        "Iris Design cancellation state exceeds its byte limit"
    );

    let owner_committed = private_io::read_private_file_limited(&owner_path, 16_384)?
        .context("Iris saved design has no workspace ownership record during cancellation")?;
    let mut owner: WorkspaceOwner = serde_json::from_slice(&owner_committed)
        .context("Iris workspace ownership record is corrupt during cancellation")?;
    ensure!(
        owner.session_id == saved.scope.session_id
            && owner.turn_id == saved.scope.turn_id
            && owner.state_path == path,
        "Iris workspace ownership changed before cancellation could settle"
    );
    owner.terminal = true;
    let owner_replacement = serde_json::to_vec(&owner)?;

    private_io::compare_and_swap_private(&path, Some(&committed), &replacement)?;
    private_io::compare_and_swap_private(
        &owner_path,
        Some(&owner_committed),
        &owner_replacement,
    )?;
    Ok(true)
}

fn bridge_request_bytes(request: &Value) -> Result<Vec<u8>> {
    let input = serde_json::to_vec(request)?;
    ensure!(input.len() <= MAX_BRIDGE_BYTES,
        "Iris Design bridge request exceeds its 8 MiB byte limit; no engine process was started");
    Ok(input)
}

fn validate_phase_image_count(count: usize) -> Result<()> {
    ensure!(count <= MAX_PHASE_ATTACHMENTS,
        "Iris Design phase exceeds its image count limit, including reference and review captures");
    Ok(())
}

async fn phase_image_data_uri(state: &Value, path: &Path) -> Result<String> {
    let is_capture = state.get("screenshots").and_then(Value::as_array)
        .is_some_and(|shots| shots.iter().any(|shot|
            shot.get("path").and_then(Value::as_str).is_some_and(|name| Path::new(name) == path)));
    if is_capture {
        crate::runtime::vision::native_design_capture_data_uri(path).await
    } else {
        crate::runtime::vision::native_screen_data_uri(path).await
    }
}

async fn bridge_context(bridge: &DesignBridge, state: &Value) -> Result<DesignContext> {
    let context: DesignContext = serde_json::from_value(bridge.call(json!({"action":"context", "state":state})).await?)?;
    ensure!(matches!(context.kind.as_str(), "model" | "capture" | "terminal"), "Iris Design returned an unknown context kind");
    if context.kind == "model" {
        ensure!(context.prompt.as_deref().is_some_and(|text| !text.trim().is_empty()), "Iris Design model phase has no prompt");
    }
    Ok(context)
}

fn validate_owner(saved: &SavedDesign, scope: &DesignScope, engine: &str) -> Result<()> {
    ensure!(saved.version == 1, "unsupported Iris Design state version");
    ensure!(saved.scope.session_id == scope.session_id && saved.scope.workspace == scope.workspace
        && saved.scope.browser_instance == scope.browser_instance,
        "Iris Design state belongs to another conversation, workspace or browser; refusing foreign replay");
    ensure!(saved.engine_hash == engine, "Iris Design state belongs to another engine version; refusing to redraw its references");
    ensure!(!saved.scope.turn_id.is_empty() && !saved.original_request.trim().is_empty(), "Iris Design state lacks its authored turn identity");
    Ok(())
}

fn validate_resume_identity(saved: &SavedDesign, scope: &DesignScope, request: &str,
    references: &[PathBuf], continuation: bool) -> Result<()> {
    if saved.scope.turn_id == scope.turn_id {
        ensure!(saved.original_request == request,
            "Iris authored turn identity was reused with a different request; refusing stale design state");
    }
    if continuation || saved.scope.turn_id == scope.turn_id {
        ensure!(references.is_empty() || references == saved.reference_paths,
            "Iris continuation changed its reference inputs; an explicit design amendment is required before changing the saved reference selection");
    }
    Ok(())
}

fn validate_workspace_owner(owner: Option<&WorkspaceOwner>, saved: &SavedDesign, state_path: &Path) -> Result<()> {
    let owner = owner.context("Iris saved design has no workspace ownership record; refusing unconfirmed replay")?;
    ensure!(owner.session_id == saved.scope.session_id && owner.turn_id == saved.scope.turn_id
        && owner.state_path == state_path,
        "Iris workspace ownership does not match the exact saved session, turn and state path; refusing foreign or stale replay");
    Ok(())
}

fn validate_reference_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    ensure!(paths.len() <= MAX_ATTACHMENTS, "too many authored Iris reference images");
    let mut canonical = Vec::new();
    for path in paths {
        ensure!(path.is_absolute(), "authored Iris reference paths must be absolute");
        read_regular(path, MAX_IMAGE_BYTES)?;
        let path = path.canonicalize()?;
        ensure!(!canonical.contains(&path), "authored Iris reference images contain a duplicate");
        canonical.push(path);
    }
    Ok(canonical)
}

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    private_io::reject_symlink_components(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK); }
    let file = options.open(path).with_context(|| format!("cannot read Iris Design input {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file() && metadata.len() <= limit as u64, "Iris Design input must be a bounded regular file: {}", path.display());
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "Iris Design input grew beyond its byte limit");
    Ok(bytes)
}

fn engine_hash(root: &Path) -> Result<String> {
    let mut files = vec![root.join("scripts/iris-design-runtime.mjs"),
        root.join("vendor/tastecode-design/MANIFEST.json")];
    let dist = root.join("vendor/tastecode-design/dist");
    let mut pending = vec![dist];
    while let Some(directory) = pending.pop() {
        private_io::reject_symlink_components(&directory)?;
        for entry in std::fs::read_dir(&directory).context("Iris Design engine bundle is missing")? {
            let entry = entry?;
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "Iris Design engine contains a symlink");
            if kind.is_dir() { pending.push(entry.path()); }
            else if kind.is_file() { files.push(entry.path()); }
            else { bail!("Iris Design engine contains an unsupported file type"); }
            ensure!(files.len() + pending.len() <= 1024, "Iris Design engine bundle has too many entries");
        }
    }
    files.sort();
    let mut hash = Sha256::new();
    let mut total = 0usize;
    for file in files {
        let bytes = read_regular(&file, MAX_STATE_BYTES)?;
        total = total.checked_add(bytes.len()).context("Iris Design engine size overflow")?;
        ensure!(total <= MAX_STATE_BYTES, "Iris Design engine bundle exceeds its byte limit");
        hash.update(file.strip_prefix(root)?.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests;
