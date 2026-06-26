//! SVG extraction.
//!
//! Registers the SVG file as a `Diagram` node and emits a `Concept`
//! node per non-empty `<text>` element it contains. Concept → symbol
//! cross-linking is delegated to the resolver also used by markdown.

use crate::core::{Edge, EdgeKind, GraphMut, Node, NodeKind};
use anyhow::Result;
use std::fs;
use std::path::Path;

pub fn extract_file(path: &Path, graph: &mut dyn GraphMut) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let file_uri = path.to_string_lossy().to_string();
    let qn = format!("diagram::{}", file_uri);
    let diag_id =
        graph.add_node(Node::new(NodeKind::Diagram, &qn).with_source(file_uri.clone(), 0, 0));

    for label in extract_text_labels(&source) {
        let concept_qn = format!("concept::{}", label);
        let concept_id = graph.add_node(Node::new(NodeKind::Concept, &concept_qn));
        graph.add_edge(
            diag_id,
            concept_id,
            Edge::inferred(EdgeKind::Illustrates, 0.7),
        );
    }

    Ok(())
}

fn extract_text_labels(svg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find next "<text"
        if let Some(start) = find_subslice(&bytes[i..], b"<text") {
            let open_start = i + start;
            // Find end of opening tag
            let after_open = match find_subslice(&bytes[open_start..], b">") {
                Some(p) => open_start + p + 1,
                None => break,
            };
            let close = match find_subslice(&bytes[after_open..], b"</text>") {
                Some(p) => after_open + p,
                None => break,
            };
            if let Ok(text) = std::str::from_utf8(&bytes[after_open..close]) {
                let clean = strip_inner_tags(text).trim().to_string();
                if !clean.is_empty() {
                    out.push(clean);
                }
            }
            i = close + b"</text>".len();
        } else {
            break;
        }
    }
    out
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn strip_inner_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}
