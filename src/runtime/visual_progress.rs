//! Advisory feedback for successful desktop input that keeps returning to the
//! same pixels. A tool receipt proves input delivery, not task progress.
use std::collections::VecDeque;

use serde_json::Value;
use sha2::{Digest, Sha256};

const SURFACES: usize = 8;
const HISTORY: usize = 8;
const REPEATS: usize = 4;

#[derive(Default)]
pub(crate) struct VisualProgress {
    mutation: u64,
    surfaces: VecDeque<Surface>,
    quality_reviews: bool,
    inspection_only: bool,
    quality_reviewed: bool,
    last_quality_review: u64,
    unreviewed_renders: u8,
}

struct Surface {
    key: String,
    last_mutation: u64,
    history: VecDeque<[u8; 32]>,
    observations_since_notice: usize,
}

impl VisualProgress {
    pub(crate) fn for_assignment(building: bool, inspection_only: bool) -> Self {
        Self {
            // Activate from execution intent, then deliver feedback only with
            // actual images or saved-image receipts. No application keyword
            // is needed in the user's request.
            quality_reviews: building && !inspection_only,
            inspection_only,
            ..Self::default()
        }
    }

    pub(crate) fn record_file_review(&mut self) {
        self.unreviewed_renders = 0;
        self.record_quality_review();
    }

    fn record_quality_review(&mut self) {
        self.last_quality_review = self.mutation;
        self.quality_reviewed = true;
    }

