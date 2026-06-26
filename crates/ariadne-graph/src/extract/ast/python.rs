//! Python source extraction.
//!
//! The extractor keeps Python scopes in qualified names, so methods like
//! `A.__init__` and `B.__init__` remain distinct. It emits class,
//! function, method, import, inheritance, and call edges.

use crate::core::{Edge, EdgeKind, GraphMut, Node, NodeId, NodeKind};
use crate::extract::should_suppress_call_placeholder;
use crate::extract::test_detect::{is_test_file_path, is_test_name};
use anyhow::Result;
use std::fs;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

pub fn extract_file(path: &Path, graph: &mut dyn GraphMut) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let mut parser = Parser::new();
    parser
        .set_language(tree_sitter_python::language())
        .map_err(|e| anyhow::anyhow!("language load failed: {}", e))?;
    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed for {}", path.display()))?;

    let file_uri = path.to_string_lossy().to_string();
    let file_qn = format!("file::{}", file_uri);
    let file_is_test = is_test_file_path(path);
    let file_id = graph.add_node(Node::new(NodeKind::File, &file_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    emit_imports(tree.root_node(), &source, graph, file_id)?;
    walk_scope(
        tree.root_node(),
        &source,
        graph,
        &file_uri,
        &file_qn,
        file_id,
        Vec::new(),
        false,
        file_is_test,
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn walk_scope(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    file_uri: &str,
    file_qn: &str,
    parent_id: NodeId,
    scope: Vec<String>,
    parent_is_class: bool,
    file_is_test: bool,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name_node, source);
                let mut child_scope = scope.clone();
                child_scope.push(name.clone());
                let qn = scoped_qname(file_qn, &child_scope);
                let class_id = graph.add_node(Node::new(NodeKind::Class, &qn).with_source(
                    file_uri.to_string(),
                    child.start_position().row as u32,
                    child.end_position().row as u32,
                ));
                graph.add_edge(parent_id, class_id, Edge::extracted(EdgeKind::Defines));
                emit_python_bases(child, source, graph, class_id);
                if let Some(body) = child.child_by_field_name("body") {
                    walk_scope(
                        body,
                        source,
                        graph,
                        file_uri,
                        file_qn,
                        class_id,
                        child_scope,
                        true,
                        file_is_test,
                    );
                }
            }
            "function_definition" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name_node, source);
                let is_test = file_is_test || is_test_name(&name);
                let mut child_scope = scope.clone();
                child_scope.push(name);
                let qn = scoped_qname(file_qn, &child_scope);
                let kind = if parent_is_class {
                    NodeKind::Method
                } else {
                    NodeKind::Function
                };
                let mut node = Node::new(kind, &qn).with_source(
                    file_uri.to_string(),
                    child.start_position().row as u32,
                    child.end_position().row as u32,
                );
                if is_test {
                    node = node.with_property("is_test", serde_json::Value::Bool(true));
                }
                let fn_id = graph.add_node(node);
                graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
                if let Some(body) = child.child_by_field_name("body") {
                    emit_calls(body, source, graph, fn_id);
                    walk_scope(
                        body,
                        source,
                        graph,
                        file_uri,
                        file_qn,
                        fn_id,
                        child_scope,
                        false,
                        file_is_test,
                    );
                }
            }
            _ => {
                if child.is_named() {
                    walk_scope(
                        child,
                        source,
                        graph,
                        file_uri,
                        file_qn,
                        parent_id,
                        scope.clone(),
                        parent_is_class,
                        file_is_test,
                    );
                }
            }
        }
    }
}

fn emit_imports(
    root: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    file_id: NodeId,
) -> Result<()> {
    let query = Query::new(
        tree_sitter_python::language(),
        r#"
        [
          (import_statement name: (dotted_name) @path)
          (import_from_statement module_name: (dotted_name) @path)
        ]
        "#,
    )?;
    let mut cursor = QueryCursor::new();
    for m in cursor.matches(&query, root, source.as_bytes()) {
        for cap in m.captures {
            if cap.node.kind() != "dotted_name" {
                continue;
            }
            let path_text = text(cap.node, source);
            if path_text.is_empty() {
                continue;
            }
            let mod_id = graph.add_node(Node::new(
                NodeKind::Module,
                format!("module::{}", path_text),
            ));
            graph.add_edge(file_id, mod_id, Edge::extracted(EdgeKind::Imports));
        }
    }
    Ok(())
}

fn emit_python_bases(
    class_node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    class_id: NodeId,
) {
    let mut to_visit = children(class_node);
    while let Some(node) = to_visit.pop() {
        if matches!(node.kind(), "identifier" | "dotted_name" | "attribute") {
            let name = call_target_name(node, source).unwrap_or_else(|| text(node, source));
            if !name.is_empty() {
                let base_id = graph.add_node(Node::new(NodeKind::Class, format!("type::{}", name)));
                graph.add_edge(class_id, base_id, Edge::extracted(EdgeKind::Inherits));
            }
        }
        if node.kind() == "block" {
            continue;
        }
        to_visit.extend(children(node));
    }
}

fn emit_calls(node: tree_sitter::Node, source: &str, graph: &mut dyn GraphMut, caller: NodeId) {
    let mut to_visit = children(node);
    while let Some(n) = to_visit.pop() {
        if matches!(n.kind(), "function_definition" | "class_definition") {
            continue;
        }
        if n.kind() == "call" {
            if let Some(func_node) = n.child_by_field_name("function") {
                if let Some(name) = call_target_name(func_node, source) {
                    if should_suppress_call_placeholder(&name) {
                        continue;
                    }
                    let callee_id =
                        graph.add_node(Node::new(NodeKind::Function, format!("call::{}", name)));
                    graph.add_edge(caller, callee_id, Edge::ambiguous(EdgeKind::Calls));
                }
            }
        }
        to_visit.extend(children(n));
    }
}

fn call_target_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, source)),
        "attribute" => {
            let attr = node.child_by_field_name("attribute")?;
            Some(text(attr, source))
        }
        "dotted_name" => Some(text(node, source).rsplit('.').next()?.to_string()),
        _ => None,
    }
}

fn scoped_qname(file_qn: &str, scope: &[String]) -> String {
    format!("{}::{}", file_qn, scope.join("::"))
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
