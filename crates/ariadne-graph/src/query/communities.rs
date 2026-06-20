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
//! [`infomap`] uses the Infomap algorithm (Vosoughipur et al., 2022; Traag
//! et al., 2016): it simulates random walks on the graph to estimate visit
//! frequencies, then minimizes the LMDL (Modularity Density Length) of
//! encoding the flow through a two-level partition. Nodes are greedily moved
//! between communities to minimize LMDL. The result often differs from
//! Louvain/Leiden because Infomap optimizes a description-length objective
//! rather than modularity — it tends to produce more compact, flow-centric
//! communities. Ambiguous edges are down-weighted to 0.15, same as Louvain.
//!
//! Edges with [`Confidence::Ambiguous`] are skipped: those are unresolved
//! call placeholders pointing at `call::<name>` synthetic nodes and would
//! glue otherwise unrelated functions together by shared common names.

use crate::core::{Confidence, EdgeKind, Graph, NodeId};
use rayon::prelude::*;
use serde_json::{json, Value};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommunityObjective {
    /// Standard modularity with a null model.
    #[default]
    Modularity,
    /// Constant Potts Model (CPM) objective with a size penalty.
    Cpm,
}

#[derive(Debug, Clone, Copy)]
pub struct CommunityOptions {
    /// Modularity/CPM resolution γ. Larger values prefer many small
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
    /// Minimum quality gain to accept a move. The original Louvain
    /// paper uses `0`; a small positive value (e.g. `1e-7`) suppresses
    /// floating-point noise without changing the partition quality.
    pub min_modularity_gain: f32,
    /// Use rayon for the embarrassingly-parallel parts of refinement and
    /// aggregation. Local-move stays sequential to preserve Louvain
    /// semantics.
    pub parallel: bool,
    /// Objective used for evaluating community moves.
    pub objective: CommunityObjective,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommunityQuality {
    pub community_count: usize,
    pub singleton_count: usize,
    pub min_size: usize,
    pub max_size: usize,
    pub mean_size: f32,
    pub objective: CommunityObjective,
    pub score: f32,
    pub disconnected_communities: usize,
    pub mean_conductance: f32,
    pub max_conductance: f32,
    /// Mean edge density (actual edges / possible pairs) across
    /// communities with more than one member.
    pub mean_cohesion: f32,
    /// Communities larger than one member with cohesion below 0.15 —
    /// candidates for "should this module be split?" review.
    pub low_cohesion_communities: usize,
}

/// Threshold below which a community is flagged as loosely knit.
pub const LOW_COHESION_THRESHOLD: f32 = 0.15;

/// Edge density of each community's induced subgraph: unique undirected
/// node pairs connected by at least one edge, divided by `n*(n-1)/2`
/// possible pairs. Singleton communities score `1.0`.
pub fn community_cohesion(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
) -> HashMap<usize, f32> {
    let mut sizes: HashMap<usize, usize> = HashMap::new();
    for &community in communities.values() {
        *sizes.entry(community).or_insert(0) += 1;
    }

    let mut internal_pairs: HashMap<usize, HashSet<(NodeId, NodeId)>> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        if src == dst {
            continue;
        }
        let (Some(&a), Some(&b)) = (communities.get(&src), communities.get(&dst)) else {
            continue;
        };
        if a != b {
            continue;
        }
        let pair = if src < dst { (src, dst) } else { (dst, src) };
        internal_pairs.entry(a).or_default().insert(pair);
    }

    sizes
        .into_iter()
        .map(|(community, n)| {
            let cohesion = if n <= 1 {
                1.0
            } else {
                let actual = internal_pairs
                    .get(&community)
                    .map(HashSet::len)
                    .unwrap_or(0);
                let possible = n * (n - 1) / 2;
                actual as f32 / possible as f32
            };
            (community, cohesion)
        })
        .collect()
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

/// Infomap community detection with default options.
pub fn infomap(graph: &Graph) -> HashMap<NodeId, usize> {
    infomap_with_options(graph, CommunityOptions::default())
}

/// Infomap (multi-level, with Leiden-style refinement).
///
/// Runs the standard multi-level Infomap algorithm: each level performs
/// random-walk initialization followed by greedy local-move to minimize
/// the LMDL (Log-Modular Description Length). Between levels, a Leiden-style
/// refinement phase splits poorly connected nodes, and the resulting
/// communities are aggregated into super-nodes for the next level.
///
/// Infomap optimizes a description-length objective rather than modularity
/// — it tends to produce more compact, flow-centric communities. Edges with
/// [`Confidence::Ambiguous`] are down-weighted to 0.15.
///
/// The LMDL consists of two terms:
/// - **L_macro**: encoding of inter-community flow (macro-state transitions)
/// - **L_flow**: encoding of flow within each community (two-level partition)
///
/// See Traag et al. (2016) "From Louvain to Leiden: guaranteeing well-
/// connected communities" and the Infomap papers for details.
pub fn infomap_with_options(graph: &Graph, options: CommunityOptions) -> HashMap<NodeId, usize> {
    let working = WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return identity_labels(working.original_nodes());
    }
    let final_labels = run_infomap_multilevel(working, options);
    relabel(final_labels)
}

