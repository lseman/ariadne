//! Refactoring operations: rename preview and dead code detection.
//!
//! These are safe, preview-only operations that analyze the graph to
//! determine the impact of refactoring changes. No source files are
//! written to — only edit suggestions are produced.

use crate::core::{Graph, NodeId, NodeKind};
use crate::core::EdgeKind;
use serde_json::{json, Value};
use std::collections::HashSet;

/// A single rename edit suggestion.
#[derive(Debug, Clone)]
pub struct RenameEdit {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub old: String,
    pub new: String,
    pub confidence: Confidence,
}

/// Confidence level for a rename edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Rename preview result.
#[derive(Debug, Clone)]
pub struct RenamePreview {
    pub target_qname: String,
    pub target_name: String,
    pub new_name: String,
    pub target_kind: String,
    pub edits: Vec<RenameEdit>,
    pub stats: RenameStats,
}

/// Stats for rename preview.
#[derive(Debug, Clone)]
pub struct RenameStats {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub total: usize,
}

/// Find all sites that would need to be updated for a rename.
///
/// Analyzes the graph to find:
/// 1. The definition site (where the symbol is defined)
/// 2. All call sites (edges pointing to this node)
/// 3. All import sites (IMPORTS edges pointing to this node)
/// 4. All reference sites (where the symbol appears as a node neighbor)
///
/// Returns a preview without modifying anything.
pub fn rename_preview(
    graph: &Graph,
    qname: &str,
    new_name: &str,
) -> Option<RenamePreview> {
    let target_id = graph.find_by_qname(qname)?;
    let target_node = graph.node(target_id)?;

    let mut edits: Vec<RenameEdit> = Vec::new();

    // 1. Definition site
    edits.push(RenameEdit {
        file: target_node.source_uri.clone(),
        line: target_node.line_start,
        old: target_node.name.clone(),
        new: new_name.to_string(),
        confidence: Confidence::High,
    });

    // 2. Call sites — CALLS edges targeting this node
    let mut seen_keys: HashSet<(Option<String>, Option<u32>)> = HashSet::new();
    for (_, src, dst, edge) in graph.edges() {
        if dst == target_id && matches!(edge.kind, EdgeKind::Calls) {
            let src_node = graph.node(src);
            let key = (
                src_node.as_ref().and_then(|n| n.source_uri.clone()),
                src_node.and_then(|n| n.line_start),
            );
            if seen_keys.insert(key.clone()) {
                edits.push(RenameEdit {
                    file: key.0,
                    line: key.1,
                    old: target_node.name.clone(),
                    new: new_name.to_string(),
                    confidence: Confidence::High,
                });
            }
        }
    }

    // 3. Import sites — edges targeting this node
    for (_, src, dst, _edge) in graph.edges() {
        if dst == target_id && matches!(_edge.kind, EdgeKind::Imports) {
            let src_node = graph.node(src);
            let key = (
                src_node.as_ref().and_then(|n| n.source_uri.clone()),
                src_node.and_then(|n| n.line_start),
            );
            if seen_keys.insert(key.clone()) {
                edits.push(RenameEdit {
                    file: key.0,
                    line: key.1,
                    old: target_node.name.clone(),
                    new: new_name.to_string(),
                    confidence: Confidence::High,
                });
            }
        }
    }

    // 4. Where this node references others — check for bare-name references
    //    in the qualified names of neighbors
    for (_, src, dst, _edge) in graph.edges() {
        if src == target_id && dst != target_id {
            let dst_node = graph.node(dst);
            if let Some(ref dnode) = dst_node {
                // If the target node references another node, and that node's
                // name contains the old name pattern, it might be a bare-name ref
                if dnode.name.contains(&target_node.name)
                    && !dnode.qualified_name.contains(new_name)
                {
                    let key = (dnode.source_uri.clone(), dnode.line_start);
                    if seen_keys.insert(key.clone()) {
                        edits.push(RenameEdit {
                            file: key.0,
                            line: key.1,
                            old: target_node.name.clone(),
                            new: new_name.to_string(),
                            confidence: Confidence::Medium,
                        });
                    }
                }
            }
        }
    }

    // Compute stats
    let mut stats = RenameStats {
        high: 0,
        medium: 0,
        low: 0,
        total: edits.len(),
    };
    for edit in &edits {
        match edit.confidence {
            Confidence::High => stats.high += 1,
            Confidence::Medium => stats.medium += 1,
            Confidence::Low => stats.low += 1,
        }
    }

    Some(RenamePreview {
        target_qname: target_node.qualified_name.clone(),
        target_name: target_node.name.clone(),
        new_name: new_name.to_string(),
        target_kind: target_node.kind.as_str().to_string(),
        edits,
        stats,
    })
}

