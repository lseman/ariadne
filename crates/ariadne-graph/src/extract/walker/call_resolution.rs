//! Call-placeholder resolution: a 6-tier name-resolution heuristic engine
//! that turns `call::name` placeholder edges (emitted by the AST
//! extractors when a call target can't be resolved locally) into real
//! `Calls` edges pointing at a specific function/method definition.

use super::suppress_list::should_suppress_call_placeholder;
use crate::core::{Edge, EdgeKind, GraphMut, NodeKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Length of the common prefix shared by two qualified names, counted in
/// `::` segments (not raw bytes) so a 3-segment common prefix always beats
/// a 2-segment one regardless of string length.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.split("::")
        .zip(b.split("::"))
        .take_while(|(x, y)| x == y)
        .count()
}

/// The module name a source file answers to in import paths: its stem,
/// or the parent directory for container files (`mod.rs`, `index.ts`,
/// `__init__.py`, …).
fn module_stem(uri: &str) -> Option<String> {
    let path = Path::new(uri);
    let stem = path.file_stem()?.to_str()?;
    if matches!(stem, "mod" | "index" | "__init__" | "lib" | "main") {
        path.parent()?.file_name()?.to_str().map(|s| s.to_string())
    } else {
        Some(stem.to_string())
    }
}

fn build_by_name(graph: &dyn crate::core::GraphMut) -> HashMap<String, Vec<crate::core::NodeId>> {
    let mut by_name: HashMap<String, Vec<_>> = HashMap::new();
    for (id, node) in graph.nodes() {
        if matches!(
            node.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Type
        ) && !node.qualified_name.starts_with("call::")
        {
            by_name.entry(node.name.clone()).or_default().push(id);
        }
    }
    by_name
}

/// Build a map from caller NodeId → impl type string.
///
/// For Method nodes (e.g. `Graph::add_node`), the impl type is extracted
/// from the qualified name. When resolving `graph.add_node()`, we look up
/// the caller's impl type — if caller is `Graph::some_method`, then
/// `self.add_node()` → `Graph::add_node`.
fn build_caller_impl_context(
    graph: &dyn crate::core::GraphMut,
) -> HashMap<crate::core::NodeId, String> {
    let mut context: HashMap<crate::core::NodeId, String> = HashMap::new();

    for (id, node) in graph.nodes() {
        if matches!(node.kind, crate::core::NodeKind::Method) {
            // Method qname: `file::./crates/ariadne-graph/src/core/graph.rs::Graph::add_node`
            // Parts: ["file", "./crates/...", "graph.rs", "Graph", "add_node"]
            // The impl type is the component right before the method name.
            let qn = &node.qualified_name;
            let parts: Vec<&str> = qn.split("::").collect();
            if parts.len() >= 2 {
                // Find the method name (last part), then the impl type is the
                // last non-trivial part before it.
                let _method_name = parts.last().unwrap();
                let mut impl_type = None;
                for part in parts.iter().rev().skip(1) {
                    // Skip file path components: "file", empty, paths starting with "." or "/", .rs files
                    if *part == "file" || part.is_empty() {
                        continue;
                    }
                    if part.starts_with("./") || part.starts_with("/") || part.ends_with(".rs") {
                        continue;
                    }
                    // The first non-trivial part before the method name is the impl type
                    impl_type = Some(part.to_string());
                    break;
                }
                if let Some(ty) = impl_type {
                    context.insert(id, ty);
                }
            }
        }
    }

    context
}

