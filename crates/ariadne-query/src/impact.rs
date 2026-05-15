//! Bounded impact analysis.
//!
//! Impact walks the reverse graph from a seed symbol and ranks nodes that
//! can reach it. That answers the review-oriented question: "what depends
//! on this thing, and how strongly?"

use ariadne_core::{EdgeKind, Graph, NodeId, NodeKind};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

#[derive(Debug, Clone)]
pub struct ImpactQuery {
    pub seed: NodeId,
    pub max_hops: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct ImpactHit {
    pub id: NodeId,
    pub score: f32,
    pub distance: usize,
    pub via: Vec<EdgeKind>,
}

#[derive(Debug, Clone)]
struct Candidate {
    node: NodeId,
    cost: f32,
    distance: usize,
    via: Vec<EdgeKind>,
}

impl Eq for Candidate {}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && self.cost == other.cost
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn analyze_impact(graph: &Graph, query: ImpactQuery) -> Vec<ImpactHit> {
    let mut heap = BinaryHeap::new();
    let mut best: HashMap<NodeId, Candidate> = HashMap::new();
    heap.push(Candidate {
        node: query.seed,
        cost: 0.0,
        distance: 0,
        via: Vec::new(),
    });

    while let Some(candidate) = heap.pop() {
        if candidate.distance > query.max_hops {
            continue;
        }
        if best
            .get(&candidate.node)
            .map(|seen| seen.cost <= candidate.cost)
            .unwrap_or(false)
        {
            continue;
        }
        best.insert(candidate.node, candidate.clone());

        if candidate.distance == query.max_hops {
            continue;
        }

        for (prev, edge) in graph.in_neighbors(candidate.node) {
            let mut via = candidate.via.clone();
            via.push(edge.kind);
            heap.push(Candidate {
                node: prev,
                cost: candidate.cost + impact_cost(edge.kind, edge.confidence.score()),
                distance: candidate.distance + 1,
                via,
            });
        }
    }

    let mut hits: Vec<_> = best
        .into_iter()
        .filter(|(id, _)| *id != query.seed)
        .map(|(id, candidate)| {
            let kind_boost = graph
                .node(id)
                .map(|node| node_kind_boost(node.kind))
                .unwrap_or(1.0);
            ImpactHit {
                id,
                score: kind_boost / (1.0 + candidate.cost),
                distance: candidate.distance,
                via: candidate.via,
            }
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.distance.cmp(&b.distance))
    });
    hits.truncate(query.limit);
    hits
}

fn impact_cost(kind: EdgeKind, confidence: f32) -> f32 {
    let base = match kind {
        EdgeKind::Calls => 1.0,
        EdgeKind::Defines => 1.25,
        EdgeKind::Imports => 1.6,
        EdgeKind::Inherits | EdgeKind::Implements => 0.75,
        EdgeKind::ReadsWrites => 0.9,
        EdgeKind::DocumentedBy | EdgeKind::Describes => 1.2,
        EdgeKind::Mentions | EdgeKind::Illustrates => 1.8,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor => 2.0,
    };
    base / confidence.clamp(0.05, 1.0)
}

fn node_kind_boost(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Type => 1.3,
        NodeKind::Trait | NodeKind::Impl => 1.2,
        NodeKind::File | NodeKind::Module => 0.95,
        NodeKind::Document | NodeKind::Section | NodeKind::Concept => 0.85,
        NodeKind::Diagram | NodeKind::Image => 0.75,
        NodeKind::Variable | NodeKind::Hyperedge | NodeKind::Commit | NodeKind::Author => 0.7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_core::{Edge, Node};

    #[test]
    fn impact_walks_reverse_calls() {
        let mut g = Graph::new();
        let caller = g.add_node(Node::new(NodeKind::Function, "caller"));
        let callee = g.add_node(Node::new(NodeKind::Function, "callee"));
        g.add_edge(caller, callee, Edge::extracted(EdgeKind::Calls));

        let hits = analyze_impact(
            &g,
            ImpactQuery {
                seed: callee,
                max_hops: 2,
                limit: 10,
            },
        );
        assert_eq!(hits[0].id, caller);
    }
}
