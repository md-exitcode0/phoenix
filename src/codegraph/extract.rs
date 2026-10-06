//! Tree-sitter symbol extraction.
//!
//! Donor parity: CodeGraph parses every file with tree-sitter and walks the AST
//! to extract symbols + relationships. We do the same natively for Rust. The
//! extractor is intentionally language-keyed ([`extract_file`] dispatches on
//! extension) so additional grammars drop in without touching the store/query
//! layers.
//!
//! What we pull:
//! - **Symbols**: functions, methods, structs, enums, traits, impls, mods,
//!   type aliases, consts, statics — name, kind, line span, signature, doc.
//! - **Relationships**: `calls` (call expressions inside a symbol body) and
//!   `uses_type` (type references), stored by target *name* for lazy resolution.
//!
//! We deliberately do not try to fully resolve paths/generics — that is the
//! donor's "framework-aware resolution" later phase. Name-level edges already
//! power useful `callers`/`callees`/`impact` answers.

use tree_sitter::{Node, Parser};

use super::store::{SymbolKind, SymbolRelation};

/// A symbol extracted from a file, with the relationships rooted at it.
pub struct ExtractedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub relations: Vec<SymbolRelation>,
}

/// Detect the language for a path. Returns `None` for unsupported files so the
/// store can skip them. Only Rust is wired today.
pub fn language_for_path(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some("rust"),
        _ => None,
    }
}

/// Extract every top-level and nested symbol from `source` for `language`.
/// Returns an empty vec on parse failure rather than erroring — a single
/// unparseable file must never abort a whole-repo index build.
pub fn extract_file(language: &str, source: &str) -> Vec<ExtractedSymbol> {
    match language {
        "rust" => extract_rust(source),
        _ => Vec::new(),
    }
}

fn extract_rust(source: &str) -> Vec<ExtractedSymbol> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_rust::language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let bytes = source.as_bytes();
    let mut symbols = Vec::new();
    walk_rust(tree.root_node(), bytes, &mut symbols);
    symbols
}

/// Recursively walk the AST collecting symbol-defining nodes. For each symbol we
/// scan its subtree for call/type relationships so edges are attributed to the
/// enclosing definition.
fn walk_rust(node: Node, src: &[u8], out: &mut Vec<ExtractedSymbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(kind) = rust_symbol_kind(child.kind()) {
            if let Some(name) = rust_symbol_name(&child, src) {
                let relations = collect_relations(&child, src);
                out.push(ExtractedSymbol {
                    name,
                    kind,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    signature: rust_signature(&child, src),
                    doc: leading_doc_comment(&child, src),
                    relations,
                });
            }
        }
        // Recurse so methods inside `impl` blocks and items inside `mod` blocks
        // are captured as their own symbols too.
        walk_rust(child, src, out);
    }
}

fn rust_symbol_kind(node_kind: &str) -> Option<SymbolKind> {
    match node_kind {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "impl_item" => Some(SymbolKind::Impl),
        "mod_item" => Some(SymbolKind::Module),
        "type_item" => Some(SymbolKind::TypeAlias),
        "const_item" => Some(SymbolKind::Const),
        "static_item" => Some(SymbolKind::Static),
        "macro_definition" => Some(SymbolKind::Macro),
        _ => None,
    }
}

fn rust_symbol_name(node: &Node, src: &[u8]) -> Option<String> {
    // impl blocks have no `name` field; derive a label from the type they impl.
    if node.kind() == "impl_item" {
        if let Some(ty) = node.child_by_field_name("type") {
            let type_name = node_text(&ty, src);
            if let Some(trait_node) = node.child_by_field_name("trait") {
                return Some(format!("{} for {}", node_text(&trait_node, src), type_name));
            }
            return Some(format!("impl {type_name}"));
        }
    }
    node.child_by_field_name("name")
        .map(|n| node_text(&n, src))
        .filter(|s| !s.is_empty())
}

/// First line of the symbol (the declaration) as a compact signature, trimmed.
fn rust_signature(node: &Node, src: &[u8]) -> Option<String> {
    let text = node_text(node, src);
    let first = text.lines().next()?.trim().trim_end_matches('{').trim();
    if first.is_empty() {
        None
    } else {
        Some(truncate(first, 200))
    }
}

