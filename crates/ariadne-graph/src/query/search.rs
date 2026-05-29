use crate::core::{Graph, NodeId, NodeKind};
use crate::store::Store;
use std::collections::HashMap;
use std::path::Path;

const RRF_K: f32 = 60.0;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: NodeId,
    pub score: f32,
    pub signals: Vec<&'static str>,
}

pub fn search_by_name(graph: &Graph, query: &str) -> Vec<NodeId> {
    ranked_search(graph, query, 50)
        .into_iter()
        .map(|hit| hit.id)
        .collect()
}

pub fn ranked_search(graph: &Graph, query: &str, limit: usize) -> Vec<SearchHit> {
    let q = normalize_identifier(query);
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }

    let tokens: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<SearchHit> = graph
        .nodes()
        .filter_map(|(id, n)| {
            let name = normalize_identifier(&n.name);
            let qname = normalize_identifier(&n.qualified_name.replace("::", " "));
            let mut score = 0.0;
            let mut signals = Vec::new();
            let mut matched = false;

            if name == q || n.qualified_name.eq_ignore_ascii_case(query) {
                score += 100.0;
                signals.push("exact");
                matched = true;
            }
            if name.starts_with(&q) {
                score += 35.0;
                signals.push("prefix");
                matched = true;
            }
            if name.contains(&q) || qname.contains(&q) {
                score += 20.0;
                signals.push("substring");
                matched = true;
            }
            let token_hits = tokens
                .iter()
                .filter(|t| name.contains(**t) || qname.contains(**t))
                .count();
            if token_hits > 0 {
                score += 8.0 * token_hits as f32 / tokens.len() as f32;
                signals.push("tokens");
                matched = true;
            }
            let fuzzy = fuzzy_score(&q, &name).max(fuzzy_score(&q, &qname));
            if fuzzy >= 0.58 {
                score += fuzzy * 28.0;
                signals.push("fuzzy");
                matched = true;
            }

            if !matched {
                return None;
            }

            let indegree = graph.in_neighbors(id).count() as f32;
            let outdegree = graph.out_neighbors(id).count() as f32;
            if indegree + outdegree > 0.0 {
                score += (indegree + outdegree).ln_1p().min(4.0);
                signals.push("graph");
            }
            score += kind_prior(n.kind);

            if score > 0.0 {
                Some(SearchHit { id, score, signals })
            } else {
                None
            }
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    hits.truncate(limit);
    hits
}

fn normalize_identifier(s: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        if c.is_alphanumeric() {
            if let Some(p) = prev {
                let camel_boundary = p.is_lowercase() && c.is_uppercase();
                let acronym_boundary =
                    p.is_uppercase() && c.is_uppercase() && next.is_some_and(|n| n.is_lowercase());
                let digit_boundary = p.is_alphabetic() != c.is_alphabetic();
                if camel_boundary || acronym_boundary || digit_boundary {
                    out.push(' ');
                }
            }
            out.extend(c.to_lowercase());
            prev = Some(c);
        } else {
            out.push(' ');
            prev = None;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fuzzy_score(query: &str, candidate: &str) -> f32 {
    if query.is_empty() || candidate.is_empty() {
        return 0.0;
    }
    let compact_query = compact(query);
    let compact_candidate = compact(candidate);
    [
        ratio(query, candidate),
        ratio(&compact_query, &compact_candidate),
        partial_ratio(&compact_query, &compact_candidate),
        token_sort_ratio(query, candidate),
        token_set_ratio(query, candidate),
        acronym_ratio(query, candidate),
        subsequence_ratio(&compact_query, &compact_candidate),
    ]
    .into_iter()
    .fold(0.0, f32::max)
}

fn ratio(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let distance = levenshtein(a, b) as f32;
    1.0 - distance / a.chars().count().max(b.chars().count()) as f32
}

fn ratio_bytes(a: &[u8], b: &[u8]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let distance = levenshtein_bytes(a, b) as f32;
    1.0 - distance / a.len().max(b.len()) as f32
}

fn partial_ratio(shorter: &str, longer: &str) -> f32 {
    if shorter.is_empty() || longer.is_empty() {
        return 0.0;
    }
    if shorter.is_ascii() && longer.is_ascii() {
        return partial_ratio_bytes(shorter.as_bytes(), longer.as_bytes());
    }
    let (needle, haystack) = if shorter.chars().count() <= longer.chars().count() {
        (shorter, longer)
    } else {
        (longer, shorter)
    };
    let needle_len = needle.chars().count();
    let hay_chars: Vec<char> = haystack.chars().collect();
    if needle_len >= hay_chars.len() {
        return ratio(needle, haystack);
    }
    let mut best: f32 = 0.0;
    for start in 0..=hay_chars.len() - needle_len {
        let window: String = hay_chars[start..start + needle_len].iter().collect();
        best = best.max(ratio(needle, &window));
        if best >= 1.0 {
            break;
        }
    }
    best
}

fn partial_ratio_bytes(shorter: &[u8], longer: &[u8]) -> f32 {
    let (needle, haystack) = if shorter.len() <= longer.len() {
        (shorter, longer)
    } else {
        (longer, shorter)
    };
    if needle.len() >= haystack.len() {
        return ratio_bytes(needle, haystack);
    }
    let mut best: f32 = 0.0;
    for window in haystack.windows(needle.len()) {
        best = best.max(ratio_bytes(needle, window));
        if best >= 1.0 {
            break;
        }
    }
    best
}

fn token_sort_ratio(a: &str, b: &str) -> f32 {
    ratio(&sorted_tokens(a).join(" "), &sorted_tokens(b).join(" "))
}

fn token_set_ratio(a: &str, b: &str) -> f32 {
    let mut a_tokens = sorted_tokens(a);
    let mut b_tokens = sorted_tokens(b);
    a_tokens.dedup();
    b_tokens.dedup();
    let common: Vec<&str> = a_tokens
        .iter()
        .copied()
        .filter(|t| b_tokens.contains(t))
        .collect();
    if common.is_empty() {
        return 0.0;
    }
    let common_text = common.join(" ");
    ratio(&common_text, a).max(ratio(&common_text, b))
}

fn acronym_ratio(query: &str, candidate: &str) -> f32 {
    let acronym: String = candidate
        .split_whitespace()
        .filter_map(|token| token.chars().next())
        .collect();
    ratio(&compact(query), &acronym)
}

fn subsequence_ratio(query: &str, candidate: &str) -> f32 {
    let mut qchars = query.chars();
    let mut current = qchars.next();
    let mut matched = 0usize;
    for c in candidate.chars() {
        if Some(c) == current {
            matched += 1;
            current = qchars.next();
            if current.is_none() {
                break;
            }
        }
    }
    if current.is_none() {
        matched as f32 / candidate.chars().count().max(1) as f32
    } else {
        0.0
    }
}

fn sorted_tokens(s: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    tokens.sort_unstable();
    tokens
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a.is_ascii() && b.is_ascii() {
        return levenshtein_bytes(a.as_bytes(), b.as_bytes());
    }
    levenshtein_chars(a, b)
}

fn levenshtein_chars(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

fn levenshtein_bytes(a: &[u8], b: &[u8]) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut a = a;
    let mut b = b;
    let prefix_len = a
        .iter()
        .zip(b.iter())
        .take_while(|(ca, cb)| ca == cb)
        .count();
    a = &a[prefix_len..];
    b = &b[prefix_len..];

    let suffix_len = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(ca, cb)| ca == cb)
        .count();
    if suffix_len > 0 {
        a = &a[..a.len() - suffix_len];
        b = &b[..b.len() - suffix_len];
    }

    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    if a.len() > b.len() {
        std::mem::swap(&mut a, &mut b);
    }
    if a.len() <= usize::BITS as usize {
        return levenshtein_myers(a, b);
    }
    levenshtein_dp_bytes(a, b)
}

fn levenshtein_myers(pattern: &[u8], text: &[u8]) -> usize {
    debug_assert!(!pattern.is_empty());
    debug_assert!(pattern.len() <= usize::BITS as usize);

    let mut peq = [0usize; 256];
    for (i, &byte) in pattern.iter().enumerate() {
        peq[byte as usize] |= 1usize << i;
    }

    let last = 1usize << (pattern.len() - 1);
    let mut pv = !0usize;
    let mut mv = 0usize;
    let mut score = pattern.len();

    for &byte in text {
        let eq = peq[byte as usize];
        let xv = eq | mv;
        let xh = (((eq & pv).wrapping_add(pv)) ^ pv) | eq;
        let ph = mv | !(xh | pv);
        let mh = pv & xh;

        if (ph & last) != 0 {
            score += 1;
        }
        if (mh & last) != 0 {
            score -= 1;
        }

        let ph = (ph << 1) | 1;
        let mh = mh << 1;
        pv = mh | !(xv | ph);
        mv = ph & xv;
    }

    score
}

fn levenshtein_dp_bytes(a: &[u8], b: &[u8]) -> usize {
    let mut row: Vec<usize> = (0..=a.len()).collect();
    for (i, &bb) in b.iter().enumerate() {
        let mut prev_diag = row[0];
        row[0] = i + 1;
        for (j, &aa) in a.iter().enumerate() {
            let old = row[j + 1];
            let insert = row[j] + 1;
            let delete = old + 1;
            let replace = prev_diag + usize::from(aa != bb);
            row[j + 1] = insert.min(delete).min(replace);
            prev_diag = old;
        }
    }
    row[a.len()]
}

fn kind_prior(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Type => 4.0,
        NodeKind::Trait | NodeKind::Impl => 3.0,
        NodeKind::File | NodeKind::Module => 1.0,
        NodeKind::Document | NodeKind::Section | NodeKind::Concept => 2.0,
        NodeKind::Diagram | NodeKind::Image => 1.5,
        NodeKind::Variable | NodeKind::Hyperedge | NodeKind::Commit | NodeKind::Author => 0.5,
        // Flow nodes are synthetic and should be findable but not ranked
        // ahead of real symbols when a user types a generic query.
        NodeKind::Flow => 1.5,
    }
}

fn query_kind_boosts(query: &str) -> Vec<(NodeKind, f32)> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut boosts = Vec::new();
    let mut chars = q.chars();
    if matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && q.chars().any(|c| c.is_ascii_lowercase())
    {
        boosts.push((NodeKind::Class, 1.35));
        boosts.push((NodeKind::Type, 1.35));
        boosts.push((NodeKind::Trait, 1.20));
    }
    if q.contains('_') && q.chars().any(|c| c.is_ascii_alphabetic()) {
        boosts.push((NodeKind::Function, 1.35));
        boosts.push((NodeKind::Method, 1.35));
    }
    boosts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node};
    use crate::store::{Store, DEFAULT_EMBEDDING_MODEL};

    #[test]
    fn ranked_search_prefers_exact_symbols() {
        let mut g = Graph::new();
        let file = g.add_node(Node::new(NodeKind::File, "file::src/lib.rs"));
        let f = g.add_node(Node::new(
            NodeKind::Function,
            "file::src/lib.rs::extract_directory",
        ));
        g.add_edge(file, f, Edge::extracted(EdgeKind::Defines));
        let hits = ranked_search(&g, "extract_directory", 10);
        assert_eq!(hits[0].id, f);
    }

    #[test]
    fn fuzzy_search_handles_typos_camel_case_and_acronyms() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::extract_directory"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::HTTPRequestParser"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::ranked_search"));

        assert_eq!(ranked_search(&g, "extractDirectory", 10)[0].id, a);
        assert_eq!(ranked_search(&g, "http parser", 10)[0].id, b);
        assert_eq!(ranked_search(&g, "rnked serch", 10)[0].id, c);
    }

    #[test]
    fn search_normalization_preserves_unicode_identifiers() {
        let mut g = Graph::new();
        let cafe = g.add_node(Node::new(NodeKind::Function, "pkg::CaféParser"));
        let greek = g.add_node(Node::new(NodeKind::Function, "pkg::ΔιαδρομήParser"));

        assert_eq!(ranked_search(&g, "café parser", 10)[0].id, cafe);
        assert_eq!(ranked_search(&g, "διαδρομή parser", 10)[0].id, greek);
        assert_eq!(
            normalize_identifier("CaféParser Δelta42"),
            "café parser δelta 42"
        );
    }

    #[test]
    fn levenshtein_fast_path_matches_known_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("extract_directory", "extract_dirctory"), 1);
        assert_eq!(levenshtein("HTTPRequestParser", "HTTPParser"), 7);
        assert_eq!(
            levenshtein(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2",
            ),
            1
        );
        assert_eq!(levenshtein("cafe", "café"), 1);
    }

    #[test]
    fn partial_ratio_scores_ascii_windows_without_allocating_strings() {
        assert_eq!(
            partial_ratio("rankedsearch", "prefixrankedsearchsuffix"),
            1.0
        );
        assert!(partial_ratio("rnkedserch", "rankedsearch") >= 0.8);
    }

    #[test]
    fn hybrid_search_uses_semantic_embeddings_when_present() {
        let mut g = Graph::new();
        let remove = g.add_node(Node::new(NodeKind::Function, "pkg::remove_sources"));
        g.add_node(Node::new(NodeKind::Function, "pkg::build_graph"));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();
        store.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = fts_ranked_search(&store, &g, "delete source", 5);
        assert_eq!(hits[0].id, remove);
        assert!(hits[0].signals.contains(&"semantic"));
    }

    #[test]
    fn fts_search_applies_query_kind_boosts() {
        let mut g = Graph::new();
        let class = g.add_node(Node::new(NodeKind::Class, "pkg::UserService"));
        g.add_node(Node::new(NodeKind::Function, "pkg::user_service"));
        let function = g.add_node(Node::new(NodeKind::Function, "pkg::get_users"));
        g.add_node(Node::new(NodeKind::Class, "pkg::GetUsers"));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let class_hits = fts_ranked_search(&store, &g, "UserService", 10);
        assert_eq!(class_hits[0].id, class);
        assert!(class_hits[0].signals.contains(&"kind_boost"));

        let function_hits = fts_ranked_search(&store, &g, "get_users", 10);
        assert_eq!(function_hits[0].id, function);
        assert!(function_hits[0].signals.contains(&"kind_boost"));
    }

    #[test]
    fn hybrid_search_boosts_symbol_definitions() {
        let mut g = Graph::new();
        let class = g.add_node(Node::new(NodeKind::Class, "pkg::UserService").with_source(
            "src/user_service.rs",
            1,
            5,
        ));
        g.add_node(
            Node::new(NodeKind::Variable, "pkg::uses_UserService").with_source("src/main.rs", 8, 9),
        );

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let hits = fts_ranked_search(&store, &g, "UserService", 10);
        assert_eq!(hits[0].id, class);
        assert!(hits[0].signals.contains(&"definition_boost"));
    }

    #[test]
    fn hybrid_search_penalizes_call_placeholders() {
        let mut g = Graph::new();
        let real = g.add_node(
            Node::new(NodeKind::Function, "pkg::save_model").with_source("src/model.rs", 1, 5),
        );
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::save_model"));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let hits = fts_ranked_search(&store, &g, "save_model", 10);
        assert_eq!(hits[0].id, real);
        let placeholder_hit = hits.iter().find(|hit| hit.id == placeholder).unwrap();
        assert!(placeholder_hit.signals.contains(&"placeholder_penalty"));
        assert!(!placeholder_hit.signals.contains(&"definition_boost"));
    }

    #[test]
    fn hybrid_search_boosts_coherent_files() {
        let mut g = Graph::new();
        let auth_a = g.add_node(
            Node::new(NodeKind::Function, "pkg::auth_login").with_source("src/auth.rs", 1, 5),
        );
        g.add_node(
            Node::new(NodeKind::Function, "pkg::auth_logout").with_source("src/auth.rs", 7, 11),
        );
        g.add_node(
            Node::new(NodeKind::Function, "pkg::login_helper").with_source("src/login.rs", 1, 3),
        );

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let hits = fts_ranked_search(&store, &g, "auth", 10);
        let auth_hit = hits.iter().find(|hit| hit.id == auth_a).unwrap();
        assert!(auth_hit.signals.contains(&"file_coherence"));
    }
}

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

    if fts_hits.is_empty() && semantic_hits.is_empty() {
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
        if let Some(id) = graph.find_by_qname(&qname) {
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
    hits.truncate(limit);
    hits
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

fn search_query_tokens(normalized_query: &str) -> Vec<String> {
    normalized_query
        .split_whitespace()
        .filter(|token| token.len() >= 3 && !SEARCH_STOPWORDS.contains(token))
        .map(ToOwned::to_owned)
        .collect()
}

const SEARCH_STOPWORDS: &[&str] = &[
    "and", "are", "for", "from", "has", "have", "how", "the", "what", "when", "where", "who",
    "why", "with",
];

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
