//! TypeScript, TSX, and JavaScript source extraction.
//!
//! Emits file, class, interface, type alias, enum, function, method, import/export,
//! and call edges from tree-sitter TypeScript parse trees. Plain JavaScript
//! (.js/.mjs/.cjs) parses under the TypeScript grammar; .jsx uses the TSX grammar.

use crate::core::{Edge, EdgeKind, GraphMut, Node, NodeId, NodeKind};
use crate::extract::should_suppress_call_placeholder;
use crate::extract::test_detect::{is_test_file_path, is_test_name};
use anyhow::Result;
use std::fs;
use std::path::Path;
use tree_sitter::Parser;

pub fn extract_file(path: &Path, graph: &mut dyn GraphMut) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let mut parser = Parser::new();
    let lang = if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s == "tsx" || s == "jsx")
        .unwrap_or(false)
    {
        tree_sitter_typescript::language_tsx()
    } else {
        tree_sitter_typescript::language_typescript()
    };
    parser
        .set_language(lang)
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

    let ctx = TsContext {
        source: &source,
        file_uri: &file_uri,
        file_qn: &file_qn,
        file_is_test,
    };
    walk_scope(tree.root_node(), graph, file_id, Vec::new(), &ctx);

    Ok(())
}

struct TsContext<'a> {
    source: &'a str,
    file_uri: &'a str,
    file_qn: &'a str,
    file_is_test: bool,
}

