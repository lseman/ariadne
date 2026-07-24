//! Hybrid FTS5/semantic search: fuses SQLite full-text and embedding
//! candidates with in-memory ranked-search signals via reciprocal-rank
//! fusion, then applies kind/identifier/noise boosts.

use crate::core::{Graph, NodeId, NodeKind};
use crate::store::Store;
use std::collections::HashMap;
use std::path::Path;

use super::fuzzy::{compact, normalize_identifier};
use super::vocabulary::SEARCH_STOPWORDS;
use super::{extract_query_identifiers, query_kind_boosts, ranked_search, SearchHit};

const RRF_K: f32 = 60.0;
const SOURCE_SATURATION_DECAY: f32 = 0.72;

/// FTS5-boosted search.
///
/// Runs a SQLite FTS5 query for fast candidate retrieval, then blends the
/// BM25 score with the in-memory signals from [`ranked_search`] (graph
/// topology, fuzzy, kind prior).  Falls back to pure in-memory search if the
/// FTS index is empty or the query produces no hits.
pub fn fts_ranked_search(
    store: &Store,
    graph: &Graph,
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let fts_hits = store.fts_search(query, limit * 3).unwrap_or_default();
    let semantic_hits = store.semantic_search(query, limit * 3).unwrap_or_default();
    let query_identifiers = extract_query_identifiers(query);

    if fts_hits.is_empty() && semantic_hits.is_empty() && query_identifiers.is_empty() {
        return ranked_search(graph, query, limit);
    }

    // Start from in-memory results to get fuzzy/graph topology signals.
    let mem_hits = ranked_search(graph, query, limit * 2);
    let mut merged: HashMap<NodeId, SearchHit> = mem_hits.into_iter().map(|h| (h.id, h)).collect();

    // Fuse FTS5 and semantic candidates with reciprocal-rank boosts. This is
    // less sensitive to incomparable raw score scales than max-normalization.
    for (rank, (qname, _)) in fts_hits.iter().enumerate() {
        if let Some(id) = graph.find_by_qname(qname) {
            let fts_boost = reciprocal_rank_boost(rank, 3600.0);
            match merged.get_mut(&id) {
                Some(hit) => {
                    hit.score += fts_boost;
                    if !hit.signals.contains(&"fts5") {
                        hit.signals.push("fts5");
                    }
                }
                None => {
                    merged.insert(
                        id,
                        SearchHit {
                            id,
                            score: fts_boost,
                            signals: vec!["fts5"],
                        },
                    );
                }
            }
        }
    }

    for (rank, (qname, _)) in semantic_hits.iter().enumerate() {
        if let Some(id) = graph.find_by_qname(qname) {
            let semantic_boost = reciprocal_rank_boost(rank, 2700.0);
            match merged.get_mut(&id) {
                Some(hit) => {
                    hit.score += semantic_boost;
                    if !hit.signals.contains(&"semantic") {
                        hit.signals.push("semantic");
                    }
                }
                None => {
                    merged.insert(
                        id,
                        SearchHit {
                            id,
                            score: semantic_boost,
                            signals: vec!["semantic"],
                        },
                    );
                }
            }
        }
    }

    let kind_boosts = query_kind_boosts(query);
    let dotted_query = query.contains('.');
    let normalized_query = normalize_identifier(query);
    let query_tokens = search_query_tokens(&normalized_query);
    let symbol_query = is_symbol_query(query);
    for (id, hit) in merged.iter_mut() {
        let Some(node) = graph.node(*id) else {
            continue;
        };
        for (kind, multiplier) in &kind_boosts {
            if node.kind == *kind {
                hit.score *= *multiplier;
                if !hit.signals.contains(&"kind_boost") {
                    hit.signals.push("kind_boost");
                }
                break;
            }
        }
        if dotted_query
            && normalize_identifier(&node.qualified_name.replace("::", "."))
                .contains(&normalized_query)
        {
            hit.score *= 1.25;
            if !hit.signals.contains(&"qualified_boost") {
                hit.signals.push("qualified_boost");
            }
        }
        if !query_identifiers.is_empty() {
            let normalized_qname = normalize_identifier(&node.qualified_name.replace("::", " "));
            if query_identifiers
                .iter()
                .any(|identifier| normalized_qname.contains(identifier))
            {
                hit.score *= 1.30;
                if !hit.signals.contains(&"identifier_boost") {
                    hit.signals.push("identifier_boost");
                }
            }
        }
        if symbol_query && is_definition_like_node(node) && symbol_matches_node(query, node) {
            hit.score *= 1.35;
            if !hit.signals.contains(&"definition_boost") {
                hit.signals.push("definition_boost");
            }
        }
        if node.qualified_name.starts_with("call::") {
            hit.score *= 0.45;
            if !hit.signals.contains(&"placeholder_penalty") {
                hit.signals.push("placeholder_penalty");
            }
        }
        apply_noise_penalty(&mut hit.score, &mut hit.signals, node, &normalized_query);
        if !query_tokens.is_empty()
            && source_stem_matches(node.source_uri.as_deref(), &query_tokens)
        {
            hit.score *= 1.12;
            if !hit.signals.contains(&"file_stem_boost") {
                hit.signals.push("file_stem_boost");
            }
        }
    }
    boost_multi_hit_sources(&mut merged, graph);

    let mut hits: Vec<SearchHit> = merged.into_values().collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    apply_source_saturation(&mut hits, graph);
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    hits.truncate(limit);
    hits
}

