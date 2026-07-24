use anyhow::Result;
use ariadne_graph::extract::flows::all_flows;
use ariadne_graph::{EdgeKind, Graph, NodeKind};
use serde_json::{json, Value};
use std::path::Path;

use super::super::helpers::resolve;
use super::temporal::detect_changes_json;

pub fn handle_flows(graph: &Graph, params: &Value) -> Value {
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
    let ids = all_flows(graph);
    let total = ids.len();
    let hits: Vec<Value> = ids
        .into_iter()
        .take(limit)
        .filter_map(|id| {
            let node = graph.node(id)?;
            Some(json!({
                "id": id.0,
                "qualified_name": node.qualified_name,
                "entry_qualified_name": node.properties.get("entry_qualified_name"),
                "entry_name": node.properties.get("entry_name"),
                "criticality": node.properties.get("criticality"),
                "node_count": node.properties.get("node_count"),
                "depth": node.properties.get("depth"),
                "is_test_flow": node.properties.get("is_test_flow"),
            }))
        })
        .collect();
    json!({
        "operation": "flows",
        "hits": hits,
        "total": total,
        "truncated": total > limit,
    })
}

pub fn handle_affected_flows(db: &Path, params: &Value) -> Result<Value> {
    let base = params
        .get("base")
        .and_then(Value::as_str)
        .unwrap_or("HEAD~1");
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let analysis = detect_changes_json(db, base, 2)?;
    let payload = analysis
        .get("affected_flows")
        .cloned()
        .unwrap_or_else(|| json!({"hits": [], "total": 0, "truncated": false}));
    let truncated_hits: Vec<Value> = payload["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect();
    Ok(json!({
        "operation": "affected_flows",
        "base": base,
        "hits": truncated_hits,
        "total": payload["total"],
    }))
}

pub fn handle_blast_radius(db: &Path, params: &Value) -> Result<Value> {
    let base = params
        .get("base")
        .and_then(Value::as_str)
        .unwrap_or("HEAD~1");
    let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(2) as usize;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
    let analysis = detect_changes_json(db, base, max_depth)?;
    let changed_files: Vec<Value> = analysis["changed_files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect();
    let changed_symbols: Vec<Value> = analysis["changed_symbols"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect();
    let impacted: Vec<Value> = analysis["impacted"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect();
    let test_cov = analysis
        .get("test_coverage")
        .cloned()
        .unwrap_or_else(|| json!({"covered": [], "missing": [], "missing_count": 0}));
    Ok(json!({
        "operation": "blast_radius",
        "base": base,
        "risk": analysis["risk"].clone(),
        "risk_score": analysis["risk_score"],
        "changed_files": changed_files,
        "changed_files_count": changed_files.len(),
        "changed_symbols": changed_symbols,
        "changed_symbols_count": changed_symbols.len(),
        "impacted": impacted,
        "impacted_count": impacted.len(),
        "test_coverage": test_cov,
    }))
}

pub fn handle_test_coverage(db: &Path, graph: &Graph, params: &Value) -> Result<Value> {
    let base = params
        .get("base")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let target = params
        .get("target")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let result = match (base, target) {
        (Some(base), _) => {
            let analysis = detect_changes_json(db, &base, 2)?;
            analysis
                .get("test_coverage")
                .cloned()
                .unwrap_or_else(|| json!({"covered": [], "missing": [], "missing_count": 0}))
        }
        (_, Some(target)) => {
            let seed = resolve(graph, &target)?;
            let mut covered = Vec::new();
            let mut missing = Vec::new();
            if let Some(node) = graph.node(seed) {
                if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                    let tests: Vec<Value> = graph
                        .out_neighbors(seed)
                        .filter(|(_, edge)| edge.kind == EdgeKind::TestedBy)
                        .filter_map(|(test_id, _)| {
                            graph.node(test_id).map(|t| {
                                json!({
                                    "id": test_id.0,
                                    "qualified_name": t.qualified_name,
                                    "source_uri": t.source_uri,
                                })
                            })
                        })
                        .collect();
                    let entry = json!({
                        "id": seed.0,
                        "qualified_name": node.qualified_name,
                        "kind": node.kind,
                        "source_uri": node.source_uri,
                        "tests": tests,
                    });
                    if tests.is_empty() {
                        missing.push(entry);
                    } else {
                        covered.push(entry);
                    }
                }
            }
            json!({
                "target": target,
                "covered": covered,
                "missing": missing,
                "missing_count": missing.len(),
            })
        }
        _ => json!({"covered": [], "missing": [], "missing_count": 0}),
    };
    Ok(json!({ "operation": "test_coverage", "result": result }))
}
