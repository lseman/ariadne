//! Ariadne extraction passes.
//!
//! The pipeline runs in three passes:
//!
//! 1. **AST pass** — tree-sitter parses every supported source file and
//!    emits structural nodes (`Function`, `Class`, …) and edges
//!    (`Defines`, `Calls`, `Imports`, …). Deterministic, parallel, no
//!    network. Implemented in [`ast`].
//!
//! 2. **Concept pass** — markdown / LaTeX text is parsed and
//!    cross-linked to symbols discovered in pass 1 by name. Emits
//!    `Document`, `Section`, `Concept` nodes and `Mentions` /
//!    `Describes` edges with `Confidence::Inferred`. Implemented in
//!    [`concept`].
//!
//! 3. **Vision pass** — diagram formats (SVG, Mermaid, PlantUML) are
//!    parsed directly to extract concepts. Implemented in [`vision`].

pub mod ast;
pub mod concept;
pub mod flows;
pub mod test_detect;
pub mod vision;
pub mod walker;

pub use flows::{
    affected_flows, all_flows, compute_flows, compute_flows_with_options, flows_through,
    FlowOptions,
};
pub use concept::markdown::resolve_mentions;
pub use walker::{
    derive_tested_by_edges, extract_directory, extract_file, ignore_set, is_supported,
    resolve_call_placeholders, should_suppress_call_placeholder, IgnoreSet,
};
