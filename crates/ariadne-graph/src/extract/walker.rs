use crate::core::{Edge, EdgeKind, Graph, NodeKind};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct IgnoreSet {
    matcher: ignore::gitignore::Gitignore,
}

impl IgnoreSet {
    pub fn load(root: &Path) -> Self {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
        for ignore_file in [".gitignore", ".ariadneignore"] {
            if let Some(err) = builder.add(root.join(ignore_file)) {
                tracing::warn!("failed to load {}: {}", ignore_file, err);
            }
        }
        let matcher = builder.build().unwrap_or_else(|err| {
            tracing::warn!("failed to build ignore matcher: {}", err);
            ignore::gitignore::Gitignore::empty()
        });
        Self { matcher }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        default_ignored_name(name) || self.matcher.matched(path, path.is_dir()).is_ignore()
    }
}

/// Walk `root` and dispatch each supported file to the right pass.
///
/// Returns the number of files processed. Skips hidden directories
/// (`.git`, `.venv`, `target`, `node_modules`).
pub fn extract_directory(root: &Path, graph: &mut Graph) -> Result<usize> {
    let mut count = 0usize;
    let ignore = IgnoreSet::load(root);
    let registry = super::ast::language_registry::registry();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(lang_def) = registry.get_by_path(path) {
            if let Err(e) = super::ast::custom_lang::extract_file(path, graph, lang_def) {
                tracing::warn!("failed to extract {}: {}", path.display(), e);
            } else {
                count += 1;
                continue;
            }
        }
        // Concept (prose/diagram) extractor as fallback.
        if let Some(extractor) = super::concept::concept_registry::get_by_path(path) {
            if let Err(e) = extractor(path, graph) {
                tracing::warn!("failed to extract {}: {}", path.display(), e);
            } else {
                count += 1;
            }
        }
    }
    resolve_call_placeholders(graph);
    super::concept::resolve_all_mentions(graph);
    derive_tested_by_edges(graph);
    super::flows::compute_flows(graph);
    Ok(count)
}

/// Same as `extract_directory` but with a custom language registry.
///
/// Custom language entries are merged on top of the global registry at
/// runtime (useful for tests that inject ad-hoc languages).
pub fn extract_directory_with_custom(
    root: &Path,
    graph: &mut Graph,
    custom: &std::collections::HashMap<String, super::ast::language_registry::LanguageDef>,
) -> Result<usize> {
    let mut count = 0usize;
    let ignore = IgnoreSet::load(root);
    let registry = super::ast::language_registry::registry();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Check custom languages first (they override built-in extension matching)
        let lang_def = custom
            .values()
            .find(|l| l.matches_ext(path))
            .or_else(|| registry.get_by_path(path));
        if let Some(lang_def) = lang_def {
            if let Err(e) = super::ast::custom_lang::extract_file(path, graph, lang_def) {
                tracing::warn!("failed to extract {}: {}", path.display(), e);
            } else {
                count += 1;
                continue;
            }
        }
        // Concept (prose/diagram) extractor as fallback.
        if let Some(extractor) = super::concept::concept_registry::get_by_path(path) {
            if let Err(e) = extractor(path, graph) {
                tracing::warn!("failed to extract {}: {}", path.display(), e);
            } else {
                count += 1;
            }
        }
    }
    resolve_call_placeholders(graph);
    super::concept::resolve_all_mentions(graph);
    derive_tested_by_edges(graph);
    super::flows::compute_flows(graph);
    Ok(count)
}

pub fn ignore_set(root: &Path) -> IgnoreSet {
    IgnoreSet::load(root)
}

/// Extract a single file — dispatches to the language-specific extractor
/// based on file extension.
pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let registry = super::ast::language_registry::registry();
    if let Some(lang_def) = registry.get_by_path(path) {
        return super::ast::custom_lang::extract_file(path, graph, lang_def);
    }
    // Concept (prose/diagram) extractor as fallback.
    if let Some(extractor) = super::concept::concept_registry::get_by_path(path) {
        return extractor(path, graph);
    }
    Ok(())
}