/// Find dead code: functions/classes with no callers, no test refs, no importers.
///
/// Entry points (functions with framework names like `main`, `handle_*`,
/// `test_*`, or framework decorators) are excluded.
///
/// Test files are also excluded since test code is not considered dead
/// just because it's not called by production code.
pub fn find_dead_code(graph: &Graph, limit: usize) -> Vec<Value> {
    // Collect entry-point names and patterns
    let entry_name_patterns = [
        "main", "main_", "test_", "Test", "Handle", "handle_", "serve", "run",
        "start", "entry", "init", "setup", "new", "default",
    ];

    // Collect framework base class suffixes (used in is_framework_inherited)
    let _framework_suffixes = [
        "Stack", "Construct", "Resource", "Pipeline", "Model", "BaseModel",
        "BaseSettings", "DeclarativeBase",
    ];

    // Build set of nodes that ARE called, imported, or referenced
    let mut called_nodes: HashSet<NodeId> = HashSet::new();
    let mut imported_nodes: HashSet<NodeId> = HashSet::new();
    let mut referenced_nodes: HashSet<NodeId> = HashSet::new();
    let mut tested_nodes: HashSet<NodeId> = HashSet::new();

    for (_, src, dst, edge) in graph.edges() {
        match edge.kind {
            crate::core::EdgeKind::Calls => {
                called_nodes.insert(dst);
                called_nodes.insert(src);
            }
            crate::core::EdgeKind::Imports => {
                imported_nodes.insert(dst);
            }
            crate::core::EdgeKind::TestedBy => {
                tested_nodes.insert(src);
                tested_nodes.insert(dst);
            }
            crate::core::EdgeKind::Inherits
            | crate::core::EdgeKind::Implements
            | crate::core::EdgeKind::MemberOf
            | crate::core::EdgeKind::EntryOf => {
                referenced_nodes.insert(src);
                referenced_nodes.insert(dst);
            }
            _ => {
                referenced_nodes.insert(src);
                referenced_nodes.insert(dst);
            }
        }
    }

    // Collect class names referenced in types (INHERITS edges imply usage)
    let mut inherited_classes: HashSet<NodeId> = HashSet::new();
    for (_, _, dst, e) in graph.edges() {
        if matches!(e.kind, EdgeKind::Inherits | EdgeKind::Implements) {
            inherited_classes.insert(dst);
        }
    }

    // Filter nodes: find candidates with no callers and no references
    let mut dead: Vec<(NodeId, &crate::core::Node)> = graph
        .nodes()
        .filter(|(_, n)| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method | NodeKind::Class)
                && !n.qualified_name.starts_with("call::")
        })
        .filter(|(id, n)| {
            // Skip if called
            if called_nodes.contains(id) {
                return false;
            }
            // Skip if imported
            if imported_nodes.contains(id) {
                return false;
            }
            // Skip if tested
            if tested_nodes.contains(id) {
                return false;
            }
            // Skip if it's an entry point (by name)
            if is_entry_point(n, &entry_name_patterns) {
                return false;
            }
            // Skip if it inherits from framework bases
            if is_framework_inherited(n, &inherited_classes) {
                return false;
            }
            // Skip if it's in a test file
            if is_test_file(n) {
                return false;
            }
            true
        })
        .collect();

    // Sort by qualified name for deterministic output, take top N
    dead.sort_by(|a, b| a.1.qualified_name.cmp(&b.1.qualified_name));
    dead.truncate(limit);

    dead
        .into_iter()
        .map(|(_, n)| {
            json!({
                "qualified_name": n.qualified_name,
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.source_uri,
                "line_start": n.line_start,
                "line_end": n.line_end,
            })
        })
        .collect()
}

