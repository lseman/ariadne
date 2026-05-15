//! Markdown extraction.
//!
//! This is a deliberately minimal first pass: split on ATX headings,
//! emit a `Document` node and one `Section` per heading, and look for
//! inline code spans whose contents match a `Function` or `Class`
//! `qualified_name` suffix. Each match emits a `Mentions` edge with
//! confidence 0.85.
//!
//! A full implementation would parse with `pulldown-cmark`, follow
//! reference-style links, and run an embedding-based concept extractor
//! over paragraph text — left as a Phase 2 TODO.

use anyhow::Result;
use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use std::fs;
use std::path::Path;

pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let file_uri = path.to_string_lossy().to_string();
    let doc_qn = format!("doc::{}", file_uri);
    let doc_id = graph.add_node(Node::new(NodeKind::Document, &doc_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    let mut current_section: Option<NodeId> = None;
    for (i, line) in source.lines().enumerate() {
        if let Some(rest) = line.strip_prefix('#') {
            let title = rest.trim_start_matches('#').trim();
            if !title.is_empty() {
                let qn = format!("{}::{}", doc_qn, slugify(title));
                let id = graph.add_node(Node::new(NodeKind::Section, &qn).with_source(
                    file_uri.clone(),
                    i as u32,
                    i as u32,
                ));
                graph.add_edge(doc_id, id, Edge::extracted(EdgeKind::Defines));
                current_section = Some(id);
            }
            continue;
        }
        // Inline code spans: `foo` — collect and try to match.
        for token in collect_inline_code(line) {
            if let Some(target) = resolve_symbol(graph, &token) {
                let from = current_section.unwrap_or(doc_id);
                graph.add_edge(from, target, Edge::inferred(EdgeKind::Mentions, 0.85));
            }
        }
    }

    Ok(())
}

fn collect_inline_code(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_code = false;
    for c in line.chars() {
        if c == '`' {
            if in_code {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
                in_code = false;
            } else {
                in_code = true;
            }
        } else if in_code {
            buf.push(c);
        }
    }
    out
}

fn resolve_symbol(graph: &Graph, token: &str) -> Option<NodeId> {
    // Match by exact suffix of qualified_name, restricted to code-kind
    // nodes so we don't link to other documents.
    for (id, node) in graph.nodes() {
        if !matches!(
            node.kind,
            NodeKind::Function | NodeKind::Class | NodeKind::Method | NodeKind::Type
        ) {
            continue;
        }
        if node.name == token || node.qualified_name.ends_with(&format!("::{}", token)) {
            return Some(id);
        }
    }
    None
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
