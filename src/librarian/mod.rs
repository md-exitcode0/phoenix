//! Phoenix memory — Cognee-backed knowledge graph.
//!
//! The old LLM librarian (model-driven preload/prune/save over a Markdown
//! file-tier store) is archived at `_archive/librarian-filetier/`. Memory now
//! works exactly like Cognee: `memory` holds the deterministic recall/remember
//! triggers, `cognee` wraps the vendored engine, and the remaining modules are
//! the runtime plumbing types that survived the archive (session cache, prompt
//! context types, trust receipts).

#[cfg(feature = "cognee")]
pub mod cognee;
pub mod memory;
mod session_cache;
mod types;

pub use session_cache::SessionCache;
pub use types::{
    render_trust_receipt, GroundingLabel, InvalidationTrigger, KnowledgeDoc, LoadedMemories,
    MemoryEntry, MemorySource, MemoryTrustReceipt, MemoryVolatility, StalenessVerdict,
};
