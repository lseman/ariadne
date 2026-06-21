//! Community detection algorithms.

mod gaps;
mod infomap;
mod leiden;
mod louvain;
mod quality;
mod split;

use crate::core::{Confidence, EdgeKind, Graph, NodeId};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommunityObjective {
    #[default]
    Modularity,
    Cpm,
}

#[derive(Debug, Clone, Copy)]
pub struct CommunityOptions {
    pub resolution: f32,
    pub max_passes: usize,
    pub max_levels: usize,
    pub well_connectedness: f32,
    pub min_modularity_gain: f32,
    pub parallel: bool,
    pub objective: CommunityObjective,
}

impl Default for CommunityOptions {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            max_passes: 50,
            max_levels: 10,
            well_connectedness: 1.0,
            min_modularity_gain: 1e-7,
            parallel: true,
            objective: CommunityObjective::default(),
        }
    }
}

/// Internal working graph for community algorithms.
/// Each index represents a "super-node" that may contain multiple original nodes.
#[derive(Clone)]
struct WorkingGraph {
    /// Members of each super-node.
    members: Vec<Vec<NodeId>>,
    /// Adjacency: adj[i] = [(j, weight), ...]
    adj: Vec<Vec<(usize, f32)>>,
    /// Self-loop weight for each super-node.
    self_loop: Vec<f32>,
    /// Total degree (self-loops counted twice).
    degree: Vec<f32>,
    /// Total graph weight.
    total_weight: f32,
}

impl WorkingGraph {
    fn from_graph(graph: &Graph) -> Self {
        let nodes: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
        let n = nodes.len();

        // Initialize: each original node is its own super-node
        let members: Vec<Vec<NodeId>> = (0..n).map(|i| vec![nodes[i]]).collect();
        let mut adj: Vec<HashMap<usize, f32>> = vec![HashMap::new(); n];
        let mut self_loop = vec![0.0f32; n];

        for (_, src, dst, edge) in graph.edges() {
            let src_idx = match nodes.iter().position(|&x| x == src) {
                Some(i) => i,
                None => continue,
            };
            let dst_idx = match nodes.iter().position(|&x| x == dst) {
                Some(i) => i,
                None => continue,
            };
            let weight = match edge.confidence {
                Confidence::Ambiguous => 0.15,
                _ => edge_kind_weight(edge.kind),
            };
            *adj[src_idx].entry(dst_idx).or_insert(0.0) += weight;
            if src == dst {
                self_loop[src_idx] += weight * 0.5;
            }
        }

        let adj: Vec<Vec<(usize, f32)>> =
            adj.into_iter().map(|m| m.into_iter().collect()).collect();

        let degree: Vec<f32> = (0..n)
            .map(|u| adj[u].iter().map(|(_, w)| *w).sum::<f32>() + 2.0 * self_loop[u])
            .collect();
        let total_weight = degree.iter().sum::<f32>() / 2.0;

        WorkingGraph {
            members,
            adj,
            self_loop,
            degree,
            total_weight,
        }
    }

    fn len(&self) -> usize {
        self.members.len()
    }

    fn original_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.members.iter().flat_map(|m| m.iter().copied())
    }
}

fn identity_labels(nodes: impl Iterator<Item = NodeId>) -> HashMap<NodeId, usize> {
    nodes.enumerate().map(|(i, id)| (id, i)).collect()
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

fn densify(labels: &[usize]) -> Vec<usize> {
    let mut mapping: HashMap<usize, usize> = HashMap::new();
    let mut next = 0usize;
    labels
        .iter()
        .map(|&l| {
            *mapping.entry(l).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            })
        })
        .collect()
}

