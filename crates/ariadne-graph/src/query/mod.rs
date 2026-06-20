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
pub mod dedup;
pub mod differential;
pub mod export;
pub mod impact;
pub mod motifs;
pub mod paths;
pub mod refactor;
pub mod search;
pub mod semsearch;
pub mod structure;

pub use centrality::{is_rank_noise, pagerank, personalized_pagerank};
pub use communities::{
    community_cohesion, community_quality, infomap, infomap_with_options, knowledge_gaps,
    leiden, leiden_with_options, louvain, louvain_with_options, split_oversized,
    CommunityObjective, CommunityOptions, CommunityQuality, LOW_COHESION_THRESHOLD,
};
pub use dedup::{deduplicate_nodes, DedupOptions, DedupResult};
pub use differential::{
    is_active_at, temporal_diff, ChangedEdge, TemporalChangeKind, TemporalDiff,
};
pub use impact::{analyze_impact, ImpactHit, ImpactQuery};
pub use paths::{callees_of, callers_of, find_paths, find_top_paths, PathQuery, WeightedPath};
pub use search::{fts_ranked_search, ranked_search, search_by_name, SearchHit};
pub use semsearch::{find_related, semantic_query, SemanticHit};
pub use refactor::{find_dead_code, rename_preview, Confidence, RenameEdit, RenamePreview};
pub use structure::{
    approx_betweenness, articulation_points, bridge_scores, call_resolution_stats, core_numbers,
    cyclic_components, hub_nodes, strongly_connected_components, BridgeScore, CallResolution,
    Component, CoreNumber, HubNode,
};
