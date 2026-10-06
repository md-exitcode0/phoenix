//! Phoenix CLI rendering and interaction patterns.

use std::io::{self, Write};

use crate::config::PhoenixConfig;
use crate::providers::model_id::display_provider_model;

use super::style::{bold, cyan, dim, green, red, rule};
use super::AppState;

pub fn print_banner(state: &AppState) {
    let model = resolve_model_label(state);
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "?".to_string());
    println!();
    println!(
        "  {}  {}  {}  {}",
        bold("phoenix"),
        dim(&state.session_id),
        dim("·"),
        dim(&model),
    );
    println!(
        "  {} {} ({})  {}  {} {}",
        dim("permissions"),
        if state.yolo {
            red(state.permission_label())
        } else {
            green(state.permission_label())
        },
        dim(state.permission_detail()),
        dim("·"),
        dim("workspace"),
        dim(&cwd),
    );
    println!("  {}", rule(48));
    println!();
}

fn resolve_model_label(state: &AppState) -> String {
    if state.scaffold || state.real {
        state.mode_label().to_string()
    } else {
        PhoenixConfig::load()
            .map(|c| {
                display_provider_model(Some(&c.profile.llm.provider), &c.profile.llm.orchestrator())
            })
            .unwrap_or_else(|_| "not configured".into())
    }
}

pub fn print_help() {
    use super::registry::{visible_commands, SECTIONS};
    let visible = visible_commands();

    println!("{}", bold("Commands"));
    println!(
        "  {}",
        dim(
            "Type / to open the command menu. Up/Down moves the selection, Tab accepts it. End a line with \\ to continue."
        )
    );

    for section in SECTIONS {
        println!();
        println!("  {}", bold(section));
        for cmd in visible.iter().filter(|c| &c.section == section) {
            let args = if cmd.args.is_empty() {
                String::new()
            } else {
                format!(" {}", cmd.args)
            };
            println!("    /{:<14}{}", format!("{}{}", cmd.name, args), cmd.desc);
        }
    }
}

pub fn display_tool_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "read" => "Read",
        "write" => "Write",
        "str_replace" => "Replace",
        "grep" => "Grep",
        "glob" => "Glob",
        "list_directory" => "List",
        "bash" => "Bash",
        "talk" => "Talk",
        "codebase_search" => "CodeSearch",
        "ask_user" => "Ask",
        "todo_write" => "Todos",
        "web_search" => "Search",
        "web_fetch" => "Fetch",
        "web_scrape" => "Scrape",
        "web_crawl" => "Crawl",
        "memory_list" => "MemList",
        "memory_read" => "MemRead",
        "memory_write" => "MemWrite",
        "memory_search" => "MemSearch",
        "session_cache_read" => "Cache",
        "session_tail" => "Tail",
        "session_tool_outputs" => "ToolOut",
        "session_prune" => "Prune",
        "phase_contract" => "Contract",
        "response_validation" | "scaffold_execution" | "receive_specialist_result" => return None,
        _ => return None,
    })
}

pub fn format_tool_running(agent: &str, tool_name: &str, input_summary: &str) -> String {
    let name = display_tool_name(tool_name).unwrap_or(tool_name);
    let args = if input_summary.is_empty() {
        String::new()
    } else {
        format!(" {}", dim(input_summary))
    };
    format!("  {} {} {}{}", dim("◐"), dim(agent), bold(name), args)
}

