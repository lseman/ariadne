mod analysis;
mod architecture;
mod context;
mod flows;
mod hints;
mod impact;
mod paths;
mod reports;
mod reviews;
mod search;
mod temporal;

use anyhow::{bail, Result};
use ariadne_graph::Graph;
use serde_json::{json, Value, Map};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLockWriteGuard;

pub use analysis::{
    articulation_json, bridge_nodes_json, core_json, cycles_json, diagnostics_json,
    gaps_json, large_functions_json, surprises_json,
};
pub use architecture::architecture_overview_json;
pub use context::minimal_context_json;
pub use flows::{
    handle_affected_flows, handle_blast_radius, handle_flows, handle_test_coverage,
};
pub use impact::{handle_god_nodes, handle_impact};
pub use paths::handle_paths;
pub use reports::generate_report_markdown;
pub use reviews::{
    counterfactual_json, motifs_json, review_context_json, suggested_questions_json,
    traverse_json,
};
pub use search::handle_search;
pub use temporal::{detect_changes_json, graph_diff_json};
pub use hints::SessionState;

pub type ResponseSession = std::sync::RwLock<hints::SessionState>;

/// One-operation JSON interface for agents and MCP wrappers.
pub fn tool_response(db: &Path, operation: &str, params: &Value) -> Result<Value> {
    let session = Session();
    let mut guard = session.write().unwrap();
    let response = _tool_response(db, operation, params, &mut guard)?;
    Ok(response)
}

/// Internal: build response with session guard held.
fn _tool_response(
    db: &Path,
    operation: &str,
    params: &Value,
    session: &mut RwLockWriteGuard<hints::SessionState>,
) -> Result<Value> {
    let store = ariadne_graph::store::Store::open(db)?;
    let graph = store.load()?;
    let detail = DetailLevel::from_params(params);
    let response = match operation {
        "minimal_context" | "context" => {
            let target = params.get("target").and_then(Value::as_str);
            let mode = params
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("review");
            compact_for_detail(minimal_context_json(&graph, target, mode), detail)
        }
        "status" => {
            let (nodes, edges) = store.stats()?;
            use ariadne_graph::query::call_resolution_stats;
            let calls = call_resolution_stats(&graph);
            json!({
                "operation": operation,
                "nodes": nodes,
                "edges": edges,
                "call_resolution": {
                    "resolved": calls.resolved,
                    "unresolved": calls.unresolved,
                    "rate": calls.rate(),
                },
            })
        }
        "search" => compact_for_detail(handle_search(&graph, params), detail),
        "paths" => compact_for_detail(handle_paths(&graph, params)?, detail),
        "impact" => compact_for_detail(handle_impact(&graph, params)?, detail),
        "detect_changes" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(2) as usize;
            compact_for_detail(detect_changes_json(db, base, max_depth)?, detail)
        }
        "review_context" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_lines_per_file = params
                .get("max_lines_per_file")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1600) as usize;
            compact_for_detail(
                review_context_json(db, base, max_lines_per_file, token_budget)?,
                detail,
            )
        }
        "traverse" => {
            let target = required_str(params, "target")?;
            let direction = params
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("both");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1200) as usize;
            let seed = super::helpers::resolve(&graph, target)?;
            compact_for_detail(
                traverse_json(&graph, seed, direction, max_depth, token_budget),
                detail,
            )
        }
        "large_functions" => {
            let min_lines = params
                .get("min_lines")
                .and_then(Value::as_u64)
                .unwrap_or(80) as u32;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(large_functions_json(&graph, min_lines, limit), detail)
        }
        "bridge_nodes" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(bridge_nodes_json(&graph, limit), detail)
        }
        "cycles" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(cycles_json(&graph, limit), detail)
        }
        "core" | "k_core" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(core_json(&graph, limit), detail)
        }
        "articulation" | "articulation_points" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(articulation_json(&graph, limit), detail)
        }
        "gaps" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(gaps_json(&graph, limit), detail)
        }
        "surprises" | "surprise_scoring" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(surprises_json(&graph, limit), detail)
        }
        "diagnostics" | "health" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(diagnostics_json(db, limit)?, detail)
        }
        "graph_diff" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let head = params.get("head").and_then(Value::as_str).unwrap_or("HEAD");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(graph_diff_json(db, base, head, limit)?, detail)
        }
        "counterfactual" => {
            let target = required_str(params, "target")?;
            let direction = params
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("out");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(5) as usize;
            compact_for_detail(
                counterfactual_json(db, target, direction, max_depth)?,
                detail,
            )
        }
        "motifs" => {
            let built_in = params
                .get("built_in")
                .and_then(Value::as_str)
                .unwrap_or("security_audit");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(motifs_json(db, built_in, limit)?, detail)
        }
        "suggested_questions" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let analysis = detect_changes_json(db, base, 2)?;
            compact_for_detail(suggested_questions_json(&analysis, limit), detail)
        }
        "architecture_overview" | "architecture" => architecture_overview_json(&graph, detail),
        "god_nodes" => compact_for_detail(handle_god_nodes(&graph, params)?, detail),
        "flows" => compact_for_detail(handle_flows(&graph, params), detail),
        "affected_flows" => compact_for_detail(handle_affected_flows(db, params)?, detail),
        "blast_radius" | "impact_radius" => {
            compact_for_detail(handle_blast_radius(db, params)?, detail)
        }
        "test_coverage" => compact_for_detail(handle_test_coverage(db, &graph, params)?, detail),
        "report" => {
            let output = required_str(params, "output")?;
            let top = params.get("top").and_then(Value::as_u64).unwrap_or(25) as usize;
            let markdown = generate_report_markdown(db, top)?;
            std::fs::write(output, markdown)?;
            compact_for_detail(
                json!({ "operation": operation, "output": output, "written": true }),
                detail,
            )
        }
        other => bail!("unknown tool operation {}", other),
    };
    let response = apply_response_guardrails(response, &graph, params, detail);
    // Attach hints (suppress if caller disables them)
    if params.get("no_hints").and_then(Value::as_bool).unwrap_or(false) {
        Ok(response)
    } else {
        let hints = hints::generate_hints(operation, &response, session);
        let mut out = response.as_object().cloned().unwrap_or_default();
        if hints != Value::Null {
            out.insert("_hints".into(), hints);
        }
        Ok(Value::Object(out))
    }
}