fn walk_scope(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: Vec<String>,
    ctx: &TsContext<'_>,
) {
    for child in children(node) {
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            "import_statement" => {
                emit_import_statement(child, ctx.source, graph, parent_id);
            }
            "export_declaration" => {
                if let Some(decl) = child.child_by_field_name("declaration") {
                    emit_exported_declaration(&decl, graph, parent_id, &scope, ctx);
                }
                emit_re_exports(child, ctx.source, graph, parent_id);
            }
            "export_statement" => {
                if let Some(decl) = child.child_by_field_name("declaration") {
                    emit_exported_declaration(&decl, graph, parent_id, &scope, ctx);
                }
            }
            "type_alias_declaration" => {
                emit_type_alias(
                    child,
                    ctx.source,
                    graph,
                    ctx.file_uri,
                    ctx.file_qn,
                    parent_id,
                    &scope,
                );
            }
            "interface_declaration" => {
                emit_interface(child, graph, parent_id, &scope, ctx);
            }
            "class_declaration" => {
                emit_class(child, graph, parent_id, &scope, ctx);
            }
            "enum_declaration" => {
                emit_enum(
                    child,
                    ctx.source,
                    graph,
                    ctx.file_uri,
                    ctx.file_qn,
                    parent_id,
                    &scope,
                );
            }
            "function_declaration" => {
                emit_fn(child, graph, parent_id, &scope, ctx);
            }
            "variable_declaration" | "lexical_declaration" => {
                emit_var_functions(child, graph, parent_id, &scope, ctx);
            }
            "module_declaration" => {
                emit_namespace(child, graph, parent_id, &scope, ctx);
            }
            "ambient_declaration" => {
                if let Some(module) = child.named_child(0) {
                    if module.kind() == "module_declaration" {
                        emit_namespace(module, graph, parent_id, &scope, ctx);
                    }
                }
            }
            "method_definition" => {
                emit_method(child, graph, parent_id, &scope, ctx);
            }
            "property_definition" => {
                if let Some(value) = child.child_by_field_name("value") {
                    if matches!(value.kind(), "arrow_function" | "function_expression") {
                        if let Some(name) = child.child_by_field_name("name") {
                            let nm = text(name, ctx.source);
                            let qn = scoped_qname(ctx.file_qn, &scope, &nm);
                            let fn_id =
                                graph.add_node(Node::new(NodeKind::Method, qn).with_source(
                                    ctx.file_uri.to_string(),
                                    child.start_position().row as u32,
                                    child.end_position().row as u32,
                                ));
                            graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
                            if let Some(body) = value.child_by_field_name("body") {
                                emit_calls(body, ctx.source, graph, fn_id);
                            }
                        }
                    }
                }
            }
            "property_signature" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let nm = text(name_node, ctx.source);
                    let qn = scoped_qname(ctx.file_qn, &scope, &nm);
                    graph.add_node(
                        Node::new(NodeKind::Type, qn)
                            .with_source(
                                ctx.file_uri.to_string(),
                                child.start_position().row as u32,
                                child.end_position().row as u32,
                            )
                            .with_source_text(
                                super::super::extract_source_text(
                                    ctx.source,
                                    child.start_position().row as u32,
                                    child.end_position().row as u32,
                                )
                                .unwrap_or_default(),
                            ),
                    );
                }
            }
            "function_expression" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let nm = text(name, ctx.source);
                    let qn = scoped_qname(ctx.file_qn, &scope, &nm);
                    let fn_id = graph.add_node(
                        Node::new(NodeKind::Function, &qn)
                            .with_source(
                                ctx.file_uri.to_string(),
                                child.start_position().row as u32,
                                child.end_position().row as u32,
                            )
                            .with_source_text(
                                super::super::extract_source_text(
                                    ctx.source,
                                    child.start_position().row as u32,
                                    child.end_position().row as u32,
                                )
                                .unwrap_or_default(),
                            ),
                    );
                    graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
                    if let Some(body) = child.child_by_field_name("body") {
                        emit_calls(body, ctx.source, graph, fn_id);
                    }
                } else if let Some(body) = child.child_by_field_name("body") {
                    let line = child.start_position().row;
                    let qn = format!("file::{}::anonymous_fn_{}", ctx.file_uri, line);
                    let fn_id = graph.add_node(
                        Node::new(NodeKind::Function, &qn)
                            .with_source(
                                ctx.file_uri.to_string(),
                                line as u32,
                                child.end_position().row as u32,
                            )
                            .with_source_text(
                                super::super::extract_source_text(
                                    ctx.source,
                                    line as u32,
                                    child.end_position().row as u32,
                                )
                                .unwrap_or_default(),
                            ),
                    );
                    graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
                    emit_calls(body, ctx.source, graph, fn_id);
                }
            }
            "expression_statement" => {
                if let Some(expr) = child.child(0) {
                    emit_calls(expr, ctx.source, graph, parent_id);
                }
            }
            _ => {
                walk_scope(child, graph, parent_id, scope.clone(), ctx);
            }
        }
    }
}

/// Emit a declaration that may be wrapped in an `export` keyword.
fn emit_exported_declaration(
    decl: &tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    match decl.kind() {
        "class_declaration" => {
            emit_class(*decl, graph, parent_id, scope, ctx);
        }
        "interface_declaration" => {
            emit_interface(*decl, graph, parent_id, scope, ctx);
        }
        "type_alias_declaration" => {
            emit_type_alias(
                *decl,
                ctx.source,
                graph,
                ctx.file_uri,
                ctx.file_qn,
                parent_id,
                scope,
            );
        }
        "enum_declaration" => {
            emit_enum(
                *decl,
                ctx.source,
                graph,
                ctx.file_uri,
                ctx.file_qn,
                parent_id,
                scope,
            );
        }
        "function_declaration" => {
            emit_fn(*decl, graph, parent_id, scope, ctx);
        }
        "variable_declaration" | "lexical_declaration" => {
            emit_var_functions(*decl, graph, parent_id, scope, ctx);
        }
        _ => {
            walk_scope(*decl, graph, parent_id, scope.to_vec(), ctx);
        }
    }
}

