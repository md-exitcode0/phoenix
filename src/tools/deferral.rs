//! Progressive tool disclosure.
//!
//! Every coworker keeps the complete tool catalog, but whole families that a
//! turn rarely touches (browser, desktop control, connected apps, vault, MCP)
//! ship as one line each until the agent asks for them. Their schemas were
//! ~45% of every request (~50 tools, ~13k tokens on each round) even for a
//! coder fixing a CLI. Loading is one `tools_load` call — or simply calling a
//! member tool by name, which runs normally and loads its family for the
//! rest of the turn. Nothing is gated: execution still checks the agent's
//! real allowlist and every permission rule.

use std::collections::HashSet;

use crate::providers::contracts::ToolDefinition;

pub const LOAD_TOOL: &str = "tools_load";

pub struct Family {
    pub name: &'static str,
    pub summary: &'static str,
    members: fn(&str) -> bool,
}

pub const FAMILIES: &[Family] = &[
    Family {
        name: "browser",
        summary: "your private managed browser: open pages, click, type, read, screenshots, downloads, cookies, site logins (ask_for_login)",
        members: |name| name.starts_with("browser_") || name == "ask_for_login",
    },
    Family {
        name: "desktop",
        summary: "native desktop apps and OS dialogs: screenshots, clicks, typing, windows (computer_*)",
        members: |name| name.starts_with("computer_"),
    },
    Family {
        name: "connected_apps",
        summary: "the user's connected services through Composio: email, calendar, docs, CRM and more",
        members: |name| name.starts_with("composio_"),
    },
    Family {
        name: "vault",
        summary: "Passes — saved logins, cards, API keys, tokens: list, ask the user for one (ask_for_pass), use one without seeing it (pass_use), generate passwords, track accounts",
        members: |name| matches!(name, "credential_list" | "credential_generate" | "account_manage" | "ask_for_pass" | "pass_use"),
    },
    Family {
        name: "mcp",
        summary: "tools on configured MCP servers",
        members: |name| name.starts_with("mcp_"),
    },
];

pub fn family_of(tool: &str) -> Option<&'static str> {
    FAMILIES.iter().find(|family| (family.members)(tool)).map(|family| family.name)
}

/// Families whose tools ARE the agent's job are loaded from the start.
pub fn preloaded_for(agent: &str) -> HashSet<String> {
    let agent = agent.to_ascii_lowercase();
    let mut loaded = HashSet::new();
    if agent.contains("browser") || agent.contains("surf") {
        loaded.insert("browser".to_string());
    }
    if agent.contains("computer") {
        loaded.insert("desktop".to_string());
    }
    loaded
}

/// Drop the schemas of families not yet loaded and, if anything was held
/// back, add `tools_load` describing exactly what is available.
pub fn apply(tools: &mut Vec<ToolDefinition>, loaded: &HashSet<String>) {
    let mut deferred: Vec<(&'static Family, usize)> = Vec::new();
    tools.retain(|tool| {
        let Some(family) = FAMILIES.iter().find(|family| (family.members)(&tool.name)) else {
            return true;
        };
        if loaded.contains(family.name) {
            return true;
        }
        match deferred.iter_mut().find(|(seen, _)| seen.name == family.name) {
            Some((_, count)) => *count += 1,
            None => deferred.push((family, 1)),
        }
        false
    });
    if deferred.is_empty() || tools.iter().any(|tool| tool.name == LOAD_TOOL) {
        return;
    }
    let index = deferred
        .iter()
        .map(|(family, count)| format!("- {} ({count} tools): {}", family.name, family.summary))
        .collect::<Vec<_>>()
        .join("\n");
    tools.push(ToolDefinition {
        name: LOAD_TOOL.to_string(),
        description: format!(
            "Load the full schemas of a tool family so you can call its tools from your next step. These families are available to you now but not listed yet:\n{index}\nLoad a family as soon as the task needs it; do not work around it."
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "families": {
                    "type": "array",
                    "items": {"type": "string", "enum": deferred.iter().map(|(family, _)| family.name).collect::<Vec<_>>()},
                    "description": "Families to load."
                }
            },
            "required": ["families"]
        }),
    });
}

/// Record a `tools_load` request; returns the user-facing tool result.
pub fn load(input: &serde_json::Value, loaded: &mut HashSet<String>) -> Result<String, String> {
    let requested: Vec<String> = input
        .get("families")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect())
        .or_else(|| input.get("family").and_then(|value| value.as_str()).map(|family| vec![family.to_string()]))
        .unwrap_or_default();
    if requested.is_empty() {
        return Err("tools_load needs `families`, e.g. {\"families\":[\"browser\"]}.".into());
    }
    let unknown: Vec<&String> = requested.iter().filter(|name| !FAMILIES.iter().any(|family| family.name == name.as_str())).collect();
    if !unknown.is_empty() {
        let known = FAMILIES.iter().map(|family| family.name).collect::<Vec<_>>().join(", ");
        return Err(format!("Unknown tool family {unknown:?}. Available: {known}."));
    }
    for name in &requested {
        loaded.insert(name.clone());
    }
    Ok(format!("Loaded {}. Their full schemas are in your tool list from your next step.", requested.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition { name: name.into(), description: String::new(), parameters: serde_json::json!({}) }
    }

    #[test]
    fn unloaded_families_collapse_into_one_loader() {
        let mut tools = vec![tool("read"), tool("browser_navigate"), tool("browser_click"), tool("computer_act"), tool("ask_for_login")];
        apply(&mut tools, &HashSet::new());
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read", LOAD_TOOL]);
        let loader = tools.last().unwrap();
        assert!(loader.description.contains("browser (3 tools)"));
        assert!(loader.description.contains("desktop (1 tools)"));
    }

    #[test]
    fn loading_restores_exactly_that_family() {
        let mut loaded = HashSet::new();
        assert!(load(&serde_json::json!({"families":["browser"]}), &mut loaded).is_ok());
        assert!(load(&serde_json::json!({"families":["nope"]}), &mut loaded).is_err());
        let mut tools = vec![tool("read"), tool("browser_click"), tool("computer_act")];
        apply(&mut tools, &loaded);
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read", "browser_click", LOAD_TOOL]);
        assert_eq!(family_of("computer_act"), Some("desktop"));
        assert!(preloaded_for("Browser").contains("browser"));
    }
}
