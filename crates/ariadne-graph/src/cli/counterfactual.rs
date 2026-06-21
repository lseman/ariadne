//! cmd_counterfactual.

use anyhow::Result;
use std::path::Path;

pub fn cmd_counterfactual(
    db: &Path,
    symbol: &str,
    direction: &str,
    max_depth: usize,
) -> Result<()> {
    let report = super::response::counterfactual_json(db, symbol, direction, max_depth)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
