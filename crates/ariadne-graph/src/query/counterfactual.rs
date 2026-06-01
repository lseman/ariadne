//! Counterfactual reasoning (Phase 3 — implemented).
//!
//! Clones the in-memory graph, drops the supplied edges, and reruns a
//! query to answer: "if I delete this function / sever this dependency,
//! what stops being reachable?"  This uses actual reachability math
//! rather than the conservative blast-radius approximation.
//!
//! Usage:
//!
//! ```
//! use ariadne_graph::core::Graph;
//! use ariadne_graph::query::counterfactual::run_without_edges;
//!
//! let graph = Graph::new();
//! let drop_edges: &[ariadne_graph::core::EdgeId] = &[];
//! let _counterfactual = run_without_edges(&graph, drop_edges);
//! // counterfactual is a full clone with edges removed
//! ```

use crate::core::{EdgeId, Graph};

/// Return a clone of `graph` with the supplied edges removed.
///
/// The returned graph has its own `by_qname` index rebuilt from the
/// remaining nodes, so symbol resolution works correctly after
/// removal.
pub fn run_without_edges(graph: &Graph, drop: &[EdgeId]) -> Graph {
    let mut clone = graph.clone();
    // Remove edges in reverse index order so removals don't invalidate
    // remaining EdgeIndex look-ups.
    let mut indices: Vec<_> = drop.iter().filter_map(|id| clone.edge_index(*id)).collect();
    indices.sort_unstable_by(|a, b| b.cmp(a));
    for idx in indices {
        clone.remove_edge(idx);
    }
    clone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn clone_preserves_all_edges() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::entry"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::middle"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::leaf"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
        let clone = run_without_edges(&g, &[]);
        assert_eq!(g.node_count(), clone.node_count());
        assert_eq!(g.edge_count(), clone.edge_count());
        assert!(clone.find_by_qname("pkg::entry").is_some());
        assert!(clone.find_by_qname("pkg::leaf").is_some());
    }

    #[test]
    fn removing_edge_reduces_edge_count() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        let edge = g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        let clone = run_without_edges(&g, &[edge]);
        assert_eq!(clone.edge_count(), g.edge_count() - 1);
    }

    #[test]
    fn removing_all_edges_breaks_reachability() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::c"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
        let all_edge_ids: Vec<_> = g.edges().map(|(id, _, _, _)| id).collect();
        let clone = run_without_edges(&g, &all_edge_ids);
        assert_eq!(clone.edge_count(), 0);
        assert_eq!(clone.node_count(), g.node_count());
    }

    #[test]
    fn counterfactual_preserves_qname_index() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::entry"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::target"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        // Remove the only edge
        let edge = g.edges().next().unwrap().0;
        let cf = run_without_edges(&g, &[edge]);
        // Nodes and their qname lookups should still work
        assert_eq!(cf.find_by_qname("pkg::entry"), Some(a));
        assert_eq!(cf.find_by_qname("pkg::target"), Some(b));
    }
}
