//! Community quality metrics and cohesion analysis.

use crate::core::{Graph, NodeId};
use std::collections::{HashMap, HashSet};

use super::CommunityOptions;

#[derive(Debug, Clone, PartialEq)]
pub struct CommunityQuality {
    pub community_count: usize,
    pub singleton_count: usize,
    pub min_size: usize,
    pub max_size: usize,
    pub mean_size: f32,
    pub score: f32,
    pub disconnected_communities: usize,
    pub mean_conductance: f32,
    pub max_conductance: f32,
    pub mean_cohesion: f32,
    pub low_cohesion_communities: usize,
}

pub const LOW_COHESION_THRESHOLD: f32 = 0.15;

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

pub fn community_quality(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
    options: CommunityOptions,
) -> CommunityQuality {
    let cohesion = community_cohesion(graph, communities);

    let mut sizes: HashMap<usize, usize> = HashMap::new();
    for &community in communities.values() {
        *sizes.entry(community).or_insert(0) += 1;
    }

    let community_count = sizes.len();
    let sizes_vec: Vec<usize> = sizes.values().copied().collect();
    let min_size = sizes_vec.iter().min().copied().unwrap_or(0);
    let max_size = sizes_vec.iter().max().copied().unwrap_or(0);
    let mean_size = if community_count > 0 {
        sizes_vec.iter().sum::<usize>() as f32 / community_count as f32
    } else {
        0.0
    };
    let singleton_count = sizes_vec.iter().filter(|&&s| s == 1).count();

    let mut score = 0.0f32;
    if community_count > 0 {
        let n = graph.nodes().count() as f32;
        if n > 0.0 {
            let sum_sq: f32 = sizes.values().map(|&s| (s as f32 / n).powi(2)).sum();
            score = (sum_sq * options.resolution).sqrt();
        }
    }

    let mut conductances: Vec<f32> = Vec::new();
    for (&cid, &size) in &sizes {
        if size <= 1 {
            conductances.push(0.0);
            continue;
        }
        let mut internal_edges = 0usize;
        let mut external_edges = 0usize;
        for (_edge_id, src, dst, _edge) in graph.edges() {
            let src_comm = communities.get(&src).copied();
            let dst_comm = communities.get(&dst).copied();
            match (src_comm, dst_comm) {
                (Some(a), Some(b)) if a == b && a == cid => internal_edges += 1,
                (Some(a), Some(b)) if (a == cid) != (b == cid) => external_edges += 1,
                _ => {}
            }
        }
        let total = internal_edges + external_edges;
        conductances.push(if total > 0 {
            external_edges as f32 / total as f32
        } else {
            0.0
        });
    }

    let mean_conductance = if conductances.is_empty() {
        0.0
    } else {
        conductances.iter().sum::<f32>() / conductances.len() as f32
    };
    let max_conductance = conductances.iter().copied().fold(0.0f32, f32::max);

    let low_cohesion: usize = cohesion
        .iter()
        .filter(|(&cid, &c)| {
            sizes.get(&cid).copied().unwrap_or(0) > 1 && c < LOW_COHESION_THRESHOLD
        })
        .count();

    let mean_cohesion = {
        let vals: Vec<f32> = cohesion
            .iter()
            .filter(|(cid, _)| sizes.get(*cid).copied().unwrap_or(0) > 1)
            .map(|(_, c)| *c)
            .collect();
        if vals.is_empty() {
            0.0
        } else {
            vals.iter().sum::<f32>() / vals.len() as f32
        }
    };

    CommunityQuality {
        community_count,
        singleton_count,
        min_size,
        max_size,
        mean_size,
        score,
        disconnected_communities: 0,
        mean_conductance,
        max_conductance,
        mean_cohesion,
        low_cohesion_communities: low_cohesion,
    }
}
