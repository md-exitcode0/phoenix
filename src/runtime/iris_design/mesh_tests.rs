use super::*;
use crate::providers::contracts::NativeToolCall;
use crate::providers::TokenUsage;

fn response(content: &str, tool_calls: Vec<NativeToolCall>) -> CompletionResponse {
    CompletionResponse { content: content.into(), model: "fixture".into(), usage: TokenUsage::new(0, 0),
        reasoning: None, stop_reason: None, tool_calls,
        provider_replay: None, }
}

#[tokio::test]
async fn design_website_refuses_folders_that_are_not_a_site_of_its_own() {
    let launch = DesignLaunch { session_id: "agent-frontend".into(), turn_id: "turn-1".into(),
        browser_instance: "agent-frontend".into(), references: Vec::new() };
    let root = std::env::temp_dir();
    for folder in [
        "relative/site".to_string(),
        env!("CARGO_MANIFEST_DIR").to_string(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().display().to_string(),
    ] {
        let input = serde_json::json!({"brief": "Build a bakery site", "folder": folder});
        assert!(launch_from_tool(&root, &launch, &input).await.is_err(), "{folder}");
    }
    let missing = serde_json::json!({"folder": "/tmp/site"});
    assert!(launch_from_tool(&root, &launch, &missing).await.is_err());
}

#[test]
fn design_website_is_iris_tool_with_a_brief_and_folder() {
    let definition = crate::tools::tool_definitions_for_agent(&["design_website".to_string()]).into_iter().find(|tool| tool.name == "design_website").expect("registered");
    assert_eq!(definition.parameters["required"], serde_json::json!(["brief", "folder"]));
    let spec = crate::sub_agents::specialist_config(crate::session::SubAgentType::Frontend).spec;
    assert!(spec.tool_allowlist.iter().any(|tool| tool == "design_website"));
}

#[test]
fn ordinary_visual_checks_return_after_managed_qualification() {
    let mut managed = visual_progress(true, false, true);
    managed.record_action("write", &serde_json::json!({"path":"index.html"}));
    assert!(managed.file_review_guidance().is_none());
    let mut ordinary = visual_progress(false, false, false);
    ordinary.record_action("write", &serde_json::json!({"path":"scene.py"}));
    assert!(ordinary.file_review_guidance().is_some());
    let mut inspection = visual_progress(true, true, false);
    inspection.record_action("write", &serde_json::json!({"path":"report.md"}));
    assert!(inspection.file_review_guidance().is_none());
}

#[test]
fn only_briefing_uses_upstream_low_effort() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"brief", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    assert_eq!(phase_effort(Some(&context), Some("high".into())).as_deref(), Some("low"));
    context.phase = "brand".into();
    assert_eq!(phase_effort(Some(&context), Some("high".into())).as_deref(), Some("high"));
    assert!(phase_effort(None, None).is_none());
}

#[test]
fn managed_design_phases_prompt_for_bounded_theo_reference_help() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"brief", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    for phase in ["brief", "brand", "page", "assets", "build"] {
        context.phase = phase.into();
        let guidance = phase_collaboration_guidance(&context).expect("active design phase should carry collaboration guidance");
        assert!(guidance.contains("Theo"));
        assert!(guidance.contains("varied real examples"));
        assert!(guidance.contains("Iris remains accountable"));
    }
    context.phase = "review".into();
    assert!(phase_collaboration_guidance(&context).is_none());
}

#[test]
fn commissioned_sites_are_written_as_the_business_would_publish_them() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"brief", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    for phase in ["brief", "page", "build", "repair", "review"] {
        context.phase = phase.into();
        let guidance = phase_content_guidance(&context).expect("content guidance");
        assert!(guidance.contains("Never add copy that questions whether it is real"));
        assert!(guidance.contains("never invent testimonials"));
    }
    for phase in ["brand", "assets", "preview"] {
        context.phase = phase.into();
        assert!(phase_content_guidance(&context).is_none(), "{phase}");
    }
}

