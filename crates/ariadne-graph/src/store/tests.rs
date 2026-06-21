#[cfg(test)]
mod tests {
    use crate::core::{Edge, EdgeKind, Graph, Node, NodeKind};
    use crate::store::{Store, StoredNodeRow, DEFAULT_EMBEDDING_MODEL};

    #[test]
    fn round_trip_in_memory() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let b = g.add_node(Node::new(NodeKind::Function, "m::g"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        let loaded = s.load().unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);
        assert!(loaded.find_by_qname("m::f").is_some());
    }

    #[test]
    fn fts_search_finds_node_by_name() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_changes"));
        g.add_node(Node::new(NodeKind::Function, "mymod::graph_builder"));
        g.add_node(Node::new(NodeKind::Class, "mymod::GraphNode"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("detect", 10).unwrap();
        assert!(
            !hits.is_empty(),
            "expected at least one FTS hit for 'detect'"
        );
        assert!(hits.iter().any(|(qn, _)| qn == "mymod::detect_changes"));
    }

    #[test]
    fn fts_search_prefix_match() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::graph_traversal"));
        g.add_node(Node::new(NodeKind::Function, "ns::path_finder"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("graph", 10).unwrap();
        assert!(hits.iter().any(|(qn, _)| qn == "ns::graph_traversal"));
        // unrelated node should not appear
        assert!(!hits.iter().any(|(qn, _)| qn == "ns::path_finder"));
    }

    #[test]
    fn fts_search_empty_query_returns_empty() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::f"));
        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        assert!(s.fts_search("", 10).unwrap().is_empty());
        assert!(s.fts_search("  ", 10).unwrap().is_empty());
    }

    #[test]
    fn rebuild_fts_index_reports_indexed_rows() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::alpha_search"));
        g.add_node(Node::new(NodeKind::Class, "ns::BetaSearch"));
        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let count = s.rebuild_fts_index().unwrap();
        assert_eq!(count, 2);
        assert_eq!(s.fts_stats().unwrap(), 2);
        assert!(!s.fts_search("alpha", 10).unwrap().is_empty());
    }

    #[test]
    fn load_temporal_includes_archived_rows() {
        // Save an active graph, archive one node, then confirm load()
        // omits the archived row while load_temporal() includes it with
        // its closing commit intact.
        let mut g = Graph::new();
        let mut keep = Node::new(NodeKind::Function, "m::keep");
        keep.valid_from = Some("c1".to_string());
        keep.source_uri = Some("src/a.rs".to_string());
        g.add_node(keep);

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let mut gone = Node::new(NodeKind::Function, "m::gone");
        gone.valid_from = Some("c1".to_string());
        gone.source_uri = Some("src/a.rs".to_string());
        s.archive_nodes(&[StoredNodeRow { node: gone }], "c2")
            .unwrap();

        let active = s.load().unwrap();
        assert!(active.find_by_qname("m::keep").is_some());
        assert!(
            active.find_by_qname("m::gone").is_none(),
            "load() must not surface archived rows"
        );

        let temporal = s.load_temporal().unwrap();
        let gone_id = temporal
            .find_by_qname("m::gone")
            .expect("load_temporal must surface archived rows");
        let gone_node = temporal.node(gone_id).unwrap();
        assert_eq!(gone_node.valid_from.as_deref(), Some("c1"));
        assert_eq!(gone_node.valid_to.as_deref(), Some("c2"));
    }

    #[test]
    fn fts_search_multi_word_matches_snake_case() {
        // With separators "_", "detect_changes" indexes as tokens "detect" + "changes".
        // A two-word query "detect changes" should AND-match both tokens.
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_changes"));
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_errors"));
        g.add_node(Node::new(NodeKind::Function, "mymod::apply_changes"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("detect changes", 10).unwrap();
        // Only "detect_changes" contains both tokens.
        assert_eq!(
            hits.len(),
            1,
            "expected only detect_changes to match 'detect changes'"
        );
        assert_eq!(hits[0].0, "mymod::detect_changes");
    }

    #[test]
    fn delete_sources_removes_fts_rows() {
        let mut g = Graph::new();
        let mut n = Node::new(NodeKind::Function, "mod::stale_fn");
        n.source_uri = Some("src/stale.rs".to_string());
        g.add_node(n);
        let mut n2 = Node::new(NodeKind::Function, "mod::keep_fn");
        n2.source_uri = Some("src/keep.rs".to_string());
        g.add_node(n2);

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        // confirm both are searchable
        assert!(!s.fts_search("stale", 10).unwrap().is_empty());
        assert!(!s.fts_search("keep", 10).unwrap().is_empty());

        s.delete_sources(&["src/stale.rs".to_string()]).unwrap();

        // stale_fn must be gone from FTS
        assert!(
            s.fts_search("stale", 10).unwrap().is_empty(),
            "stale FTS row should be removed"
        );
        // keep_fn must still be found
        assert!(!s.fts_search("keep", 10).unwrap().is_empty());
    }

    #[test]
    fn build_fts5_query_produces_prefix_terms() {
        use crate::store::build_fts5_query;
        assert_eq!(build_fts5_query("detect changes"), "detect* changes*");
        assert_eq!(build_fts5_query("graph"), "graph*");
        assert_eq!(build_fts5_query("detect_changes"), "detect_changes*");
        assert!(build_fts5_query("").is_empty());
    }

    #[test]
    fn semantic_search_finds_related_terms() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "pkg::remove_sources"));
        g.add_node(Node::new(NodeKind::Function, "pkg::build_graph"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        s.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = s.semantic_search("delete source", 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "pkg::remove_sources");
    }

    #[test]
    fn hash_v2_captures_code_aware_semantic_concepts() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "pkg::rebuild_embeddings"));
        g.add_node(Node::new(NodeKind::Function, "pkg::install_mcp_config"));
        g.add_node(Node::new(NodeKind::Function, "pkg::compute_flows"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        s.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let semantic_hits = s
            .semantic_search("vector semantic search ranking", 5)
            .unwrap();
        assert!(!semantic_hits.is_empty());
        assert_eq!(semantic_hits[0].0, "pkg::rebuild_embeddings");

        let agent_hits = s.semantic_search("agent json tool setup", 5).unwrap();
        assert!(!agent_hits.is_empty());
        assert_eq!(agent_hits[0].0, "pkg::install_mcp_config");
    }
}
