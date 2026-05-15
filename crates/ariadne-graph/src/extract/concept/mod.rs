//! Pass 2: prose extraction.
//!
//! Reads documentation files, builds `Document` / `Section` nodes, and
//! cross-links any code symbol referenced by name back to its
//! `Function` / `Class` node from pass 1. Edges emitted here always
//! carry `Confidence::Inferred(score)`.

pub mod latex;
pub mod markdown;