/// Extract a single file with optional custom language support.
pub fn extract_file_with_custom(
    path: &Path,
    graph: &mut Graph,
    custom: &std::collections::HashMap<String, super::ast::language_registry::LanguageDef>,
) -> Result<()> {
    // Check custom languages first (they override built-in)
    if let Some(lang_def) = custom.values().find(|l| l.matches_ext(path)) {
        return super::ast::custom_lang::extract_file(path, graph, lang_def);
    }
    let registry = super::ast::language_registry::registry();
    if let Some(lang_def) = registry.get_by_path(path) {
        return super::ast::custom_lang::extract_file(path, graph, lang_def);
    }
    // Concept (prose/diagram) extractor as fallback.
    if let Some(extractor) = super::concept::concept_registry::get_by_path(path) {
        return extractor(path, graph);
    }
    Ok(())
}

/// Built-in file extensions that are always supported, regardless of
/// TOML config. Document languages (markdown, HTML, LaTeX) live
/// outside the AST pass and use concept extractors. SVG uses the
/// vision (diagram) extractor. All are listed here for relevance
/// filtering.
pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some(
            "rs" | "py"
                | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "c"
                | "cc"
                | "cpp"
                | "cxx"
                | "h"
                | "hh"
                | "hpp"
                | "hxx"
                | "md"
                | "markdown"
                | "tex"
                | "html"
                | "htm"
                | "svg"
        )
    )
}

/// Check if a path matches any custom language extension.
pub fn is_custom_supported(
    path: &Path,
    custom: &std::collections::HashMap<String, super::ast::language_registry::LanguageDef>,
) -> bool {
    custom.values().any(|lang| lang.matches_ext(path))
}

