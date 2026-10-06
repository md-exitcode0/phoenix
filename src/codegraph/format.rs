//! Compact, agent-optimized rendering of graph results.
//!
//! Donor parity: CodeGraph formats results as terse one-liners instead of raw
//! JSON, because tokens are the cost. This is where the context saving lands —
//! a `callers` answer is a handful of `path:line name` lines, not several full
//! files. Every renderer here is deliberately line-oriented and bounded.

use super::store::{GraphStats, Symbol};

/// Render a symbol as a single compact line: `kind name — path:line  «sig»`.
pub fn symbol_line(symbol: &Symbol) -> String {
    let sig = symbol
        .signature
        .as_deref()
        .map(|s| format!("  «{s}»"))
        .unwrap_or_default();
    format!(
        "{} {} — {}:{}{}",
        symbol.kind, symbol.name, symbol.path, symbol.start_line, sig
    )
}

/// Render a list of symbols under a heading, capped, with an honest overflow note.
pub fn symbol_list(heading: &str, symbols: &[Symbol], cap: usize) -> String {
    if symbols.is_empty() {
        return format!("{heading}: none found.");
    }
    let mut out = format!("{heading} ({}):\n", symbols.len());
    for sym in symbols.iter().take(cap) {
        out.push_str("  ");
        out.push_str(&symbol_line(sym));
        out.push('\n');
    }
    if symbols.len() > cap {
        out.push_str(&format!("  … and {} more\n", symbols.len() - cap));
    }
    out
}

/// Render callees (name + optional resolved definition).
pub fn callee_list(heading: &str, callees: &[(String, Option<Symbol>)], cap: usize) -> String {
    if callees.is_empty() {
        return format!("{heading}: none found.");
    }
    let mut out = format!("{heading} ({}):\n", callees.len());
    for (name, resolved) in callees.iter().take(cap) {
        match resolved {
            Some(sym) => out.push_str(&format!("  {name} → {}:{}\n", sym.path, sym.start_line)),
            None => out.push_str(&format!("  {name} → (external/unresolved)\n")),
        }
    }
    if callees.len() > cap {
        out.push_str(&format!("  … and {} more\n", callees.len() - cap));
    }
    out
}

/// Render a call path as an ordered hop chain, one line per hop.
pub fn path_list(from: &str, to: &str, chain: &[(String, Option<Symbol>)]) -> String {
    if chain.is_empty() {
        return format!(
            "Call path `{from}` → `{to}`: none found. The graph tracks static call edges only — \
             dynamic dispatch, trait objects, callbacks, and cross-process hops are invisible to it; \
             verify with callers/callees around each end or a targeted read."
        );
    }
    let mut out = format!(
        "Call path `{from}` → `{to}` ({} hop(s)):\n",
        chain.len() - 1
    );
    for (i, (name, resolved)) in chain.iter().enumerate() {
        let arrow = if i == 0 { "  " } else { "  ↳ " };
        match resolved {
            Some(sym) => out.push_str(&format!(
                "{arrow}{name} — {}:{}\n",
                sym.path, sym.start_line
            )),
            None => out.push_str(&format!("{arrow}{name} — (external/unresolved)\n")),
        }
    }
    out
}

/// Render impact closure ranked by distance.
pub fn impact_list(name: &str, impact: &[(usize, Symbol)], cap: usize) -> String {
    if impact.is_empty() {
        return format!("Impact of changing `{name}`: no known callers (safe / leaf symbol).");
    }
    let mut out = format!(
        "Impact of changing `{name}` — {} affected symbol(s), ranked by call distance:\n",
        impact.len()
    );
    for (distance, sym) in impact.iter().take(cap) {
        out.push_str(&format!(
            "  [d{}] {} — {}:{}\n",
            distance, sym.name, sym.path, sym.start_line
        ));
    }
    if impact.len() > cap {
        out.push_str(&format!("  … and {} more\n", impact.len() - cap));
    }
    out
}

/// Render the project map for preload — a few lines an agent reads to avoid
/// speaking blindly about the codebase.
pub fn overview_with_hotspots(
    stats: &GraphStats,
    top_files: &[(String, usize)],
    hotspots: &[(String, String, usize)],
) -> String {
    let mut out = overview(stats, top_files);
    if !hotspots.is_empty() {
        out.push_str("Load-bearing symbols (highest fan-in — changing these ripples widest):\n");
        for (name, path, fan_in) in hotspots {
            out.push_str(&format!("  {name} ({path}) ← {fan_in} call sites\n"));
        }
    }
    out
}

pub fn overview(stats: &GraphStats, top_files: &[(String, usize)]) -> String {
    let mut out = format!(
        "Project symbol map: {} files, {} symbols, {} relationships.\n",
        stats.file_count, stats.symbol_count, stats.relationship_count
    );
    if !stats.by_kind.is_empty() {
        let kinds = stats
            .by_kind
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("By kind: {kinds}.\n"));
    }
    if !top_files.is_empty() {
        out.push_str("Densest modules:\n");
        for (path, count) in top_files {
            out.push_str(&format!("  {path} ({count} symbols)\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: "function".to_string(),
            path: "src/lib.rs".to_string(),
            start_line: 10,
            end_line: 20,
            signature: Some(format!("fn {name}()")),
            doc: None,
        }
    }

    #[test]
    fn symbol_list_caps_and_notes_overflow() {
        let syms: Vec<Symbol> = (0..5).map(|i| sym(&format!("f{i}"))).collect();
        let out = symbol_list("Callers", &syms, 2);
        assert!(out.contains("Callers (5)"));
        assert!(out.contains("and 3 more"));
    }

    #[test]
    fn empty_lists_are_honest() {
        assert!(symbol_list("Callers", &[], 5).contains("none found"));
        assert!(impact_list("x", &[], 5).contains("leaf symbol"));
    }
}
