//! C and C++ source extraction.
//!
//! This is a pragmatic tree-sitter extractor for C-family code. It emits
//! file, namespace/module, class/type, function/method, import/include,
//! inheritance, and call edges.

use anyhow::Result;
use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use crate::extract::test_detect::{is_test_file_path, is_test_name};
use std::fs;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let mut parser = Parser::new();
    parser
        .set_language(tree_sitter_cpp::language())
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

    emit_includes(tree.root_node(), &source, graph, file_id)?;
    walk_scope(
        tree.root_node(),
        &source,
        graph,
        &file_uri,
        &file_qn,
        file_id,
        Vec::new(),
        file_is_test,
    );

    Ok(())
}

fn walk_scope(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut Graph,
    file_uri: &str,
    file_qn: &str,
    parent_id: NodeId,
    scope: Vec<String>,
    file_is_test: bool,
) {
    for child in children(node) {
        match child.kind() {
            "namespace_definition" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or_else(|| "anonymous_namespace".to_string());
                let mut child_scope = scope.clone();
                child_scope.push(name.clone());
                let ns_id = graph.add_node(Node::new(
                    NodeKind::Module,
                    format!("module::{}", child_scope.join("::")),
                ));
                graph.add_edge(parent_id, ns_id, Edge::extracted(EdgeKind::Defines));
                if let Some(body) = child.child_by_field_name("body") {
                    walk_scope(
                        body,
                        source,
                        graph,
                        file_uri,
                        file_qn,
                        ns_id,
                        child_scope,
                        file_is_test,
                    );
                }
            }
            "class_specifier" | "struct_specifier" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name_node, source);
                let mut child_scope = scope.clone();
                child_scope.push(name);
                let class_id = graph.add_node(
                    Node::new(NodeKind::Class, scoped_qname(file_qn, &child_scope)).with_source(
                        file_uri.to_string(),
                        child.start_position().row as u32,
                        child.end_position().row as u32,
                    ),
                );
                graph.add_edge(parent_id, class_id, Edge::extracted(EdgeKind::Defines));
                emit_cpp_bases(child, source, graph, class_id);
                if let Some(body) = child.child_by_field_name("body") {
                    walk_scope(
                        body,
                        source,
                        graph,
                        file_uri,
                        file_qn,
                        class_id,
                        child_scope,
                        file_is_test,
                    );
                }
            }
            "function_definition" => {
                if let Some((name, declarator)) = function_name(child, source) {
                    let is_test = file_is_test || is_test_name(name.rsplit("::").next().unwrap_or(&name));
                    let qn = if name.contains("::") {
                        format!("{}::{}", file_qn, name)
                    } else {
                        scoped_qname(file_qn, &[scope.clone(), vec![name.clone()]].concat())
                    };
                    let kind = if scope.is_empty() && !name.contains("::") {
                        NodeKind::Function
                    } else {
                        NodeKind::Method
                    };
                    let mut node = Node::new(kind, qn).with_source(
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
                    } else {
                        emit_calls(declarator, source, graph, fn_id);
                    }
                }
            }
            "declaration" => emit_declaration_function(
                child, source, graph, file_uri, file_qn, parent_id, &scope, file_is_test,
            ),
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
                        file_is_test,
                    );
                }
            }
        }
    }
}

fn emit_declaration_function(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut Graph,
    file_uri: &str,
    file_qn: &str,
    parent_id: NodeId,
    scope: &[String],
    file_is_test: bool,
) {
    let Some(declarator) = find_descendant(node, "function_declarator") else {
        return;
    };
    let Some(name) = name_from_declarator(declarator, source) else {
        return;
    };
    let is_test = file_is_test || is_test_name(name.rsplit("::").next().unwrap_or(&name));
    let qn = scoped_qname(file_qn, &[scope.to_vec(), vec![name]].concat());
    let mut decl_node = Node::new(NodeKind::Function, qn).with_source(
        file_uri.to_string(),
        node.start_position().row as u32,
        node.end_position().row as u32,
    );
    if is_test {
        decl_node = decl_node.with_property("is_test", serde_json::Value::Bool(true));
    }
    let id = graph.add_node(decl_node);
    graph.add_edge(parent_id, id, Edge::extracted(EdgeKind::Defines));
}