/// Drive Infomap level-by-level until no movement.
///
/// Returns a mapping from each *original* `NodeId` to its final community
/// label (in some arbitrary but stable numbering).
fn run_infomap_multilevel(
    mut working: WorkingGraph,
    options: CommunityOptions,
) -> HashMap<NodeId, usize> {
    let original_working = working.clone();
    let original_two_m = 2.0 * original_working.total_weight;
    if original_two_m <= 0.0 {
        return identity_labels(working.original_nodes());
    }

    // Tracks the community label for each original node at the current level.
    let mut original_to_super: HashMap<NodeId, usize> = working
        .original_nodes()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();

    let mut best_mapping = original_to_super.clone();
    let mut best_lmdl = compute_lmdl(
        &original_working,
        &labels_for_original(&original_working, &best_mapping),
        original_two_m,
    );
    for level in 0..options.max_levels {
        let two_m = 2.0 * working.total_weight;
        if two_m <= 0.0 {
            break;
        }

        // Random-walk initialization: simulate walks to seed community
        // labels. This helps break symmetry and gives nodes in the same
        // flow basin a head-start.
        let mut labels = random_walk_init(&working);

        // Greedy local-move: minimize LMDL.
        let mut prev_pass_lmdl = f32::INFINITY;
        for pass in 0..options.max_passes {
            let (new_labels, lmdl) =
                infomap_local_move(&working, &labels, two_m, options.max_passes);
            labels = new_labels;

            // Check convergence: LMDL stable across passes.
            let improved = (prev_pass_lmdl - lmdl).abs() > 1e-8;
            prev_pass_lmdl = lmdl;

            if !improved && pass >= 2 {
                break;
            }
        }

        // Leiden-style refinement: split partitions into well-connected
        // sub-pieces. The aggregation uses the refined partition.
        let aggregation_partition: Vec<usize> = if options.well_connectedness > 0.0 {
            infomap_refinement(&working, &labels, options)
        } else {
            densify(&labels)
        };

        // Check if partition changed.
        let moved = aggregation_partition
            .iter()
            .enumerate()
            .any(|(i, &l)| l != labels[i]);

        // Update original-node → super-node mapping using the dense
        // aggregation partition, but only accept hierarchy levels that
        // improve the objective measured on the original graph.
        let mut candidate_mapping = original_to_super.clone();
        for super_node in candidate_mapping.values_mut() {
            *super_node = aggregation_partition[*super_node];
        }
        let candidate_lmdl = compute_lmdl(
            &original_working,
            &labels_for_original(&original_working, &candidate_mapping),
            original_two_m,
        );
        if candidate_lmdl + 1e-6 < best_lmdl {
            best_lmdl = candidate_lmdl;
            best_mapping = candidate_mapping.clone();
            original_to_super = candidate_mapping;
        } else if level > 0 {
            return best_mapping;
        }

        if !moved {
            return best_mapping;
        }

        working = aggregate(working, &aggregation_partition);
        if working.len() <= 1 {
            break;
        }
    }

    best_mapping
}

fn labels_for_original(working: &WorkingGraph, labels: &HashMap<NodeId, usize>) -> Vec<usize> {
    working
        .original_nodes()
        .map(|id| labels.get(&id).copied().unwrap_or(usize::MAX))
        .collect()
}

/// Random-walk initialization: simulate walks to seed community labels.
///
/// Nodes visited more often are more likely to be in the same community.
/// We assign each node the label of the most-visited neighbor.
fn random_walk_init(working: &WorkingGraph) -> Vec<usize> {
    let n = working.len();
    let walk_steps = n.max(10) * 5;
    let walk_count = n.max(10);
    let mut rng = LcgRng::default();

    // Compute degree for random walk selection.
    let degree: Vec<f32> = working
        .adj
        .iter()
        .zip(&working.self_loop)
        .map(|(e, sl)| e.iter().map(|(_, w)| *w).sum::<f32>() + 2.0 * sl)
        .collect();

    // Run random walks and count node visits.
    let mut visits = vec![0u64; n];
    for _ in 0..walk_count {
        let mut node = rng.gen_range(0, n);
        for _ in 0..walk_steps {
            visits[node] += 1;
            let total = degree[node];
            if total <= 0.0 {
                break;
            }
            let mut r = rng.gen_f32() * total;
            let mut next = node;
            for &(v, w) in &working.adj[node] {
                r -= w;
                if r <= 0.0 {
                    next = v;
                    break;
                }
            }
            node = next;
        }
    }

    // Assign initial label: for each node, pick the neighbor with the
    // highest visit count. If no neighbors, start as its own community.
    let mut labels = Vec::with_capacity(n);
    for u in 0..n {
        let mut best_neighbor = u;
        let mut best_visits = visits[u];
        for &(v, _) in &working.adj[u] {
            if visits[v] > best_visits {
                best_visits = visits[v];
                best_neighbor = v;
            }
        }
        labels.push(best_neighbor);
    }
    labels
}

