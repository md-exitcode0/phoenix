//! Inspect the resolved Iris configuration without submitting a provider request.
//! Includes the selected Phoenix home's overlay and built-in agent overrides.
use sha2::{Digest, Sha256};

fn main() -> anyhow::Result<()> {
    let iris = phoenix_agent::sub_agents::specialist_config(
        phoenix_agent::session::SubAgentType::Frontend,
    ).spec;
    let expected = include_str!("../prompts/frontend_system.md");
    let checks = serde_json::json!({
        "installed_role_matches_build": iris.system_prompt == expected,
        "managed_phase_contract": iris.system_prompt.contains("current phase prompt and selected reference images"),
        "no_design_toggle": iris.system_prompt.contains("no user-facing design-mode toggle"),
        "working_browser_capability": iris.tool_allowlist.iter().any(|tool| tool == "browser_screenshot"),
        "bounded_host_role": iris.system_prompt.len() < 4_000,
        "withdrawn_clarity_doctrine_absent": !iris.system_prompt.contains("Plain meaning and usefulness"),
    });
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "role": "Iris", "checks": checks,
        "resolved_prompt_sha256": format!("{:x}", Sha256::digest(iris.system_prompt.as_bytes())),
        "expected_prompt_sha256": format!("{:x}", Sha256::digest(expected.as_bytes())),
        "scope": "Resolved installed role and working tools. This does not execute or certify the staged design workflow.",
        "model_requests": 0,
    }))?);
    anyhow::ensure!(checks.as_object().unwrap().values().all(|value| value == true),
        "The effective Iris configuration does not match the new staged-design host role");
    Ok(())
}