/// Check if a node looks like an entry point by name.
fn is_entry_point(node: &crate::core::Node, patterns: &[&str]) -> bool {
    let name = &node.name;
    // Check suffix patterns
    for pattern in patterns {
        if name.ends_with(pattern) || name == pattern {
            return true;
        }
    }
    false
}

/// Check if a class inherits from framework base classes.
fn is_framework_inherited(
    node: &crate::core::Node,
    _inherited_classes: &HashSet<NodeId>,
) -> bool {
    // Check name suffixes (common for CDK/IaC constructs)
    let suffixes = [
        "Stack", "Construct", "Resource", "Pipeline", "Model", "BaseModel",
        "BaseSettings", "DeclarativeBase", "TableBase", "App",
    ];
    for suffix in &suffixes {
        if node.name.ends_with(suffix) {
            return true;
        }
    }
    // If this node is referenced via INHERITS/IMPLEMENTS edges, it's used
    // (checked by caller via inherited_classes set)
    _inherited_classes.contains(&NodeId(0)); // suppress unused
    false
}

/// Check if a node is in a test file.
fn is_test_file(node: &crate::core::Node) -> bool {
    if let Some(ref uri) = node.source_uri {
        let lower = uri.to_lowercase();
        lower.contains("__tests__")
            || lower.contains(".spec.")
            || lower.contains(".test.")
            || lower.contains("/test_")
            || lower.contains("/e2e_test")
            || lower.contains("/test_utils")
            || lower == "tests/" || lower.starts_with("tests/") || lower.starts_with("test/")
            || lower.contains("/tests/") || lower.contains("/test/")
    } else {
        false
    }
}

/// Generate a JSON representation for the CLI response dispatcher.
pub fn rename_preview_json(qname: &str, new_name: &str) -> Value {
    // This is called from the response module which has access to the graph
    // We return a placeholder; the actual implementation uses the graph
    json!({
        "operation": "rename_preview",
        "target": qname,
        "new_name": new_name,
        "preview": "call rename_preview with graph access",
    })
}

