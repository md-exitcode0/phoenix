//! Single source of truth for the desktop ↔ gateway wire contract.
//!
//! The desktop is intentionally a standalone crate, so it includes this file
//! by path. Keeping the compatibility token here prevents independently built
//! release binaries from drifting and entering a restart loop.

/// Bump whenever the JSONL request/response contract changes incompatibly.
pub const GATEWAY_WIRE_PROTOCOL: &str = "phoenix-gateway-jsonl/2026-09-18.1";
