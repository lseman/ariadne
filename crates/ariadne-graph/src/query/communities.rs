//! Community detection.
//!
//! [`louvain`] runs the standard multi-level Louvain algorithm (Blondel et
//! al., 2008): each level does modularity-gain local movement, then
//! aggregates communities into super-nodes and runs again on the smaller
//! graph. Iteration stops when a level produces no movement.
//!
//! [`leiden`] adds a refinement phase between local-move and aggregation
//! (Traag et al., 2019): within each Louvain community, members are
//! re-singletoned and a constrained local-move only allows merges that
//! stay inside the parent community and only joins nodes that are
//! "well-connected" to the candidate sub-community. This guarantees the
//! returned communities are connected and meaningfully internally cohesive
//! rather than the accidental hub-and-spoke clusters single-level Louvain
//! can produce.
//!
//! Edges with [`Confidence::Ambiguous`] are skipped: those are unresolved
//! call placeholders pointing at `call::<name>` synthetic nodes and would
//! glue otherwise unrelated functions together by shared common names.

use crate::core::{Confidence, EdgeKind, Graph, NodeId};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// Tuning for community detection. The defaults are calibrated for code
/// graphs (small-world topology, hub nodes from frequently-called helpers).
///
/// Cheat sheet:
/// - Communities too small / fragmented? Lower `resolution` (e.g. 0.7) and
///   lower `well_connectedness` (e.g. 0.5).
/// - Communities too coarse / "everything in one blob"? Raise `resolution`
///   (e.g. 1.5) and/or raise `well_connectedness` (e.g. 1.5).
/// - Want determinism / smaller diffs across runs? Set `parallel = false`.
#[derive(Debug, Clone, Copy)]
pub struct CommunityOptions {
    /// Modularity resolution γ. Larger values prefer many small
    /// communities, smaller values prefer few large ones.
    pub resolution: f32,
    /// Max sweeps per level during local-move.
    pub max_passes: usize,
    /// Max hierarchy levels (aggregation steps). Caps cost on graphs that
    /// keep slowly compacting.
    pub max_levels: usize,
    /// Multiplier on the Leiden well-connectedness threshold. `1.0` is the
    /// standard formulation; `0.0` disables the check (refinement degrades
    /// to vanilla connectivity enforcement); larger values are stricter
    /// and split more aggressively.
    pub well_connectedness: f32,
    /// Minimum modularity gain to accept a move. The original Louvain
    /// paper uses `0`; a small positive value (e.g. `1e-7`) suppresses
    /// floating-point noise without changing the partition quality.
    pub min_modularity_gain: f32,
    /// Use rayon for the embarrassingly-parallel parts of refinement and
    /// aggregation. Local-move stays sequential to preserve Louvain
    /// semantics.
    pub parallel: bool,
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
        }
    }
}

pub fn louvain(graph: &Graph) -> HashMap<NodeId, usize> {
    louvain_with_options(graph, CommunityOptions::default())
}

pub fn leiden(graph: &Graph) -> HashMap<NodeId, usize> {
    leiden_with_options(graph, CommunityOptions::default())
}

pub fn louvain_with_options(graph: &Graph, options: CommunityOptions) -> HashMap<NodeId, usize> {
    let working = WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return identity_labels(working.original_nodes());
    }
    let final_labels = run_multilevel(working, options, false);
    relabel(final_labels)
}

pub fn leiden_with_options(graph: &Graph, options: CommunityOptions) -> HashMap<NodeId, usize> {
    let working = WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return identity_labels(working.original_nodes());
    }
    let final_labels = run_multilevel(working, options, true);
    relabel(final_labels)
}

