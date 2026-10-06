//! Iris-only wiring for TasteCode's staged controller, inside the normal mesh.

use super::*;
use crate::providers::CompletionResponse;
use crate::runtime::iris_design::{DesignContext, IrisDesignController};
use crate::runtime::prompt::PromptAssembly;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct DesignAction {
    receipt: Option<crate::runtime::extensions::ActionReceipt>,
    leases: Vec<crate::runtime::asks::ProtectedActionLease>,
}

impl DesignAction {
    fn finish(mut self, success: bool, confirmed: bool, output: &str) {
        let result = if !confirmed { crate::runtime::asks::ProtectedActionResult::Blocked }
            else if success { crate::runtime::asks::ProtectedActionResult::Succeeded }
            else { crate::runtime::asks::ProtectedActionResult::Failed };
        super::turn_loop::finish_action_leases(&self.leases, result, output);
        self.leases.clear();
        if let Some(receipt) = self.receipt.take() {
            crate::runtime::extensions::finish_action(receipt, success, confirmed, output);
        }
    }
}

impl Drop for DesignAction {
    fn drop(&mut self) {
        let output = "Iris preview operation ended without confirmed completion; it was not accepted or retried.";
        super::turn_loop::finish_action_leases(&self.leases,
            crate::runtime::asks::ProtectedActionResult::Blocked, output);
        if let Some(receipt) = self.receipt.take() {
            crate::runtime::extensions::finish_action(receipt, false, false, output);
        }
    }
}

struct CancelCapture(Arc<AtomicBool>);
impl Drop for CancelCapture {
    fn drop(&mut self) { self.0.store(true, Ordering::Release); }
}

impl MeshRunner {
    async fn iris_action(
        &self, executor: &Arc<ToolExecutor>, session: &Session, spec: &AgentSpec,
        tool: &str, input: &Value, summary: &str,
    ) -> Result<DesignAction> {
        anyhow::ensure!(spec.tool_allowlist.iter().any(|name| name == tool)
            && self.group_context.as_ref().is_none_or(|group| group.permits_tool("frontend", tool)),
            "Iris preview operation `{tool}` is unavailable in this assignment");
        let authorized = self.executor_for_tool_call(executor, &agent_display_name(&spec.name),
            tool, input, summary).await.context("Iris preview permission was not granted")?;
        let settings_scope = self.group_context.as_ref()
            .map(|group| crate::settings::SettingsScope::Group { id: group.group_id.clone() })
            .unwrap_or_else(|| crate::settings::SettingsScope::Agent { id: "frontend".into() });
        let receipt = crate::runtime::extensions::begin_action(crate::runtime::extensions::ActionContext::new(
            Some(session.id.clone()), "frontend".into(), tool.into(), summary.into(), input.clone(),
            &self.workspace_root, authorized.executor.permission_mode().as_str().into(), "execute".into(),
            crate::settings::effective_string("permissions.action_review", &settings_scope).unwrap_or_else(|| "shadow".into())));
        let action = DesignAction { receipt: Some(receipt), leases: authorized.approval_leases };
        if let Some(reason) = action.receipt.as_ref().and_then(|receipt| receipt.block_reason()) {
            action.finish(false, true, &reason);
            anyhow::bail!("{reason}");
        }
        self.emit(CliEvent::ToolCallStarted { agent: agent_display_name(&spec.name),
            tool_name: tool.into(), input_summary: summary.into() });
        Ok(action)
    }

