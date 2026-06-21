//! Knowledge gaps — structural weaknesses in the codebase graph.

use crate::core::{EdgeKind, Graph, NodeId};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub fn knowledge_gaps(graph: &Graph) -> Value {
    let communities = super::leiden(graph);

    let mut degree: HashMap<NodeId, usize> = HashMap::new();
    let mut tested_nodes: HashSet<NodeId> = HashSet::new();
    for (_, src, dst, edge) in graph.edges() {
        *degree.entry(src).or_default() += 1;
        *degree.entry(dst).or_default() += 1;
        if edge.kind == EdgeKind::TestedBy {
            tested_nodes.insert(src);
        }
    }

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

    let mut comm_sizes: HashMap<usize, usize> = HashMap::new();
    let mut comm_files: HashMap<usize, HashSet<String>> = HashMap::new();
    for (node_id, &comm_id) in &communities {
        if let Some(n) = graph.node(*node_id) {
            *comm_sizes.entry(comm_id).or_default() += 1;
            comm_files
                .entry(comm_id)
                .or_default()
                .insert(n.source_uri.clone().unwrap_or_default());
        }
    }

    let thin: Vec<_> = comm_sizes
        .iter()
        .filter(|(_, &size)| size < 3)
        .map(|(&cid, &size)| {
            json!({
                "community_id": cid,
                "size": size,
            })
        })
        .collect();

    let untested: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| {
            let is_test = n
                .properties
                .get("is_test")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
