//! Slash command handlers for the interactive CLI.
//!
//! Each command returns `Ok(true)` if the CLI should quit,
//! `Ok(false)` to continue the REPL.

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Select};

use crate::config::PhoenixConfig;
use crate::session;
use crate::tools;

use super::{style_bold, style_dim, AppState};

/// Handle a slash command input (e.g. "/help", "/quit").
/// Returns `Ok(true)` to signal quit.
pub fn handle(input: &str, state: &mut AppState) -> Result<bool> {
    let mut parts = input.trim_start_matches('/').split_whitespace();
    let command = parts.next().unwrap_or_default();
    let args: Vec<&str> = parts.collect();

    match command {
        "help" | "?" => help(),
        "clear" | "cls" => clear(),
        "quit" | "exit" | "q" => return Ok(true),

        "session" => show_session(state),
        "sessions" => list_sessions(state)?,
        "new" => new_session(args, state)?,
        "use" | "resume" => switch_session(args, state)?,

        "status" => status(state)?,
        "model" => show_model()?,
        "provider" => provider_cmd(args)?,
        "actions" => toggle_actions(state),
        "thinking" => toggle_thinking(state),
        "reasoning" => reasoning_cmd(args)?,
        "debug" => toggle_debug(state),
        "compact" => compact(state),

        "yolo" => set_yolo(state, true),
        "safe" | "workspace" => set_yolo(state, false),
        "defaults" => defaults_cmd(args, state),

        "memory" => memory(state),
        "prompt" => show_prompt(state),
        "tools" => show_tools(),
        "doctor" => doctor()?,

        other => {
            println!("  {}  Unknown command: /{other}", style_dim("·"));
            println!("  {}  Type /help for available commands.", style_dim("·"));
        }
    }

    Ok(false)
}

// ── Reasoning ─────────────────────────────────────────────────────────

// OpenAI Codex reasoning effort levels (no `minimal` in practice; gpt-5.4 adds
// `xhigh`, gpt-5.4-mini does not; `max` is GPT-5.6 Sol only — picking it on
// another model surfaces the provider's own error, visibly).
const REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// `/reasoning [level]` — set the model's reasoning effort. Only meaningful for
/// providers that support it (currently openai-codex); the command is hidden
/// elsewhere, but we re-check here in case it's typed directly.
fn reasoning_cmd(args: Vec<&str>) -> Result<()> {
    let cfg = PhoenixConfig::load().ok();
    let provider = cfg
        .as_ref()
        .map(|c| c.profile.llm.provider.clone())
        .unwrap_or_default();
    if !crate::providers::providers_data::supports_reasoning_effort(&provider) {
        println!(
            "  {}  /reasoning isn't available for provider '{}' yet.",
            style_dim("·"),
            provider
        );
        return Ok(());
    }

    let current = cfg
        .as_ref()
        .and_then(|c| c.profile.llm.reasoning_effort.clone());
    let level = match args.first() {
        Some(arg) => {
            let a = arg.to_ascii_lowercase();
            if !REASONING_LEVELS.contains(&a.as_str()) {
                println!(
                    "  {}  Unknown level '{}'. Use: {}",
                    style_dim("·"),
                    arg,
                    REASONING_LEVELS.join(" | ")
                );
                return Ok(());
            }
            a
        }
        None => {
            let theme = crate::config::setup::phoenix_theme();
            let default_idx = current
                .as_deref()
                .and_then(|c| REASONING_LEVELS.iter().position(|l| *l == c))
                .or_else(|| REASONING_LEVELS.iter().position(|l| *l == "medium"))
                .unwrap_or(0);
            let idx = dialoguer::Select::with_theme(&theme)
                .with_prompt("  Reasoning effort")
                .items(REASONING_LEVELS)
                .default(default_idx)
                .interact()?;
            REASONING_LEVELS[idx].to_string()
        }
    };

    set_config_reasoning_effort(&level)?;
    println!(
        "  {}  Reasoning effort set to {} (applies to the next turn).",
        style_dim("·"),
        style_bold(&level)
    );
    Ok(())
}

