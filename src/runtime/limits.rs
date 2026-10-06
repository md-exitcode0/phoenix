//! Shared runtime sampling settings.

/// Sampling temperature for every Phoenix agent loop.
pub const AGENT_TEMPERATURE: f32 = 0.0;

// Context retention is intentionally not configured here. Phoenix keeps the
// complete transcript in the active model window and delegates reduction to
// `runtime::compaction`, which runs from actual context pressure and preserves
// an anchored summary, deterministic tool ledger, archive, and recent verbatim
// tail. Fixed-count result trimming and fixed replay slices are forbidden.