fn emit_class(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    let nm = match node.child_by_field_name("name") {
        Some(n) => text(n, ctx.source),
        None => return,
    };
    let class_id = graph.add_node(
        Node::new(NodeKind::Class, scoped_qname(ctx.file_qn, scope, &nm))
            .with_source(
                ctx.file_uri.to_string(),
                node.start_position().row as u32,
                node.end_position().row as u32,
            )
            .with_source_text(
                super::super::extract_source_text(
                    ctx.source,
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                )
                .unwrap_or_default(),
            ),
    );
    graph.add_edge(parent_id, class_id, Edge::extracted(EdgeKind::Defines));
    emit_ts_bases(node, ctx.source, graph, class_id);
}

fn emit_interface(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    let nm = match node.child_by_field_name("name") {
        Some(n) => text(n, ctx.source),
        None => return,
    };
    let mut child_scope = scope.to_vec();
    child_scope.push(nm.clone());
    let iface_id = graph.add_node(
        Node::new(NodeKind::Class, scoped_qname(ctx.file_qn, scope, &nm))
            .with_source(
                ctx.file_uri.to_string(),
                node.start_position().row as u32,
                node.end_position().row as u32,
            )
            .with_source_text(
                super::super::extract_source_text(
                    ctx.source,
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                )
                .unwrap_or_default(),
            ),
    );
    graph.add_edge(parent_id, iface_id, Edge::extracted(EdgeKind::Defines));
    emit_ts_bases(node, ctx.source, graph, iface_id);
    if let Some(body) = node.child_by_field_name("body") {
        walk_scope(body, graph, iface_id, child_scope, ctx);
    }
}

fn emit_type_alias(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    file_uri: &str,
    file_qn: &str,
    parent_id: NodeId,
    scope: &[String],
) {
    if let Some(name) = node.child_by_field_name("name") {
        let nm = text(name, source);
        let qn = scoped_qname(file_qn, scope, &nm);
        let type_id = graph.add_node(
            Node::new(NodeKind::Type, qn)
                .with_source(
                    file_uri.to_string(),
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                )
                .with_source_text(
                    super::super::extract_source_text(
                        source,
                        node.start_position().row as u32,
                        node.end_position().row as u32,
                    )
                    .unwrap_or_default(),
                ),
        );
        graph.add_edge(parent_id, type_id, Edge::extracted(EdgeKind::Defines));
    }
}

fn emit_enum(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    file_uri: &str,
    file_qn: &str,
    parent_id: NodeId,
    scope: &[String],
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let nm = text(name_node, source);
        let qn = scoped_qname(file_qn, scope, &nm);
        let enum_id = graph.add_node(
            Node::new(NodeKind::Type, qn)
                .with_source(
                    file_uri.to_string(),
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                )
                .with_source_text(
                    super::super::extract_source_text(
                        source,
                        node.start_position().row as u32,
                        node.end_position().row as u32,
                    )
                    .unwrap_or_default(),
                ),
        );
        graph.add_edge(parent_id, enum_id, Edge::extracted(EdgeKind::Defines));
    }
}

fn emit_fn(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    let nm = match node.child_by_field_name("name") {
        Some(n) => text(n, ctx.source),
        None => return,
    };
    let is_test = ctx.file_is_test || is_test_name(nm.rsplit('.').next().unwrap_or(&nm));
    let qn = scoped_qname(ctx.file_qn, scope, &nm);
    let mut fn_node = Node::new(NodeKind::Function, qn).with_source(
        ctx.file_uri.to_string(),
        node.start_position().row as u32,
        node.end_position().row as u32,
    );
    fn_node = fn_node.with_source_text(
        super::super::extract_source_text(
            ctx.source,
            node.start_position().row as u32,
            node.end_position().row as u32,
        )
        .unwrap_or_default(),
    );
    if is_test {
        fn_node = fn_node.with_property("is_test", serde_json::Value::Bool(true));
    }
    let fn_id = graph.add_node(fn_node);
    graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
    if let Some(body) = node.child_by_field_name("body") {
        emit_calls(body, ctx.source, graph, fn_id);
    }
}

