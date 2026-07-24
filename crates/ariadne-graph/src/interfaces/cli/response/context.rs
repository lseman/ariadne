use ariadne_graph::query::ranked_search;
use ariadne_graph::Graph;
use serde_json::{json, Value};

/// Minimal context for a target.
pub fn minimal_context_json(graph: &Graph, target: Option<&str>, mode: &str) -> Value {
    let hits = target
        .map(|q| ranked_search(graph, q, 5))
        .unwrap_or_default();
    let top_symbols: Vec<_> = hits
        .iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "score": hit.score,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    let risk = if hits.first().map(|h| h.score > 120.0).unwrap_or(false) {
        "medium"
    } else {
        "low"
    };
    json!({
        "operation": "minimal_context",
        "mode": mode,
        "target": target,
        "summary": if target.is_some() {
            "Resolved target candidates and prepared next graph operations."
        } else {
            "No target supplied; start with search, detect_changes, or bridge_nodes."
        },
        "risk": risk,
        "top_symbols": top_symbols,
        "suggested_next_tools": ["detect_changes", "impact", "traverse", "review_context"]
    })
}