pub(super) fn apply_source_saturation(hits: &mut [SearchHit], graph: &Graph) {
    let mut seen_by_source: HashMap<String, usize> = HashMap::new();
    for hit in hits {
        let Some(source) = graph.node(hit.id).and_then(|node| node.source_uri.as_ref()) else {
            continue;
        };
        let count = seen_by_source.entry(source.clone()).or_insert(0);
        if *count > 0 {
            hit.score *= SOURCE_SATURATION_DECAY.powi(*count as i32);
            if !hit.signals.contains(&"source_saturation") {
                hit.signals.push("source_saturation");
            }
        }
        *count += 1;
    }
}

fn reciprocal_rank_boost(rank: usize, weight: f32) -> f32 {
    weight / (RRF_K + rank as f32 + 1.0)
}

fn is_definition_like_node(node: &crate::core::Node) -> bool {
    if node.qualified_name.starts_with("call::") {
        return false;
    }
    matches!(
        node.kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Type
            | NodeKind::Trait
            | NodeKind::Impl
            | NodeKind::Module
    )
}

fn is_symbol_query(query: &str) -> bool {
    let q = query.trim();
    !q.is_empty()
        && !q.contains(' ')
        && (q.contains("::")
            || q.contains('.')
            || q.contains('_')
            || q.starts_with('_')
            || q.chars().any(|c| c.is_ascii_uppercase()))
}

fn symbol_matches_node(query: &str, node: &crate::core::Node) -> bool {
    let leaf = query
        .rsplit("::")
        .next()
        .unwrap_or(query)
        .rsplit('.')
        .next()
        .unwrap_or(query);
    let query_norm = normalize_identifier(leaf);
    let name_norm = normalize_identifier(&node.name);
    query_norm == name_norm
        || compact(&query_norm) == compact(&name_norm)
        || normalize_identifier(&node.qualified_name.replace("::", " ")).contains(&query_norm)
}

