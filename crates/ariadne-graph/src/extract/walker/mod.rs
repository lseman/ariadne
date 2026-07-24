use crate::core::{Edge, EdgeKind, Graph, GraphMut};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;

mod call_resolution;
mod exclusions;
mod suppress_list;
pub use call_resolution::resolve_call_placeholders;
use exclusions::default_ignored_name;
pub use suppress_list::should_suppress_call_placeholder;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct IgnoreSet {
    matchers: Vec<ignore::gitignore::Gitignore>,
}

impl IgnoreSet {
    pub fn load(root: &Path) -> Self {
        let mut matchers = Vec::new();
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_dir()
                    || !default_ignored_name(&entry.file_name().to_string_lossy())
            })
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().is_file()
                    && matches!(
                        entry.file_name().to_str(),
                        Some(".gitignore" | ".ariadneignore")
                    )
            })
        {
            let base = entry.path().parent().unwrap_or(root);
            let mut builder = ignore::gitignore::GitignoreBuilder::new(base);
            if let Some(err) = builder.add(entry.path()) {
                tracing::warn!("failed to read {}: {}", entry.path().display(), err);
                continue;
            }
            match builder.build() {
                Ok(matcher) => matchers.push(matcher),
                Err(err) => {
                    tracing::warn!("failed to load {}: {}", entry.path().display(), err);
                }
            }
        }
        Self { matchers }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if default_ignored_name(name) {
            return true;
        }

        let mut ignored = false;
        for matcher in &self.matchers {
            let matched = matcher.matched(path, path.is_dir());
            if matched.is_ignore() {
                ignored = true;
            } else if matched.is_whitelist() {
                ignored = false;
            }
        }
        ignored
    }
}

/// Walk `root` and dispatch each supported file to the right pass.
///
/// Files are processed in parallel using rayon. Each file is extracted
/// into its own graph, then all graphs are merged. Post-processing
/// (placeholder resolution, concept mentions, flows) runs once on the
/// merged graph.
///
/// Returns the number of files processed. Skips hidden directories
/// (`.git`, `.venv`, `target`, `node_modules`).
pub fn extract_directory(root: &Path, graph: &mut dyn GraphMut) -> Result<usize> {
    let ignore = IgnoreSet::load(root);
    let registry = super::ast::language_registry::registry();

    // Collect all file paths first (sequential walk, fast).
    let files: Vec<_> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    let count = files.len();

    // Extract each file in parallel, each into its own Graph.
    let per_file: Vec<_> = files
        .par_iter()
        .map(|path| {
            let mut g = Graph::new();
            if let Some(lang_def) = registry.get_by_path(path) {
                let _ = super::ast::custom_lang::extract_file(path, &mut g, lang_def);
            } else if let Some(extractor) = super::concept::concept_registry::get_by_path(path) {
                let _ = extractor(path, &mut g);
            }
            g
        })
        .collect();

    // Merge all per-file graphs into the target graph.
    for g in per_file {
        graph.merge(g);
    }

    // Post-processing on the merged graph.
    resolve_call_placeholders(graph);
    // Resolve TypeScript path aliases (e.g. @/ → src/) so IMPORTS_FROM
    // edges point to real file nodes rather than bare alias strings.
    super::ast::tsconfig_resolver::resolve_ts_path_aliases(graph, root);
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
pub fn extract_file(path: &Path, graph: &mut dyn GraphMut) -> Result<()> {
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

/// True when the path is handled by either the TOML-backed AST registry
/// or the concept/diagram registry.
pub fn is_supported(path: &Path) -> bool {
    super::ast::language_registry::registry()
        .get_by_path(path)
        .is_some()
        || super::concept::concept_registry::is_supported(path)
}

/// True when a filesystem event on `path` could change the graph: the
/// file is a supported source type, no component under `root` is a
/// default-ignored directory, and the ignore set does not match.
pub fn is_relevant_source(root: &Path, path: &Path, ignore: &IgnoreSet) -> bool {
    if !is_supported(path) {
        return false;
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
    let ignored_component = rel.components().any(|c| match c {
        std::path::Component::Normal(name) => default_ignored_name(&name.to_string_lossy()),
        _ => false,
    });
    !ignored_component && !ignore.is_ignored(path)
}

/// Reverse every `test_fn -[Calls]-> production_fn` edge into a
/// `production_fn -[TestedBy]-> test_fn` edge.
///
/// "Test" is the source node having `is_test=true` in its properties.
/// Placeholder targets (qualified names starting with `call::`) are
/// ignored — they're never real definitions. Idempotent: an existing
/// `TestedBy` edge between the same pair is left alone.
pub fn derive_tested_by_edges(graph: &mut dyn GraphMut) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Node, NodeKind};

    fn make_test_fn(graph: &mut dyn GraphMut, qname: &str) -> crate::core::NodeId {
        let node = Node::new(NodeKind::Function, qname)
            .with_property("is_test", serde_json::Value::Bool(true));
        graph.add_node(node)
    }

    fn make_fn(graph: &mut dyn GraphMut, qname: &str) -> crate::core::NodeId {
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
    fn extract_directory_honors_nested_ariadneignore() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_nested_ariadneignore_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("packages/app/src/generated")).unwrap();
        std::fs::write(dir.join("packages/app/.ariadneignore"), "src/generated/\n").unwrap();
        std::fs::write(dir.join("packages/app/src/lib.rs"), "pub fn kept() {}\n").unwrap();
        std::fs::write(
            dir.join("packages/app/src/generated/noisy.rs"),
            "pub fn noisy() {}\n",
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
                .all(|(_, n)| !n.qualified_name.ends_with("::noisy")),
            "nested .ariadneignore entries should be excluded from extraction"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_ariadneignore_can_reinclude_file_patterns() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_nested_reinclude_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("packages/app/src")).unwrap();
        std::fs::write(dir.join(".ariadneignore"), "packages/app/src/*.rs\n").unwrap();
        std::fs::write(dir.join("packages/app/.ariadneignore"), "!src/lib.rs\n").unwrap();
        std::fs::write(dir.join("packages/app/src/lib.rs"), "pub fn kept() {}\n").unwrap();
        std::fs::write(dir.join("packages/app/src/noisy.rs"), "pub fn noisy() {}\n").unwrap();

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
            "nested .ariadneignore negations should reinclude matching files"
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
}
