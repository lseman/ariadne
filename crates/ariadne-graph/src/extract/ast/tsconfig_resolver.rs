//! Post-extraction resolver for TypeScript path aliases.
//!
//! Tree-sitter parses import source strings verbatim — `@/components/foo`
//! becomes `module::@/components/foo`.  This pass walks the graph, finds
//! module nodes whose names start with known TS alias prefixes (`@`, `~`,
//! `#`, or `@src`), loads the nearest `tsconfig.json`, resolves the path
//! alias, probes for the target file, and renames the module node to the
//! canonical `file::/abs/path` so that `importers_of` and `impact` work
//! correctly across files.

use crate::core::{GraphMut, NodeId, NodeKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extensions probed when resolving an alias target.
const PROBE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".vue", ".mjs", ".cjs"];

/// Tsconfig filenames to look for when walking up the directory tree.
const TSCONFIG_NAMES: &[&str] = &["tsconfig.json", "tsconfig.app.json", "tsconfig.node.json"];

/// Known path aliases that are NOT project-internal aliases but rather
/// npm-package imports. These should NOT be resolved to file paths.
const BUILTIN_ALIASES: &[&str] = &[
    "@types",
    "@angular",
    "@ionic",
    "@nx",
    "@nrwl",
    "@babel",
    "@testing-library",
    "@storybook",
    "@mui",
    "@next",
    "@vue",
    "@sveltejs",
    "@remix-run",
    "@emotion",
    "@radix-ui",
    "@tanstack",
    "@headlessui",
    "@chakra-ui",
];

/// Resolve TypeScript path aliases in the graph.
///
/// Returns the number of module nodes that were renamed.
pub fn resolve_ts_path_aliases(graph: &mut dyn GraphMut, repo_root: &Path) -> usize {
    // Collect all module nodes that look like TS alias imports.
    let mut candidates: Vec<(NodeId, String)> = Vec::new();
    for (id, node) in graph.nodes() {
        if node.kind != NodeKind::Module {
            continue;
        }
        let qn = &node.qualified_name;
        // Module nodes from TS imports look like "module::@/..." or "module::~/..."
        // or "module::#/..." or "module::@src/..."
        if let Some(mod_name) = qn.strip_prefix("module::") {
            // Skip built-in aliases (@types, @angular, etc.) and non-alias
            // imports (bare npm packages like "react").
            if is_ts_alias_path(mod_name) && !is_builtin_alias(mod_name) {
                candidates.push((id, mod_name.to_string()));
            }
        }
    }

    if candidates.is_empty() {
        return 0;
    }

    // Build a cache: directory → tsconfig data.
    let mut tsconfig_cache: HashMap<String, TsconfigData> = HashMap::new();

    let mut resolved = 0;
    for (mod_id, mod_name) in candidates {
        let data = match find_tsconfig_for_module(&mut tsconfig_cache, mod_id, graph, repo_root) {
            Some(d) => d,
            None => continue,
        };
        if let Some(resolved_path) = resolve_alias(&mod_name, &data) {
            let resolved_qn = format!("file::{}", resolved_path.display());
            graph.rename_node(
                mod_id,
                &resolved_qn,
                resolved_path
                    .file_stem()
                    .map(|s| s.to_str().unwrap_or(""))
                    .unwrap_or(""),
            );
            resolved += 1;
        }
    }
    resolved
}

#[derive(Default, Clone)]
struct TsconfigData {
    base_url: Option<PathBuf>,
    paths: HashMap<String, Vec<String>>,
    tsconfig_dir: PathBuf,
}

fn is_ts_alias_path(name: &str) -> bool {
    // Known alias prefixes
    name.starts_with('@')
        || name.starts_with('~')
        || name.starts_with('#')
        || name.starts_with("@src")
        || name.starts_with("@app")
        || name.starts_with("@lib")
        || name.starts_with("@shared")
}

fn is_builtin_alias(name: &str) -> bool {
    BUILTIN_ALIASES
        .iter()
        .any(|alias| name == *alias || name.starts_with(&format!("{alias}/")))
}

