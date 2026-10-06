//! `phoenix configure` → Agents: the roster as data (plan 019).
//!
//! Every coworker lives in `~/.phoenix/agents/<role>/`. Creation, archival,
//! trash restoration, and 30-day deletion are company-directory operations in
//! the desktop app; this maintenance menu never bypasses those safeguards.

use super::*;
use crate::sub_agents::registry;

pub(super) fn run_agents_menu(theme: &ColorfulTheme, _config_path: &std::path::Path) -> Result<()> {
    loop {
        registry::refresh();
        println!();
        print_roster();
        let actions = [
            "Park / unpark an agent    disable a custom agent without deleting it",
            "Export built-in manifests materialize editable agent.toml for every built-in",
            "Open the agents dir       print the path everything lives under",
            "← Back",
        ];
        let pick = match Select::with_theme(theme)
            .with_prompt("Agents")
            .items(&actions)
            .default(0)
            .interact()
        {
            Ok(pick) => pick,
            Err(_) => return Ok(()),
        };
        let outcome = match pick {
            0 => toggle_agent(theme),
            1 => {
                let written = registry::export_builtin_manifests()?;
                println!(
                    "  {} {} manifest(s) exported to {} (already-present files untouched).",
                    style("✔").green(),
                    written,
                    style(registry::agents_dir().display()).dim()
                );
                Ok(())
            }
            2 => {
                println!(
                    "  {} agents live at {}",
                    style("•").dim(),
                    style(registry::agents_dir().display()).bold()
                );
                println!(
                    "  {}",
                    style("  <role>/agent.toml = persona/model/tools · system.md = prompt (custom) · knowledge/ = research docs").dim()
                );
                Ok(())
            }
            _ => return Ok(()),
        };
        if let Err(error) = outcome {
            if is_cancel(&error) {
                println!("  {}", style("cancelled — back to Agents").dim());
            } else {
                return Err(error);
            }
        }
    }
}

fn print_roster() {
    println!(
        "  {}",
        style(format!(
            "built-ins ({} on the roster)",
            registry::BUILTIN_ROLES.len()
        ))
        .dim()
    );
    for role in registry::BUILTIN_ROLES {
        let persona = crate::runtime::delegation::agent_persona(role).unwrap_or("-");
        let overridden = registry::builtin_override(role).is_some();
        println!(
            "  {} {}  {}",
            style(format!("{persona:<10}")).bold(),
            style(format!("{role:<14}")).dim(),
            if overridden {
                style("manifest override active").yellow().to_string()
            } else {
                style("compiled defaults").dim().to_string()
            }
        );
    }
    let custom = registry::custom_roster();
    if custom.is_empty() {
        println!(
            "  {}",
            style("no custom agents yet — Phoenix creates them via create_agent when a new domain needs an owner").dim()
        );
        return;
    }
    println!("  {}", style("custom").dim());
    for (role, description) in custom {
        let persona = crate::runtime::delegation::agent_persona(&role).unwrap_or("-");
        println!(
            "  {} {}  {}",
            style(format!("{persona:<10}")).bold(),
            style(format!("{role:<14}")).dim(),
            description
        );
    }
}

fn pick_custom_agent(theme: &ColorfulTheme, prompt: &str) -> Result<Option<String>> {
    // Parked agents are listed too (that is the point of unparking) — read
    // dirs rather than the enabled-only roster. Registry discovery enforces
    // direct-child role names and rejects symlinked/unsafe state paths.
    let mut roles = registry::custom_roles_on_disk()?;
    if roles.is_empty() {
        println!("  {}", style("no custom agents on disk").dim());
        return Ok(None);
    }
    roles.sort();
    let mut labels = roles.clone();
    labels.push("← Cancel".to_string());
    let pick = Select::with_theme(theme)
        .with_prompt(format!("  {prompt}"))
        .items(&labels)
        .default(0)
        .interact()
        .context("agent selection cancelled")?;
    if pick >= roles.len() {
        return Ok(None);
    }
    Ok(Some(roles[pick].clone()))
}

fn toggle_agent(theme: &ColorfulTheme) -> Result<()> {
    let Some(role) = pick_custom_agent(theme, "Park/unpark which agent?")? else {
        return Ok(());
    };
    let manifest_path = registry::agent_dir_for_role(&role)?.join("agent.toml");
    let enabled = toggle_agent_manifest(&manifest_path)?;
    registry::refresh();
    println!(
        "  {} `{role}` is now {}.",
        style("✔").green(),
        if enabled {
            "PARKED (won't resolve as a talk target)"
        } else {
            "active"
        }
    );
    Ok(())
}

fn toggle_agent_manifest(manifest_path: &std::path::Path) -> Result<bool> {
    const MAX_AGENT_MANIFEST_BYTES: usize = 1024 * 1024;
    crate::config::private_io::read_modify_write_private(manifest_path, |current| {
        let raw = current
            .with_context(|| format!("agent manifest is missing: {}", manifest_path.display()))?;
        if raw.len() > MAX_AGENT_MANIFEST_BYTES {
            anyhow::bail!("agent manifest {} is too large", manifest_path.display());
        }
        let raw = std::str::from_utf8(raw)
            .with_context(|| format!("{} is not valid UTF-8", manifest_path.display()))?;
        let mut value: toml::Value = raw.parse().context("invalid agent.toml")?;
        let table = value.as_table_mut().context("agent.toml is not a table")?;
        let enabled = match table.get("enabled") {
            None => true,
            Some(value) => value
                .as_bool()
                .context("agent.toml field `enabled` must be a boolean")?,
        };
        table.insert("enabled".to_string(), toml::Value::Boolean(!enabled));
        let rendered = toml::to_string_pretty(&value)?.into_bytes();
        Ok((enabled, rendered))
    })
    .with_context(|| format!("failed to update {}", manifest_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_is_lock_scoped_and_preserves_other_manifest_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(
            &path,
            "persona = \"Ledger\"\ndescription = \"markets\"\nenabled = true\n",
        )
        .unwrap();

        assert!(toggle_agent_manifest(&path).unwrap());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("persona = \"Ledger\""));
        assert!(raw.contains("description = \"markets\""));
        assert!(raw.contains("enabled = false"));
    }

    #[test]
    fn corrupt_toggle_input_is_not_replaced_with_a_default_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        let corrupt = b"enabled = [not-a-boolean]";
        std::fs::write(&path, corrupt).unwrap();

        assert!(toggle_agent_manifest(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }
}
