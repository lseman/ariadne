//! Leiden algorithm — Louvain with refinement for guaranteed well-connected communities.

use super::{CommunityOptions, WorkingGraph};
use crate::core::NodeId;
use std::collections::{HashMap, HashSet};

pub fn leiden(graph: &crate::Graph) -> HashMap<NodeId, usize> {
    leiden_with_options(graph, super::CommunityOptions::default())
}

pub fn leiden_with_options(
    graph: &crate::Graph,
    options: super::CommunityOptions,
) -> HashMap<NodeId, usize> {
    let working = WorkingGraph::from_graph(graph);
    if working.total_weight <= 0.0 {
        return super::identity_labels(working.original_nodes());
    }
    let final_labels = run_multilevel_leiden(working, options);
    super::relabel(final_labels)
}

fn run_multilevel_leiden(
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

        let aggregation_partition = refinement_phase(&working, &partition, options);

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

    let mut by_parent: HashMap<usize, Vec<usize>> = HashMap::new();
    for (u, &c) in partition.iter().enumerate() {
        by_parent.entry(c).or_default().push(u);
    }
    let mut parents: Vec<(usize, Vec<usize>)> = by_parent.into_iter().collect();
    parents.sort_by_key(|(p, _)| *p);

    let mut parent_degree: HashMap<usize, f32> = HashMap::new();
    for (u, &c) in partition.iter().enumerate() {
        *parent_degree.entry(c).or_insert(0.0) += working.degree[u];
    }

    let mut label_base = Vec::with_capacity(parents.len());
    let mut cursor = 0usize;
    for (_, members) in &parents {
        label_base.push(cursor);
        cursor += members.len();
    }

    let refine_parent = |idx: usize| -> Vec<usize> {
        let (parent, members) = &parents[idx];
        let base = label_base[idx];
        let parent_total = parent_degree.get(parent).copied().unwrap_or(0.0);

        if members.len() <= 1 {
            return vec![base];
        }
        let member_set: HashSet<usize> = members.iter().copied().collect();
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
                let stay_gain = if options.objective == super::CommunityObjective::Cpm {
                    stay_weight - options.resolution * node_mass * local_size[&current]
                } else {
                    stay_weight - options.resolution * node_degree * local_degree[&current] / two_m
                };
                if stay_gain > best_gain {
                    best_gain = stay_gain;
                    best = current;
                }

                for (&target, &weight) in &weight_to_comm {
                    if target == current {
                        continue;
                    }
                    let gain = if options.objective == super::CommunityObjective::Cpm {
                        weight - options.resolution * node_mass * local_size[&target]
                    } else {
                        weight - options.resolution * node_degree * local_degree[&target] / two_m
                    };

                    let threshold = if options.well_connectedness > 0.0
                        && parent_total > 0.0
                        && local_degree[&target] > 0.0
                    {
                        let w_ratio = weight / local_degree[&target];
                        let wc_threshold = options.well_connectedness
                            * (stay_weight / two_m - node_degree * local_degree[&current] / two_m
                                + node_mass * local_size[&current] / two_m);
                        gain > best_gain && w_ratio >= wc_threshold
                    } else {
                        gain > best_gain
                    };
                    if threshold {
                        best_gain = gain;
                        best = target;
                    }
                }

                if best != current {
                    refined.insert(u, best);
                    moved = true;
                }

                *local_degree.get_mut(&current).unwrap() += node_degree;
                *local_size.get_mut(&current).unwrap() += node_mass;
            }
            if !moved {
                break;
            }
        }
        members.iter().map(|&u| refined[&u]).collect()
    };

    let refined: Vec<Vec<usize>> = (0..parents.len()).map(refine_parent).collect();

    let mut result = vec![0usize; n];
    for (idx, (_, members)) in parents.iter().enumerate() {
        for (i, &u) in members.iter().enumerate() {
            result[u] = refined[idx][i];
        }
    }

    super::enforce_connected(working, &mut result);
    super::densify(&result)
}