/// Detail level for response compactness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLevel {
    Minimal,
    Standard,
    Full,
}

impl DetailLevel {
    pub fn parse(value: &str) -> Self {
        match value {
            "minimal" => Self::Minimal,
            "full" => Self::Full,
            _ => Self::Standard,
        }
    }

    pub fn from_params(params: &Value) -> Self {
        params
            .get("detail_level")
            .and_then(Value::as_str)
            .map(Self::parse)
            .unwrap_or(Self::Standard)
    }

    pub fn limit(self, standard: usize) -> usize {
        match self {
            Self::Minimal => standard.min(5),
            Self::Standard => standard,
            Self::Full => standard.saturating_mul(4),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
}

fn compact_for_detail(mut value: Value, detail: DetailLevel) -> Value {
    if detail == DetailLevel::Minimal {
        if let Some(arr) = value.get_mut("snippets").and_then(Value::as_array_mut) {
            for item in arr {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("snippet");
                }
            }
        }
    }
    value
}

/// Response guardrails for pagination.
#[derive(Debug, Clone, Copy)]
struct ResponseGuardrails {
    offset: usize,
    limit: usize,
    include_graph_summary: bool,
}

impl ResponseGuardrails {
    const HARD_LIMIT: usize = 500;

    fn from_params(params: &Value, detail: DetailLevel) -> Self {
        let default_limit = match detail {
            DetailLevel::Minimal => 10,
            DetailLevel::Standard => 50,
            DetailLevel::Full => 200,
        };
        let requested = params
            .get("response_limit")
            .or_else(|| params.get("page_limit"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(default_limit);
        Self {
            offset: params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
            limit: requested.clamp(1, Self::HARD_LIMIT),
            include_graph_summary: params
                .get("include_graph_summary")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

fn apply_response_guardrails(
    mut value: Value,
    graph: &Graph,
    params: &Value,
    detail: DetailLevel,
) -> Value {
    let guardrails = ResponseGuardrails::from_params(params, detail);
    let mut pagination: Map<String, Value> = Map::new();
    for key in PAGEABLE_RESPONSE_KEYS {
        if let Some(arr) = value.get_mut(key).and_then(Value::as_array_mut) {
            let total = arr.len();
            let start = guardrails.offset.min(total);
            let end = (start + guardrails.limit).min(total);
            let page: Vec<Value> = arr[start..end].to_vec();
            *arr = page;
            pagination.insert(
                key.to_string(),
                json!({
                    "offset": guardrails.offset,
                    "limit": guardrails.limit,
                    "returned": end.saturating_sub(start),
                    "total": total,
                    "has_more": end < total,
                }),
            );
        }
    }

    if let Some(obj) = value.as_object_mut() {
        if guardrails.include_graph_summary && !obj.contains_key("graph_summary") {
            obj.insert("graph_summary".to_string(), graph_summary_json(graph));
        }
        obj.insert(
            "guardrails".to_string(),
            json!({
                "response_limit": guardrails.limit,
                "offset": guardrails.offset,
                "hard_limit": ResponseGuardrails::HARD_LIMIT,
                "pagination": pagination,
            }),
        );
    }
    value
}

const PAGEABLE_RESPONSE_KEYS: &[&str] = &[
    "hits",
    "nodes",
    "paths",
    "impacted",
    "changed_files",
    "changed_ranges",
    "changed_symbols",
    "changed_nodes",
    "snippets",
    "communities",
    "cross_community_coupling",
    "bridge_nodes",
    "cycles",
    "core_nodes",
    "articulation_points",
    "warnings",
    "questions",
];

/// Graph summary for response guardrails.
pub fn graph_summary_json(graph: &Graph) -> Value {
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    for (_, node) in graph.nodes() {
        *kind_counts.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
        if let Some(source) = node.source_uri.as_ref() {
            *source_counts.entry(source.clone()).or_insert(0) += 1;
        }
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut sources: Vec<_> = source_counts.into_iter().collect();
    sources.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "kind_counts": kinds.into_iter().map(|(kind, count)| json!({
            "kind": kind,
            "count": count,
        })).collect::<Vec<_>>(),
        "top_sources": sources.into_iter().take(5).map(|(source, nodes)| json!({
            "source": source,
            "nodes": nodes,
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string param '{}'", key))
}

/// Global response session (singleton).
#[allow(non_snake_case)]
pub fn Session() -> &'static ResponseSession {
    use std::sync::{OnceLock, RwLock};
    static SESSION: OnceLock<ResponseSession> = OnceLock::new();
    SESSION.get_or_init(|| RwLock::new(SessionState::new()))
}