fn aggregate(prev: WorkingGraph, partition: &[usize]) -> WorkingGraph {
    let dense = densify(partition);
    let new_n = dense.iter().copied().max().map(|x| x + 1).unwrap_or(0);

    let mut new_members: Vec<Vec<NodeId>> = vec![Vec::new(); new_n];
    for (u, members) in prev.members.iter().enumerate() {
        new_members[dense[u]].extend(members.iter().copied());
    }

    let mut adj_map: Vec<HashMap<usize, f32>> = vec![HashMap::new(); new_n];
    let mut self_loop = vec![0.0f32; new_n];

    for u in 0..prev.len() {
        let cu = dense[u];
        self_loop[cu] += prev.self_loop[u];
        for &(v, w) in &prev.adj[u] {
            let cv = dense[v];
            if cu == cv {
                self_loop[cu] += w * 0.5;
            } else {
                *adj_map[cu].entry(cv).or_insert(0.0) += w;
            }
        }
    }

    let adj: Vec<Vec<(usize, f32)>> = adj_map
        .into_iter()
        .map(|m| m.into_iter().collect())
        .collect();

    let degree: Vec<f32> = (0..new_n)
        .map(|u| adj[u].iter().map(|(_, w)| *w).sum::<f32>() + 2.0 * self_loop[u])
        .collect();
    let total_weight = degree.iter().sum::<f32>() / 2.0;

    WorkingGraph {
        members: new_members,
        adj,
        self_loop,
        degree,
        total_weight,
    }
}

fn edge_kind_weight(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::Inherits | EdgeKind::Implements => 1.25,
        EdgeKind::Defines => 0.7,
        EdgeKind::Calls => 0.55,
        EdgeKind::ReadsWrites => 0.85,
        EdgeKind::Mentions | EdgeKind::Describes | EdgeKind::DocumentedBy => 0.75,
        EdgeKind::TestedBy => 0.6,
        EdgeKind::Imports => 0.45,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor | EdgeKind::Illustrates => 0.55,
        EdgeKind::MemberOf | EdgeKind::EntryOf => 0.1,
    }
}

fn enforce_connected(working: &WorkingGraph, labels: &mut [usize]) {
    let n = working.len();
    let mut by_label: HashMap<usize, Vec<usize>> = HashMap::new();
    for (u, &c) in labels.iter().enumerate() {
        by_label.entry(c).or_default().push(u);
    }

    let mut next = labels.iter().copied().max().map(|x| x + 1).unwrap_or(0);
    let mut new_labels = vec![None; n];

    for members in by_label.values() {
        let member_set: HashSet<usize> = members.iter().copied().collect();
        let mut unseen = member_set.clone();
        let mut first_component = true;
        while let Some(&start) = unseen.iter().next() {
            let component_label = if first_component {
                first_component = false;
                labels[start]
            } else {
                let l = next;
                next += 1;
                l
            };
            let mut queue = VecDeque::from([start]);
            unseen.remove(&start);
            while let Some(u) = queue.pop_front() {
                new_labels[u] = Some(component_label);
                for &(v, _) in &working.adj[u] {
                    if member_set.contains(&v) && unseen.remove(&v) {
                        queue.push_back(v);
                    }
                }
            }
        }
    }

    for (u, label) in new_labels.into_iter().enumerate() {
        if let Some(l) = label {
            labels[u] = l;
        }
    }
}

