//! cmd_report.

use anyhow::Result;
use std::path::Path;

pub fn cmd_report(db: &Path, output: &str, top: usize) -> Result<()> {
    let markdown = super::response::generate_report_markdown(db, top)?;
    std::fs::write(output, markdown)?;
    println!("report written to {}", output);
    Ok(())
}
