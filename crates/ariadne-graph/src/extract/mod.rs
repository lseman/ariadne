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

pub use concept::markdown::resolve_mentions;
pub use flows::{
    affected_flows, all_flows, compute_flows, compute_flows_with_options, flows_through,
    FlowOptions,
};
pub use walker::{
    derive_tested_by_edges, extract_directory, extract_directory_with_custom, extract_file,
    extract_file_with_custom, ignore_set, is_relevant_source, is_supported,
    resolve_call_placeholders, should_suppress_call_placeholder, IgnoreSet,
};

// Re-export the language registry for external use.
pub use ast::language_registry::{get_language, get_language_by_path, registry, LanguageDef};