#[test]
fn imagery_comes_from_web_photos_not_generation_or_3d() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"assets", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    for phase in ["page", "assets", "build", "repair"] {
        context.phase = phase.into();
        let guidance = phase_imagery_guidance(&context).expect("imagery guidance");
        assert!(guidance.contains("real photographs"));
        assert!(guidance.contains("render 3D models"));
        assert!(!phase_tool_allowed(&context, "image_gen"), "{phase}");
    }
    context.phase = "brief".into();
    assert!(phase_imagery_guidance(&context).is_none());
}

#[test]
fn catalog_components_are_named_in_page_downloaded_in_assets_and_installed_in_build() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"page", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    assert!(phase_component_guidance(&context).unwrap().contains("componentNeeds ID"));
    context.phase = "assets".into();
    let assets = phase_component_guidance(&context).unwrap();
    assert!(assets.contains(".taste/components/<need-id>.json"));
    assert!(assets.contains("https://beui.dev/r/registry.json"));
    assert!(assets.contains("source.kind external"));
    context.phase = "build".into();
    let build = phase_component_guidance(&context).unwrap();
    assert!(build.contains("shadcn@latest add"));
    assert!(build.contains("never hand-drawn inline <svg>"));
    assert!(build.contains("create-next-app"));
    assert!(build.contains("public/*.svg"));
    for phase in ["brief", "brand", "preview", "review"] {
        context.phase = phase.into();
        assert!(phase_component_guidance(&context).is_none(), "{phase}");
    }
    context.phase = "build".into();
    context.kind = "terminal".into();
    assert!(phase_component_guidance(&context).is_none());
}

#[test]
fn phase_json_and_final_wrappers_are_consumed_before_normal_final_parsing() {
    let raw = r#"{"status":"complete","message":"Brief complete.","brief":{}}"#;
    assert_eq!(phase_output(&response(raw, vec![])).as_deref(), Some(raw));
    let wrapped = NativeToolCall { id: "phase-a".into(), tool_name: "final_answer".into(),
        arguments: serde_json::json!({"summary":"Phase output", "final_markdown":raw}) };
    let response = response("", vec![wrapped]);
    assert_eq!(phase_output(&response).as_deref(), Some(raw));
    let mut tail = Vec::new();
    retain_internal_reply(&response, &mut tail, "brand");
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[1].tool_call_id.as_deref(), Some("phase-a"));
}

#[test]
fn ordinary_tools_remain_tools_and_mixed_finish_never_executes() {
    let read = NativeToolCall { id: "read-a".into(), tool_name: "read".into(), arguments: serde_json::json!({"path":"index.html"}) };
    assert!(phase_output(&response("Inspecting", vec![read.clone()])).is_none());
    let finish = NativeToolCall { id: "finish-a".into(), tool_name: "final_answer".into(), arguments: serde_json::json!({"final_markdown":"{}"}) };
    assert!(phase_output(&response("", vec![read, finish])).unwrap().contains("not executed"));
}

#[test]
fn briefing_cannot_mutate_and_build_cannot_load_legacy_design() {
    let mut context: DesignContext = serde_json::from_value(serde_json::json!({
        "phase":"brief", "status":"running", "kind":"model", "prompt":"upstream", "attachments":[]
    })).unwrap();
    assert!(phase_tool_allowed(&context, "read"));
    assert!(phase_tool_allowed(&context, "talk"));
    assert!(!phase_tool_allowed(&context, "write"));
    assert!(!phase_tool_allowed(&context, "bash"));
    context.phase = "brand".into();
    assert!(phase_tool_allowed(&context, "talk"));
    context.phase = "page".into();
    assert!(phase_tool_allowed(&context, "talk"));
    context.phase = "preview".into();
    assert!(!phase_tool_allowed(&context, "talk"));
    context.phase = "review".into();
    assert!(!phase_tool_allowed(&context, "talk"));
    context.phase = "build".into();
    assert!(phase_tool_allowed(&context, "write"));
    assert!(phase_tool_allowed(&context, "talk"));
    assert!(!phase_tool_allowed(&context, "design_reference"));
    assert!(!phase_tool_allowed(&context, "design_studio"));
}