    fn iris_result(&self, session: &mut Session, results: &mut Vec<ToolCallResult>, spec: &AgentSpec,
        tool: &str, input: &Value, summary: &str, success: bool, output: String) {
        self.emit(CliEvent::ToolCallCompleted { agent: agent_display_name(&spec.name), tool_name: tool.into(),
            input_summary: summary.into(), success, output_summary: first_line(&output).to_owned(), diff: None });
        session.push_message(Message::ToolResult { tool_name: tool.into(), input: input.to_string(), success, output: output.clone() });
        results.push(ToolCallResult { tool_name: tool.into(), input_summary: summary.into(), success, output });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_iris_preview(
        &self, design: &mut IrisDesignController, executor: &Arc<ToolExecutor>, spec: &AgentSpec,
        deadline: Option<tokio::time::Instant>, session: &mut Session, results: &mut Vec<ToolCallResult>,
        tool_count: &mut usize, tool_limit: Option<usize>,
    ) -> Result<()> {
        let plan = design.context().preview_plan.as_ref().context("Iris preview lacks its validated plan")?.clone();
        let kind = plan.get("kind").and_then(Value::as_str).context("Iris preview lacks its validated kind")?;
        anyhow::ensure!(crate::runtime::iris_design::preview_permitted(executor.permission_mode(), kind),
            "Iris command preview requires the existing Full Access posture: the upstream command launcher cannot omit Workspace shell isolation. The design and review remain unfinished; no preview child was started");
        anyhow::ensure!(tool_limit.is_none_or(|limit| tool_count.saturating_add(2) <= limit),
            "the user tool-call limit leaves no room for preview startup and verified capture");
        anyhow::ensure!(self.group_context.as_ref().is_none_or(|group| group.permits_tool("frontend", "browser_navigate")),
            "this assignment cannot navigate Iris's managed browser");
        let (start_tool, start_input, start_summary) = if kind == "command" {
            let mut argv = vec![plan.get("command").and_then(Value::as_str).context("missing preview command")?];
            argv.extend(plan.get("args").and_then(Value::as_array).context("missing preview argv")?
                .iter().map(|arg| arg.as_str().context("invalid preview argument")).collect::<Result<Vec<_>>>()?);
            // Permission/audit representation only; actual execution remains
            // upstream executable + argv, with no native shell interpolation.
            let command = argv.iter().map(|arg| format!("'{}'", arg.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ");
            ("bash", serde_json::json!({"command":command,"cwd":plan.get("cwd"),"iris_preview_plan":plan}),
                format!("Start or reuse the exact owned local preview: {command}"))
        } else {
            let entry = design.workspace().join(plan.get("cwd").and_then(Value::as_str).unwrap_or("."))
                .join(plan.get("entry").and_then(Value::as_str).context("missing static preview entry")?);
            ("read", serde_json::json!({"path":entry,"iris_preview_plan":plan}),
                "Serve the validated workspace static entry in the owned local preview".to_string())
        };
        let action = self.iris_action(executor, session, spec, start_tool, &start_input, &start_summary).await?;
        *tool_count += 1;
        let actual_url = match design.start_preview().await {
            Ok(url) => {
                let output = format!("Owned Iris preview ready at {url}. The runtime retains the service and closes its input to stop this exact server.");
                action.finish(true, true, &output);
                self.iris_result(session, results, spec, start_tool, &start_input, &start_summary, true, output);
                url
            }
            Err(error) => {
                let output = format!("Iris preview startup failed: {error:#}. Readiness was not accepted; cleanup may still be completing.");
                action.finish(false, false, &output);
                self.iris_result(session, results, spec, start_tool, &start_input, &start_summary, false, output);
                return Err(error);
            }
        };
        let viewports = plan.get("viewports").and_then(Value::as_array).context("missing preview viewports")?
            .iter().map(|viewport| Ok((
                u32::try_from(viewport.get("width").and_then(Value::as_u64).context("invalid preview width")?)?,
                u32::try_from(viewport.get("height").and_then(Value::as_u64).context("invalid preview height")?)?,
            ))).collect::<Result<Vec<_>>>()?;
        let input = serde_json::json!({"url":actual_url,"viewports":plan["viewports"],"full_page":true,"design_audit":true});
        let summary = "Capture every approved viewport and audit the actual DOM in Iris's owned browser";
        let action = self.iris_action(executor, session, spec, "browser_screenshot", &input, summary).await?;
        *tool_count += 1;
        design.begin_capture()?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelCapture(Arc::clone(&cancellation));
        let instance = design.browser_instance().to_string();
        let url = actual_url.clone();
        // The review capture needs no logins, so it always runs in a
        // disposable job-scoped headless browser. The coworker's embedded
        // browser is hidden whenever the user is looking at another
        // conversation, and a hidden view never paints the frame capture and
        // input need, which failed finished builds at Preview.
        let capture = tokio::task::spawn_blocking(move || {
            let review = format!("{instance}{}", crate::tools::browser_native::DESIGN_REVIEW_INSTANCE_SUFFIX);
            let shots = crate::tools::browser_native::capture_design_preview(&review, &url, &viewports, cancellation);
            crate::tools::browser_native::close_instance_for_desktop(&review);
            shots
        });
        let capture_deadline = deadline.map_or_else(|| tokio::time::Instant::now() + Duration::from_secs(120),
            |deadline| deadline.min(tokio::time::Instant::now() + Duration::from_secs(120)));
        let captured = tokio::time::timeout_at(capture_deadline, capture).await;
        let (shots, confirmed, problem) = match captured {
            Ok(Ok(Ok(shots))) => (Some(shots), true, None),
            Ok(Ok(Err(error))) => (None, true, Some(format!("{error:#}"))),
            Ok(Err(error)) => (None, false, Some(error.to_string())),
            Err(_) => (None, false, Some("the owned browser capture exceeded its budget; an in-flight browser operation is unconfirmed".into())),
        };
        if let Some(problem) = problem {
            action.finish(false, confirmed, &problem);
            self.iris_result(session, results, spec, "browser_screenshot", &input, summary, false, problem.clone());
            if confirmed { design.finish_capture()?; }
            anyhow::bail!("Iris visual review remains unfinished: {problem}");
        }
        let shots = shots.context("Iris capture returned no evidence")?;
        let output = serde_json::json!({"url":actual_url,"screenshots":shots}).to_string();
        action.finish(true, true, &output);
        self.iris_result(session, results, spec, "browser_screenshot", &input, summary, true, output);
        design.finish_capture()?;
        design.capture(&actual_url, shots).await
    }
}

/// What Iris needs to open a design run from her own `design_website` call.
pub(super) struct DesignLaunch {
    pub session_id: String,
    pub turn_id: String,
    pub browser_instance: String,
    pub references: Vec<std::path::PathBuf>,
}

/// Open the staged design workflow when Iris decides the job is a website.
/// A keyword guess used to start it for praise ("those components make the
/// website feel alive") and ran it over Phoenix's own repository.
pub(super) async fn launch_from_tool(
    state_root: &Path, launch: &DesignLaunch, input: &Value,
) -> Result<Option<crate::runtime::iris_design::IrisDesignController>> {
    let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty());
    let brief = text("brief").context("design_website needs the full brief")?;
    let folder = std::path::PathBuf::from(text("folder").context("design_website needs the site's folder")?);
    anyhow::ensure!(folder.is_absolute(), "folder must be an absolute path to the site's own folder");
    std::fs::create_dir_all(&folder).with_context(|| format!("cannot create {}", folder.display()))?;
    let folder = folder.canonicalize()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = source.canonicalize().unwrap_or_else(|_| source.to_path_buf());
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    anyhow::ensure!(!source.starts_with(&folder) && folder != source && Some(&folder) != home.as_ref(),
        "{} holds Phoenix's own source or your whole home folder; give the site its own folder", folder.display());
    let resume = input.get("resume").and_then(Value::as_bool).unwrap_or(false);
    crate::runtime::iris_design::IrisDesignController::open(state_root,
        crate::runtime::iris_design::DesignScope {
            session_id: launch.session_id.clone(), turn_id: launch.turn_id.clone(),
            workspace: folder, browser_instance: launch.browser_instance.clone(),
        }, brief, resume, &launch.references).await
}

pub(super) fn visual_progress(building: bool, inspection: bool, managed: bool) -> crate::runtime::visual_progress::VisualProgress {
    // VisualProgress can reactivate its legacy aesthetic guidance on writes.
    // The managed path already has upstream review; retain action observation
    // while suppressing that reactivation, then restore ordinary behavior when
    // qualification ends with not_design.
    crate::runtime::visual_progress::VisualProgress::for_assignment(building && !managed, inspection || managed)
}

pub(super) fn phase_effort(context: Option<&DesignContext>, configured: Option<String>) -> Option<String> {
    if context.is_some_and(|context| context.kind == "model" && context.phase == "brief") {
        Some("low".into())
    } else { configured }
}

fn projected_session(session: &Session, workspace: &Path) -> Session {
    let mut projected = session.clone();
    projected.pinned_refs.retain(|pin| pin.tool != "design_reference"
        && !(pin.tool == "skill"
            && crate::tools::skills::named_skill_is_visual_build(Some(workspace), &pin.key)));
    projected
}

fn apply_phase(prompt: &mut PromptAssembly, design: &IrisDesignController) {
    // These paragraphs belong to the superseded aesthetic workflow. Keep the
    // rest of the shared trust/permission/context contract byte-for-byte.
    prompt.system_prompt = prompt.system_prompt.split("\n\n").filter(|paragraph| {
        !["Visual-design gate.", "For visual artifacts, establish", "Ground the work before building.",
            "Work every substantial deliverable in four passes:", "No unsolicited fluff in coding or UI work."]
            .iter().any(|prefix| paragraph.starts_with(prefix))
    }).collect::<Vec<_>>().join("\n\n");
    let context = design.context();
    if context.kind == "model" {
        prompt.system_prompt.push_str("\n\nThe current internal TasteCode phase below controls design decisions and phase output. Preserve all permission, cancellation and workspace rules. Return its requested JSON as the phase result; Phoenix consumes that result internally and advances the next phase. It is not a final answer to the user. Ordinary tool calls remain available within this phase's scope.");
        if let Some(guidance) = phase_content_guidance(context) {
            prompt.system_prompt.push_str(guidance);
        }
        if let Some(guidance) = phase_collaboration_guidance(context) {
            prompt.system_prompt.push_str(guidance);
        }
        if let Some(guidance) = phase_imagery_guidance(context) {
            prompt.system_prompt.push_str(guidance);
        }
        if let Some(guidance) = phase_component_guidance(context) {
            prompt.system_prompt.push_str(guidance);
        }
        prompt.user_prompt.push_str("\n\n=== CURRENT INTERNAL DESIGN PHASE ===\n");
        prompt.user_prompt.push_str(context.prompt.as_deref().unwrap_or_default());
    } else if context.kind == "terminal" && context.outcome.as_deref() == Some("passed") {
        prompt.system_prompt.push_str("\n\nThe staged design workflow has completed its validated visual review. Its JSON phase protocol is finished. Reconcile the existing runtime task list and workflow evidence, then use final_answer to report the actual result. Do not change the approved project after review. Any remaining real blocker must be reported as incomplete.");
    }
}

/// A site commissioned for a business or product the user names is the
/// user's own subject, not third-party concept work. TasteCode's concept
/// doctrine plus its numeric-claim check pushed Iris to disclaim the client's
/// own site ("not official terms", an FAQ asking whether the product is real,
/// "illustrative" on every block), which no client would ship.
fn phase_content_guidance(context: &DesignContext) -> Option<&'static str> {
    (context.kind == "model" && matches!(context.phase.as_str(), "brief" | "page" | "build" | "repair" | "review"))
        .then_some("\n\nWhen the user commissions a site for a business or product they name, that business, its offer and the sections they asked for are the user's supplied subject, not concept work about a third party. Write the page as that business would publish it. Never add copy that questions whether it is real: no 'not official' or 'concept site' notices, no FAQ about whether the product exists, and no 'illustrative' tag repeated across blocks. Specifics the user did not supply (prices, hours, address, contact details) get realistic placeholder values; record the user's request as that section's evidence (for example 'User requested three pricing tiers; placeholder prices for the owner to confirm') instead of labelling the page, and list every placeholder in the final verification note for the user. Still never invent testimonials, client logos, endorsements, awards or measured outcomes. Every visible card, tile and panel carries real content or imagery; a colour block holding only a label or number is unfinished.")
}

/// TasteCode prefers generating "original" visuals, so a fictional product
/// became a Blender render and the page read as CGI. The user wants real
/// photography sourced from the web unless they ask for generated art.
fn phase_imagery_guidance(context: &DesignContext) -> Option<&'static str> {
    (context.kind == "model" && matches!(context.phase.as_str(), "page" | "assets" | "asset" | "build" | "repair"))
        .then_some("\n\nImages come from the web: real photographs found with web or image search, downloaded at full resolution with the source page and license recorded. This overrides the phase's preference for image generation, including for new or fictional products: pick real photos of the closest matching subject and material. Do not generate images, render 3D models (Blender or otherwise), or draw substitute visuals unless the user explicitly asked for generated or 3D art.")
}

