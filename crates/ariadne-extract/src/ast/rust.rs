//! Rust source extraction.
//!
//! Emits nodes for:
//! - `File` (one per source file)
//! - `Function` (each `fn` declaration)
//! - `Class` (each `struct` or `enum`)
//! - `Impl`   (each `impl` block, with `inherent` or `trait` property)
//!
//! Emits edges for:
//! - `Defines` from file → function/struct/impl
//! - `Calls`   between functions (resolved heuristically by name; unresolved
//!   calls become edges to a synthetic `unresolved::<name>` node so the
//!   call site is preserved even when the target is external)
//! - `Imports` from file → module (one per `use` path)
//! - `Implements` from impl → trait (best-effort)

use anyhow::Result;
use ariadne_core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use std::fs;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let mut parser = Parser::new();
    parser
        .set_language(tree_sitter_rust::language())
        .map_err(|e| anyhow::anyhow!("language load failed: {}", e))?;
    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed for {}", path.display()))?;

    let file_uri = path.to_string_lossy().to_string();
    let file_qn = format!("file::{}", file_uri);
    let file_id = graph.add_node(Node::new(NodeKind::File, &file_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    // --- function definitions ---
    let fn_query = Query::new(
        tree_sitter_rust::language(),
        r#"(function_item name: (identifier) @name) @def"#,
    )?;
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&fn_query, tree.root_node(), source.as_bytes());
    for m in matches {
        let mut name: Option<String> = None;
        let mut start = 0u32;
        let mut end = 0u32;
        let mut def_node: Option<tree_sitter::Node> = None;
        for cap in m.captures {
            let cn = &fn_query.capture_names()[cap.index as usize];
            let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
            match cn.as_str() {
                "name" => name = Some(text.to_string()),
                "def" => {
                    start = cap.node.start_position().row as u32;
                    end = cap.node.end_position().row as u32;
                    def_node = Some(cap.node);
                }
                _ => {}
            }
        }
        if let Some(n) = name {
            let scope = def_node
                .map(|node| rust_scope(node, &source))
                .unwrap_or_default();
            let qn = if scope.is_empty() {
                format!("{}::{}", file_qn, n)
            } else {
                format!("{}::{}::{}", file_qn, scope.join("::"), n)
            };
            let kind = if scope.is_empty() {
                NodeKind::Function
            } else {
                NodeKind::Method
            };
            let id = graph.add_node(Node::new(kind, &qn).with_source(file_uri.clone(), start, end));
            graph.add_edge(file_id, id, Edge::extracted(EdgeKind::Defines));
        }
    }

    // --- trait definitions ---
    let trait_query = Query::new(
        tree_sitter_rust::language(),
        r#"(trait_item name: (type_identifier) @name) @def"#,
    )?;
    let matches = cursor.matches(&trait_query, tree.root_node(), source.as_bytes());
    for m in matches {
        let mut name: Option<String> = None;
        let mut start = 0u32;
        let mut end = 0u32;
        for cap in m.captures {
            let cn = &trait_query.capture_names()[cap.index as usize];
            let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
            match cn.as_str() {
                "name" => name = Some(text.to_string()),
                "def" => {
                    start = cap.node.start_position().row as u32;
                    end = cap.node.end_position().row as u32;
                }
                _ => {}
            }
        }
        if let Some(n) = name {
            let qn = format!("{}::{}", file_qn, n);
            let id = graph.add_node(Node::new(NodeKind::Trait, &qn).with_source(
                file_uri.clone(),
                start,
                end,
            ));
            graph.add_edge(file_id, id, Edge::extracted(EdgeKind::Defines));
        }
    }

    // --- struct definitions ---
    let struct_query = Query::new(
        tree_sitter_rust::language(),
        r#"(struct_item name: (type_identifier) @name) @def"#,
    )?;
    let matches = cursor.matches(&struct_query, tree.root_node(), source.as_bytes());
    for m in matches {
        let mut name: Option<String> = None;
        let mut start = 0u32;
        let mut end = 0u32;
        for cap in m.captures {
            let cn = &struct_query.capture_names()[cap.index as usize];
            let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
            match cn.as_str() {
                "name" => name = Some(text.to_string()),
                "def" => {
                    start = cap.node.start_position().row as u32;
                    end = cap.node.end_position().row as u32;
                }
                _ => {}
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

    // --- enum definitions ---
    let enum_query = Query::new(
        tree_sitter_rust::language(),
        r#"(enum_item name: (type_identifier) @name) @def"#,
    )?;
    let matches = cursor.matches(&enum_query, tree.root_node(), source.as_bytes());
    for m in matches {
        let mut name: Option<String> = None;
        let mut start = 0u32;
        let mut end = 0u32;
        for cap in m.captures {
            let cn = &enum_query.capture_names()[cap.index as usize];
            let text = cap.node.utf8_text(source.as_bytes()).unwrap_or("");
            match cn.as_str() {
                "name" => name = Some(text.to_string()),
                "def" => {
                    start = cap.node.start_position().row as u32;
                    end = cap.node.end_position().row as u32;
                }
                _ => {}
            }
        }
        if let Some(n) = name {
            let qn = format!("{}::{}", file_qn, n);
            let id = graph.add_node(Node::new(NodeKind::Type, &qn).with_source(
                file_uri.clone(),
                start,
                end,
            ));
            graph.add_edge(file_id, id, Edge::extracted(EdgeKind::Defines));
        }
    }

    // --- use declarations (imports) ---
    let use_query = Query::new(
        tree_sitter_rust::language(),
        r#"(use_declaration argument: (_) @path) @use"#,
    )?;
    let matches = cursor.matches(&use_query, tree.root_node(), source.as_bytes());
    for m in matches {
        for cap in m.captures {
            let cn = &use_query.capture_names()[cap.index as usize];
            if cn.as_str() != "path" {
                continue;
            }
            let path_text = cap.node.utf8_text(source.as_bytes()).unwrap_or("").trim();
            if path_text.is_empty() {
                continue;
            }
            let mod_qn = format!("module::{}", clean_use_path(path_text));
            let mod_id = graph.add_node(Node::new(NodeKind::Module, &mod_qn));
            graph.add_edge(file_id, mod_id, Edge::extracted(EdgeKind::Imports));
        }
    }

    // --- call expressions ---
    // We walk function_item nodes, then look for call_expression descendants
    // and emit Calls edges from the enclosing function to a (possibly
    // unresolved) callee node keyed by the called identifier.
    let fn_with_calls = Query::new(
        tree_sitter_rust::language(),
        r#"
        (function_item
            name: (identifier) @caller_name
            body: (block) @body) @def
        "#,
    )?;
    let matches = cursor.matches(&fn_with_calls, tree.root_node(), source.as_bytes());
    for m in matches {
        let mut caller_name: Option<String> = None;
        let mut body_node: Option<tree_sitter::Node> = None;
        let mut def_node: Option<tree_sitter::Node> = None;
        for cap in m.captures {
            let cn = &fn_with_calls.capture_names()[cap.index as usize];
            match cn.as_str() {
                "caller_name" => {
                    caller_name = Some(
                        cap.node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string(),
                    )
                }
                "body" => body_node = Some(cap.node),
                "def" => def_node = Some(cap.node),
                _ => {}
            }
        }
        let (caller_name, body) = match (caller_name, body_node) {
            (Some(n), Some(b)) => (n, b),
            _ => continue,
        };
        let scope = def_node
            .map(|node| rust_scope(node, &source))
            .unwrap_or_default();
        let caller_qn = if scope.is_empty() {
            format!("{}::{}", file_qn, caller_name)
        } else {
            format!("{}::{}::{}", file_qn, scope.join("::"), caller_name)
        };
        let caller_id = match graph.find_by_qname(&caller_qn) {
            Some(id) => id,
            None => continue,
        };
        emit_calls_in_subtree(body, &source, graph, caller_id);
    }

    Ok(())
}

fn emit_calls_in_subtree(node: tree_sitter::Node, source: &str, graph: &mut Graph, caller: NodeId) {
    let mut walker = node.walk();
    let mut to_visit: Vec<tree_sitter::Node> = node.children(&mut walker).collect();
    while let Some(n) = to_visit.pop() {
        if n.kind() == "call_expression" {
            if let Some(func_node) = n.child_by_field_name("function") {
                let name = call_target_name(func_node, source);
                if let Some(name) = name {
                    let callee_qn = format!("call::{}", name);
                    let callee_id = graph.add_node(Node::new(NodeKind::Function, &callee_qn));
                    graph.add_edge(caller, callee_id, Edge::extracted(EdgeKind::Calls));
                }
            }
        }
        let mut w = n.walk();
        for child in n.children(&mut w) {
            to_visit.push(child);
        }
    }
}

fn call_target_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node.utf8_text(source.as_bytes()).ok()?.to_string()),
        "field_expression" => {
            // foo.bar() — take `bar`
            let field = node.child_by_field_name("field")?;
            Some(field.utf8_text(source.as_bytes()).ok()?.to_string())
        }
        "scoped_identifier" | "scoped_type_identifier" => {
            // module::path::name — take the last segment
            let text = node.utf8_text(source.as_bytes()).ok()?;
            Some(text.rsplit("::").next()?.to_string())
        }
        _ => None,
    }
}

fn clean_use_path(s: &str) -> String {
    s.trim().trim_end_matches(';').trim().to_string()
}

fn rust_scope(mut node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut scope = Vec::new();
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "impl_item" => {
                if let Some(name) = impl_type_name(parent, source) {
                    scope.push(name);
                }
            }
            "trait_item" => {
                if let Some(name) = parent.child_by_field_name("name") {
                    scope.push(text(name, source));
                }
            }
            "mod_item" => {
                if let Some(name) = parent.child_by_field_name("name") {
                    scope.push(text(name, source));
                }
            }
            _ => {}
        }
        node = parent;
    }
    scope.reverse();
    scope
}

fn impl_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    for child in children(node) {
        if matches!(
            child.kind(),
            "type_identifier" | "scoped_type_identifier" | "generic_type"
        ) {
            let raw = text(child, source);
            return Some(raw.split('<').next().unwrap_or(&raw).trim().to_string());
        }
    }
    None
}

fn children(node: tree_sitter::Node) -> Vec<tree_sitter::Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or("")
        .trim()
        .to_string()
}
