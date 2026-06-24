//! Infomap algorithm — LMDL description-length optimization via random walks.
//!
//! Derived from the original implementation in the monolithic communities.rs.
//! This module preserves the full algorithm: multi-level aggregation, random-walk
//! initialization, LMDL-based local-move, and Leiden-style refinement.

use crate::core::NodeId;
use std::collections::{HashMap, HashSet};

pub fn infomap(graph: &crate::Graph) -> HashMap<NodeId, usize> {
    infomap_with_options(graph, super::CommunityOptions::default())
}

/// Infomap (multi-level, with Leiden-style refinement).
///
/// Runs the standard multi-level Infomap algorithm: each level performs
/// random-walk initialization followed by greedy local-move to minimize
/// the LMDL (Log-Modular Description Length). Between levels, a Leiden-style
/// refinement phase splits poorly connected nodes, and the resulting
/// communities are super::aggregated into super-nodes for the next level.
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
pub fn infomap_with_options(
    graph: &crate::Graph,
    options: super::CommunityOptions,
) -> HashMap<NodeId, usize> {
    let working = super::WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return super::identity_labels(working.original_nodes());
    }
    let final_labels = run_infomap_multilevel(working, options);
    super::relabel(final_labels)
}

/// Drive Infomap level-by-level until no movement.
///
/// Returns a mapping from each *original* `NodeId` to its final community
/// label (in some arbitrary but stable numbering).
fn run_infomap_multilevel(
    mut working: super::WorkingGraph,
    options: super::CommunityOptions,
) -> HashMap<NodeId, usize> {
    let original_working = working.clone();
    let original_two_m = 2.0 * original_working.total_weight;
    if original_two_m <= 0.0 {
        return super::identity_labels(working.original_nodes());
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
            super::densify(&labels)
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

        working = super::aggregate(working, &aggregation_partition);
        if working.len() <= 1 {
            break;
        }
    }

    best_mapping
}

fn labels_for_original(
    working: &super::WorkingGraph,
    labels: &HashMap<NodeId, usize>,
) -> Vec<usize> {
    working
        .original_nodes()
        .map(|id| labels.get(&id).copied().unwrap_or(usize::MAX))
        .collect()
}