fn emit_var_functions(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    // Collect variable declarators: either from 'declarations' field
    // or as unnamed direct children (TS grammar variant).
    let mut declarators: Vec<tree_sitter::Node> = Vec::new();
    if let Some(declarations) = node.child_by_field_name("declarations") {
        declarators.extend(children(declarations));
    }
    for child in children(node) {
        if child.kind() == "variable_declarator" {
            declarators.push(child);
        }
    }
    if declarators.is_empty() {
        return;
    }
    for vdecl in declarators {
        if let Some(name) = vdecl.child_by_field_name("name") {
            let nm = text(name, ctx.source);
            let init = vdecl.child_by_field_name("value");
            let kind = if init
                .as_ref()
                .map(|v| matches!(v.kind(), "arrow_function" | "function_expression"))
                .unwrap_or(false)
            {
                NodeKind::Function
            } else {
                continue;
            };
            let is_test = ctx.file_is_test || is_test_name(nm.rsplit('.').next().unwrap_or(&nm));
            let qn = scoped_qname(ctx.file_qn, scope, &nm);
            let mut n = Node::new(kind, qn)
                .with_source(
                    ctx.file_uri.to_string(),
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                )
                .with_source_text(
                    super::super::extract_source_text(
                        ctx.source,
                        node.start_position().row as u32,
                        node.end_position().row as u32,
                    )
                    .unwrap_or_default(),
                );
            if is_test {
                n = n.with_property("is_test", serde_json::Value::Bool(true));
            }
            let fn_id = graph.add_node(n);
            graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
            if let Some(body) = init.and_then(|v| v.child_by_field_name("body")) {
                emit_calls(body, ctx.source, graph, fn_id);
            }
        }
    }
}

fn emit_method(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    let nm = match node.child_by_field_name("name") {
        Some(n) => text(n, ctx.source),
        None => return,
    };
    let is_test = ctx.file_is_test || is_test_name(nm.rsplit('.').next().unwrap_or(&nm));
    let qn = scoped_qname(ctx.file_qn, scope, &nm);
    let mut n = Node::new(NodeKind::Method, qn)
        .with_source(
            ctx.file_uri.to_string(),
            node.start_position().row as u32,
            node.end_position().row as u32,
        )
        .with_source_text(
            super::super::extract_source_text(
                ctx.source,
                node.start_position().row as u32,
                node.end_position().row as u32,
            )
            .unwrap_or_default(),
        );
    if is_test {
        n = n.with_property("is_test", serde_json::Value::Bool(true));
    }
    let fn_id = graph.add_node(n);
    graph.add_edge(parent_id, fn_id, Edge::extracted(EdgeKind::Defines));
    if let Some(body) = node.child_by_field_name("body") {
        emit_calls(body, ctx.source, graph, fn_id);
    }
}

fn emit_namespace(
    node: tree_sitter::Node,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
    scope: &[String],
    ctx: &TsContext<'_>,
) {
    let nm = match node.child_by_field_name("name") {
        Some(n) => text(n, ctx.source),
        None => return,
    };
    let mut child_scope = scope.to_vec();
    child_scope.push(nm.clone());
    let ns_id = graph.add_node(Node::new(
        NodeKind::Module,
        scoped_qname(ctx.file_qn, scope, &nm),
    ));
    graph.add_edge(parent_id, ns_id, Edge::extracted(EdgeKind::Defines));
    if let Some(body) = node.child_by_field_name("body") {
        walk_scope(body, graph, ns_id, child_scope, ctx);
    }
}