/// Upsert `reasoning_effort = "<level>"` under `[profile.llm]` in config.toml.
/// Raw text edit (there is no config serializer); placed at the end of the
/// `[profile.llm]` table, before any subtable like `[profile.llm.auth]`.
fn set_config_reasoning_effort(level: &str) -> Result<()> {
    let path = crate::config::phoenix_home().join("config.toml");
    let text = String::from_utf8(
        crate::config::private_io::read_private_file(&path)?
            .with_context(|| format!("missing {}", path.display()))?,
    )
    .with_context(|| format!("{} is not UTF-8", path.display()))?;
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut wrote = false;
    let new_line = format!("reasoning_effort = \"{level}\"");
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if in_llm && !wrote {
                out.push(new_line.clone());
                wrote = true;
            }
            in_llm = trimmed.starts_with("[profile.llm]");
        }
        if in_llm && trimmed.starts_with("reasoning_effort") {
            out.push(new_line.clone());
            wrote = true;
            continue;
        }
        out.push(line.to_string());
    }
    if in_llm && !wrote {
        out.push(new_line.clone());
        wrote = true;
    }
    if !wrote {
        out.push("[profile.llm]".to_string());
        out.push(new_line);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    super::prefs::replace_config_atomic(&path, Some(&text), &joined)?;
    Ok(())
}

// ── General ───────────────────────────────────────────────────────────

fn help() {
    super::render::print_help();
}

fn clear() {
    print!("\x1b[2J\x1b[H");
    // A cleared screen is a fresh start: index the pending memory backlog now
    // (detached, best-effort on the gateway) so everything already saved is
    // searchable from here on.
    tokio::spawn(async {
        let _ = super::daemon::request_memory_index().await;
    });
}

// ── Sessions ──────────────────────────────────────────────────────────

fn show_session(state: &AppState) {
    println!(
        "  {}  session: {}",
        style_dim("·"),
        style_bold(&state.session_id)
    );
}

fn list_sessions(state: &AppState) -> Result<()> {
    let root = crate::config::phoenix_home().join("sessions");
    let mut store = session::SessionStore::new(root);
    store.load_from_disk()?;
    let mut sessions = store.list();
    sessions.sort_by(|a, b| a.id.cmp(&b.id));

    if sessions.is_empty() {
        println!("  {}  No persisted sessions.", style_dim("·"));
        return Ok(());
    }

    println!("{}", style_bold("Sessions"));
    for s in &sessions {
        let marker = if s.id == state.session_id { "*" } else { " " };
        let kind = match &s.kind {
            session::SessionKind::Main => "main".to_string(),
            session::SessionKind::SubAgent(t) => format!("sub:{t}"),
        };
        println!(
            "  {} {:<42} {:<14} {} msgs",
            marker, s.id, kind, s.message_count,
        );
    }
    Ok(())
}

fn validated_session_id(value: String) -> Result<String> {
    session::SessionStore::validate_session_id(&value)
        .with_context(|| format!("invalid session id `{value}`"))?;
    Ok(value)
}

fn new_session(args: Vec<&str>, state: &mut AppState) -> Result<()> {
    let next = validated_session_id(
        args.first()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("main-{}", uuid::Uuid::new_v4())),
    )?;
    // Switching away ends the old session — digest it into memory now
    // (best-effort, detached on the gateway) so the new one can recall it.
    let ending = state.session_id.clone();
    tokio::spawn(async move {
        let _ = super::daemon::request_session_digest(ending).await;
    });
    state.session_id = next;
    println!(
        "  {}  session: {}",
        style_dim("·"),
        style_bold(&state.session_id)
    );
    Ok(())
}

