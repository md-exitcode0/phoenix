//! Prompt assembly for Phoenix runtime turns.

use std::path::Path;

use crate::librarian::{render_trust_receipt, LoadedMemories};
use crate::providers::{ChatMessage, CompletionRequest};
use crate::runtime::shared_contract::{runtime_context_block, shared_phoenix_contract};
use crate::runtime::{AgentSpec, AgentTargetSpec, TaskEnvelope};
use crate::session::{Message, Session};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptAssembly {
    pub system_prompt: String,
    pub user_prompt: String,
    /// Volatile state (the live todo map) that changes between rounds. It is
    /// sent as the LAST message of each request, never inside `user_prompt`:
    /// anything that changes above the transcript re-bills the whole
    /// transcript because the provider's prompt cache is prefix-based.
    #[serde(default)]
    pub tail: String,
}

fn durable_todo_block(session_id: &str) -> String {
    let Ok((todos, updated_at)) = crate::tools::todo_snapshot_with_update_time(session_id) else {
        return String::new();
    };
    if todos.is_empty() {
        return String::new();
    }
    let rows = todos
        .iter()
        .map(|item| {
            format!(
                "- [{}] {}",
                if item.completed { "x" } else { " " },
                item.task
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Coworker conversations are endless, so this map can outlive the request
    // that wrote it. "Always continue it" made an abandoned test's steps part
    // of every later, unrelated request.
    let written = chrono::DateTime::parse_from_rfc3339(&updated_at)
        .map(|time| format!(" (last written {})", time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")))
        .unwrap_or_default();
    format!(
        "=== CURRENT DURABLE TODO MAP{written} ===\n\
         {rows}\n\
         If this map belongs to the current request or its continuation, preserve completed evidence and every unfinished item: update this same map as evidence arrives, do not replace it with synonymous planning or reset completed counts, and merge genuinely new work into it. If it was written for an earlier, different request, it is not your current job: leave those items out of the list you write for this request and do not work on them unless the current request asks to resume that work. If work is executable, perform the first unfinished action in this response rather than spending another response re-planning.\n\n"
    )
}

impl PromptAssembly {
    pub fn to_completion_request(
        &self,
        model: &str,
        session_id: Option<String>,
    ) -> CompletionRequest {
        let mut request = CompletionRequest::new(
            model.to_string(),
            vec![
                ChatMessage::system(self.system_prompt.clone()),
                ChatMessage::user(self.user_prompt.clone()),
            ],
        );
        request.session_id = session_id;
        request
    }

    /// Append the volatile tail after everything else in the request.
    pub fn append_tail(&self, request: &mut CompletionRequest) {
        if !self.tail.trim().is_empty() {
            request.messages.push(ChatMessage::system(self.tail.clone()));
        }
    }
}

pub fn assemble_prompt(
    spec: &AgentSpec,
    task: &TaskEnvelope,
    loaded: &LoadedMemories,
    session: &Session,
) -> PromptAssembly {
    assemble_prompt_with_context(spec, task, loaded, session, None, None, None)
}

/// Pinned reference material, re-materialized FRESH from its source on every
/// assembly (embedded design libraries are instant; skills read from disk).
/// This is the "compaction gitignore" for skills: the transcript copies of
/// these loads may be pruned/aged/folded freely — the content re-enters the
/// prompt from here, at the system end, every round. Budget spends on the
/// Taste is a reserved primary contract and never competes on recency with
/// supplements. Its full rendered size is charged first against one bounded
/// design-reference budget; only the remainder is available to supplements.
/// That keeps Taste mandatory without allowing Taste + a huge supplement to
/// quietly add 200K+ characters to every provider round.
// A design supplement is optional guidance, not a second system prompt. Keep
// the entire envelope small enough that the mission and current product still
// dominate attention. Oversized source handbooks remain available on demand,
// but are never replayed into every provider turn.
const PINNED_REFS_MAX_CHARS: usize = 40_000;

fn rendered_taste_reference(session: &Session) -> Option<String> {
    session
        .pinned_refs
        .iter()
        .any(|pin| {
            pin.tool == "design_reference"
                && crate::tools::design_refs::canonical_path(&pin.key)
                    == crate::runtime::design_contract::TASTE_REFERENCE_PATH
        })
        .then(|| {
            crate::tools::design_refs::execute(crate::tools::design_refs::DesignReferenceInput {
                path: Some(crate::runtime::design_contract::TASTE_REFERENCE_PATH.to_string()),
            })
            .ok()
            .map(|output| output.content)
        })
        .flatten()
}

/// True only when the session has a successful Taste pin *and* the current
/// embedded source can actually be materialized into the system prompt.
/// Runtime admission uses this same predicate as prompt assembly, so a stale
/// registry entry can never masquerade as an applied design contract.
pub(crate) fn taste_reference_is_rendered(session: &Session) -> bool {
    rendered_taste_reference(session).is_some()
}

fn pinned_refs_block(
    session: &Session,
    workspace_root: Option<&Path>,
    render_visual_references: bool,
    mission: &str,
) -> String {
    if session.pinned_refs.is_empty() {
        return String::new();
    }
    let reserved_taste = render_visual_references
        .then(|| rendered_taste_reference(session))
        .flatten();
    let mut total = reserved_taste
        .as_ref()
        .map(|content| content.chars().count())
        .unwrap_or(0);
    let mut kept: Vec<String> = Vec::new();
    let mut evicted: Vec<String> = Vec::new();
    let mut visual_supplement_kept = false;
    let matched_skill =
        workspace_root.and_then(|root| crate::tools::skills::matching_skill_name(root, mission));
    for pin in session.pinned_refs.iter().rev() {
        if pin.tool == "skill" && matched_skill.as_deref() != Some(pin.key.as_str()) {
            continue;
        }
        if !render_visual_references
            && (pin.tool == "design_reference"
                || (pin.tool == "skill"
                    && crate::tools::skills::named_skill_is_visual_build(workspace_root, &pin.key)))
        {
            continue;
        }
        if pin.tool == "design_reference"
            && crate::tools::design_refs::canonical_path(&pin.key)
                == crate::runtime::design_contract::TASTE_REFERENCE_PATH
        {
            continue;
        }
        let is_visual_supplement = render_visual_references
            && (pin.tool == "design_reference"
                || (pin.tool == "skill"
                    && crate::tools::skills::named_skill_is_visual_build(
                        workspace_root,
                        &pin.key,
                    )));
        // Keep one task-specific visual supplement at most.  Pinned references
        // survive across an endless coworker session, so replaying every old
        // design playbook made later prompts grow into tens of thousands of
        // conflicting tokens.  Newest wins; Taste is reserved separately.
        if is_visual_supplement && visual_supplement_kept {
            evicted.push(format!("{} `{}`", pin.tool, pin.key));
            continue;
        }
        let content = match pin.tool.as_str() {
            "design_reference" => crate::tools::design_refs::execute(
                crate::tools::design_refs::DesignReferenceInput {
                    path: Some(pin.key.clone()),
                },
            )
            .ok()
            .map(|o| o.content),
            "skill" => crate::tools::skills::skill_manifest_content(workspace_root, &pin.key),
            _ => None,
        };
        // Unresolvable (uninstalled skill, renamed library): drop silently —
        // the agent re-loading it will either succeed (re-pins) or see the
        // tool's own error.
        let Some(content) = content else { continue };
        let chars = content.chars().count();
        if total + chars > PINNED_REFS_MAX_CHARS {
            evicted.push(format!("{} `{}`", pin.tool, pin.key));
            continue;
        }
        total += chars;
        if is_visual_supplement {
            visual_supplement_kept = true;
        }
        kept.push(format!(
            "--- {} `{}` (pinned) ---\n{content}",
            pin.tool, pin.key
        ));
    }
    if kept.is_empty() && reserved_taste.is_none() {
        return String::new();
    }
    kept.reverse();
    let evict_note = if evicted.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n(Evicted from the pin budget — newest loads win: {}. Re-load one to bring it back.)",
            evicted.join(", ")
        )
    };
    let taste_block = reserved_taste.map_or_else(String::new, |content| {
        format!(
            "\n\n=== PRIMARY VISUAL-DESIGN CONTRACT (reserved before supplements) ===\n\
             This exact Taste contract is IN FORCE before every visual-design action.\n\n\
             --- design_reference `taste/SKILL.md` (reserved, pinned) ---\n{content}"
        )
    });
    let supplement_block = if kept.is_empty() && evict_note.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n=== PINNED SUPPLEMENTARY REFERENCE MATERIAL ===\n\
             These supplements are IN FORCE after Taste — they survive transcript compression; \
             re-loading is only needed to un-evict or refresh.\n\n{}{}",
            kept.join("\n\n"),
            evict_note
        )
    };
    format!("{taste_block}{supplement_block}")
}

/// Assemble a prompt with full runtime context injected.
///
/// `workspace_root` and `provider_name` are optional; when provided they appear
/// in the runtime context block. When absent, placeholders are used.
#[allow(clippy::too_many_arguments)]
pub fn assemble_prompt_with_context(
    spec: &AgentSpec,
    task: &TaskEnvelope,
    loaded: &LoadedMemories,
    session: &Session,
    workspace_root: Option<&Path>,
    provider_name: Option<&str>,
    // The tiered cross-thread block from `project_brain` — the main thread's
    // digest for a specialist, or the specialists' digests for the main thread.
    // None on a solo thread or non-mesh assembly.
    project_context: Option<&str>,
) -> PromptAssembly {
    // Durable PINNED memories (carried across turns on the session, so the
    // librarian never re-reads them) render first; any freshly-loaded-but-not-
    // yet-pinned memories follow. Dedup by path and normalized content so
    // overlapping librarian/indexer sources never make the provider pay for
    // the same memory twice.
    let memory_block = {
        let mut seen = std::collections::HashSet::new();
        let mut seen_content = std::collections::HashSet::new();
        let mut blocks: Vec<String> = Vec::new();
        for pinned in &session.pinned_memory {
            let content_key = pinned
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if seen.insert(pinned.path.clone()) && seen_content.insert(content_key) {
                blocks.push(format!(
                    "Path: {} (pinned project memory)\n{}",
                    pinned.path, pinned.content
                ));
            }
        }
        for memory in &loaded.memories {
            let content_key = memory
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if seen.insert(memory.path.clone()) && seen_content.insert(content_key) {
                blocks.push(format!(
                    "Path: {}\nGrounding: {}\nScore: {:.1}\nWhy: {}\n{}",
                    memory.path,
                    memory.grounding.as_str(),
                    memory.score,
                    memory.reasons.join(", "),
                    memory.content
                ));
            }
        }
        if blocks.is_empty() {
            "No relevant memories loaded.".to_string()
        } else {
            blocks.join("\n\n---\n\n")
        }
    };

    // User-authored intent/state is not generic knowledge. Put it immediately
    // after the current request at the high-attention edge and remove it from
    // the bulky knowledge list, so old completions and standing goals cannot
    // be buried or duplicated after compaction.
    let durable_user_context = loaded
        .knowledge_docs
        .iter()
        .find(|doc| doc.path == "session://user-context")
        .map(|doc| doc.summary.clone())
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or_else(|| crate::runtime::compaction::durable_user_context(session, None));
    let durable_user_block = if durable_user_context.is_empty() {
        String::new()
    } else {
        format!(
            "=== DURABLE USER INTENT AND STATE — binding across compaction ===\n\
             Read this before planning. These are direct bounded excerpts from this task's user; preserve their scope and completed states. If a mutable external status matters now, verify its authoritative live source rather than guessing from history.\n\
             {durable_user_context}\n\n"
        )
    };

    let ordinary_knowledge = loaded
        .knowledge_docs
        .iter()
        .filter(|doc| doc.path != "session://user-context")
        .collect::<Vec<_>>();
    let knowledge_block = if ordinary_knowledge.is_empty() {
        "No relevant knowledge docs loaded.".to_string()
    } else {
        crate::runtime::efficiency::join_unique(
            ordinary_knowledge
                .iter()
                .map(|doc| {
                    format!(
                        "Path: {}\nGrounding: {}\nScore: {:.1}\nSummary: {}",
                        doc.path,
                        doc.grounding.as_str(),
                        doc.score,
                        doc.summary
                    )
                })
                .collect::<Vec<_>>(),
            "\n",
        )
    };

    let librarian_receipts = if loaded.grounding_receipts.is_empty() {
        "No librarian receipts.".to_string()
    } else {
        crate::runtime::efficiency::join_unique(loaded.grounding_receipts.clone(), "\n")
    };

    let trust_receipts = if loaded.trust_receipts.is_empty() {
        "No memory trust receipts.".to_string()
    } else {
        crate::runtime::efficiency::join_unique(
            loaded
                .trust_receipts
                .iter()
                .map(render_trust_receipt)
                .collect::<Vec<_>>(),
            "\n",
        )
    };

    let omitted_block = if loaded.omitted_items.is_empty() {
        "No notable omissions.".to_string()
    } else {
        crate::runtime::efficiency::join_unique(loaded.omitted_items.clone(), "\n")
    };

    let librarian_selection_block = {
        let ranked = if loaded.ranked_context_items.is_empty() {
            "No ranked context items.".to_string()
        } else {
            crate::runtime::efficiency::join_unique(loaded.ranked_context_items.clone(), "\n")
        };
        let open_questions = if loaded.open_questions.is_empty() {
            "No open questions.".to_string()
        } else {
            crate::runtime::efficiency::join_unique(loaded.open_questions.clone(), "\n")
        };
        let recommended_next = loaded
            .recommended_next_agent_or_tool
            .as_deref()
            .unwrap_or("None");

        format!(
            "Budget used: {}\nRecommended next: {}\nRanked context:\n{}\nOpen questions:\n{}",
            loaded.context_budget_used, recommended_next, ranked, open_questions
        )
    };

    let context_block = if task.context.is_empty() {
        "No extra context.".to_string()
    } else {
        crate::runtime::efficiency::join_unique(
            task.context
                .iter()
                .map(|item| format!("{}: {}", item.label, item.content))
                .collect::<Vec<_>>(),
            "\n",
        )
    };

    let tool_block = if spec.tool_allowlist.is_empty() {
        "No tools allowed.".to_string()
    } else {
        crate::runtime::efficiency::compact_allowlist(&spec.tool_allowlist)
    };

    let artifact_block = if spec.output.required_artifacts.is_empty() {
        "No required artifacts.".to_string()
    } else {
        spec.output.required_artifacts.join(", ")
    };

    let workflow_block = if spec.workflow.notes.is_empty() {
        "No extra workflow notes.".to_string()
    } else {
        spec.workflow.notes.join("\n")
    };

    let session_block = if session.messages.is_empty() {
        "No prior session messages.".to_string()
    } else {
        render_bounded_session(&session.messages)
    };

    let shared_contract = shared_phoenix_contract();

    let mut runtime_ctx = runtime_context_block(
        spec,
        workspace_root,
        &task.session_id,
        provider_name,
        &spec.default_model,
    );
    let company_brief_actor = match &spec.target {
        AgentTargetSpec::Orchestrator => "phoenix".to_string(),
        AgentTargetSpec::Specialist(agent) => {
            crate::runtime::delegation::specialist_label(*agent).to_string()
        }
    };
    let company_brief =
        crate::runtime::context_compiler::brief(&task.session_id, &company_brief_actor);
    if !company_brief.is_empty() {
        runtime_ctx.push_str("\n\n");
        runtime_ctx.push_str(&company_brief);
    }
    let routine_actor = match &spec.target {
        AgentTargetSpec::Orchestrator => "phoenix".to_string(),
        AgentTargetSpec::Specialist(agent) => {
            crate::runtime::delegation::specialist_label(*agent).to_string()
        }
    };
    let routine_group = crate::runtime::workflow_teaching::group_id_for_session(&task.session_id);
    let routine_hints = crate::runtime::workflow_teaching::context_hints(
        &routine_actor,
        routine_group.as_deref(),
        &task.user_request,
    );
    if !routine_hints.is_empty() {
        runtime_ctx.push_str("\n\n");
        runtime_ctx.push_str(&routine_hints);
    }
    // Offer the same advisory discovery to every coworker. Keyword overlap
    // alone is not a mandatory playbook or permission to broaden the task.
    if let Some(root) = workspace_root {
        if let Some(hint) = crate::tools::skills::skill_hint(root, &task.user_request) {
            runtime_ctx.push_str("\n\n");
            runtime_ctx.push_str(&hint);
        }
    }
    // YOUR TEAM: the live company directory, injected per turn for EVERY
    // agent. Teammate names are user-editable runtime data, so no prompt file
    // hardcodes them; this block is where every coworker (including itself,
    // marked "(you)") learns the current names. Custom specialists live in
    // the registry (plan 019) and are folded in, deduped by role id.
    {
        let custom = crate::sub_agents::registry::production_custom_roster();
        let team = crate::runtime::company::global_if_initialized()
            .and_then(|company| company.directory_snapshot().ok())
            .map(|snapshot| {
                snapshot
                    .agents
                    .into_iter()
                    .map(|record| record.profile)
                    .collect::<Vec<_>>()
            });
        let self_key = match (&task.agent, &spec.target) {
            (Some(agent), _) => agent.internal_role.clone(),
            (None, AgentTargetSpec::Orchestrator) => "orchestrator".to_string(),
            (None, AgentTargetSpec::Specialist(agent)) => {
                crate::runtime::delegation::specialist_label(*agent).to_string()
            }
        };
        match team.and_then(|agents| render_team_block(&agents, &self_key, &custom)) {
            Some(block) => runtime_ctx.push_str(&block),
            // Directory unavailable: keep the pre-directory behaviour so the
            // orchestrator still knows every custom teammate exists.
            None if spec.name == "Orchestrator" && !custom.is_empty() => {
                runtime_ctx.push_str(
                    "\n\nCUSTOM SPECIALISTS (created via create_agent — valid `talk` targets like any teammate):",
                );
                for (role, description) in custom {
                    runtime_ctx.push_str(&format!("\n- `{role}` — {description}"));
                }
            }
            None => {}
        }
    }
    // "Lost in the middle": LLMs recall the START and END of a long context far
    // better than the middle (a measured 30+pt accuracy drop for mid-context
    // facts). The full request is authoritative at the top; this capped echo
    // rides the recency end so a long transcript can't bury what the user asked.
    let recency_pressure_chars = session_block
        .chars()
        .count()
        .saturating_add(memory_block.chars().count())
        .saturating_add(knowledge_block.chars().count())
        .saturating_add(context_block.chars().count())
        .saturating_add(
            project_context
                .map(|context| context.chars().count())
                .unwrap_or(0),
        );
    let request_echo =
        crate::runtime::efficiency::request_echo(&task.user_request, recency_pressure_chars);
    let durable_todos = durable_todo_block(&task.session_id);

    // The project brain rides the SALIENT top (right after the request), so
    // cross-thread memory is never buried in the dead middle. Empty when solo.
    let project_block = match project_context {
        Some(block) if !block.trim().is_empty() => format!(
            "=== PROJECT MEMORY — what your OTHER threads have done (retain it; never redo their work) ===\n{block}\n\n"
        ),
        _ => String::new(),
    };

    PromptAssembly {
        // Pinned references ride the SYSTEM side: binding constraints belong
        // with the rules, the position is cache-stable between loads, and no
        // transcript compression path can ever touch them.
        system_prompt: format!(
            "{shared_contract}\n\n{efficiency_contract}\n\n=== AGENT ROLE (read in order — binding rules before tools/exemplars) ===\n\n{system_prompt}{pinned_refs}",
            shared_contract = shared_contract,
            efficiency_contract = crate::runtime::efficiency::EXECUTION_EFFICIENCY_CONTRACT,
            system_prompt = spec.system_prompt,
            pinned_refs = pinned_refs_block(
                session,
                workspace_root,
                crate::runtime::design_contract::should_render_visual_references(
                    &task.user_request,
                ),
                &task.user_request,
            ),
        ),
        // Ordered for the U-shaped attention curve (context engineering): the
        // request + the grounding the model needs to ACT sit at the salient TOP;
        // the bulky session transcript goes in the MIDDLE (where mid-context recall
        // is weakest, so the least loss); low-signal librarian provenance trails it;
        // and the END carries the recency-critical re-read of the request + answer
        // style. Earlier the grounding (memories/knowledge/selection) was placed
        // AFTER the transcript — buried in the dead middle on any long session.
        user_prompt: format!(
            "Phoenix task\n\
             Title: {title}\n\
             Agent: {agent}\n\
             Allowed tools: {tools}\n\
             Required artifacts: {artifacts}\n\
             \n\
             {runtime_ctx}\n\
             \n\
             User request:\n{request}\n\
             \n\
             {durable_user_block}\
             {project_block}\
             === What Phoenix already knows for this task (grounding — read before acting) ===\n\
             Loaded memories:\n{memories}\n\
             \n\
             Knowledge references:\n{knowledge}\n\
             \n\
             Librarian selection metadata:\n{selection}\n\
             \n\
             Context:\n{context}\n\
             \n\
             Workflow notes:\n{workflow}\n\
             \n\
             === Conversation so far ===\n\
             Session transcript:\n{session_messages}\n\
             \n\
             === Librarian provenance (reference) ===\n\
             Librarian receipts:\n{receipts}\n\
             \n\
             Memory trust receipts:\n{trust_receipts}\n\
             \n\
             Omitted context:\n{omitted}\n\
             \n\
             {request_echo}\
             Final answer style: {style}",
            title = task.title,
            agent = spec.name,
            tools = tool_block,
            artifacts = artifact_block,
            runtime_ctx = runtime_ctx,
            request = task.user_request,
            durable_user_block = durable_user_block,
            project_block = project_block,
            context = context_block,
            session_messages = session_block,
            memories = memory_block,
            knowledge = knowledge_block,
            receipts = librarian_receipts,
            trust_receipts = trust_receipts,
            selection = librarian_selection_block,
            omitted = omitted_block,
            workflow = workflow_block,
            request_echo = request_echo
                .map(|request| format!(
                    "=== STAY ON TASK — request recency anchor ===\nUser request: {request}\n\n"
                ))
                .unwrap_or_default(),
            style = spec.output.final_answer_style,
        ),
        tail: durable_todos,
    }
}

/// The talk id a directory row answers to. The chief of staff's directory
/// row is `phoenix`, but `talk` addresses it as `orchestrator`.
fn team_talk_id(profile: &crate::runtime::company_directory::AgentProfile) -> &str {
    if profile.agent_id == "phoenix" || profile.internal_role == "phoenix" {
        "orchestrator"
    } else {
        profile.internal_role.as_str()
    }
}

/// Render the per-turn YOUR TEAM block from directory profiles. Pure (no
/// global store) so it is unit-testable. Only active agents are listed,
/// ordered by `sort_order`; the reading agent's own row is marked "(you)".
/// `self_key` may be an agent id, an internal role, or `orchestrator`.
/// `extra_custom` rows (registry custom specialists) are appended only when
/// the directory does not already list that role. Returns `None` when there
/// is nobody to list, so the caller can fall back.
pub(crate) fn render_team_block(
    agents: &[crate::runtime::company_directory::AgentProfile],
    self_key: &str,
    extra_custom: &[(String, String)],
) -> Option<String> {
    use crate::runtime::company_directory::LifecycleState;
    let mut active: Vec<&crate::runtime::company_directory::AgentProfile> = agents
        .iter()
        .filter(|profile| profile.lifecycle == LifecycleState::Active)
        .collect();
    active.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    let key = self_key.trim();
    let extras: Vec<&(String, String)> = extra_custom
        .iter()
        .filter(|(role, _)| {
            !agents.iter().any(|profile| {
                profile.internal_role.eq_ignore_ascii_case(role)
                    || profile.agent_id.eq_ignore_ascii_case(role)
            })
        })
        .collect();
    if active.is_empty() && extras.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\n\nYOUR TEAM (live directory; use these names, `talk` takes the role id):",
    );
    let mut own_name = None;
    for profile in active.iter().copied() {
        let talk_id = team_talk_id(profile);
        let name = match profile.display_name.trim() {
            "" => talk_id.to_string(),
            name => name.to_string(),
        };
        let is_self = !key.is_empty()
            && (profile.agent_id.eq_ignore_ascii_case(key)
                || profile.internal_role.eq_ignore_ascii_case(key)
                || talk_id.eq_ignore_ascii_case(key));
        let you = if is_self {
            own_name = Some(name.clone());
            " (you)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n- {name} (`{talk_id}`), {}: {}{you}",
            flat(&profile.role_title),
            flat(&profile.description),
        ));
    }
    for (role, description) in extras {
        let you = if role.eq_ignore_ascii_case(key) { " (you)" } else { "" };
        out.push_str(&format!(
            "\n- {role} (`{role}`), Custom specialist: {}{you}",
            flat(description)
        ));
    }
    if let Some(name) = own_name {
        out.push_str(&format!(
            "\nYou are {name}. Use the names above, never an older name you remember."
        ));
    }
    Some(out)
}

