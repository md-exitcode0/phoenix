//! Interactive ask_user collection for the Phoenix CLI.

use std::io::{self, Write};

use anyhow::{bail, Result};
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crate::cli::render::style_dim;
use crate::tools::ask_user::{AskUserInput, AskUserQuestion};

pub fn collect_answers(input: &AskUserInput) -> Result<String> {
    if input.questions.is_empty() {
        bail!("ask_user requires at least one question");
    }

    let total = input.questions.len();
    let mut lines = vec![format!("Collected {total} answer(s) from user:")];
    let theme = crate::config::setup::phoenix_theme();

    println!();
    println!(
        "  {} Phoenix needs your input ({} question{})",
        style_dim("◆"),
        total,
        if total == 1 { "" } else { "s" }
    );

    for (i, q) in input.questions.iter().enumerate() {
        let header = q.header.as_deref().unwrap_or("Question");
        print_question_box(i + 1, total, header, &q.question);

        let answer = if q.options.is_empty() {
            Input::with_theme(&theme)
                .with_prompt("  Your answer")
                .allow_empty(false)
                .interact_text()
                .map_err(|_| anyhow::anyhow!("ask_user cancelled"))?
        } else if q.multi_select {
            collect_multi_select(&theme, q)?
        } else {
            collect_single_select(&theme, q)?
        };

        lines.push(format!("\n  [{header}] Q: {}", q.question));
        lines.push(format!("  A: {answer}"));
    }

    println!();
    Ok(lines.join("\n"))
}

fn print_question_box(index: usize, total: usize, header: &str, question: &str) {
    // Every row renders exactly `width + 2` cells so the box edges align:
    // the old version appended the title AFTER the top bar (it jutted past
    // the corner) and drew a bottom bar two cells short.
    let title = format!(" {header} ({index}/{total}) ");
    let width = 60usize.max(title.chars().count() + 2);
    let text_width = width - 2;
    let top_bar = "─".repeat(width - title.chars().count());
    println!();
    println!("  ╭{title}{top_bar}╮");
    for line in wrap_lines(question, text_width) {
        println!("  │ {line:<text_width$} │");
    }
    println!("  ╰{}╯", "─".repeat(width));
    let _ = io::stdout().flush();
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let w = width.max(20);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() <= w {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn collect_single_select(theme: &ColorfulTheme, q: &AskUserQuestion) -> Result<String> {
    let mut items: Vec<String> = q.options.clone();
    items.push("Other (type custom answer)".to_string());
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("  Choose one")
        .items(&refs)
        .default(0)
        .interact()
        .map_err(|_| anyhow::anyhow!("ask_user cancelled"))?;
    if idx == items.len() - 1 {
        Ok(Input::with_theme(theme)
            .with_prompt("  Custom answer")
            .allow_empty(false)
            .interact_text()
            .map_err(|_| anyhow::anyhow!("ask_user cancelled"))?)
    } else {
        Ok(items[idx].clone())
    }
}

fn collect_multi_select(theme: &ColorfulTheme, q: &AskUserQuestion) -> Result<String> {
    let mut selected = Vec::new();
    println!(
        "  {} (enter numbers separated by commas, e.g. 1,3)",
        style_dim("Options:")
    );
    for (j, opt) in q.options.iter().enumerate() {
        println!("    {}. {}", j + 1, opt);
    }
    let raw: String = Input::with_theme(theme)
        .with_prompt("  Selection (e.g. 1,3 or done)")
        .allow_empty(true)
        .interact_text()
        .map_err(|_| anyhow::anyhow!("ask_user cancelled"))?;
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("done") && selected.is_empty() {
        bail!("ask_user multi-select requires at least one choice");
    }
    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("done") {
        for part in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
            if part.is_empty() {
                continue;
            }
            if let Ok(n) = part.parse::<usize>() {
                if let Some(opt) = q.options.get(n.saturating_sub(1)) {
                    if !selected.contains(opt) {
                        selected.push(opt.clone());
                    }
                }
            }
        }
    }
    if selected.is_empty() {
        bail!("ask_user multi-select: no valid options chosen");
    }
    Ok(selected.join(", "))
}