pub(super) fn apply_noise_penalty(
    score: &mut f32,
    signals: &mut Vec<&'static str>,
    node: &crate::core::Node,
    normalized_query: &str,
) {
    let Some(source) = node.source_uri.as_deref() else {
        return;
    };
    if should_preserve_noise(normalized_query) {
        return;
    }

    let source_lower = source.replace('\\', "/").to_ascii_lowercase();
    let mut multiplier: f32 = 1.0;
    if crate::extract::test_detect::is_test_file_path(Path::new(source)) {
        multiplier = multiplier.min(0.72);
    }
    if source_lower.ends_with(".d.ts") {
        multiplier = multiplier.min(0.70);
    }
    if source_lower.contains("/examples/")
        || source_lower.starts_with("examples/")
        || source_lower.contains("/sample/")
        || source_lower.starts_with("sample/")
    {
        multiplier = multiplier.min(0.82);
    }
    if source_lower.contains("/legacy/")
        || source_lower.starts_with("legacy/")
        || source_lower.contains("/compat/")
        || source_lower.starts_with("compat/")
        || source_lower.contains("/generated/")
        || source_lower.starts_with("generated/")
    {
        multiplier = multiplier.min(0.78);
    }

    if multiplier < 1.0 {
        *score *= multiplier;
        if !signals.contains(&"noise_penalty") {
            signals.push("noise_penalty");
        }
    }
}

fn should_preserve_noise(normalized_query: &str) -> bool {
    normalized_query.split_whitespace().any(|token| {
        matches!(
            token,
            "test"
                | "tests"
                | "testing"
                | "spec"
                | "specs"
                | "example"
                | "examples"
                | "sample"
                | "samples"
                | "legacy"
                | "compat"
                | "generated"
                | "declaration"
                | "types"
        )
    })
}

fn search_query_tokens(normalized_query: &str) -> Vec<String> {
    normalized_query
        .split_whitespace()
        .filter(|token| token.len() >= 3 && !SEARCH_STOPWORDS.contains(token))
        .map(ToOwned::to_owned)
        .collect()
}

fn source_stem_matches(source: Option<&str>, query_tokens: &[String]) -> bool {
    let Some(source) = source else {
        return false;
    };
    let Some(stem) = Path::new(source).file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let stem_norm = normalize_identifier(stem);
    let stem_compact = compact(&stem_norm);
    query_tokens.iter().any(|token| {
        stem_norm.split_whitespace().any(|part| part == token)
            || stem_compact == compact(token)
            || stem_compact.contains(&compact(token))
    })
}

fn boost_multi_hit_sources(hits: &mut HashMap<NodeId, SearchHit>, graph: &Graph) {
    let mut file_sum: HashMap<String, f32> = HashMap::new();
    let mut best: HashMap<String, NodeId> = HashMap::new();

    for (id, hit) in hits.iter() {
        let Some(source) = graph.node(*id).and_then(|node| node.source_uri.as_ref()) else {
            continue;
        };
        *file_sum.entry(source.clone()).or_insert(0.0) += hit.score.max(0.0);
        match best.get(source).copied() {
            Some(current)
                if hits
                    .get(&current)
                    .is_some_and(|best_hit| best_hit.score >= hit.score) => {}
            _ => {
                best.insert(source.clone(), *id);
            }
        }
    }

    let max_sum = file_sum.values().copied().fold(0.0_f32, f32::max);
    if max_sum <= 0.0 {
        return;
    }
    let max_score = hits.values().map(|hit| hit.score).fold(0.0_f32, f32::max);
    let boost_unit = max_score * 0.12;
    for (source, id) in best {
        let count_relevant = hits
            .iter()
            .filter(|(candidate, _)| {
                graph
                    .node(**candidate)
                    .and_then(|node| node.source_uri.as_ref())
                    .map(|candidate_source| candidate_source == &source)
                    .unwrap_or(false)
            })
            .count();
        if count_relevant < 2 {
            continue;
        }
        if let Some(hit) = hits.get_mut(&id) {
            hit.score += boost_unit * file_sum.get(&source).copied().unwrap_or(0.0) / max_sum;
            if !hit.signals.contains(&"file_coherence") {
                hit.signals.push("file_coherence");
            }
        }
    }
}
