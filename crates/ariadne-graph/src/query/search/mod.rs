use crate::core::{Graph, NodeId, NodeKind};

mod fusion;
mod fuzzy;
mod vocabulary;

use fuzzy::{fuzzy_score, normalize_identifier};

pub use fusion::fts_ranked_search;

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
    let query_identifiers = extract_query_identifiers(query);
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
            let identifier_hits = query_identifiers
                .iter()
                .filter(|identifier| {
                    name.contains(identifier.as_str()) || qname.contains(identifier.as_str())
                })
                .count();
            if identifier_hits > 0 {
                score += 45.0 * identifier_hits as f32 / query_identifiers.len() as f32;
                signals.push("identifier");
                matched = true;
            }
            // Fuzzy scoring is expensive (7 similarity algorithms). Skip when
            // a cheaper match already hit, and skip when the candidate is
            // shorter than the query (can't be a fuzzy match for typos).
            if !matched && name.len() >= q.len() {
                let fuzzy = fuzzy_score(&q, &name).max(fuzzy_score(&q, &qname));
                if fuzzy >= 0.58 {
                    score += fuzzy * 28.0;
                    signals.push("fuzzy");
                    matched = true;
                }
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
            fusion::apply_noise_penalty(&mut score, &mut signals, n, &q);

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
    fusion::apply_source_saturation(&mut hits, graph);
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    hits.truncate(limit);
    hits
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

fn extract_query_identifiers(query: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    for token in query.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))) {
        let token = token.trim_matches(|c: char| matches!(c, '.' | ':' | '_'));
        if token.len() < 3 || !is_identifier_shaped(token) {
            continue;
        }
        let normalized = normalize_identifier(&token.replace("::", " "));
        if !normalized.is_empty() && !identifiers.contains(&normalized) {
            identifiers.push(normalized);
        }
    }
    identifiers
}

fn is_identifier_shaped(token: &str) -> bool {
    let has_separator = token.contains('_') || token.contains('.') || token.contains("::");
    let mut chars = token.chars().filter(|c| c.is_alphabetic());
    let Some(first) = chars.next() else {
        return false;
    };
    let has_camel_boundary = first.is_uppercase() && chars.any(|c| c.is_uppercase());
    has_separator || has_camel_boundary
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
    fn ranked_search_extracts_identifiers_from_natural_language() {
        let mut g = Graph::new();
        let next = g.add_node(Node::new(NodeKind::Method, "gin::Context::Next"));
        g.add_node(Node::new(NodeKind::Class, "gin::Context"));
        g.add_node(Node::new(NodeKind::Function, "pkg::middleware_chain"));

        let hits = ranked_search(&g, "who advances the middleware via Context.Next", 10);
        assert_eq!(hits[0].id, next);
        assert!(hits[0].signals.contains(&"identifier"));
    }

    #[test]
    fn search_normalization_preserves_unicode_identifiers() {
        let mut g = Graph::new();
        let cafe = g.add_node(Node::new(NodeKind::Function, "pkg::CaféParser"));
        let greek = g.add_node(Node::new(NodeKind::Function, "pkg::ΔιαδρομήParser"));

        assert_eq!(ranked_search(&g, "café parser", 10)[0].id, cafe);
        assert_eq!(ranked_search(&g, "διαδρομή parser", 10)[0].id, greek);
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
    fn hybrid_search_boosts_identifiers_inside_natural_language() {
        let mut g = Graph::new();
        let next = g.add_node(
            Node::new(NodeKind::Method, "gin::Context::Next").with_source("src/context.rs", 1, 5),
        );
        g.add_node(Node::new(NodeKind::Class, "gin::Context").with_source("src/context.rs", 1, 50));
        g.add_node(
            Node::new(NodeKind::Function, "pkg::middleware_chain").with_source(
                "src/middleware.rs",
                1,
                10,
            ),
        );

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let hits = fts_ranked_search(
            &store,
            &g,
            "who advances the middleware chain via Context.Next",
            10,
        );
        assert_eq!(hits[0].id, next);
        assert!(hits[0].signals.contains(&"identifier_boost"));
    }

    #[test]
    fn ranked_search_penalizes_test_files_for_generic_queries() {
        let mut g = Graph::new();
        let prod = g.add_node(Node::new(NodeKind::Function, "pkg::login").with_source(
            "src/auth.rs",
            10,
            20,
        ));
        let test = g.add_node(
            Node::new(NodeKind::Function, "pkg::tests::login").with_source(
                "tests/test_auth.rs",
                10,
                20,
            ),
        );

        let hits = ranked_search(&g, "login", 10);
        assert_eq!(hits[0].id, prod);
        let test_hit = hits.iter().find(|hit| hit.id == test).unwrap();
        assert!(test_hit.signals.contains(&"noise_penalty"));

        let test_hits = ranked_search(&g, "login tests", 10);
        let test_hit = test_hits.iter().find(|hit| hit.id == test).unwrap();
        assert!(!test_hit.signals.contains(&"noise_penalty"));
    }

    #[test]
    fn hybrid_search_penalizes_declaration_and_example_files() {
        let mut g = Graph::new();
        let prod = g.add_node(Node::new(NodeKind::Function, "pkg::Client").with_source(
            "src/client.ts",
            1,
            20,
        ));
        let declaration = g.add_node(
            Node::new(NodeKind::Function, "pkg::types::Client").with_source(
                "src/client.d.ts",
                1,
                20,
            ),
        );
        let example = g.add_node(
            Node::new(NodeKind::Function, "pkg::examples::Client").with_source(
                "examples/client.ts",
                1,
                20,
            ),
        );

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();

        let hits = fts_ranked_search(&store, &g, "Client", 10);
        assert_eq!(hits[0].id, prod);
        assert!(hits
            .iter()
            .find(|hit| hit.id == declaration)
            .unwrap()
            .signals
            .contains(&"noise_penalty"));
        assert!(hits
            .iter()
            .find(|hit| hit.id == example)
            .unwrap()
            .signals
            .contains(&"noise_penalty"));
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

    #[test]
    fn ranked_search_applies_source_saturation_to_repeated_file_hits() {
        let mut g = Graph::new();
        let first = g.add_node(
            Node::new(NodeKind::Function, "pkg::auth::auth_login").with_source("src/auth.rs", 1, 5),
        );
        let second = g.add_node(
            Node::new(NodeKind::Function, "pkg::auth::auth_logout").with_source(
                "src/auth.rs",
                7,
                11,
            ),
        );
        let other = g.add_node(
            Node::new(NodeKind::Function, "pkg::session::auth_session").with_source(
                "src/session.rs",
                1,
                5,
            ),
        );

        let hits = ranked_search(&g, "auth", 10);
        let first_hit = hits.iter().find(|hit| hit.id == first).unwrap();
        let second_hit = hits.iter().find(|hit| hit.id == second).unwrap();
        let other_hit = hits.iter().find(|hit| hit.id == other).unwrap();

        assert!(!first_hit.signals.contains(&"source_saturation"));
        assert!(second_hit.signals.contains(&"source_saturation"));
        assert!(!other_hit.signals.contains(&"source_saturation"));
    }
}
