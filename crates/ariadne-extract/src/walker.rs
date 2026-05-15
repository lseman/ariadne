use anyhow::Result;
use ariadne_core::{Edge, EdgeKind, Graph, NodeKind};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct IgnoreSet {
    root: std::path::PathBuf,
    patterns: Vec<String>,
}

impl IgnoreSet {
    pub fn load(root: &Path) -> Self {
        let mut patterns = Vec::new();
        patterns.extend(read_ignore_file(&root.join(".gitignore")));
        patterns.extend(read_ignore_file(&root.join(".ariadneignore")));
        Self {
            root: root.to_path_buf(),
            patterns,
        }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let rel = rel.to_string_lossy().replace('\\', "/");
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        default_ignored_name(name) || self.patterns.iter().any(|p| matches_pattern(&rel, name, p))
    }
}

fn read_ignore_file(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
                .map(|line| line.trim_start_matches("./").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Walk `root` and dispatch each supported file to the right pass.
///
/// Returns the number of files processed. Skips hidden directories
/// (`.git`, `.venv`, `target`, `node_modules`).
pub fn extract_directory(root: &Path, graph: &mut Graph) -> Result<usize> {
    let mut count = 0usize;
    let ignore = IgnoreSet::load(root);
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !is_supported(path) {
            continue;
        }
        let res = extract_file(path, graph);
        if let Err(e) = res {
            tracing::warn!("failed to extract {}: {}", path.display(), e);
            continue;
        }
        count += 1;
    }
    resolve_call_placeholders(graph);
    Ok(count)
}

pub fn ignore_set(root: &Path) -> IgnoreSet {
    IgnoreSet::load(root)
}

pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("rs") => crate::ast::rust::extract_file(path, graph),
        Some("py") => crate::ast::python::extract_file(path, graph),
        Some("c") | Some("cc") | Some("cpp") | Some("cxx") | Some("h") | Some("hh")
        | Some("hpp") | Some("hxx") => crate::ast::cpp::extract_file(path, graph),
        Some("md") | Some("markdown") => crate::concept::markdown::extract_file(path, graph),
        Some("tex") => crate::concept::latex::extract_file(path, graph),
        Some("svg") => crate::vision::svg::extract_file(path, graph),
        _ => Ok(()),
    }
}

pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some(
            "rs" | "py"
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
                | "svg"
        )
    )
}

pub fn resolve_call_placeholders(graph: &mut Graph) -> usize {
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

    let existing: HashSet<_> = graph
        .edges()
        .filter(|(_, _, _, edge)| edge.kind == EdgeKind::Calls)
        .map(|(_, src, dst, _)| (src, dst))
        .collect();
    let mut additions = Vec::new();

    for (_, src, dst, edge) in graph.edges() {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        let Some(callee) = graph.node(dst) else {
            continue;
        };
        let Some(name) = callee.qualified_name.strip_prefix("call::") else {
            continue;
        };
        let Some(candidates) = by_name.get(name) else {
            continue;
        };
        if candidates.len() == 1 && !existing.contains(&(src, candidates[0])) {
            additions.push((src, candidates[0]));
        }
    }

    let count = additions.len();
    for (src, dst) in additions {
        let mut edge = Edge::extracted(EdgeKind::Calls);
        edge.properties.insert(
            "resolved_from".into(),
            serde_json::Value::String("call_placeholder".into()),
        );
        graph.add_edge(src, dst, edge);
    }
    count
}

fn default_ignored_name(name: &str) -> bool {
    (name.starts_with('.') && name.len() > 1)
        || matches!(name, "target" | "node_modules" | "__pycache__")
}

fn matches_pattern(rel: &str, name: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let pattern = pattern.trim_end_matches('/');
    if pattern.contains('*') {
        return glob_match(pattern, rel) || glob_match(pattern, name);
    }
    rel == pattern
        || name == pattern
        || rel.starts_with(&format!("{}/", pattern))
        || rel.contains(&format!("/{}", pattern))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut cursor = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(found) = text[cursor..].find(part) else {
            return false;
        };
        if i == 0 && !pattern.starts_with('*') && found != 0 {
            return false;
        }
        cursor += found + part.len();
    }
    if !pattern.ends_with('*') {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }
    true
}
