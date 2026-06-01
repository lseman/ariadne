//! Query and reasoning kernel for Ariadne.
//!
//! This is where Ariadne differentiates itself from graphify and
//! code-review-graph. Both references expose lookups (`callers_of`,
//! `file_summary`); Ariadne exposes *primitives* that compose:
//!
//! - [`paths`]: constrained simple-path enumeration.
//! - [`centrality`]: PageRank for "god-node" detection on arbitrary
//!   subgraphs.
//! - [`communities`]: greedy modularity / Louvain-style clustering.
//! - [`motifs`]: VF2-style subgraph pattern matching (Phase 3).
//! - [`counterfactual`]: drop-edges-and-rerun (Phase 3).
//! - [`differential`]: SHA-bounded temporal diffs (Phase 2).
//! - [`search`]: exact and substring name search.

pub mod centrality;
pub mod communities;
pub mod counterfactual;
pub mod differential;
pub mod impact;
pub mod motifs;
pub mod paths;
pub mod search;
pub mod structure;

pub use centrality::{pagerank, personalized_pagerank};
pub use communities::{
    community_quality, leiden, leiden_with_options, louvain, louvain_with_options,
    CommunityObjective, CommunityOptions, CommunityQuality,
};
pub use differential::{
    is_active_at, temporal_diff, ChangedEdge, TemporalChangeKind, TemporalDiff,
};
pub use impact::{analyze_impact, ImpactHit, ImpactQuery};
pub use paths::{callees_of, callers_of, find_paths, find_top_paths, PathQuery, WeightedPath};
pub use search::{fts_ranked_search, ranked_search, search_by_name, SearchHit};
pub use structure::{
    approx_betweenness, articulation_points, bridge_scores, call_resolution_stats, core_numbers,
    cyclic_components, strongly_connected_components, BridgeScore, CallResolution, Component,
    CoreNumber,
};
