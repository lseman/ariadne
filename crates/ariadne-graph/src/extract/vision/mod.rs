//! Pass 3: vision and diagram extraction.
//!
//! Text-based diagram formats (SVG, Mermaid, PlantUML) are parsed
//! directly. Bitmap formats (PNG, JPG, screenshots) are sent to a
//! vision LLM — that path requires network and an API key and is
//! scaffolded in [`llm`].

pub mod llm;
pub mod svg;