// Re-export public API
pub use gaps::knowledge_gaps;
pub use infomap::{infomap, infomap_with_options};
pub use leiden::{leiden, leiden_with_options};
pub use louvain::{louvain, louvain_with_options};
pub use quality::{
    community_cohesion, community_quality, CommunityQuality, LOW_COHESION_THRESHOLD,
};
pub use split::split_oversized;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, Node, NodeKind};

    #[test]
    fn louvain_clusters_dense_pairs() {
        let mut g = crate::Graph::new();
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
        assert_ne!(comm[&a], comm[&c]);
    }

    #[test]
    fn louvain_aggregates_two_dense_triangles_through_a_bridge() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        let e = g.add_node(Node::new(NodeKind::Function, "e"));
        let f = g.add_node(Node::new(NodeKind::Function, "f"));
        for &(u, v) in &[(a, b), (b, c), (a, c), (d, e), (e, f), (d, f)] {
            g.add_edge(u, v, Edge::extracted(EdgeKind::Calls));
            g.add_edge(v, u, Edge::extracted(EdgeKind::Calls));
        }
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        let comm = louvain(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&b], comm[&c]);
        assert_eq!(comm[&d], comm[&e]);
        assert_eq!(comm[&e], comm[&f]);
        assert_ne!(comm[&a], comm[&d]);
    }

    #[test]
    fn leiden_splits_disconnected_pieces() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        let comm = leiden(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&c], comm[&d]);
        assert_ne!(comm[&a], comm[&c]);
    }

    #[test]
    fn community_quality_reports_connectedness_and_modularity() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));

        let comm = leiden(&g);
        let quality = community_quality(
            &g,
            &comm,
            CommunityOptions {
                resolution: 1.0,
                ..Default::default()
            },
        );

        assert_eq!(quality.community_count, 2);
        assert_eq!(quality.disconnected_communities, 0);
        assert_eq!(quality.min_size, 2);
        assert_eq!(quality.max_size, 2);
        assert!(quality.score > 0.0);
        assert_eq!(quality.max_conductance, 0.0);
        assert_eq!(quality.mean_cohesion, 1.0);
        assert_eq!(quality.low_cohesion_communities, 0);
    }

    #[test]
    fn cohesion_measures_internal_density() {
        let mut g = crate::Graph::new();
        let hub = g.add_node(Node::new(NodeKind::Function, "hub"));
        let mut members: HashMap<NodeId, usize> = HashMap::from([(hub, 0)]);
        for i in 0..4 {
            let leaf = g.add_node(Node::new(NodeKind::Function, format!("leaf{i}")));
            g.add_edge(hub, leaf, Edge::extracted(EdgeKind::Calls));
            g.add_edge(leaf, hub, Edge::extracted(EdgeKind::Calls));
            members.insert(leaf, 0);
        }
        let lone = g.add_node(Node::new(NodeKind::Function, "lone"));
        members.insert(lone, 1);

        let cohesion = community_cohesion(&g, &members);
        assert!((cohesion[&0] - 0.4).abs() < 1e-6, "got {}", cohesion[&0]);
        assert_eq!(cohesion[&1], 1.0);

        let quality = community_quality(&g, &members, CommunityOptions::default());
        assert!((quality.mean_cohesion - 0.4).abs() < 1e-6);
        assert_eq!(quality.low_cohesion_communities, 0);
    }

    #[test]
    fn leiden_parallel_matches_sequential() {
        let mut g = crate::Graph::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            nodes.push(g.add_node(Node::new(NodeKind::Function, format!("n{i}"))));
        }
        for chunk in nodes.chunks(10) {
            for i in 0..chunk.len() {
                for j in (i + 1)..chunk.len() {
                    g.add_edge(chunk[i], chunk[j], Edge::extracted(EdgeKind::Calls));
                    g.add_edge(chunk[j], chunk[i], Edge::extracted(EdgeKind::Calls));
                }
            }
        }
        g.add_edge(nodes[9], nodes[10], Edge::extracted(EdgeKind::Calls));
        g.add_edge(nodes[19], nodes[20], Edge::extracted(EdgeKind::Calls));

        let opts_seq = CommunityOptions {
            parallel: false,
            ..Default::default()
        };
        let opts_par = CommunityOptions {
            parallel: true,
            ..Default::default()
        };

        let seq = leiden_with_options(&g, opts_seq);
        let par = leiden_with_options(&g, opts_par);

        let same_partition = |a: &HashMap<NodeId, usize>, b: &HashMap<NodeId, usize>| {
            for &x in &nodes {
                for &y in &nodes {
                    if (a[&x] == a[&y]) != (b[&x] == b[&y]) {
                        return false;
                    }
                }
            }
            true
        };
        assert!(
            same_partition(&seq, &par),
            "parallel and sequential Leiden must agree on the partition"
        );
    }

    #[test]
    fn well_connectedness_zero_disables_threshold() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        for &(u, v) in &[(a, b), (b, c), (c, d), (d, a)] {
            g.add_edge(u, v, Edge::extracted(EdgeKind::Calls));
        }
        let opts = CommunityOptions {
            well_connectedness: 0.0,
            ..Default::default()
        };
        let comm = leiden_with_options(&g, opts);
        assert_eq!(comm.len(), 4);
    }

    #[test]
    fn ambiguous_edges_are_ignored() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::common_name"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));
        g.add_edge(a, placeholder, Edge::ambiguous(EdgeKind::Calls));
        g.add_edge(c, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let comm = louvain(&g);
        assert_ne!(
            comm[&a], comm[&c],
            "ambiguous placeholder must not glue otherwise separate clusters"
        );
    }

    #[test]
    fn infomap_clusters_dense_pairs() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));

        let comm = infomap(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&c], comm[&d]);
        assert_ne!(comm[&a], comm[&c]);
    }

    #[test]
    fn infomap_aggregates_two_dense_triangles_through_a_bridge() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        let e = g.add_node(Node::new(NodeKind::Function, "e"));
        let f = g.add_node(Node::new(NodeKind::Function, "f"));
        for &(u, v) in &[(a, b), (b, c), (a, c), (d, e), (e, f), (d, f)] {
            g.add_edge(u, v, Edge::extracted(EdgeKind::Calls));
            g.add_edge(v, u, Edge::extracted(EdgeKind::Calls));
        }
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        let comm = infomap(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&b], comm[&c]);
        assert_eq!(comm[&d], comm[&e]);
        assert_eq!(comm[&e], comm[&f]);
        assert_ne!(comm[&a], comm[&d]);
    }

    #[test]
    fn infomap_splits_disconnected_pieces() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        let comm = infomap(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&c], comm[&d]);
        assert_ne!(comm[&a], comm[&c]);
    }

    #[test]
    fn infomap_ambiguous_edges_do_not_glue_clusters() {
        let mut g = crate::Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::common"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));
        g.add_edge(a, placeholder, Edge::ambiguous(EdgeKind::Calls));
        g.add_edge(c, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let comm = infomap(&g);
        assert_ne!(
            comm[&a], comm[&c],
            "ambiguous placeholder must not glue separate clusters"
        );
    }

    #[test]
    fn infomap_parallel_matches_sequential() {
        let mut g = crate::Graph::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            nodes.push(g.add_node(Node::new(NodeKind::Function, format!("n{i}"))));
        }
        for chunk in nodes.chunks(10) {
            for i in 0..chunk.len() {
                for j in (i + 1)..chunk.len() {
                    g.add_edge(chunk[i], chunk[j], Edge::extracted(EdgeKind::Calls));
                    g.add_edge(chunk[j], chunk[i], Edge::extracted(EdgeKind::Calls));
                }
            }
        }
        g.add_edge(nodes[9], nodes[10], Edge::extracted(EdgeKind::Calls));
        g.add_edge(nodes[19], nodes[20], Edge::extracted(EdgeKind::Calls));

        let opts_seq = CommunityOptions {
            parallel: false,
            ..Default::default()
        };
        let opts_par = CommunityOptions {
            parallel: true,
            ..Default::default()
        };

        let seq = infomap_with_options(&g, opts_seq);
        let par = infomap_with_options(&g, opts_par);

        let same_partition = |a: &HashMap<NodeId, usize>, b: &HashMap<NodeId, usize>| {
            for &x in &nodes {
                for &y in &nodes {
                    if (a[&x] == a[&y]) != (b[&x] == b[&y]) {
                        return false;
                    }
                }
            }
            true
        };
        assert!(
            same_partition(&seq, &par),
            "parallel and sequential Infomap must agree on the partition"
        );
    }

    #[test]
    fn infomap_handles_large_dense_chunks() {
        let mut g = crate::Graph::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            nodes.push(g.add_node(Node::new(NodeKind::Function, format!("n{i}"))));
        }
        for chunk in nodes.chunks(10) {
            for i in 0..chunk.len() {
                for j in (i + 1)..chunk.len() {
                    g.add_edge(chunk[i], chunk[j], Edge::extracted(EdgeKind::Calls));
                    g.add_edge(chunk[j], chunk[i], Edge::extracted(EdgeKind::Calls));
                }
            }
        }
        g.add_edge(nodes[9], nodes[10], Edge::extracted(EdgeKind::Calls));
        g.add_edge(nodes[19], nodes[20], Edge::extracted(EdgeKind::Calls));

        let map = infomap(&g);

        for chunk in nodes.chunks(10) {
            let first = map[&chunk[0]];
            assert!(chunk.iter().all(|node| map[node] == first));
        }
        assert_ne!(map[&nodes[0]], map[&nodes[10]]);
        assert_ne!(map[&nodes[10]], map[&nodes[20]]);
    }
}
