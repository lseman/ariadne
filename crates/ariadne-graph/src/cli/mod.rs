pub mod git;
/// CLI argument definitions and dispatch.
pub mod handlers;
pub mod helpers;
pub mod http;
pub mod mcp;
pub mod response;

// Extracted command implementations
mod advanced;
mod analysis;
mod basic;
mod build;
mod counterfactual;
mod daemon;
mod report;
mod search;
mod serve;
mod structure;
mod tool;
mod watch;

use anyhow::Result;
use handlers::Commands;
use std::path::Path;

/// Run the CLI with the given arguments.
pub fn run(db: &Path, command: &Commands) -> Result<()> {
    match command {
        Commands::Build { path } => build::cmd_build(db, path),
        Commands::Update { path } => build::cmd_update(db, path),
        Commands::Watch { path, interval } => watch::cmd_watch(db, path, *interval),
        Commands::Daemon { command } => daemon::cmd_daemon(db, command.clone()),
        Commands::Install {
            repo,
            force,
            agents,
            mcp,
        } => daemon::cmd_install(db, repo, *force, *agents, *mcp),
        Commands::Serve {
            host,
            port,
            bind,
            algorithm,
        } => {
            let bind = bind.clone().unwrap_or_else(|| format!("{}:{}", host, port));
            serve::cmd_serve(db, &bind, algorithm).map_err(|e| anyhow::anyhow!("serve: {}", e))
        }
        Commands::Status => basic::cmd_status(db),
        Commands::Paths {
            from,
            to,
            max_hops,
            top,
            structural_only,
        } => analysis::cmd_paths(db, from, to, *max_hops, *top, *structural_only),
        Commands::Callers { target } => analysis::cmd_callers(db, target),
        Commands::Callees { source } => analysis::cmd_callees(db, source),
        Commands::Impact {
            target,
            max_hops,
            top,
        } => analysis::cmd_impact(db, target, *max_hops, *top),
        Commands::DetectChanges {
            base,
            max_depth,
            brief,
        } => analysis::cmd_detect_changes(db, base, *max_depth, *brief),
        Commands::ReviewContext {
            base,
            max_lines_per_file,
            token_budget,
        } => analysis::cmd_review_context(db, base, *max_lines_per_file, *token_budget),
        Commands::Traverse {
            target,
            direction,
            max_depth,
            token_budget,
        } => analysis::cmd_traverse(db, target, direction, *max_depth, *token_budget),
        Commands::LargeFunctions { min_lines, top } => {
            structure::cmd_large_functions(db, *min_lines, *top)
        }
        Commands::BridgeNodes { top } => structure::cmd_bridge_nodes(db, *top),
        Commands::Cycles { top } => structure::cmd_cycles(db, *top),
        Commands::Core { top } => structure::cmd_core(db, *top),
        Commands::Articulation { top } => structure::cmd_articulation(db, *top),
        Commands::Gaps { top } => structure::cmd_gaps(db, *top),
        Commands::Diagnostics { top } => structure::cmd_diagnostics(db, *top),
        Commands::Surprises { top } => structure::cmd_surprises(db, *top),
        Commands::SuggestedQuestions { base, top } => {
            structure::cmd_suggested_questions(db, base, *top)
        }
        Commands::Architecture { detail_level } => structure::cmd_architecture(db, detail_level),
        Commands::RebuildFts => basic::cmd_rebuild_fts(db),
        Commands::Embed { model } => basic::cmd_embed(db, model),
        Commands::Tui => basic::cmd_tui(db),
        Commands::GraphDiff { base, head, top } => basic::cmd_graph_diff(db, base, head, *top),
        Commands::Counterfactual {
            symbol,
            direction,
            max_depth,
        } => counterfactual::cmd_counterfactual(db, symbol, direction, *max_depth),
        Commands::Motifs { built_in, limit } => analysis::cmd_motifs(db, built_in, *limit),
        Commands::Tool { operation, params } => tool::cmd_tool(db, operation, params),
        Commands::Mcp => tool::cmd_mcp(db),
        Commands::McpServer => tool::cmd_mcp_server(db),
        Commands::GodNodes { top, seed } => advanced::cmd_god_nodes(db, *top, seed.as_deref()),
        Commands::Communities { top, algorithm } => advanced::cmd_communities(db, *top, algorithm),
        Commands::Dedup {
            threshold,
            community_boost,
            community_algo,
        } => advanced::cmd_dedup(db, *threshold, *community_boost, community_algo.clone()),
        Commands::Flows { top } => advanced::cmd_flows(db, *top),
        Commands::AffectedFlows { base, top } => advanced::cmd_affected_flows(db, base, *top),
        Commands::BlastRadius {
            base,
            max_depth,
            top,
        } => advanced::cmd_blast_radius(db, base, *max_depth, *top),
        Commands::TestCoverage { base, target } => {
            advanced::cmd_test_coverage(db, base.as_deref(), target.as_deref())
        }
        Commands::Report { output, top } => report::cmd_report(db, output, *top),
        Commands::Search { query } => search::cmd_search(db, query),
    }
}