pub fn format_tool_completed(
    agent: &str,
    tool_name: &str,
    input_summary: &str,
    success: bool,
    output_summary: &str,
) -> (String, Option<String>) {
    let glyph = if tool_name == "response_validation" || tool_name == "phase_contract" {
        dim("◦")
    } else if success {
        green("✓")
    } else {
        red("✗")
    };
    let name = display_tool_name(tool_name).unwrap_or(tool_name);
    let args = if input_summary.is_empty() {
        String::new()
    } else {
        format!(" {}", dim(input_summary))
    };
    let headline = format!("  {} {} {}{}", glyph, dim(agent), bold(name), args);
    let detail = if output_summary.trim().is_empty() {
        None
    } else {
        let first = output_summary
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(output_summary);
        let preview: String = first.chars().take(160).collect();
        let suffix = if first.chars().count() > 160 {
            "…"
        } else {
            ""
        };
        Some(format!("      {}{}", dim(&preview), dim(suffix)))
    };
    (headline, detail)
}

pub fn format_delegate(agent: &str, subject: &str) -> String {
    let brief: String = subject.chars().take(72).collect();
    let suffix = if subject.chars().count() > 72 {
        "…"
    } else {
        ""
    };
    format!(
        "  {} {}  {}{}",
        cyan("→"),
        bold(agent),
        dim(&brief),
        dim(suffix)
    )
}

pub fn format_delegate_done(agent: &str) -> String {
    format!("  {} {}", dim("←"), dim(&format!("{agent} done")))
}

pub fn format_thinking_line(spinner: &str, label: &str) -> String {
    format!("  {} {} {}", spinner, dim("*"), dim(label))
}

pub fn format_agent_thinking_header(source: &str) -> String {
    format!(
        "  {} {} {}",
        dim("*"),
        dim("Thinking"),
        dim(&format!("[{source}]"))
    )
}

/// Print the full model-thinking body (used when `/thinking` is on).
pub fn print_thinking_block(source: &str, full_text: &str) {
    println!("{}", format_agent_thinking_header(source));
    for line in full_text.lines() {
        if line.trim().is_empty() {
            println!();
        } else {
            println!("      {line}");
        }
    }
}

pub fn format_agent_thinking_line(line: &str) -> String {
    let preview: String = line.chars().take(200).collect();
    let suffix = if line.chars().count() > 200 {
        "…"
    } else {
        ""
    };
    format!("      {}{}", dim(&preview), dim(suffix))
}

pub fn print_final_answer(text: &str) {
    println!();
    if text.chars().count() > 2_500 {
        println!("{text}");
    } else {
        for line in text.lines() {
            println!("{line}");
        }
    }
    println!();
}

pub fn print_footer(
    tokens: u32,
    orchestrator_tokens: Option<(u32, u32)>,
    coder_tokens: Option<(u32, u32)>,
    trace: &str,
) {
    if tokens > 0 || !trace.is_empty() {
        let mut parts = Vec::new();
        if tokens > 0 {
            if let (Some((oi, oo)), Some((ci, co))) = (orchestrator_tokens, coder_tokens) {
                parts.push(format!(
                    "{tokens} tokens (orch {}+{}, specialists {}+{})",
                    oi, oo, ci, co
                ));
            } else {
                parts.push(format!("{tokens} tokens"));
            }
        }
        if !trace.is_empty() {
            parts.push(format!("trace {trace}"));
        }
        println!("  {} {}", dim("↳"), dim(&parts.join(" · ")));
    }
}

pub fn print_error(message: &str) {
    println!("\n  {}  {}\n", red("error"), message);
}

/// Colored edit diff for the plain (non-TUI) event printer: `+` green,
/// `-` red, `@@ path` header cyan, elision notes dim.
pub fn print_diff(diff: &str) {
    for line in diff.lines() {
        if line.starts_with("@@") {
            println!("      {}", cyan(line));
        } else if line.starts_with('+') {
            println!("      {}", green(line));
        } else if line.starts_with('-') {
            println!("      {}", red(line));
        } else {
            println!("      {}", dim(line));
        }
    }
}

pub fn flush_stdout() {
    let _ = io::stdout().flush();
}

// Re-export for commands.rs
pub use super::style::{bold as style_bold, cyan as style_cyan, dim as style_dim};

pub async fn stream_output(text: &str) {
    print_final_answer(text);
}