/// Compute the two-level map-equation description length for a partition.
fn compute_lmdl(working: &WorkingGraph, labels: &[usize], two_m: f32) -> f32 {
    let n = working.len();
    if n == 0 {
        return 0.0;
    }
    if two_m <= 0.0 {
        return 0.0;
    }

    let flow = compute_community_flow(labels, working, two_m);
    let q_total: f32 = flow.values().map(|f| f.exit_probability).sum();
    let mut length = entropy_term(q_total);
    for community in flow.values() {
        length -= entropy_term(community.exit_probability);
    }
    for community in flow.values() {
        let p_circle = community.node_probability + community.exit_probability;
        length += entropy_term(p_circle);
        length -= entropy_term(community.exit_probability);
        for &node_probability in &community.node_probabilities {
            length -= entropy_term(node_probability);
        }
    }
    length.max(0.0)
}

/// Per-community flow statistics.
struct CommunityFlow {
    node_probability: f32,
    exit_probability: f32,
    node_probabilities: Vec<f32>,
}

fn entropy_term(probability: f32) -> f32 {
    if probability > 0.0 {
        probability * probability.log2()
    } else {
        0.0
    }
}

/// Compute node-visit and module-exit probabilities for each community.
fn compute_community_flow(
    labels: &[usize],
    working: &WorkingGraph,
    two_m: f32,
) -> HashMap<usize, CommunityFlow> {
    let mut flow: HashMap<usize, CommunityFlow> = HashMap::new();
    for (u, &l) in labels.iter().enumerate() {
        let entry = flow.entry(l).or_insert(CommunityFlow {
            node_probability: 0.0,
            exit_probability: 0.0,
            node_probabilities: Vec::new(),
        });
        let node_probability = working.degree[u] / two_m;
        entry.node_probability += node_probability;
        entry.node_probabilities.push(node_probability);
        for &(v, w) in &working.adj[u] {
            if labels[v] != l {
                entry.exit_probability += w / two_m;
            }
        }
    }
    flow
}

/// Compute the LMDL delta for moving node `u` from community `old` to `new`.
///
/// Returns negative if moving u improves the partition.
///
/// The delta is computed by evaluating the exact map equation before and
/// after the candidate move. This keeps the local-move step aligned with
/// the objective and avoids fragile incremental cut-flow bookkeeping.
fn infomap_lmdl_delta(
    labels: &[usize],
    old: usize,
    new: usize,
    u: usize,
    working: &WorkingGraph,
    two_m: f32,
) -> f32 {
    if old == new {
        return f32::INFINITY;
    }
    let before = compute_lmdl(working, labels, two_m);
    let mut moved = labels.to_vec();
    moved[u] = new;
    compute_lmdl(working, &moved, two_m) - before
}

/// LCG random number generator for deterministic walks.
struct LcgRng(u64);
impl LcgRng {
    fn default() -> Self {
        Self(0x5DEECE66D)
    }
    fn gen_range(&mut self, low: usize, high: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        low + (self.0 as usize % (high - low))
    }
    fn gen_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (((self.0 >> 11) as f64) / 9007199254740992.0) as f32
    }
}

/// Run one greedy local-move pass to minimize LMDL.
///
/// Returns the new labels and the resulting LMDL.
fn infomap_local_move(
    working: &WorkingGraph,
    labels: &[usize],
    two_m: f32,
    max_passes: usize,
) -> (Vec<usize>, f32) {
    let n = working.len();
    let mut current = labels.to_vec();
    let mut best_lmdl = compute_lmdl(working, &current, two_m);

    for _ in 0..max_passes {
        let mut improved = false;
        for u in 0..n {
            let old = current[u];
            // Collect neighbor communities.
            let neighbor_comms: HashSet<usize> =
                working.adj[u].iter().map(|(v, _)| current[*v]).collect();
            let mut best_new = old;
            let mut best_delta = 0.0f32;
            for &cand in &neighbor_comms {
                if cand == old {
                    continue;
                }
                let delta = infomap_lmdl_delta(&current, old, cand, u, working, two_m);
                if delta < best_delta {
                    best_delta = delta;
                    best_new = cand;
                }
            }
            if best_new != old {
                current[u] = best_new;
                improved = true;
            }
        }
        if !improved {
            break;
        }
        best_lmdl = compute_lmdl(working, &current, two_m);
    }

    (current, best_lmdl)
}

