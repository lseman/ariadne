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
use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use crate::extract::test_detect::{is_test_file_path, is_test_name};
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
    let file_is_test = is_test_file_path(path);
    let file_id = graph.add_node(Node::new(NodeKind::File, &file_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    // Pre-compute the byte ranges of `#[cfg(test)] mod …` blocks so any
    // function landing inside one gets is_test=true automatically.
    let test_mod_ranges = find_cfg_test_mod_ranges(tree.root_node(), &source);

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
            // Method iff the function is the direct child of an impl or
            // trait body — not just any non-empty scope. A fn defined
            // inside `mod foo` or another fn is still a free function.
            let kind = if def_node.map(has_method_parent).unwrap_or(false) {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            let is_test = file_is_test
                || is_test_name(&n)
                || def_node
                    .map(|fn_node| {
                        has_test_attribute(fn_node, &source)
                            || in_any_range(fn_node, &test_mod_ranges)
                    })
                    .unwrap_or(false);
            let mut node = Node::new(kind, &qn).with_source(file_uri.clone(), start, end);
            if is_test {
                node = node.with_property("is_test", serde_json::Value::Bool(true));
            }
            let id = graph.add_node(node);
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
        // Don't descend into nested function or closure bodies: their
        // calls belong to the nested item's caller node, not to ours.
        // The `fn_with_calls` query in extract_file will visit each
        // `function_item` independently.
        if matches!(n.kind(), "function_item" | "closure_expression") {
            continue;
        }
        if n.kind() == "call_expression" {
            if let Some(func_node) = n.child_by_field_name("function") {
                let name = call_target_name(func_node, source);
                if let Some(name) = name {
                    add_ambiguous_call(graph, caller, &name);
                }
            }
        } else if n.kind() == "macro_invocation" {
            // Tree-sitter-rust does not parse inside macro bodies; the
            // contents come back as raw `token_tree` nodes. Without
            // this, the pervasive `assert!(foo())`, `info!("...", x())`,
            // `dbg!(x())` calls produce no edges at all. We scan the
            // token tree for `identifier(` shapes and emit ambiguous
            // call edges — the post-extraction resolver still has a
            // chance to upgrade them if the name is unique.
            emit_macro_calls(n, source, graph, caller);
            // Don't descend into the macro further; everything is raw.
            continue;
        }
        let mut w = n.walk();
        for child in n.children(&mut w) {
            to_visit.push(child);
        }
    }
}

/// Walk a `macro_invocation` node's raw token tree and emit ambiguous
/// `Calls` edges for any `identifier` directly followed by a `(`-led
/// `token_tree`. Misses method-call chains (`x.foo()`) because the dot
/// is inside the token tree; that's fine — we only need to catch the
/// common cases `name(...)` and `path::name(...)`.
fn emit_macro_calls(
    macro_node: tree_sitter::Node,
    source: &str,
    graph: &mut Graph,
    caller: NodeId,
) {
    // Walk every descendant token_tree; for each, scan its direct
    // children for the `identifier` then `token_tree`(starts with `(`)
    // shape. We descend into nested token_trees so calls inside
    // arguments of other calls are also visible.
    let mut stack = vec![macro_node];
    while let Some(node) = stack.pop() {
        if node.kind() == "token_tree" {
            let kids = children(node);
            for i in 0..kids.len() {
                if kids[i].kind() != "identifier" {
                    continue;
                }
                let Some(next) = kids.get(i + 1) else { continue };
                if next.kind() != "token_tree" {
                    continue;
                }
                // token_tree's first child is the opening delimiter;
                // we only want `(` calls, not `[ ]` (vec!, etc.) or `{ }`.
                let opener = children(*next)
                    .first()
                    .map(|c| c.kind().to_string())
                    .unwrap_or_default();
                if opener != "(" {
                    continue;
                }
                if let Ok(name_text) = kids[i].utf8_text(source.as_bytes()) {
                    // Skip known control-flow keywords that can appear
                    // as identifiers in token trees (`return`, `if`,
                    // `let`, `match`, `for`, `while`).
                    if matches!(
                        name_text,
                        "return"
                            | "if"
                            | "else"
                            | "let"
                            | "match"
                            | "for"
                            | "while"
                            | "loop"
                            | "in"
                            | "mut"
                            | "ref"
                            | "as"
                            | "move"
                    ) {
                        continue;
                    }
                    add_ambiguous_call(graph, caller, name_text);
                }
            }
        }
        let mut w = node.walk();
        for child in node.children(&mut w) {
            stack.push(child);
        }
    }
}

fn add_ambiguous_call(graph: &mut Graph, caller: NodeId, name: &str) {
    let callee_qn = format!("call::{}", name);
    let callee_id = graph.add_node(Node::new(NodeKind::Function, &callee_qn));
    graph.add_edge(caller, callee_id, Edge::ambiguous(EdgeKind::Calls));
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

/// True iff this `function_item` is defined directly inside an
/// `impl`/`trait` block — i.e. it's a method, not a free function. We
/// walk up only one level of `declaration_list`/`impl_item`/`trait_item`
/// containment so a free function inside `impl Foo { fn outer() { fn
/// inner() {} } }` doesn't get mistaken for a method.
fn has_method_parent(node: tree_sitter::Node) -> bool {
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        match parent.kind() {
            "impl_item" | "trait_item" => return true,
            // Methods are immediately inside the impl/trait's
            // declaration_list. Anything else (block, function_item,
            // mod_item, ...) means we've left the impl scope.
            "declaration_list" => {
                cur = parent;
                continue;
            }
            _ => return false,
        }
    }
    false
}

fn rust_scope(mut node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut scope = Vec::new();
    // Track the node we started at so we don't add it as its own scope
    // segment (the outermost `function_item` is the function being named,
    // not its enclosing context).
    let original_id = node.id();
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
            // A function defined inside another function's body
            // (`fn outer() { fn helper() {} }`) should be qualified by
            // its enclosing function so its qname stays unique and
            // doesn't collide with top-level helpers of the same name.
            "function_item" if parent.id() != original_id => {
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

/// Find byte ranges of `mod` items annotated with `#[cfg(test)]`. Any
/// function whose byte range falls inside one of these is a test. We
/// walk the whole tree once and collect; nested test mods are fine
/// because we check containment, not exact match.
///
/// Tree-sitter-rust models `#[cfg(test)]` as a *preceding sibling*
/// `attribute_item` of `mod_item`, not as a child — so we have to look
/// at the mod's parent slot, not the mod's own children.
fn find_cfg_test_mod_ranges(root: tree_sitter::Node, source: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "mod_item" && preceding_marks_cfg_test(node, source) {
            ranges.push((node.start_byte(), node.end_byte()));
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    ranges
}

/// True iff `node` is preceded by `attribute_item` siblings that include
/// `#[test]`, `#[tokio::test]`, `#[rstest]`, `#[test_case(...)]`, etc.
///
/// Tree-sitter-rust places outer attributes as `attribute_item` siblings
/// before the item they decorate, all under the same parent (a
/// `source_file` at top level, or a `declaration_list` inside a `mod`).
/// Inner attributes (`#![...]`) don't apply to a specific item and are
/// ignored here.
fn has_test_attribute(node: tree_sitter::Node, source: &str) -> bool {
    preceding_attribute_items(node)
        .into_iter()
        .any(|attr| attribute_marks_test(&text(attr, source)))
}

/// True iff `mod_item` `node` is preceded by an `attribute_item` for
/// `#[cfg(test)]`.
fn preceding_marks_cfg_test(node: tree_sitter::Node, source: &str) -> bool {
    preceding_attribute_items(node).into_iter().any(|attr| {
        let body = text(attr, source);
        body.contains("cfg(test)") || body.contains("cfg ( test )")
    })
}

/// Collect the contiguous run of `attribute_item` siblings immediately
/// preceding `node` in its parent's child list. Returns them in source
/// order (oldest first), though order doesn't matter for our callers.
fn preceding_attribute_items(node: tree_sitter::Node) -> Vec<tree_sitter::Node> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let mut cursor = parent.walk();
    let siblings: Vec<tree_sitter::Node> = parent.children(&mut cursor).collect();
    let Some(idx) = siblings.iter().position(|c| c.id() == node.id()) else {
        return Vec::new();
    };
    let mut attrs = Vec::new();
    for sib in siblings[..idx].iter().rev() {
        if sib.kind() == "attribute_item" {
            attrs.push(*sib);
        } else if sib.is_named() {
            // Hit a non-attribute named sibling — stop scanning back.
            break;
        }
    }
    attrs.reverse();
    attrs
}

/// Recognise `#[test]`, `#[tokio::test]`, `#[async_std::test]`,
/// `#[rstest]`, `#[test_case(...)]`, `#[should_panic]`-on-test-mod
/// patterns, etc. The naked check is "an identifier `test` appears as the
/// terminal segment of the attribute path."
fn attribute_marks_test(raw: &str) -> bool {
    // Strip `#[...]` wrapper.
    let inner = raw
        .trim()
        .strip_prefix("#[")
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw)
        .trim();
    // Take the head identifier (before any `(`).
    let head = inner.split('(').next().unwrap_or(inner).trim();
    if head == "test" || head == "rstest" {
        return true;
    }
    // Path attributes like `tokio::test`, `async_std::test`,
    // `test_case`, `test_log::test`.
    if head.ends_with("::test") || head == "test_case" {
        return true;
    }
    // `tokio::test(flavor = "current_thread")` covered by the head check
    // above. Nothing else needs special-casing.
    false
}

/// True if a tree-sitter node's byte range is contained in any of
/// `ranges`.
fn in_any_range(node: tree_sitter::Node, ranges: &[(usize, usize)]) -> bool {
    let (s, e) = (node.start_byte(), node.end_byte());
    ranges.iter().any(|&(rs, re)| rs <= s && e <= re)
}
