//! Ariadne core: graph types and in-memory store.
//!
//! The graph models source code, documentation, and diagrams as a typed
//! property graph. Nodes have a [`NodeKind`] (`Function`, `Document`, etc.);
//! edges carry a [`EdgeKind`] and a [`Confidence`] tag that distinguishes
//! deterministic structural extraction from inferred semantic links.

pub mod edge;
pub mod graph;
pub mod id;
pub mod node;

pub use edge::{Confidence, Edge, EdgeKind};
pub use graph::Graph;
pub use id::{EdgeId, NodeId};
pub use node::{Node, NodeKind};