fn switch_session(args: Vec<&str>, state: &mut AppState) -> Result<()> {
    let root = crate::config::phoenix_home().join("sessions");
    let mut store = session::SessionStore::new(root);
    store.load_from_disk()?;

    // No id → list resumable main sessions and return.
    let Some(id) = args.first().map(|s| s.to_string()) else {
        let mut mains: Vec<_> = store
            .list()
            .into_iter()
            .filter(|s| matches!(s.kind, session::SessionKind::Main))
            .collect();
        mains.sort_by(|a, b| a.id.cmp(&b.id));
        if mains.is_empty() {
            println!(
                "  {}  No resumable sessions yet. Usage: /use <id>",
                style_dim("·")
            );
            return Ok(());
        }
        println!("{}", style_bold("Resumable sessions"));
        for s in &mains {
            let marker = if s.id == state.session_id { "*" } else { " " };
            println!("  {} {:<42} {} msgs", marker, s.id, s.message_count);
        }
        println!(
            "  {}  /use <id> to resume and load its context.",
            style_dim("·")
        );
        return Ok(());
    };

    let id = validated_session_id(id)?;
    state.session_id = id.clone();
    println!(
        "  {}  session: {}",
        style_dim("·"),
        style_bold(&state.session_id)
    );
    match store.get(&id) {
        Some(session) => print_session_context(session),
        None => println!(
            "  {}  New/empty session — no prior context to load.",
            style_dim("·")
        ),
    }
    Ok(())
}

/// Print the recent conversation (user/assistant turns) so the user can see
/// what they're resuming. Tool/talk noise is omitted for readability.
fn print_session_context(session: &session::Session) {
    let turns: Vec<&session::Message> = session
        .messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                session::Message::User { .. } | session::Message::Assistant { .. }
            )
        })
        .collect();
    if turns.is_empty() {
        println!("  {}  No conversation yet in this session.", style_dim("·"));
        return;
    }
    println!("{}", style_bold("Session context"));
    let start = turns.len().saturating_sub(12);
    if start > 0 {
        println!("  {}  …{} earlier turn(s) hidden", style_dim("·"), start);
    }
    for message in &turns[start..] {
        match message {
            session::Message::User { content } => {
                println!("  {} {}", style_bold("you"), truncate_line(content));
            }
            session::Message::Assistant { content } => {
                println!("  {} {}", style_dim("phoenix"), truncate_line(content));
            }
            _ => {}
        }
    }
}

fn truncate_line(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first.chars().count() > 160 {
        format!("{}…", first.chars().take(160).collect::<String>())
    } else {
        first.to_string()
    }
}

fn defaults_cmd(args: Vec<&str>, state: &AppState) {
    match args.first().copied() {
        Some("save") => match super::prefs::save(state.current_defaults()) {
            Ok(path) => println!(
                "  {}  Saved current toggles (yolo/actions/thinking/debug) as defaults → {}",
                style_dim("·"),
                path.display()
            ),
            Err(error) => println!("  {}  Failed to save defaults: {error}", style_dim("·")),
        },
        _ => println!(
            "  {}  Usage: /defaults save  — persists current yolo/actions/thinking/debug toggles.",
            style_dim("·")
        ),
    }
}

// ── Runtime ───────────────────────────────────────────────────────────

fn status(state: &AppState) -> Result<()> {
    println!("{}", style_bold("Runtime"));
    println!("  session:    {}", state.session_id);
    println!("  mode:       {}", state.mode_label());
    println!(
        "  actions:    {}",
        if state.show_actions { "on" } else { "off" }
    );
    println!(
        "  thinking:   {}  (* spinner always on; model thinking text when on)",
        if state.show_reasoning { "on" } else { "off" }
    );
    println!(
        "  debug:      {}",
        if state.show_debug { "on" } else { "off" }
    );
    println!(
        "  perms:      {}",
        if state.yolo {
            "yolo (unconfined — can act outside cwd)"
        } else {
            "workspace (confined to cwd)"
        }
    );
    println!("  cwd:        {}", std::env::current_dir()?.display());
    println!(
        "  composio:   {}",
        crate::tools::composio::local_status_line()
    );

    if let Some(ref run) = state.last_run {
        println!("{}", style_bold("Last Run"));
        println!("  id:         {}", run.run_id);
        println!("  route:      {}", run.route);
        println!("  tokens:     {}", run.token_total);
        println!("  trace:      {}", relative_path(&run.trace_path));
        let first_line = run.reply_summary.lines().next().unwrap_or("—");
        println!("  reply:      {}", first_line);
    }
    Ok(())
}

