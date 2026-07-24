//! Pass 2: prose and diagram extraction.
//!
//! Reads documentation files (markdown, HTML) and diagram files
//! (SVG), builds `Document` / `Section` / `Diagram` nodes, and
//! cross-links any code symbol referenced by name back to its
//! `Function` / `Class` node from pass 1. Edges emitted here always
//! carry `Confidence::Inferred(score)`.

pub mod concept_registry;
mod document_utils;
pub mod html;
pub mod markdown;

pub use concept_registry::resolve_all_mentions;
pub use html::extract_file as extract_html;
pub use markdown::extract_file as extract_markdown;