    /// A scripted build can save new images repeatedly without any
    /// desktop mutation. Remind the actor to inspect fresh output; neither
    /// shell success nor this reminder is a visual-quality verdict.
    /// Call only for successful tool results. This adds no provider call.
    pub(crate) fn scripted_review_checkpoint(&mut self, tool: &str, _input: &Value, output: &str) -> Option<&'static str> {
        if !self.quality_reviews || tool != "bash"
            || output.contains("Traceback (most recent call last)")
        {
            return None;
        }
        let saved_render = output.lines().any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("saved:")
                && [".png", ".jpg", ".jpeg", ".webp"].iter().any(|ext| line.contains(ext))
        });
        if !saved_render { return None; }
        self.unreviewed_renders = self.unreviewed_renders.saturating_add(1);
        if self.unreviewed_renders < 2 { return None; }
        self.unreviewed_renders = 0;
        Some("\nVISUAL REVIEW DUE: two output commands have reported saved images since your last file-image review or reminder. Inspect the current whole result with image_analyze, then a useful detail or alternative view. Confirm it is the updated file. Preserve the original assignment: for a review or report, describe the findings without changing the artifact; only an authorized creation or repair task should use them to choose the next change. Do not keep adjusting a script from an older image. This reminder does not expand authority, stop work or certify completion.")
    }

    /// Delivered alongside successfully attached artifact pixels. Scripted
    /// creation needs the same criticism as native desktop editing, including
    /// when a self-authored inspection question asks to confirm a "final" view.
    pub(crate) fn file_review_guidance(&self) -> Option<&'static str> {
        self.quality_reviews.then_some("\nVISUAL QUALITY CHECK: compare these actual pixels with the complete original brief and any real reference. Preserve the assigned scope: writing notes or a report does not authorize changing the pictured artifact. For review/report work, describe the observed findings without performing repairs. The following repair guidance applies only to authorized creation or revision. When a reference file is available, include it with image_analyze reference_paths so both images arrive together; an earlier image may now be only a text memory. A filename or inspection question calling this final is not evidence of completion. Identify the strongest visible mismatch, check it in a whole view and a useful detail, then use that observation to choose the next repair. Compare compatible views; framing, scale, lighting, or perspective may hide or exaggerate a defect. A local improvement can make the whole worse. If cosmetic changes repeatedly leave the same defect, change the underlying construction. Inspect the changed result before judging it. Finish when evidence meets the original criteria; do not invent defects, lower the brief, or substitute a list of implemented features for how the result actually looks. This is an instruction to examine the attached image, not an automated quality verdict.")
    }

    /// Call only after successful execution. Pure observation, pointer moves,
    /// and waits must not turn a stationary screen into a recovery warning.
    pub(crate) fn record_action(&mut self, tool: &str, input: &Value) {
        // A plain creation request may not need a durable workflow. Successful
        // authoring still needs its existing quality guidance when pixels are
        // later delivered. Downloads, navigation and inspection alone do not
        // activate this path, and the caller excludes failed mutations.
        if !self.inspection_only && matches!(tool, "write" | "str_replace" | "image_gen") {
            self.quality_reviews = true;
        }
        if matches!(
            tool,
            "computer_click"
                | "computer_drag"
                | "computer_scroll"
                | "computer_type"
                | "computer_key"
        ) {
            self.mutation = self.mutation.saturating_add(1);
            return;
        }
        if !matches!(tool, "computer_act" | "computer_window_act") {
            return;
        }
        let changes = input
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    matches!(
                        action.get("type").and_then(Value::as_str),
                        Some("click" | "double_click" | "drag" | "key" | "type" | "scroll")
                    )
                })
            });
        if changes {
            self.mutation = self.mutation.saturating_add(1);
        }
    }

    /// Uses the exact bounded native image payload already prepared for the
    /// model: no second decode, model call, file read, or retained screen data.
    /// Separate surfaces cannot be mistaken for one another. Only observations
    /// following fresh input enter the history, including input in a dialog.
    pub(crate) fn observe(&mut self, tool: &str, input: &Value, image: &str) -> Option<String> {
        let key = match tool {
            "computer_window_act" | "computer_capture_window" => {
                format!("window {}", input.get("id")?.as_i64()?)
            }
            "computer_screenshot" | "computer_act" | "computer_locate" | "computer_read_text" => {
                "desktop".into()
            }
            _ => return None,
        };
        let digest: [u8; 32] = Sha256::digest(image.as_bytes()).into();
        let position = self.surfaces.iter().position(|surface| surface.key == key);
        let mut surface = if let Some(position) = position {
            self.surfaces.remove(position)?
        } else {
            Surface {
                key: key.clone(),
                last_mutation: self.mutation,
                history: VecDeque::from([digest]),
                observations_since_notice: REPEATS,
            }
        };
        let mut notice = None;
        if surface.last_mutation != self.mutation {
            surface.last_mutation = self.mutation;
            surface.history.push_back(digest);
            if surface.history.len() > HISTORY {
                surface.history.pop_front();
            }
            surface.observations_since_notice += 1;
            let repeats = surface
                .history
                .iter()
                .filter(|previous| **previous == digest)
                .count();
            if repeats >= REPEATS && surface.observations_since_notice >= REPEATS {
                surface.observations_since_notice = 0;
                notice = Some(format!(
                    "\nVISUAL RECOVERY CHECK: {key} has returned to exactly the same image {repeats} times in its last {} observations following input. Input delivery alone does not prove progress. Inspect the attached image and identify the visible state and the change you expected. If this is an intentional unchanged view (for example, saving or working in another window), verify that outcome. Otherwise change the recovery approach: inspect the actual dialog or target, check the last useful checkpoint, and verify one changed step before repeating a batch. Preserve the original goal and quality requirements; this notice does not stop the task or establish completion.",
                    surface.history.len()
                ));
            }
        }
        // Native pixels alone did not prompt timely artifact criticism in a
        // real artifact run. Ask early, then at spaced editing milestones. This
        // is deliberation on the image already being delivered, not another
        // model call, a completion verdict, or a fixed time/tool limit.
        let review_interval = if self.quality_reviewed { 12 } else { 4 };
        if notice.is_none()
            && self.quality_reviews
            && self.mutation.saturating_sub(self.last_quality_review) >= review_interval
        {
            self.record_quality_review();
            notice = Some("\nVISUAL QUALITY CHECKPOINT: inspect the attached pixels against the original request. For authorized creation or revision, identify the visible result's strongest mismatch, repair it, and check the changed result. For review/report work, describe findings without changing the artifact; notes do not expand the assignment. If this is only a terminal, documentation, or an intermediate dialog, use it to verify the current step and obtain a useful view of the deliverable when appropriate; do not invent visual-design requirements for a nonvisual task. Compare whole and detail views where they help evaluate the result. If there is no material mismatch at this stage, continue the remaining work. Successful inputs or a saved file do not establish quality. This checkpoint does not stop the task or request a user-facing audit report.".into());
        }
        self.surfaces.push_back(surface);
        if self.surfaces.len() > SURFACES {
            self.surfaces.pop_front();
        }
        notice
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn observed_authoring_activates_review_without_workflow_or_application_keywords() {
        for tool in ["write", "str_replace", "image_gen"] {
            let mut guard = VisualProgress::for_assignment(false, false);
            assert!(guard.file_review_guidance().is_none());
            guard.record_action(tool, &json!({"path":"artifact.txt"}));
            assert!(guard.file_review_guidance().is_some());
            assert_eq!(guard.mutation, 0, "file authoring must not counterfeit desktop input");
        }
        let mut inspection = VisualProgress::for_assignment(false, false);
        for tool in ["read", "image_analyze", "web_fetch", "browser_navigate", "browser_screenshot", "computer_capture_window"] {
            inspection.record_action(tool, &json!({}));
            assert!(inspection.file_review_guidance().is_none(), "observation stays observation: {tool}");
        }
        let mut scoped = VisualProgress::for_assignment(true, true);
        for tool in ["write", "str_replace", "image_gen"] {
            scoped.record_action(tool, &json!({"path":"notes.md"}));
            assert!(scoped.file_review_guidance().is_none(), "typed inspection remains excluded after note writes");
        }
    }

    #[test]
    fn scripted_render_reviews_are_spaced_and_reset_by_actual_image_delivery() {
        let mut guard = VisualProgress::for_assignment(true, false);
        let input = json!({"command":"/snap/bin/blender --background --python-exit-code 1 --python scene.py"});
        // Several cameras saved by one command count as one render batch.
        let output = "render | Saved: '/work/whole.png'\nrender | Saved: '/work/detail.png'";
        assert!(guard.scripted_review_checkpoint("bash", &input, output).is_none());
        guard.record_file_review();
        assert!(guard.scripted_review_checkpoint("bash", &input, output).is_none());
        assert!(guard.scripted_review_checkpoint("bash", &input, output).unwrap().contains("current whole result"));
        assert!(guard.scripted_review_checkpoint("bash", &input, output).is_none());
        // A desktop checkpoint may inspect an older open scene; it cannot
        // substitute for the new scripted image on disk.
        guard.record_quality_review();
        assert!(guard.scripted_review_checkpoint("bash", &input, output).is_some());
    }

    #[test]
    fn scripted_review_ignores_inspection_failed_scripts_and_nonrenders() {
        let input = json!({"command":"blender --background --python inspect.py"});
        let render = "render | Saved: '/work/whole.png'";
        for inspection_only in [false, true] {
            let mut guard = VisualProgress::for_assignment(false, inspection_only);
            for _ in 0..4 { assert!(guard.scripted_review_checkpoint("bash", &input, render).is_none()); }
        }
        let mut guard = VisualProgress::for_assignment(true, false);
        for _ in 0..4 {
            assert!(guard.scripted_review_checkpoint("bash", &input, "Info: Saved as scene.blend").is_none());
            assert!(guard.scripted_review_checkpoint("bash", &input, &format!("{render}\nTraceback (most recent call last):")).is_none());
            assert!(guard.scripted_review_checkpoint("read", &input, render).is_none());
            assert!(guard.scripted_review_checkpoint("bash", &json!({"command":"cargo test"}), "test result: ok").is_none());
        }
        assert_eq!(guard.unreviewed_renders, 0);
    }

    #[test]
    fn visual_reviews_follow_images_without_application_specific_requests() {
        for initially_building in [false, true] {
            let mut guard = VisualProgress::for_assignment(initially_building, false);
            if !initially_building { guard.record_action("write", &json!({"path":"artifact.txt"})); }
            let input = json!({"command":"python build.py"});
            let receipt = "Saved: '/work/preview.webp'";
            assert!(guard.scripted_review_checkpoint("bash", &input, receipt).is_none());
            assert!(guard.scripted_review_checkpoint("bash", &input, receipt).is_some());
            let guidance = guard.file_review_guidance().unwrap();
            assert!(guidance.contains("reference_paths"));
            assert!(!guidance.contains("Blender") && !guidance.contains("stem"));
        }
    }

    fn act(guard: &mut VisualProgress) {
        guard.record_action(
            "computer_window_act",
            &json!({"id":1,"actions":[{"type":"key","combo":"ctrl+o"}]}),
        );
    }
    fn observe(guard: &mut VisualProgress, image: &str) -> Option<String> {
        guard.observe("computer_capture_window", &json!({"id":1}), image)
    }

    /// Explicit diagnostic replay; no desktop actions or provider calls run.
    #[tokio::test]
    #[ignore = "requires PHOENIX_VISUAL_REPLAY pointing at retained tool receipts"]
    async fn replay_recorded_desktop_recovery() {
        let path = std::env::var("PHOENIX_VISUAL_REPLAY").expect("set recorded receipt path");
        let receipts: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut guard = VisualProgress::default();
        let mut notices = vec![];
        for (index, receipt) in receipts.iter().enumerate() {
            if receipt["success"] != true {
                continue;
            }
            let tool = receipt["tool_name"].as_str().unwrap();
            let input: Value = serde_json::from_str(receipt["input"].as_str().unwrap()).unwrap();
            guard.record_action(tool, &input);
            let path = receipt["output"]
                .as_str()
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("Screenshot saved: "));
            if let Some(path) = path {
                let image =
                    crate::runtime::vision::native_screen_data_uri(std::path::Path::new(path))
                        .await
                        .unwrap();
                if let Some(notice) = guard.observe(tool, &input, &image) {
                    println!("receipt {index}: {notice}");
                    notices.push(index);
                }
            }
        }
        assert!(
            !notices.is_empty(),
            "recorded recovery must produce actionable feedback"
        );
        println!("Visual recovery notices at receipt indices: {notices:?}");
    }

    #[test]
    fn unchanged_recovery_warns_after_four_observed_returns_and_keeps_running() {
        let mut guard = VisualProgress::default();
        assert!(observe(&mut guard, "empty scene").is_none());
        for _ in 0..2 {
            act(&mut guard);
            assert!(observe(&mut guard, "empty scene").is_none());
        }
        act(&mut guard);
        let notice = observe(&mut guard, "empty scene").unwrap();
        assert!(notice.contains("4 times"));
        assert!(notice.contains("does not stop the task"));
        for _ in 0..3 {
            act(&mut guard);
            assert!(observe(&mut guard, "empty scene").is_none());
        }
        act(&mut guard);
        assert!(observe(&mut guard, "empty scene").is_some());
    }

    #[test]
    fn detects_short_cycles_even_when_action_and_dialog_ids_change() {
        let mut guard = VisualProgress::default();
        for index in 0..7 {
            guard.record_action(
                "computer_window_act",
                &json!({"id":10+index,"actions":[{"type":"click","x":index,"y":20}]}),
            );
            let notice = observe(&mut guard, if index % 2 == 0 { "scene" } else { "dialog" });
            assert_eq!(notice.is_some(), index == 6);
        }
    }

    #[test]
    fn creative_build_review_is_early_spaced_and_reset_by_file_inspection() {
        let mut guard = VisualProgress::for_assignment(true, false);
        assert!(guard.file_review_guidance().is_some(), "scripted builds receive criticism even before native input");
        for index in 1..=16 {
            act(&mut guard);
            let notice = observe(&mut guard, &format!("edited view {index}"));
            assert_eq!(
                notice
                    .as_deref()
                    .is_some_and(|text| text.contains("VISUAL QUALITY CHECKPOINT")),
                index == 4 || index == 16
            );
        }
        act(&mut guard);
        guard.record_file_review();
        for index in 1..12 {
            act(&mut guard);
            assert!(observe(&mut guard, &format!("new view {index}")).is_none());
        }
        act(&mut guard);
        assert!(observe(&mut guard, "later edited artifact").is_some());
    }

    #[test]
    fn direct_make_assignment_enables_pure_native_quality_checkpoints() {
        for request in [
            "Make me a realistic banana in Blender using banana-reference.jpg. Work through the application interface, not scripts, code or modeling APIs. Save banana.blend and a polished banana.png in this workspace.",
            "Please make a detailed illustration from the supplied reference.",
            "Could you please make a realistic sculpture from the supplied reference?",
        ] {
            for (action, capture) in [
                ("computer_window_act", "computer_capture_window"),
                ("computer_act", "computer_screenshot"),
            ] {
                // Match the mesh caller: do not substitute a hardcoded
                // building=true or a file-authoring tool for the real intent.
                let build = crate::runtime::build_contract::BuildContractGuard::for_assignment(request, false);
                let mut guard = VisualProgress::for_assignment(build.requires_durable_goal(), false);
                let input = if action == "computer_window_act" {
                    json!({"id":1,"actions":[{"type":"key","combo":"ctrl+s"}]})
                } else {
                    json!({"actions":[{"type":"key","combo":"ctrl+s"}]})
                };
                for index in 1..=16 {
                    guard.record_action(action, &input);
                    let notice = guard.observe(capture, &input, &format!("observed candidate {index}"));
                    assert_eq!(
                        notice.as_deref().is_some_and(|text| text.contains("VISUAL QUALITY CHECKPOINT")),
                        index == 4 || index == 16,
                        "{request}: {action} at {index}",
                    );
                }
                assert!(guard.file_review_guidance().is_some(), "{request}");
                for _ in 0..8 {
                    assert!(guard.observe(capture, &input, "observed candidate 16").is_none(),
                        "unchanged observations must not create another quality loop");
                }
            }
        }
    }

    #[test]
    fn composed_native_inspection_and_advisory_work_do_not_activate_repairs() {
        for (request, inspection) in [
            ("Make a detailed illustration from the supplied reference.", true),
            ("Explain how to make a realistic illustration.", false),
            ("Could you explain how to make a realistic illustration?", false),
            ("Review a plan to make a detailed illustration.", false),
            ("The future request is 'Make a realistic illustration.' Explain its meaning.", false),
            ("Open the document and read the current title.", false),
            ("Make the UI label shorter.", false),
        ] {
            let build = crate::runtime::build_contract::BuildContractGuard::for_assignment(request, inspection);
            let mut guard = VisualProgress::for_assignment(build.requires_durable_goal(), inspection);
            for index in 0..24 {
                act(&mut guard);
                assert!(observe(&mut guard, &format!("navigation state {index}")).is_none(), "{request}");
            }
            guard.record_file_review();
            assert!(guard.file_review_guidance().is_none(), "{request}");
        }
    }

    #[test]
    fn quality_checkpoints_preserve_inspection_scope_and_need_observed_images() {
        for inspection_only in [false, true] {
            let mut guard = VisualProgress::for_assignment(false, inspection_only);
            assert!(guard.file_review_guidance().is_none(), "inspection and nonvisual work keep their original scope");
            for index in 0..24 {
                act(&mut guard);
                assert!(observe(&mut guard, &format!("view {index}")).is_none());
            }
        }
        let mut guard = VisualProgress::for_assignment(true, false);
        for _ in 0..24 { guard.record_action("bash", &json!({"command":"cargo test"})); }
        assert_eq!(guard.mutation, 0, "shell checks without images cannot trigger desktop visual review");
        assert_eq!(guard.last_quality_review, 0);
    }

    #[test]
    fn individual_desktop_actions_are_also_observed() {
        let mut guard = VisualProgress::default();
        assert!(observe(&mut guard, "unchanged").is_none());
        for tool in ["computer_click", "computer_type", "computer_key"] {
            guard.record_action(tool, &json!({}));
            let notice = observe(&mut guard, "unchanged");
            assert_eq!(notice.is_some(), tool == "computer_key");
        }
    }

    #[test]
    fn polling_waiting_pointer_moves_and_file_review_do_not_warn() {
        let mut guard = VisualProgress::default();
        for _ in 0..30 {
            guard.record_action(
                "computer_window_act",
                &json!({"id":1,"actions":[{"type":"wait","ms":1000},{"type":"move","x":4,"y":5}]}),
            );
            assert!(observe(&mut guard, "rendering").is_none());
            assert!(guard
                .observe("image_analyze", &json!({"path":"render.png"}), "rendering")
                .is_none());
        }
    }

    #[test]
    fn changing_images_age_out_old_repeats_and_surfaces_are_isolated_and_bounded() {
        let mut guard = VisualProgress::default();
        for index in 0..24 {
            act(&mut guard);
            assert!(observe(&mut guard, &format!("progress {index}")).is_none());
        }
        for id in 2..30 {
            act(&mut guard);
            assert!(guard
                .observe("computer_capture_window", &json!({"id":id}), "same pixels")
                .is_none());
        }
        assert_eq!(guard.surfaces.len(), SURFACES);
        assert!(guard
            .surfaces
            .iter()
            .all(|surface| surface.history.len() <= HISTORY));
    }
}
