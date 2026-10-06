//! Single source of truth for slash commands.
//!
//! Both the help screen and the interactive completer/hinter read this table,
//! so there is exactly one place to add or edit a command.

pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub desc: &'static str,
    pub section: &'static str,
}

pub const SECTIONS: &[&str] = &["General", "Sessions", "Runtime", "Inspect"];

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        aliases: &["?"],
        args: "",
        desc: "Show this help",
        section: "General",
    },
    SlashCommand {
        name: "clear",
        aliases: &["cls"],
        args: "",
        desc: "Clear the screen",
        section: "General",
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit", "q"],
        args: "",
        desc: "Exit",
        section: "General",
    },
    SlashCommand {
        name: "session",
        aliases: &[],
        args: "",
        desc: "Show the active session",
        section: "Sessions",
    },
    SlashCommand {
        name: "sessions",
        aliases: &[],
        args: "",
        desc: "List resumable sessions",
        section: "Sessions",
    },
    SlashCommand {
        name: "new",
        aliases: &[],
        args: "[id]",
        desc: "Start a new session",
        section: "Sessions",
    },
    SlashCommand {
        name: "use",
        aliases: &["resume"],
        args: "[id]",
        desc: "Resume a session and load its context",
        section: "Sessions",
    },
    SlashCommand {
        name: "status",
        aliases: &[],
        args: "",
        desc: "Runtime status",
        section: "Runtime",
    },
    SlashCommand {
        name: "model",
        aliases: &[],
        args: "",
        desc: "Provider / model",
        section: "Runtime",
    },
    SlashCommand {
        name: "actions",
        aliases: &[],
        args: "",
        desc: "Toggle tool visibility",
        section: "Runtime",
    },
    SlashCommand {
        name: "thinking",
        aliases: &[],
        args: "",
        desc: "Toggle model thinking text",
        section: "Runtime",
    },
    SlashCommand {
        name: "reasoning",
        aliases: &[],
        args: "[low|medium|high|xhigh]",
        desc: "Set model reasoning effort",
        section: "Runtime",
    },
    SlashCommand {
        name: "debug",
        aliases: &[],
        args: "",
        desc: "Toggle librarian debug",
        section: "Runtime",
    },
    SlashCommand {
        name: "yolo",
        aliases: &[],
        args: "",
        desc: "Unconfine tools (act outside cwd)",
        section: "Runtime",
    },
    SlashCommand {
        name: "safe",
        aliases: &["workspace"],
        args: "",
        desc: "Re-confine tools to cwd",
        section: "Runtime",
    },
    SlashCommand {
        name: "defaults",
        aliases: &[],
        args: "save",
        desc: "Save current toggles to config as defaults",
        section: "Runtime",
    },
    SlashCommand {
        name: "memory",
        aliases: &[],
        args: "",
        desc: "Memory paths from last run",
        section: "Inspect",
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        args: "",
        desc: "Session compaction/archive state",
        section: "Inspect",
    },
    SlashCommand {
        name: "prompt",
        aliases: &[],
        args: "",
        desc: "Last assembled prompt",
        section: "Inspect",
    },
    SlashCommand {
        name: "tools",
        aliases: &[],
        args: "",
        desc: "Agent tool rosters",
        section: "Inspect",
    },
    SlashCommand {
        name: "doctor",
        aliases: &[],
        args: "",
        desc: "Local checks",
        section: "Inspect",
    },
];

/// True when the current configured provider exposes a selectable reasoning
/// effort, so `/reasoning` should be shown. Gated to capable providers only.
pub fn reasoning_available() -> bool {
    crate::config::PhoenixConfig::load()
        .ok()
        .map(|c| {
            crate::providers::providers_data::supports_reasoning_effort(&c.profile.llm.provider)
        })
        .unwrap_or(false)
}

/// True if a command should be hidden for the current configuration.
fn command_hidden(cmd: &SlashCommand) -> bool {
    cmd.name == "reasoning" && !reasoning_available()
}

/// Commands visible for the current configuration (capability-gated).
pub fn visible_commands() -> Vec<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .filter(|c| !command_hidden(c))
        .collect()
}

/// Commands whose name (or an alias) prefix-matches the typed partial.
pub fn matches(partial: &str) -> Vec<&'static SlashCommand> {
    let partial = partial.trim_start_matches('/');
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| !command_hidden(cmd))
        .filter(|cmd| {
            cmd.name.starts_with(partial)
                || cmd.aliases.iter().any(|alias| alias.starts_with(partial))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_is_advertised_for_command_discovery() {
        assert!(
            visible_commands()
                .iter()
                .any(|command| command.name == "compact"),
            "/compact exists in command handling and must stay discoverable"
        );
        assert!(
            matches("/comp")
                .iter()
                .any(|command| command.name == "compact"),
            "/compact should be prefix-matchable"
        );
    }
}
