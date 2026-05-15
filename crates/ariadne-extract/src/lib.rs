//! Ariadne extraction passes.
//!
//! The pipeline runs in three passes:
//!
//! 1. **AST pass** — tree-sitter parses every supported source file and
//!    emits structural nodes (`Function`, `Class`, …) and edges
//!    (`Defines`, `Calls`, `Imports`, …). Deterministic, parallel, no
//!    network. Implemented in [`ast`].
//!
//! 2. **Concept pass** — markdown / LaTeX / PDF text is parsed and
//!    cross-linked to symbols discovered in pass 1 by name. Emits
//!    `Document`, `Section`, `Concept` nodes and `Mentions` /
//!    `Describes` edges with `Confidence::Inferred`. Scaffolded in
//!    [`concept`].
//!
//! 3. **Vision pass** — diagrams and bitmap images are extracted via a
//!    vision LLM (or parsed directly when the format is text-based, e.g.
//!    SVG / Mermaid / PlantUML). Scaffolded in [`vision`].

pub mod ast;
pub mod concept;
pub mod vision;
pub mod walker;

pub use walker::{
    extract_directory, extract_file, ignore_set, is_supported, resolve_call_placeholders, IgnoreSet,
};