fn emit_includes(
    root: tree_sitter::Node,
    source: &str,
    graph: &mut Graph,
    file_id: NodeId,
) -> Result<()> {
    let query = Query::new(
        tree_sitter_cpp::language(),
        r#"(preproc_include path: (_) @path)"#,
    )?;
    let mut cursor = QueryCursor::new();
    for m in cursor.matches(&query, root, source.as_bytes()) {
        for cap in m.captures {
            let include = text(cap.node, source)
                .trim_matches('"')
                .trim_matches('<')
                .trim_matches('>')
                .to_string();
            if !include.is_empty() {
                let mod_id =
                    graph.add_node(Node::new(NodeKind::Module, format!("include::{}", include)));
                graph.add_edge(file_id, mod_id, Edge::extracted(EdgeKind::Imports));
            }
        }
    }
    Ok(())
}

fn emit_cpp_bases(
    class_node: tree_sitter::Node,
    source: &str,
    graph: &mut Graph,
    class_id: NodeId,
) {
    let mut to_visit = children(class_node);
    while let Some(node) = to_visit.pop() {
        if matches!(
            node.kind(),
            "base_class_clause" | "base_class_clause_repeat1"
        ) {
            for child in children(node) {
                if let Some(name) = type_name(child, source) {
                    let base_id =
                        graph.add_node(Node::new(NodeKind::Class, format!("type::{}", name)));
                    graph.add_edge(class_id, base_id, Edge::extracted(EdgeKind::Inherits));
                }
            }
        }
        if node.kind() == "field_declaration_list" {
            continue;
        }
        to_visit.extend(children(node));
    }
}

fn emit_calls(node: tree_sitter::Node, source: &str, graph: &mut Graph, caller: NodeId) {
    let mut to_visit = children(node);
    while let Some(n) = to_visit.pop() {
        if n.kind() == "function_definition" {
            continue;
        }
        if n.kind() == "call_expression" {
            if let Some(func) = n.child_by_field_name("function") {
                if let Some(name) = call_target_name(func, source) {
                    let callee_id =
                        graph.add_node(Node::new(NodeKind::Function, format!("call::{}", name)));
                    graph.add_edge(caller, callee_id, Edge::ambiguous(EdgeKind::Calls));
                }
            }
        }
        to_visit.extend(children(n));
    }
}

fn function_name<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
) -> Option<(String, tree_sitter::Node<'a>)> {
    let declarator = node
        .child_by_field_name("declarator")
        .or_else(|| find_descendant(node, "function_declarator"))?;
    name_from_declarator(declarator, source).map(|name| (name, declarator))
}

fn name_from_declarator(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
        | "operator_name" => Some(text(node, source)),
        "qualified_identifier" | "scoped_identifier" | "scoped_type_identifier" => {
            Some(text(node, source).replace(" :: ", "::"))
        }
        _ => {
            if let Some(d) = node.child_by_field_name("declarator") {
                return name_from_declarator(d, source);
            }
            for child in children(node) {
                if let Some(name) = name_from_declarator(child, source) {
                    return Some(name);
                }
            }
            None
        }
    }
}

fn call_target_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(text(node, source)),
        "field_expression" => node
            .child_by_field_name("field")
            .map(|field| text(field, source)),
        "qualified_identifier" | "scoped_identifier" => {
            Some(text(node, source).rsplit("::").next()?.trim().to_string())
        }
        _ => name_from_declarator(node, source),
    }
}

fn type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "qualified_identifier" | "scoped_type_identifier" => {
            Some(text(node, source).replace(" :: ", "::"))
        }
        _ => {
            for child in children(node) {
                if let Some(name) = type_name(child, source) {
                    return Some(name);
                }
            }
            None
        }
    }
}

fn find_descendant<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut to_visit = children(node);
    while let Some(n) = to_visit.pop() {
        if n.kind() == kind {
            return Some(n);
        }
        to_visit.extend(children(n));
    }
    None
}

fn scoped_qname(file_qn: &str, scope: &[String]) -> String {
    if scope.is_empty() {
        file_qn.to_string()
    } else {
        format!("{}::{}", file_qn, scope.join("::"))
    }
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
