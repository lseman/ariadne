//! cmd_large_functions, cmd_bridge_nodes, cmd_cycles, cmd_core, cmd_articulation, cmd_gaps,
//! cmd_diagnostics, cmd_surprises, cmd_suggested_questions, cmd_architecture.

use anyhow::Result;
use ariadne_graph::store::Store;
use std::path::Path;

pub fn cmd_large_functions(db: &Path, min_lines: u32, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::large_functions_json(
            &graph, min_lines, top
        ))?
    );
    Ok(())
}

pub fn cmd_bridge_nodes(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::bridge_nodes_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_cycles(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::cycles_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_core(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::core_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_articulation(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::articulation_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_gaps(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::gaps_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_diagnostics(db: &Path, top: usize) -> Result<()> {
    let report = super::response::diagnostics_json(db, top)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn cmd_surprises(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::surprises_json(&graph, top))?
    );
    Ok(())
}

pub fn cmd_suggested_questions(db: &Path, base: &str, top: usize) -> Result<()> {
    let analysis = super::response::detect_changes_json(db, base, 2)?;
    let questions = super::response::suggested_questions_json(&analysis, top);
    println!("{}", serde_json::to_string_pretty(&questions)?);
    Ok(())
}

pub fn cmd_architecture(db: &Path, detail_level: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let detail = super::response::DetailLevel::parse(detail_level);
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::architecture_overview_json(&graph, detail))?
    );
    Ok(())
}