/// TasteCode treats buttons, cards and motion as hand-written Build work and
/// only sources components from OriginKit, so Iris never used the user's
/// component catalogs. These rules make the catalogs the component source:
/// Page names the needs, Assets downloads the real code, Build installs it.
fn phase_component_guidance(context: &DesignContext) -> Option<&'static str> {
    if context.kind != "model" { return None; }
    match context.phase.as_str() {
        "page" => Some("\n\nComponent needs: this page's buttons and calls to action, navigation, link hovers, interactive or hover cards, tabs, accordions, menus, counters, marquees, text effects and section transitions come from real component catalogs, not hand-written Build work. Give each distinct one a snake-case componentNeeds ID in the sections that show it (for example primary_button, nav_bar, feature_card_hover, section_reveal). This replaces the phase's instruction to leave cards and anchors out of componentNeeds; plain headings, paragraphs and grid layout stay Build work."),
        "assets" | "asset" => Some(concat!("\n\nAcquire every componentNeeds entry as real code from the catalogs below, never as a description or a local rewrite. For each need: read the registry list, shortlist two or three candidates that fit the approved brand and reference, open each one's demo or docs page in the browser and look at it, then pick the most distinctive fit (a magnetic, metallic or morphing button beats a flat default). Download the chosen item's registry JSON unchanged to .taste/components/<need-id>.json (for Transitions.dev, save its page's CSS or React source as .taste/components/<need-id>.css or .tsx), and fetch its registryDependencies the same way. Record it with kind and role component, status ready, source.kind external, source.reference set to the item's demo or docs page URL, source.license set to the catalog's terms (MIT for shadcn/ui and Libraries.dev; otherwise 'free to use: ' plus the site or repository URL), and destination set to the saved file. This replaces the phase's OriginKit-only component search.\n\n", include_str!("../../../prompts/iris_component_catalogs.md"))),
        "build" | "repair" => Some("\n\nComponents are installed from .taste/components, not written from memory. Unless the project already has a framework, build the page as a Next.js app, the stack these catalogs are written for (their files use \"use client\", next/link and next/navigation): create-next-app refuses a folder that already holds .taste and assets, so scaffold into a subfolder and copy it up: `npx create-next-app@latest site-scaffold --ts --tailwind --app --src-dir --eslint --import-alias \"@/*\" --use-npm --disable-git --yes && cp -a site-scaffold/. . && rm -rf site-scaffold`. Delete the scaffold's public/*.svg files and demo page content, and copy (never move) the approved assets into public/ so the page can serve them; set `output: \"export\"` and `images: { unoptimized: true }` in next.config so the site ships as static files, run `npx shadcn@latest init -d`, then `npx shadcn@latest add -y <item-json-url>` for each saved registry component (it installs the component's dependencies and helper files). Libraries.dev effects install from npm; Transitions.dev patterns use their React version or their CSS exactly as given. Use each component through its props and the approved brand tokens (colour, type, radius). Do not rewrite, simplify or restyle a catalog component into a generic version, and never hand-build a button or effect a saved component already covers. A component's own durations, easings and springs are the approved motion for that element. Keep a catalog component's own inline SVG exactly as shipped; the source check accepts SVG that matches the saved catalog code. Every other icon comes from lucide-react or the component's icon package, never hand-drawn inline <svg>, which the source check rejects. Set the dev script to `next dev -H 127.0.0.1 -p 5173` for Preview, and confirm `npm run build` succeeds and the page renders."),

        _ => None,
    }
}

