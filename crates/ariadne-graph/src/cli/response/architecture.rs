use ariadne_graph::query::{bridge_scores, community_cohesion, leiden, LOW_COHESION_THRESHOLD};
use ariadne_graph::{Graph, NodeId};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::analysis::{articulation_json, core_json, cycles_json};
use super::DetailLevel;

/// Architecture overview at community level.
pub fn architecture_overview_json(graph: &Graph, detail: DetailLevel) -> Value {
    let communities = leiden(graph);
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in &communities {
        by_comm.entry(community).or_default().push(node);
    }
    let cohesion = community_cohesion(graph, &communities);

    let summaries = community_summaries_json(graph, &by_comm, &cohesion, detail);
    let coupling_rows = cross_community_coupling_json(graph, &communities, detail);
    let bridge_rows = bridge_rows_json(graph, &communities, detail.limit(10));
    let cycles = cycles_json(graph, detail.limit(8));
    let core = core_json(graph, detail.limit(10));
    let articulations = articulation_json(graph, detail.limit(10));
    let warnings = architecture_warnings_json(&coupling_rows, &by_comm, &cohesion);

    json!({
        "operation": "architecture_overview",
        "detail_level": detail.as_str(),
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "community_count": by_comm.len(),
        "communities": summaries,
        "cross_community_coupling": coupling_rows,
        "bridge_nodes": bridge_rows,
        "cycles": cycles["hits"].clone(),
        "core_nodes": core["hits"].clone(),
        "articulation_points": articulations["hits"].clone(),
        "warnings": warnings,
        "suggested_next_tools": ["bridge_nodes", "cycles", "core", "articulation_points", "traverse", "impact", "gaps"]
    })
}

fn community_summaries_json(
    graph: &Graph,
    by_comm: &HashMap<usize, Vec<NodeId>>,
    cohesion: &HashMap<usize, f32>,
    detail: DetailLevel,
) -> Vec<Value> {
    let mut summaries: Vec<_> = by_comm
        .iter()
        .map(|(community, members)| {
            let mut files: HashMap<String, usize> = HashMap::new();
            let mut kinds: HashMap<String, usize> = HashMap::new();
            for id in members {
                if let Some(node) = graph.node(*id) {
                    if let Some(source) = &node.source_uri {
                        *files.entry(source.clone()).or_insert(0) += 1;
                    }
                    *kinds.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
                }
            }
            let mut top_files: Vec<_> = files.into_iter().collect();
            top_files.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            let mut kind_counts: Vec<_> = kinds.into_iter().collect();
            kind_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            json!({
                "community": community,
                "size": members.len(),
                "cohesion": cohesion.get(community).copied().unwrap_or(0.0),
                "top_files": top_files.into_iter().take(detail.limit(5)).map(|(path, count)| json!({"path": path, "nodes": count})).collect::<Vec<_>>(),
                "kind_counts": kind_counts.into_iter().map(|(kind, count)| json!({"kind": kind, "count": count})).collect::<Vec<_>>(),
            })
        })
        .collect();
    summaries.sort_by_key(|v| std::cmp::Reverse(v["size"].as_u64().unwrap_or_default()));
    summaries.truncate(detail.limit(12));
    summaries
}

fn cross_community_coupling_json(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
    detail: DetailLevel,
) -> Vec<Value> {
    let mut coupling: HashMap<(usize, usize), usize> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        let Some(a) = communities.get(&src).copied() else {
            continue;
        };
        let Some(b) = communities.get(&dst).copied() else {
            continue;
        };
        if a != b {
            let key = if a < b { (a, b) } else { (b, a) };
            *coupling.entry(key).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<_> = coupling
        .into_iter()
        .map(|((a, b), edges)| json!({"from": a, "to": b, "edges": edges}))
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["edges"].as_u64().unwrap_or_default()));
    rows.truncate(detail.limit(10));
    rows
}

fn bridge_rows_json(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
    limit: usize,
) -> Vec<Value> {
    bridge_scores(graph, communities, limit)
        .into_iter()
        .filter_map(|row| {
            graph.node(row.node).map(|n| {
                json!({
                    "id": row.node.0,
                    "score": row.score,
                    "communities_touched": row.communities_touched,
                    "degree": row.degree,
                    "approx_betweenness": row.approx_betweenness,
                    "articulation": row.articulation,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect()
}

fn architecture_warnings_json(
    coupling_rows: &[Value],
    by_comm: &HashMap<usize, Vec<NodeId>>,
    cohesion: &HashMap<usize, f32>,
) -> Vec<Value> {
    let mut warnings: Vec<_> = coupling_rows
        .iter()
        .take(5)
        .filter_map(|row| {
            let edges = row["edges"].as_u64().unwrap_or_default();
            (edges >= 5).then(|| {
                json!({
                    "kind": "cross_community_coupling",
                    "severity": if edges >= 20 { "high" } else { "medium" },
                    "communities": [row["from"].clone(), row["to"].clone()],
                    "edges": edges,
                })
            })
        })
        .collect();
    let mut low_cohesion: Vec<(&usize, usize, f32)> =
        by_comm
            .iter()
            .filter_map(|(community, members)| {
                let score = cohesion.get(community).copied().unwrap_or(1.0);
                (members.len() > 1 && score < LOW_COHESION_THRESHOLD)
                    .then_some((community, members.len(), score))
            })
            .collect();
    low_cohesion.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    for (community, size, score) in low_cohesion.into_iter().take(5) {
        warnings.push(json!({
            "kind": "low_cohesion_community",
            "severity": "medium",
            "community": community,
            "size": size,
            "cohesion": score,
        }));
    }
    warnings
}
