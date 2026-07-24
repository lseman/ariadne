use anyhow::{bail, Result};
use ariadne_graph::query::counterfactual::run_without_edges;
use ariadne_graph::query::motifs::{
    diamond_inheritance_motif, doc_function_triangle, find_motifs, security_audit_motif,
};
use ariadne_graph::store::Store;
use ariadne_graph::{Graph, NodeId};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

use super::super::helpers::{resolve, source_matches};
use super::temporal::{detect_changes_json, nodes_json};

/// Token-budgeted review context for changed and impacted files.
pub fn review_context_json(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<Value> {
    let analysis = detect_changes_json(db, base, 2)?;
    let mut files: Vec<String> = analysis["changed_files"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    for item in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(source) = item["source_uri"].as_str() {
            if !files.iter().any(|f| source_matches(f, source)) {
                files.push(source.to_string());
            }
        }
    }
    let mut used_tokens = 0usize;
    let mut snippets = Vec::new();
    for file in files {
        if used_tokens >= token_budget {
            break;
        }
        let ranges = ranges_for_file_from_analysis(&analysis, &file);
        if let Ok(snippet) = file_snippet_for_ranges(&file, &ranges, max_lines_per_file) {
            let tokens = approx_tokens(&snippet);
            if used_tokens + tokens > token_budget && !snippets.is_empty() {
                continue;
            }
            used_tokens += tokens;
            snippets.push(json!({
                "path": file,
                "tokens": tokens,
                "changed_ranges": ranges,
                "snippet": snippet
            }));
        }
    }
    Ok(json!({
        "operation": "review_context",
        "base": base,
        "token_budget": token_budget,
        "used_tokens": used_tokens,
        "analysis": analysis,
        "snippets": snippets,
    }))
}

/// Traverse graph relationships from a target with a token budget.
pub fn traverse_json(
    graph: &Graph,
    seed: NodeId,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Value {
    let mut queue = std::collections::VecDeque::from([(seed, 0usize)]);
    let mut seen = HashSet::from([seed]);
    let mut nodes = Vec::new();
    let mut used = 0usize;
    while let Some((id, depth)) = queue.pop_front() {
        if used >= token_budget {
            break;
        }
        if let Some(n) = graph.node(id) {
            let item = json!({
                "depth": depth,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
                "in_degree": graph.in_neighbors(id).count(),
                "out_degree": graph.out_neighbors(id).count(),
            });
            used += approx_tokens(&item.to_string());
            nodes.push(item);
        }
        if depth >= max_depth {
            continue;
        }
        let mut neighbors = Vec::new();
        if direction == "out" || direction == "both" {
            neighbors.extend(graph.out_neighbors(id).map(|(n, _)| n));
        }
        if direction == "in" || direction == "both" {
            neighbors.extend(graph.in_neighbors(id).map(|(n, _)| n));
        }
        for next in neighbors {
            if seen.insert(next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    json!({ "operation": "traverse", "direction": direction, "used_tokens": used, "nodes": nodes })
}

/// Drop a symbol's edges, rerun BFS, and report nodes that become
/// unreachable from the rest of the graph.
pub fn counterfactual_json(
    db: &Path,
    symbol: &str,
    direction: &str,
    max_depth: usize,
) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let target = resolve(&graph, symbol)?;

    let drop: Vec<_> = graph
        .edges()
        .filter(|(_, src, dst, _)| match direction {
            "in" => *dst == target,
            "both" => *src == target || *dst == target,
            _ => *src == target,
        })
        .map(|(id, _, _, _)| id)
        .collect();

    let reach = |g: &Graph| -> HashSet<NodeId> {
        let mut seen = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((target, 0usize));
        seen.insert(target);
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let next: Vec<NodeId> = match direction {
                "in" => g.in_neighbors(node).map(|(n, _)| n).collect(),
                "both" => g
                    .out_neighbors(node)
                    .chain(g.in_neighbors(node))
                    .map(|(n, _)| n)
                    .collect(),
                _ => g.out_neighbors(node).map(|(n, _)| n).collect(),
            };
            for n in next {
                if seen.insert(n) {
                    queue.push_back((n, depth + 1));
                }
            }
        }
        seen
    };

    let before = reach(&graph);
    let counterfactual = run_without_edges(&graph, &drop);
    let after = reach(&counterfactual);

    let mut lost: Vec<NodeId> = before.difference(&after).copied().collect();
    lost.sort_by_key(|id| id.0);

    Ok(json!({
        "operation": "counterfactual",
        "target": graph.node(target).map(|n| n.qualified_name.clone()),
        "direction": direction,
        "dropped_edges": drop.len(),
        "reachable_before": before.len(),
        "reachable_after": after.len(),
        "unreachable_count": lost.len(),
        "now_unreachable": nodes_json(&graph, &lost, 50),
    }))
}

/// Match a built-in subgraph motif against the graph.
pub fn motifs_json(db: &Path, built_in: &str, limit: usize) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;

    let motif = match built_in {
        "security_audit" => security_audit_motif(),
        "diamond" => diamond_inheritance_motif(),
        "doc_triangle" => doc_function_triangle(),
        other => bail!(
            "unknown built-in motif {other}; expected security_audit, diamond, or doc_triangle"
        ),
    };

    let matches = find_motifs(&graph, &motif, limit);
    Ok(json!({
        "operation": "motifs",
        "built_in": built_in,
        "match_count": matches.len(),
        "matches": matches,
    }))
}

/// Generate prioritized review questions from graph analysis.
pub fn suggested_questions_json(analysis: &Value, limit: usize) -> Value {
    let mut questions = Vec::new();
    for file in analysis["changed_files"].as_array().unwrap_or(&Vec::new()) {
        if let Some(file) = file.as_str() {
            questions.push(format!(
                "What behavior changed in {} and is it covered by tests?",
                file
            ));
        }
    }
    for hit in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(name) = hit["qualified_name"].as_str() {
            questions.push(format!(
                "Does the change alter assumptions relied on by {}?",
                name
            ));
        }
    }
    questions
        .push("Are any unresolved calls or large functions involved in this change?".to_string());
    questions.truncate(limit);
    json!({ "operation": "suggested_questions", "questions": questions })
}

/// File snippet utilities.
fn file_snippet(path: &str, max_lines: usize) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .take(max_lines)
        .enumerate()
        .map(|(i, line)| format!("{:>4}: {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn file_snippet_for_ranges(path: &str, ranges: &[(u32, u32)], max_lines: usize) -> Result<String> {
    if ranges.is_empty() {
        return file_snippet(path, max_lines);
    }

    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(String::new());
    }

    let mut windows = Vec::<(usize, usize)>::new();
    let context = 4usize;
    for (start, end) in ranges {
        let range_start = (*start).max(1) as usize;
        let range_end = (*end).max(*start).max(1) as usize;
        let from = range_start.saturating_sub(context + 1);
        let to = (range_end + context).min(lines.len());
        if from < to {
            windows.push((from, to));
        }
    }
    windows.sort_unstable();

    let mut merged = Vec::<(usize, usize)>::new();
    for (from, to) in windows {
        if let Some((_, last_to)) = merged.last_mut() {
            if from <= *last_to + 1 {
                *last_to = (*last_to).max(to);
                continue;
            }
        }
        merged.push((from, to));
    }

    let mut emitted = 0usize;
    let mut out = Vec::new();
    for (idx, (from, to)) in merged.into_iter().enumerate() {
        if emitted >= max_lines {
            break;
        }
        if idx > 0 {
            out.push("   ...".to_string());
        }
        for (local_idx, line) in lines.iter().skip(from).take(to - from).enumerate() {
            if emitted >= max_lines {
                break;
            }
            emitted += 1;
            out.push(format!("{:>4}: {}", from + local_idx + 1, line));
        }
    }
    Ok(out.join("\n"))
}

fn ranges_for_file_from_analysis(analysis: &Value, file: &str) -> Vec<(u32, u32)> {
    analysis["changed_ranges"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|entry| {
            entry["path"]
                .as_str()
                .map(|path| source_matches(path, file))
                .unwrap_or(false)
        })
        .flat_map(|entry| entry["hunks"].as_array().into_iter().flatten())
        .filter_map(|hunk| {
            let start = hunk["new_start"].as_u64()? as u32;
            let end = hunk["new_end"].as_u64()? as u32;
            Some((start.max(1), end.max(start).max(1)))
        })
        .collect()
}

fn approx_tokens(s: &str) -> usize {
    (s.len() / 4).max(1)
}
