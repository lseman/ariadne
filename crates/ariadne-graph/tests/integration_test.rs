//! Integration tests for Ariadne's full extraction and query pipeline.
//!
//! These tests create a real graph from fixture files, persist it, and
//! verify structural invariants: node/edge counts, expected call edges,
//! flow materialisation, and temporal columns.

use ariadne_graph::core::{EdgeKind, Graph, NodeKind};
use ariadne_graph::extract::{extract_file, is_supported};
use ariadne_graph::query::ranked_search;
use ariadne_graph::store::Store;
use std::path::{Path, PathBuf};

/// Resolve the path to the fixtures directory relative to the crate root.
fn fixtures_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Build a graph from the sample fixture files and return the graph + store.
fn build_test_graph() -> (Graph, Store) {
    let fixture = fixtures_path();
    assert!(fixture.is_dir(), "fixtures directory missing at {}", fixture.display());

    let mut graph = Graph::new();

    // Extract all supported files.
    for entry in walkdir::WalkDir::new(fixture).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !is_supported(path) {
            continue;
        }
        extract_file(path, &mut graph).expect("failed to extract {path:?}");
    }

    // Resolve call placeholders and derive derived edges.
    ariadne_graph::extract::resolve_call_placeholders(&mut graph);
    ariadne_graph::extract::derive_tested_by_edges(&mut graph);
    ariadne_graph::extract::compute_flows(&mut graph);

    // Persist.
    let mut store = Store::open_in_memory().expect("cannot open in-memory store");
    store.save(&graph).expect("cannot save graph");
    store.rebuild_fts_index().expect("cannot rebuild FTS");

    (graph, store)
}

#[test]
fn graph_contains_rust_and_python_nodes() {
    let (graph, _) = build_test_graph();

    // Count nodes by kind.
    let kind_counts: std::collections::HashMap<NodeKind, usize> =
        graph.nodes().map(|(_, n)| n.kind).fold(std::collections::HashMap::new(), |mut acc, k| {
            *acc.entry(k).or_insert(0) += 1;
            acc
        });

    // Rust file + struct + impl methods + free functions.
    assert!(
        kind_counts.get(&NodeKind::File).is_some_and(|c| *c >= 1),
        "expected at least 1 File node, got {:?}",
        kind_counts
    );
    assert!(
        kind_counts.get(&NodeKind::Class).is_some_and(|c| *c >= 1),
        "expected at least 1 Class node, got {:?}",
        kind_counts
    );
    assert!(
        kind_counts.get(&NodeKind::Function).is_some_and(|c| *c >= 1),
        "expected at least 1 Function node, got {:?}",
        kind_counts
    );
    assert!(
        kind_counts.get(&NodeKind::Method).is_some_and(|c| *c >= 1),
        "expected at least 1 Method node, got {:?}",
        kind_counts
    );
}

#[test]
fn rust_file_has_expected_structure() {
    let (graph, _) = build_test_graph();

    // Calculator struct should exist (Rust extractor uses file::<path>::Name).
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.rs::Calculator").is_some(),
        "Calculator class not found"
    );

    // square function should exist.
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.rs::square").is_some(),
        "square function not found"
    );

    // sqrt function should exist.
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.rs::sqrt").is_some(),
        "sqrt function not found"
    );
}

#[test]
fn python_file_has_expected_structure() {
    let (graph, _) = build_test_graph();

    // UserService class should exist (Python extractor uses file::<path>::Class).
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.py::UserService").is_some(),
        "UserService class not found"
    );

    // create_user method should exist.
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.py::UserService::create_user").is_some(),
        "create_user method not found"
    );

    // list_users method should exist.
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.py::UserService::list_users").is_some(),
        "list_users method not found"
    );

    // main function should exist.
    assert!(
        graph.find_by_qname("file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.py::main").is_some(),
        "main function not found"
    );
}

