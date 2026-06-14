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
    let mut ranks: HashMap<NodeId, f32> = HashMap::with_capacity(n);
    if n == 0 {
        return ranks;
    }
    let init = 1.0 / n as f32;
    for &id in &nodes {
        ranks.insert(id, init);
    }

    let uniform = 1.0 / n as f32;
    let has_personalization = !personalization.is_empty();
    for _ in 0..iterations {
        let mut next: HashMap<NodeId, f32> = HashMap::with_capacity(n);
        for &id in &nodes {
            let p = if has_personalization {
                personalization.get(&id).copied().unwrap_or(0.0)
            } else {
                uniform
            };
            next.insert(id, (1.0 - damping) * p);
        }
        let mut dangling_mass = 0.0f32;
        for &id in &nodes {
            let out: Vec<(NodeId, f32)> = graph
                .out_neighbors(id)
                .filter(|(_, e)| !matches!(e.confidence, Confidence::Ambiguous))
                .map(|(n, e)| (n, edge_weight(e.kind) * e.confidence.score().max(0.05)))
                .collect();
            if out.is_empty() {
                dangling_mass += ranks[&id];
                continue;
            }
            let total_weight: f32 = out.iter().map(|(_, w)| *w).sum();
            for (n_id, weight) in out {
                *next.entry(n_id).or_insert(0.0) += damping * ranks[&id] * weight / total_weight;
            }
        }
        for &id in &nodes {
            let p = if has_personalization {
                personalization.get(&id).copied().unwrap_or(0.0)
            } else {
                uniform
            };
            *next.entry(id).or_insert(0.0) += damping * dangling_mass * p;
        }
        ranks = next;
    }
    ranks
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
}
