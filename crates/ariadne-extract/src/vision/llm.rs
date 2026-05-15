//! LLM-backed vision extraction (stub).
//!
//! When wired up, this module will:
//!
//! 1. Read the image bytes from disk.
//! 2. Hash the contents (sha256) and consult a local cache.
//! 3. On miss, post to the Anthropic Messages API (or OpenAI Vision /
//!    Gemini) with a structured prompt asking for `(concept, related)`
//!    triples.
//! 4. Materialise the response as `Concept` and `Image` nodes plus
//!    `Illustrates` / `Mentions` edges with `Confidence::Inferred`.
//!
//! Everything in this module is gated behind the (yet to be added)
//! `vision-api` feature.

use anyhow::Result;
use ariadne_core::Graph;
use std::path::Path;

pub fn extract_file(_path: &Path, _graph: &mut Graph) -> Result<()> {
    tracing::debug!("vision LLM extraction not yet implemented");
    Ok(())
}