/// Assemble the alternate provider-native prompt view without weakening the
/// portable fallback. The durable Session remains untouched; this read-only
/// projection omits only the recovery-summary prefix already represented by
/// opaque replay. All standing system context, pinned material, task grounding,
/// and project memory are assembled identically to the portable view.
#[allow(clippy::too_many_arguments)]
pub fn assemble_prompt_with_native_suffix(
    spec: &AgentSpec,
    task: &TaskEnvelope,
    loaded: &LoadedMemories,
    session: &Session,
    suffix_start: usize,
    workspace_root: Option<&Path>,
    provider_name: Option<&str>,
    project_context: Option<&str>,
) -> Option<PromptAssembly> {
    if suffix_start == 0 || suffix_start > session.messages.len() {
        return None;
    }
    let mut projected = session.clone();
    projected.messages = session.messages[suffix_start..].to_vec();
    projected.clear_provider_compaction();
    Some(assemble_prompt_with_context(
        spec,
        task,
        loaded,
        &projected,
        workspace_root,
        provider_name,
        project_context,
    ))
}

/// Render the complete durable transcript selected for this prompt. Fixed-count
/// trimming used to hide old tool results and specialist returns long before the
/// model approached its context window. Context reduction now belongs solely to
/// the anchored compactor, which preserves a semantic summary, deterministic
/// tool ledger, archive, and recent verbatim tail.
fn render_bounded_session(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|message| {
            !matches!(message, Message::User { content } if is_internal_runtime_user_message(content))
        })
        .map(|message| match message {
            Message::ToolResult {
                tool_name,
                input,
                success,
                output,
            } => Message::format_tool_result(tool_name, input, *success, output),
            Message::Talk {
                from,
                to,
                subject,
                body,
                reply_expected,
                ..
            } => Message::format_talk_envelope(from, to, subject, body, *reply_expected),
            other => format_session_message(other),
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// Old Canvas builds could persist queue/wake transport envelopes as if the
/// user had typed them. Keep those durable rows for forensic recovery, but
/// never teach a model that Phoenix's own protocol prose came from the user.
fn is_internal_runtime_user_message(content: &str) -> bool {
    let normalized = content.trim().to_ascii_lowercase();
    normalized.starts_with("[late ask answer]")
        || normalized.starts_with("[queued wake]")
        || (normalized.starts_with("queued prompt queued_")
            && (normalized.ends_with(" is starting") || normalized.contains(" failed:")))
}

fn format_session_message(message: &Message) -> String {
    match message {
        Message::User { content } => format!("User\n{content}"),
        Message::Assistant { content } => {
            // Existing sessions may carry a legacy compaction banner that
            // called the model-written summary "ground truth". Do not mutate
            // the durable transcript, but repair that precedence in every
            // prompt projection so an old Avery session benefits immediately
            // instead of waiting for its next compaction.
            let projected = if content.starts_with("[AUTO-COMPACTED HISTORY") {
                content.replace(
                    "Treat it as ground truth of what already happened",
                    "Treat it only as a lossy navigation index of earlier work; the current user request, direct USER INTENT LEDGER, exact recalled archive rows, and freshly verified mutable state outrank it",
                )
            } else {
                content.clone()
            };
            format!("Assistant\n{projected}")
        }
        Message::Talk {
            from,
            to,
            subject,
            body,
            reply_expected,
            ..
        } => Message::format_talk_envelope(from, to, subject, body, *reply_expected),
        Message::GroupContribution {
            agent_id,
            display_name,
            role_title,
            subject,
            body,
            ..
        } => Message::format_group_contribution(agent_id, display_name, role_title, subject, body),
        Message::ToolResult {
            tool_name,
            input,
            success,
            output,
        } => Message::format_tool_result(tool_name, input, *success, output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::librarian::{
        GroundingLabel, InvalidationTrigger, KnowledgeDoc, MemoryEntry, MemorySource,
        MemoryTrustReceipt, MemoryVolatility, StalenessVerdict,
    };
    use crate::runtime::{
        AgentSpec, AgentTargetSpec, ContextItem, ExecutionStyle, OutputContract, PermissionProfile,
        WorkflowContract,
    };
    use crate::session::Session;

    fn minimal_spec() -> AgentSpec {
        AgentSpec {
            name: "Frontend".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: "design well".to_string(),
            default_model: "model".to_string(),
            tool_allowlist: vec!["design_reference".to_string()],
            permissions: PermissionProfile {
                can_delegate: false,
                can_use_shell: true,
                can_write_files: true,
                can_access_network: false,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::CodeExecution,
                must_report_to_orchestrator: true,
                review_required_before_done: false,
                notes: vec![],
            },
            output: OutputContract {
                label: "design report".to_string(),
                required_artifacts: vec![],
                final_answer_style: "structured".to_string(),
            },
        }
    }

    #[test]
    fn conversation_voice_reaches_main_direct_delegated_and_custom_roles() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let main = crate::orchestrator::Orchestrator::new(
            "model",
            crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
        );
        let coder = crate::sub_agents::coder_config().spec;
        let tester = crate::sub_agents::specialist_config(crate::session::SubAgentType::Tester).spec;
        let critic = crate::sub_agents::specialist_config(crate::session::SubAgentType::Critic).spec;
        let custom_dir = root.path().join("agents/voice_fixture");
        std::fs::create_dir_all(&custom_dir).unwrap();
        std::fs::write(
            custom_dir.join("agent.toml"),
            "persona = \"Fixture\"\ndescription = \"voice fixture\"\ntools = [\"read\", \"final_answer\"]\nenabled = true\n",
        ).unwrap();
        std::fs::write(
            custom_dir.join("system.md"),
            "Keep this custom role's identity and domain expertise.",
        )
        .unwrap();
        crate::sub_agents::registry::refresh();
        let custom = crate::sub_agents::specialist_config(crate::session::SubAgentType::custom(
            "voice_fixture",
        ))
        .spec;
        assert!(custom
            .system_prompt
            .contains("Keep this custom role's identity"));
        // Direct and delegated turns share the same assembler. Audience is
        // carried by the real request/transcript and runtime return route,
        // not guessed from a role name or an absent direct-conversation field.
        for (case, spec, delegated) in [
            ("main-direct", main.spec(), false),
            ("specialist-direct", &coder, false),
            ("specialist-delegated", &coder, true),
            ("tester-direct", &tester, false),
            ("tester-delegated", &tester, true),
            ("critic-direct", &critic, false),
            ("critic-delegated", &critic, true),
            ("custom-direct", &custom, false),
            ("custom-delegated", &custom, true),
        ] {
            let target = match spec.target {
                AgentTargetSpec::Orchestrator => crate::runtime::AgentTarget::Orchestrator,
                AgentTargetSpec::Specialist(agent) => {
                    crate::runtime::AgentTarget::Specialist(agent)
                }
            };
            let request = if delegated {
                "Check the assigned claim and return evidence and unresolved limits to me."
            } else {
                "Walk me through this carefully, with technical detail."
            };
            let mut task = TaskEnvelope::new("voice-fixture", target, "Voice", request);
            let mut session = Session::new_main("model", &spec.system_prompt);
            if delegated {
                session.push_message(crate::session::Message::Talk {
                    from: "Orchestrator".into(),
                    to: spec.name.clone(),
                    subject: "Voice".into(),
                    body: request.into(),
                    reply_expected: true,
                    handoff_id: "voice-handoff".into(),
                    reply_to: None,
                    causation_id: None,
                    status: "working".into(),
                });
            } else {
                task.agent = Some(crate::runtime::agent_conversation::AgentTurnContext {
                    agent_id: spec.target.label(),
                    internal_role: spec.target.label(),
                    display_name: spec.name.clone(),
                    role_title: "Fixture".into(),
                    canonical_session_id: task.session_id.clone(),
                });
                session.push_message(crate::session::Message::User {
                    content: request.into(),
                });
            }
            let assembled = assemble_prompt(spec, &task, &LoadedMemories::default(), &session);
            // Optional review evidence from this real assembler and synthetic
            // private home. This evaluates assembly, not model behaviour.
            if let Some(directory) = std::env::var_os("PHOENIX_PROMPT_REVIEW_DIR") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(
                    directory.join(format!("{case}.json")),
                    serde_json::to_vec_pretty(&assembled).unwrap(),
                )
                .unwrap();
            }
            assert!(assembled.system_prompt.contains(&spec.system_prompt));
            assert!(assembled
                .system_prompt
                .contains("technical questions deserve technical substance"));
            assert!(assembled.system_prompt.contains("Do not fake tool results"));
            if matches!(spec.target, AgentTargetSpec::Specialist(crate::session::SubAgentType::Coder | crate::session::SubAgentType::Tester | crate::session::SubAgentType::Critic)) {
                assert!(assembled.system_prompt.contains("at the depth they requested"));
                assert!(!assembled.system_prompt.contains("Your report — the `talk` body"));
                assert!(!assembled.system_prompt.contains("Keep it short unless the test matrix"));
                assert!(!assembled.system_prompt.contains("Your report is severity-ordered"));
            }
            assert!(!assembled.system_prompt.contains("two to five sentences"));
            assert!(!assembled
                .user_prompt
                .contains("structured specialist report"));
            assert!(assembled.user_prompt.ends_with(&format!(
                "Final answer style: {}",
                spec.output.final_answer_style
            )));
            assert!(assembled.user_prompt.contains(request));
            for artifact in &spec.output.required_artifacts {
                assert!(assembled.user_prompt.contains(artifact));
            }
            if matches!(spec.target, AgentTargetSpec::Specialist(_)) {
                assert!(assembled.user_prompt.contains("In direct conversation"));
                assert!(assembled.user_prompt.contains("For delegated work"));
                assert!(assembled
                    .user_prompt
                    .contains("evidence, artifacts, verification and unresolved limits"));
            }
        }
    }

    #[test]
    fn damaged_vital_history_surfaces_integrity_failure_in_assembled_context() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        let original = "## Always do\n- Ask before sending messages\n\n<!-- phoenix-vital-revisions-v1\n## Preferences\n- DAMAGED_HISTORY_IS_NOT_ACTIVE";
        std::fs::write(root.path().join("VITALS.md"), original).unwrap();
        let task = TaskEnvelope::new(
            "vital-integrity-fixture",
            crate::runtime::AgentTarget::Orchestrator,
            "Continue",
            "Continue the work",
        );
        let session = Session::new_main("model", "system");
        let assembled = assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        assert!(assembled.user_prompt.contains("Ask before sending messages"));
        assert!(assembled.user_prompt.contains("VITAL MEMORY INTEGRITY FAILURE"));
        assert!(assembled.user_prompt.contains("Tell the user"));
        assert!(!assembled.user_prompt.contains("DAMAGED_HISTORY_IS_NOT_ACTIVE"));
        assert_eq!(std::fs::read_to_string(root.path().join("VITALS.md")).unwrap(), original);
    }

    #[test]
    fn durable_todos_are_reinjected_without_resetting_completed_work() {
        let root = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(root.path());
        crate::tools::todo_execute(
            crate::tools::TodoWriteInput {
                todos: vec![
                    crate::tools::TodoItem {
                        task: "Verify the source".to_string(),
                        completed: true, ..Default::default()
                    },
                    crate::tools::TodoItem {
                        task: "Download the artifact".to_string(),
                        completed: false, ..Default::default()
                    },
                ],
            },
            "durable-plan",
        )
        .unwrap();
        let task = TaskEnvelope::new(
            "durable-plan",
            crate::runtime::AgentTarget::Orchestrator,
            "Continue",
            "Continue the work",
        );
        let session = Session::new_main("model", "system");
        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);

        // The live map rides the request tail, never the cache-prefix user prompt.
        assert!(!assembled.user_prompt.contains("CURRENT DURABLE TODO MAP"));
        assert!(assembled.tail.contains("CURRENT DURABLE TODO MAP"));
        assert!(assembled.tail.contains("- [x] Verify the source"));
        assert!(assembled
            .tail
            .contains("- [ ] Download the artifact"));
        assert!(assembled
            .tail
            .contains("do not replace it with synonymous planning"));
        assert!(assembled
            .tail
            .contains("perform the first unfinished action in this response"));
        assert!(assembled.tail.contains("(last written "));
        assert!(assembled.tail.contains("written for an earlier, different request, it is not your current job"));
    }

    /// The compaction-gitignore render side: a pinned design_reference load is
    /// re-materialized VERBATIM into the system prompt every assembly — the
    /// transcript copy can be pruned to a stub without losing the material.
    #[test]
    fn pinned_reference_rides_the_system_prompt() {
        let task = TaskEnvelope::new(
            "session-p",
            crate::runtime::AgentTarget::Orchestrator,
            "Redesign",
            "Redesign the page",
        );
        let mut session = Session::new_main("model", "system");
        session.push_message(Message::ToolResult {
            tool_name: "design_reference".to_string(),
            input: r#"{"path":"studio/SKILL.md"}"#.to_string(),
            success: true,
            // The transcript copy is ALREADY a pruned stub — the pin must not care.
            output: "…[pruned in durable session]".to_string(),
        });
        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        assert!(
            assembled
                .system_prompt
                .contains("PINNED SUPPLEMENTARY REFERENCE MATERIAL"),
            "supplement header present"
        );
        let studio =
            crate::tools::design_refs::execute(crate::tools::design_refs::DesignReferenceInput {
                path: Some("studio/SKILL.md".to_string()),
            })
            .unwrap()
            .content;
        let probe: String = studio.chars().skip(200).take(120).collect();
        assert!(
            assembled.system_prompt.contains(&probe),
            "full library text re-materialized from source, not from the stubbed transcript"
        );
        assert!(
            crate::runtime::design_contract::DesignContractGuard::new(
                "Redesign the page",
                &session,
            )
            .final_feedback()
            .is_some(),
            "a rendered supplement must never satisfy the mandatory Taste gate"
        );
    }

    #[test]
    fn visual_reference_pins_stay_out_of_non_building_turns() {
        let task = TaskEnvelope::new(
            "session-status",
            crate::runtime::AgentTarget::Orchestrator,
            "Status",
            "What are you doing right now?",
        );
        let mut session = Session::new_main("model", "system");
        for (tool, input) in [
            ("design_reference", r#"{"path":"taste/SKILL.md"}"#),
            ("design_reference", r#"{"path":"process/SKILL.md"}"#),
            ("skill", r#"{"name":"frontend-design-deslop"}"#),
        ] {
            session.push_message(Message::ToolResult {
                tool_name: tool.to_string(),
                input: input.to_string(),
                success: true,
                output: "loaded".to_string(),
            });
        }
        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        assert!(!assembled
            .system_prompt
            .contains("PRIMARY VISUAL-DESIGN CONTRACT"));
        assert!(!assembled
            .system_prompt
            .contains("PINNED SUPPLEMENTARY REFERENCE MATERIAL"));
        assert!(!assembled.system_prompt.contains("frontend-design-deslop"));
    }

    #[test]
    fn old_skill_pins_do_not_bloat_an_unrelated_school_turn() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join(".phoenix/skills");
        for (name, description, marker) in [
            (
                "mobbin-gallery-browse",
                "browse application interface patterns with browser tools",
                "MOBBIN_MANIFEST_BODY",
            ),
            (
                "writing-prose-like-a-human",
                "edit long form prose and articles with a natural voice",
                "PROSE_MANIFEST_BODY",
            ),
        ] {
            let root = skills.join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {description}\n---\n{marker}"),
            )
            .unwrap();
        }
        let mut session = Session::new_main("model", "system");
        for name in ["mobbin-gallery-browse", "writing-prose-like-a-human"] {
            session.push_message(Message::ToolResult {
                tool_name: "skill".into(),
                input: format!(r#"{{"name":"{name}"}}"#),
                success: true,
                output: "loaded".into(),
            });
        }

        let unrelated = pinned_refs_block(
            &session,
            Some(dir.path()),
            false,
            "What school work is next after Science Lesson 4?",
        );
        assert!(unrelated.is_empty(), "{unrelated}");

        let explicit = pinned_refs_block(
            &session,
            Some(dir.path()),
            false,
            "Use writing-prose-like-a-human to edit this article",
        );
        assert!(explicit.contains("PROSE_MANIFEST_BODY"));
        assert!(!explicit.contains("MOBBIN_MANIFEST_BODY"));
    }

    #[test]
    fn only_the_newest_visual_supplement_is_replayed() {
        let task = TaskEnvelope::new(
            "session-one-supplement",
            crate::runtime::AgentTarget::Orchestrator,
            "Build",
            "Build a new user interface",
        );
        let mut session = Session::new_main("model", "system");
        for path in ["taste/SKILL.md", "system/SKILL.md", "process/SKILL.md"] {
            session.push_message(Message::ToolResult {
                tool_name: "design_reference".to_string(),
                input: format!(r#"{{"path":"{path}"}}"#),
                success: true,
                output: "loaded".to_string(),
            });
        }

        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        assert!(assembled
            .system_prompt
            .contains("design_reference `process/SKILL.md` (pinned)"));
        assert!(!assembled
            .system_prompt
            .contains("design_reference `system/SKILL.md` (pinned)"));
        assert!(assembled
            .system_prompt
            .contains("Evicted from the pin budget"));
    }

    /// Taste wins the bounded design-reference budget before supplements.
    /// A huge newest supplement cannot evict it or push total pinned design
    /// material beyond the provider-economy boundary.
    #[test]
    fn taste_is_reserved_first_and_total_pin_budget_stays_bounded() {
        let task = TaskEnvelope::new(
            "session-b",
            crate::runtime::AgentTarget::Orchestrator,
            "Redesign",
            "Redesign the page",
        );
        let mut session = Session::new_main("model", "system");
        for path in ["taste/SKILL.md", "10k-look/SKILL.md", "vibecurb/motion.md"] {
            session.push_message(Message::ToolResult {
                tool_name: "design_reference".to_string(),
                input: format!(r#"{{"path":"{path}"}}"#),
                success: true,
                output: String::new(),
            });
        }
        let empty_session = Session::new_main("model", "system");
        let baseline = assemble_prompt(
            &minimal_spec(),
            &task,
            &LoadedMemories::default(),
            &empty_session,
        );

        // Taste spends first. The largest supplement cannot evict it and does
        // not fit in the remaining total design-reference budget.
        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        let sys = &assembled.system_prompt;
        assert!(
            !sys.contains("design_reference `strict/motion.md` (pinned)"),
            "largest supplement is evicted rather than overflowing the total budget"
        );
        assert!(
            !sys.contains("design_reference `immersive/SKILL.md` (pinned)"),
            "older evicted"
        );
        assert!(
            sys.contains("design_reference `taste/SKILL.md` (reserved, pinned)"),
            "Taste never competes with supplement budget"
        );
        let taste =
            crate::tools::design_refs::execute(crate::tools::design_refs::DesignReferenceInput {
                path: Some("taste/SKILL.md".to_string()),
            })
            .unwrap()
            .content;
        let taste_probe = taste.chars().skip(1_000).take(240).collect::<String>();
        assert!(
            sys.contains(&taste_probe),
            "the actual Taste source, not merely its pin/header, must render"
        );
        assert!(
            sys.contains("Evicted from the pin budget") && sys.contains("immersive/SKILL.md"),
            "eviction note names evicted supplements"
        );
        assert!(sys.contains("strict/motion.md"));
        assert!(taste_reference_is_rendered(&session));
        let pinned_growth = assembled
            .system_prompt
            .chars()
            .count()
            .saturating_sub(baseline.system_prompt.chars().count());
        assert!(
            pinned_growth <= PINNED_REFS_MAX_CHARS + 2_000,
            "Taste plus the largest supplement must remain within one bounded design-reference envelope; grew by {pinned_growth} chars"
        );

        let guard = crate::runtime::design_contract::DesignContractGuard::new(
            "Redesign the settings page",
            &session,
        );
        assert!(
            guard.final_feedback().is_none(),
            "runtime satisfaction and rendered reserved Taste use one predicate"
        );
    }

    #[test]
    fn assembles_prompt_with_memory_and_contract_sections() {
        let spec = AgentSpec {
            name: "Coder".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: "system".to_string(),
            default_model: "model".to_string(),
            tool_allowlist: vec!["read".to_string(), "write".to_string()],
            permissions: PermissionProfile {
                can_delegate: true,
                can_use_shell: true,
                can_write_files: true,
                can_access_network: false,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::CodeExecution,
                must_report_to_orchestrator: true,
                review_required_before_done: true,
                notes: vec!["Review diff.".to_string()],
            },
            output: OutputContract {
                label: "code report".to_string(),
                required_artifacts: vec!["plan".to_string()],
                final_answer_style: "structured".to_string(),
            },
        };

        let mut task = TaskEnvelope::new(
            "session-1",
            crate::runtime::AgentTarget::Orchestrator,
            "Fix bug",
            "Repair the failing path",
        );
        task.context.push(ContextItem {
            label: "Repo".to_string(),
            content: "phoenix_agent".to_string(),
        });

        let loaded = LoadedMemories {
            memories: vec![MemoryEntry {
                path: "HOT/learnings/test.md".to_string(),
                content: "Important learning".to_string(),
                excerpt: "Important learning".to_string(),
                grounding: GroundingLabel::Observed,
                score: 42.0,
                reasons: vec!["test".to_string()],
                tier: "HOT".to_string(),
            }],
            knowledge_docs: vec![KnowledgeDoc {
                path: "knowledge/ref.md".to_string(),
                summary: "Reference summary".to_string(),
                grounding: GroundingLabel::Retrieved,
                score: 21.0,
                reasons: vec!["test".to_string()],
            }],
            ranked_context_items: vec!["HOT/learnings/test.md [42.0]".to_string()],
            omitted_items: vec![],
            grounding_receipts: vec!["Loaded HOT/learnings/test.md".to_string()],
            trust_receipts: vec![MemoryTrustReceipt {
                path: "memory/WARM/projects/PHOENIX_PROJECT.md".to_string(),
                kind: "project_report".to_string(),
                scope: "phoenix_agent".to_string(),
                source: MemorySource::SpecialistInvestigation,
                volatility: MemoryVolatility::Slow,
                age_hours: 2.0,
                staleness: StalenessVerdict::Fresh,
                evidence: vec!["phoenix_agent/Cargo.toml".to_string()],
                invalidation_triggers: vec![InvalidationTrigger::WorkspaceFileChanged],
                summary: "current project report".to_string(),
            }],
            completion_state: "loaded".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: Some("coder".to_string()),
            context_budget_used: 128,
        };

        let mut session = Session::new_main("model", "system");
        session.push_message(Message::User {
            content: "Inspect the runtime".to_string(),
        });

        let assembled = assemble_prompt(&spec, &task, &loaded, &session);

        assert!(assembled.user_prompt.contains("Loaded memories"));
        assert!(assembled.user_prompt.contains("Important learning"));
        assert!(assembled.user_prompt.contains("Memory trust receipts"));
        assert!(assembled.user_prompt.contains("kind=project_report"));
        assert!(assembled.user_prompt.contains("staleness=fresh"));
        assert!(assembled.user_prompt.contains("Reference summary"));
        assert!(assembled.user_prompt.contains("Allowed tools: read, write"));
        assert!(assembled.user_prompt.contains("Session transcript"));
        assert!(assembled.user_prompt.contains("Inspect the runtime"));
        assert!(assembled
            .user_prompt
            .contains("Librarian selection metadata"));
        assert!(assembled.user_prompt.contains("Budget used: 128"));

        // Context-engineering order (U-shaped attention): grounding precedes
        // the transcript. A short transcript does not pay to duplicate the
        // request at the recency edge.
        let mem_pos = assembled.user_prompt.find("Loaded memories:").unwrap();
        let tx_pos = assembled.user_prompt.find("Session transcript:").unwrap();
        assert!(
            mem_pos < tx_pos,
            "grounding must precede the transcript, not be buried after it"
        );
        assert!(!assembled.user_prompt.contains("request recency anchor"));
    }

    #[test]
    fn every_named_and_future_coworker_prompt_receives_craft_and_persistence_contracts() {
        let home = tempfile::tempdir().expect("isolated Phoenix home");
        let _home_guard = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let assert_prompt =
            |spec: &AgentSpec, target: crate::runtime::AgentTarget, mut session: Session| {
                session.push_message(Message::User {
                    content: "I finished the first assessment; preserve the full vacation plan."
                        .to_string(),
                });
                let task = TaskEnvelope::new(
                    "taste-contract",
                    target,
                    "Design contract",
                    "Design a useful interface",
                );
                let assembled = assemble_prompt(spec, &task, &LoadedMemories::default(), &session);
                assert!(assembled.system_prompt.contains("Visual-design gate"));
                assert!(assembled.system_prompt.contains("`taste/SKILL.md`"));
                assert!(assembled.system_prompt.contains("Motion gate"));
                assert!(assembled
                    .system_prompt
                    .contains("whenever motion, animation or video output would help"));
                assert!(assembled.system_prompt.contains("`motion_graphics`"));
                assert!(assembled
                    .system_prompt
                    .contains("applies to every coworker"));
                assert!(assembled
                    .system_prompt
                    .contains("# Persistent completion discipline"));
                assert!(assembled
                    .system_prompt
                    .contains("Substantial autonomous work runs by a durable goal by default"));
                assert!(assembled
                    .system_prompt
                    .contains("Implement → Expert reread → Defect hunt → Polish"));
                assert!(assembled
                    .system_prompt
                    .contains("Taste and usability are acceptance criteria"));
                assert!(assembled
                    .system_prompt
                    .contains("Do not compose a done report while any required item is unmet"));
                assert!(assembled
                    .system_prompt
                    .contains("# Autonomous ownership loop"));
                assert!(assembled
                    .system_prompt
                    .contains("Do not make the user act as your project manager"));
                assert!(assembled
                    .system_prompt
                    .contains("perform a dependency sweep"));
                assert!(assembled
                    .system_prompt
                    .contains("Carry the work through the whole safe chain"));
                assert!(assembled
                    .system_prompt
                    .contains("Compaction is a navigation aid"));
                let durable_pos = assembled
                    .user_prompt
                    .find("=== DURABLE USER INTENT AND STATE")
                    .expect("every coworker receives the deterministic user-state lane");
                let transcript_pos = assembled
                    .user_prompt
                    .find("=== Conversation so far ===")
                    .expect("prompt has transcript");
                assert!(durable_pos < transcript_pos);
                assert!(assembled
                    .user_prompt
                    .contains("I finished the first assessment"));
                assert!(assembled.user_prompt.contains("full vacation plan"));
            };

        let phoenix = crate::orchestrator::Orchestrator::new(
            "model",
            crate::orchestrator::ORCHESTRATOR_SYSTEM_PROMPT,
        );
        assert_prompt(
            phoenix.spec(),
            crate::runtime::AgentTarget::Orchestrator,
            Session::new_main("model", "system"),
        );

        for role in crate::sub_agents::registry::BUILTIN_ROLES {
            let agent = crate::sub_agents::registry::builtin_by_label(role)
                .expect("named coworker resolves");
            let config = crate::sub_agents::specialist_config(agent);
            assert_prompt(
                &config.spec,
                crate::runtime::AgentTarget::Specialist(agent),
                Session::new_sub_agent(agent, "model", "system"),
            );
        }

        let volume_worker = crate::sub_agents::volume_worker::agent_type();
        let volume_config = crate::sub_agents::volume_worker::config();
        assert_prompt(
            &volume_config.spec,
            crate::runtime::AgentTarget::Specialist(volume_worker),
            Session::new_sub_agent(volume_worker, "model", "system"),
        );

        let crate::session::SubAgentType::Custom(future_id) =
            crate::session::SubAgentType::custom("future_prompt_coworker_test")
        else {
            unreachable!("custom coworker must use custom variant")
        };
        let future = crate::sub_agents::registry::custom_config(future_id);
        let future_agent = crate::session::SubAgentType::Custom(future_id);
        assert_prompt(
            &future.spec,
            crate::runtime::AgentTarget::Specialist(future_agent),
            Session::new_sub_agent(future_agent, "model", "system"),
        );
    }

    #[test]
    fn long_transcript_keeps_a_bounded_request_recency_anchor() {
        let task = TaskEnvelope::new(
            "session-long",
            crate::runtime::AgentTarget::Orchestrator,
            "Long task",
            "remember this objective ".repeat(80),
        );
        let mut session = Session::new_main("model", "system");
        session.push_message(Message::Assistant {
            content: "historical context ".repeat(4_500),
        });
        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        let transcript = assembled.user_prompt.find("Session transcript:").unwrap();
        let anchor = assembled
            .user_prompt
            .find("request recency anchor")
            .unwrap();
        assert!(anchor > transcript);
        let anchored = &assembled.user_prompt[anchor..];
        assert!(anchored.chars().count() < 900);
    }

    #[test]
    fn injects_shared_contract_into_system_prompt() {
        let spec = AgentSpec {
            name: "Orchestrator".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: "custom-system".to_string(),
            default_model: "model".to_string(),
            tool_allowlist: vec!["talk".to_string()],
            permissions: PermissionProfile {
                can_delegate: true,
                can_use_shell: false,
                can_write_files: false,
                can_access_network: false,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::RouteOnly,
                must_report_to_orchestrator: false,
                review_required_before_done: false,
                notes: vec![],
            },
            output: OutputContract {
                label: "test".to_string(),
                required_artifacts: vec![],
                final_answer_style: "test".to_string(),
            },
        };

        let task = TaskEnvelope::new(
            "session-2",
            crate::runtime::AgentTarget::Orchestrator,
            "Test",
            "Verify contract injection",
        );
        let loaded = LoadedMemories {
            memories: vec![],
            knowledge_docs: vec![],
            ranked_context_items: vec![],
            omitted_items: vec![],
            grounding_receipts: vec![],
            trust_receipts: vec![],
            completion_state: "empty".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: None,
            context_budget_used: 0,
        };
        let session = Session::new_main("model", "system");

        let assembled = assemble_prompt(&spec, &task, &loaded, &session);

        // Shared contract leads; agent role follows.
        assert!(assembled.system_prompt.starts_with("# Phoenix team basics"));
        assert!(assembled.system_prompt.contains("custom-system"));
        assert!(assembled.system_prompt.contains("Talk like a real person"));
        assert!(assembled.system_prompt.contains("Do not fake tool results"));
        assert!(assembled
            .system_prompt
            .contains("# Persistent completion discipline"));
        assert!(assembled
            .system_prompt
            .contains("# Autonomous ownership loop"));
        assert!(assembled
            .system_prompt
            .contains("Do not make the user act as your project manager"));
    }

    #[test]
    fn injects_runtime_context_block_into_user_prompt() {
        let spec = AgentSpec {
            name: "Coder".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: "system".to_string(),
            default_model: "gpt-5-mini".to_string(),
            tool_allowlist: vec![
                "read".to_string(),
                "write".to_string(),
                "str_replace".to_string(),
            ],
            permissions: PermissionProfile {
                can_delegate: false,
                can_use_shell: true,
                can_write_files: true,
                can_access_network: false,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::CodeExecution,
                must_report_to_orchestrator: true,
                review_required_before_done: true,
                notes: vec![],
            },
            output: OutputContract {
                label: "test".to_string(),
                required_artifacts: vec![],
                final_answer_style: "test".to_string(),
            },
        };

        let task = TaskEnvelope::new(
            "session-3",
            crate::runtime::AgentTarget::Orchestrator,
            "Test",
            "Verify runtime context",
        );
        let loaded = LoadedMemories {
            memories: vec![],
            knowledge_docs: vec![],
            ranked_context_items: vec![],
            omitted_items: vec![],
            grounding_receipts: vec![],
            trust_receipts: vec![],
            completion_state: "empty".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: None,
            context_budget_used: 0,
        };
        let session = Session::new_main("model", "system");

        let assembled = assemble_prompt_with_context(
            &spec,
            &task,
            &loaded,
            &session,
            Some(std::path::Path::new("/workspace/test")),
            Some("openrouter"),
            None,
        );

        assert!(assembled.user_prompt.contains("RUNTIME CONTEXT"));
        assert!(assembled.user_prompt.contains("Date:"));
        assert!(assembled.user_prompt.contains("/workspace/test"));
        assert!(assembled.user_prompt.contains("session-3"));
        assert!(assembled.user_prompt.contains("Agent: Coder"));
        assert!(assembled.user_prompt.contains("openrouter/gpt-5-mini"));
        assert!(assembled
            .user_prompt
            .contains("Available tools: read, write, str_replace"));
        assert!(assembled
            .user_prompt
            .contains("every listed tool is implemented"));
        assert!(!assembled.user_prompt.contains("codegraph_search"));
        assert!(!assembled.user_prompt.contains("lsp_go_to_definition"));

        // Project brain: with no cross-thread context, the block is absent.
        assert!(!assembled.user_prompt.contains("PROJECT MEMORY"));

        // With a project block, it renders at the salient top (after the request,
        // before the grounding section) and carries the sibling's work verbatim.
        let with_project = assemble_prompt_with_context(
            &spec,
            &task,
            &loaded,
            &session,
            Some(std::path::Path::new("/workspace/test")),
            Some("openrouter"),
            Some("[coder] built the parser — 3 files, tests green"),
        );
        assert!(with_project.user_prompt.contains("PROJECT MEMORY"));
        assert!(with_project.user_prompt.contains("built the parser"));
        let request_pos = with_project.user_prompt.find("User request:").unwrap();
        let project_pos = with_project.user_prompt.find("PROJECT MEMORY").unwrap();
        let grounding_pos = with_project
            .user_prompt
            .find("What Phoenix already knows")
            .unwrap();
        assert!(
            request_pos < project_pos && project_pos < grounding_pos,
            "project memory must sit between the request and the grounding block"
        );
    }

    fn team_profile(
        agent_id: &str,
        internal_role: &str,
        display_name: &str,
        role_title: &str,
        sort_order: i64,
        lifecycle: crate::runtime::company_directory::LifecycleState,
    ) -> crate::runtime::company_directory::AgentProfile {
        crate::runtime::company_directory::AgentProfile {
            agent_id: agent_id.to_string(),
            internal_role: internal_role.to_string(),
            display_name: display_name.to_string(),
            role_title: role_title.to_string(),
            description: format!("Owns {role_title}.\n  Second line."),
            color: "#000000".to_string(),
            icon_seed: agent_id.to_string(),
            kind: crate::runtime::company_directory::AgentKind::ResponsibilityOwner,
            lifecycle,
            pinned: false,
            sort_order,
            canonical_session_id: None,
            browser_profile_id: format!("agent-{agent_id}"),
            metadata_json: "{}".to_string(),
        }
    }

    #[test]
    fn your_team_lists_live_names_role_ids_and_marks_the_reader() {
        use crate::runtime::company_directory::LifecycleState;
        let agents = vec![
            team_profile("frontend", "frontend", "Leon Lin", "Product Design & Frontend", 2, LifecycleState::Active),
            team_profile("phoenix", "phoenix", "Tibo", "Chief of Staff", 0, LifecycleState::Active),
            team_profile("coder", "coder", "Robin", "Engineering", 1, LifecycleState::Active),
            team_profile("marketing", "marketing", "June", "Publishing & Content", 3, LifecycleState::Dormant),
            team_profile("growth", "growth", "Avery", "Growth", 4, LifecycleState::Active),
        ];
        let custom = vec![
            ("growth".to_string(), "duplicate of a directory row".to_string()),
            ("tax_helper".to_string(), "files quarterly taxes".to_string()),
        ];

        let block = render_team_block(&agents, "coder", &custom).unwrap();
        assert!(block.contains(
            "YOUR TEAM (live directory; use these names, `talk` takes the role id):"
        ));
        assert!(block.contains("\n- Tibo (`orchestrator`), Chief of Staff: Owns Chief of Staff. Second line."));
        assert!(block.contains("\n- Robin (`coder`), Engineering: Owns Engineering. Second line. (you)"));
        assert!(block.contains("\n- Leon Lin (`frontend`), Product Design & Frontend:"));
        assert!(block.contains("\n- Avery (`growth`), Growth:"));
        assert!(block.contains("\n- tax_helper (`tax_helper`), Custom specialist: files quarterly taxes"));
        assert!(!block.contains("June"), "dormant agents are not on the live team");
        assert!(!block.contains("duplicate of a directory row"), "custom rows dedupe by role id");
        assert_eq!(block.matches("(you)").count(), 1);
        assert!(block.contains("You are Robin."));
        // sort_order decides the order, not input order.
        let tibo = block.find("Tibo").unwrap();
        let robin = block.find("Robin").unwrap();
        let leon = block.find("Leon Lin").unwrap();
        assert!(tibo < robin && robin < leon);

        // The chief of staff's directory row is `phoenix`; turns key it as
        // `orchestrator` or `phoenix`, and both mark the same row.
        for key in ["orchestrator", "phoenix"] {
            let block = render_team_block(&agents, key, &[]).unwrap();
            assert!(block.contains("Tibo (`orchestrator`), Chief of Staff: Owns Chief of Staff. Second line. (you)"));
            assert_eq!(block.matches("(you)").count(), 1);
            assert!(block.contains("You are Tibo."));
        }

        // Nobody to list: the caller falls back to the registry roster.
        assert!(render_team_block(&[], "coder", &[]).is_none());
        let dormant_only = vec![agents[3].clone()];
        assert!(render_team_block(&dormant_only, "coder", &[]).is_none());
    }

    #[test]
    fn lean_prompt_files_have_required_agent_roles() {
        let coder = include_str!("../../prompts/coder_system.md");
        assert!(coder.starts_with("You are the Engineering coworker"));
        assert!(coder.contains("Skill-first is a hard rule"));
        assert!(coder.contains("Run the smallest useful verification"));

        let orchestrator = include_str!("../../prompts/orchestrator_system.md");
        assert!(orchestrator.starts_with("You are the chief of staff"));
        assert!(orchestrator.contains("researcher"));
        assert!(orchestrator.contains("browser"));
        assert!(orchestrator.contains("skill_search"));
        assert!(orchestrator.contains("You are the root operator"));
        assert!(coder.contains("To the user, answer naturally at the depth they requested"));
        assert!(coder.contains("To a coworker, the `talk` body is the evidence"));

        let researcher = include_str!("../../prompts/researcher_system.md");
        assert!(researcher.contains("visual-build reference handoff"));
        assert!(researcher.contains("different angles and conditions"));
        assert!(researcher.contains("No link dumps"));

        let frontend = include_str!("../../prompts/frontend_system.md");
        assert!(frontend.contains("use the researcher as a research partner"));
        assert!(frontend.starts_with("You are the Product Design and Frontend coworker"));
        assert!(frontend.contains("structurally different layouts"));

        let browser = include_str!("../../prompts/browser_system.md");
        assert!(!browser.contains("Surf"));
        assert!(!browser.contains("browser_swarm"));
        assert!(browser.contains("universal tool"));

        let prompts = [
            coder,
            orchestrator,
            researcher,
            browser,
            frontend,
            include_str!("../../prompts/presentation_system.md"),
            include_str!("../../prompts/computer_use_system.md"),
            include_str!("../../prompts/finance_system.md"),
            include_str!("../../prompts/database_system.md"),
            include_str!("../../prompts/hacker_system.md"),
            include_str!("../../prompts/critic_system.md"),
            include_str!("../../prompts/tester_system.md"),
            include_str!("../../prompts/planner_system.md"),
        ];
        for prompt in prompts {
            assert!(!prompt.contains("MANDATORY PRE-FLIGHT"));
            assert!(!prompt.contains("START HERE"));
            assert!(!prompt.contains("DEPTH OF THOUGHT"));
            assert!(!prompt.contains("Deep think"));
            assert!(!prompt.contains("DONOR OPERATING RULES"));
        }
    }

    #[test]
    fn specialist_prompt_receives_matching_installed_skill() {
        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace
            .path()
            .join(".phoenix/skills/email-followup-workflow");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: email-followup-workflow\ndescription: manage email followup workflow reliably\n---\n\n# Email follow-up\n",
        )
        .unwrap();

        let mut spec = minimal_spec();
        spec.name = "Nico".to_string();
        spec.target = AgentTargetSpec::Specialist(crate::session::SubAgentType::Scribe);
        let task = TaskEnvelope::new(
            "agent-scribe",
            crate::runtime::AgentTarget::Specialist(crate::session::SubAgentType::Scribe),
            "Email follow-up",
            "Manage this email followup workflow reliably",
        );
        let session =
            Session::new_sub_agent(crate::session::SubAgentType::Scribe, "model", "system");
        let assembled = assemble_prompt_with_context(
            &spec,
            &task,
            &LoadedMemories::default(),
            &session,
            Some(workspace.path()),
            Some("test"),
            None,
        );
        assert!(assembled
            .user_prompt
            .contains("OPTIONAL SKILL SUGGESTION (keyword match, not a requirement): `email-followup-workflow`"));
        assert!(!assembled.user_prompt.contains("REQUIRED SKILL"),
            "a heuristic match must not become a mandatory skill load");
    }

    #[test]
    fn assemble_prompt_without_context_uses_placeholders() {
        let spec = AgentSpec {
            name: "Unknown".to_string(),
            target: AgentTargetSpec::Orchestrator,
            system_prompt: "system".to_string(),
            default_model: "model".to_string(),
            tool_allowlist: vec![],
            permissions: PermissionProfile {
                can_delegate: false,
                can_use_shell: false,
                can_write_files: false,
                can_access_network: false,
            },
            workflow: WorkflowContract {
                execution_style: ExecutionStyle::RouteOnly,
                must_report_to_orchestrator: false,
                review_required_before_done: false,
                notes: vec![],
            },
            output: OutputContract {
                label: "test".to_string(),
                required_artifacts: vec![],
                final_answer_style: "test".to_string(),
            },
        };

        let task = TaskEnvelope::new(
            "session-4",
            crate::runtime::AgentTarget::Orchestrator,
            "Test",
            "Verify fallback",
        );
        let loaded = LoadedMemories {
            memories: vec![],
            knowledge_docs: vec![],
            ranked_context_items: vec![],
            omitted_items: vec![],
            grounding_receipts: vec![],
            trust_receipts: vec![],
            completion_state: "empty".to_string(),
            open_questions: vec![],
            recommended_next_agent_or_tool: None,
            context_budget_used: 0,
        };
        let session = Session::new_main("model", "system");

        // Original assemble_prompt (no context) still works
        let assembled = assemble_prompt(&spec, &task, &loaded, &session);

        assert!(assembled.system_prompt.starts_with("# Phoenix team basics"));
        assert!(assembled.user_prompt.contains("RUNTIME CONTEXT"));
        assert!(assembled.user_prompt.contains("Workspace: (unknown)"));
        assert!(assembled
            .user_prompt
            .contains("provider unavailable for this run"));
    }

    #[test]
    fn internal_canvas_transport_rows_never_impersonate_the_user() {
        let messages = vec![
            Message::User {
                content: "A real request".to_string(),
            },
            Message::User {
                content: "[late ask answer] internal wake envelope".to_string(),
            },
            Message::User {
                content: "queued prompt queued_deadbeef is starting".to_string(),
            },
            Message::Assistant {
                content: "A real answer".to_string(),
            },
        ];

        let rendered = render_bounded_session(&messages);
        assert!(rendered.contains("A real request"));
        assert!(rendered.contains("A real answer"));
        assert!(!rendered.contains("late ask answer"));
        assert!(!rendered.contains("queued_deadbeef"));
    }

    #[test]
    fn render_session_keeps_old_and_recent_tool_results_verbatim_until_compaction() {
        let big = "S".repeat(4_800);
        let mut messages = Vec::new();
        let old_marker = "OLDTOOLMARKER";
        messages.push(Message::ToolResult {
            tool_name: "file_symbols".to_string(),
            input: "runner.rs".to_string(),
            success: true,
            output: format!("{old_marker}{big}"),
        });
        for i in 0..24 {
            messages.push(Message::ToolResult {
                tool_name: "read".to_string(),
                input: format!("file{i}.rs"),
                success: true,
                output: format!("RECENT{i}X{big}"),
            });
        }

        let rendered = render_bounded_session(&messages);

        assert!(rendered.contains(&format!("{old_marker}{big}")));
        assert!(rendered.contains(&format!("RECENT{}X{big}", 23)));
        assert!(!rendered.contains("chars trimmed"));
    }

    #[test]
    fn long_multi_surface_history_stays_verbatim_until_context_compaction() {
        let task = TaskEnvelope::new(
            "multi-surface-run",
            crate::runtime::AgentTarget::Orchestrator,
            "Reconcile external work",
            "Inspect changed portal records, prepare artifacts, and update the connected tracker",
        );
        let mut session = Session::new_main("model", "system");
        session.push_message(Message::User {
            content: "Resume from durable receipts and do not replay completed writes".to_string(),
        });
        for index in 0..80 {
            session.push_message(Message::ToolResult {
                tool_name: if index % 2 == 0 {
                    "browser_extract".to_string()
                } else {
                    "composio_run".to_string()
                },
                input: format!(r#"{{"batch":{index}}}"#),
                success: true,
                output: format!("BATCH-{index}-RECEIPT\n{}", "x".repeat(20_000)),
            });
        }
        session.push_message(Message::Assistant {
            content: "LATEST-DURABLE-CHECKPOINT all changed records reconciled".to_string(),
        });

        let assembled =
            assemble_prompt(&minimal_spec(), &task, &LoadedMemories::default(), &session);
        assert!(assembled.user_prompt.chars().count() > 1_500_000);
        assert!(assembled.user_prompt.contains("BATCH-0-RECEIPT"));
        assert!(assembled.user_prompt.contains("BATCH-79-RECEIPT"));
        assert!(assembled.user_prompt.contains("LATEST-DURABLE-CHECKPOINT"));
        assert!(assembled
            .user_prompt
            .contains("Resume from durable receipts"));
    }

    #[test]
    fn render_session_keeps_specialist_returns_verbatim_until_compaction() {
        let big = "R".repeat(38_000);
        let old_body_marker = "OLDRETURNBODY";
        let old_subject = "Research: FeneVision PDF replication";
        let mut messages = vec![Message::User {
            content: "finish doorquoter".to_string(),
        }];
        messages.push(Message::Talk {
            from: "researcher".to_string(),
            to: "orchestrator".to_string(),
            subject: old_subject.to_string(),
            body: format!("{old_body_marker}{big}"),
            reply_expected: false,
            handoff_id: "message_old_return".to_string(),
            reply_to: None,
            causation_id: None,
            status: "done".to_string(),
        });
        for i in 0..6 {
            messages.push(Message::Talk {
                from: "coder".to_string(),
                to: "orchestrator".to_string(),
                subject: format!("return {i}"),
                body: format!("RECENTRET{i}{big}"),
                reply_expected: false,
                handoff_id: format!("message_recent_{i}"),
                reply_to: None,
                causation_id: None,
                status: "done".to_string(),
            });
        }

        let rendered = render_bounded_session(&messages);

        assert!(rendered.contains(old_subject), "subject must survive");
        assert!(rendered.contains(&format!("{old_body_marker}{big}")));
        assert!(rendered.contains(&format!("RECENTRET{}{}", 5, big)));
        assert!(!rendered.contains("chars trimmed"));
    }

    #[test]
    fn render_bounded_session_is_prefix_stable_between_generation_checkpoints() {
        let big = "S".repeat(4_800);
        let initial = 24;
        let mut messages: Vec<Message> = (0..initial)
            .map(|i| Message::ToolResult {
                tool_name: "read".to_string(),
                input: format!("f{i}"),
                success: true,
                output: format!("BODY{i}X{big}"),
            })
            .collect();

        let before = render_bounded_session(&messages);
        messages.push(Message::ToolResult {
            tool_name: "read".to_string(),
            input: "appended".to_string(),
            success: true,
            output: format!("APPENDED{big}"),
        });
        let after = render_bounded_session(&messages);

        // Appending a result inside the same generation must not rewrite any
        // earlier byte of the render — that is the prompt-prefix-cache contract.
        assert!(after.starts_with(&before));
        assert!(after.len() > before.len());
    }

    #[test]
    fn render_bounded_session_keeps_failures_verbatim() {
        let big = "E".repeat(4_800);
        let mut messages = vec![Message::ToolResult {
            tool_name: "grep".to_string(),
            input: "docs".to_string(),
            success: false,
            output: format!("ERRBODY{big}"),
        }];
        // Push it out of the recent window with successful results.
        for i in 0..6 {
            messages.push(Message::ToolResult {
                tool_name: "read".to_string(),
                input: format!("f{i}"),
                success: true,
                output: "ok".to_string(),
            });
        }
        let rendered = render_bounded_session(&messages);
        // Failure kept in full despite being old and bulky.
        assert!(rendered.contains(&format!("ERRBODY{big}")));
    }

    #[test]
    fn legacy_compaction_banner_cannot_override_direct_user_or_live_state() {
        let rendered = render_bounded_session(&[Message::Assistant {
            content: "[AUTO-COMPACTED HISTORY — 188 earlier messages were folded. Treat it as ground truth of what already happened; do not redo completed work.]\n\nStatus: Test Drive 1.2 is incomplete."
                .to_string(),
        }]);

        assert!(!rendered.contains("Treat it as ground truth"));
        assert!(rendered.contains("lossy navigation index"));
        assert!(rendered.contains("current user request"));
        assert!(rendered.contains("freshly verified mutable state outrank it"));
    }
}
