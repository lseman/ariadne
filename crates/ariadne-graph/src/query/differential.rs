//! Temporal / differential queries (Phase 2).
//!
//! Every node and edge carries `valid_from` and `valid_to` SHA columns.
//! These queries classify graph entities by their validity windows rather
//! than by file hashes or the current working tree diff.

use crate::core::{EdgeId, EdgeKind, Graph, NodeId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalChangeKind {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedEdge {
    pub id: EdgeId,
    pub src: NodeId,
    pub dst: NodeId,
    pub edge_kind: EdgeKind,
    pub change: TemporalChangeKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemporalDiff {
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub added_edges: Vec<ChangedEdge>,
    pub removed_edges: Vec<ChangedEdge>,
}

impl TemporalDiff {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.added_edges.is_empty()
            && self.removed_edges.is_empty()
    }

    pub fn changed_nodes(&self) -> Vec<NodeId> {
        let mut nodes = Vec::new();
        for id in self
            .added_nodes
            .iter()
            .chain(self.removed_nodes.iter())
            .copied()
        {
            if !nodes.contains(&id) {
                nodes.push(id);
            }
        }
        for edge in self.added_edges.iter().chain(self.removed_edges.iter()) {
            if !nodes.contains(&edge.src) {
                nodes.push(edge.src);
            }
            if !nodes.contains(&edge.dst) {
                nodes.push(edge.dst);
            }
        }
        nodes
    }
}

pub fn temporal_diff<F>(graph: &Graph, base: &str, head: &str, is_ancestor: &mut F) -> TemporalDiff
where
    F: FnMut(&str, &str) -> bool,
{
    let mut diff = TemporalDiff::default();

    for (id, node) in graph.nodes() {
        let active_at_base = is_active_at(
            node.valid_from.as_deref(),
            node.valid_to.as_deref(),
            base,
            is_ancestor,
        );
        let active_at_head = is_active_at(
            node.valid_from.as_deref(),
            node.valid_to.as_deref(),
            head,
            is_ancestor,
        );
        match (active_at_base, active_at_head) {
            (false, true) => diff.added_nodes.push(id),
            (true, false) => diff.removed_nodes.push(id),
            _ => {}
        }
    }

    for (id, src, dst, edge) in graph.edges() {
        let active_at_base = is_active_at(
            edge.valid_from.as_deref(),
            edge.valid_to.as_deref(),
            base,
            is_ancestor,
        );
        let active_at_head = is_active_at(
            edge.valid_from.as_deref(),
            edge.valid_to.as_deref(),
            head,
            is_ancestor,
        );
        let changed = ChangedEdge {
            id,
            src,
            dst,
            edge_kind: edge.kind,
            change: if active_at_head {
                TemporalChangeKind::Added
            } else {
                TemporalChangeKind::Removed
            },
        };
        match (active_at_base, active_at_head) {
            (false, true) => diff.added_edges.push(changed),
            (true, false) => diff.removed_edges.push(changed),
            _ => {}
        }
    }

    diff.added_nodes.sort_by_key(|id| id.0);
    diff.removed_nodes.sort_by_key(|id| id.0);
    diff.added_edges.sort_by_key(|edge| edge.id.0);
    diff.removed_edges.sort_by_key(|edge| edge.id.0);
    diff
}

pub fn is_active_at<F>(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    commit: &str,
    is_ancestor: &mut F,
) -> bool
where
    F: FnMut(&str, &str) -> bool,
{
    if let Some(from) = valid_from {
        if from != commit && !is_ancestor(from, commit) {
            return false;
        }
    }
    if let Some(to) = valid_to {
        if to != commit && is_ancestor(to, commit) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn temporal_diff_classifies_validity_windows() {
        let mut graph = Graph::new();
        let unchanged = graph.add_node(Node::new(NodeKind::Function, "m::unchanged"));
        let mut added_node = Node::new(NodeKind::Function, "m::added");
        added_node.valid_from = Some("c".to_string());
        let added = graph.add_node(added_node);
        let mut removed_node = Node::new(NodeKind::Function, "m::removed");
        removed_node.valid_from = Some("a".to_string());
        removed_node.valid_to = Some("b".to_string());
        let removed = graph.add_node(removed_node);

        let mut added_edge = Edge::extracted(EdgeKind::Calls);
        added_edge.valid_from = Some("c".to_string());
        graph.add_edge(unchanged, added, added_edge);
        let mut removed_edge = Edge::extracted(EdgeKind::Calls);
        removed_edge.valid_to = Some("b".to_string());
        graph.add_edge(unchanged, removed, removed_edge);

        let mut is_ancestor = |ancestor: &str, descendant: &str| {
            matches!(
                (ancestor, descendant),
                ("a", "b") | ("a", "d") | ("b", "d") | ("c", "d")
            )
        };
        let diff = temporal_diff(&graph, "b", "d", &mut is_ancestor);

        assert_eq!(diff.added_nodes, vec![added]);
        assert_eq!(diff.removed_nodes, vec![removed]);
        assert_eq!(diff.added_edges.len(), 1);
        assert_eq!(diff.removed_edges.len(), 1);
        assert!(diff.changed_nodes().contains(&unchanged));
    }

    #[test]
    fn valid_to_commit_is_still_active_at_that_commit() {
        let mut is_ancestor =
            |ancestor: &str, descendant: &str| matches!((ancestor, descendant), ("b", "d"));
        assert!(is_active_at(None, Some("b"), "b", &mut is_ancestor));
        assert!(!is_active_at(None, Some("b"), "d", &mut is_ancestor));
    }
}