/// True when a filesystem event on `path` could change the graph: the
/// file is a supported source type, no component under `root` is a
/// default-ignored directory, and the ignore set does not match.
pub fn is_relevant_source(root: &Path, path: &Path, ignore: &IgnoreSet) -> bool {
    if !is_supported(path) && !is_custom_supported(path, &Default::default()) {
        return false;
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
    let ignored_component = rel.components().any(|c| match c {
        std::path::Component::Normal(name) => default_ignored_name(&name.to_string_lossy()),
        _ => false,
    });
    !ignored_component && !ignore.is_ignored(path)
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

fn build_by_name(graph: &Graph) -> HashMap<String, Vec<crate::core::NodeId>> {
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

// Identifier tokens from each file's import paths (`use crate::auth;`
// → {crate, auth}, `from pkg.auth import login` → {pkg, auth},
// `import './auth'` → {auth}), used by Tier 4 to prefer candidates
// whose module the caller's file actually imports.
fn build_import_tokens(graph: &Graph) -> HashMap<String, HashSet<String>> {
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

pub fn resolve_call_placeholders(graph: &mut Graph) -> usize {
    let by_name = build_by_name(graph);
    let import_tokens = build_import_tokens(graph);

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

pub fn should_suppress_call_placeholder(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        // Python builtins and common constructors.
        "abs"
            | "all"
            | "any"
            | "bool"
            | "bytes"
            | "callable"
            | "dict"
            | "dir"
            | "enumerate"
            | "filter"
            | "float"
            | "getattr"
            | "hasattr"
            | "hash"
            | "id"
            | "int"
            | "isinstance"
            | "iter"
            | "len"
            | "list"
            | "map"
            | "max"
            | "min"
            | "next"
            | "open"
            | "print"
            | "range"
            | "repr"
            | "reversed"
            | "round"
            | "set"
            | "sorted"
            | "str"
            | "sum"
            | "super"
            | "tuple"
            | "type"
            | "vars"
            | "zip"
            // Rust/std/common fluent API calls that otherwise dominate
            // unresolved call nodes.
            | "and_then"
            | "as_bytes"
            | "as_deref"
            | "as_ref"
            | "as_str"
            | "chars"
            | "clone"
            | "cloned"
            | "clamp"
            | "collect"
            | "contains"
            | "copied"
            | "count"
            | "default"
            | "ends_with"
            | "entry"
            | "err"
            | "expect"
            | "extend"
            | "filter_map"
            | "find"
            | "first"
            | "flat_map"
            | "fold"
            | "from"
            | "get"
            | "index"
            | "insert"
            | "into"
            | "into_iter"
            | "is_empty"
            | "is_none"
            | "iter_mut"
            | "join"
            | "last"
            | "lines"
            | "map_err"
            | "new"
            | "none"
            | "ok"
            | "ok_or"
            | "ok_or_else"
            | "or_default"
            | "position"
            | "push"
            | "push_str"
            | "rsplit"
            | "some"
            | "split"
            | "splitn"
            | "starts_with"
            | "take"
            | "to_owned"
            | "to_string"
            | "to_string_lossy"
            | "trim"
            | "unwrap"
            | "unwrap_or"
            | "unwrap_or_default"
            | "unwrap_or_else"
            | "with_capacity"
            // Common graph-library traversal/mutation helpers. Keeping
            // these out of the code graph prevents external petgraph calls
            // from masquerading as unresolved project calls.
            | "contains_node"
            | "edge_indices"
            | "edge_references"
            | "edge_weight_mut"
            | "edges_directed"
            | "node_indices"
            | "node_weight"
            | "node_weight_mut"
            // C/C++ and libc-style calls.
            | "malloc"
            | "free"
            | "printf"
            | "fprintf"
            | "memcpy"
            | "memset"
            | "strlen"
            | "strcmp"
            | "std"
    )
}

/// Reverse every `test_fn -[Calls]-> production_fn` edge into a
/// `production_fn -[TestedBy]-> test_fn` edge.
///
/// "Test" is the source node having `is_test=true` in its properties.
/// Placeholder targets (qualified names starting with `call::`) are
/// ignored — they're never real definitions. Idempotent: an existing
/// `TestedBy` edge between the same pair is left alone.
pub fn derive_tested_by_edges(graph: &mut Graph) -> usize {
    fn is_test_node(node: &crate::core::Node) -> bool {
        node.properties
            .get("is_test")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    let existing: HashSet<(_, _)> = graph
        .edges()
        .filter(|(_, _, _, edge)| edge.kind == EdgeKind::TestedBy)
        .map(|(_, src, dst, _)| (src, dst))
        .collect();

    let mut additions = Vec::new();
    for (_, src, dst, edge) in graph.edges() {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        let Some(src_node) = graph.node(src) else {
            continue;
        };
        let Some(dst_node) = graph.node(dst) else {
            continue;
        };
        if !is_test_node(src_node) {
            continue;
        }
        if is_test_node(dst_node) {
            // Don't link test → test; we only care about test → production.
            continue;
        }
        if dst_node.qualified_name.starts_with("call::") {
            continue;
        }
        // Reverse direction: production_fn -[TestedBy]-> test_fn.
        if existing.contains(&(dst, src)) {
            continue;
        }
        additions.push((dst, src));
    }

    // Deduplicate within this batch — the same (production, test) pair
    // could appear via multiple call edges, so we keep a HashSet of
    // pairs already emitted and skip duplicates. The count returned is
    // the number of *new* edges actually added.
    let mut seen: HashSet<(crate::core::NodeId, crate::core::NodeId)> = HashSet::new();
    let mut count = 0usize;
    for (production, test) in additions {
        if seen.insert((production, test)) {
            graph.add_edge(production, test, Edge::extracted(EdgeKind::TestedBy));
            count += 1;
        }
    }
    count
}

fn default_ignored_name(name: &str) -> bool {
    (name.starts_with('.') && name.len() > 1)
        || matches!(name, "target" | "node_modules" | "__pycache__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Node, NodeKind};

    fn make_test_fn(graph: &mut Graph, qname: &str) -> crate::core::NodeId {
        let node = Node::new(NodeKind::Function, qname)
            .with_property("is_test", serde_json::Value::Bool(true));
        graph.add_node(node)
    }

    fn make_fn(graph: &mut Graph, qname: &str) -> crate::core::NodeId {
        graph.add_node(Node::new(NodeKind::Function, qname))
    }

    #[test]
    fn extract_directory_honors_gitignore() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_gitignore_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/generated")).unwrap();
        std::fs::write(dir.join(".gitignore"), "src/generated/\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn kept() {}\n").unwrap();
        std::fs::write(
            dir.join("src/generated/ignored.rs"),
            "pub fn ignored() {}\n",
        )
        .unwrap();

        let mut graph = Graph::new();
        let count = extract_directory(&dir, &mut graph).unwrap();
        assert_eq!(count, 1);
        assert!(graph
            .nodes()
            .any(|(_, n)| n.qualified_name.ends_with("::kept")));
        assert!(
            graph
                .nodes()
                .all(|(_, n)| !n.qualified_name.ends_with("::ignored")),
            ".gitignore entries should be excluded from extraction"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_directory_honors_ariadneignore() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_ariadneignore_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/scratch")).unwrap();
        std::fs::write(dir.join(".ariadneignore"), "src/scratch/*.rs\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn kept() {}\n").unwrap();
        std::fs::write(dir.join("src/scratch/noisy.rs"), "pub fn noisy() {}\n").unwrap();

        let mut graph = Graph::new();
        let count = extract_directory(&dir, &mut graph).unwrap();
        assert_eq!(count, 1);
        assert!(graph
            .nodes()
            .any(|(_, n)| n.qualified_name.ends_with("::kept")));
        assert!(
            graph
                .nodes()
                .all(|(_, n)| !n.qualified_name.ends_with("::noisy")),
            ".ariadneignore entries should be excluded from extraction"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relevance_filters_event_paths() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_relevance_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(".gitignore"), "src/gen.rs\n").unwrap();
        let ignore = IgnoreSet::load(&dir);

        assert!(is_relevant_source(&dir, &dir.join("src/lib.rs"), &ignore));
        // Unsupported extensions: the db file and its WAL sibling must
        // never trigger an update, or watch mode would loop on itself.
        assert!(!is_relevant_source(&dir, &dir.join("ariadne.db"), &ignore));
        assert!(!is_relevant_source(
            &dir,
            &dir.join("ariadne.db-wal"),
            &ignore
        ));
        // Default-ignored directories anywhere in the relative path.
        assert!(!is_relevant_source(
            &dir,
            &dir.join("target/debug/build.rs"),
            &ignore
        ));
        assert!(!is_relevant_source(
            &dir,
            &dir.join(".git/objects/aa/bb.rs"),
            &ignore
        ));
        assert!(!is_relevant_source(
            &dir,
            &dir.join("web/node_modules/x/index.js"),
            &ignore
        ));
        // .gitignore entries apply to event paths too.
        assert!(!is_relevant_source(&dir, &dir.join("src/gen.rs"), &ignore));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extracts_plain_javascript_files() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_js_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/app.js"),
            "function greet(name) { return name; }\nfunction main() { greet('x'); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/view.jsx"),
            "function View() { return <div>{greet('y')}</div>; }\n",
        )
        .unwrap();

        let mut graph = Graph::new();
        let count = extract_directory(&dir, &mut graph).unwrap();
        assert_eq!(count, 2);

        let greet = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("::greet") && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("greet must be extracted from .js");
        let main = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("app.js::main"))
            .map(|(id, _)| id)
            .expect("main must be extracted from .js");
        assert!(
            graph
                .out_neighbors(main)
                .any(|(dst, e)| e.kind == EdgeKind::Calls && dst == greet),
            "call from main should resolve to greet"
        );
        assert!(
            graph
                .nodes()
                .any(|(_, n)| n.qualified_name.ends_with("::View")),
            "JSX component must be extracted via TSX grammar"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn derives_tested_by_edges_from_calls() {
        let mut g = Graph::new();
        let test_fn = make_test_fn(&mut g, "tests::test_login");
        let prod_fn = make_fn(&mut g, "src::login");
        g.add_edge(test_fn, prod_fn, Edge::extracted(EdgeKind::Calls));

        let added = derive_tested_by_edges(&mut g);
        assert_eq!(added, 1);

        // Edge convention: `production -[TestedBy]-> test`. So the test
        // is reachable via prod_fn's outgoing TestedBy edges.
        let outgoing: Vec<_> = g
            .out_neighbors(prod_fn)
            .filter(|(_, e)| e.kind == EdgeKind::TestedBy)
            .collect();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].0, test_fn);
    }

    #[test]
    fn derive_tested_by_is_idempotent() {
        let mut g = Graph::new();
        let test_fn = make_test_fn(&mut g, "tests::test_login");
        let prod_fn = make_fn(&mut g, "src::login");
        g.add_edge(test_fn, prod_fn, Edge::extracted(EdgeKind::Calls));

        let first = derive_tested_by_edges(&mut g);
        let second = derive_tested_by_edges(&mut g);
        assert_eq!(first, 1);
        assert_eq!(second, 0, "running twice must not duplicate edges");

        let count = g
            .edges()
            .filter(|(_, _, _, e)| e.kind == EdgeKind::TestedBy)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn rust_pipeline_marks_cfg_test_mod_functions() {
        // Tree-sitter-rust places #[test] / #[cfg(test)] as *siblings*
        // of the item they decorate, not as children. Regression test
        // for a bug where the detector walked children and missed every
        // real-world test function.
        let dir =
            std::env::temp_dir().join(format!("ariadne_rust_test_detect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // Uses `assert!(login(...))` deliberately — this exercises both
        // the #[cfg(test)] mod + #[test] attribute detection AND the
        // macro-call extraction fallback (tree-sitter-rust returns raw
        // token_trees inside macro bodies rather than parsed
        // call_expressions, so without that fallback the call would be
        // invisible).
        std::fs::write(
            dir.join("src/lib.rs"),
            r#"
pub fn login(user: &str) -> bool { user == "alice" }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_login_accepts_alice() {
        assert!(login("alice"));
    }
}
"#,
        )
        .unwrap();

        let mut graph = Graph::new();
        extract_directory(&dir, &mut graph).unwrap();

        let test_fn = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name
                    .ends_with("::tests::test_login_accepts_alice")
            })
            .map(|(_, n)| n)
            .expect("test function must be extracted");
        assert_eq!(
            test_fn.properties.get("is_test").and_then(|v| v.as_bool()),
            Some(true),
            "#[cfg(test)] mod + #[test] function must be marked is_test"
        );

        let login = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("::login") && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("login must be extracted");
        let outgoing: Vec<_> = graph
            .out_neighbors(login)
            .map(|(n, e)| {
                (
                    graph
                        .node(n)
                        .map(|x| x.qualified_name.clone())
                        .unwrap_or_default(),
                    e.kind,
                )
            })
            .collect();
        let tested_by: Vec<_> = graph
            .out_neighbors(login)
            .filter(|(_, e)| e.kind == EdgeKind::TestedBy)
            .collect();
        assert_eq!(
            tested_by.len(),
            1,
            "login should have exactly one TestedBy edge from the test fn; out edges = {:?}",
            outgoing
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rust_nested_inner_fn_qualified_under_outer() {
        // `fn outer() { fn helper() { ... } }` previously collapsed
        // `helper` to a bare top-level qname, colliding across files
        // and producing spurious orphan flows. The fix qualifies it
        // as `outer::helper` and keeps its kind as Function (it's not
        // a method just because it lives inside another fn).
        let dir = std::env::temp_dir().join(format!("ariadne_nested_fn_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            r#"
pub fn outer() -> u32 {
    fn helper() -> u32 { 1 }
    helper() + helper()
}
"#,
        )
        .unwrap();

        let mut graph = Graph::new();
        extract_directory(&dir, &mut graph).unwrap();

        let helper = graph
            .nodes()
            .find(|(_, n)| n.qualified_name.ends_with("::outer::helper"))
            .map(|(id, n)| (id, n.clone()))
            .expect("nested helper must be qualified under outer");
        assert_eq!(
            helper.1.kind,
            crate::core::NodeKind::Function,
            "nested fn is a free function, not a method"
        );
        // And it must NOT also be registered as a bare top-level
        // `helper`: if both forms coexisted the resolver could pick
        // either and the bug would resurface.
        let bare = graph.nodes().find(|(_, n)| {
            n.qualified_name.ends_with("/lib.rs::helper") && !n.qualified_name.starts_with("call::")
        });
        assert!(
            bare.is_none(),
            "helper must not be emitted at file-top-level qname"
        );

        // outer should have a Calls edge to the nested helper (via
        // file-local resolution since their `name` matches).
        let outer = graph
            .nodes()
            .find(|(_, n)| {
                n.qualified_name.ends_with("::outer") && !n.qualified_name.contains("helper")
            })
            .map(|(id, _)| id)
            .expect("outer must be extracted");
        let resolved_to_helper = graph
            .out_neighbors(outer)
            .any(|(dst, e)| e.kind == EdgeKind::Calls && dst == helper.0);
        assert!(
            resolved_to_helper,
            "call from outer should resolve to nested helper"
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
    fn python_pipeline_marks_tests_and_derives_edges() {
        // End-to-end on a temp dir: ensure that test detection in the
        // extractor + placeholder resolution + TestedBy derivation
        // compose correctly without us having to set is_test manually.
        let dir =
            std::env::temp_dir().join(format!("ariadne_test_pipeline_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        std::fs::write(
            dir.join("src/auth.py"),
            "def login(user):\n    return user\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tests/test_auth.py"),
            "def test_login():\n    login('alice')\n",
        )
        .unwrap();

        let mut graph = Graph::new();
        let count = extract_directory(&dir, &mut graph).unwrap();
        assert_eq!(count, 2);

        // Find the production `login` function.
        let login = graph
            .nodes()
            .find(|(_, n)| {
                matches!(n.kind, NodeKind::Function)
                    && n.qualified_name.ends_with("::login")
                    && !n.qualified_name.starts_with("call::")
            })
            .map(|(id, _)| id)
            .expect("production login function should be present");

        let tests_out: Vec<_> = graph
            .out_neighbors(login)
            .filter(|(_, e)| e.kind == EdgeKind::TestedBy)
            .collect();
        assert_eq!(
            tests_out.len(),
            1,
            "login must have exactly one TestedBy edge"
        );
        let test_node = graph.node(tests_out[0].0).unwrap();
        assert!(
            test_node.qualified_name.ends_with("::test_login"),
            "TestedBy must point at the test function"
        );
        assert_eq!(
            test_node
                .properties
                .get("is_test")
                .and_then(|v| v.as_bool()),
            Some(true),
            "test function must carry is_test=true"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignores_placeholder_targets_and_test_to_test_calls() {
        let mut g = Graph::new();
        let test_a = make_test_fn(&mut g, "tests::test_a");
        let test_b = make_test_fn(&mut g, "tests::test_b");
        let placeholder = make_fn(&mut g, "call::some_external");
        let prod_fn = make_fn(&mut g, "src::real");

        g.add_edge(test_a, test_b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(test_a, placeholder, Edge::ambiguous(EdgeKind::Calls));
        g.add_edge(test_a, prod_fn, Edge::extracted(EdgeKind::Calls));

        let added = derive_tested_by_edges(&mut g);
        assert_eq!(
            added, 1,
            "only the test→production call should yield a TestedBy edge"
        );

        // Placeholder should not appear as a TestedBy *source* (nothing
        // is "tested by" a placeholder), nor as a target (we skip them).
        let from_placeholder: Vec<_> = g
            .out_neighbors(placeholder)
            .filter(|(_, e)| e.kind == EdgeKind::TestedBy)
            .collect();
        assert!(from_placeholder.is_empty());

        // test_b is itself a test; production code never points to it.
        let test_b_incoming: Vec<_> = g
            .in_neighbors(test_b)
            .filter(|(_, e)| e.kind == EdgeKind::TestedBy)
            .collect();
        assert!(test_b_incoming.is_empty());
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
}
