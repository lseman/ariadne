use ariadne_core::{Graph, NodeId, NodeKind};

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
        if c.is_ascii_alphanumeric() {
            if let Some(p) = prev {
                let camel_boundary = p.is_ascii_lowercase() && c.is_ascii_uppercase();
                let acronym_boundary = p.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase());
                let digit_boundary = p.is_ascii_alphabetic() != c.is_ascii_alphabetic();
                if camel_boundary || acronym_boundary || digit_boundary {
                    out.push(' ');
                }
            }
            out.push(c.to_ascii_lowercase());
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

fn partial_ratio(shorter: &str, longer: &str) -> f32 {
    if shorter.is_empty() || longer.is_empty() {
        return 0.0;
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

fn kind_prior(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Type => 4.0,
        NodeKind::Trait | NodeKind::Impl => 3.0,
        NodeKind::File | NodeKind::Module => 1.0,
        NodeKind::Document | NodeKind::Section | NodeKind::Concept => 2.0,
        NodeKind::Diagram | NodeKind::Image => 1.5,
        NodeKind::Variable | NodeKind::Hyperedge | NodeKind::Commit | NodeKind::Author => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_core::{Edge, EdgeKind, Node};

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
}
