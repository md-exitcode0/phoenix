# Phoenix Coworker Prompts

Readable source of truth for Phoenix system prompts. Rust loads the active source prompts via `include_str!`; live runtime overlays are mirrored under `.phoenix/prompts/`. Craft labels describe responsibility and knowledge, not extra visible or hidden agents. Every visible coworker receives the universal company/tool contract and keeps the identity of the conversation the user opened.

| File | Agent |
|------|--------|
| [orchestrator_system.md](./orchestrator_system.md) | Phoenix: company coordination, synthesis, and accountable delivery |
| [coder_system.md](./coder_system.md) | Engineering craft |
| [researcher_system.md](./researcher_system.md) | Research craft |
| [browser_system.md](./browser_system.md) | Universal private-browser operating contract |
| [computer_use_system.md](./computer_use_system.md) | Universal native-desktop operating contract |
| [frontend_system.md](./frontend_system.md) | Product-design and frontend craft |
| [presentation_system.md](./presentation_system.md) | Knowledge and artifact craft |
| [database_system.md](./database_system.md) | Data craft used by the responsible coworker |
| [hacker_system.md](./hacker_system.md) | Defensive-security craft |
| [critic_system.md](./critic_system.md) | Adversarial review craft |
| [tester_system.md](./tester_system.md) | Test and verification craft |
| [planner_system.md](./planner_system.md) | Planning and coordination craft |

**Archive:** [archive/2026-05-28-pre-block1/](./archive/2026-05-28-pre-block1/) — prompts replaced by the block-1 rewrite. The former LLM librarian prompt was retired when memory moved to Phoenix's deterministic knowledge graph.

**Doctrine:** Each active prompt has a `# How You Think` section plus operating rules, tool/surface contracts, and `# Full-Run Examples`. The thinking sections define the agent's cognitive shape: operator, failure-loop, evidence, browser-state, product-experience, screen-side-effect, artifact, data-contract, threat-chain, proof, defect, dependency-graph, or continuity. The examples live near the bottom of each prompt under `# Full-Run Examples`; they are behavior sketches, not response templates.

Every built-in and future custom coworker also receives the shared `# Persistent completion discipline` from `src/runtime/shared_contract.rs`. Substantial work starts or resumes a durable Phoenix workflow goal, records observable acceptance outcomes, runs four craft passes, independently re-verifies returned work, and reconciles the latest request before completion. This uses Phoenix's native `work`/`todo_write`/evidence runtime, without a second repository gate file or external completion hook. Source provenance and required notices remain in `THIRD_PARTY_NOTICES.md`.

Visible replies follow the shared conversation contract: routine narration stays private; `user_update` publishes a rare, necessary interim update. The final reply adds results and new information rather than repeating updates the user already read. The UI folds those earlier updates under the completed reply, scoped to the same speaker and turn, while preserving their original text behind a closed disclosure. A final reply accompanying a question remains visible.

The frontend coworker's integrated design process and local palette, typography, blueprint and copy
tools are documented in `docs/iris-design.md`. The primary runtime contract is
bounded, with at most one task-specific supplement. Product meaning and a fresh
visitor's actual browser journey are part of design verification, not optional
marketing polish. No design-mode toggle or model call is required by the local
`design_studio` utilities.

The examples are not optional decoration. They show the full route: how the agent widens, rejects the lazy path, chooses the right surface, handles approval boundaries, verifies, and finishes in normal coworker language without exposing private chain-of-thought or rigid `Decision/Reasoning` blocks.

Memory: the runtime recalls relevant knowledge-graph chunks before every turn and remembers durable outcomes after it (ingest → index → search, with maintenance on the 12h timer). No prompt drives it — it is deterministic runtime plumbing.
