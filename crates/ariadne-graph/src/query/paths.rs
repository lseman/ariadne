//! Constrained path enumeration.
//!
//! [`find_paths`] enumerates simple paths between two nodes (or from a
//! source to any node) under three constraints:
//!
//! - `max_hops` — drop paths longer than this (BFS bound).
//! - `edge_kinds` — restrict traversal to specific [`EdgeKind`]s.
//! - `min_confidence` — drop edges whose confidence score is below this
//!   threshold, which lets callers say "structural paths only" by
//!   setting it to `1.0`.

use crate::core::{EdgeKind, Graph, NodeId};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

#[derive(Debug, Clone)]
pub struct PathQuery {
    pub from: NodeId,
    pub to: Option<NodeId>,
    pub max_hops: usize,
    pub edge_kinds: Option<Vec<EdgeKind>>,
    pub min_confidence: f32,
}

#[derive(Debug, Clone)]
pub struct WeightedPath {
    pub nodes: Vec<NodeId>,
    pub cost: f32,
}

impl PathQuery {
    pub fn between(from: NodeId, to: NodeId, max_hops: usize) -> Self {
        Self {
            from,
            to: Some(to),
            max_hops,
            edge_kinds: None,
            min_confidence: 0.0,
        }
    }

    pub fn with_edge_kinds(mut self, kinds: Vec<EdgeKind>) -> Self {
        self.edge_kinds = Some(kinds);
        self
    }

    pub fn with_min_confidence(mut self, c: f32) -> Self {
        self.min_confidence = c;
        self
    }
}

#[derive(Debug, Clone)]
struct CandidatePath {
    nodes: Vec<NodeId>,
    cost: f32,
}

impl Eq for CandidatePath {}

impl PartialEq for CandidatePath {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost && self.nodes == other.nodes
    }
}

impl Ord for CandidatePath {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.nodes.len().cmp(&self.nodes.len()))
    }
}

impl PartialOrd for CandidatePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn find_paths(graph: &Graph, q: &PathQuery) -> Vec<Vec<NodeId>> {
    let mut results = Vec::new();
    let mut queue: VecDeque<Vec<NodeId>> = VecDeque::new();
    queue.push_back(vec![q.from]);

    while let Some(path) = queue.pop_front() {
        if path.len() > q.max_hops + 1 {
            continue;
        }
        let last = *path.last().unwrap();
        if let Some(target) = q.to {
            if last == target && path.len() > 1 {
                results.push(path.clone());
                if path.len() > q.max_hops {
                    continue;
                }
            }
        }
        for (next, edge) in graph.out_neighbors(last) {
            if path.contains(&next) {
                continue;
            }
            if let Some(allowed) = q.edge_kinds.as_ref() {
                if !allowed.contains(&edge.kind) {
                    continue;
                }
            }
            if edge.confidence.score() < q.min_confidence {
                continue;
            }
            let mut new_path = path.clone();
            new_path.push(next);
            queue.push_back(new_path);
        }
    }

    results
}

pub fn find_top_paths(graph: &Graph, q: &PathQuery, limit: usize) -> Vec<WeightedPath> {
    if limit == 0 {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut heap = BinaryHeap::new();
    heap.push(CandidatePath {
        nodes: vec![q.from],
        cost: 0.0,
    });

    while let Some(path) = heap.pop() {
        if path.nodes.len() > q.max_hops + 1 {
            continue;
        }

        let last = *path.nodes.last().unwrap();
        if q.to == Some(last) && path.nodes.len() > 1 {
            results.push(WeightedPath {
                nodes: path.nodes.clone(),
                cost: path.cost,
            });
            if results.len() >= limit {
                break;
            }
            continue;
        }

        for (next, edge) in graph.out_neighbors(last) {
            if path.nodes.contains(&next) {
                continue;
            }
            if let Some(allowed) = q.edge_kinds.as_ref() {
                if !allowed.contains(&edge.kind) {
                    continue;
                }
            }
            if edge.confidence.score() < q.min_confidence {
                continue;
            }

            let mut nodes = path.nodes.clone();
            nodes.push(next);
            heap.push(CandidatePath {
                nodes,
                cost: path.cost + edge_cost(edge.kind, edge.confidence.score()),
            });
        }
    }

    diversify_paths(results)
}

fn edge_cost(kind: EdgeKind, confidence: f32) -> f32 {
    let base = match kind {
        EdgeKind::Defines => 0.35,
        EdgeKind::Calls => 1.0,
        EdgeKind::Imports => 1.35,
        EdgeKind::Inherits | EdgeKind::Implements => 0.8,
        EdgeKind::ReadsWrites => 1.15,
        EdgeKind::DocumentedBy | EdgeKind::Describes => 1.1,
        EdgeKind::Mentions | EdgeKind::Illustrates => 1.7,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor => 2.0,
        // Test edges are rarely useful for general traversal; high cost
        // discourages routing reasoning paths through them.
        EdgeKind::TestedBy => 1.8,
        // Flow bookkeeping — never a useful path edge for reasoning.
        EdgeKind::MemberOf | EdgeKind::EntryOf => 3.0,
    };
    base / confidence.clamp(0.05, 1.0)
}

fn diversify_paths(mut paths: Vec<WeightedPath>) -> Vec<WeightedPath> {
    let mut chosen: Vec<WeightedPath> = Vec::with_capacity(paths.len());
    while !paths.is_empty() {
        paths.sort_by(|a, b| a.cost.partial_cmp(&b.cost).unwrap_or(Ordering::Equal));
        let next = paths.remove(0);
        for candidate in &mut paths {
            let overlap = node_overlap(&next.nodes, &candidate.nodes);
            candidate.cost += overlap * 0.15;
        }
        chosen.push(next);
    }
    chosen
}

fn node_overlap(a: &[NodeId], b: &[NodeId]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.iter().filter(|id| b.contains(id)).count();
    shared as f32 / a.len().min(b.len()) as f32
}

pub fn callers_of(graph: &Graph, target: NodeId) -> Vec<NodeId> {
    graph
        .in_neighbors(target)
        .filter(|(_, e)| e.kind == EdgeKind::Calls)
        .map(|(n, _)| n)
        .collect()
}

pub fn callees_of(graph: &Graph, source: NodeId) -> Vec<NodeId> {
    graph
        .out_neighbors(source)
        .filter(|(_, e)| e.kind == EdgeKind::Calls)
        .map(|(n, _)| n)
        .collect()
}