fn phase_collaboration_guidance(context: &DesignContext) -> Option<&'static str> {
    (context.kind == "model" && matches!(context.phase.as_str(), "brief" | "brand" | "page" | "assets" | "build"))
        .then_some("\n\nUse the Phoenix team when outside evidence would materially improve this phase. For an image-led, place-based, physical-subject, category-specific, or unfamiliar-interface build, proactively ask Theo through `talk` for a bounded reference/source pass instead of doing all reference discovery alone. Ask for varied real examples, useful source observations, and image/source options when imagery matters; incorporate the returned evidence into the current TasteCode decisions. Iris remains accountable for the final visual system and must not hand off the design itself.")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_prompt(
    spec: &AgentSpec, task: &TaskEnvelope, loaded: &LoadedMemories, session: &Session,
    workspace: &Path, provider: &str, project_context: Option<&str>,
    design: Option<&IrisDesignController>,
) -> PromptAssembly {
    let projected;
    let source = if design.is_some() { projected = projected_session(session, workspace); &projected } else { session };
    let mut prompt = assemble_prompt_with_context(spec, task, loaded, source,
        Some(workspace), Some(provider), project_context);
    if let Some(design) = design { apply_phase(&mut prompt, design); }
    prompt
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_native_prompt(
    spec: &AgentSpec, task: &TaskEnvelope, loaded: &LoadedMemories, session: &Session,
    suffix_start: usize, workspace: &Path, provider: &str, project_context: Option<&str>,
    design: Option<&IrisDesignController>,
) -> Option<PromptAssembly> {
    let projected;
    let source = if design.is_some() { projected = projected_session(session, workspace); &projected } else { session };
    let mut prompt = crate::runtime::prompt::assemble_prompt_with_native_suffix(spec, task, loaded, source,
        suffix_start, Some(workspace), Some(provider), project_context)?;
    if let Some(design) = design { apply_phase(&mut prompt, design); }
    Some(prompt)
}

pub(super) fn phase_tool_allowed(context: &DesignContext, name: &str) -> bool {
    // Creative output has a single authority in this path. Loading a second
    // pinned aesthetic skill would restore the conflicting legacy doctrine.
    if matches!(name, "design_reference" | "design_studio" | "skill" | "skill_search" | "skill_install") {
        return false;
    }
    if matches!(name, "final_answer" | "routine") { return true; }
    if context.kind == "terminal" {
        return matches!(name, "read" | "grep" | "glob" | "list_directory" | "work" | "todo_write"
            | "symbol_search" | "file_symbols" | "callers");
    }
    // Iris may need current references before TasteCode has committed the
    // composition. Keep this narrow: the early creative phases can ask one of
    // Phoenix's visible coworkers (normally Theo) for evidence, but they still
    // cannot mutate the workspace or load a competing design system.
    if name == "talk" && matches!(context.phase.as_str(), "brief" | "brand" | "page") {
        return true;
    }
    // Photography is sourced from the web; see phase_imagery_guidance.
    if name == "image_gen" { return false; }
    if matches!(context.phase.as_str(), "assets" | "asset" | "build" | "repair") { return true; }
    matches!(name, "read" | "grep" | "glob" | "list_directory" | "symbol_search" | "file_symbols"
        | "callers" | "work" | "todo_write" | "image_analyze" | "browser_state" | "browser_screenshot"
        | "browser_console" | "recall" | "memory_recall")
}

fn unwrap_final(value: &Value) -> Option<String> {
    if let Some(raw) = value.as_str() { return Some(raw.into()); }
    // Never search arbitrary prose for a JSON fragment. Upstream owns strict
    // parsing, including the whole-fence format and unknown-key policy.
    for key in ["final_markdown", "output", "content", "answer"] {
        if let Some(raw) = value.get(key).and_then(Value::as_str) { return Some(raw.into()); }
    }
    if let Some(final_value) = value.get("final_answer") { return unwrap_final(final_value); }
    if value.get("tool_name").or_else(|| value.get("name")).and_then(Value::as_str) == Some("final_answer") {
        return value.get("arguments").or_else(|| value.get("input")).and_then(unwrap_final);
    }
    // Some native adapters accept phase JSON directly in final_answer args.
    if value.get("status").is_some() || value.get("version").is_some() || value.get("verdict").is_some() {
        return Some(value.to_string());
    }
    value.get("summary").and_then(Value::as_str).map(str::to_owned)
}

/// None means ordinary tool execution. Some means an internal phase response,
/// including malformed output which upstream will correct once, then fail.
pub(super) fn phase_output(response: &CompletionResponse) -> Option<String> {
    if !response.tool_calls.is_empty() {
        let finals: Vec<_> = response.tool_calls.iter().filter(|call| call.tool_name == "final_answer").collect();
        if finals.is_empty() { return None; }
        if finals.len() != 1 || response.tool_calls.len() != 1 {
            return Some("A phase result must be returned alone; this mixed final_answer/tool batch was not executed.".into());
        }
        return Some(unwrap_final(&finals[0].arguments).unwrap_or_else(|| finals[0].arguments.to_string()));
    }
    if let Ok(AgentTurnResponse::ToolRequest { tool_calls, .. }) = parse_agent_turn_response(&response.content) {
        if !tool_calls.iter().any(|call| call.tool_name == "final_answer") { return None; }
        if tool_calls.len() != 1 {
            return Some("A phase result must be returned alone; this mixed final_answer/tool batch was not executed.".into());
        }
        return Some(unwrap_final(&tool_calls[0].input).unwrap_or_else(|| tool_calls[0].input.to_string()));
    }
    let raw = response.content.trim();
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if value.get("final_markdown").is_some() || value.get("final_answer").is_some()
            || value.get("type").and_then(Value::as_str) == Some("final") {
            return Some(unwrap_final(&value).unwrap_or_else(|| raw.into()));
        }
    }
    Some(raw.into())
}

pub(super) fn retain_internal_reply(response: &CompletionResponse, messages: &mut Vec<ChatMessage>, phase: &str) {
    if !response.tool_calls.is_empty() {
        let mut reply = ChatMessage::assistant_reply(response);
        reply.content.clear();
        messages.push(reply);
        for call in &response.tool_calls {
            messages.push(ChatMessage::tool_result(&call.id,
                format!("Internal design controller consumed this response. Current phase: {phase}. No other tool in this response was executed.")));
        }
    }
}

pub(super) fn terminal_error(design: &IrisDesignController) -> Option<String> {
    let context = design.context();
    if context.kind != "terminal" || matches!(context.outcome.as_deref(), Some("passed" | "not_design")) {
        return None;
    }
    Some(format!("Iris Design did not complete: {} (phase {}, status {}). {} Preserved state: {}",
        context.outcome.as_deref().unwrap_or("failed"), context.phase, context.status,
        context.error.as_ref().map(Value::to_string).unwrap_or_default(), design.state_path().display()))
}

#[cfg(test)]
#[path = "../iris_design/mesh_tests.rs"]
mod tests;
