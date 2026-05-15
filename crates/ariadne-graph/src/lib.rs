//! `ariadne-graph` — a graph-based semantic system for code, documents, and diagrams.
//!
//! Historically split across four internal crates (`ariadne-core`,
//! `ariadne-extract`, `ariadne-store`, `ariadne-query`); now collapsed into a
//! single publishable crate with corresponding modules. The CLI binary
//! `ariadne` lives in `main.rs` and consumes this library.
//!
//! Top-level re-exports mirror the old `crate::core::*` flat namespace so
//! call sites can keep writing `use ariadne_graph::{Edge, Graph, NodeId}`.

pub mod core;
pub mod extract;
pub mod query;
pub mod store;
pub mod tui;

pub use crate::core::{
    Confidence, Edge, EdgeId, EdgeKind, Graph, Node, NodeId, NodeKind,
};
