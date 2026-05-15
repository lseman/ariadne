use serde::{Deserialize, Serialize};

/// Stable identifier for a node within a [`crate::Graph`].
///
/// The underlying integer matches the index used by the in-memory
/// `petgraph::StableDiGraph`, so IDs survive removals.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Stable identifier for an edge within a [`crate::Graph`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u32);