/// Scan source text for let-binding type annotations and constructor calls,
/// returning the type of `var_name` if found.
///
/// Recognises:
/// - `let var: TypeName` / `let mut var: TypeName`
/// - `let var = TypeName::new(` / `let var = TypeName::`
/// - `let var = TypeName {`  (struct literal)
///
/// Returns only the final type identifier (no generics, no path prefix).
fn infer_type_from_let_bindings(source: &str, var_name: &str) -> Option<String> {
    // Scan each `let` statement in the source for the variable name.
    // Works on both multi-line and single-line (inline) function bodies.
    //
    // Recognises:
    //   let [mut] var: Type ...
    //   let [mut] var = Type::  (constructor / associated fn)
    //   let [mut] var = Type {  (struct literal)
    for let_pos in source.match_indices("let ").map(|(i, _)| i) {
        let slice = &source[let_pos + 4..]; // skip "let "
        let slice = slice.trim_start_matches("mut").trim_start();
        // Must start with the variable name.
        if !slice.starts_with(var_name) {
            continue;
        }
        let after_name = slice[var_name.len()..].trim_start();
        // Boundary check: next char must be `:`, `=`, ` `, or `;`.
        let boundary = after_name
            .chars()
            .next()
            .map(|c| matches!(c, ':' | '=' | ';' | ' ' | '\n' | '\r'))
            .unwrap_or(false);
        if !boundary {
            continue;
        }
        // `let var: Type` — explicit annotation.
        if let Some(type_part) = after_name.strip_prefix(':') {
            let type_part = type_part.trim_start();
            let ty: String = type_part
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !ty.is_empty() && ty.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return Some(ty);
            }
        }
        // `let var = …`
        if let Some(rhs) = after_name.strip_prefix('=') {
            let rhs = rhs.trim_start();
            let rhs = rhs
                .trim_start_matches('&')
                .trim_start_matches("mut")
                .trim_start();
            // `let var = Type::` — constructor or associated fn.
            if let Some(idx) = rhs.find("::") {
                let ty: String = rhs[..idx]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !ty.is_empty() && ty.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Some(ty);
                }
            }
            // `let var = Type {` — struct literal.
            if let Some(idx) = rhs.find('{') {
                let ty: String = rhs[..idx]
                    .trim()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !ty.is_empty() && ty.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Some(ty);
                }
            }
        }
    }
    None
}

/// Infer an impl type from a variable name.
/// Maps common variable names to their likely types.
fn infer_type_from_var_name(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        // Graph types
        "graph" | "main_graph" | "app_graph" => Some("Graph".to_string()),
        "g" => Some("Graph".to_string()),
        "motif" | "mb" | "motif_builder" => Some("MotifBuilder".to_string()),
        // Query types
        "store" | "db" | "database" => Some("Store".to_string()),
        "parser" | "ts_parser" => Some("Parser".to_string()),
        // Other common types
        "config" | "cfg" => Some("Config".to_string()),
        "ctx" | "context" => Some("Context".to_string()),
        "options" | "opts" => Some("Options".to_string()),
        _ => None,
    }
}

// Identifier tokens from each file's import paths (`use crate::auth;`
// → {crate, auth}, `from pkg.auth import login` → {pkg, auth},
// `import './auth'` → {auth}), used by Tier 4 to prefer candidates
// whose module the caller's file actually imports.
fn build_import_tokens(graph: &dyn crate::core::GraphMut) -> HashMap<String, HashSet<String>> {
    let mut import_tokens: HashMap<String, HashSet<String>> = HashMap::new();
    for (_, src, dst, edge) in graph.edges() {
        if edge.kind != EdgeKind::Imports {
            continue;
        }
        let (Some(file), Some(module)) = (graph.node(src), graph.node(dst)) else {
            continue;
        };
        let Some(uri) = file.source_uri.as_ref() else {
            continue;
        };
        let Some(path) = module.qualified_name.strip_prefix("module::") else {
            continue;
        };
        let tokens = import_tokens.entry(uri.clone()).or_default();
        for token in path.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if !token.is_empty() {
                // Lowercased so `use foo::Graph` matches graph.rs — type
                // names are typically the CamelCase of their module stem.
                tokens.insert(token.to_ascii_lowercase());
            }
        }
    }
    import_tokens
}

