//! SQLite-backed persistence for an Ariadne graph.
//!
//! Split into four modules:
//! - `db` — `Store` struct, schema, DB operations
//! - `embedding` — local feature-hash embedding model
//! - `query` — SQL row helpers, FTS query building
//! - `tests` — unit tests (cfg-gated)

pub mod db;
mod embedding;
mod query;
#[cfg(test)]
mod tests;

#[doc(hidden)] // Re-export for embedding module.
pub use db::{Store, DEFAULT_EMBEDDING_DIM, DEFAULT_EMBEDDING_MODEL, SCHEMA};

pub use db::{StoredEdgeRow, StoredNodeRow};
pub use embedding::{
    cosine_similarity, decode_embedding, embedding_source_text, semantic_embedding,
};
pub use query::build_fts5_query;
pub use query::{edge_identity, edge_row_from_sql, node_row_from_sql};