/// Random-walk initialization: simulate walks to seed community labels.
///
/// Nodes visited more often are more likely to be in the same community.
/// We assign each node the label of the most-visited neighbor.
fn random_walk_init(working: &super::WorkingGraph) -> Vec<usize> {
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
fn compute_lmdl(working: &super::WorkingGraph, labels: &[usize], two_m: f32) -> f32 {
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
    working: &super::WorkingGraph,
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

/// Precompute per-community flow statistics for incremental LMDL delta.
///
/// Returns (community stats, incoming weights per community per node).
///
/// - `stats[c]` contains node_probability, exit_probability, h_p_sum for community c.
/// - `incoming_to[u][c]` contains sum of w(v,u) for v in community c (raw weights).
///
/// O(E) total.
fn precompute_incremental(
    labels: &[usize],
    working: &super::WorkingGraph,
    two_m: f32,
) -> (Vec<(f32, f32, f32)>, Vec<HashMap<usize, f32>>) {
    let n = working.len();
    let max_label = labels.iter().copied().max().unwrap_or(0);

    // Community stats: (node_probability, exit_probability, h_p_sum)
    let mut stats: Vec<(f32, f32, f32)> = vec![(0.0, 0.0, 0.0); max_label + 1];

    // Node probabilities and h_p sums.
    for (u, &l) in labels.iter().enumerate() {
        let p = working.degree[u] / two_m;
        let entry = &mut stats[l];
        entry.0 += p; // node_probability
        entry.2 += entropy_term(p); // h_p_sum
    }

    // Exit probabilities and incoming weights.
    let mut incoming_to: Vec<HashMap<usize, f32>> = vec![HashMap::new(); n];
    for v in 0..n {
        let lv = labels[v];
        for &(u, w) in &working.adj[v] {
            // Track incoming weight to u from community lv
            *incoming_to[u].entry(lv).or_insert(0.0) += w;
            // If this edge crosses communities, add to exit probability
            if labels[u] != lv {
                let entry = &mut stats[lv];
                entry.1 += w / two_m; // exit_probability
            }
        }
    }

    (stats, incoming_to)
}

/// Compute the LMDL delta for moving node `u` from community `old` to `new`.
///
/// O(degree(u)) — only the two affected communities are re-evaluated.
/// Returns negative if moving u improves the partition.
fn infomap_lmdl_delta(
    labels: &[usize],
    old: usize,
    new: usize,
    u: usize,
    working: &super::WorkingGraph,
    two_m: f32,
    stats: &[(f32, f32, f32)], // (node_prob, exit_prob, h_p_sum)
    incoming_to: &[HashMap<usize, f32>],
) -> f32 {
    if old == new {
        return f32::INFINITY;
    }

    let (p_old, exit_old, _) = stats[old];
    let (p_new, exit_new, _) = stats[new];
    let p_u = working.degree[u] / two_m;

    // Compute outgoing weights from u to old and new communities.
    let mut w_out_old = 0.0f32; // sum of w(u,v)/2m for v in old
    let mut w_out_new = 0.0f32; // sum of w(u,v)/2m for v in new
    let mut w_out_other = 0.0f32; // sum of w(u,v)/2m for v in neither
    for &(v, w) in &working.adj[u] {
        let label_v = labels[v];
        let w_m = w / two_m;
        match (label_v == old, label_v == new) {
            (true, _) => w_out_old += w_m,
            (_, true) => w_out_new += w_m,
            _ => w_out_other += w_m,
        }
    }

    // Incoming weights from old/new communities to u.
    let w_in_old = incoming_to[u].get(&old).copied().unwrap_or(0.0) / two_m;
    let w_in_new = incoming_to[u].get(&new).copied().unwrap_or(0.0) / two_m;

    // Exit probability deltas:
    // old: loses outgoing to non-old, gains incoming from old to u
    // delta_exit_old = w_in_old - (w_out_new + w_out_other)
    let delta_exit_old = w_in_old - w_out_new - w_out_other;

    // new: loses incoming from new to u, gains outgoing to non-new
    // delta_exit_new = (w_out_old + w_out_other) - w_in_new
    let delta_exit_new = w_out_old + w_out_other - w_in_new;

    // q_total = sum of all exit probabilities.
    // delta_q_total = delta_exit_old + delta_exit_new (only old/new change)
    let delta_q_total = delta_exit_old + delta_exit_new;

    // LMDL = H(q_total) - 2*sum_c H(exit_c) + sum_c H(p_c + exit_c) - sum_i H(p_i)
    //
    // h_p terms cancel: -H(p_u) from old + H(p_u) from new = 0.
    // So we only need H(q_total) and the two affected communities.
    //
    // delta = H(q_total+delta) - H(q_total)
    //       -2*[H(exit_old+de_old) - H(exit_old) + H(exit_new+de_new) - H(exit_new)]
    //       +[H(p_old-p_u+exit_old+de_old) - H(p_old+exit_old)
    //        + H(p_new+p_u+exit_new+de_new) - H(p_new+exit_new)]

    let q_total = stats.iter().map(|s| s.1).sum::<f32>();
    let q_total_after = q_total + delta_q_total;

    let q_old_before = p_old + exit_old;
    let q_old_after = (p_old - p_u) + (exit_old + delta_exit_old);

    let q_new_before = p_new + exit_new;
    let q_new_after = (p_new + p_u) + (exit_new + delta_exit_new);

    entropy_term(q_total_after) - entropy_term(q_total)
        - 2.0 * (entropy_term(exit_old + delta_exit_old) - entropy_term(exit_old))
        - 2.0 * (entropy_term(exit_new + delta_exit_new) - entropy_term(exit_new))
        + (entropy_term(q_old_after) - entropy_term(q_old_before))
        + (entropy_term(q_new_after) - entropy_term(q_new_before))
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
    working: &super::WorkingGraph,
    labels: &[usize],
    two_m: f32,
    max_passes: usize,
) -> (Vec<usize>, f32) {
    let n = working.len();
    let mut current = labels.to_vec();
    let mut best_lmdl = compute_lmdl(working, &current, two_m);

    for _ in 0..max_passes {
        let mut improved = false;
        // Precompute community stats and incoming weights once per pass — O(E).
        let (stats, incoming_to) = precompute_incremental(&current, working, two_m);
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
                let delta = infomap_lmdl_delta(
                    &current,
                    old,
                    cand,
                    u,
                    working,
                    two_m,
                    &stats,
                    &incoming_to,
                );
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
    working: &super::WorkingGraph,
    partition: &[usize],
    options: super::CommunityOptions,
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
        (0..parents.len()).into_iter().map(refine_parent).collect()
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
    super::enforce_connected(working, &mut refined);
    super::densify(&refined)
}
