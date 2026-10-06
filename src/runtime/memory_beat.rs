//! The lib beat (plan 017) — the librarian runs at EVERY agent completion.
//!
//! "The model should remember everything." Per-turn tool saves and idle
//! digests exist, but both are best-effort and lumpy; the beat closes the gap
//! deterministically: the moment an agent completes (an orchestrator final,
//! a specialist's background return), a detached lib pass reads the exchange
//! and decides — is there a durable, reusable learning here? If yes it saves
//! the note itself (the agent is already gone; a reminder would reach no one)
//! and kicks the incremental indexer so the memory is *searchable* within
//! minutes, not at the next 12-hour tick.
//!
//! Cheap by construction: the lib lane model, one bounded call, a global
//! debounce, and every failure path a silent no-op — the beat may never slow
//! or block the mesh.

use anyhow::{Context, Result};

use crate::providers::{ChatMessage, CompletionRequest};

const BEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// Minimum spacing between beats — a burst of chain returns must not become
/// a burst of model calls (the indexer batches whatever the beats saved).
const DEBOUNCE_SECS: i64 = 30;
/// Exchanges shorter than this carry nothing durable ("done", "ok ✓").
const MIN_FINAL_CHARS: usize = 160;

/// Fire the beat for one completed agent exchange. Detached and debounced;
/// returns immediately.
pub fn on_completion(agent: &str, mission: &str, final_text: &str) {
    if cfg!(test) {
        return;
    }
    if final_text.trim().chars().count() < MIN_FINAL_CHARS {
        return;
    }
    if !debounce_ok() {
        return;
    }
    let agent = agent.to_string();
    let mission = mission.to_string();
    let final_text = final_text.to_string();
    tokio::spawn(async move {
        match beat_call(&agent, &mission, &final_text).await {
            Ok(Some((scopes, note))) => {
                // The indexer routes: TEAM, the agent's own memory, or a
                // named set of agents (user design 2026-07-18 — "indexer
                // should pick which memory goes to all, or one or some").
                // Every copy carries WHO learned it.
                let learned_by = crate::runtime::delegation::agent_display_name(
                    crate::runtime::postbox::base_agent(&agent),
                );
                let mut stored_scopes = 0usize;
                for scope in &scopes {
                    let stamped = crate::librarian::memory::stamp(&note, &learned_by, scope);
                    let outcome = crate::librarian::memory::remember_scoped(&stamped, scope).await;
                    if outcome.is_stored() {
                        stored_scopes += 1;
                    } else {
                        tracing::warn!(
                            "lib beat: memory durability not confirmed for scope {} ({outcome}); retry is still needed",
                            scope.dataset()
                        );
                    }
                }
                if stored_scopes == 0 {
                    tracing::warn!(
                        "lib beat: no memory scope was confirmed stored for {agent}; skipping cognify and not claiming the note was remembered"
                    );
                    return;
                }
                tracing::info!(
                    "lib beat: remembered a note from {agent}'s completion into {} scope(s)",
                    stored_scopes
                );
                // Index NOW (serializes against itself; quota breaker inside)
                // so the note is recallable this session, not tomorrow.
                if let Err(error) = crate::librarian::memory::cognify_backlog().await {
                    tracing::debug!("lib beat: cognify deferred: {error:#}");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::debug!("lib beat unavailable, skipping: {error:#}"),
        }
    });
}

fn debounce_ok() -> bool {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = chrono::Utc::now().timestamp();
    let last = LAST.load(Ordering::Relaxed);
    if now - last < DEBOUNCE_SECS {
        return false;
    }
    LAST.store(now, Ordering::Relaxed);
    true
}

/// One lib-lane call: SKIP or SAVE with a SCOPE + the note. The prompt is
/// biased to SKIP — a junk note pollutes recall forever, a missed note costs
/// one lesson. The indexer also ROUTES the note: TEAM (everyone recalls it),
/// MINE (only the completing agent), or a named list of agent roles.
async fn beat_call(agent: &str, mission: &str, final_text: &str) -> Result<Option<BeatSave>> {
    let (provider, model) = librarian_lane()?;
    let mut request = CompletionRequest::new(
        &model,
        vec![
            ChatMessage::system(
                "You are the librarian of an AI agent team, reviewing ONE completed \
                 exchange for durable memory. Save ONLY a learning that will be true and \
                 useful in FUTURE sessions: a discovered fact about the user's world \
                 (accounts, infrastructure, preferences), a hard-won technique or \
                 root-cause, a reusable route ('X is best done via Y'). Do NOT save: \
                 narrations of what happened, task status, anything obvious, anything \
                 the workspace files already record. When in doubt, SKIP — a junk note \
                 pollutes recall forever. You also ROUTE what you save: TEAM = facts \
                 about the user's world, preferences, infrastructure — things EVERY \
                 agent benefits from; MINE = craft specific to this one agent's lane \
                 (a browser trick belongs to the browser agent, not the coder); or a \
                 comma list of agent roles when exactly those lanes need it. Reply \
                 with exactly one line: `SKIP`, `SAVE TEAM: <note>`, `SAVE MINE: \
                 <note>`, or `SAVE <role[,role]>: <note>` — the note 1-3 sentences, \
                 self-contained, present tense.",
            ),
            ChatMessage::user(format!(
                "Agent: {agent}\nMission (truncated):\n{}\n\nCompletion (truncated):\n{}",
                cap(mission, 1200),
                cap(final_text, 4000),
            )),
        ],
    );
    request.max_tokens = Some(160);
    request.temperature = Some(0.0);
    let response = tokio::time::timeout(BEAT_TIMEOUT, provider.complete(request))
        .await
        .context("lib beat timed out")??;
    Ok(parse_beat(&response.content, agent))
}

type BeatSave = (Vec<crate::librarian::memory::MemoryScope>, String);

/// Parse `SKIP` / `SAVE TEAM: …` / `SAVE MINE: …` / `SAVE coder,browser: …`.
/// Unrecognized scope words default to MINE — the safe tier (a misrouted
/// private note is invisible noise; a misrouted team note pollutes everyone).
fn parse_beat(raw: &str, completing_agent: &str) -> Option<BeatSave> {
    use crate::librarian::memory::MemoryScope;
    let text = raw.trim();
    let idx = text.find("SAVE")?;
    if text[..idx].to_ascii_lowercase().contains("skip") {
        return None;
    }
    let rest = text[idx + "SAVE".len()..].trim_start();
    let own_role = crate::runtime::postbox::base_agent(completing_agent)
        .trim()
        .to_ascii_lowercase();
    let own = || MemoryScope::agent(&own_role);
    // Split "<scope words>: <note>" — legacy `SAVE: note` has an empty scope.
    let (scope_part, note) = match rest.split_once(':') {
        Some((s, n)) => (s.trim(), n.trim()),
        None => ("", rest.trim_start_matches(['-', ' ']).trim()),
    };
    if note.is_empty() {
        return None;
    }
    let scopes: Vec<MemoryScope> = match scope_part.to_ascii_lowercase().as_str() {
        "team" => vec![MemoryScope::Team],
        "" | "mine" => vec![own()],
        list => {
            let roles: Vec<MemoryScope> = list
                .split(',')
                .map(|r| r.trim())
                .filter(|r| {
                    !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                .map(MemoryScope::agent)
                .collect();
            if roles.is_empty() {
                vec![own()]
            } else {
                roles
            }
        }
    };
    Some((scopes, cap(note, 700)))
}

/// The lib lane: librarian provider + fallback chain + librarian model —
/// the same resolution the session digest uses.
fn librarian_lane() -> Result<(std::sync::Arc<dyn crate::providers::LLMProvider>, String)> {
    let config = crate::config::PhoenixConfig::load()?;
    let llm = &config.profile.llm;
    let factory = crate::providers::ProviderFactory::new();
    // A dead primary must not skip the account-fallback chain below — build
    // it as a `Result` and let `lane_with_fallback_chain` treat a missing
    // first link as just that.
    let provider = match llm
        .librarian_provider
        .as_deref()
        .filter(|id| !id.trim().is_empty() && *id != llm.provider)
    {
        Some(id) => factory.build_role_provider(llm, id),
        None => factory.build_llm_provider(llm),
    };
    let librarian_pid = llm
        .librarian_provider
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(&llm.provider)
        .to_string();
    let provider = factory.lane_with_fallback_chain(
        provider,
        llm,
        "librarian",
        &librarian_pid,
        &llm.fallback.librarian,
    )?;
    Ok((provider, llm.librarian()))
}

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::librarian::memory::MemoryScope;

    #[test]
    fn parse_beat_extracts_save_notes_and_skips() {
        assert_eq!(parse_beat("SKIP", "coder"), None);
        assert_eq!(parse_beat("skip — nothing durable", "coder"), None);
        // Legacy bare SAVE routes to the completing agent's OWN memory.
        assert_eq!(
            parse_beat(
                "SAVE: the user's staging DB runs Postgres 17 on port 5433",
                "coder"
            ),
            Some((
                vec![MemoryScope::agent("coder")],
                "the user's staging DB runs Postgres 17 on port 5433".to_string()
            ))
        );
        assert_eq!(parse_beat("SAVE:", "coder"), None);
        assert_eq!(parse_beat("no idea", "coder"), None);
        // A model that hedges both ways is a skip.
        assert_eq!(
            parse_beat("SKIP (but you could SAVE the port)", "coder"),
            None
        );
    }

    #[test]
    fn parse_beat_routes_scopes() {
        // TEAM routing.
        assert_eq!(
            parse_beat("SAVE TEAM: the user's yahoo address is X", "browser"),
            Some((
                vec![MemoryScope::Team],
                "the user's yahoo address is X".to_string()
            ))
        );
        // MINE = the completing agent (instance suffix stripped by base_agent).
        assert_eq!(
            parse_beat(
                "SAVE MINE: reddit search needs old.reddit for scraping",
                "browser#2"
            ),
            Some((
                vec![MemoryScope::agent("browser")],
                "reddit search needs old.reddit for scraping".to_string()
            ))
        );
        // Named roles fan out to each listed agent's memory.
        assert_eq!(
            parse_beat(
                "SAVE coder,tester: the suite needs --bin phoenix too",
                "planner"
            ),
            Some((
                vec![MemoryScope::agent("coder"), MemoryScope::agent("tester")],
                "the suite needs --bin phoenix too".to_string()
            ))
        );
        // Garbage scope words fall back to MINE, never team.
        assert_eq!(
            parse_beat("SAVE whatever!!: some note", "coder"),
            Some((vec![MemoryScope::agent("coder")], "some note".to_string()))
        );
    }
}
