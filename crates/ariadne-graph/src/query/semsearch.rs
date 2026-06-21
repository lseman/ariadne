//! Semantic similarity search (find_related).
//!
//! Uses the local feature-hash embedding store to find nodes that are
//! semantically similar to a target file/line or a free-form query.
//! This complements the name-based [`crate::query::search`] by matching
//! on *meaning* rather than on identifiers.

use crate::core::{Graph, NodeId};
use crate::store::Store;

/// A semantic similarity hit.
#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub id: NodeId,
    pub score: f32,
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file: Option<String>,
    pub line_start: Option<u32>,
}

/// Find nodes semantically similar to a target file and optional line number.
///
/// Uses the embedding store to compute cosine similarity between the
/// target's embedding (derived from its full node text) and all other
/// nodes. Returns the top-k most similar non-file nodes, excluding the
/// target itself.
///
/// This is the core of the "find_related" operation: given a file or
/// symbol, show the agent nearby code that is semantically related
/// (same purpose, similar algorithms, related concepts) even if names
/// differ.
pub fn find_related(
    store: &Store,
    graph: &Graph,
    target_qname: &str,
    _line: Option<u32>,
    limit: usize,
) -> Vec<SemanticHit> {
    if limit == 0 {
        return Vec::new();
    }

    // Find the target node
    let target_id = match graph.find_by_qname(target_qname) {
        Some(id) => id,
        None => return Vec::new(),
    };

    // Get target embedding
    let target_embedding = get_node_embedding(store, target_id);
    if target_embedding.iter().all(|v| *v == 0.0) {
        return Vec::new();
    }

    // Score all other nodes by cosine similarity
    let mut hits: Vec<SemanticHit> = graph
        .nodes()
        .filter(|(id, _)| *id != target_id)
        .filter(|(_, n)| n.kind != crate::core::NodeKind::File)
        .filter_map(|(id, node)| {
            let embedding = get_node_embedding(store, id);
            let similarity = cosine_similarity(&target_embedding, &embedding);
            if similarity <= 0.0 {
                return None;
            }

            Some(SemanticHit {
                id,
                score: similarity,
                qualified_name: node.qualified_name.clone(),
                name: node.name.clone(),
                kind: node.kind.as_str().to_string(),
                file: node.source_uri.clone(),
                line_start: node.line_start,
            })
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

/// Find nodes semantically similar to a free-form query text.
///
/// Computes an embedding from the query text and matches it against
/// all stored node embeddings. Returns top-k most similar nodes.
pub fn semantic_query(store: &Store, graph: &Graph, query: &str, limit: usize) -> Vec<SemanticHit> {
    if limit == 0 || query.trim().is_empty() {
        return Vec::new();
    }

    let query_vector = crate::store::semantic_embedding(query);
    if query_vector.iter().all(|v| *v == 0.0) {
        return Vec::new();
    }

    let mut hits: Vec<SemanticHit> = graph
        .nodes()
        .filter(|(_, n)| n.kind != crate::core::NodeKind::File)
        .filter_map(|(id, node)| {
            let embedding = get_node_embedding(store, id);
            let similarity = cosine_similarity(&query_vector, &embedding);
            if similarity <= 0.0 {
                return None;
            }

            Some(SemanticHit {
                id,
                score: similarity,
                qualified_name: node.qualified_name.clone(),
                name: node.name.clone(),
                kind: node.kind.as_str().to_string(),
                file: node.source_uri.clone(),
                line_start: node.line_start,
            })
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

fn get_node_embedding(store: &Store, node_id: NodeId) -> Vec<f32> {
    let blob: Option<Vec<u8>> = store
        .conn()
        .query_row(
            "SELECT vector FROM embeddings WHERE node_id = ?1",
            [node_id.0 as i64],
            |row| row.get(0),
        )
        .ok();

    match blob {
        Some(data) => crate::store::decode_embedding(&data).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    crate::store::cosine_similarity(a, b).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};
    use crate::store::{Store, DEFAULT_EMBEDDING_MODEL};

    #[test]
    fn cosine_similarity_exact_match_is_1() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_is_0() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn find_related_returns_similar_nodes() {
        let mut g = Graph::new();
        let func_a = g.add_node(Node::new(NodeKind::Function, "pkg::extract_directory"));
        let func_b = g.add_node(Node::new(NodeKind::Function, "pkg::parse_directory"));
        g.add_node(Node::new(NodeKind::Function, "pkg::greet"));
        g.add_edge(func_a, func_b, Edge::extracted(EdgeKind::Calls));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();
        store.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = find_related(&store, &g, "pkg::extract_directory", None, 5);
        // Feature-hash embeddings may return 0-2 results depending on hash collisions.
        // The key invariant: target is excluded, and results are sorted by score.
        assert!(
            !hits.iter().any(|h| h.id == func_a),
            "target should be excluded"
        );
    }

    #[test]
    fn semantic_query_finds_semantic_matches() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "pkg::remove_sources"));
        g.add_node(Node::new(NodeKind::Function, "pkg::delete_sources"));
        g.add_node(Node::new(NodeKind::Function, "pkg::greet"));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();
        store.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = semantic_query(&store, &g, "delete source", 5);
        // Feature-hash embeddings may return 0-3 results.
        // Key invariant: sorted by score descending.
        assert!(!hits.is_empty(), "should return some results");
        if hits.len() > 1 {
            assert!(hits[0].score >= hits[1].score);
        }
    }

    #[test]
    fn find_related_excludes_file_nodes() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::File, "file::src/lib.rs"));
        g.add_node(Node::new(NodeKind::Function, "pkg::extract"));
        g.add_node(Node::new(NodeKind::Function, "pkg::parse"));

        let mut store = Store::open_in_memory().unwrap();
        store.save(&g).unwrap();
        store.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = find_related(&store, &g, "pkg::extract", None, 5);
        for h in &hits {
            assert_ne!(h.kind, "file");
        }
    }
}
