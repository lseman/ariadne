//! Config-driven custom language support — delegates TOML loading and
//! language resolution to the central [`language_registry`].
//!
//! Custom languages defined in `.ariadne/languages.toml` get a lightweight
//! generic tree-sitter walker. Built-in languages use the registry
//! definitions but keep their existing extractors.

use crate::core::{Edge, EdgeKind, Node, NodeKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

// Re-export the registry for external use.
pub use super::language_registry::{get_language, get_language_by_path, registry, LanguageDef};

/// Extract a single file for a known language definition.
///
/// For built-in languages (rust, python, cpp, typescript) the existing
/// extractors are used. For custom languages a generic walker is employed.
pub fn extract_file(
    path: &Path,
    graph: &mut dyn crate::core::GraphMut,
    lang_def: &LanguageDef,
) -> Result<()> {
    match lang_def.name.as_str() {
        "rust" => super::rust::extract_file(path, graph),
        "python" => super::python::extract_file(path, graph),
        "cpp" => super::cpp::extract_file(path, graph),
        "typescript" | "tsx" | "javascript" => super::typescript::extract_file(path, graph),
        _ => extract_custom_file(path, lang_def, graph),
    }
}

/// Extract a single file with the generic custom-language walker.
///
/// Builds tree-sitter queries from the node types defined in the language
/// definition and extracts functions, classes, and imports.
pub fn extract_custom_file(
    path: &Path,
    lang: &LanguageDef,
    graph: &mut dyn crate::core::GraphMut,
) -> Result<()> {
    let source = std::fs::read_to_string(path)?;
    let mut parser = Parser::new();

    let ts_lang = resolve_language(&lang.grammar).ok_or_else(|| {
        anyhow::anyhow!(
            "tree-sitter grammar '{}' not available for '{}'",
            lang.grammar,
            lang.name
        )
    })?;

    parser
        .set_language(ts_lang)
        .map_err(|e| anyhow::anyhow!("language load failed for '{}': {}", lang.name, e))?;

    let tree = parser.parse(&source, None).ok_or_else(|| {
        anyhow::anyhow!("parse failed for {} with '{}'", path.display(), lang.name)
    })?;

    let file_uri = path.to_string_lossy().to_string();
    let file_qn = format!("file::{}", file_uri);
    let file_is_test = crate::extract::test_detect::is_test_file_path(path);
    let file_id = graph.add_node(Node::new(NodeKind::File, &file_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    let mut cursor = QueryCursor::new();

    // --- function definitions ---
    for node_type in &lang.function_node_types {
        let query_str = format!("({} name: (identifier) @name) @def", node_type);
        if let Ok(query) = Query::new(ts_lang, &query_str) {
            let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            for m in matches {
                let mut name: Option<String> = None;
                let mut start = 0u32;
                let mut end = 0u32;
                for cap in m.captures {
                    let cn = &query.capture_names()[cap.index as usize];
                    let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
                    if *cn == "name" {
                        name = Some(text.to_string());
                    } else if *cn == "def" {
                        start = cap.node.start_position().row as u32;
                        end = cap.node.end_position().row as u32;
                    }
                }
                if let Some(n) = name {
                    let qn = format!("{}::{}", file_qn, n);
                    let mut node = Node::new(NodeKind::Function, &qn).with_source(
                        file_uri.clone(),
                        start,
                        end,
                    );
                    if file_is_test {
                        node = node.with_property("is_test", serde_json::Value::Bool(true));
                    }
                    let id = graph.add_node(node);
                    graph.add_edge(file_id, id, Edge::extracted(EdgeKind::Defines));
                }
            }
        }
    }

    // --- class/type definitions ---
    for node_type in &lang.class_node_types {
        // Try with (type_identifier) first, fall back to (identifier)
        let query_str = format!("({} name: (type_identifier) @name) @def", node_type);
        let query = Query::new(ts_lang, &query_str).unwrap_or_else(|_| {
            let fallback = format!("({} name: (identifier) @name) @def", node_type);
            Query::new(ts_lang, &fallback).expect("fallback query must compile")
        });
        let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        for m in matches {
            let mut name: Option<String> = None;
            let mut start = 0u32;
            let mut end = 0u32;
            for cap in m.captures {
                let cn = &query.capture_names()[cap.index as usize];
                let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
                if *cn == "name" {
                    name = Some(text.to_string());
                } else if *cn == "def" {
                    start = cap.node.start_position().row as u32;
                    end = cap.node.end_position().row as u32;
                }
            }
            if let Some(n) = name {
                let qn = format!("{}::{}", file_qn, n);
                let id = graph.add_node(Node::new(NodeKind::Class, &qn).with_source(
                    file_uri.clone(),
                    start,
                    end,
                ));
                graph.add_edge(file_id, id, Edge::extracted(EdgeKind::Defines));
            }
        }
    }

    // --- import definitions ---
    for node_type in &lang.import_node_types {
        // Try several common patterns for import node types
        let patterns = [
            format!("({} argument: (_) @path) @import", node_type),
            format!("({} path: (_) @path) @import", node_type),
            format!("({} source: (_) @path) @import", node_type),
        ];
        let mut found = false;
        for query_str in &patterns {
            if let Ok(query) = Query::new(ts_lang, query_str) {
                let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
                for m in matches {
                    for cap in m.captures {
                        let cn = &query.capture_names()[cap.index as usize];
                        if *cn != "path" {
                            continue;
                        }
                        let path_text = cap.node.utf8_text(source.as_bytes()).unwrap_or("").trim();
                        if path_text.is_empty() {
                            continue;
                        }
                        let mod_qn = format!("module::{}", clean_path(path_text));
                        let mod_id = graph.add_node(Node::new(NodeKind::Module, &mod_qn));
                        graph.add_edge(file_id, mod_id, Edge::extracted(EdgeKind::Imports));
                    }
                }
                found = true;
                break;
            }
        }
        // If no query matched, emit a placeholder import node
        if !found {
            tracing::debug!(
                "no import query matched for '{}' in {}",
                node_type,
                lang.name
            );
        }
    }

    // --- call placeholders ---
    for node_type in &lang.call_node_types {
        let query_str = format!("({} function: (identifier) @callee) @call", node_type);
        if let Ok(query) = Query::new(ts_lang, &query_str) {
            let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            for m in matches {
                for cap in m.captures {
                    let cn = &query.capture_names()[cap.index as usize];
                    if *cn == "callee" {
                        let name = cap.node.utf8_text(source.as_bytes()).unwrap_or("").trim();
                        if !name.is_empty()
                            && !crate::extract::should_suppress_call_placeholder(name)
                        {
                            let unresolved_qn = format!("unresolved::{}", name);
                            let unresolved_id =
                                graph.add_node(Node::new(NodeKind::Function, &unresolved_qn));
                            graph.add_edge(
                                file_id,
                                unresolved_id,
                                Edge::extracted(EdgeKind::Calls),
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn clean_path(s: &str) -> String {
    s.split("::")
        .filter(|p| !p.is_empty() && *p != "*")
        .map(|p| p.replace(['<', '>'], ""))
        .collect::<Vec<_>>()
        .join("::")
}

/// Resolve a grammar name to a tree-sitter language.
fn resolve_language(name: &str) -> Option<tree_sitter::Language> {
    match name {
        "rust" => Some(tree_sitter_rust::language()),
        "python" => Some(tree_sitter_python::language()),
        "cpp" | "c" | "c++" => Some(tree_sitter_cpp::language()),
        "tsx" | "typescript" => Some(tree_sitter_typescript::language_tsx()),
        "javascript" => Some(tree_sitter_typescript::language_typescript()),
        _ => {
            // Custom grammar — try tree_sitter_<name> crate
            // This is a best-effort heuristic; most custom grammars need
            // their own crate or a pre-compiled .so
            let _crate_name = format!("tree_sitter_{}", name.replace("-", "_"));
            // We can't dynamically load crates at runtime, so this is a no-op.
            // Custom languages require the tree-sitter grammar crate to be
            // added to Cargo.toml and a case in this match.
            None
        }
    }
}

/// Generate tree-sitter queries from a language definition's node types.
///
/// Returns a list of (query_string, capture_name) tuples that can be used
/// to build [`Query`] objects at runtime.
pub fn build_queries(lang_def: &LanguageDef) -> Vec<(String, String, String)> {
    let mut queries = Vec::new();

    // Function queries
    for node_type in &lang_def.function_node_types {
        queries.push((
            format!("({} name: (identifier) @name) @def", node_type),
            "function".into(),
            node_type.into(),
        ));
    }

    // Class queries (try type_identifier first, fallback to identifier)
    for node_type in &lang_def.class_node_types {
        queries.push((
            format!("({} name: (type_identifier) @name) @def", node_type),
            "class".into(),
            node_type.into(),
        ));
        queries.push((
            format!("({} name: (identifier) @name) @def", node_type),
            "class".into(),
            node_type.into(),
        ));
    }

    // Import queries (try several common patterns)
    for node_type in &lang_def.import_node_types {
        queries.push((
            format!("({} argument: (_) @path) @import", node_type),
            "import".into(),
            node_type.into(),
        ));
        queries.push((
            format!("({} path: (_) @path) @import", node_type),
            "import".into(),
            node_type.into(),
        ));
    }

    queries
}
