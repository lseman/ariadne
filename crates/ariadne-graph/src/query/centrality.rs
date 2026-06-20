//! Centrality metrics.
//!
//! [`pagerank`] runs a weighted random-walk-with-damping iteration on the
//! directed graph. Edge kind and confidence shape transition probability,
//! and [`personalized_pagerank`] biases the teleport distribution around
//! supplied seed nodes.
//!
//! Edges with [`Confidence::Ambiguous`] are skipped: those are the
//! unresolved call-site placeholders pointing at `call::<name>` synthetic
//! nodes, and including them distorts rank toward common function names
//! like `new`, `len`, `clone`.

use crate::core::{Confidence, EdgeKind, Graph, NodeId};
use std::collections::HashMap;

pub fn pagerank(graph: &Graph, damping: f32, iterations: usize) -> HashMap<NodeId, f32> {
    weighted_pagerank(graph, damping, iterations, &HashMap::new())
}

pub fn personalized_pagerank(
    graph: &Graph,
    seeds: &[(NodeId, f32)],
    damping: f32,
    iterations: usize,
) -> HashMap<NodeId, f32> {
    let mut personalization = HashMap::new();
    let total: f32 = seeds.iter().map(|(_, w)| w.max(0.0)).sum();
    if total > 0.0 {
        for &(id, weight) in seeds {
            personalization.insert(id, weight.max(0.0) / total);
        }
    }
    weighted_pagerank(graph, damping, iterations, &personalization)
}

fn weighted_pagerank(
    graph: &Graph,
    damping: f32,
    iterations: usize,
    personalization: &HashMap<NodeId, f32>,
) -> HashMap<NodeId, f32> {
    let nodes: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }
    let node_index: HashMap<NodeId, usize> = nodes
        .iter()
        .enumerate()
        .map(|(idx, id)| (*id, idx))
        .collect();
    let init = 1.0 / n as f32;
    let mut ranks = vec![init; n];

    let transitions = weighted_transitions(graph, &nodes, &node_index);
    let personalization = personalization_vector(&nodes, personalization);
    let uniform = 1.0 / n as f32;
    let has_personalization = personalization.iter().any(|weight| *weight > 0.0);
    for _ in 0..iterations {
        let mut next: Vec<_> = if has_personalization {
            personalization
                .iter()
                .map(|p| (1.0 - damping) * p)
                .collect()
        } else {
            vec![(1.0 - damping) * uniform; n]
        };
        let mut dangling_mass = 0.0f32;
        for (idx, out) in transitions.iter().enumerate() {
            if out.edges.is_empty() {
                dangling_mass += ranks[idx];
                continue;
            }
            for &(neighbor_idx, weight) in &out.edges {
                next[neighbor_idx] += damping * ranks[idx] * weight / out.total;
            }
        }
        for idx in 0..n {
            let p = if has_personalization {
                personalization[idx]
            } else {
                uniform
            };
            next[idx] += damping * dangling_mass * p;
        }
        ranks = next;
    }
    nodes.into_iter().zip(ranks).collect()
}

struct WeightedTransitions {
    edges: Vec<(usize, f32)>,
    total: f32,
}

fn weighted_transitions(
    graph: &Graph,
    nodes: &[NodeId],
    node_index: &HashMap<NodeId, usize>,
) -> Vec<WeightedTransitions> {
    let mut transitions = Vec::with_capacity(nodes.len());
    for &id in nodes {
        let edges: Vec<_> = graph
            .out_neighbors(id)
            .filter(|(_, e)| !matches!(e.confidence, Confidence::Ambiguous))
            .filter_map(|(n, e)| {
                let idx = node_index.get(&n)?;
                Some((*idx, edge_weight(e.kind) * e.confidence.score().max(0.05)))
            })
            .collect();
        let total = edges.iter().map(|(_, weight)| *weight).sum();
        transitions.push(WeightedTransitions { edges, total });
    }
    transitions
}

fn personalization_vector(nodes: &[NodeId], personalization: &HashMap<NodeId, f32>) -> Vec<f32> {
    nodes
        .iter()
        .map(|id| personalization.get(id).copied().unwrap_or(0.0))
        .collect()
}

fn edge_weight(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::Defines => 0.7,
        EdgeKind::Calls => 1.0,
        EdgeKind::Imports => 0.55,
        EdgeKind::Inherits | EdgeKind::Implements => 1.15,
        EdgeKind::ReadsWrites => 0.9,
        EdgeKind::Mentions | EdgeKind::Describes | EdgeKind::DocumentedBy => 0.75,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor | EdgeKind::Illustrates => 0.6,
        // Production→test edge: low weight so tests don't pull rank away
        // from the code they exercise.
        EdgeKind::TestedBy => 0.3,
        // Flow bookkeeping — overlay-only; don't let it skew rank.
        EdgeKind::MemberOf | EdgeKind::EntryOf => 0.05,
    }
}

/// True for nodes that inflate god-node rankings without representing a
/// real symbol: file containers (high degree purely from `Defines`
/// edges), synthetic flow and hyperedge nodes, and unresolved call
/// placeholders.
pub fn is_rank_noise(node: &crate::core::Node) -> bool {
    matches!(
        node.kind,
        crate::core::NodeKind::File
            | crate::core::NodeKind::Flow
            | crate::core::NodeKind::Hyperedge
    ) || node.qualified_name.starts_with("call::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn rank_noise_filters_files_flows_and_placeholders() {
        assert!(is_rank_noise(&Node::new(
            NodeKind::File,
            "file::src/lib.rs"
        )));
        assert!(is_rank_noise(&Node::new(NodeKind::Flow, "flow::main")));
        assert!(is_rank_noise(&Node::new(NodeKind::Function, "call::len")));
        assert!(!is_rank_noise(&Node::new(NodeKind::Function, "src::login")));
        assert!(!is_rank_noise(&Node::new(NodeKind::Class, "src::Auth")));
    }

    #[test]
    fn pagerank_concentrates_on_sinks() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        g.add_edge(a, c, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
        let ranks = pagerank(&g, 0.85, 30);
        assert!(ranks[&c] > ranks[&a]);
        assert!(ranks[&c] > ranks[&b]);
    }

    #[test]
    fn personalization_biases_toward_seed_neighborhood() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, b, Edge::extracted(EdgeKind::Calls));
        let ranks = personalized_pagerank(&g, &[(a, 1.0)], 0.85, 30);
        assert!(ranks[&a] > ranks[&c]);
    }

    #[test]
    fn pagerank_ignores_unresolved_placeholders() {
        let mut g = Graph::new();
        let real = g.add_node(Node::new(NodeKind::Function, "real"));
        let caller = g.add_node(Node::new(NodeKind::Function, "caller"));
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::missing"));
        g.add_edge(caller, real, Edge::extracted(EdgeKind::Calls));
        g.add_edge(caller, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let ranks = pagerank(&g, 0.85, 30);
        assert!(ranks[&real] > ranks[&placeholder]);
    }
}