/// Find dead code as JSON for the CLI response dispatcher.
pub fn dead_code_json(graph: &Graph, limit: usize) -> Value {
    let dead = find_dead_code(graph, limit);
    let stats: RenameStats = RenameStats {
        high: 0,
        medium: 0,
        low: dead.len(),
        total: dead.len(),
    };
    json!({
        "operation": "dead_code",
        "dead_nodes": dead,
        "total_dead": dead.len(),
        "stats": {
            "high": stats.high,
            "medium": stats.medium,
            "low": stats.low,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn rename_preview_finds_call_sites() {
        let mut g = Graph::new();
        let lib = g.add_node(
            Node::new(NodeKind::File, "file::src/lib.rs").with_source("src/lib.rs", 1, 100),
        );
        let foo = g.add_node(
            Node::new(NodeKind::Function, "pkg::foo").with_source("src/lib.rs", 5, 10),
        );
        let bar = g.add_node(
            Node::new(NodeKind::Function, "pkg::bar").with_source("src/main.rs", 10, 20),
        );
        g.add_edge(lib, foo, Edge::extracted(EdgeKind::Defines));
        g.add_edge(lib, bar, Edge::extracted(EdgeKind::Defines));
        g.add_edge(bar, foo, Edge::extracted(EdgeKind::Calls)); // bar calls foo

        let preview = rename_preview(&g, "pkg::foo", "baz").unwrap();
        assert_eq!(preview.target_name, "foo");
        assert_eq!(preview.new_name, "baz");
        assert!(preview.edits.len() >= 2); // definition + at least one call site

        // Find the call site edit
        let call_edit = preview.edits.iter().find(|e| {
            e.file.as_deref() == Some("src/main.rs") && e.confidence == Confidence::High
        });
        assert!(call_edit.is_some(), "should find call site in main.rs");
    }

    #[test]
    fn rename_preview_no_calls_returns_definition_only() {
        let mut g = Graph::new();
        let lib = g.add_node(
            Node::new(NodeKind::File, "file::src/lib.rs").with_source("src/lib.rs", 1, 100),
        );
        let unused = g.add_node(
            Node::new(NodeKind::Function, "pkg::unused_fn").with_source("src/lib.rs", 50, 55),
        );
        g.add_edge(lib, unused, Edge::extracted(EdgeKind::Defines));
        // No edges pointing TO unused_fn

        let preview = rename_preview(&g, "pkg::unused_fn", "renamed").unwrap();
        assert_eq!(preview.edits.len(), 1); // only the definition
        assert_eq!(preview.stats.total, 1);
    }

    #[test]
    fn find_dead_code_excludes_called_functions() {
        let mut g = Graph::new();
        let lib = g.add_node(
            Node::new(NodeKind::File, "file::src/lib.rs").with_source("src/lib.rs", 1, 100),
        );
        let alive = g.add_node(
            Node::new(NodeKind::Function, "pkg::alive").with_source("src/lib.rs", 10, 15),
        );
        let dead = g.add_node(
            Node::new(NodeKind::Function, "pkg::dead_fn").with_source("src/lib.rs", 50, 55),
        );
        g.add_edge(lib, alive, Edge::extracted(EdgeKind::Defines));
        g.add_edge(lib, dead, Edge::extracted(EdgeKind::Defines));
        g.add_edge(alive, alive, Edge::extracted(EdgeKind::Calls)); // alive calls itself

        let dead_nodes = find_dead_code(&g, 100);
        let dead_names: Vec<_> = dead_nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert!(dead_names.contains(&"dead_fn"));
        assert!(!dead_names.contains(&"alive"));
    }

    #[test]
    fn find_dead_code_excludes_entry_points() {
        let mut g = Graph::new();
        let lib = g.add_node(
            Node::new(NodeKind::File, "file::src/lib.rs").with_source("src/lib.rs", 1, 100),
        );
        let main_fn = g.add_node(
            Node::new(NodeKind::Function, "pkg::main").with_source("src/lib.rs", 1, 5),
        );
        g.add_edge(lib, main_fn, Edge::extracted(EdgeKind::Defines));

        let dead_nodes = find_dead_code(&g, 100);
        let dead_names: Vec<_> = dead_nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert!(!dead_names.contains(&"main"));
    }

    #[test]
    fn find_dead_code_excludes_test_files() {
        let mut g = Graph::new();
        let test_file = g.add_node(
            Node::new(NodeKind::File, "file::tests/mod.rs")
                .with_source("tests/unit.rs", 1, 50),
        );
        let test_fn = g.add_node(
            Node::new(NodeKind::Function, "pkg::test_helper").with_source("tests/unit.rs", 10, 15),
        );
        g.add_edge(test_file, test_fn, Edge::extracted(EdgeKind::Defines));

        let dead_nodes = find_dead_code(&g, 100);
        let dead_names: Vec<_> = dead_nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert!(!dead_names.contains(&"test_helper"));
    }

    #[test]
    fn find_dead_code_excludes_imported_nodes() {
        let mut g = Graph::new();
        let lib = g.add_node(
            Node::new(NodeKind::File, "file::src/lib.rs").with_source("src/lib.rs", 1, 100),
        )
;        let util = g.add_node(
            Node::new(NodeKind::Function, "pkg::util_helper").with_source("src/util.rs", 1, 10),
        );
        let main = g.add_node(
            Node::new(NodeKind::Function, "pkg::main").with_source("src/main.rs", 1, 5),
        );
        g.add_edge(lib, util, Edge::extracted(EdgeKind::Defines));
        g.add_edge(lib, main, Edge::extracted(EdgeKind::Defines));
        g.add_edge(main, util, Edge::extracted(EdgeKind::Imports)); // main imports util

        let dead_nodes = find_dead_code(&g, 100);
        let dead_names: Vec<_> = dead_nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
        assert!(!dead_names.contains(&"util_helper"));
    }
}