/// A weighted undirected graph used during community detection. After
/// aggregation, each "node" represents a community from the previous level
/// but the same operations apply uniformly.
struct WorkingGraph {
    /// For each working-graph node, the set of original `NodeId`s it
    /// represents. Length equals the number of nodes at this level.
    members: Vec<Vec<NodeId>>,
    /// Adjacency: `adj[u] = [(v, weight), ...]`. Undirected and symmetric.
    adj: Vec<Vec<(usize, f32)>>,
    /// Self-loop weight per node (intra-community weight after aggregation).
    self_loop: Vec<f32>,
    /// Sum of incident edge weights per node, including 2 * self_loop.
    degree: Vec<f32>,
    /// Sum of all edge weights (counting each undirected edge once;
    /// self-loops counted once at full weight).
    total_weight: f32,
}

impl WorkingGraph {
    fn from_graph(graph: &Graph) -> Self {
        let nodes: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
        let index: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let n = nodes.len();
        let mut adj_map: Vec<HashMap<usize, f32>> = vec![HashMap::new(); n];
        let mut self_loop = vec![0.0f32; n];

        for (_, src, dst, edge) in graph.edges() {
            if matches!(edge.confidence, Confidence::Ambiguous) {
                continue;
            }
            let Some(&u) = index.get(&src) else { continue };
            let Some(&v) = index.get(&dst) else { continue };
            let weight = edge_kind_weight(edge.kind) * edge.confidence.score().max(0.05);
            if weight <= 0.0 {
                continue;
            }
            if u == v {
                self_loop[u] += weight;
                continue;
            }
            // Project directed multigraph to undirected: sum both directions.
            *adj_map[u].entry(v).or_insert(0.0) += weight;
            *adj_map[v].entry(u).or_insert(0.0) += weight;
        }

        let adj: Vec<Vec<(usize, f32)>> = adj_map
            .into_iter()
            .map(|m| m.into_iter().collect())
            .collect();

        let degree: Vec<f32> = (0..n)
            .map(|u| adj[u].iter().map(|(_, w)| *w).sum::<f32>() + 2.0 * self_loop[u])
            .collect();
        let total_weight = degree.iter().sum::<f32>() / 2.0;

        let members: Vec<Vec<NodeId>> = nodes.iter().map(|&id| vec![id]).collect();
        Self {
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

/// Drive Louvain/Leiden level-by-level until no movement.
///
/// Returns a mapping from each *original* `NodeId` to its final community
/// label (in some arbitrary but stable numbering).
fn run_multilevel(
    mut working: WorkingGraph,
    options: CommunityOptions,
    refine: bool,
) -> HashMap<NodeId, usize> {
    // Tracks the community label for each original node at the current level.
    // After every aggregation we rewrite this so that the labels refer to
    // super-nodes of the new level.
    let mut original_to_super: HashMap<NodeId, usize> = working
        .original_nodes()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();

    for _ in 0..options.max_levels {
        let partition = local_move(&working, options);
        let distinct: HashSet<usize> = partition.iter().copied().collect();
        let moved = distinct.len() < working.len();

        // Refinement (Leiden): split partitions into well-connected sub-pieces.
        // The aggregation uses the *refined* partition (so weakly-connected
        // communities split into multiple super-nodes), and we also report
        // refined labels back to the caller — that's the whole point of the
        // Leiden guarantee.
        let aggregation_partition: Vec<usize> = if refine {
            refinement_phase(&working, &partition, options)
        } else {
            densify(&partition)
        };

        // Update original-node → super-node mapping using the dense
        // aggregation partition. Densification matters: super-node indices
        // in the *next* working graph are dense [0, k), so the labels we
        // store here must be in that range to index into level-(L+1)
        // partition vectors.
        for super_node in original_to_super.values_mut() {
            *super_node = aggregation_partition[*super_node];
        }

        if !moved {
            return original_to_super;
        }

        working = aggregate(working, &aggregation_partition);
        if working.len() <= 1 {
            break;
        }
    }

    original_to_super
}

/// Single Louvain local-move pass: iterate until a full sweep produces no
/// node movement, or `max_passes` is reached. Each node greedily picks the
/// neighboring community that maximises modularity gain.
fn local_move(working: &WorkingGraph, options: CommunityOptions) -> Vec<usize> {
    let n = working.len();
    let mut comm: Vec<usize> = (0..n).collect();
    let mut comm_degree: Vec<f32> = working.degree.clone();
    let two_m = 2.0 * working.total_weight;
    if two_m <= 0.0 {
        return comm;
    }

    for _ in 0..options.max_passes {
        let mut moved = false;
        for u in 0..n {
            let current = comm[u];
            let node_degree = working.degree[u];
            if node_degree == 0.0 {
                continue;
            }

            // Remove u from its current community for the gain calculation.
            comm_degree[current] -= node_degree;

            // Sum edge weights from u into each neighbouring community.
            let mut weight_to_comm: HashMap<usize, f32> = HashMap::new();
            for &(v, w) in &working.adj[u] {
                *weight_to_comm.entry(comm[v]).or_insert(0.0) += w;
            }

            let mut best = current;
            let mut best_gain = options.min_modularity_gain;
            // Always allow staying put: the baseline gain is the weight u
            // already has into its own (now u-less) community.
            let stay_weight = weight_to_comm.get(&current).copied().unwrap_or(0.0);
            let stay_gain = stay_weight
                - options.resolution * node_degree * comm_degree[current] / two_m;
            if stay_gain > best_gain {
                best_gain = stay_gain;
                best = current;
            }
            for (&candidate, &edge_weight) in &weight_to_comm {
                if candidate == current {
                    continue;
                }
                let gain = edge_weight
                    - options.resolution * node_degree * comm_degree[candidate] / two_m;
                if gain > best_gain {
                    best_gain = gain;
                    best = candidate;
                }
            }

            comm[u] = best;
            comm_degree[best] += node_degree;
            if best != current {
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }

    comm
}

/// Leiden refinement.
///
/// Within each community from `partition`, restart all members as
/// singletons, then do a constrained local-move that only allows nodes to
/// merge into sub-communities that (a) live inside the same parent
/// community and (b) the node is "well-connected" to, where
/// well-connected means the edge weight from the node into the
/// sub-community exceeds the modularity threshold for the parent.
///
/// The output is a refined partition over the same nodes, and is what
/// drives the aggregation step. Returned labels are dense.
fn refinement_phase(
    working: &WorkingGraph,
    partition: &[usize],
    options: CommunityOptions,
) -> Vec<usize> {
    let n = working.len();
    let two_m = 2.0 * working.total_weight;
    if two_m <= 0.0 {
        return partition.to_vec();
    }

    // Group nodes by parent community. Sort parents by id so the label
    // numbering is deterministic regardless of HashMap iteration order
    // (matters when comparing parallel vs sequential runs).
    let mut by_parent: HashMap<usize, Vec<usize>> = HashMap::new();
    for (u, &c) in partition.iter().enumerate() {
        by_parent.entry(c).or_default().push(u);
    }
    let mut parents: Vec<(usize, Vec<usize>)> = by_parent.into_iter().collect();
    parents.sort_by_key(|(p, _)| *p);

    // Total weight inside each parent community, used to anchor the
    // Leiden well-connectedness threshold.
    let mut parent_degree: HashMap<usize, f32> = HashMap::new();
    for u in 0..n {
        *parent_degree.entry(partition[u]).or_insert(0.0) += working.degree[u];
    }

    // Pre-allocate a disjoint label range per parent so each parent task
    // can write into its own slice of the output without touching any
    // other parent's labels. Single-member parents skip refinement (they
    // keep their parent label, remapped to the parent's base).
    let mut label_base = Vec::with_capacity(parents.len());
    let mut cursor = 0usize;
    for (_, members) in &parents {
        label_base.push(cursor);
        cursor += members.len();
    }
    let total_labels = cursor.max(1);

    // Each parent task returns the refined label *within its own range*
    // for every member, in member order.
    let refine_parent = |idx: usize| -> Vec<usize> {
        let (parent, members) = &parents[idx];
        let base = label_base[idx];
        let parent_total = parent_degree.get(parent).copied().unwrap_or(0.0);

        if members.len() <= 1 {
            // Single-member parent: still take a label slot to keep the
            // mapping dense and deterministic.
            return vec![base];
        }
        let member_set: HashSet<usize> = members.iter().copied().collect();
        // Local refined labels live in [base, base + members.len()).
        let mut refined: HashMap<usize, usize> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (u, base + i))
            .collect();
        let mut local_degree: HashMap<usize, f32> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (base + i, working.degree[u]))
            .collect();

        for _ in 0..options.max_passes {
            let mut moved = false;
            for &u in members {
                let current = refined[&u];
                let node_degree = working.degree[u];
                if node_degree == 0.0 {
                    continue;
                }
                *local_degree.get_mut(&current).unwrap() -= node_degree;

                let mut weight_to_comm: HashMap<usize, f32> = HashMap::new();
                for &(v, w) in &working.adj[u] {
                    if !member_set.contains(&v) {
                        continue;
                    }
                    *weight_to_comm.entry(refined[&v]).or_insert(0.0) += w;
                }

                let mut best = current;
                let mut best_gain = options.min_modularity_gain;
                let stay_weight = weight_to_comm.get(&current).copied().unwrap_or(0.0);
                let stay_deg = local_degree.get(&current).copied().unwrap_or(0.0);
                let stay_gain = stay_weight
                    - options.resolution * node_degree * stay_deg / two_m;
                if stay_gain > best_gain {
                    best_gain = stay_gain;
                    best = current;
                }
                for (&candidate, &edge_weight) in &weight_to_comm {
                    if candidate == current {
                        continue;
                    }
                    let cand_deg = local_degree.get(&candidate).copied().unwrap_or(0.0);
                    // Well-connectedness gate. Scaled by the user knob so
                    // the strictness is tunable; 0 disables the gate.
                    let threshold = options.well_connectedness
                        * options.resolution
                        * cand_deg
                        * (parent_total - cand_deg)
                        / (two_m * parent_total.max(1e-9));
                    if edge_weight < threshold {
                        continue;
                    }
                    let gain = edge_weight
                        - options.resolution * node_degree * cand_deg / two_m;
                    if gain > best_gain {
                        best_gain = gain;
                        best = candidate;
                    }
                }

                refined.insert(u, best);
                *local_degree.entry(best).or_insert(0.0) += node_degree;
                if best != current {
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }

        members.iter().map(|u| refined[u]).collect()
    };

    // Run all parents. Each parent operates on a disjoint label range and
    // its own node-set, so there is zero cross-parent contention: this is
    // safe for rayon without any locking.
    let per_parent_labels: Vec<Vec<usize>> = if options.parallel {
        (0..parents.len()).into_par_iter().map(refine_parent).collect()
    } else {
        (0..parents.len()).map(refine_parent).collect()
    };

    // Assemble the global refined vector. Initialise to a sentinel so we
    // can verify every node got a label (every node is in exactly one
    // parent).
    let mut refined: Vec<usize> = vec![total_labels; n];
    for (idx, labels) in per_parent_labels.iter().enumerate() {
        let (_, members) = &parents[idx];
        debug_assert_eq!(members.len(), labels.len());
        for (&u, &label) in members.iter().zip(labels.iter()) {
            refined[u] = label;
        }
    }

    // Enforce connectivity inside each refined label — the Leiden
    // guarantee. Disconnected pieces get fresh labels beyond the
    // pre-allocated range, which densify() will compress.
    enforce_connected(working, &mut refined);
    densify(&refined)
}

/// Split any community whose induced subgraph is disconnected into one
/// label per connected component. Operates on the working graph.
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

/// Build the next level of the hierarchy: each community in `partition`
/// becomes a single node whose incident edges are the summed weights to
/// other communities.
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
        // Previous self-loops collapse into the new self-loop on cu.
        self_loop[cu] += prev.self_loop[u];
        for &(v, w) in &prev.adj[u] {
            let cv = dense[v];
            if cu == cv {
                // Each intra-community undirected edge appears twice (u→v
                // and v→u in the symmetric adjacency); add half to the
                // self-loop weight.
                self_loop[cu] += w * 0.5;
            } else {
                *adj_map[cu].entry(cv).or_insert(0.0) += w;
            }
        }
    }

    // Symmetrise: the loop above already adds w in both directions because
    // adj is symmetric, so no extra work is needed. But we need to make
    // sure self-loop double-count is avoided. The loop adds 0.5*w for each
    // direction of an intra-community edge, summing to w — the correct
    // self-loop weight for one undirected edge. Good.

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
        EdgeKind::Defines => 0.7,
        EdgeKind::Calls => 1.0,
        EdgeKind::Imports => 0.45,
        EdgeKind::Inherits | EdgeKind::Implements => 1.25,
        EdgeKind::ReadsWrites => 0.85,
        EdgeKind::Mentions | EdgeKind::Describes | EdgeKind::DocumentedBy => 0.75,
        EdgeKind::SimilarTo | EdgeKind::RationaleFor | EdgeKind::Illustrates => 0.55,
        // Tests cluster with the code they exercise, but more loosely than
        // structural call/inherit edges.
        EdgeKind::TestedBy => 0.6,
        // Flow bookkeeping. Tiny weight so members of the same flow lean
        // toward the same community without flow nodes becoming hubs.
        EdgeKind::MemberOf | EdgeKind::EntryOf => 0.1,
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
    use crate::core::{Edge, Node, NodeKind};

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
        assert_ne!(comm[&a], comm[&c]);
    }

    #[test]
    fn louvain_aggregates_two_dense_triangles_through_a_bridge() {
        // Two triangles {a,b,c} and {d,e,f} joined by a single weak edge
        // c—d. Multi-level Louvain should keep them separate; the bridge
        // edge alone shouldn't be enough to merge them at any level.
        let mut g = Graph::new();
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
        // Two isolated components — leiden must place them in different
        // communities even if some adversarial assignment merged them.
        let mut g = Graph::new();
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
    fn leiden_parallel_matches_sequential() {
        // Build a moderate graph with several disjoint clusters joined by
        // weak bridges, so the refinement phase has multiple parents to
        // process — that's where parallelism kicks in.
        let mut g = Graph::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            nodes.push(g.add_node(Node::new(NodeKind::Function, format!("n{i}"))));
        }
        // Three triangles, daisy-chained with single bridge edges.
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

        let opts_seq = CommunityOptions { parallel: false, ..Default::default() };
        let opts_par = CommunityOptions { parallel: true, ..Default::default() };

        let seq = leiden_with_options(&g, opts_seq);
        let par = leiden_with_options(&g, opts_par);

        // Compare partitions by their equivalence classes (label values may
        // differ between runs, but the grouping must match).
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
        // With well_connectedness = 0 the threshold is 0, so refinement is
        // pure local-move + connectivity enforcement. Just check it
        // doesn't panic and still returns a valid partition.
        let mut g = Graph::new();
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
        // Two real-call clusters that would be glued together by an
        // ambiguous placeholder edge if we counted Confidence::Ambiguous.
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::common_name"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));
        g.add_edge(d, c, Edge::extracted(EdgeKind::Calls));
        // Both groups call the same ambiguous helper.
        g.add_edge(a, placeholder, Edge::ambiguous(EdgeKind::Calls));
        g.add_edge(c, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let comm = louvain(&g);
        assert_ne!(
            comm[&a], comm[&c],
            "ambiguous placeholder must not glue otherwise separate clusters"
        );
    }
}
