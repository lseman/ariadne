use anyhow::Result;
use ariadne_graph::query::{
    analyze_impact, is_rank_noise, pagerank, personalized_pagerank, ImpactQuery,
};
use ariadne_graph::Graph;
use serde_json::{json, Value};

use super::super::helpers::resolve;
use super::required_str;

pub fn handle_impact(graph: &Graph, params: &Value) -> Result<Value> {
    let target = required_str(params, "target")?;
    let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(4) as usize;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
    let seed = resolve(graph, target)?;
    let hits: Vec<_> = analyze_impact(
        graph,
        ImpactQuery {
            seed,
            max_hops,
            limit,
        },
    )
    .into_iter()
    .filter_map(|hit| {
        graph.node(hit.id).map(|n| {
            json!({
                "id": hit.id.0,
                "score": hit.score,
                "distance": hit.distance,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
                "via": hit.via,
            })
        })
    })
    .collect();
    Ok(json!({ "operation": "impact", "target": target, "hits": hits }))
}

pub fn handle_god_nodes(graph: &Graph, params: &Value) -> Result<Value> {
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let ranks = if let Some(seed) = params.get("seed").and_then(Value::as_str) {
        let seed_id = resolve(graph, seed)?;
        personalized_pagerank(graph, &[(seed_id, 1.0)], 0.85, 50)
    } else {
        pagerank(graph, 0.85, 50)
    };
    let mut sorted: Vec<_> = ranks.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let hits: Vec<_> = sorted
        .into_iter()
        .filter_map(|(id, score)| {
            let n = graph.node(id)?;
            if is_rank_noise(n) {
                return None;
            }
            Some(json!({
                "id": id.0,
                "score": score,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
            }))
        })
        .take(limit)
        .collect();
    Ok(json!({ "operation": "god_nodes", "hits": hits }))
}
