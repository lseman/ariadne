//! Graph export formats (GraphML, CSV).
//!
//! GraphML is the de facto standard for graph interchange, supported by
//! Gephi, yEd, Cytoscape, and many other tools. This module exports the
//! Ariadne graph as GraphML XML.

use crate::core::{Graph, NodeId};
use std::collections::HashMap;
use std::io::Write;

/// Export the graph as GraphML XML.
///
/// Returns the XML as a `String`. Nodes carry `kind`, `qualified_name`,
/// `name`, `file`, and `community_id` attributes. Edges carry `kind`,
/// `confidence`, and `score` attributes.
pub fn export_graphml(graph: &Graph, community_map: &HashMap<NodeId, usize>) -> String {
    let mut out = Vec::new();
    writeln!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8"?>"#
    )
    .unwrap();
    writeln!(
        out,
        r#"<graphml xmlns="http://graphml.graphstruct.org/graphml"
  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
  xsi:schemaLocation="http://graphml.graphstruct.org/graphml">"#
    )
    .unwrap();
    // Node keys
    for (attr, typ) in [
        ("kind", "string"),
        ("qualified_name", "string"),
        ("name", "string"),
        ("file", "string"),
        ("kind_raw", "string"),
    ] {
        writeln!(
            out,
            r#"  <key id="{attr}" for="node" attr.name="{attr}" attr.type="{typ}"/>"#
        )
        .unwrap();
    }
    // Edge keys
    for (attr, typ) in [
        ("edge_kind", "string"),
        ("confidence", "string"),
        ("score", "double"),
        ("source_file", "string"),
        ("target_file", "string"),
    ] {
        writeln!(
            out,
            r#"  <key id="{attr}" for="edge" attr.name="{attr}" attr.type="{typ}"/>"#
        )
        .unwrap();
    }
    writeln!(out, r#"  <graph id="ariadne" edgedefault="directed">"#).unwrap();

    // Nodes
    for (id, node) in graph.nodes() {
        let qn = html_escape(&node.qualified_name);
        let name = html_escape(&node.name);
        let kind = html_escape(node.kind.as_str());
        let file = html_escape(&node.source_uri.as_deref().unwrap_or(""));
        let comm_id = community_map.get(&id).copied();

        writeln!(out, r#"    <node id="n{idx}">"#, idx = id.0).unwrap();
        writeln!(out, r#"      <data key="qualified_name">{qn}</data>"#).unwrap();
        writeln!(out, r#"      <data key="name">{name}</data>"#).unwrap();
        writeln!(out, r#"      <data key="kind">{kind}</data>"#).unwrap();
        writeln!(out, r#"      <data key="file">{file}</data>"#).unwrap();
        writeln!(out, r#"      <data key="kind_raw">{kind_raw}</data>"#, kind_raw = kind).unwrap();
        if let Some(cid) = comm_id {
            writeln!(out, r#"      <data key="community_id">{cid}</data>"#).unwrap();
        }
        writeln!(out, r#"    </node>"#).unwrap();
    }

    // Edges
    for (edge_id, src, dst, edge) in graph.edges() {
        let src_kind = html_escape(edge.kind.as_str());
        let conf_str = match edge.confidence {
            crate::core::Confidence::Extracted => "extracted".to_string(),
            crate::core::Confidence::Inferred(s) => format!("inferred:{s:.3}"),
            crate::core::Confidence::Ambiguous => "ambiguous".to_string(),
        };
        let score = edge.confidence.score();
        let src_node = graph.node(src);
        let dst_node = graph.node(dst);
        let src_file = html_escape(&src_node.as_ref().map_or("", |n| n.source_uri.as_deref().unwrap_or("")));
        let dst_file = html_escape(&dst_node.as_ref().map_or("", |n| n.source_uri.as_deref().unwrap_or("")));

        writeln!(
            out,
            r#"    <edge id="e{}" source="n{}" target="n{}">"#,
            edge_id.0, src.0, dst.0
        )
        .unwrap();
        writeln!(out, r#"      <data key="edge_kind">{src_kind}</data>"#).unwrap();
        writeln!(out, r#"      <data key="confidence">{conf_str}</data>"#).unwrap();
        writeln!(out, r#"      <data key="score">{score:.3}</data>"#).unwrap();
        writeln!(out, r#"      <data key="source_file">{src_file}</data>"#).unwrap();
        writeln!(out, r#"      <data key="target_file">{dst_file}</data>"#).unwrap();
        writeln!(out, r#"    </edge>"#).unwrap();
    }

    writeln!(out, "  </graph>").unwrap();
    writeln!(out, "</graphml>").unwrap();

    String::from_utf8(out).unwrap_or_default()
}

/// HTML-escape a string (minimal set for XML safety).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn graphml_contains_nodes_and_edges() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "foo"));
        let b = g.add_node(Node::new(NodeKind::Function, "bar"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let comm = HashMap::new();
        let xml = export_graphml(&g, &comm);
        assert!(xml.contains("<node id=\"n0\">"));
        assert!(xml.contains("<node id=\"n1\">"));
        assert!(xml.contains("<edge"));
        assert!(xml.contains("function"));
        assert!(xml.contains("</graphml>"));
    }

    #[test]
    fn graphml_escapes_special_chars() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "foo <bar>"));
        let b = g.add_node(Node::new(NodeKind::Function, "baz & qux"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let comm = HashMap::new();
        let xml = export_graphml(&g, &comm);
        assert!(xml.contains("&lt;bar&gt;"));
        assert!(xml.contains("&amp;"));
    }
}
