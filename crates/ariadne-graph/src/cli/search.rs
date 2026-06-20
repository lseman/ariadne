use ariadne_graph::query::ranked_search;
use ariadne_graph::Graph;
use serde_json::{json, Value};

pub(super) fn handle_search(graph: &Graph, params: &Value) -> Value {
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let hits: Vec<_> = ranked_search(graph, query, limit)
        .into_iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "id": hit.id.0,
                    "score": hit.score,
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                    "signals": hit.signals,
                })
            })
        })
        .collect();
    json!({ "operation": "search", "hits": hits })
}