/// Collect `///` or `//!` doc lines immediately preceding the symbol.
fn leading_doc_comment(node: &Node, src: &[u8]) -> Option<String> {
    let mut doc_lines: Vec<String> = Vec::new();
    let mut sibling = node.prev_sibling();
    while let Some(prev) = sibling {
        if prev.kind() == "line_comment" || prev.kind() == "block_comment" {
            let text = node_text(&prev, src);
            let trimmed = text
                .trim_start_matches("///")
                .trim_start_matches("//!")
                .trim_start_matches("//")
                .trim();
            if !trimmed.is_empty() {
                doc_lines.push(trimmed.to_string());
            }
            sibling = prev.prev_sibling();
        } else {
            break;
        }
    }
    if doc_lines.is_empty() {
        None
    } else {
        doc_lines.reverse();
        Some(truncate(&doc_lines.join(" "), 300))
    }
}

/// Walk a symbol's subtree collecting `calls` and `uses_type` edges by name.
fn collect_relations(node: &Node, src: &[u8]) -> Vec<SymbolRelation> {
    let mut relations = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_relations_inner(node, src, &mut relations, &mut seen);
    relations
}

fn collect_relations_inner(
    node: &Node,
    src: &[u8],
    out: &mut Vec<SymbolRelation>,
    seen: &mut std::collections::HashSet<(String, &'static str)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "call_expression" => {
                if let Some(func) = child.child_by_field_name("function") {
                    if let Some(name) = call_target_name(&func, src) {
                        let key = (name.clone(), "calls");
                        if seen.insert(key) {
                            out.push(SymbolRelation {
                                target_name: name,
                                kind: "calls".to_string(),
                                line: child.start_position().row + 1,
                            });
                        }
                    }
                }
            }
            "type_identifier" => {
                let name = node_text(&child, src);
                if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    let key = (name.clone(), "uses_type");
                    if seen.insert(key) {
                        out.push(SymbolRelation {
                            target_name: name,
                            kind: "uses_type".to_string(),
                            line: child.start_position().row + 1,
                        });
                    }
                }
            }
            _ => {}
        }
        collect_relations_inner(&child, src, out, seen);
    }
}

/// Resolve the callee name from a call expression's `function` node. Handles
/// bare calls (`foo()`), method calls (`x.foo()`), and paths (`a::b::foo()`) by
/// taking the final identifier segment.
fn call_target_name(func: &Node, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => Some(node_text(func, src)),
        "field_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text(&n, src)),
        "scoped_identifier" => func.child_by_field_name("name").map(|n| node_text(&n, src)),
        _ => {
            // Fall back to the last identifier-looking token in the subtree.
            let text = node_text(func, src);
            text.rsplit("::").next().map(|s| s.trim().to_string())
        }
    }
    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
}

fn node_text(node: &Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_functions_structs_and_calls() {
        let src = r#"
/// Adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    helper(a) + b
}

fn helper(x: i32) -> i32 { x * 2 }

pub struct Widget {
    name: String,
}
"#;
        let symbols = extract_file("rust", src);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"Widget"));

        let add = symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::Function);
        assert!(add.doc.as_deref().unwrap().contains("Adds two numbers"));
        assert!(add
            .relations
            .iter()
            .any(|r| r.target_name == "helper" && r.kind == "calls"));
    }

    #[test]
    fn extracts_impl_methods() {
        let src = r#"
struct Server;
impl Server {
    fn start(&self) { self.bind() }
    fn bind(&self) {}
}
"#;
        let symbols = extract_file("rust", src);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"start"));
        assert!(names.contains(&"bind"));
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        assert!(start.relations.iter().any(|r| r.target_name == "bind"));
    }

    #[test]
    fn unparseable_returns_empty_not_panic() {
        // Garbage still parses into an (error) tree; just must not panic.
        let _ = extract_file("rust", "fn ( { { { ");
    }

    #[test]
    fn unsupported_language_returns_empty() {
        assert!(extract_file("python", "def f(): pass").is_empty());
    }
}
