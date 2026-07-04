//! SQL row helpers and FTS query building.

use crate::core::{Confidence, Edge, EdgeKind, Node, NodeKind};

/// Parse confidence class from DB string to `Confidence` enum.
pub fn parse_confidence(conf_class: &str, confidence: f64) -> Confidence {
    match conf_class {
        "extracted" => Confidence::Extracted,
        "inferred" => Confidence::Inferred(confidence as f32),
        "ambiguous" => Confidence::Ambiguous,
        _ => Confidence::Inferred(confidence as f32),
    }
}

/// Convert a DB row into a `Node`.
#[allow(clippy::too_many_arguments)]
pub fn node_row_from_sql(
    kind_str: String,
    qname: String,
    source_uri: Option<String>,
    line_start: Option<u32>,
    line_end: Option<u32>,
    properties: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
    source_text: Option<String>,
) -> Node {
    let kind: NodeKind =
        serde_json::from_value(serde_json::Value::String(kind_str)).unwrap_or(NodeKind::Function);
    let mut node = Node::new(kind, qname);
    node.source_uri = source_uri;
    node.line_start = line_start;
    node.line_end = line_end;
    node.properties = serde_json::from_str(&properties).unwrap_or_default();
    node.valid_from = valid_from;
    node.valid_to = valid_to;
    node.source_text = source_text;
    node
}

/// Convert a DB edge row into an `Edge`.
pub fn edge_row_from_sql(
    kind_str: String,
    confidence: f64,
    conf_class: String,
    properties: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
) -> Edge {
    let kind: EdgeKind =
        serde_json::from_value(serde_json::Value::String(kind_str)).unwrap_or(EdgeKind::Calls);
    let confidence = parse_confidence(&conf_class, confidence);
    Edge {
        kind,
        confidence,
        properties: serde_json::from_str(&properties).unwrap_or_default(),
        valid_from,
        valid_to,
    }
}

/// Unique identity string for an edge (src, dst, kind).
pub fn edge_identity(src_qname: &str, dst_qname: &str, kind: EdgeKind) -> String {
    format!("{}\u{1f}{}\u{1f}{:?}", src_qname, dst_qname, kind)
}

/// Build a safe FTS5 MATCH expression from a raw user query.
///
/// Each whitespace/punctuation-separated token becomes a prefix term (`token*`).
/// Special FTS5 syntax characters are stripped to prevent query parse errors.
pub fn build_fts5_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| {
            let clean: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            clean
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("{}*", t))
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    // Tokens joined by space = AND in FTS5; each token is a prefix match.
    tokens.join(" ")
}