fn emit_import_statement(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
) {
    if let Some(src) = node.child_by_field_name("source") {
        let module_name = text(src, source)
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();

        if !module_name.is_empty() {
            let mod_id = graph.add_node(Node::new(
                NodeKind::Module,
                format!("module::{}", module_name),
            ));
            graph.add_edge(parent_id, mod_id, Edge::extracted(EdgeKind::Imports));

            if let Some(specifiers) = node.child_by_field_name("specifiers") {
                for spec in children(specifiers) {
                    match spec.kind() {
                        "import_clause" => {
                            if let Some(name) = spec.child_by_field_name("name") {
                                let import_name = text(name, source);
                                let sym_id = graph.add_node(Node::new(
                                    NodeKind::Type,
                                    format!("import::{}", import_name),
                                ));
                                graph.add_edge(
                                    parent_id,
                                    sym_id,
                                    Edge::extracted(EdgeKind::Defines),
                                );
                            }
                        }
                        "named_imports" => {
                            for imp in children(spec) {
                                if let Some(name) = imp.child_by_field_name("name") {
                                    let import_name = text(name, source);
                                    let sym_id = graph.add_node(Node::new(
                                        NodeKind::Type,
                                        format!("import::{}", import_name),
                                    ));
                                    graph.add_edge(
                                        parent_id,
                                        sym_id,
                                        Edge::extracted(EdgeKind::Defines),
                                    );
                                }
                            }
                        }
                        "import_specifier" => {
                            if let Some(name) = spec.child_by_field_name("name") {
                                let import_name = text(name, source);
                                let sym_id = graph.add_node(Node::new(
                                    NodeKind::Type,
                                    format!("import::{}", import_name),
                                ));
                                graph.add_edge(
                                    parent_id,
                                    sym_id,
                                    Edge::extracted(EdgeKind::Defines),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn emit_re_exports(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    parent_id: NodeId,
) {
    if let Some(src) = node.child_by_field_name("source") {
        let module_name = text(src, source)
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if !module_name.is_empty() {
            let mod_id = graph.add_node(Node::new(
                NodeKind::Module,
                format!("export::{}", module_name),
            ));
            graph.add_edge(parent_id, mod_id, Edge::extracted(EdgeKind::Defines));
        }
    }
}

fn emit_ts_bases(
    node: tree_sitter::Node,
    source: &str,
    graph: &mut dyn GraphMut,
    class_id: NodeId,
) {
    let mut to_visit = children(node);
    while let Some(n) = to_visit.pop() {
        if matches!(n.kind(), "extends_clause") {
            for child in children(n) {
                if let Some(name) = type_name(child, source) {
                    let base_id =
                        graph.add_node(Node::new(NodeKind::Class, format!("type::{}", name)));
                    graph.add_edge(class_id, base_id, Edge::extracted(EdgeKind::Inherits));
                }
            }
        }
        to_visit.extend(children(n));
    }
}

fn emit_calls(node: tree_sitter::Node, source: &str, graph: &mut dyn GraphMut, caller: NodeId) {
    let mut to_visit = children(node);
    while let Some(n) = to_visit.pop() {
        if matches!(
            n.kind(),
            "function_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "method_definition"
        ) {
            continue;
        }
        if matches!(n.kind(), "call_expression" | "new_expression") {
            if let Some(func) = call_expr_function(n) {
                if let Some(name) = call_target_name(func, source) {
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

fn call_expr_function(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    match node.kind() {
        "call_expression" | "new_expression" => node.child_by_field_name("function"),
        _ => None,
    }
}

fn call_target_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, source)),
        "qualified_name" => Some(text(node, source).rsplit('.').next()?.to_string()),
        "member_expression" => node
            .child_by_field_name("property")
            .map(|p| text(p, source)),
        _ => find_descendant(node, "identifier")
            .or_else(|| find_descendant(node, "qualified_name"))
            .map(|n| text(n, source)),
    }
}

fn type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "qualified_name" => Some(text(node, source)),
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

fn scoped_qname(file_qn: &str, scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        format!("{}::{}", file_qn, name)
    } else {
        format!("{}::{}::{}", file_qn, scope.join("::"), name)
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
