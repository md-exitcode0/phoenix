//! Phoenix — the open-source whole-computer agent inbox.
//!
//! Core components:
//! - Orchestrator: routes work, coordinates specialists, produces final answers
//! - Coder: writes, edits, tests, and verifies code
//! - Librarian: hidden memory worker (runs at session boundaries)
//! - Memory: folder-based (HOT/WARM/COLD)
//! - Providers: curated provider/model catalog

mod auth;
mod channels;
mod cli;
mod codegraph;
mod config;
mod cron;
mod debug_session;
mod librarian;
mod notifications;
mod onboarding;
mod orchestrator;
mod providers;
mod runtime;
mod security;
mod session;
mod settings;
mod sub_agents;
mod tools;
mod vital_memory_document;
mod voice;
mod wire_protocol;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

use crate::config::PhoenixConfig;

#[derive(Parser)]
#[command(name = "phoenix")]
#[command(about = "Phoenix agent runtime")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum ChannelAction {
    /// List saved desktop connections and their delivery status
    List { #[arg(long)] summary: bool },
    /// Save connection JSON from stdin (contains no token)
    Save,
    /// Remove a disconnected connection; history is retained
    Remove { id: String },
    /// After handling an interrupted run in Phoenix, unblock queued messages without resubmitting it
    AcknowledgeTurn { connection: std::path::PathBuf, turn_id: String, #[arg(long)] snapshot: Option<String> },
    /// After checking the destination, resolve an interrupted send. Retry can duplicate a reply if it already arrived.
    ResolveDelivery { connection: std::path::PathBuf, delivery_id: String, #[arg(long)] retry: bool, #[arg(long)] snapshot: Option<String> },
    /// Remember a bot token from stdin in the encrypted Phoenix vault
    RememberToken { connection: std::path::PathBuf },
    /// Forget a disconnected channel's saved bot login
    ForgetToken { connection: std::path::PathBuf },
    /// Run a saved connection using its vault login, environment token, or stdin
    Run { connection: std::path::PathBuf, #[arg(long)] token_stdin: bool },
    /// Read local connection and delivery status without exposing message contents
    Status { connection: std::path::PathBuf, #[arg(long)] summary: bool },
    /// Read one exact page of recovery identities and bounded stored excerpts
    ReviewPage { connection: std::path::PathBuf, #[arg(long)] kind: String,
        #[arg(long, default_value_t = 0)] offset: usize, #[arg(long, default_value_t = 10)] limit: usize,
        #[arg(long)] snapshot: Option<String> },
    /// Verify the bot login without sending a message
    Check { connection: std::path::PathBuf, #[arg(long)] token_stdin: bool },
}

#[derive(Subcommand)]
enum Commands {
    /// Connect Telegram or Discord to a chosen agent conversation
    Channels {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// Start the interactive agent REPL, or run one task (positional or piped stdin)
    Start {
        /// Run with a built-in mock provider for diagnostics
        #[arg(long)]
        scaffold: bool,
        /// Run with real local tool execution
        #[arg(long)]
        real: bool,
        /// Continue a specific session
        #[arg(long)]
        session: Option<String>,
        /// Create a brand new session
        #[arg(long)]
        new_session: bool,
        /// Show librarian debug internals
        #[arg(long)]
        librarian_debug: bool,
        /// Run tools unconfined (allow reads/writes/exec outside the launch dir)
        #[arg(long)]
        yolo: bool,
        /// One-shot task (remaining args joined)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        task: Vec<String>,
    },
    /// First-run provider/model onboarding
    #[command(alias = "setup")]
    Onboard {
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        auth: Option<String>,
    },
    /// Change any configured setting after onboarding (browser, Composio, models)
    #[command(alias = "config")]
    Configure,
    /// Validate configured provider/model connectivity
    Check {
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// List available providers
    Providers {
        #[arg(short, long)]
        list: bool,
        /// Machine-readable catalog + configured auth summaries (dashboard)
        #[arg(long)]
        json: bool,
    },
    /// Manage stored auth profiles headlessly (used by the canvas dashboard)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Native browser OAuth or device-code login for a provider
    Login {
        /// Provider id from `phoenix providers --json`
        provider: String,
        /// Catalog auth method: oauth, device_code, or device_code_cn
        #[arg(long, default_value = "oauth")]
        method: String,
        /// Store under this profile id (e.g. grok-cli:2) — omit to take the
        /// next free slot, so several subscriptions can be added side by side
        #[arg(long)]
        profile: Option<String>,
    },
    /// Stop the running gateway daemon
    Stop,
    /// Stop the running gateway (if any) and run a fresh one in this terminal
    Restart,
    /// Run the smoke suite against the running gateway (alias: smoke).
    /// Internal regression gate — NOT the public benchmark (Terminal-Bench is).
    #[command(alias = "smoke")]
    Bench {
        /// Label for this run in bench/RESULTS.md (default: timestamp)
        #[arg(long)]
        label: Option<String>,
        /// Include the desktop task (acts on the live screen)
        #[arg(long)]
        desktop: bool,
        /// Skip tasks that need live web access
        #[arg(long)]
        no_web: bool,
    },
    /// Undo the agent's last turn of file edits (checkpoint stack)
    Rewind {
        /// List rewindable checkpoints instead of restoring
        #[arg(long)]
        list: bool,
    },
    /// Inspect Phoenix memory: recall a query, or render the graph to HTML
    Memory {
        /// Render the whole knowledge graph to an HTML file and print its path
        #[arg(long)]
        graph: bool,
        /// Print the whole knowledge graph as JSON {nodes, edges} (for the dashboard)
        #[arg(long = "graph-json")]
        graph_json: bool,
        /// Index the saved-but-unprocessed backlog now (cognify), then report
        #[arg(long)]
        maintain: bool,
        /// Search the memory graph and print the matching chunks
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        query: Vec<String>,
    },
    /// Inspect or maintain the local Phoenix company directory.
    Company {
        #[command(subcommand)]
        action: CompanyAction,
    },
}

#[derive(clap::Subcommand, Debug)]
enum CompanyAction {
    /// Print the secret-free coworker/context mapping as JSON.
    Status,
    /// Give every coworker except Phoenix a new empty canonical conversation.
    /// Previous transcripts remain linked as recoverable history.
    FreshCoworkerContexts {
        /// Required safety acknowledgement for the real company state.
        #[arg(long)]
        confirm: bool,
    },
    /// Associate an existing Phoenix transcript with a project folder so the
    /// tiered project-memory layer can surface its bounded digest.
    AttachPhoenixProjectContext {
        #[arg(long)]
        session: String,
        #[arg(long)]
        workspace: std::path::PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    /// Select one already-linked Phoenix transcript as the canonical company
    /// conversation. Intended for explicit recovery and migration only.
    SetPhoenixCanonical {
        #[arg(long)]
        session: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Promote one verified provider account across the existing Phoenix,
    /// coworker, memory, vision, and browser routes without changing models.
    PromoteAccount {
        #[arg(long)]
        profile: String,
        /// Remove the old ordered backups after promotion. Use this when each
        /// backup was independently verified unhealthy.
        #[arg(long)]
        clear_fallbacks: bool,
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(clap::Subcommand, Debug)]
enum AuthAction {
    /// List stored auth profiles as JSON (summaries — never secrets)
    List,
    /// Store an API key profile; the key is read from STDIN (first line)
    SetKey {
        /// Profile id, e.g. "xai:default" or "search-tavily"
        profile_id: String,
        /// Provider id from the LLM or web catalog, e.g. "xai", "tavily"
        provider: String,
    },
    /// Remove a stored auth profile by id
    Remove { profile_id: String },
    /// Live-probe a profile: real completion through the real factory;
    /// prints a JSON verdict (catches tier-gated OAuth accounts)
    Probe {
        profile_id: String,
        /// Probe this exact model instead of inferring one from stored lanes.
        #[arg(long)]
        model: Option<String>,
        /// Send this exact reasoning effort with the probe.
        #[arg(long)]
        effort: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // Logs go to stderr, not stdout: the interactive CLI renders the agent UI on
    // stdout (spinner uses in-place `\r` redraws), and a tracing line landing on
    // stdout mid-render corrupts the animation and pollutes piped output. stderr
    // keeps logs visible without fighting the UI stream.
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Start {
            scaffold,
            real,
            session,
            new_session,
            librarian_debug,
            yolo,
            task,
        }) => {
            if scaffold && real {
                anyhow::bail!("Select only one execution mode: `--scaffold` or `--real`.");
            }
            // Ensure the unified `~/.phoenix` home exists and absorb any legacy
            // cwd-relative state so memory/sessions persist regardless of where
            // `phoenix` is launched from.
            let phoenix_home =
                config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            maybe_migrate_legacy_state(&phoenix_home)?;
            let session_id = resolve_session_id(session, new_session)?;
            let one_shot = if task.is_empty() {
                read_piped_stdin()?
            } else {
                Some(task.join(" "))
            };
            if let Some(prompt) = one_shot {
                cli::run_oneshot(scaffold, real, session_id, prompt, librarian_debug, yolo).await?;
            } else if std::io::stdin().is_terminal() {
                cli::run_interactive(scaffold, real, session_id, librarian_debug, yolo).await?;
            } else {
                anyhow::bail!("No task provided. Pass a prompt, pipe stdin, or run interactively.");
            }
        }
        Some(Commands::Onboard {
            provider,
            model,
            auth,
        }) => {
            let path = config::run_setup(provider, model, auth).await?;
            let cfg = PhoenixConfig::load_from_path(path.clone())?;
            let factory = providers::ProviderFactory::new();
            let resolved = factory
                .resolve_llm_profile(&cfg.profile.llm)
                .expect("setup wrote an invalid provider profile");

            println!("Wrote config to {}", path.display());
            println!("Default provider: {}", resolved.provider_id);
            println!("Default model: {}", resolved.model_id);
            println!("Auth method: {}", resolved.auth.method);
            println!("Auth source: {}", resolved.auth.source_summary());
            println!("Provider verification: ok");
        }
        Some(Commands::Configure) => {
            config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            config::run_configure().await?;
        }
        Some(Commands::Check {
            config: config_path,
        }) => {
            let cfg = match config_path {
                Some(p) => PhoenixConfig::load_from_path(p)?,
                None => PhoenixConfig::load()?,
            };
            let result = config::run_check(&cfg).await?;
            println!("Provider check succeeded.");
            println!("Provider: {}", result.provider_id);
            println!("Model: {}", result.model_id);
            println!("Endpoint: {}", result.endpoint);
            println!("Auth method: {}", result.auth_method);
            println!("Auth source: {}", result.auth_source);
            println!("Response preview: {}", result.response_preview);
        }
        Some(Commands::Channels { action }) => match action {
            ChannelAction::AcknowledgeTurn { connection, turn_id, snapshot } => cli::channels::acknowledge_turn_checked(&connection, &turn_id, snapshot.as_deref())?,
            ChannelAction::ResolveDelivery { connection, delivery_id, retry, snapshot } => cli::channels::resolve_delivery_checked(&connection, &delivery_id, retry, snapshot.as_deref())?,
            ChannelAction::List { summary } => println!("{}", if summary {cli::channels::list_saved_summary()?} else {cli::channels::list_saved()?}),
            ChannelAction::Save => println!("{}", cli::channels::save_stdin()?),
            ChannelAction::Remove { id } => cli::channels::remove_saved(&id)?,
            ChannelAction::RememberToken { connection } => { cli::channels::remember_token(&connection)?; println!("Bot login saved in the encrypted vault."); }
            ChannelAction::ForgetToken { connection } => { cli::channels::forget_token(&connection)?; println!("Saved bot login removed."); }
            ChannelAction::Run { connection, token_stdin } => cli::channels::run(&connection, token_stdin).await?,
            ChannelAction::Status { connection, summary } => println!("{}", if summary {cli::channels::status_summary(&connection)?} else {cli::channels::status(&connection)?}),
            ChannelAction::ReviewPage { connection, kind, offset, limit, snapshot } => println!("{}", cli::channels::review_page(&connection, &kind, offset, limit, snapshot.as_deref())?),
            ChannelAction::Check { connection, token_stdin } => { cli::channels::check(&connection, token_stdin).await?; println!("Bot sign-in verified."); }
        },
        Some(Commands::Providers { list, json }) => {
            if json {
                println!("{}", cli::headless_config::providers_json()?);
            } else if list {
                let providers = providers::providers_data::all_providers();
                println!("\nAvailable Providers ({} total)\n", providers.len());
                println!("{:<20} {:<30} {:<50}", "ID", "Name", "Base URL");
                println!("{}", "-".repeat(100));
                let mut sorted = providers;
                sorted.sort_by(|a, b| a.id.cmp(b.id));
                for p in sorted {
                    println!("{:<20} {:<30} {:<50}", p.id, p.name, p.base_url);
                }
                println!();
            }
        }
        Some(Commands::Auth { action }) => match action {
            AuthAction::List => println!("{}", cli::headless_config::auth_list_json()?),
            AuthAction::SetKey {
                profile_id,
                provider,
            } => cli::headless_config::auth_set_key(&profile_id, &provider)?,
            AuthAction::Remove { profile_id } => cli::headless_config::auth_remove(&profile_id)?,
            AuthAction::Probe {
                profile_id,
                model,
                effort,
            } => {
                println!(
                    "{}",
                    cli::headless_config::auth_probe_json(
                        &profile_id,
                        model.as_deref(),
                        effort.as_deref(),
                    )
                    .await?
                )
            }
        },
        Some(Commands::Login {
            provider,
            method,
            profile,
        }) => {
            // The OAuth flows use blocking HTTP + a blocking callback server;
            // running them on the async runtime panics ("Cannot drop a
            // runtime…"). spawn_blocking gives them a real blocking thread —
            // same reason configure wraps auth flows in block_in_place.
            tokio::task::spawn_blocking(move || {
                config::setup::headless_provider_login(&provider, &method, profile.as_deref())
            })
            .await??;
        }
        Some(Commands::Stop) => {
            cli::daemon::stop_daemon().await?;
        }
        Some(Commands::Bench {
            label,
            desktop,
            no_web,
        }) => {
            cli::bench::run_bench(label, desktop, no_web).await?;
        }
        Some(Commands::Rewind { list }) => {
            let phoenix_home =
                config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            let store = crate::tools::checkpoint::CheckpointStore::new(&phoenix_home);
            let workspace = std::env::current_dir().context("cwd")?;
            if list {
                let scopes = store.scopes_result().context("checkpoint listing failed")?;
                if scopes.is_empty() {
                    println!("No checkpoints — nothing the agent edited is rewindable yet.");
                } else {
                    println!("{:<24} {:<40} files", "checkpoint", "session");
                    for scope in scopes {
                        println!("{:<24} {:<40} {}", scope.scope, scope.session, scope.files);
                    }
                }
            } else {
                match store.rewind_latest(&workspace)? {
                    None => println!("No checkpoints to rewind."),
                    Some(report) => {
                        for path in &report.restored {
                            println!("restored {path}");
                        }
                        for path in &report.deleted {
                            println!("deleted  {path} (was created by the agent)");
                        }
                        println!(
                            "rewound checkpoint {} ({} restored, {} deleted)",
                            report.scope,
                            report.restored.len(),
                            report.deleted.len()
                        );
                    }
                }
            }
        }
        Some(Commands::Memory {
            graph,
            graph_json,
            maintain,
            query,
        }) => {
            config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            if maintain {
                let receipt = librarian::memory::cognify_backlog_now().await?;
                println!("backlog cognify: {receipt}");
            } else {
                cli::commands::run_memory(graph, graph_json, query.join(" ")).await?;
            }
        }
        Some(Commands::Company { action }) => {
            config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            let store = runtime::company::global()?;
            match action {
                CompanyAction::Status => {
                    let snapshot = store.directory_snapshot()?;
                    let rows = snapshot
                        .agents
                        .into_iter()
                        .map(|agent| {
                            serde_json::json!({
                                "agent_id": agent.profile.agent_id,
                                "display_name": agent.profile.display_name,
                                "lifecycle": agent.profile.lifecycle,
                                "canonical_session_id": agent.profile.canonical_session_id,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                }
                CompanyAction::FreshCoworkerContexts { confirm } => {
                    anyhow::ensure!(
                        confirm,
                        "refusing to rotate real coworker conversations without --confirm"
                    );
                    let snapshot = store.directory_snapshot()?;
                    let mut receipts = Vec::new();
                    for agent in snapshot.agents {
                        if agent.profile.agent_id == "phoenix" {
                            continue;
                        }
                        let (previous, canonical) = store
                            .rotate_agent_canonical_session("user", &agent.profile.agent_id)?;
                        receipts.push(serde_json::json!({
                            "agent_id": agent.profile.agent_id,
                            "display_name": agent.profile.display_name,
                            "previous_session_id": previous,
                            "canonical_session_id": canonical,
                        }));
                    }
                    println!("{}", serde_json::to_string_pretty(&receipts)?);
                }
                CompanyAction::AttachPhoenixProjectContext {
                    session,
                    workspace,
                    confirm,
                } => {
                    anyhow::ensure!(
                        confirm,
                        "refusing to alter a real Phoenix transcript without --confirm"
                    );
                    session::SessionStore::validate_session_id(&session)?;
                    let workspace = workspace
                        .canonicalize()
                        .with_context(|| format!("invalid workspace {}", workspace.display()))?;
                    anyhow::ensure!(workspace.is_dir(), "workspace is not a directory");
                    let snapshot = store.directory_snapshot()?;
                    anyhow::ensure!(
                        snapshot.conversation_sources.iter().any(|source| {
                            source.owner_kind == "agent"
                                && source.owner_id == "phoenix"
                                && source.session_id == session
                        }),
                        "session `{session}` is not linked to Phoenix"
                    );
                    let root = config::phoenix_home().join("sessions");
                    let mut sessions = session::SessionStore::new(root);
                    let mut record =
                        session::SessionStore::read_one_from_disk(sessions.root(), &session)?
                            .with_context(|| format!("Phoenix session `{session}` is missing"))?;
                    record.workspace = Some(workspace.display().to_string());
                    sessions.upsert(record);
                    sessions.save_one(&session)?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "session_id": session,
                            "workspace": workspace,
                            "owner": "phoenix",
                        })
                    );
                }
                CompanyAction::SetPhoenixCanonical { session, confirm } => {
                    anyhow::ensure!(
                        confirm,
                        "refusing to change Phoenix's canonical conversation without --confirm"
                    );
                    session::SessionStore::validate_session_id(&session)?;
                    let snapshot = store.directory_snapshot()?;
                    anyhow::ensure!(
                        snapshot.conversation_sources.iter().any(|source| {
                            source.owner_kind == "agent"
                                && source.owner_id == "phoenix"
                                && source.session_id == session
                        }),
                        "session `{session}` is not linked to Phoenix"
                    );
                    store.apply_directory_change(
                        "user",
                        format!("phoenix-canonical-recovery:{session}"),
                        runtime::company_directory::DirectoryChange::ConversationSourceLinked {
                            session_id: session.clone(),
                            owner_kind: "agent".to_string(),
                            owner_id: "phoenix".to_string(),
                            source_kind: "production_canonical".to_string(),
                            canonical: true,
                        },
                    )?;
                    println!(
                        "{}",
                        serde_json::json!({"agent_id":"phoenix","canonical_session_id":session})
                    );
                }
                CompanyAction::PromoteAccount {
                    profile,
                    clear_fallbacks,
                    confirm,
                } => {
                    anyhow::ensure!(
                        confirm,
                        "refusing to alter production model routes without --confirm"
                    );
                    let initial =
                        match settings::execute(settings::SettingsCommand::ModelsSnapshot)? {
                            settings::SettingsReply::Models { snapshot } => snapshot,
                            _ => unreachable!(),
                        };
                    let account = initial
                        .accounts
                        .iter()
                        .find(|account| account.profile_id == profile)
                        .with_context(|| format!("unknown provider account `{profile}`"))?;
                    let provider_id = account.provider_id.clone();
                    let mut promoted = Vec::new();
                    let target_lanes = ["phoenix", "specialist", "librarian", "vision", "browser"];
                    for lane_name in target_lanes {
                        let snapshot =
                            match settings::execute(settings::SettingsCommand::ModelsSnapshot)? {
                                settings::SettingsReply::Models { snapshot } => snapshot,
                                _ => unreachable!(),
                            };
                        let Some(lane) = snapshot.lanes.iter().find(|lane| lane.lane == lane_name)
                        else {
                            continue;
                        };
                        anyhow::ensure!(
                            lane.provider_id == provider_id,
                            "route `{lane_name}` uses provider `{}`; refusing to change its model while promoting `{profile}`",
                            lane.provider_id
                        );
                        settings::execute(settings::SettingsCommand::SetModelLane {
                            lane: lane_name.to_string(),
                            provider_id: provider_id.clone(),
                            model: lane.model.clone(),
                            reasoning_effort: lane.reasoning_effort.clone(),
                            auth_profile_id: Some(profile.clone()),
                            expected_config_revision: Some(snapshot.config_revision),
                        })?;
                        promoted.push(lane_name);
                    }
                    if clear_fallbacks {
                        for lane in ["orchestrator", "specialist", "browser"] {
                            let snapshot =
                                match settings::execute(settings::SettingsCommand::ModelsSnapshot)?
                                {
                                    settings::SettingsReply::Models { snapshot } => snapshot,
                                    _ => unreachable!(),
                                };
                            settings::execute(settings::SettingsCommand::SetFallbackChain {
                                lane: lane.to_string(),
                                profile_ids: Vec::new(),
                                expected_config_revision: Some(snapshot.config_revision),
                            })?;
                        }
                    }
                    println!(
                        "{}",
                        serde_json::json!({
                            "profile_id": profile,
                            "provider_id": provider_id,
                            "promoted_lanes": promoted,
                            "fallbacks_cleared": clear_fallbacks,
                        })
                    );
                }
            }
        }
        Some(Commands::Restart) => {
            // Never overlap two gateways. A verified stop can fail while the
            // old process is still draining browser/Cognee state; starting a
            // replacement anyway would race the WS port and Chrome profiles.
            cli::daemon::stop_daemon().await?;
            let phoenix_home =
                config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            maybe_migrate_legacy_state(&phoenix_home)?;
            cli::daemon::run_daemon().await?;
        }
        None => {
            // Bare `phoenix` boots the gateway DAEMON — the long-running
            // process every session routes through. Open sessions with
            // `phoenix start` in another terminal; the CLI refuses to run a
            // turn without a live gateway.
            let phoenix_home =
                config::ensure_phoenix_home().context("failed to prepare Phoenix home")?;
            maybe_migrate_legacy_state(&phoenix_home)?;
            cli::daemon::run_daemon().await?;
        }
    }

    Ok(())
}

const MAX_PIPED_PROMPT_BYTES: u64 = 16 * 1024 * 1024;

fn read_piped_stdin() -> Result<Option<String>> {
    if std::io::stdin().is_terminal() {
        return Ok(None);
    }
    read_piped_prompt(std::io::stdin())
}

fn read_piped_prompt(reader: impl std::io::Read) -> Result<Option<String>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let mut limited = reader.take(MAX_PIPED_PROMPT_BYTES + 1);
    limited
        .read_to_end(&mut bytes)
        .context("failed to read piped task")?;
    if bytes.len() as u64 > MAX_PIPED_PROMPT_BYTES {
        anyhow::bail!(
            "piped task exceeds the {} MiB input limit",
            MAX_PIPED_PROMPT_BYTES / (1024 * 1024)
        );
    }
    let buf = String::from_utf8(bytes).context("piped task is not valid UTF-8")?;
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

/// Where the most recent session id is remembered between launches.
fn last_session_path() -> std::path::PathBuf {
    config::phoenix_home().join("last_session")
}

/// `phoenix start` continues where you left off: no flag → the last session
/// used; `--new-session` → fresh id; `--session X` → exactly X. Whatever wins
/// is persisted as the new "last".
fn resolve_session_id(session: Option<String>, new_session: bool) -> Result<String> {
    let id = if new_session {
        format!("main-{}", uuid::Uuid::new_v4())
    } else if let Some(explicit) = session {
        explicit
    } else {
        config::private_io::read_private_file(&last_session_path())?
            .map(String::from_utf8)
            .transpose()
            .context("last_session is not valid UTF-8")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main-session".to_string())
    };
    crate::session::SessionStore::validate_session_id(&id)
        .with_context(|| format!("invalid session id `{id}`"))?;
    let path = last_session_path();
    config::private_io::atomic_write_private(&path, id.as_bytes())
        .with_context(|| format!("failed to persist the last session in {}", path.display()))?;
    Ok(id)
}

fn maybe_migrate_legacy_state(phoenix_home: &std::path::Path) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to resolve the current directory")?;
    migrate_legacy_state_from(phoenix_home, &cwd)
}

// Version the commit record. Older builds wrote both of the legacy values
// below even after swallowed/partial migration failures, so they must trigger
// one safe copy-if-missing audit before being upgraded.
const LEGACY_MIGRATION_COMPLETE: &[u8] = b"done-v2\n";
const LEGACY_MIGRATION_OLD_DONE: &[u8] = b"done\n";
const LEGACY_MIGRATION_OLD_POPULATED: &[u8] = b"already-populated\n";

enum MigrationMarkerState {
    Missing,
    Complete,
    Legacy(Vec<u8>),
}

fn migration_marker_state(marker: &std::path::Path) -> Result<MigrationMarkerState> {
    match config::private_io::read_private_file(marker)? {
        Some(contents) if contents == LEGACY_MIGRATION_COMPLETE => {
            Ok(MigrationMarkerState::Complete)
        }
        Some(contents)
            if contents == LEGACY_MIGRATION_OLD_DONE
                || contents == LEGACY_MIGRATION_OLD_POPULATED =>
        {
            Ok(MigrationMarkerState::Legacy(contents))
        }
        Some(_) => anyhow::bail!(
            "legacy migration marker {} is corrupt or incomplete",
            marker.display()
        ),
        None => Ok(MigrationMarkerState::Missing),
    }
}

fn migrate_legacy_state_from(
    phoenix_home: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<()> {
    let configured_home = config::phoenix_home();
    if configured_home != phoenix_home {
        anyhow::bail!(
            "refusing legacy migration with mismatched Phoenix homes: {} != {}",
            phoenix_home.display(),
            configured_home.display()
        );
    }
    let marker = phoenix_home.join(".legacy-state-migrated");
    let marker_state = migration_marker_state(&marker)?;
    if matches!(&marker_state, MigrationMarkerState::Complete) {
        return Ok(());
    }

    // Always run the copy-if-missing walk when the commit marker is absent.
    // Existing unified state is preserved by the migration primitive. This is
    // also what makes a partial disk/I/O failure retryable: some copied files
    // must never be mistaken for proof that the entire migration completed.
    let report = config::migrate_legacy_state(workspace).with_context(|| {
        format!(
            "failed to migrate legacy Phoenix state from {}",
            workspace.display()
        )
    })?;
    if report.files_copied > 0 {
        eprintln!(
            "Phoenix: migrated {} legacy state file(s) into {}",
            report.files_copied,
            phoenix_home.display()
        );
    }
    // This marker is the commit record for the migration. Never publish it
    // after a partial/failed copy: the next launch must be allowed to retry.
    match marker_state {
        MigrationMarkerState::Missing => {
            let created = config::private_io::atomic_write_private_if_missing(
                &marker,
                LEGACY_MIGRATION_COMPLETE,
            )
            .with_context(|| format!("failed to write {}", marker.display()))?;
            if !created
                && !matches!(
                    migration_marker_state(&marker)?,
                    MigrationMarkerState::Complete
                )
            {
                anyhow::bail!(
                    "legacy migration marker {} changed before completion",
                    marker.display()
                );
            }
        }
        MigrationMarkerState::Legacy(old) => {
            if let Err(error) = config::private_io::compare_and_swap_private(
                &marker,
                Some(&old),
                LEGACY_MIGRATION_COMPLETE,
            ) {
                if !matches!(
                    migration_marker_state(&marker)?,
                    MigrationMarkerState::Complete
                ) {
                    return Err(error).with_context(|| {
                        format!("failed to upgrade migration marker {}", marker.display())
                    });
                }
            }
        }
        MigrationMarkerState::Complete => unreachable!("handled before migration"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_defaults_to_provider_backed() {
        let cli = Cli::try_parse_from(["phoenix", "start"]).unwrap();
        match cli.command {
            Some(Commands::Start {
                scaffold,
                real,
                task,
                ..
            }) => {
                assert!(!scaffold);
                assert!(!real);
                assert!(task.is_empty());
            }
            _ => panic!("expected start command"),
        }
    }

    #[test]
    fn start_scaffold_flag_is_explicit() {
        let cli = Cli::try_parse_from(["phoenix", "start", "--scaffold"]).unwrap();
        match cli.command {
            Some(Commands::Start {
                scaffold,
                real,
                task,
                ..
            }) => {
                assert!(scaffold);
                assert!(!real);
                assert!(task.is_empty());
            }
            _ => panic!("expected start command"),
        }
    }

    #[test]
    fn piped_prompt_is_trimmed_and_bounded() {
        use std::io::Read;

        assert_eq!(
            read_piped_prompt(std::io::Cursor::new(b"  build it\n"))
                .unwrap()
                .as_deref(),
            Some("build it")
        );
        assert!(
            read_piped_prompt(std::io::repeat(b'x').take(MAX_PIPED_PROMPT_BYTES + 1))
                .unwrap_err()
                .to_string()
                .contains("input limit")
        );
        assert!(read_piped_prompt(std::io::Cursor::new(vec![0xff]))
            .unwrap_err()
            .to_string()
            .contains("UTF-8"));
    }

    #[test]
    fn failed_partial_migration_does_not_publish_marker_and_retry_completes() {
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("phoenix-home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(&home);
        config::ensure_phoenix_home().unwrap();

        let workspace = tempfile::tempdir().unwrap();
        let sessions = workspace.path().join(".phoenix/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("first.json"), b"first").unwrap();
        let memory = workspace.path().join("memory/WARM");
        std::fs::create_dir_all(&memory).unwrap();
        let blocked = memory.join("too-large.md");
        std::fs::File::create(&blocked)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();

        assert!(migrate_legacy_state_from(&home, workspace.path()).is_err());
        let marker = home.join(".legacy-state-migrated");
        assert!(!marker.exists(), "a partial copy must remain retryable");
        assert_eq!(
            std::fs::read(home.join("sessions/first.json")).unwrap(),
            b"first"
        );

        std::fs::write(&blocked, b"recovered").unwrap();
        migrate_legacy_state_from(&home, workspace.path()).unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), LEGACY_MIGRATION_COMPLETE);
        assert_eq!(
            std::fs::read(home.join("memory/WARM/too-large.md")).unwrap(),
            b"recovered"
        );
    }

    #[test]
    fn corrupt_completion_marker_never_suppresses_migration() {
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("phoenix-home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(&home);
        config::ensure_phoenix_home().unwrap();
        config::private_io::atomic_write_private(
            &home.join(".legacy-state-migrated"),
            b"incomplete",
        )
        .unwrap();

        let workspace = tempfile::tempdir().unwrap();
        let sessions = workspace.path().join(".phoenix/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("must-not-be-skipped.json"), b"legacy").unwrap();

        assert!(migrate_legacy_state_from(&home, workspace.path()).is_err());
        assert!(!home.join("sessions/must-not-be-skipped.json").exists());
        assert_eq!(
            std::fs::read(home.join(".legacy-state-migrated")).unwrap(),
            b"incomplete"
        );
    }

    #[test]
    fn legacy_completion_marker_is_audited_and_upgraded() {
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("phoenix-home");
        let _guard = crate::config::test_env::PhoenixHomeGuard::set(&home);
        config::ensure_phoenix_home().unwrap();
        config::private_io::atomic_write_private(
            &home.join(".legacy-state-migrated"),
            LEGACY_MIGRATION_OLD_POPULATED,
        )
        .unwrap();

        let workspace = tempfile::tempdir().unwrap();
        let sessions = workspace.path().join(".phoenix/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("recovered.json"), b"legacy").unwrap();

        migrate_legacy_state_from(&home, workspace.path()).unwrap();
        assert_eq!(
            std::fs::read(home.join("sessions/recovered.json")).unwrap(),
            b"legacy"
        );
        assert_eq!(
            std::fs::read(home.join(".legacy-state-migrated")).unwrap(),
            LEGACY_MIGRATION_COMPLETE
        );
    }
}
