use anyhow::Result;
use ariadne_graph::query::{find_top_paths, PathQuery};
use ariadne_graph::Graph;
use serde_json::{json, Value};

use super::super::helpers::resolve;
use super::required_str;

pub fn handle_paths(graph: &Graph, params: &Value) -> Result<Value> {
    let from = required_str(params, "from")?;
    let to = required_str(params, "to")?;
    let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(5) as usize;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let from_id = resolve(graph, from)?;
    let to_id = resolve(graph, to)?;
    let paths: Vec<_> = find_top_paths(
        graph,
        &PathQuery::between(from_id, to_id, max_hops),
        limit,
    )
    .into_iter()
    .map(|path| {
        let nodes: Vec<_> = path
            .nodes
            .into_iter()
            .filter_map(|id| {
                graph.node(id).map(|n| {
                    json!({
                        "id": id.0,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                    })
                })
            })
            .collect();
        json!({ "cost": path.cost, "nodes": nodes })
    })
    .collect();
    Ok(json!({ "operation": "paths", "paths": paths }))
}