fn find_tsconfig_for_module(
    cache: &mut HashMap<String, TsconfigData>,
    mod_id: NodeId,
    graph: &dyn GraphMut,
    repo_root: &Path,
) -> Option<TsconfigData> {
    // Get the source file of the importer (the file that has this import edge).
    let file_uri = graph
        .in_neighbors(mod_id)
        .find_map(|(src_id, _)| graph.node(src_id).and_then(|n| n.source_uri.clone()))?;

    let file_dir = PathBuf::from(&file_uri)
        .parent()?
        .to_string_lossy()
        .to_string();

    if let Some(data) = cache.get(&file_dir) {
        return Some(data.clone());
    }

    // Resolve the file path relative to repo_root if it's not absolute.
    let file_path = if PathBuf::from(&file_uri).is_absolute() {
        PathBuf::from(&file_uri)
    } else {
        repo_root.join(&file_uri)
    };

    // Walk up from the file's directory to find tsconfig.json.
    let tsconfig_path = find_nearest_tsconfig(file_path.parent()?, repo_root)?;
    let tsconfig_dir = tsconfig_path.parent()?.to_path_buf();
    let data = parse_tsconfig(&tsconfig_path)?;
    let result = TsconfigData {
        base_url: data.base_url.map(|b| tsconfig_dir.join(b)),
        paths: data.paths,
        tsconfig_dir,
    };
    cache.insert(file_dir, result.clone());
    Some(result)
}