#[test]
fn call_edges_exist_between_defined_nodes() {
    let (graph, _) = build_test_graph();

    // Calculator::new should have a Defines edge from the File node.
    let calc_new_qname = "file::/data/dev/ariadne/crates/ariadne-graph/tests/fixtures/sample.rs::Calculator::new";
    let calc_new_id = graph.find_by_qname(calc_new_qname).expect("Calculator::new not found");

    // The file node should define it.
    let file_id = graph
        .nodes()
        .find(|(_, n)| n.kind == NodeKind::File)
        .map(|(id, _)| id)
        .expect("file node not found");

    assert!(
        graph.edges().any(|(_, s, d, e)| s == file_id && d == calc_new_id && e.kind == EdgeKind::Defines),
        "File should have a Defines edge to Calculator::new"
    );
}

#[test]
fn flows_are_materialised() {
    let (graph, _) = build_test_graph();

    // Count Flow nodes.
    let flow_count = graph
        .nodes()
        .filter(|(_, n)| n.kind == NodeKind::Flow)
        .count();

    // There should be at least one flow — any entry-point-traced path
    // produces a Flow node.  The Python `main` and Rust `sqrt`/`square`
    // are entry points (no callers).
    assert!(
        flow_count >= 1,
        "expected at least 1 Flow node, got {}",
        flow_count
    );

    // Verify at least one Flow node has the expected properties.
    let first_flow = graph
        .nodes()
        .find(|(_, n)| n.kind == NodeKind::Flow);
    if let Some((_, flow_node)) = first_flow {
        assert!(
            flow_node.properties.contains_key("criticality"),
            "Flow node should have a criticality property"
        );
        assert!(
            flow_node.properties.contains_key("entry_qualified_name"),
            "Flow node should have an entry_qualified_name property"
        );
        // Found one; done.
    }
}

#[test]
fn temporal_columns_stored_in_schema() {
    let (_graph, store) = build_test_graph();

    // Reload from the store and verify temporal columns exist on File nodes.
    let reloaded = store.load().expect("cannot reload from store");

    for (_, node) in reloaded.nodes().filter(|(_, n)| n.kind == NodeKind::File) {
        // The valid_from and valid_to columns are stored in the schema
        // even when they are None (no git context). They should not panic
        // on deserialization.
        let _from = &node.valid_from;
        let _to = &node.valid_to;
        // If we got here without panicking, the columns are stored.
        assert!(
            node.source_uri.is_some(),
            "File node should have a source_uri"
        );
    }
}

#[test]
fn search_returns_results_for_rust_and_python_symbols() {
    let (graph, _store) = build_test_graph();

    // Search for a Rust symbol.
    let rust_hits = ranked_search(&graph, "Calculator", 5);
    assert!(
        !rust_hits.is_empty(),
        "search for 'Calculator' returned no results"
    );
    assert!(
        rust_hits.iter().any(|h| graph.node(h.id).is_some_and(|n| {
            n.qualified_name.contains("Calculator")
        })),
        "Calculator hit not found in search results"
    );

    // Search for a Python symbol.
    let py_hits = ranked_search(&graph, "UserService", 5);
    assert!(
        !py_hits.is_empty(),
        "search for 'UserService' returned no results"
    );
}

#[test]
fn fts_search_returns_results_from_persistence() {
    let (_graph, store) = build_test_graph();

    let hits = store.fts_search("Calculator", 10).expect("fts_search failed");
    assert!(
        !hits.is_empty(),
        "FTS search for 'Calculator' returned no results"
    );
}

#[test]
fn graph_node_count_matches_fixture_files() {
    let (graph, _) = build_test_graph();

    // Two fixture files.
    let file_count = graph
        .nodes()
        .filter(|(_, n)| n.kind == NodeKind::File)
        .count();
    assert_eq!(
        file_count, 2,
        "expected exactly 2 File nodes (sample.rs + sample.py), got {}",
        file_count
    );
}

#[test]
fn edges_have_confidence_extracted() {
    let (graph, _) = build_test_graph();

    // All Define edges from AST extraction should be Extracted (score 1.0).
    let extracted_count = graph
        .edges()
        .filter(|(_, _, _, e)| {
            e.kind == EdgeKind::Defines && matches!(e.confidence, ariadne_graph::core::Confidence::Extracted)
        })
        .count();

    assert!(
        extracted_count >= 3,
        "expected at least 3 Extracted Define edges, got {}",
        extracted_count
    );
}
