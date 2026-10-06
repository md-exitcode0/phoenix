use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadedMemories {
    pub memories: Vec<MemoryEntry>,
    pub knowledge_docs: Vec<KnowledgeDoc>,
    pub ranked_context_items: Vec<String>,
    pub omitted_items: Vec<String>,
    pub grounding_receipts: Vec<String>,
    pub trust_receipts: Vec<MemoryTrustReceipt>,
    pub completion_state: String,
    pub open_questions: Vec<String>,
    pub recommended_next_agent_or_tool: Option<String>,
    pub context_budget_used: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub path: String,
    pub content: String,
    pub excerpt: String,
    pub grounding: GroundingLabel,
    pub score: f32,
    pub reasons: Vec<String>,
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDoc {
    pub path: String,
    pub summary: String,
    pub grounding: GroundingLabel,
    pub score: f32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroundingLabel {
    Observed,
    Retrieved,
    Summarized,
    Inferred,
}

impl GroundingLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Retrieved => "retrieved",
            Self::Summarized => "summarized",
            Self::Inferred => "inferred",
        }
    }
}

// ---------------------------------------------------------------------------
// Trust receipts. These describe the provenance/freshness of injected context
// so the orchestrator can decide verification depth. They survived the
// file-tier librarian archive because prompt assembly still renders them.
// ---------------------------------------------------------------------------

/// How a remembered body was produced. Combined with `evidence`, this is the
/// provenance half of the trust contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    /// Produced directly from tool output (read, coder investigation, web fetch).
    SpecialistInvestigation,
    /// Produced from a real specialist execution result block.
    SpecialistResult,
    /// Produced by the user, either explicit statement or remembered preference.
    UserStatement,
    /// Synthesized by the model from other memories or transcript context.
    Synthesized,
    /// Carried over from a previous session without re-grounding this turn.
    Inherited,
}

impl MemorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpecialistInvestigation => "specialist_investigation",
            Self::SpecialistResult => "specialist_result",
            Self::UserStatement => "user_statement",
            Self::Synthesized => "synthesized",
            Self::Inherited => "inherited",
        }
    }
}

/// Estimated volatility for a piece of remembered context.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVolatility {
    /// Stable for months/years (preferences, decisions).
    Stable,
    /// Stable for days to weeks (project reports, architecture docs).
    Slow,
    /// Stable for hours to a day (active investigation, current task state).
    Fast,
    /// Snapshot of a single moment. Should be replaced, not relied on long.
    Ephemeral,
}

impl MemoryVolatility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Slow => "slow",
            Self::Fast => "fast",
            Self::Ephemeral => "ephemeral",
        }
    }
}

/// Conditions that should make a downstream agent re-verify (or refuse to
/// trust) an injected memory body.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationTrigger {
    /// Any change to the referenced workspace paths invalidates this memory.
    WorkspaceFileChanged,
    /// Newer memory of the same kind/scope overwrites this one.
    Superseded,
    /// User has explicitly contradicted or reversed this memory.
    UserReversed,
    /// Time has passed beyond the freshness window.
    Stale,
    /// The referenced external surface (web, app, browser) has changed.
    ExternalSurfaceChanged,
}

impl InvalidationTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceFileChanged => "workspace_file_changed",
            Self::Superseded => "superseded",
            Self::UserReversed => "user_reversed",
            Self::Stale => "stale",
            Self::ExternalSurfaceChanged => "external_surface_changed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StalenessVerdict {
    /// Fresh enough to answer directly.
    Fresh,
    /// Within window but recheck before relying on it.
    Ageing,
    /// Older than the kind/volatility window allows; verify before use.
    Stale,
    /// Memory has an explicit invalidation trigger that was not cleared.
    Invalidated,
}

impl StalenessVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Ageing => "ageing",
            Self::Stale => "stale",
            Self::Invalidated => "invalidated",
        }
    }
}

/// Trust receipt: a one-line summary of an injected memory handed to the
/// orchestrator. No routing signal — just enough to decide verification depth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryTrustReceipt {
    pub path: String,
    pub kind: String,
    pub scope: String,
    pub source: MemorySource,
    pub volatility: MemoryVolatility,
    pub age_hours: f64,
    pub staleness: StalenessVerdict,
    pub evidence: Vec<String>,
    pub invalidation_triggers: Vec<InvalidationTrigger>,
    pub summary: String,
}

/// Render a single receipt as a one-line trust summary.
pub fn render_trust_receipt(receipt: &MemoryTrustReceipt) -> String {
    let evidence = if receipt.evidence.is_empty() {
        "no-evidence".to_string()
    } else {
        receipt.evidence.join(",")
    };
    let triggers = if receipt.invalidation_triggers.is_empty() {
        "no-triggers".to_string()
    } else {
        receipt
            .invalidation_triggers
            .iter()
            .map(|trigger| trigger.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{} | kind={} | scope={} | source={} | volatility={} | age={:.1}h | staleness={} | invalidates_on={} | evidence={} | {}",
        receipt.path,
        receipt.kind,
        receipt.scope,
        receipt.source.as_str(),
        receipt.volatility.as_str(),
        receipt.age_hours,
        receipt.staleness.as_str(),
        triggers,
        evidence,
        receipt.summary
    )
}
