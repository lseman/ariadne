//! Louvain algorithm — standard multi-level modularity optimization.

use super::{CommunityOptions, WorkingGraph};
use crate::core::NodeId;
use std::collections::{HashMap, HashSet};

pub fn louvain(graph: &crate::Graph) -> HashMap<NodeId, usize> {
    louvain_with_options(graph, super::CommunityOptions::default())
}

pub fn louvain_with_options(
    graph: &crate::Graph,
    options: super::CommunityOptions,
) -> HashMap<NodeId, usize> {
    let working = WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return super::identity_labels(working.original_nodes());
    }
    let final_labels = run_multilevel_louvain(working, options);
    super::relabel(final_labels)
}

fn run_multilevel_louvain(
    mut working: WorkingGraph,
    options: CommunityOptions,
) -> HashMap<NodeId, usize> {
    let current: HashMap<NodeId, usize> = working
        .original_nodes()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();

    let mut current = current;

    for _ in 0..options.max_levels {
        let partition = local_move(&working, options);
        let distinct: HashSet<usize> = partition.iter().copied().collect();
        let moved = distinct.len() < working.len();

        let aggregation_partition = super::densify(&partition);

        for super_node in current.values_mut() {
            *super_node = aggregation_partition[*super_node];
        }

        if !moved {
            return current;
        }

        working = super::aggregate(working, &aggregation_partition);
        if working.len() <= 1 {
            break;
        }
    }

    current
}

fn local_move(working: &WorkingGraph, options: CommunityOptions) -> Vec<usize> {
    let n = working.len();
    let mut comm: Vec<usize> = (0..n).collect();
    let mut comm_degree: Vec<f32> = working.degree.clone();
    let mut comm_size: Vec<f32> = working.members.iter().map(|m| m.len() as f32).collect();
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

            let mut weight_to_comm: HashMap<usize, f32> = HashMap::new();
            for &(v, w) in &working.adj[u] {
                *weight_to_comm.entry(comm[v]).or_insert(0.0) += w;
            }

            let mut best = current;
            let mut best_gain = options.min_modularity_gain;
            let stay_weight = weight_to_comm.get(&current).copied().unwrap_or(0.0);
            let stay_gain = if options.objective == super::CommunityObjective::Cpm {
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
                let gain = if options.objective == super::CommunityObjective::Cpm {
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