pub fn resolve_call_placeholders(graph: &mut dyn GraphMut) -> usize {
    let by_name = build_by_name(graph);
    let import_tokens = build_import_tokens(graph);
    // Build caller→impl_type map: for each function, determine which impl
    // block it belongs to. If caller is `Graph::some_method`, then receivers
    // in that function resolve to `Graph`. E.g. `self.add_node()` → `Graph::add_node`.
    let caller_impl_context = build_caller_impl_context(graph);

    let existing: HashSet<_> = graph
        .edges()
        .filter(|(_, _, _, edge)| edge.kind == EdgeKind::Calls)
        .map(|(_, src, dst, _)| (src, dst))
        .collect();
    // Resolution outcome tag, used as a property on the new edge so we
    // can tell file-local matches from globally-unique ones during
    // analysis. A `false` confidence flag marks tiers that are inferred
    // rather than structural, so queries can still filter them out.
    let mut additions: Vec<(_, _, &'static str, bool)> = Vec::new();
    // Placeholder edges made redundant by a resolution; removed after
    // the additions land so the same call is not counted as both
    // resolved and unresolved.
    let mut stale_edges: Vec<crate::core::EdgeId> = Vec::new();

    for (edge_id, src, dst, edge) in graph.edges() {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        let Some(callee) = graph.node(dst) else {
            continue;
        };
        let Some(name) = callee.qualified_name.strip_prefix("call::") else {
            continue;
        };
        if should_suppress_call_placeholder(name) {
            continue;
        }
        let Some(candidates) = by_name.get(name) else {
            continue;
        };

        // Tier 1: exactly one candidate in the whole graph.
        if candidates.len() == 1 {
            stale_edges.push(edge_id);
            if !existing.contains(&(src, candidates[0])) {
                additions.push((src, candidates[0], "unique_name", true));
            }
            continue;
        }

        // Tier 2: multiple candidates, but exactly one in the caller's
        // own file. Common case for per-file helpers (`scoped_qname`,
        // `walk_scope`, …) defined in several language extractors.
        let src_file = graph.node(src).and_then(|n| n.source_uri.clone());
        if let Some(src_file) = src_file.as_ref() {
            let local: Vec<_> = candidates
                .iter()
                .filter(|&&cand| {
                    graph
                        .node(cand)
                        .and_then(|n| n.source_uri.as_ref())
                        .map(|uri| uri == src_file)
                        .unwrap_or(false)
                })
                .copied()
                .collect();
            if local.len() == 1 {
                stale_edges.push(edge_id);
                if !existing.contains(&(src, local[0])) {
                    additions.push((src, local[0], "file_local", true));
                }
                continue;
            }
        }

        // Tier 3: a path-qualified call (`module::path::name`) carried its
        // scope onto the placeholder edge. Prefer the unique candidate
        // whose qualified name contains that scope. Inferred, not
        // structural — the match is by name fragment, not full resolution.
        //
        // Tier 3b: when multiple candidates match the scope, pick the one
        // whose qualified name shares the longest common prefix with the
        // caller's qualified name (same module subtree wins).
        if let Some(scope) = edge.properties.get("call_scope").and_then(|v| v.as_str()) {
            let scoped: Vec<_> = candidates
                .iter()
                .filter(|&&cand| {
                    graph
                        .node(cand)
                        .map(|n| n.qualified_name.contains(scope))
                        .unwrap_or(false)
                })
                .copied()
                .collect();
            if scoped.len() == 1 {
                stale_edges.push(edge_id);
                if !existing.contains(&(src, scoped[0])) {
                    additions.push((src, scoped[0], "scoped", false));
                }
                continue;
            }
            if scoped.len() > 1 {
                let caller_qn = graph
                    .node(src)
                    .map(|n| n.qualified_name.as_str())
                    .unwrap_or("");
                let best = scoped.iter().copied().max_by_key(|&cand| {
                    graph
                        .node(cand)
                        .map(|n| common_prefix_len(caller_qn, &n.qualified_name))
                        .unwrap_or(0)
                });
                if let Some(cand) = best {
                    stale_edges.push(edge_id);
                    if !existing.contains(&(src, cand)) {
                        additions.push((src, cand, "scoped_prefix", false));
                    }
                    continue;
                }
            }
        }

        // Tier 3.5: receiver-based disambiguation. For method calls like
        // `self.add_node()` or `graph.add_node()`, the Rust extractor captured
        // `call_receiver` on the edge. We look up the caller's impl context:
        //   - If caller is `Graph::some_method` and receiver is `self` → `Graph::add_node`
        //   - If receiver name hints at a type (e.g. `graph` → `Graph`) → narrow by that type
        //
        // Tier 3.5+: for non-self receivers, try AST-derived let-binding scan
        // before falling back to the hardcoded name map.
        if let Some(receiver_name) = edge
            .properties
            .get("call_receiver")
            .and_then(|v| v.as_str())
        {
            // Determine the impl type from the receiver and caller context.
            let impl_type: Option<String> = if receiver_name == "self"
                || receiver_name.starts_with("self.")
            {
                caller_impl_context.get(&src).cloned()
            } else {
                // First try AST-derived bindings from the caller's source file.
                let ast_inferred = graph
                    .node(src)
                    .and_then(|n| n.source_uri.as_ref())
                    .and_then(|uri| std::fs::read_to_string(uri).ok())
                    .and_then(|src_text| infer_type_from_let_bindings(&src_text, receiver_name));
                // Fall back to the heuristic name map.
                ast_inferred.or_else(|| infer_type_from_var_name(receiver_name))
            };

            if let Some(impl_type) = impl_type {
                // impl_type is like "Graph" or "MotifBuilder" — narrow candidates
                // to those whose qualified name ends with `::ImplType::method`.
                // We check that the impl type appears right before the method name
                // (not just anywhere in the qualified name, which would match
                // `GraphMut` when looking for `Graph`).
                let receiver_candidates: Vec<_> = candidates
                    .iter()
                    .copied()
                    .filter(|&cand| {
                        graph
                            .node(cand)
                            .map(|n| {
                                let qn = &n.qualified_name;
                                qn.rsplit("::").skip(1).any(|part| part == impl_type)
                            })
                            .unwrap_or(false)
                    })
                    .collect();
                if receiver_candidates.len() == 1 {
                    stale_edges.push(edge_id);
                    let cand = receiver_candidates[0];
                    if !existing.contains(&(src, cand)) {
                        additions.push((src, cand, "receiver", false));
                    }
                    continue;
                }
            }
        }

        // Tier 4: exactly one candidate lives in a module the caller's
        // file imports (matched by file stem against import-path
        // tokens). Inferred — import-path → file mapping is heuristic.
        if let Some(src_file) = src_file.as_ref() {
            if let Some(tokens) = import_tokens.get(src_file.as_str()) {
                let imported: Vec<_> = candidates
                    .iter()
                    .filter(|&&cand| {
                        graph
                            .node(cand)
                            .and_then(|n| n.source_uri.as_deref())
                            .and_then(module_stem)
                            .map(|stem| tokens.contains(&stem.to_ascii_lowercase()))
                            .unwrap_or(false)
                    })
                    .copied()
                    .collect();
                if imported.len() == 1 {
                    stale_edges.push(edge_id);
                    if !existing.contains(&(src, imported[0])) {
                        additions.push((src, imported[0], "import_scoped", false));
                    }
                    continue;
                }
            }
        }

        // Tier 5: same-directory affinity. When multiple candidates survive,
        // prefer the one(s) whose source file lives in the same directory as
        // the caller. Sibling modules in a crate commonly call each other.
        if let Some(src_file) = src_file.as_ref() {
            let src_dir = Path::new(src_file.as_str()).parent();
            if let Some(src_dir) = src_dir {
                let same_dir: Vec<_> = candidates
                    .iter()
                    .filter(|&&cand| {
                        graph
                            .node(cand)
                            .and_then(|n| n.source_uri.as_deref())
                            .map(|uri| {
                                Path::new(uri)
                                    .parent()
                                    .map(|d| d == src_dir)
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    })
                    .copied()
                    .collect();
                if same_dir.len() == 1 {
                    stale_edges.push(edge_id);
                    if !existing.contains(&(src, same_dir[0])) {
                        additions.push((src, same_dir[0], "same_dir", false));
                    }
                    continue;
                }
            }
        }

        // Tier 6: call-frequency prior. As a last-resort tiebreaker, prefer
        // the candidate that already has the most resolved Calls in-edges.
        // Only fires when the winner has ≥1 existing call — a tie at zero
        // means no statistical signal and we leave the edge unresolved.
        {
            let scored: Vec<(crate::core::NodeId, usize)> = candidates
                .iter()
                .copied()
                .map(|cand| {
                    let in_calls = graph
                        .in_neighbors(cand)
                        .filter(|(_, edge)| edge.kind == EdgeKind::Calls)
                        .count();
                    (cand, in_calls)
                })
                .collect();
            let max_score = scored.iter().map(|(_, s)| *s).max().unwrap_or(0);
            if max_score > 0 {
                // Unique winner at the highest score.
                let winners: Vec<_> = scored
                    .iter()
                    .filter(|(_, s)| *s == max_score)
                    .map(|(id, _)| *id)
                    .collect();
                if winners.len() == 1 {
                    let cand = winners[0];
                    stale_edges.push(edge_id);
                    if !existing.contains(&(src, cand)) {
                        additions.push((src, cand, "freq_prior", false));
                    }
                }
            }
        }
    }

    let count = additions.len();
    for (src, dst, tag, structural) in additions {
        let mut edge = if structural {
            Edge::extracted(EdgeKind::Calls)
        } else {
            Edge::inferred(EdgeKind::Calls, 0.7)
        };
        edge.properties.insert(
            "resolved_from".into(),
            serde_json::Value::String(format!("call_placeholder::{}", tag)),
        );
        graph.add_edge(src, dst, edge);
    }

    // A resolved call must not also be counted as unresolved: drop the
    // redundant placeholder edges, then any `call::` nodes left with no
    // edges at all.
    graph.remove_edges_by_id(&stale_edges);
    if !stale_edges.is_empty() {
        let orphaned: Vec<_> = graph
            .nodes()
            .filter(|(id, n)| {
                n.qualified_name.starts_with("call::")
                    && graph.in_neighbors(*id).next().is_none()
                    && graph.out_neighbors(*id).next().is_none()
            })
            .map(|(id, _)| id)
            .collect();
        graph.remove_nodes_by_id(&orphaned);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Graph, GraphMut, Node, NodeKind};
    use crate::extract::walker::extract_directory;

    fn make_fn(graph: &mut dyn GraphMut, qname: &str) -> crate::core::NodeId {
        graph.add_node(Node::new(NodeKind::Function, qname))
    }

    #[test]
    fn resolver_uses_imports_to_disambiguate() {
        // Two files define `login`. The caller's file imports `pkg.auth`,
        // so the call must resolve to auth.py's copy. Neither Tier 1
        // (name not unique), Tier 2 (caller in a third file), nor Tier 3
        // (unqualified call, no scope) can decide.
        let mut g = Graph::new();
        let mut caller_file = Node::new(NodeKind::File, "file::main.py");
        caller_file.source_uri = Some("main.py".to_string());
        let caller_file = g.add_node(caller_file);
        let module = g.add_node(Node::new(NodeKind::Module, "module::pkg.auth"));
        g.add_edge(caller_file, module, Edge::extracted(EdgeKind::Imports));

        let mut caller = Node::new(NodeKind::Function, "main.py::entry");
        caller.source_uri = Some("main.py".to_string());
        let caller = g.add_node(caller);
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::login"));
        g.add_edge(caller, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let mut auth_login = Node::new(NodeKind::Function, "pkg/auth.py::login");
        auth_login.source_uri = Some("pkg/auth.py".to_string());
        let auth_login = g.add_node(auth_login);
        let mut billing_login = Node::new(NodeKind::Function, "pkg/billing.py::login");
        billing_login.source_uri = Some("pkg/billing.py".to_string());
        let billing_login = g.add_node(billing_login);

        let added = resolve_call_placeholders(&mut g);
        assert_eq!(added, 1);
        let resolved: Vec<_> = g
            .out_neighbors(caller)
            .filter(|(_, e)| e.kind == EdgeKind::Calls)
            .map(|(dst, _)| dst)
            .collect();
        assert!(
            resolved.contains(&auth_login) && !resolved.contains(&billing_login),
            "import of pkg.auth must pick auth.py::login (resolved={:?})",
            resolved
        );
        // The placeholder edge is redundant once resolved, and the
        // orphaned placeholder node goes with it.
        assert!(
            !resolved.contains(&placeholder),
            "redundant placeholder edge must be removed"
        );
        assert!(
            g.node(placeholder).is_none(),
            "orphaned placeholder node must be removed"
        );
    }

    #[test]
    fn module_stem_uses_parent_for_container_files() {
        assert_eq!(module_stem("src/auth.rs").as_deref(), Some("auth"));
        assert_eq!(module_stem("src/auth/mod.rs").as_deref(), Some("auth"));
        assert_eq!(module_stem("pkg/auth/__init__.py").as_deref(), Some("auth"));
        assert_eq!(module_stem("web/auth/index.ts").as_deref(), Some("auth"));
    }

    #[test]
    fn rust_use_declaration_scopes_unqualified_call() {
        // a.rs and b.rs both define `shared`. c.rs has `use crate::a::shared;`
        // and calls it unqualified — Tier 4 must pick a.rs's copy from the
        // use-declaration tokens.
        let dir = std::env::temp_dir().join(format!(
            "ariadne_import_scoped_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "pub fn shared() -> u32 { 1 }\n").unwrap();
        std::fs::write(dir.join("src/b.rs"), "pub fn shared() -> u32 { 2 }\n").unwrap();
        std::fs::write(
            dir.join("src/c.rs"),
            "use crate::a::shared;\npub fn entry() -> u32 { shared() }\n",
        )
        .unwrap();

        let mut graph = Graph::new();
        extract_directory(&dir, &mut graph).unwrap();

        let entry = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("/c.rs::entry"))
            .map(|(id, _)| id)
            .expect("entry must be extracted");
        let shared_a = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("/a.rs::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("a.rs::shared must be extracted");
        let shared_b = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("/b.rs::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("b.rs::shared must be extracted");

        let resolved: Vec<_> = graph
            .out_neighbors(entry)
            .filter(|(dst, e)| {
                e.kind == EdgeKind::Calls
                    && graph
                        .node(*dst)
                        .map(|n| !n.qualified_name.starts_with("call::"))
                        .unwrap_or(false)
            })
            .map(|(dst, _)| dst)
            .collect();
        assert!(
            resolved.contains(&shared_a) && !resolved.contains(&shared_b),
            "use crate::a::shared must resolve to a.rs (resolved={:?})",
            resolved
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolver_uses_call_scope_to_disambiguate() {
        // Two modules in one file each define `fn shared()`. A path call
        // `beta::shared()` from a third function must resolve to
        // `beta::shared`, not `alpha::shared`. Neither Tier 1 (name is not
        // unique) nor Tier 2 (all three live in the same file) can pick;
        // only the captured `call_scope` from the path resolves it.
        let dir = std::env::temp_dir().join(format!("ariadne_scoped_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            r#"
mod alpha { pub fn shared() -> u32 { 1 } }
mod beta { pub fn shared() -> u32 { 2 } }
pub fn entry() -> u32 { beta::shared() }
"#,
        )
        .unwrap();

        let mut graph = Graph::new();
        extract_directory(&dir, &mut graph).unwrap();

        let entry = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("::entry"))
            .map(|(id, _)| id)
            .expect("entry must be extracted");
        let beta_shared = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("::beta::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("beta::shared must be extracted");
        let alpha_shared = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("::alpha::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("alpha::shared must be extracted");

        let resolved: Vec<_> = graph
            .out_neighbors(entry)
            .filter(|(dst, e)| {
                e.kind == EdgeKind::Calls
                    && graph
                        .node(*dst)
                        .map(|n| !n.qualified_name.starts_with("call::"))
                        .unwrap_or(false)
            })
            .map(|(dst, _)| dst)
            .collect();
        assert!(
            resolved.contains(&beta_shared) && !resolved.contains(&alpha_shared),
            "scoped call beta::shared() must resolve to beta, not alpha (resolved={:?})",
            resolved
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolver_prefers_file_local_candidate() {
        // Two files each define `fn shared() { … }`. A call to
        // `shared()` from inside file_a must resolve to file_a's
        // copy, not file_b's. The pre-fix resolver bailed entirely
        // when more than one candidate existed.
        let dir = std::env::temp_dir().join(format!("ariadne_file_local_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/a.rs"),
            r#"
fn shared() -> u32 { 1 }
pub fn entry_a() -> u32 { shared() }
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("src/b.rs"),
            r#"
fn shared() -> u32 { 2 }
pub fn entry_b() -> u32 { shared() }
"#,
        )
        .unwrap();

        let mut graph = Graph::new();
        extract_directory(&dir, &mut graph).unwrap();

        let entry_a = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("/a.rs::entry_a"))
            .map(|(id, _)| id)
            .expect("entry_a must be extracted");
        let entry_b = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("/b.rs::entry_b"))
            .map(|(id, _)| id)
            .expect("entry_b must be extracted");
        let shared_a = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("/a.rs::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("a.rs::shared must be extracted");
        let shared_b = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("/b.rs::shared")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("b.rs::shared must be extracted");

        let calls_from = |src| -> Vec<crate::core::NodeId> {
            graph
                .out_neighbors(src)
                .filter(|(dst, e)| {
                    e.kind == EdgeKind::Calls
                        && graph
                            .node(*dst)
                            .map(|n| !n.qualified_name.starts_with("call::"))
                            .unwrap_or(false)
                })
                .map(|(dst, _)| dst)
                .collect()
        };

        let a_calls = calls_from(entry_a);
        let b_calls = calls_from(entry_b);
        assert!(
            a_calls.contains(&shared_a) && !a_calls.contains(&shared_b),
            "entry_a should call a.rs::shared, not b.rs::shared (a_calls={:?})",
            a_calls
        );
        assert!(
            b_calls.contains(&shared_b) && !b_calls.contains(&shared_a),
            "entry_b should call b.rs::shared, not a.rs::shared (b_calls={:?})",
            b_calls
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn suppresses_low_signal_call_placeholders_before_resolution() {
        assert!(should_suppress_call_placeholder("len"));
        assert!(should_suppress_call_placeholder("rsplit"));
        assert!(should_suppress_call_placeholder("to_string_lossy"));
        assert!(should_suppress_call_placeholder("edges_directed"));
        assert!(should_suppress_call_placeholder("node_weight_mut"));
        assert!(should_suppress_call_placeholder("unwrap_or"));
        assert!(should_suppress_call_placeholder("printf"));
        assert!(!should_suppress_call_placeholder(
            "resolve_call_placeholders"
        ));

        let mut g = Graph::new();
        let caller = make_fn(&mut g, "src::caller");
        let placeholder = make_fn(&mut g, "call::len");
        let real_len = make_fn(&mut g, "src::len");
        g.add_edge(caller, placeholder, Edge::ambiguous(EdgeKind::Calls));

        assert_eq!(resolve_call_placeholders(&mut g), 0);
        assert!(g
            .out_neighbors(caller)
            .all(|(dst, edge)| dst != real_len || edge.kind != EdgeKind::Calls));
    }

    #[test]
    fn common_prefix_len_counts_segments() {
        assert_eq!(common_prefix_len("a::b::c", "a::b::d"), 2);
        assert_eq!(common_prefix_len("a::b::c", "a::b::c"), 3);
        assert_eq!(common_prefix_len("a::b", "x::y"), 0);
        assert_eq!(common_prefix_len("", "a::b"), 0);
    }

    #[test]
    fn infer_type_from_let_bindings_explicit_annotation() {
        let src = "fn foo() { let calc: Calculator = Calculator::new(0.0); }";
        assert_eq!(
            infer_type_from_let_bindings(src, "calc"),
            Some("Calculator".to_string())
        );
    }

    #[test]
    fn infer_type_from_let_bindings_constructor() {
        let src = "fn foo() { let svc = UserService::new(); }";
        assert_eq!(
            infer_type_from_let_bindings(src, "svc"),
            Some("UserService".to_string())
        );
    }

    #[test]
    fn infer_type_from_let_bindings_struct_literal() {
        let src = "fn foo() { let cfg = Config { debug: true }; }";
        assert_eq!(
            infer_type_from_let_bindings(src, "cfg"),
            Some("Config".to_string())
        );
    }

    #[test]
    fn infer_type_from_let_bindings_no_match() {
        let src = "fn foo() { let x = 42; }";
        assert_eq!(infer_type_from_let_bindings(src, "x"), None);
    }

    #[test]
    fn tier3b_scoped_prefix_picks_closest_module() {
        let mut g = Graph::new();
        // Two functions named `helper` — both in "src" so Tier 3 can't
        // disambiguate, but `a` shares the longer "src::utils" prefix with
        // the caller, so Tier 3b should pick it.
        let a = make_fn(&mut g, "file::src::utils::helper");
        let _b = make_fn(&mut g, "file::src::core::helper");
        let caller_node = Node::new(NodeKind::Function, "file::src::utils::caller").with_source(
            "src/utils/caller.rs".to_string(),
            0,
            10,
        );
        let caller = g.add_node(caller_node);
        let ph = g.add_node(Node::new(NodeKind::Function, "call::helper"));
        let mut call_edge = Edge::ambiguous(EdgeKind::Calls);
        // scope "src" matches BOTH candidates → Tier 3 passes, Tier 3b fires.
        call_edge
            .properties
            .insert("call_scope".into(), serde_json::json!("src"));
        g.add_edge(caller, ph, call_edge);
        let resolved = resolve_call_placeholders(&mut g);
        assert_eq!(
            resolved, 1,
            "expected exactly 1 resolution via scoped_prefix"
        );
        let points_to_a = g
            .out_neighbors(caller)
            .any(|(dst, e)| dst == a && e.kind == EdgeKind::Calls);
        assert!(
            points_to_a,
            "should resolve to src::utils::helper (closer prefix)"
        );
    }

    #[test]
    fn tier5_same_dir_affinity() {
        let mut g = Graph::new();
        // Two `process` functions: one in src/pipeline, one in src/io.
        let pipeline_fn = Node::new(NodeKind::Function, "file::src/pipeline/mod::process")
            .with_source("src/pipeline/mod.rs".to_string(), 0, 5);
        let io_fn = Node::new(NodeKind::Function, "file::src/io/mod::process").with_source(
            "src/io/mod.rs".to_string(),
            0,
            5,
        );
        let a = g.add_node(pipeline_fn);
        let b = g.add_node(io_fn);
        // Caller in src/pipeline/runner.rs — same dir as `a`.
        let caller_node = Node::new(NodeKind::Function, "file::src/pipeline/runner::run")
            .with_source("src/pipeline/runner.rs".to_string(), 0, 10);
        let caller = g.add_node(caller_node);
        let ph = g.add_node(Node::new(NodeKind::Function, "call::process"));
        g.add_edge(caller, ph, Edge::ambiguous(EdgeKind::Calls));
        let resolved = resolve_call_placeholders(&mut g);
        assert_eq!(resolved, 1, "Tier 5 same-dir should resolve to 1");
        let points_to_a = g
            .out_neighbors(caller)
            .any(|(dst, e)| dst == a && e.kind == EdgeKind::Calls);
        assert!(points_to_a, "should resolve to pipeline/mod::process");
        let points_to_b = g
            .out_neighbors(caller)
            .any(|(dst, e)| dst == b && e.kind == EdgeKind::Calls);
        assert!(!points_to_b, "should NOT resolve to io/mod::process");
    }

    #[test]
    fn tier6_freq_prior_picks_most_called() {
        let mut g = Graph::new();
        // Two `render` functions; `b` has more in-edges.
        let a = make_fn(&mut g, "file::src/a::render");
        let b = make_fn(&mut g, "file::src/b::render");
        // Give `b` two existing resolved callers.
        let caller1 = make_fn(&mut g, "file::src/x::caller1");
        let caller2 = make_fn(&mut g, "file::src/x::caller2");
        g.add_edge(caller1, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(caller2, b, Edge::extracted(EdgeKind::Calls));
        // New caller calls ambiguous `render`.
        let new_caller = make_fn(&mut g, "file::src/y::new_caller");
        let ph = g.add_node(Node::new(NodeKind::Function, "call::render"));
        g.add_edge(new_caller, ph, Edge::ambiguous(EdgeKind::Calls));
        let resolved = resolve_call_placeholders(&mut g);
        assert_eq!(resolved, 1, "Tier 6 freq_prior should resolve");
        let points_to_b = g
            .out_neighbors(new_caller)
            .any(|(dst, e)| dst == b && e.kind == EdgeKind::Calls);
        assert!(points_to_b, "should pick b (more in-edges)");
        let points_to_a = g
            .out_neighbors(new_caller)
            .any(|(dst, e)| dst == a && e.kind == EdgeKind::Calls);
        assert!(!points_to_a, "should NOT pick a (fewer in-edges)");
    }
}
