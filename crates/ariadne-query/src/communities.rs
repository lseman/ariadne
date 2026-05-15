//! Community detection.
//!
//! `louvain` now uses modularity-gain local movement over a weighted,
//! undirected projection of the Ariadne graph. `leiden` builds on that
//! partition with a refinement pass that splits disconnected communities,
//! giving the most important practical property of Leiden-style clustering:
//! communities should be internally reachable instead of accidental labels.

use ariadne_core::{EdgeKind, Graph, NodeId};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct CommunityOptions {
    pub resolution: f32,
    pub max_passes: usize,
}

impl Default for CommunityOptions {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            max_passes: 50,
        }
    }
}

pub fn louvain(graph: &Graph) -> HashMap<NodeId, usize> {
    louvain_with_options(graph, CommunityOptions::default())
}

pub fn leiden(graph: &Graph) -> HashMap<NodeId, usize> {
    refine_connected(graph, &louvain(graph))
}

pub fn louvain_with_options(graph: &Graph, options: CommunityOptions) -> HashMap<NodeId, usize> {
    let nodes: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
    let mut comm: HashMap<NodeId, usize> =
        nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let adj = weighted_adjacency(graph);
    let degree: HashMap<NodeId, f32> = nodes
        .iter()
        .map(|&id| {
            let d = adj
                .get(&id)
                .map(|neighbors| neighbors.iter().map(|(_, w)| *w).sum())
                .unwrap_or(0.0);
            (id, d)
        })
        .collect();
    let total_weight: f32 = degree.values().sum::<f32>() / 2.0;
    if total_weight <= 0.0 {
        return relabel(comm);
    }

    let mut comm_degree: HashMap<usize, f32> = HashMap::new();
    for (&node, &community) in &comm {
        *comm_degree.entry(community).or_insert(0.0) += degree[&node];
    }

    for _ in 0..options.max_passes {
        let mut moved = false;
        for &node in &nodes {
            let current = comm[&node];
            let node_degree = degree[&node];
            if node_degree == 0.0 {
                continue;
            }

            *comm_degree.entry(current).or_insert(0.0) -= node_degree;
            let mut by_neighbor_comm: HashMap<usize, f32> = HashMap::new();
            for &(neighbor, weight) in adj.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
                *by_neighbor_comm.entry(comm[&neighbor]).or_insert(0.0) += weight;
            }

            let mut best = current;
            let mut best_gain = 0.0f32;
            for (&candidate, &edge_weight_into_comm) in &by_neighbor_comm {
                let candidate_degree = comm_degree.get(&candidate).copied().unwrap_or(0.0);
                let gain = edge_weight_into_comm
                    - options.resolution * node_degree * candidate_degree / (2.0 * total_weight);
                if gain > best_gain {
                    best_gain = gain;
                    best = candidate;
                }
            }

            if best != current {
                comm.insert(node, best);
                moved = true;
            }
            *comm_degree.entry(best).or_insert(0.0) += node_degree;
        }
        if !moved {
            break;
        }
    }

    relabel(comm)
}

fn refine_connected(graph: &Graph, comm: &HashMap<NodeId, usize>) -> HashMap<NodeId, usize> {
    let adj = weighted_adjacency(graph);
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in comm {
        by_comm.entry(community).or_default().push(node);
    }

    let mut refined = HashMap::new();
    let mut next_comm = 0usize;
    for members in by_comm.values() {
        let member_set: HashSet<NodeId> = members.iter().copied().collect();
        let mut unseen = member_set.clone();
        while let Some(&start) = unseen.iter().next() {
            let mut queue = VecDeque::from([start]);
            unseen.remove(&start);
            while let Some(node) = queue.pop_front() {
                refined.insert(node, next_comm);
                for &(neighbor, _) in adj.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
                    if member_set.contains(&neighbor) && unseen.remove(&neighbor) {
                        queue.push_back(neighbor);
                    }
                }
            }
            next_comm += 1;
        }
    }
    refined
}

fn weighted_adjacency(graph: &Graph) -> HashMap<NodeId, Vec<(NodeId, f32)>> {
    let mut adj: HashMap<NodeId, Vec<(NodeId, f32)>> = HashMap::new();
    for (_, src, dst, edge) in graph.edges() {
        if src == dst {
            continue;
        }
        let weight = edge_kind_weight(edge.kind) * edge.confidence.score().max(0.05);
        adj.entry(src).or_default().push((dst, weight));
        adj.entry(dst).or_default().push((src, weight));
    }
    adj
}

fn edge_kind_weight(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::Defines => 0.7,
        EdgeKind::Calls => 1.0,
        EdgeKind::Imports => 0.45,
        EdgeKind::Inherits | EdgeKind::Implements => 1.25,
        EdgeKind::ReadsWrites => 0.85,
        EdgeKind::Mentions | EdgeKind::Describes | EdgeKind::DocumentedBy => 0.75,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor | EdgeKind::Illustrates => 0.55,
    }
}

fn relabel(mut comm: HashMap<NodeId, usize>) -> HashMap<NodeId, usize> {
    let mut labels: HashMap<usize, usize> = HashMap::new();
    let mut next = 0usize;
    for label in comm.values_mut() {
        *label = *labels.entry(*label).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
    }
    comm
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_core::{Edge, Node, NodeKind};

    #[test]
    fn louvain_clusters_dense_pairs() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));

        let comm = louvain(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&c], comm[&d]);
    }
}