// ── Provider switching ────────────────────────────────────────────────

/// Providers offered by `/provider`. Codex stays the cloud OAuth lane; Ollama
/// covers both local (no auth) and Ollama Cloud (API key, "max plan" hosted
/// models). Other catalog providers still work by editing config.toml.
const SWITCHABLE_PROVIDERS: &[(&str, &str)] = &[
    ("openai-codex", "OpenAI Codex (cloud, OAuth login)"),
    ("ollama", "Ollama local (localhost:11434, no key)"),
    ("ollama-cloud", "Ollama Cloud (ollama.com, API key)"),
];

/// Rewrite config.toml for a provider switch: `provider`/`model` in
/// `[profile.llm]`, and the `[profile.llm.auth]` subtable replaced to match
/// (api_key profile for ollama-cloud, oauth profile for openai-codex, no auth
/// table for local ollama). Returns a human receipt. The pasted key itself
/// goes to auth-profiles.json, never into config.toml.
pub fn switch_provider_config(
    provider: &str,
    model: Option<&str>,
    api_key: Option<&str>,
) -> Result<String> {
    let known = crate::providers::providers_data::get_provider(provider)
        .ok_or_else(|| anyhow::anyhow!("unknown provider '{provider}'"))?;

    // Model: explicit arg wins; otherwise the provider's catalog default.
    let model = model
        .map(|m| m.to_string())
        .unwrap_or_else(|| match provider {
            "ollama" => "llama3.2".to_string(),
            "ollama-cloud" => "gpt-oss:20b".to_string(),
            "openai-codex" => "gpt-5.5".to_string(),
            _ => known
                .models
                .first()
                .map(|m| m.id.to_string())
                .unwrap_or_default(),
        });

    // Auth: persist a pasted key as an auth profile; config only points at it.
    let auth_lines: Vec<String> = match provider {
        "ollama" => Vec::new(),
        "ollama-cloud" => {
            let profile_id = "ollama-cloud:default".to_string();
            match api_key.map(str::trim).filter(|k| !k.is_empty()) {
                Some(key) => {
                    crate::config::auth_profile::update_auth_profile_store(|store| {
                        store.profiles.insert(
                            profile_id.clone(),
                            crate::config::auth_profile::AuthProfileCredential::ApiKey {
                                provider: "ollama-cloud".to_string(),
                                key: key.to_string(),
                                display_name: Some("Ollama Cloud".to_string()),
                            },
                        );
                        Ok(())
                    })?;
                }
                None => {
                    // No key pasted: OLLAMA_API_KEY in the env or an existing
                    // saved profile still works — only fail if neither exists.
                    let store = crate::config::auth_profile::load_auth_profile_store()?;
                    if !store.profiles.contains_key(&profile_id)
                        && std::env::var("OLLAMA_API_KEY").is_err()
                    {
                        anyhow::bail!(
                            "ollama-cloud needs an API key: /provider ollama-cloud <api-key> [model]"
                        );
                    }
                }
            }
            vec![
                "[profile.llm.auth]".to_string(),
                "method = \"api_key\"".to_string(),
                "source = \"profile\"".to_string(),
                format!("profile = \"{profile_id}\""),
            ]
        }
        "openai-codex" => vec![
            "[profile.llm.auth]".to_string(),
            "method = \"oauth\"".to_string(),
            "source = \"profile\"".to_string(),
            "profile = \"openai-codex:default\"".to_string(),
        ],
        other => {
            anyhow::bail!(
                "/provider handles {} for now; switch to '{other}' by editing config.toml",
                SWITCHABLE_PROVIDERS
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        }
    };

    rewrite_config_provider(&model, provider, &auth_lines)?;

    let model_note = if known.has_model(&model) || provider != "ollama" {
        String::new()
    } else {
        " (not in the built-in catalog — fine if it's pulled locally)".to_string()
    };
    Ok(format!(
        "provider → {provider}, model → {model}{model_note}. Applies from the next turn; run `phoenix restart` if the gateway caches config."
    ))
}

/// Line-surgical config.toml rewrite (same approach as reasoning_effort):
/// set provider/model inside `[profile.llm]` (before any subtable), drop any
/// existing `[profile.llm.auth]` table, and append the new one at the end.
fn rewrite_config_provider(model: &str, provider: &str, auth_lines: &[String]) -> Result<()> {
    let path = crate::config::phoenix_home().join("config.toml");
    let text = String::from_utf8(
        crate::config::private_io::read_private_file(&path)?
            .with_context(|| format!("missing {}", path.display()))?,
    )
    .with_context(|| format!("{} is not UTF-8", path.display()))?;
    let mut out: Vec<String> = Vec::new();
    let mut in_llm = false;
    let mut in_llm_auth = false;
    let mut wrote_provider = false;
    let mut wrote_model = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_llm = trimmed.starts_with("[profile.llm]");
            in_llm_auth = trimmed.starts_with("[profile.llm.auth]");
        }
        if in_llm_auth {
            continue; // old auth table (header + keys) is dropped wholesale
        }
        if in_llm && trimmed.starts_with("provider ") || in_llm && trimmed.starts_with("provider=")
        {
            out.push(format!("provider = \"{provider}\""));
            wrote_provider = true;
            continue;
        }
        if in_llm && (trimmed.starts_with("model ") || trimmed.starts_with("model=")) {
            out.push(format!("model = \"{model}\""));
            wrote_model = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !wrote_provider || !wrote_model {
        anyhow::bail!(
            "config.toml has no [profile.llm] provider/model lines to update — run `phoenix onboard`"
        );
    }
    if !auth_lines.is_empty() {
        out.push(String::new());
        out.extend(auth_lines.iter().cloned());
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    super::prefs::replace_config_atomic(&path, Some(&text), &joined)?;
    Ok(())
}

/// `/provider [id] [api-key] [model]` — pick the active LLM lane. With no args,
/// an interactive picker (plus hidden key prompt for ollama-cloud).
fn provider_cmd(args: Vec<&str>) -> Result<()> {
    let current = PhoenixConfig::load()
        .map(|c| c.profile.llm.provider.clone())
        .unwrap_or_default();

    let (provider, key_arg, model_arg): (String, Option<String>, Option<String>) =
        match args.first() {
            Some(id) => (
                id.to_string(),
                args.get(1).map(|s| s.to_string()),
                args.get(2).map(|s| s.to_string()),
            ),
            None => {
                let theme = crate::config::setup::phoenix_theme();
                let items: Vec<String> = SWITCHABLE_PROVIDERS
                    .iter()
                    .map(|(id, label)| {
                        if *id == current {
                            format!("{label}  (current)")
                        } else {
                            label.to_string()
                        }
                    })
                    .collect();
                let default_idx = SWITCHABLE_PROVIDERS
                    .iter()
                    .position(|(id, _)| *id == current)
                    .unwrap_or(0);
                let idx = Select::with_theme(&theme)
                    .with_prompt("  Provider")
                    .items(&items)
                    .default(default_idx)
                    .interact()?;
                let id = SWITCHABLE_PROVIDERS[idx].0.to_string();
                let key = if id == "ollama-cloud" {
                    let existing = crate::config::auth_profile::load_auth_profile_store()
                        .map(|s| s.profiles.contains_key("ollama-cloud:default"))
                        .unwrap_or(false);
                    let prompt = if existing {
                        "  Ollama Cloud API key (Enter keeps the saved one)"
                    } else {
                        "  Ollama Cloud API key"
                    };
                    let typed: String = dialoguer::Password::with_theme(&theme)
                        .with_prompt(prompt)
                        .allow_empty_password(existing)
                        .interact()?;
                    Some(typed).filter(|k| !k.trim().is_empty())
                } else {
                    None
                };
                (id, key, None)
            }
        };

    // Arg form: `/provider ollama llama3.2` has no key slot — treat the 2nd
    // arg as a model for keyless providers.
    let (key, model) = if provider == "ollama-cloud" {
        (key_arg, model_arg)
    } else {
        (None, key_arg.or(model_arg))
    };

    match switch_provider_config(&provider, model.as_deref(), key.as_deref()) {
        Ok(receipt) => println!("  {}  {}", style_dim("·"), receipt),
        Err(error) => println!("  {}  {}", style_dim("·"), error),
    }
    Ok(())
}

fn show_model() -> Result<()> {
    match PhoenixConfig::load() {
        Ok(c) => {
            let llm = &c.profile.llm;
            println!(
                "  {}  orchestrator: {}/{}",
                style_dim("·"),
                llm.provider,
                llm.orchestrator()
            );
            if llm.specialist() != llm.orchestrator() {
                println!("  {}  specialist:   {}", style_dim("·"), llm.specialist());
            }
            if llm.librarian() != llm.orchestrator() {
                println!("  {}  librarian:    {}", style_dim("·"), llm.librarian());
            }
        }
        Err(_) => {
            println!(
                "  {}  Not configured. Run `phoenix onboard` first.",
                style_dim("·")
            );
        }
    }
    Ok(())
}

fn toggle_actions(state: &mut AppState) {
    state.show_actions = !state.show_actions;
    println!(
        "  {}  actions: {}",
        style_dim("·"),
        if state.show_actions { "on" } else { "off" }
    );
    maybe_offer_save_defaults(state);
}

fn toggle_thinking(state: &mut AppState) {
    state.show_reasoning = !state.show_reasoning;
    println!(
        "  {}  thinking: {}  (* indicator always visible; {}model thinking text)",
        style_dim("·"),
        if state.show_reasoning { "on" } else { "off" },
        if state.show_reasoning { "" } else { "no " }
    );
    maybe_offer_save_defaults(state);
}

fn toggle_debug(state: &mut AppState) {
    state.show_debug = !state.show_debug;
    println!(
        "  {}  debug: {}",
        style_dim("·"),
        if state.show_debug { "on" } else { "off" }
    );
    maybe_offer_save_defaults(state);
}

fn compact(state: &AppState) {
    let home = crate::config::phoenix_home();
    let session_root = home.join("sessions");

    // Count messages in the current session
    let (msg_count, tool_count) = {
        let mut store = session::SessionStore::new(&session_root);
        let _ = store.load_from_disk();
        match store.get(&state.session_id) {
            Some(session) => {
                let tool_count = session
                    .messages
                    .iter()
                    .filter(|m| matches!(m, session::Message::ToolResult { .. }))
                    .count();
                (session.messages.len(), tool_count)
            }
            None => (0, 0),
        }
    };

    println!("{}", style_bold("Session Compaction"));
    println!("  session:  {}", state.session_id);
    println!("  messages: {msg_count} total, {tool_count} tool results");

    if let Some(ref run) = state.last_run {
        println!("  tokens:   {}", run.token_total);
        println!("  route:    {}", run.route);
    }

    // Show memory directory sizes
    let memory_root = home.join("memory");
    let mut total_memories = 0usize;
    for tier in &["HOT", "WARM", "COLD"] {
        let tier_path = memory_root.join(tier);
        if tier_path.exists() {
            if let Ok(entries) = std::fs::read_dir(&tier_path) {
                let count = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                    .count();
                total_memories += count;
            }
        }
    }
    println!("  memories: {total_memories} file(s) across HOT/WARM/COLD tiers");

    println!();
    println!(
        "  {}  Librarian automatically compacts sessions via preload/prune/save on each turn.",
        style_dim("·")
    );
    println!(
        "  {}  To start fresh, use {} to begin a new session.",
        style_dim("·"),
        style_bold("/new")
    );
}

fn set_yolo(state: &mut AppState, yolo: bool) {
    state.yolo = yolo;
    if yolo {
        println!(
            "  {}  YOLO mode ON — tools may read/write/execute {} the launch directory. Use absolute paths to reach outside.",
            style_dim("·"),
            style_bold("outside")
        );
    } else {
        println!(
            "  {}  Workspace mode — tools are confined to the launch directory.",
            style_dim("·")
        );
    }
    maybe_offer_save_defaults(state);
}

fn maybe_offer_save_defaults(state: &mut AppState) {
    if state.prompted_default_offer {
        return;
    }
    state.prompted_default_offer = true;

    let choice = Select::with_theme(&crate::config::setup::phoenix_theme())
        .with_prompt("Persist current CLI toggle defaults?")
        .items(&[
            "This session only",
            "Save as default in ~/.phoenix/config.toml",
        ])
        .default(0)
        .interact_opt();

    let Ok(Some(1)) = choice else {
        return;
    };

    match super::prefs::save(state.current_defaults()) {
        Ok(path) => println!(
            "  {}  Saved current toggles (yolo/actions/thinking/debug) as defaults → {}",
            style_dim("·"),
            path.display()
        ),
        Err(error) => println!("  {}  Failed to save defaults: {error}", style_dim("·")),
    }
}

// ── Inspection ────────────────────────────────────────────────────────

fn memory(state: &AppState) {
    println!("{}", style_bold("Memory"));
    match &state.last_run {
        Some(run) => {
            println!("  trace:   {}", relative_path(&run.trace_path));
            println!("  tokens:  {}", run.token_total);
            println!(
                "  {}",
                style_dim("Memory details: use /status for full state or check trace file.")
            );
        }
        None => {
            println!("  {}  No run completed yet.", style_dim("·"));
        }
    }
}

fn show_prompt(state: &AppState) {
    match &state.last_prompt {
        Some(p) => {
            println!("{}", style_bold("Last Prompt"));
            println!("  agent:  {}", p.agent);
            println!(
                "  system: {} lines, {} chars",
                p.system_prompt.lines().count(),
                p.system_prompt.len()
            );
            println!(
                "  user:   {} lines, {} chars",
                p.user_prompt.lines().count(),
                p.user_prompt.len()
            );
            println!();
            println!("{}", style_bold("System Prompt"));
            println!("{}", &p.system_prompt);
            println!();
            println!("{}", style_bold("User Prompt"));
            println!("{}", &p.user_prompt);
        }
        None => {
            println!(
                "  {}  No prompt assembled yet. Run a task first.",
                style_dim("·")
            );
        }
    }
}

fn show_tools() {
    let orch_tools = tools::tool_definitions_for_agent(&[
        "talk".into(),
        "read".into(),
        "write".into(),
        "glob".into(),
        "grep".into(),
        "list_directory".into(),
        "bash".into(),
        "ask_user".into(),
        "todo_write".into(),
        "web_search".into(),
        "web_fetch".into(),
    ]);
    let coder_tools = tools::tool_definitions_for_agent(&[
        "read".into(),
        "write".into(),
        "str_replace".into(),
        "grep".into(),
        "glob".into(),
        "list_directory".into(),
        "bash".into(),
        "talk".into(),
    ]);

    println!("{}", style_bold("Tool Surface"));
    println!();
    println!(
        "  {} ({} tools)",
        style_bold("orchestrator"),
        orch_tools.len()
    );
    for t in &orch_tools {
        println!("    {:<18} {}", style_dim(&t.name), t.description);
    }
    println!();
    println!("  {} ({} tools)", style_bold("coder"), coder_tools.len());
    for t in &coder_tools {
        println!("    {:<18} {}", style_dim(&t.name), t.description);
    }
    println!();
    println!("  {} (7 tools)", style_bold("librarian"));
    for (name, desc) in [
        ("memory_list", "List memory candidates across tiers"),
        ("memory_read", "Read one memory entry in full"),
        ("memory_write", "Write/update a canonical memory entry"),
        ("session_prune", "Compact old tool-result messages"),
        ("read", "Read workspace files"),
        ("glob", "File discovery by pattern"),
        ("grep", "Ripgrep-based code search"),
    ] {
        println!("    {:<18} {}", style_dim(name), desc);
    }
}

fn doctor() -> Result<()> {
    let home = crate::config::phoenix_home();
    let config_ok = PhoenixConfig::load().is_ok();
    let memory_ok = home.join("memory").exists();
    let session_root = home.join("sessions");
    let history_path = home.join("history");

    println!("{}", style_bold("Doctor"));
    println!("  home:     {}", home.display());
    println!("  config:   {}", if config_ok { "ok" } else { "missing" });
    println!("  memory:   {}", if memory_ok { "ok" } else { "missing" });
    println!("  sessions: {}", session_root.display());
    println!("  history:  {}", history_path.display());
    println!("  {}", style_dim("Run `cargo test` for full test suite."));
    Ok(())
}

/// `phoenix memory` — inspect the Phoenix knowledge graph. With `--graph`,
/// render the whole graph to an HTML file and print its path. With a query,
/// recall the matching chunks and print them. With neither, show a short status.
#[cfg(feature = "cognee")]
pub async fn run_memory(graph: bool, graph_json: bool, query: String) -> Result<()> {
    use crate::librarian::cognee::{CogneeConfig, CogneeMemory};

    let config = PhoenixConfig::load()?;
    let cognee_config = CogneeConfig::from_phoenix_config(&config)?;
    let memory = CogneeMemory::open(&cognee_config)?;

    if graph_json {
        // The REAL graph as data (nodes + relationship edges) — the dashboard
        // parses this instead of the raw note files.
        let data = memory.graph_data().await?;
        println!("{}", serde_json::to_string(&data)?);
        return Ok(());
    }

    if graph {
        let out = crate::config::phoenix_cognee_root().join("graph.html");
        let path = memory.visualize(&out).await?;
        println!("Knowledge graph rendered → {}", path.display());
        return Ok(());
    }

    let query = query.trim();
    if query.is_empty() {
        println!("{}", style_bold("Memory"));
        println!(
            "  store: {}",
            crate::config::phoenix_cognee_root().display()
        );
        println!(
            "  {}",
            style_dim(
                "Usage: `phoenix memory <query>` to recall, `phoenix memory --graph` for the graph HTML."
            )
        );
        return Ok(());
    }

    let chunks = match crate::librarian::memory::search(query, "CHUNKS", 8).await {
        crate::librarian::memory::SearchOutcome::Hits(chunks) => chunks,
        crate::librarian::memory::SearchOutcome::Empty => Vec::new(),
        other => anyhow::bail!("{other}"),
    };
    if chunks.is_empty() {
        println!("No memory recalled for {:?} (cold or empty store).", query);
    } else {
        println!("{} ({} chunks)", style_bold("Recall"), chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            println!("\n{}", style_dim(&format!("── {} ──", i + 1)));
            println!("{chunk}");
        }
    }
    Ok(())
}

/// Semantic memory is compiled out — memory inspection is unavailable.
#[cfg(not(feature = "cognee"))]
pub async fn run_memory(_graph: bool, _graph_json: bool, _query: String) -> Result<()> {
    println!("Phoenix semantic memory is unavailable in this minimal build.");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────

fn relative_path(path: &std::path::Path) -> String {
    super::relative_path(&path.to_path_buf())
}

#[cfg(test)]
mod session_command_tests {
    use super::validated_session_id;

    #[test]
    fn new_and_use_session_ids_share_the_gateway_validator() {
        assert_eq!(
            validated_session_id("main-safe_123".to_string()).unwrap(),
            "main-safe_123"
        );
        for invalid in ["../../escape", "has.space", "has space", ""] {
            assert!(
                validated_session_id(invalid.to_string()).is_err(),
                "must reject {invalid:?}"
            );
        }
    }
}
