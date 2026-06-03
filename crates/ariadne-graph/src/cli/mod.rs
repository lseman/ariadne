/// CLI argument definitions and dispatch.
pub mod handlers;
pub mod helpers;
pub mod response;
pub mod git;
pub mod http;
pub mod mcp;

use anyhow::Result;
use std::path::Path;

/// Run the CLI with the given arguments.
pub fn run(db: &Path, command: &handlers::Commands) -> Result<()> {
    use handlers::*;
    
    match command {
        Commands::Build { path } => cmd_build(db, path),
        Commands::Update { path } => cmd_update(db, path),
        Commands::Watch { path, interval } => cmd_watch(db, path, *interval),
        Commands::Daemon { command } => cmd_daemon(db, command.clone()),
        Commands::Install {
            repo,
            force,
            agents,
            mcp,
        } => cmd_install(db, repo, *force, *agents, *mcp),
        Commands::Serve {
            host,
            port,
            bind,
            algorithm,
        } => {
            let bind = bind.clone().unwrap_or_else(|| format!("{}:{}", host, port));
            cmd_serve(db, &bind, algorithm)
        }
        Commands::Status => cmd_status(db),
        Commands::Paths {
            from,
            to,
            max_hops,
            top,
            structural_only,
        } => cmd_paths(db, from, to, *max_hops, *top, *structural_only),
        Commands::Callers { target } => cmd_callers(db, target),
        Commands::Callees { source } => cmd_callees(db, source),
        Commands::Impact {
            target,
            max_hops,
            top,
        } => cmd_impact(db, target, *max_hops, *top),
        Commands::DetectChanges {
            base,
            max_depth,
            brief,
        } => cmd_detect_changes(db, base, *max_depth, *brief),
        Commands::ReviewContext {
            base,
            max_lines_per_file,
            token_budget,
        } => cmd_review_context(db, base, *max_lines_per_file, *token_budget),
        Commands::Traverse {
            target,
            direction,
            max_depth,
            token_budget,
        } => cmd_traverse(db, target, direction, *max_depth, *token_budget),
        Commands::LargeFunctions { min_lines, top } => cmd_large_functions(db, *min_lines, *top),
        Commands::BridgeNodes { top } => cmd_bridge_nodes(db, *top),
        Commands::Cycles { top } => cmd_cycles(db, *top),
        Commands::Core { top } => cmd_core(db, *top),
        Commands::Articulation { top } => cmd_articulation(db, *top),
        Commands::Gaps { top } => cmd_gaps(db, *top),
        Commands::Diagnostics { top } => cmd_diagnostics(db, *top),
        Commands::Surprises { top } => cmd_surprises(db, *top),
        Commands::RebuildFts => cmd_rebuild_fts(db),
        Commands::Embed { model } => cmd_embed(db, model),
        Commands::Tui => cmd_tui(db),
        Commands::GraphDiff { base, head, top } => cmd_graph_diff(db, base, head, *top),
        Commands::Counterfactual {
            symbol,
            direction,
            max_depth,
        } => cmd_counterfactual(db, symbol, direction, *max_depth),
        Commands::Motifs { built_in, limit } => cmd_motifs(db, built_in, *limit),
        Commands::SuggestedQuestions { base, top } => cmd_suggested_questions(db, base, *top),
        Commands::Architecture { detail_level } => cmd_architecture(db, detail_level),
        Commands::Tool { operation, params } => cmd_tool(db, operation, params),
        Commands::Mcp => cmd_mcp(db),
        Commands::McpServer => cmd_mcp_server(db),
        Commands::GodNodes { top, seed } => cmd_god_nodes(db, *top, seed.as_deref()),
        Commands::Communities { top, algorithm } => cmd_communities(db, *top, algorithm),
        Commands::Flows { top } => cmd_flows(db, *top),
        Commands::AffectedFlows { base, top } => cmd_affected_flows(db, base, *top),
        Commands::Search { query } => cmd_search(db, query),
    }
}
