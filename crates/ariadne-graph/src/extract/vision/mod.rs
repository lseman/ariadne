//! Pass 3: diagram extraction.
//!
//! Diagram formats are parsed directly — SVG text nodes become `Concept`
//! nodes linked to a `Diagram` via `Illustrates` edges.

pub mod svg;