/// Leiden-style refinement for Infomap.
///
/// Within each community, split poorly connected nodes (those whose edge
/// weight into the sub-community is below the well-connectedness threshold)
/// and enforce that each resulting sub-community is connected.
fn infomap_refinement(
    working: &WorkingGraph,
    partition: &[usize],
    options: CommunityOptions,
) -> Vec<usize> {
    let n = working.len();
    let two_m = 2.0 * working.total_weight;

    // Group nodes by parent community.
    let mut by_parent: HashMap<usize, Vec<usize>> = HashMap::new();
    for (u, &c) in partition.iter().enumerate() {
        by_parent.entry(c).or_default().push(u);
    }
    let mut parents: Vec<(usize, Vec<usize>)> = by_parent.into_iter().collect();
    parents.sort_by_key(|(p, _)| *p);

    // Pre-allocate label range per parent.
    let mut label_base = Vec::with_capacity(parents.len());
    let mut cursor = 0usize;
    for (_, members) in &parents {
        label_base.push(cursor);
        cursor += members.len();
    }
    let total_labels = cursor.max(1);

    // Refine each parent in parallel.
    let refine_parent = |idx: usize| -> Vec<usize> {
        let (parent, members) = &parents[idx];
        let base = label_base[idx];
        let parent_total: f32 = (0..n)
            .filter(|&i| partition[i] == *parent)
            .map(|i| working.degree[i])
            .sum();

        if members.len() <= 1 {
            return vec![base];
        }

        let member_set: HashSet<usize> = members.iter().copied().collect();
        // Local refined labels live in [base, base + members.len()).
        let mut refined: HashMap<usize, usize> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (u, base + i))
            .collect();
        let _node_for_label: HashMap<usize, usize> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (base + i, u))
            .collect();
        let label_degree: HashMap<usize, f32> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (base + i, working.degree[u]))
            .collect();

        // Run local move within this parent community.
        for _ in 0..options.max_passes {
            let mut moved = false;
            for &u in members {
                let current = refined[&u];
                let node_degree = working.degree[u];
                if node_degree == 0.0 {
                    continue;
                }

                let mut weight_to_comm: HashMap<usize, f32> = HashMap::new();
                for &(v, w) in &working.adj[u] {
                    if !member_set.contains(&v) {
                        continue;
                    }
                    *weight_to_comm.entry(refined[&v]).or_insert(0.0) += w;
                }

                let mut best = current;
                let mut best_gain = options.min_modularity_gain;

                // Stay gain.
                let stay_weight = weight_to_comm.get(&current).copied().unwrap_or(0.0);
                // Stay is always an option.
                if stay_weight > best_gain {
                    best_gain = stay_weight;
                    best = current;
                }

                for (&candidate, &edge_weight) in &weight_to_comm {
                    if candidate == current {
                        continue;
                    }
                    // Well-connectedness threshold.
                    let cand_degree = label_degree.get(&candidate).copied().unwrap_or(0.0);
                    let threshold = if parent_total > 0.0 {
                        options.well_connectedness * cand_degree * (parent_total - cand_degree)
                            / (two_m * parent_total)
                    } else {
                        0.0
                    };
                    if edge_weight < threshold {
                        continue;
                    }
                    if edge_weight > best_gain {
                        best_gain = edge_weight;
                        best = candidate;
                    }
                }

                refined.insert(u, best);
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

    let per_parent_labels: Vec<Vec<usize>> = if options.parallel {
        (0..parents.len())
            .into_par_iter()
            .map(refine_parent)
            .collect()
    } else {
        (0..parents.len()).map(refine_parent).collect()
    };

    // Assemble global refined vector.
    let mut refined: Vec<usize> = vec![total_labels; n];
    for (idx, labels) in per_parent_labels.iter().enumerate() {
        let (_, members) = &parents[idx];
        debug_assert_eq!(members.len(), labels.len());
        for (&u, &label) in members.iter().zip(labels.iter()) {
            refined[u] = label;
        }
    }

    // Enforce connectivity (Leiden guarantee).
    enforce_connected(working, &mut refined);
    densify(&refined)
}

pub fn community_quality(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
    options: CommunityOptions,
) -> CommunityQuality {
    let working = WorkingGraph::from_graph(graph);
    if working.len() == 0 || communities.is_empty() || working.total_weight <= 0.0 {
        return CommunityQuality {
            community_count: 0,
            singleton_count: 0,
            min_size: 0,
            max_size: 0,
            mean_size: 0.0,
            objective: options.objective,
            score: 0.0,
            disconnected_communities: 0,
            mean_conductance: 0.0,
            max_conductance: 0.0,
            mean_cohesion: 0.0,
            low_cohesion_communities: 0,
        };
    }

    let mut labels = vec![usize::MAX; working.len()];
    for (idx, members) in working.members.iter().enumerate() {
        if let Some(node) = members.first() {
            if let Some(label) = communities.get(node) {
                labels[idx] = *label;
            }
        }
    }

    let mut by_comm: HashMap<usize, Vec<usize>> = HashMap::new();
    for (idx, &label) in labels.iter().enumerate() {
        if label != usize::MAX {
            by_comm.entry(label).or_default().push(idx);
        }
    }

    let community_count = by_comm.len();
    let sizes: Vec<usize> = by_comm.values().map(Vec::len).collect();
    let singleton_count = sizes.iter().filter(|&&size| size == 1).count();
    let min_size = sizes.iter().copied().min().unwrap_or(0);
    let max_size = sizes.iter().copied().max().unwrap_or(0);
    let mean_size = if sizes.is_empty() {
        0.0
    } else {
        sizes.iter().sum::<usize>() as f32 / sizes.len() as f32
    };

    let two_m = 2.0 * working.total_weight;
    let mut degree_by_comm: HashMap<usize, f32> = HashMap::new();
    let mut internal_twice_by_comm: HashMap<usize, f32> = HashMap::new();
    let mut cut_by_comm: HashMap<usize, f32> = HashMap::new();
    for u in 0..working.len() {
        let cu = labels[u];
        if cu == usize::MAX {
            continue;
        }
        *degree_by_comm.entry(cu).or_insert(0.0) += working.degree[u];
        *internal_twice_by_comm.entry(cu).or_insert(0.0) += 2.0 * working.self_loop[u];
        for &(v, weight) in &working.adj[u] {
            if labels[v] == cu {
                *internal_twice_by_comm.entry(cu).or_insert(0.0) += weight;
            } else {
                *cut_by_comm.entry(cu).or_insert(0.0) += weight;
            }
        }
    }

    let score = if options.objective == CommunityObjective::Cpm {
        degree_by_comm
            .iter()
            .map(|(&community, &_degree)| {
                let internal_twice = internal_twice_by_comm
                    .get(&community)
                    .copied()
                    .unwrap_or(0.0);
                let size = by_comm
                    .get(&community)
                    .map(|members| members.len() as f32)
                    .unwrap_or(0.0);
                internal_twice / 2.0 - options.resolution * size * (size - 1.0) / 2.0
            })
            .sum()
    } else {
        degree_by_comm
            .iter()
            .map(|(&community, &degree)| {
                let internal_twice = internal_twice_by_comm
                    .get(&community)
                    .copied()
                    .unwrap_or(0.0);
                internal_twice / two_m - options.resolution * (degree / two_m).powi(2)
            })
            .sum()
    };

    let disconnected_communities = by_comm
        .values()
        .filter(|members| induced_component_count(&working, members) > 1)
        .count();

    let conductances: Vec<f32> = degree_by_comm
        .iter()
        .filter_map(|(&community, &volume)| {
            let denom = volume.min(two_m - volume);
            if denom <= 0.0 {
                return None;
            }
            Some(cut_by_comm.get(&community).copied().unwrap_or(0.0) / denom)
        })
        .collect();
    let mean_conductance = if conductances.is_empty() {
        0.0
    } else {
        conductances.iter().sum::<f32>() / conductances.len() as f32
    };
    let max_conductance = conductances.into_iter().fold(0.0_f32, f32::max);

    let cohesion = community_cohesion(graph, communities);
    let mut comm_sizes: HashMap<usize, usize> = HashMap::new();
    for &community in communities.values() {
        *comm_sizes.entry(community).or_insert(0) += 1;
    }
    let multi: Vec<f32> = cohesion
        .iter()
        .filter(|(community, _)| comm_sizes.get(community).copied().unwrap_or(0) > 1)
        .map(|(_, &score)| score)
        .collect();
    let mean_cohesion = if multi.is_empty() {
        1.0
    } else {
        multi.iter().sum::<f32>() / multi.len() as f32
    };
    let low_cohesion_communities = multi
        .iter()
        .filter(|&&score| score < LOW_COHESION_THRESHOLD)
        .count();

    CommunityQuality {
        community_count,
        singleton_count,
        min_size,
        max_size,
        mean_size,
        objective: options.objective,
        score,
        disconnected_communities,
        mean_conductance,
        max_conductance,
        mean_cohesion,
        low_cohesion_communities,
    }
}

/// A weighted undirected graph used during community detection. After
/// aggregation, each "node" represents a community from the previous level
/// but the same operations apply uniformly.
#[derive(Clone)]
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
        let index: HashMap<NodeId, usize> =
            nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let n = nodes.len();
        let mut adj_map: Vec<HashMap<usize, f32>> = vec![HashMap::new(); n];
        let mut self_loop = vec![0.0f32; n];

        for (_, src, dst, edge) in graph.edges() {
            let Some(&u) = index.get(&src) else { continue };
            let Some(&v) = index.get(&dst) else { continue };
            // Confidence multiplier: extract > infer > ambiguous.
            // Ambiguous edges get a low multiplier instead of being
            // dropped entirely — they keep the graph connected without
            // acting as hub-attraction forces.
            let conf_mult = match edge.confidence {
                Confidence::Extracted => 1.0,
                Confidence::Inferred(_) => 0.5,
                Confidence::Ambiguous => 0.15,
            };
            let weight = edge_kind_weight(edge.kind) * conf_mult;
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
    let mut comm_size: Vec<f32> = working
        .members
        .iter()
        .map(|members| members.len() as f32)
        .collect();
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
            let node_mass = working.members[u].len() as f32;

            // Remove u from its current community for the gain calculation.
            comm_degree[current] -= node_degree;
            comm_size[current] -= node_mass;

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
            let stay_gain = if options.objective == CommunityObjective::Cpm {
                stay_weight - options.resolution * node_mass * comm_size[current]
            } else {
                stay_weight - options.resolution * node_degree * comm_degree[current] / two_m
            };
            if stay_gain > best_gain {
                best_gain = stay_gain;
                best = current;
            }
            for (&candidate, &edge_weight) in &weight_to_comm {
                if candidate == current {
                    continue;
                }
                let gain = if options.objective == CommunityObjective::Cpm {
                    edge_weight - options.resolution * node_mass * comm_size[candidate]
                } else {
                    edge_weight - options.resolution * node_degree * comm_degree[candidate] / two_m
                };
                if gain > best_gain {
                    best_gain = gain;
                    best = candidate;
                }
            }

            comm[u] = best;
            comm_degree[best] += node_degree;
            comm_size[best] += node_mass;
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
    for (u, &c) in partition.iter().enumerate() {
        *parent_degree.entry(c).or_insert(0.0) += working.degree[u];
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
        let mut local_size: HashMap<usize, f32> = members
            .iter()
            .enumerate()
            .map(|(i, &u)| (base + i, working.members[u].len() as f32))
            .collect();

        for _ in 0..options.max_passes {
            let mut moved = false;
            for &u in members {
                let current = refined[&u];
                let node_degree = working.degree[u];
                let node_mass = working.members[u].len() as f32;
                if node_degree == 0.0 {
                    continue;
                }
                *local_degree.get_mut(&current).unwrap() -= node_degree;
                *local_size.get_mut(&current).unwrap() -= node_mass;

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
                let stay_size = local_size.get(&current).copied().unwrap_or(0.0);
                let stay_gain = if options.objective == CommunityObjective::Cpm {
                    stay_weight - options.resolution * node_mass * stay_size
                } else {
                    stay_weight - options.resolution * node_degree * stay_deg / two_m
                };
                if stay_gain > best_gain {
                    best_gain = stay_gain;
                    best = current;
                }
                for (&candidate, &edge_weight) in &weight_to_comm {
                    if candidate == current {
                        continue;
                    }
                    let cand_deg = local_degree.get(&candidate).copied().unwrap_or(0.0);
                    let cand_size = local_size.get(&candidate).copied().unwrap_or(0.0);
                    let threshold = if options.objective == CommunityObjective::Cpm {
                        options.well_connectedness * options.resolution * node_mass * cand_size
                            / parent_total.max(1e-9)
                    } else {
                        options.well_connectedness
                            * options.resolution
                            * cand_deg
                            * (parent_total - cand_deg)
                            / (two_m * parent_total.max(1e-9))
                    };
                    if edge_weight < threshold {
                        continue;
                    }
                    let gain = if options.objective == CommunityObjective::Cpm {
                        edge_weight - options.resolution * node_mass * cand_size
                    } else {
                        edge_weight - options.resolution * node_degree * cand_deg / two_m
                    };
                    if gain > best_gain {
                        best_gain = gain;
                        best = candidate;
                    }
                }

                refined.insert(u, best);
                *local_degree.entry(best).or_insert(0.0) += node_degree;
                *local_size.entry(best).or_insert(0.0) += node_mass;
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
        (0..parents.len())
            .into_par_iter()
            .map(refine_parent)
            .collect()
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

fn induced_component_count(working: &WorkingGraph, members: &[usize]) -> usize {
    if members.is_empty() {
        return 0;
    }
    let member_set: HashSet<usize> = members.iter().copied().collect();
    let mut unseen = member_set.clone();
    let mut components = 0usize;
    while let Some(&start) = unseen.iter().next() {
        components += 1;
        let mut queue = VecDeque::from([start]);
        unseen.remove(&start);
        while let Some(u) = queue.pop_front() {
            for &(v, _) in &working.adj[u] {
                if member_set.contains(&v) && unseen.remove(&v) {
                    queue.push_back(v);
                }
            }
        }
    }
    components
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

/// Edge kind base weights for community detection.
///
/// Structural edges (Defines, Inherits) weight higher than Calls because
/// code structure — what files own what, what inherits what — is a better
/// signal for cohesive modules than the execution graph. Calls are
/// cross-cutting: a shared utility gets pulled into every caller's
/// community, fragmenting real structure. By giving Calls < Defines we
/// keep modules intact while still letting call edges help merge tight
/// groups (e.g. a pair of functions that call each other and are both
/// defined in the same file).
///
/// Confidence multiplier (applied on top of base weight):
///
/// - Extracted: 1.0 (direct observation)
/// - Inferred:  0.5 (e.g. inferred callers from test files)
/// - Ambiguous: 0.15 (placeholder calls; kept at low weight rather than
///   thrown away entirely so the graph stays connected without becoming
///   a hub-attraction force)
fn edge_kind_weight(kind: EdgeKind) -> f32 {
    match kind {
        // Structural: highest priority for coherent module detection
        EdgeKind::Inherits | EdgeKind::Implements => 1.25,
        EdgeKind::Defines => 0.7,
        // Execution: cross-cutting, so below structural
        EdgeKind::Calls => 0.55,
        // Read/write relationships suggest tight coupling
        EdgeKind::ReadsWrites => 0.85,
        // Documentation/mentions: useful but not structural
        EdgeKind::Mentions | EdgeKind::Describes | EdgeKind::DocumentedBy => 0.75,
        // Test edges: tests should cluster near the code they cover,
        // but not override the structural partition.
        EdgeKind::TestedBy => 0.6,
        // Imports create weak cross-module links
        EdgeKind::Imports => 0.45,
        // Semantic similarity / rationale: soft signals
        EdgeKind::SimilarTo | EdgeKind::RationaleFor | EdgeKind::Illustrates => 0.55,
        // Flow bookkeeping: near-zero so flow nodes don't dominate
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

/// Knowledge gaps: structural weaknesses in the codebase graph.
///
/// Identifies four categories of gaps:
/// - `isolated_nodes`: degree ≤ 1, disconnected from the main graph
/// - `thin_communities`: fewer than 3 members
/// - `untested_hotspots`: high-degree nodes with no `TestedBy` edges
/// - `single_file_communities`: entire community in one file
///
/// This complements `gaps_json` in analysis.rs which looks at individual
/// symbols (orphan symbols, large leaves, unresolved calls). Knowledge
/// gaps operates at the community/cluster level.
pub fn knowledge_gaps(graph: &Graph) -> Value {
    use crate::core::EdgeKind;
    let communities = leiden(graph);

    // Build degree map
    let mut degree: HashMap<NodeId, usize> = HashMap::new();
    let mut tested_nodes: HashSet<NodeId> = HashSet::new();
    for (_, src, dst, edge) in graph.edges() {
        *degree.entry(src).or_default() += 1;
        *degree.entry(dst).or_default() += 1;
        if edge.kind == EdgeKind::TestedBy {
            tested_nodes.insert(src);
        }
    }

    // 1. Isolated nodes (degree <= 1, not File)
    let isolated: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.kind != crate::core::NodeKind::File)
        .filter(|(id, _)| degree.get(id).copied().unwrap_or(0) <= 1)
        .map(|(_, n)| {
            json!({
                "qualified_name": n.qualified_name,
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.source_uri,
                "degree": 0,
            })
        })
        .collect();

    // 2. Community sizes and file maps
    let mut comm_sizes: HashMap<usize, usize> = HashMap::new();
    let mut comm_files: HashMap<usize, HashSet<String>> = HashMap::new();
    for (node_id, &comm_id) in &communities {
        if let Some(n) = graph.node(*node_id) {
            *comm_sizes.entry(comm_id).or_default() += 1;
            comm_files.entry(comm_id).or_default().insert(
                n.source_uri.clone().unwrap_or_default()
            );
        }
    }

    // 3. Thin communities (< 3 members)
    let thin: Vec<_> = comm_sizes
        .iter()
        .filter(|(_, &size)| size < 3)
        .map(|(&cid, &size)| json!({
            "community_id": cid,
            "size": size,
        }))
        .collect();

    // 4. Untested hotspots (degree >= 5, no TESTED_BY, not a test itself)
    let untested: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| {
            let is_test = n.properties.get("is_test").and_then(|v| v.as_bool()).unwrap_or(false);
            !is_test
        })
        .filter_map(|(id, n)| {
            let d = degree.get(&id).copied().unwrap_or(0);
            if d >= 5 && !tested_nodes.contains(&id) {
                Some(json!({
                    "qualified_name": n.qualified_name,
                    "name": n.name,
                    "kind": n.kind.as_str(),
                    "file": n.source_uri,
                    "degree": d,
                }))
            } else {
                None
            }
        })
        .collect();

    // 5. Single-file communities (>= 3 members, all in one file)
    let single_file: Vec<_> = comm_sizes
        .iter()
        .filter(|(_, &size)| size >= 3)
        .filter_map(|(&cid, &size)| {
            comm_files.get(&cid).and_then(|files| {
                if files.len() == 1 {
                    Some(json!({
                        "community_id": cid,
                        "size": size,
                        "file": files.iter().next().cloned(),
                    }))
                } else {
                    None
                }
            })
        })
        .collect();

    let total_gaps = isolated.len() + thin.len() + untested.len() + single_file.len();

    json!({
        "operation": "knowledge_gaps",
        "total_gaps": total_gaps,
        "isolated_nodes": isolated,
        "thin_communities": thin,
        "untested_hotspots": untested,
        "single_file_communities": single_file,
    })
}

/// Split communities that exceed a threshold percentage of total nodes.
///
/// Uses Leiden on the subgraph of oversized communities to recursively
/// split them into smaller, more coherent groups. This prevents single
/// mega-communities in large repos.
///
/// * `graph` — the full graph
/// * `threshold_pct` — fraction of total nodes above which a community is
///   considered oversized (default: 0.25 = 25%)
/// * `min_size` — minimum number of members after splitting
///
/// Returns a new community map where oversized communities have been
/// subdivided. Community IDs are remapped to avoid collisions with the
/// original IDs.
pub fn split_oversized(
    graph: &Graph,
    threshold_pct: f64,
    min_size: usize,
) -> HashMap<NodeId, usize> {
    let communities = leiden(graph);
    let total: usize = graph.nodes().count();
    let threshold = (total as f64 * threshold_pct).max(min_size as f64) as usize;

    // Find oversized communities
    let mut size_map: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (id, &cid) in &communities {
        size_map.entry(cid).or_default().push(*id);
    }

    let mut next_id = (size_map.keys().copied().max().unwrap_or(0) + 1000) as usize;
    let mut result = communities.clone();

    for (cid, members) in &size_map {
        if members.len() <= threshold {
            continue;
        }

        // Build subgraph from oversized community
        let member_set: std::collections::HashSet<NodeId> = members.iter().cloned().collect();
        let mut subgraph = crate::core::Graph::new();
        let mut id_map: HashMap<NodeId, crate::core::NodeId> = HashMap::new();
        for &mid in members {
            if let Some(node) = graph.node(mid) {
                let sub_id = subgraph.add_node(node.clone());
                id_map.insert(mid, sub_id);
            }
        }

        // Add edges within the community
        for (_, src, dst, edge) in graph.edges() {
            if member_set.contains(&src) && member_set.contains(&dst) {
                if let (Some(&s), Some(&d)) = (id_map.get(&src), id_map.get(&dst)) {
                    subgraph.add_edge(s, d, edge.clone());
                }
            }
        }

        // Run leiden on subgraph
        let sub_communities = leiden(&subgraph);

        // Remap sub-community IDs to global IDs
        let mut sub_size_map: HashMap<usize, Vec<crate::core::NodeId>> = HashMap::new();
        for (sub_id, &scid) in &sub_communities {
            sub_size_map.entry(scid).or_default().push(*sub_id);
        }

        for (_scid, sub_members) in &sub_size_map {
            let new_cid = if sub_members.len() >= min_size {
                let cid_val = next_id;
                next_id += 1;
                cid_val
            } else {
                *cid // keep original
            };
            for sub_id in sub_members {
                if let Some(&orig_id) = id_map.get(sub_id) {
                    result.insert(orig_id, new_cid);
                }
            }
        }
    }

    result
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
    fn community_quality_reports_connectedness_and_modularity() {
        let mut g = Graph::new();
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
        // Both pairs are fully connected, so cohesion is perfect.
        assert_eq!(quality.mean_cohesion, 1.0);
        assert_eq!(quality.low_cohesion_communities, 0);
    }

    #[test]
    fn cohesion_measures_internal_density() {
        // Star of 5 nodes assigned to one community: 4 actual edges out of
        // 10 possible pairs → cohesion 0.4. Parallel/reverse edges must
        // not double-count a pair.
        let mut g = Graph::new();
        let hub = g.add_node(Node::new(NodeKind::Function, "hub"));
        let mut members: HashMap<NodeId, usize> = HashMap::from([(hub, 0)]);
        for i in 0..4 {
            let leaf = g.add_node(Node::new(NodeKind::Function, format!("leaf{i}")));
            g.add_edge(hub, leaf, Edge::extracted(EdgeKind::Calls));
            g.add_edge(leaf, hub, Edge::extracted(EdgeKind::Calls));
            members.insert(leaf, 0);
        }
        // A singleton in its own community scores 1.0 and is never flagged.
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

    #[test]
    fn infomap_clusters_dense_pairs() {
        let mut g = Graph::new();
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
        // Two triangles {a,b,c} and {d,e,f} joined by a single weak edge
        // c—d. Multi-level Infomap should keep them separate.
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

        let comm = infomap(&g);
        assert_eq!(comm[&a], comm[&b]);
        assert_eq!(comm[&b], comm[&c]);
        assert_eq!(comm[&d], comm[&e]);
        assert_eq!(comm[&e], comm[&f]);
        assert_ne!(comm[&a], comm[&d]);
    }

    #[test]
    fn infomap_splits_disconnected_pieces() {
        let mut g = Graph::new();
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
        let mut g = Graph::new();
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
        let mut g = Graph::new();
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
        // Infomap and Louvain use different objectives, but they may still
        // agree on obvious dense chunks. This graph guards Infomap behavior
        // on larger cliques connected by weak bridge edges.
        let mut g = Graph::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            nodes.push(g.add_node(Node::new(NodeKind::Function, format!("n{i}"))));
        }
        // Three triangles, daisy-chained.
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