fn find_nearest_tsconfig(start: &Path, repo_root: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    let root = repo_root.canonicalize().ok()?;
    loop {
        for name in TSCONFIG_NAMES {
            let candidate = current.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !current.pop() || current == root {
            break;
        }
        for name in TSCONFIG_NAMES {
            let candidate = current.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // Check root one final time — tsconfig is often at the project root.
    for name in TSCONFIG_NAMES {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

struct RawTsconfig {
    base_url: Option<String>,
    paths: HashMap<String, Vec<String>>,
}

fn parse_tsconfig(path: &Path) -> Option<RawTsconfig> {
    let content = std::fs::read_to_string(path).ok()?;
    let stripped = strip_jsonc_comments(&content);
    let data: serde_json::Value = serde_json::from_str(&stripped).ok()?;
    let compiler_options = data.get("compilerOptions")?.as_object()?;

    let base_url = compiler_options
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let paths_map = compiler_options.get("paths")?.as_object()?;
    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in paths_map {
        if let Some(arr) = value.as_array() {
            paths.insert(
                key.to_string(),
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect(),
            );
        }
    }

    Some(RawTsconfig { base_url, paths })
}

fn strip_jsonc_comments(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if escape {
            escape = false;
            result.push(ch);
            continue;
        }
        if ch == '\\' && in_string {
            escape = true;
            result.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            result.push(ch);
            continue;
        }
        if in_string {
            result.push(ch);
            continue;
        }
        if ch == '/' {
            if let Some(&next) = chars.peek() {
                if next == '/' {
                    // Line comment — skip to newline
                    let _ = chars.by_ref().find(|&c| c == '\n');
                    continue;
                } else if next == '*' {
                    // Block comment — skip to */
                    loop {
                        match chars.next() {
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next(); // consume '/'
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                    continue;
                }
            }
        }
        result.push(ch);
    }
    result
}

fn resolve_alias(mod_name: &str, config: &TsconfigData) -> Option<PathBuf> {
    // Try exact match first.
    for (pattern, targets) in &config.paths {
        if pattern.as_str() == mod_name {
            // Use the first target pattern.
            if let Some(target) = targets.first() {
                return probe_target(&config.base_url, &config.tsconfig_dir, target);
            }
        }
        // Try wildcard pattern: @/components/* → @/components/foo
        if pattern.contains('*') {
            let prefix = pattern.split('*').next()?;
            let suffix = pattern.rsplit('*').next()?;
            if mod_name.starts_with(prefix) && mod_name.ends_with(suffix) {
                let replacement = &mod_name[prefix.len()..mod_name.len() - suffix.len()];
                if let Some(t) = targets.first() {
                    let target_replaced = t.replace('*', replacement);
                    return probe_target(&config.base_url, &config.tsconfig_dir, &target_replaced);
                }
            }
        }
    }
    // If the module name starts with @ and there's no paths entry,
    // try baseUrl as fallback (common in simple setups).
    if let Some(base) = &config.base_url {
        let base_path = config.tsconfig_dir.join(base);
        if let Some(found) = probe_with_extensions(&base_path.join(mod_name)) {
            return Some(found);
        }
    }
    None
}

fn probe_target(
    base_url: &Option<PathBuf>,
    tsconfig_dir: &Path,
    target_pattern: &str,
) -> Option<PathBuf> {
    let base = base_url
        .as_ref()
        .map(|b| tsconfig_dir.join(b))
        .unwrap_or_else(|| tsconfig_dir.to_path_buf());
    let target_path = base.join(target_pattern);
    probe_with_extensions(&target_path)
}

fn probe_with_extensions(path: &Path) -> Option<PathBuf> {
    // Try the path as-is.
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    // Try with each extension.
    for ext in PROBE_EXTENSIONS {
        let candidate = path.with_extension(ext.trim_start_matches('.'));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Try as directory with index.{ext}.
    if path.is_dir() {
        for ext in PROBE_EXTENSIONS {
            let candidate = path.join(format!("index{}", ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Graph, Node};

    #[test]
    fn strips_jsonc_comments() {
        // JSONC only uses double-quoted strings.
        let input = r#"{ /* x */ "a": 1, "b": "c/*d" }"#;
        let result = strip_jsonc_comments(input);
        assert!(result.contains(r#""a": 1"#));
        assert!(
            result.contains(r#""c/*d""#),
            "string value must preserve /* inside quotes: {}",
            result
        );
    }

    #[test]
    fn resolves_ts_path_aliases_renames_module_nodes() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne_tsconfig_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Clean up previous runs
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("src/components")).unwrap();
        let button_path = dir.join("src/components/button.tsx");
        let app_path = dir.join("src/app.tsx");
        std::fs::write(&button_path, "export function Button() {}").unwrap();
        std::fs::write(&app_path, "import { Button } from '@/components/button';").unwrap();
        let tsconfig_path = dir.join("tsconfig.json");
        std::fs::write(
            &tsconfig_path,
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
        )
        .unwrap();
        assert!(
            tsconfig_path.is_file(),
            "tsconfig not found at {:?}",
            tsconfig_path
        );
        assert!(app_path.is_file(), "app.tsx not found at {:?}", app_path);

        let mut graph = Graph::new();
        let file_app = graph.add_node(
            Node::new(
                crate::core::NodeKind::File,
                format!("file::{}", app_path.display()),
            )
            .with_source(app_path.to_string_lossy().to_string(), 0, 10),
        );
        let alias_mod = graph.add_node(Node::new(
            crate::core::NodeKind::Module,
            "module::@/components/button",
        ));
        graph.add_edge(file_app, alias_mod, Edge::extracted(EdgeKind::Imports));

        let resolved = resolve_ts_path_aliases(&mut graph, &dir);
        assert_eq!(
            resolved,
            1,
            "should have resolved the alias, graph has {} nodes",
            graph.node_count()
        );

        let renamed = graph.node(alias_mod).expect("node should still exist");
        assert!(
            renamed.qualified_name.contains("button"),
            "module node should be renamed to file path, got: {}",
            renamed.qualified_name
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_builtin_aliases() {
        let mut graph = Graph::new();
        // @types is a builtin — should NOT be resolved.
        let types_mod = graph.add_node(Node::new(
            crate::core::NodeKind::Module,
            "module::@types/node",
        ));
        let file = graph.add_node(Node::new(crate::core::NodeKind::File, "file::app.ts"));
        graph.add_edge(file, types_mod, Edge::extracted(EdgeKind::Imports));

        // Create a real tsconfig — but @types should still be skipped
        let dir = std::env::temp_dir().join(format!(
            "ariadne_builtin_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        if std::fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@types/node": ["x"]}}}"#,
        )
        .is_ok()
        {
            let resolved = resolve_ts_path_aliases(&mut graph, &dir);
            assert_eq!(resolved, 0, "builtin aliases should be skipped");
            std::fs::remove_dir_all(&dir).ok();
        }

        let node = graph.node(types_mod).expect("node should still exist");
        assert_eq!(
            node.qualified_name, "module::@types/node",
            "builtin alias should not be renamed"
        );
    }
}
