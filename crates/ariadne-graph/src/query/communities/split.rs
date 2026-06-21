//! Split oversized communities — recursively subdivide large groups.

use crate::core::{Graph, NodeId};
use std::collections::HashMap;

pub fn split_oversized(
    graph: &Graph,
    threshold_pct: f64,
    min_size: usize,
) -> HashMap<NodeId, usize> {
    let communities = super::leiden(graph);
    let total: usize = graph.nodes().count();
    let threshold = (total as f64 * threshold_pct).max(min_size as f64) as usize;

    let mut size_map: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (id, &cid) in &communities {
        size_map.entry(cid).or_default().push(*id);
    }

    let mut next_id = size_map.keys().copied().max().unwrap_or(0) + 1000;
    let mut result = communities.clone();

    for (cid, members) in &size_map {
        if members.len() <= threshold {
            continue;
        }

        let member_set: std::collections::HashSet<NodeId> = members.iter().cloned().collect();
        let mut subgraph = crate::core::Graph::new();
        let mut id_map: HashMap<NodeId, crate::core::NodeId> = HashMap::new();
        for &mid in members {
            if let Some(node) = graph.node(mid) {
                let sub_id = subgraph.add_node(node.clone());
                id_map.insert(mid, sub_id);
            }
        }

        for (_, src, dst, edge) in graph.edges() {
            if member_set.contains(&src) && member_set.contains(&dst) {
                if let (Some(&s), Some(&d)) = (id_map.get(&src), id_map.get(&dst)) {
                    subgraph.add_edge(s, d, edge.clone());
                }
            }
        }

        let sub_communities = super::leiden(&subgraph);

        let mut sub_size_map: HashMap<usize, Vec<crate::core::NodeId>> = HashMap::new();
        for (sub_id, &scid) in &sub_communities {
            sub_size_map.entry(scid).or_default().push(*sub_id);
        }

        for sub_members in sub_size_map.values() {
            let new_cid = if sub_members.len() >= min_size {
                let cid_val = next_id;
                next_id += 1;
                cid_val
            } else {
                *cid
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
