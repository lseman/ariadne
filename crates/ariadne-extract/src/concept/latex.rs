//! LaTeX extraction stub.
//!
//! Phase 2: parse with a `nom` grammar or shell out to `latexml`; emit
//! `Document` and `Section` (from `\section{...}`) nodes and cross-link
//! `\verb|...|` or `\texttt{...}` tokens to code symbols.

use anyhow::Result;
use ariadne_core::Graph;
use std::path::Path;

pub fn extract_file(_path: &Path, _graph: &mut Graph) -> Result<()> {
    tracing::debug!("latex extraction not yet implemented");
    Ok(())
}
