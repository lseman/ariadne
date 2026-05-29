use anyhow::{bail, Result};
use ariadne_graph::extract::{
    compute_flows, derive_tested_by_edges, extract_directory, extract_file, ignore_set,
    is_supported, resolve_call_placeholders,
};
use ariadne_graph::query::{
    analyze_impact, articulation_points, bridge_scores, callees_of, callers_of, community_quality,
    core_numbers, cyclic_components, find_top_paths, fts_ranked_search, is_active_at, leiden,
    leiden_with_options, louvain, louvain_with_options, pagerank, paths::PathQuery,
    personalized_pagerank, ranked_search, search_by_name, CommunityObjective, CommunityOptions,
    ImpactQuery,
};
use ariadne_graph::store::{
    edge_identity, Store, StoredEdgeRow, StoredNodeRow, DEFAULT_EMBEDDING_MODEL,
};
use ariadne_graph::{Graph, NodeId, NodeKind};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(
    name = "ariadne",
    about = "A graph-based semantic system for code, documents, and diagrams.",
    version
)]
struct Cli {
    /// Path to the SQLite database file.
    #[arg(short, long, default_value = "ariadne.db", global = true)]
    db: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the graph from a directory of source files.
    Build { path: PathBuf },
    /// Incrementally update the graph from changed files.
    Update { path: PathBuf },
    /// Watch a path and incrementally update when supported files change.
    Watch {
        path: PathBuf,
        #[arg(long, default_value_t = 2)]
        interval: u64,
    },
    /// Manage registered repositories for continuous updates.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    /// Install auto-update git hooks for this repository.
    Install {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        agents: bool,
        #[arg(long)]
        mcp: bool,
    },
    /// Serve an interactive D3 graph explorer.
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Full bind address, overriding --host and --port.
        #[arg(long)]
        bind: Option<String>,
        /// Community algorithm used for colors: louvain or leiden.
        #[arg(long, default_value = "leiden")]
        algorithm: String,
    },
    /// Show graph statistics.
    Status,
    /// Report graph health, index coverage, and noise signals as JSON.
    Diagnostics {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Find paths between two symbols.
    Paths {
        from: String,
        to: String,
        #[arg(long, default_value_t = 5)]
        max_hops: usize,
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Only follow structural (confidence == 1.0) edges.
        #[arg(long)]
        structural_only: bool,
    },
    /// Find callers of a function.
    Callers { target: String },
    /// Find callees of a function.
    Callees { source: String },
    /// Rank symbols, files, and docs likely affected by a target.
    Impact {
        target: String,
        #[arg(long, default_value_t = 4)]
        max_hops: usize,
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Risk-scored change analysis from a git diff base.
    DetectChanges {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 2)]
        max_depth: usize,
        #[arg(long)]
        brief: bool,
    },
    /// Summarize changed and impacted functions, classes, and files.
    BlastRadius {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 2)]
        max_depth: usize,
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Token-budgeted review context for changed and impacted files.
    ReviewContext {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 200)]
        max_lines_per_file: usize,
        #[arg(long, default_value_t = 1600)]
        token_budget: usize,
    },
    /// Traverse graph relationships from a target with a token budget.
    Traverse {
        target: String,
        #[arg(long, default_value = "both")]
        direction: String,
        #[arg(long, default_value_t = 3)]
        max_depth: usize,
        #[arg(long, default_value_t = 1200)]
        token_budget: usize,
    },
    /// Find large functions/classes by source span.
    LargeFunctions {
        #[arg(long, default_value_t = 80)]
        min_lines: u32,
        #[arg(long, default_value_t = 50)]
        top: usize,
    },
    /// Find bridge/chokepoint nodes.
    BridgeNodes {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Find dependency cycles via strongly connected components.
    Cycles {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Rank nodes by k-core/coreness.
    Core {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Find articulation points whose removal disconnects graph regions.
    Articulation {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Identify structural weaknesses and likely review blind spots.
    Gaps {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Generate prioritized review questions from graph analysis.
    SuggestedQuestions {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Compare naive source-reading tokens to graph-query tokens.
    TokenBenchmark {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 1600)]
        token_budget: usize,
        #[arg(long, default_value_t = 200)]
        max_lines_per_file: usize,
    },
    /// Rank unexpected cross-community, cross-language, and hub-coupling edges.
    Surprises {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Summarize communities, bridges, and coupling at architecture level.
    Architecture {
        #[arg(long, default_value = "standard")]
        detail_level: String,
    },
    /// Export the graph as graphml, cypher, or obsidian markdown.
    Export { format: String, output: PathBuf },
    /// Generate a markdown wiki from community structure.
    Wiki {
        output: PathBuf,
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Generate a review-ready markdown graph report.
    Report {
        output: PathBuf,
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// One-operation JSON interface for agents and MCP wrappers.
    Tool {
        operation: String,
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// JSON-lines one-tool loop for MCP adapters and editor wrappers.
    Mcp,
    /// Real stdio MCP server exposing Ariadne as one tool.
    McpServer,
    /// Top-ranked nodes by PageRank.
    GodNodes {
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Bias PageRank around a symbol or file.
        #[arg(long)]
        seed: Option<String>,
    },
    /// Detect communities with Louvain or Leiden-style refinement.
    Communities {
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Community algorithm: louvain or leiden.
        #[arg(long, default_value = "louvain")]
        algorithm: String,
        /// Modularity resolution; higher values produce smaller communities.
        #[arg(long, default_value_t = 1.0)]
        resolution: f32,
        /// Leiden well-connectedness strictness; 0 disables the gate.
        #[arg(long, default_value_t = 1.0)]
        well_connectedness: f32,
        /// Maximum local-move sweeps per hierarchy level.
        #[arg(long, default_value_t = 50)]
        max_passes: usize,
        /// Maximum aggregation levels.
        #[arg(long, default_value_t = 10)]
        max_levels: usize,
        /// Disable parallel Leiden refinement for deterministic debugging.
        #[arg(long)]
        no_parallel: bool,
        /// Community objective used for scoring: modularity or cpm.
        #[arg(long, default_value = "modularity")]
        objective: String,
    },
    /// List execution flows ranked by criticality.
    Flows {
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Show flows touched by changes since `base`.
    AffectedFlows {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Report direct and nearby test coverage for a symbol or recent changes.
    TestCoverage {
        /// Specific symbol to inspect. If omitted, inspect changed callables since `--base`.
        target: Option<String>,
        #[arg(long, default_value = "HEAD~1")]
        base: String,
    },
    /// Diff two graph snapshots using temporal validity windows.
    GraphDiff {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long, default_value_t = 50)]
        top: usize,
    },
    /// Build optional local embeddings for semantic search.
    Embed {
        #[arg(long, default_value = DEFAULT_EMBEDDING_MODEL)]
        model: String,
    },
    /// Rebuild the SQLite FTS5 node search index.
    RebuildFts,
    /// Search nodes by name.
    Search { query: String },
    /// Launch the interactive terminal UI.
    Tui,
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Register a repository path.
    Add {
        path: PathBuf,
        #[arg(long)]
        alias: Option<String>,
    },
    /// Start polling all registered repositories.
    Start {
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Show registered repositories.
    Status,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Build { path } => cmd_build(&cli.db, &path),
        Commands::Update { path } => cmd_update(&cli.db, &path),
        Commands::Watch { path, interval } => cmd_watch(&cli.db, &path, interval),
        Commands::Daemon { command } => cmd_daemon(&cli.db, command),
        Commands::Install {
            repo,
            force,
            agents,
            mcp,
        } => cmd_install(&cli.db, &repo, force, agents, mcp),
        Commands::Serve {
            host,
            port,
            bind,
            algorithm,
        } => {
            let bind = bind.unwrap_or_else(|| format!("{}:{}", host, port));
            cmd_serve(&cli.db, &bind, &algorithm)
        }
        Commands::Status => cmd_status(&cli.db),
        Commands::Diagnostics { top } => cmd_diagnostics(&cli.db, top),
        Commands::Paths {
            from,
            to,
            max_hops,
            top,
            structural_only,
        } => cmd_paths(&cli.db, &from, &to, max_hops, top, structural_only),
        Commands::Callers { target } => cmd_callers(&cli.db, &target),
        Commands::Callees { source } => cmd_callees(&cli.db, &source),
        Commands::Impact {
            target,
            max_hops,
            top,
        } => cmd_impact(&cli.db, &target, max_hops, top),
        Commands::DetectChanges {
            base,
            max_depth,
            brief,
        } => cmd_detect_changes(&cli.db, &base, max_depth, brief),
        Commands::BlastRadius {
            base,
            max_depth,
            top,
        } => cmd_blast_radius(&cli.db, &base, max_depth, top),
        Commands::ReviewContext {
            base,
            max_lines_per_file,
            token_budget,
        } => cmd_review_context(&cli.db, &base, max_lines_per_file, token_budget),
        Commands::Traverse {
            target,
            direction,
            max_depth,
            token_budget,
        } => cmd_traverse(&cli.db, &target, &direction, max_depth, token_budget),
        Commands::LargeFunctions { min_lines, top } => cmd_large_functions(&cli.db, min_lines, top),
        Commands::BridgeNodes { top } => cmd_bridge_nodes(&cli.db, top),
        Commands::Cycles { top } => cmd_cycles(&cli.db, top),
        Commands::Core { top } => cmd_core(&cli.db, top),
        Commands::Articulation { top } => cmd_articulation(&cli.db, top),
        Commands::Gaps { top } => cmd_gaps(&cli.db, top),
        Commands::SuggestedQuestions { base, top } => cmd_suggested_questions(&cli.db, &base, top),
        Commands::TokenBenchmark {
            base,
            token_budget,
            max_lines_per_file,
        } => cmd_token_benchmark(&cli.db, &base, token_budget, max_lines_per_file),
        Commands::Surprises { top } => cmd_surprises(&cli.db, top),
        Commands::Architecture { detail_level } => cmd_architecture(&cli.db, &detail_level),
        Commands::Export { format, output } => cmd_export(&cli.db, &format, &output),
        Commands::Wiki { output, top } => cmd_wiki(&cli.db, &output, top),
        Commands::Report { output, top } => cmd_report(&cli.db, &output, top),
        Commands::Tool { operation, params } => cmd_tool(&cli.db, &operation, &params),
        Commands::Mcp => cmd_mcp(&cli.db),
        Commands::McpServer => cmd_mcp_server(&cli.db),
        Commands::GodNodes { top, seed } => cmd_god_nodes(&cli.db, top, seed.as_deref()),
        Commands::Communities {
            top,
            algorithm,
            resolution,
            well_connectedness,
            max_passes,
            max_levels,
            no_parallel,
            objective,
        } => cmd_communities(
            &cli.db,
            top,
            &algorithm,
            community_options(
                resolution,
                well_connectedness,
                max_passes,
                max_levels,
                !no_parallel,
                parse_community_objective(&objective)?,
            ),
        ),
        Commands::Flows { top } => cmd_flows(&cli.db, top),
        Commands::AffectedFlows { base, top } => cmd_affected_flows(&cli.db, &base, top),
        Commands::TestCoverage { target, base } => {
            cmd_test_coverage(&cli.db, target.as_deref(), &base)
        }
        Commands::GraphDiff { base, head, top } => cmd_graph_diff(&cli.db, &base, &head, top),
        Commands::Embed { model } => cmd_embed(&cli.db, &model),
        Commands::RebuildFts => cmd_rebuild_fts(&cli.db),
        Commands::Search { query } => cmd_search(&cli.db, &query),
        Commands::Tui => cmd_tui(&cli.db),
    }
}

fn cmd_build(db: &Path, path: &Path) -> Result<()> {
    let mut graph = Graph::new();
    tracing::info!("extracting from {}", path.display());
    let n = extract_directory(path, &mut graph)?;
    if let Some(head) = git_commit_hash("HEAD")? {
        stamp_missing_validity(&mut graph, &head);
    }
    tracing::info!(
        "extracted {} files: {} nodes, {} edges",
        n,
        graph.node_count(),
        graph.edge_count()
    );
    let mut store = Store::open(db)?;
    store.reset_all()?;
    store.save(&graph)?;
    let hashes = collect_file_hashes(path)?;
    store.set_file_hashes(&hashes)?;
    println!(
        "graph built: {} nodes, {} edges -> {}",
        graph.node_count(),
        graph.edge_count(),
        db.display()
    );
    Ok(())
}

fn cmd_update(db: &Path, path: &Path) -> Result<()> {
    let current = collect_file_hashes(path)?;
    let current_map: HashMap<String, String> = current.iter().cloned().collect();
    let mut store = Store::open(db)?;
    let previous = store.file_hashes()?;

    let changed: Vec<String> = current
        .iter()
        .filter(|(p, h)| previous.get(p) != Some(h))
        .map(|(p, _)| p.clone())
        .collect();
    let deleted: Vec<String> = previous
        .keys()
        .filter(|p| !current_map.contains_key(*p))
        .cloned()
        .collect();

    if changed.is_empty() && deleted.is_empty() {
        println!("graph already up to date");
        return Ok(());
    }

    let mut stale = changed.clone();
    stale.extend(deleted.iter().cloned());

    let current_commit = git_commit_hash("HEAD")?;
    let previous_nodes = if current_commit.is_some() {
        store.active_nodes_for_sources(&stale)?
    } else {
        Vec::new()
    };
    let previous_edges = if current_commit.is_some() {
        store.active_edges_for_sources(&stale)?
    } else {
        Vec::new()
    };
    store.delete_sources(&stale)?;

    let mut graph = store.load()?;
    for source in &changed {
        let file = Path::new(source);
        if file.exists() {
            extract_file(file, &mut graph)?;
        }
    }
    resolve_call_placeholders(&mut graph);
    derive_tested_by_edges(&mut graph);
    compute_flows(&mut graph);

    if let Some(commit) = current_commit.as_deref() {
        let previous_changed_nodes = previous_nodes_for_deleted_sources(&previous_nodes, &changed);
        let previous_changed_edges = previous_edges_for_deleted_sources(&previous_edges, &changed);
        apply_temporal_incremental_validity(
            &mut graph,
            &changed,
            &previous_changed_nodes,
            &previous_changed_edges,
            commit,
        );
        let current_node_keys = current_node_keys_for_sources(&graph, &changed);
        let current_edge_keys = current_edge_keys_for_sources(&graph, &changed);
        let removed_nodes = removed_nodes_for_archive(&previous_changed_nodes, &current_node_keys);
        let removed_edges = removed_edges_for_archive(&previous_changed_edges, &current_edge_keys);
        let deleted_nodes = previous_nodes_for_deleted_sources(&previous_nodes, &deleted);
        let deleted_edges = previous_edges_for_deleted_sources(&previous_edges, &deleted);
        store.archive_nodes(&removed_nodes, commit)?;
        store.archive_nodes(&deleted_nodes, commit)?;
        store.archive_edges(&removed_edges, commit)?;
        store.archive_edges(&deleted_edges, commit)?;
    }

    store.save(&graph)?;
    store.set_file_hashes(&current)?;

    println!(
        "graph updated: {} changed, {} deleted, {} nodes, {} edges",
        changed.len(),
        deleted.len(),
        graph.node_count(),
        graph.edge_count()
    );
    Ok(())
}

fn cmd_watch(db: &Path, path: &Path, interval: u64) -> Result<()> {
    println!(
        "watching {} for Ariadne graph updates every {}s",
        path.display(),
        interval
    );
    loop {
        cmd_update(db, path)?;
        thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

fn cmd_daemon(db: &Path, command: DaemonCommands) -> Result<()> {
    match command {
        DaemonCommands::Add { path, alias } => {
            let mut repos = load_daemon_repos()?;
            let path = absolute_path(&path)?;
            let alias = alias.unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("repo")
                    .to_string()
            });
            repos.push(json!({ "alias": alias, "path": path }));
            save_daemon_repos(&repos)?;
            println!("registered {}", path.display());
            Ok(())
        }
        DaemonCommands::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "repos": load_daemon_repos()? }))?
            );
            Ok(())
        }
        DaemonCommands::Start { interval } => {
            let repos = load_daemon_repos()?;
            if repos.is_empty() {
                bail!("no repositories registered; run ariadne daemon add <path>");
            }
            println!(
                "Ariadne daemon polling {} repos every {}s",
                repos.len(),
                interval
            );
            loop {
                for repo in &repos {
                    if let Some(path) = repo["path"].as_str() {
                        if let Err(e) = cmd_update(db, Path::new(path)) {
                            tracing::warn!("daemon update failed for {}: {}", path, e);
                        }
                    }
                }
                thread::sleep(Duration::from_secs(interval.max(1)));
            }
        }
    }
}

fn cmd_install(db: &Path, repo: &Path, force: bool, agents: bool, mcp: bool) -> Result<()> {
    let git_dir = repo.join(".git");
    if !git_dir.is_dir() {
        bail!("{} is not a git repository", repo.display());
    }
    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;
    let exe = std::env::current_exe()?;
    let db = absolute_path(db)?;
    let root = absolute_path(repo)?;

    let pre_commit = format!(
        r#"#!/bin/sh
"{}" --db "{}" detect-changes --base HEAD --brief > "{}" 2>/dev/null || true
exit 0
"#,
        exe.display(),
        db.display(),
        hooks_dir.join("ariadne-pre-commit.json").display()
    );
    write_git_hook(&hooks_dir.join("pre-commit"), &pre_commit, force)?;

    for hook in ["post-commit", "post-merge", "post-checkout"] {
        let script = format!(
            "#!/bin/sh\n\"{}\" --db \"{}\" update \"{}\" >/dev/null 2>&1 || true\n",
            exe.display(),
            db.display(),
            root.display()
        );
        write_git_hook(&hooks_dir.join(hook), &script, force)?;
    }

    println!("installed Ariadne git hooks in {}", hooks_dir.display());
    if agents {
        install_agents_md(repo, &db)?;
    }
    if mcp {
        install_mcp_config(repo, &db)?;
    }
    Ok(())
}

fn write_git_hook(path: &Path, script: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; rerun with --force to replace it",
            path.display()
        );
    }
    fs::write(path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn cmd_serve(db: &Path, bind: &str, algorithm: &str) -> Result<()> {
    let listener = TcpListener::bind(bind)?;
    println!("Ariadne graph explorer listening on http://{}", bind);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_http(stream, db, algorithm) {
                    tracing::warn!("serve request failed: {}", e);
                }
            }
            Err(e) => tracing::warn!("serve connection failed: {}", e),
        }
    }
    Ok(())
}

fn cmd_status(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let (n, e) = store.stats()?;
    let fts_count = store.fts_stats()?;
    let (embedding_count, embedding_model) = store.embedding_stats()?;
    println!("ariadne db: {}", db.display());
    println!("  nodes: {}", n);
    println!("  edges: {}", e);
    println!("  fts5 indexed nodes: {}", fts_count);
    println!(
        "  embeddings: {}{}",
        embedding_count,
        embedding_model
            .as_deref()
            .map(|model| format!(" ({})", model))
            .unwrap_or_default()
    );
    Ok(())
}

fn cmd_diagnostics(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let fts_count = store.fts_stats()?;
    let (embedding_count, embedding_model) = store.embedding_stats()?;
    let report = diagnostics_json(&graph, fts_count, embedding_count, embedding_model, top);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_paths(
    db: &Path,
    from: &str,
    to: &str,
    max_hops: usize,
    top: usize,
    structural_only: bool,
) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let from_id = resolve(&graph, from)?;
    let to_id = resolve(&graph, to)?;
    let mut q = PathQuery::between(from_id, to_id, max_hops);
    if structural_only {
        q = q.with_min_confidence(1.0);
    }
    let paths = find_top_paths(&graph, &q, top);
    if paths.is_empty() {
        println!("no paths found");
        return Ok(());
    }
    println!("found {} ranked path(s):", paths.len());
    for (i, p) in paths.iter().enumerate() {
        println!("  path {} (cost {:.3}):", i + 1, p.cost);
        for n in &p.nodes {
            if let Some(node) = graph.node(*n) {
                println!("    -> {} ({:?})", node.qualified_name, node.kind);
            }
        }
    }
    Ok(())
}

fn cmd_callers(db: &Path, target: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = resolve(&graph, target)?;
    let callers = callers_of(&graph, id);
    println!("callers of {} ({} total):", target, callers.len());
    for c in callers {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

fn cmd_callees(db: &Path, source: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = resolve(&graph, source)?;
    let callees = callees_of(&graph, id);
    println!("callees of {} ({} total):", source, callees.len());
    for c in callees {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

fn cmd_impact(db: &Path, target: &str, max_hops: usize, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = resolve(&graph, target)?;
    let hits = analyze_impact(
        &graph,
        ImpactQuery {
            seed,
            max_hops,
            limit: top,
        },
    );
    println!("impact of {} ({} result(s)):", target, hits.len());
    for hit in hits {
        if let Some(n) = graph.node(hit.id) {
            let via: Vec<_> = hit.via.iter().map(|kind| format!("{:?}", kind)).collect();
            println!(
                "  {:.4}  hops={}  {}  ({:?})  [{}]",
                hit.score,
                hit.distance,
                n.qualified_name,
                n.kind,
                via.join(" <- ")
            );
        }
    }
    Ok(())
}

fn cmd_detect_changes(db: &Path, base: &str, max_depth: usize, brief: bool) -> Result<()> {
    let analysis = detect_changes_json(db, base, max_depth)?;
    if brief {
        println!("{}", serde_json::to_string_pretty(&analysis)?);
        return Ok(());
    }
    println!(
        "change risk: {} ({:.2})",
        analysis["risk"], analysis["risk_score"]
    );
    println!("changed files:");
    for file in analysis["changed_files"].as_array().unwrap_or(&Vec::new()) {
        println!("  {}", file.as_str().unwrap_or(""));
    }
    if let Some(nodes) = analysis["changed_symbols"].as_array() {
        if !nodes.is_empty() {
            println!("changed symbols:");
            for node in nodes.iter().take(12) {
                let loc = match (
                    node["line_start"].as_u64(),
                    node["line_end"].as_u64(),
                    node["source_uri"].as_str(),
                ) {
                    (Some(start), Some(end), Some(src)) => format!("{}:{}-{}", src, start, end),
                    (_, _, Some(src)) => src.to_string(),
                    _ => String::new(),
                };
                println!(
                    "  {}  {}",
                    node["qualified_name"].as_str().unwrap_or(""),
                    loc
                );
            }
        }
    }
    println!("top impacted:");
    for hit in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {:.3}  {}",
            hit["score"].as_f64().unwrap_or_default(),
            hit["qualified_name"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn cmd_blast_radius(db: &Path, base: &str, max_depth: usize, top: usize) -> Result<()> {
    let radius = blast_radius_json(db, base, max_depth, top)?;
    println!("{}", serde_json::to_string_pretty(&radius)?);
    Ok(())
}

fn cmd_review_context(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<()> {
    let context = review_context_json(db, base, max_lines_per_file, token_budget)?;
    println!("{}", serde_json::to_string_pretty(&context)?);
    Ok(())
}

fn cmd_traverse(
    db: &Path,
    target: &str,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = resolve(&graph, target)?;
    let out = traverse_json(&graph, seed, direction, max_depth, token_budget);
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn cmd_large_functions(db: &Path, min_lines: u32, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&large_functions_json(&graph, min_lines, top))?
    );
    Ok(())
}

fn cmd_bridge_nodes(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&bridge_nodes_json(&graph, top))?
    );
    Ok(())
}

fn cmd_cycles(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&cycles_json(&graph, top))?
    );
    Ok(())
}

fn cmd_core(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!("{}", serde_json::to_string_pretty(&core_json(&graph, top))?);
    Ok(())
}

fn cmd_articulation(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&articulation_json(&graph, top))?
    );
    Ok(())
}

fn cmd_gaps(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!("{}", serde_json::to_string_pretty(&gaps_json(&graph, top))?);
    Ok(())
}

fn cmd_suggested_questions(db: &Path, base: &str, top: usize) -> Result<()> {
    let analysis = detect_changes_json(db, base, 2)?;
    let questions = suggested_questions_json(&analysis, top);
    println!("{}", serde_json::to_string_pretty(&questions)?);
    Ok(())
}

fn cmd_token_benchmark(
    db: &Path,
    base: &str,
    token_budget: usize,
    max_lines_per_file: usize,
) -> Result<()> {
    let out = token_benchmark_json(db, base, token_budget, max_lines_per_file)?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn cmd_surprises(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&surprises_json(&graph, top))?
    );
    Ok(())
}

fn cmd_architecture(db: &Path, detail_level: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let detail = DetailLevel::parse(detail_level);
    println!(
        "{}",
        serde_json::to_string_pretty(&architecture_overview_json(&graph, detail))?
    );
    Ok(())
}

fn cmd_export(db: &Path, format: &str, output: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let report = export_graph(&graph, format, output)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_wiki(db: &Path, output: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let report = write_wiki(&graph, output, top)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_report(db: &Path, output: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let fts_count = store.fts_stats()?;
    let (embedding_count, embedding_model) = store.embedding_stats()?;
    let markdown = graph_report_markdown(&graph, fts_count, embedding_count, embedding_model, top);
    fs::write(output, markdown)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "operation": "report",
            "output": output,
        }))?
    );
    Ok(())
}

fn cmd_tool(db: &Path, operation: &str, params: &str) -> Result<()> {
    let params: Value = serde_json::from_str(params)?;
    let response = tool_response(db, operation, &params)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn tool_response(db: &Path, operation: &str, params: &Value) -> Result<Value> {
    let mut store = Store::open(db)?;
    let graph = store.load()?;
    let detail = DetailLevel::from_params(&params);
    let response = match operation {
        "minimal_context" | "context" => {
            let target = params.get("target").and_then(Value::as_str);
            let mode = params
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("review");
            compact_for_detail(minimal_context_json(&graph, target, mode), detail)
        }
        "status" => {
            let (nodes, edges) = store.stats()?;
            let fts_count = store.fts_stats()?;
            let (embedding_count, embedding_model) = store.embedding_stats()?;
            json!({
                "operation": operation,
                "nodes": nodes,
                "edges": edges,
                "fts5": {
                    "indexed_nodes": fts_count,
                },
                "embeddings": {
                    "count": embedding_count,
                    "model": embedding_model,
                }
            })
        }
        "diagnostics" | "health" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            let fts_count = store.fts_stats()?;
            let (embedding_count, embedding_model) = store.embedding_stats()?;
            compact_for_detail(
                diagnostics_json(&graph, fts_count, embedding_count, embedding_model, limit),
                detail,
            )
        }
        "search" => {
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let hits: Vec<_> = fts_ranked_search(&store, &graph, query, limit)
                .into_iter()
                .filter_map(|hit| {
                    graph.node(hit.id).map(|n| {
                        json!({
                            "id": hit.id.0,
                            "score": hit.score,
                            "name": n.name,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                            "source_uri": n.source_uri,
                            "signals": hit.signals,
                        })
                    })
                })
                .collect();
            compact_for_detail(json!({ "operation": operation, "hits": hits }), detail)
        }
        "rebuild_fts" | "rebuild_fts_index" => {
            let indexed_nodes = store.rebuild_fts_index()?;
            json!({
                "operation": operation,
                "fts5": {
                    "indexed_nodes": indexed_nodes,
                }
            })
        }
        "paths" => {
            let from = required_str(&params, "from")?;
            let to = required_str(&params, "to")?;
            let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(5) as usize;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let from_id = resolve(&graph, from)?;
            let to_id = resolve(&graph, to)?;
            let paths: Vec<_> =
                find_top_paths(&graph, &PathQuery::between(from_id, to_id, max_hops), limit)
                    .into_iter()
                    .map(|path| {
                        let nodes: Vec<_> = path
                            .nodes
                            .into_iter()
                            .filter_map(|id| {
                                graph.node(id).map(|n| {
                                    json!({
                                        "id": id.0,
                                        "qualified_name": n.qualified_name,
                                        "kind": n.kind,
                                    })
                                })
                            })
                            .collect();
                        json!({ "cost": path.cost, "nodes": nodes })
                    })
                    .collect();
            compact_for_detail(json!({ "operation": operation, "paths": paths }), detail)
        }
        "impact" => {
            let target = required_str(&params, "target")?;
            let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(4) as usize;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            let seed = resolve(&graph, target)?;
            let hits: Vec<_> = analyze_impact(
                &graph,
                ImpactQuery {
                    seed,
                    max_hops,
                    limit,
                },
            )
            .into_iter()
            .filter_map(|hit| {
                graph.node(hit.id).map(|n| {
                    json!({
                        "id": hit.id.0,
                        "score": hit.score,
                        "distance": hit.distance,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                        "source_uri": n.source_uri,
                        "via": hit.via,
                    })
                })
            })
            .collect();
            compact_for_detail(
                json!({ "operation": operation, "target": target, "hits": hits }),
                detail,
            )
        }
        "detect_changes" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(2) as usize;
            compact_for_detail(detect_changes_json(db, base, max_depth)?, detail)
        }
        "token_benchmark" | "benchmark_tokens" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1600) as usize;
            let max_lines_per_file = params
                .get("max_lines_per_file")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            compact_for_detail(
                token_benchmark_json(db, base, token_budget, max_lines_per_file)?,
                detail,
            )
        }
        "blast_radius" | "impact_radius" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(2) as usize;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(blast_radius_json(db, base, max_depth, limit)?, detail)
        }
        "review_context" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_lines_per_file = params
                .get("max_lines_per_file")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1600) as usize;
            compact_for_detail(
                review_context_json(db, base, max_lines_per_file, token_budget)?,
                detail,
            )
        }
        "traverse" => {
            let target = required_str(&params, "target")?;
            let direction = params
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("both");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1200) as usize;
            let seed = resolve(&graph, target)?;
            compact_for_detail(
                traverse_json(&graph, seed, direction, max_depth, token_budget),
                detail,
            )
        }
        "large_functions" => {
            let min_lines = params
                .get("min_lines")
                .and_then(Value::as_u64)
                .unwrap_or(80) as u32;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(large_functions_json(&graph, min_lines, limit), detail)
        }
        "bridge_nodes" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(bridge_nodes_json(&graph, limit), detail)
        }
        "cycles" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(cycles_json(&graph, limit), detail)
        }
        "core" | "k_core" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(core_json(&graph, limit), detail)
        }
        "articulation" | "articulation_points" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(articulation_json(&graph, limit), detail)
        }
        "gaps" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(gaps_json(&graph, limit), detail)
        }
        "surprises" | "surprise_scoring" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(surprises_json(&graph, limit), detail)
        }
        "suggested_questions" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let analysis = detect_changes_json(db, base, 2)?;
            compact_for_detail(suggested_questions_json(&analysis, limit), detail)
        }
        "communities" => {
            let algorithm = params
                .get("algorithm")
                .and_then(Value::as_str)
                .unwrap_or("leiden");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let options = community_options_from_params(&params);
            compact_for_detail(communities_json(&graph, algorithm, options, limit)?, detail)
        }
        "architecture_overview" | "architecture" => architecture_overview_json(&graph, detail),
        "export" => {
            let format = required_str(&params, "format")?;
            let output = required_str(&params, "output")?;
            export_graph(&graph, format, Path::new(output))?
        }
        "wiki" => {
            let output = required_str(&params, "output")?;
            let top = params.get("top").and_then(Value::as_u64).unwrap_or(25) as usize;
            write_wiki(&graph, Path::new(output), top)?
        }
        "report" | "graph_report" => {
            let output = required_str(&params, "output")?;
            let top = params.get("top").and_then(Value::as_u64).unwrap_or(10) as usize;
            let fts_count = store.fts_stats()?;
            let (embedding_count, embedding_model) = store.embedding_stats()?;
            let markdown =
                graph_report_markdown(&graph, fts_count, embedding_count, embedding_model, top);
            fs::write(output, markdown)?;
            json!({ "operation": operation, "output": output })
        }
        "god_nodes" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let ranks = if let Some(seed) = params.get("seed").and_then(Value::as_str) {
                let seed_id = resolve(&graph, seed)?;
                personalized_pagerank(&graph, &[(seed_id, 1.0)], 0.85, 50)
            } else {
                pagerank(&graph, 0.85, 50)
            };
            let mut sorted: Vec<_> = ranks.into_iter().collect();
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let hits: Vec<_> = sorted
                .into_iter()
                .filter_map(|(id, score)| {
                    let n = graph.node(id)?;
                    if !is_rankable_node(n) {
                        return None;
                    }
                    Some(json!({
                        "id": id.0,
                        "score": score,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                    }))
                })
                .take(limit)
                .collect();
            compact_for_detail(json!({ "operation": operation, "hits": hits }), detail)
        }
        "flows" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            let ids = ariadne_graph::extract::flows::all_flows(&graph);
            let total = ids.len();
            let hits: Vec<Value> = ids
                .into_iter()
                .take(limit)
                .filter_map(|id| {
                    let node = graph.node(id)?;
                    Some(json!({
                        "id": id.0,
                        "qualified_name": node.qualified_name,
                        "entry_qualified_name": node.properties.get("entry_qualified_name"),
                        "entry_name": node.properties.get("entry_name"),
                        "criticality": node.properties.get("criticality"),
                        "node_count": node.properties.get("node_count"),
                        "depth": node.properties.get("depth"),
                        "is_test_flow": node.properties.get("is_test_flow"),
                    }))
                })
                .collect();
            compact_for_detail(
                json!({
                    "operation": operation,
                    "hits": hits,
                    "total": total,
                    "truncated": total > limit,
                }),
                detail,
            )
        }
        "affected_flows" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            // Reuse detect_changes to get the changed set, then pull its
            // affected_flows field — keeps risk-score consistent.
            let analysis = detect_changes_json(db, base, 2)?;
            let payload = analysis
                .get("affected_flows")
                .cloned()
                .unwrap_or_else(|| json!({"hits": [], "total": 0, "truncated": false}));
            let truncated_hits: Vec<Value> = payload["hits"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(limit)
                .collect();
            compact_for_detail(
                json!({
                    "operation": operation,
                    "base": base,
                    "hits": truncated_hits,
                    "total": payload["total"],
                }),
                detail,
            )
        }
        "test_coverage" => {
            if let Some(target) = params.get("target").and_then(Value::as_str) {
                let id = resolve(&graph, target)?;
                compact_for_detail(
                    json!({
                        "operation": operation,
                        "target": target,
                        "coverage": test_coverage_json(&graph, &[id]),
                    }),
                    detail,
                )
            } else {
                let base = params
                    .get("base")
                    .and_then(Value::as_str)
                    .unwrap_or("HEAD~1");
                let analysis = detect_changes_json(db, base, 2)?;
                compact_for_detail(
                    json!({
                        "operation": operation,
                        "base": base,
                        "coverage": analysis["test_coverage"].clone(),
                    }),
                    detail,
                )
            }
        }
        "graph_diff" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let head = params.get("head").and_then(Value::as_str).unwrap_or("HEAD");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(graph_diff_json(&store, &graph, base, head, limit)?, detail)
        }
        "embed_graph" => {
            let model = params
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_EMBEDDING_MODEL);
            let count = store.rebuild_embeddings(model)?;
            json!({
                "operation": operation,
                "count": count,
                "model": model,
            })
        }
        other => bail!("unknown tool operation {}", other),
    };
    Ok(apply_response_guardrails(response, &graph, params, detail))
}

fn cmd_mcp(db: &Path) -> Result<()> {
    eprintln!(
        "Ariadne MCP-style JSON loop ready. Send {{\"operation\":\"search\",\"params\":{{...}}}}."
    );
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)?;
        let operation = required_str(&request, "operation")?;
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        match tool_response(db, operation, &params) {
            Ok(response) => println!("{}", serde_json::to_string_pretty(&response)?),
            Err(e) => println!(
                "{}",
                json!({ "operation": operation, "error": e.to_string() })
            ),
        };
    }
    Ok(())
}

fn cmd_mcp_server(db: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let request: Value = serde_json::from_str(&message.body)?;
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();

        if method == "notifications/initialized" {
            continue;
        }

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "ariadne", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [ariadne_mcp_tool_schema()] }
            }),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name != "ariadne" {
                    mcp_error(id, -32602, "unknown tool")
                } else {
                    let operation = args
                        .get("operation")
                        .and_then(Value::as_str)
                        .unwrap_or("status");
                    let tool_params = args.get("params").cloned().unwrap_or_else(|| json!({}));
                    match tool_response(db, operation, &tool_params) {
                        Ok(result) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&result)?
                                }]
                            }
                        }),
                        Err(e) => mcp_error(id, -32000, &e.to_string()),
                    }
                }
            }
            _ => mcp_error(id, -32601, "method not found"),
        };
        write_mcp_message(&mut stdout, &response, message.framing)?;
    }
    Ok(())
}

fn ariadne_mcp_tool_schema() -> Value {
    json!({
        "name": "ariadne",
        "description": "One-tool interface to Ariadne's code graph: search, review context, impact, paths, architecture, cycles, core nodes, and more.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Operation name, e.g. minimal_context, search, rebuild_fts, diagnostics, report, detect_changes, token_benchmark, blast_radius, review_context, impact, paths, traverse, communities, architecture_overview, surprises, export, wiki, cycles, core, bridge_nodes, gaps, flows, affected_flows."
                },
                "params": {
                    "type": "object",
                    "description": "Operation-specific parameters. Add detail_level=minimal|standard|full for compactness control."
                }
            },
            "required": ["operation"]
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpFraming {
    ContentLength,
    JsonLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpMessage {
    body: String,
    framing: McpFraming,
}

fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<McpMessage>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with('{') {
            return Ok(Some(McpMessage {
                body: trimmed.to_string(),
                framing: McpFraming::JsonLine,
            }));
        }
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(McpMessage {
        body: String::from_utf8(buf)?,
        framing: McpFraming::ContentLength,
    }))
}

fn write_mcp_message<W: Write>(writer: &mut W, value: &Value, framing: McpFraming) -> Result<()> {
    let body = serde_json::to_string(value)?;
    match framing {
        McpFraming::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?
        }
        McpFraming::JsonLine => writeln!(writer, "{}", body)?,
    }
    writer.flush()?;
    Ok(())
}

fn mcp_error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn cmd_god_nodes(db: &Path, top: usize, seed: Option<&str>) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let ranks = if let Some(seed) = seed {
        let seed_id = resolve(&graph, seed)?;
        personalized_pagerank(&graph, &[(seed_id, 1.0)], 0.85, 50)
    } else {
        pagerank(&graph, 0.85, 50)
    };
    let mut sorted: Vec<_> = ranks.iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(seed) = seed {
        println!(
            "top {} god-nodes by personalized pagerank from {}:",
            top, seed
        );
    } else {
        println!("top {} god-nodes by weighted pagerank:", top);
    }
    for (id, rank) in sorted
        .iter()
        .filter(|(id, _)| graph.node(**id).map(is_rankable_node).unwrap_or(false))
        .take(top)
    {
        if let Some(n) = graph.node(**id) {
            println!("  {:.6}  {}  ({:?})", rank, n.qualified_name, n.kind);
        }
    }
    Ok(())
}

fn community_options(
    resolution: f32,
    well_connectedness: f32,
    max_passes: usize,
    max_levels: usize,
    parallel: bool,
    objective: CommunityObjective,
) -> CommunityOptions {
    CommunityOptions {
        resolution,
        well_connectedness,
        max_passes,
        max_levels,
        parallel,
        objective,
        ..Default::default()
    }
}

fn cmd_communities(
    db: &Path,
    top: usize,
    algorithm: &str,
    options: CommunityOptions,
) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let comm = detect_communities(&graph, algorithm, options)?;
    let quality = community_quality(&graph, &comm, options);
    let mut by_comm: std::collections::BTreeMap<usize, Vec<NodeId>> =
        std::collections::BTreeMap::new();
    for (id, &c) in &comm {
        by_comm.entry(c).or_default().push(*id);
    }
    let mut entries: Vec<_> = by_comm.into_iter().collect();
    entries.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
    println!(
        "detected {} {} communities (showing top {}):",
        entries.len(),
        algorithm,
        top
    );
    println!(
        "quality ({}): {:.4}, disconnected {}, mean conductance {:.4}, max conductance {:.4}",
        match quality.objective {
            CommunityObjective::Cpm => "cpm",
            CommunityObjective::Modularity => "modularity",
        },
        quality.score,
        quality.disconnected_communities,
        quality.mean_conductance,
        quality.max_conductance
    );
    for (c, members) in entries.into_iter().take(top) {
        let display_members = ranked_display_members(&graph, &members);
        println!(
            "  {} (community {}, {} members):",
            community_title(&graph, &members),
            c,
            members.len()
        );
        for id in display_members.iter().take(5) {
            if let Some(n) = graph.node(*id) {
                println!("    {}", n.qualified_name);
            }
        }
        if members.len() > 5 {
            println!("    ... and {} more", members.len() - 5);
        }
    }
    Ok(())
}

fn detect_communities(
    graph: &Graph,
    algorithm: &str,
    options: CommunityOptions,
) -> Result<HashMap<NodeId, usize>> {
    match algorithm {
        "louvain" => Ok(louvain_with_options(graph, options)),
        "leiden" => Ok(leiden_with_options(graph, options)),
        other => bail!(
            "unknown community algorithm {}; use louvain or leiden",
            other
        ),
    }
}

fn cmd_flows(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let flows = ariadne_graph::extract::flows::all_flows(&graph);
    println!("detected {} flows (showing top {}):", flows.len(), top);
    for flow_id in flows.into_iter().take(top) {
        if let Some(node) = graph.node(flow_id) {
            let entry = node
                .properties
                .get("entry_qualified_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let crit = node
                .properties
                .get("criticality")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let size = node
                .properties
                .get("node_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let depth = node
                .properties
                .get("depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let is_test = node
                .properties
                .get("is_test_flow")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let tag = if is_test { " [test]" } else { "" };
            println!(
                "  crit={:.2} size={:>3} depth={} {}{}",
                crit, size, depth, entry, tag
            );
        }
    }
    Ok(())
}

fn cmd_affected_flows(db: &Path, base: &str, top: usize) -> Result<()> {
    let analysis = detect_changes_json(db, base, 2)?;
    let hits = analysis["affected_flows"]["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = analysis["affected_flows"]["total"].as_u64().unwrap_or(0);
    println!(
        "{} flow(s) affected by changes since {} (showing top {}):",
        total, base, top
    );
    for hit in hits.into_iter().take(top) {
        let entry = hit["entry_qualified_name"].as_str().unwrap_or("?");
        let crit = hit["criticality"].as_f64().unwrap_or(0.0);
        let size = hit["node_count"].as_u64().unwrap_or(0);
        let is_test = hit["is_test_flow"].as_bool().unwrap_or(false);
        let tag = if is_test { " [test]" } else { "" };
        println!("  crit={:.2} size={:>3} {}{}", crit, size, entry, tag);
    }
    Ok(())
}

fn cmd_test_coverage(db: &Path, target: Option<&str>, base: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let payload = if let Some(target) = target {
        let id = resolve(&graph, target)?;
        json!({
            "operation": "test_coverage",
            "target": target,
            "coverage": test_coverage_json(&graph, &[id]),
        })
    } else {
        let analysis = detect_changes_json(db, base, 2)?;
        json!({
            "operation": "test_coverage",
            "base": base,
            "coverage": analysis["test_coverage"].clone(),
        })
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn cmd_graph_diff(db: &Path, base: &str, head: &str, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&graph_diff_json(&store, &graph, base, head, top)?)?
    );
    Ok(())
}

fn cmd_embed(db: &Path, model: &str) -> Result<()> {
    let mut store = Store::open(db)?;
    let count = store.rebuild_embeddings(model)?;
    println!(
        "built {} embedding(s) with model {} into {}",
        count,
        model,
        db.display()
    );
    Ok(())
}

fn cmd_rebuild_fts(db: &Path) -> Result<()> {
    let mut store = Store::open(db)?;
    let count = store.rebuild_fts_index()?;
    println!("rebuilt FTS5 index: {} node(s) indexed", count);
    Ok(())
}

fn cmd_search(db: &Path, query: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let results = fts_ranked_search(&store, &graph, query, 50);
    println!("found {} result(s):", results.len());
    for hit in results.iter().take(50) {
        if let Some(n) = graph.node(hit.id) {
            println!(
                "  {:.2}  {}  ({:?})  {}  [{}]",
                hit.score,
                n.qualified_name,
                n.kind,
                n.source_uri.as_deref().unwrap_or(""),
                hit.signals.join(",")
            );
        }
    }
    Ok(())
}

fn cmd_tui(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    ariadne_graph::tui::run(&store, &graph)
}

fn stamp_missing_validity(graph: &mut Graph, commit: &str) {
    let node_ids: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
    for id in node_ids {
        if let Some(node) = graph.node_mut(id) {
            if node.valid_from.is_none() {
                node.valid_from = Some(commit.to_string());
            }
        }
    }
    let edge_ids: Vec<_> = graph.edges().map(|(id, _, _, _)| id).collect();
    for id in edge_ids {
        if let Some(edge) = graph.edge_mut(id) {
            if edge.valid_from.is_none() {
                edge.valid_from = Some(commit.to_string());
            }
        }
    }
}

fn apply_temporal_incremental_validity(
    graph: &mut Graph,
    changed_sources: &[String],
    previous_nodes: &[StoredNodeRow],
    previous_edges: &[StoredEdgeRow],
    commit: &str,
) {
    let previous_node_valid_from: HashMap<String, Option<String>> = previous_nodes
        .iter()
        .map(|row| {
            (
                row.node.qualified_name.clone(),
                row.node
                    .valid_from
                    .clone()
                    .or_else(|| Some(commit.to_string())),
            )
        })
        .collect();
    let previous_edge_valid_from: HashMap<String, Option<String>> = previous_edges
        .iter()
        .map(|row| {
            (
                edge_identity(&row.src_qname, &row.dst_qname, row.edge.kind),
                row.edge
                    .valid_from
                    .clone()
                    .or_else(|| Some(commit.to_string())),
            )
        })
        .collect();

    let changed_source_set: HashSet<&str> = changed_sources.iter().map(String::as_str).collect();
    let node_ids: Vec<NodeId> = graph
        .nodes()
        .filter(|(_, node)| {
            node.source_uri
                .as_deref()
                .map(|source| changed_source_set.contains(source))
                .unwrap_or(false)
        })
        .map(|(id, _)| id)
        .collect();
    for id in node_ids {
        if let Some(node) = graph.node_mut(id) {
            node.valid_from = previous_node_valid_from
                .get(&node.qualified_name)
                .cloned()
                .flatten()
                .or_else(|| Some(commit.to_string()));
            node.valid_to = None;
        }
    }

    let edge_updates: Vec<_> = graph
        .edges()
        .filter_map(|(id, src, dst, edge)| {
            let src_source = graph.node(src).and_then(|node| node.source_uri.as_deref());
            let dst_source = graph.node(dst).and_then(|node| node.source_uri.as_deref());
            let touches_changed_source = src_source
                .map(|source| changed_source_set.contains(source))
                .unwrap_or(false)
                || dst_source
                    .map(|source| changed_source_set.contains(source))
                    .unwrap_or(false);
            if !touches_changed_source {
                return None;
            }
            let src_qname = graph.node(src)?.qualified_name.clone();
            let dst_qname = graph.node(dst)?.qualified_name.clone();
            Some((id, edge_identity(&src_qname, &dst_qname, edge.kind)))
        })
        .collect();
    for (id, identity) in edge_updates {
        if let Some(edge) = graph.edge_mut(id) {
            edge.valid_from = previous_edge_valid_from
                .get(&identity)
                .cloned()
                .flatten()
                .or_else(|| Some(commit.to_string()));
            edge.valid_to = None;
        }
    }
}

fn current_node_keys_for_sources(graph: &Graph, sources: &[String]) -> HashSet<String> {
    let source_set: HashSet<&str> = sources.iter().map(String::as_str).collect();
    graph
        .nodes()
        .filter(|(_, node)| {
            node.source_uri
                .as_deref()
                .map(|source| source_set.contains(source))
                .unwrap_or(false)
        })
        .map(|(_, node)| node.qualified_name.clone())
        .collect()
}

fn current_edge_keys_for_sources(graph: &Graph, sources: &[String]) -> HashSet<String> {
    let source_set: HashSet<&str> = sources.iter().map(String::as_str).collect();
    graph
        .edges()
        .filter_map(|(_, src, dst, edge)| {
            let src_source = graph.node(src).and_then(|node| node.source_uri.as_deref());
            let dst_source = graph.node(dst).and_then(|node| node.source_uri.as_deref());
            let touches_changed_source = src_source
                .map(|source| source_set.contains(source))
                .unwrap_or(false)
                || dst_source
                    .map(|source| source_set.contains(source))
                    .unwrap_or(false);
            if !touches_changed_source {
                return None;
            }
            Some(edge_identity(
                &graph.node(src)?.qualified_name,
                &graph.node(dst)?.qualified_name,
                edge.kind,
            ))
        })
        .collect()
}

fn removed_nodes_for_archive(
    previous_nodes: &[StoredNodeRow],
    current_node_keys: &HashSet<String>,
) -> Vec<StoredNodeRow> {
    previous_nodes
        .iter()
        .filter(|row| !current_node_keys.contains(&row.node.qualified_name))
        .cloned()
        .collect()
}

fn removed_edges_for_archive(
    previous_edges: &[StoredEdgeRow],
    current_edge_keys: &HashSet<String>,
) -> Vec<StoredEdgeRow> {
    previous_edges
        .iter()
        .filter(|row| {
            !current_edge_keys.contains(&edge_identity(
                &row.src_qname,
                &row.dst_qname,
                row.edge.kind,
            ))
        })
        .cloned()
        .collect()
}

fn previous_nodes_for_deleted_sources(
    previous_nodes: &[StoredNodeRow],
    deleted_sources: &[String],
) -> Vec<StoredNodeRow> {
    let deleted_set: HashSet<&str> = deleted_sources.iter().map(String::as_str).collect();
    previous_nodes
        .iter()
        .filter(|row| {
            row.node
                .source_uri
                .as_deref()
                .map(|source| deleted_set.contains(source))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn previous_edges_for_deleted_sources(
    previous_edges: &[StoredEdgeRow],
    deleted_sources: &[String],
) -> Vec<StoredEdgeRow> {
    let deleted_set: HashSet<&str> = deleted_sources.iter().map(String::as_str).collect();
    previous_edges
        .iter()
        .filter(|row| {
            row.source_uri
                .as_deref()
                .map(|source| deleted_set.contains(source))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn collect_file_hashes(root: &Path) -> Result<Vec<(String, String)>> {
    let mut hashes = Vec::new();
    let ignore = ignore_set(root);
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && is_supported(path) {
            hashes.push((path.to_string_lossy().to_string(), hash_file(path)?));
        }
    }
    hashes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(hashes)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn daemon_config_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()?
        .join(".ariadne")
        .join("daemon.json"))
}

fn load_daemon_repos() -> Result<Vec<Value>> {
    let path = daemon_config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)?;
    Ok(serde_json::from_str::<Value>(&data)?
        .get("repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn save_daemon_repos(repos: &[Value]) -> Result<()> {
    let path = daemon_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&json!({ "repos": repos }))?,
    )?;
    Ok(())
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string param '{}'", key))
}

fn minimal_context_json(graph: &Graph, target: Option<&str>, mode: &str) -> Value {
    let hits = target
        .map(|q| ranked_search(graph, q, 5))
        .unwrap_or_else(|| Vec::new());
    let top_symbols: Vec<_> = hits
        .iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "id": hit.id.0,
                    "score": hit.score,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    let risk = if hits.first().map(|h| h.score > 120.0).unwrap_or(false) {
        "medium"
    } else {
        "low"
    };
    json!({
        "operation": "minimal_context",
        "mode": mode,
        "target": target,
        "summary": if target.is_some() {
            "Resolved target candidates and prepared next graph operations."
        } else {
            "No target supplied; start with search, detect_changes, or bridge_nodes."
        },
        "risk": risk,
        "top_symbols": top_symbols,
        "suggested_next_tools": ["detect_changes", "impact", "traverse", "review_context"]
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailLevel {
    Minimal,
    Standard,
    Full,
}

impl DetailLevel {
    fn parse(value: &str) -> Self {
        match value {
            "minimal" => Self::Minimal,
            "full" => Self::Full,
            _ => Self::Standard,
        }
    }

    fn from_params(params: &Value) -> Self {
        params
            .get("detail_level")
            .and_then(Value::as_str)
            .map(Self::parse)
            .unwrap_or(Self::Standard)
    }

    fn limit(self, standard: usize) -> usize {
        match self {
            Self::Minimal => standard.min(5),
            Self::Standard => standard,
            Self::Full => standard.saturating_mul(4),
        }
    }
}

fn compact_for_detail(mut value: Value, detail: DetailLevel) -> Value {
    if detail == DetailLevel::Minimal {
        if let Some(arr) = value.get_mut("snippets").and_then(Value::as_array_mut) {
            for item in arr {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("snippet");
                }
            }
        }
    }
    value
}

#[derive(Debug, Clone, Copy)]
struct ResponseGuardrails {
    offset: usize,
    limit: usize,
    include_graph_summary: bool,
}

impl ResponseGuardrails {
    const HARD_LIMIT: usize = 500;

    fn from_params(params: &Value, detail: DetailLevel) -> Self {
        let default_limit = match detail {
            DetailLevel::Minimal => 10,
            DetailLevel::Standard => 50,
            DetailLevel::Full => 200,
        };
        let requested = params
            .get("response_limit")
            .or_else(|| params.get("page_limit"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(default_limit);
        Self {
            offset: params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
            limit: requested.clamp(1, Self::HARD_LIMIT),
            include_graph_summary: params
                .get("include_graph_summary")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

fn apply_response_guardrails(
    mut value: Value,
    graph: &Graph,
    params: &Value,
    detail: DetailLevel,
) -> Value {
    let guardrails = ResponseGuardrails::from_params(params, detail);
    let mut pagination = serde_json::Map::new();
    for key in PAGEABLE_RESPONSE_KEYS {
        if let Some(arr) = value.get_mut(key).and_then(Value::as_array_mut) {
            let total = arr.len();
            let start = guardrails.offset.min(total);
            let end = (start + guardrails.limit).min(total);
            let page: Vec<Value> = arr[start..end].to_vec();
            *arr = page;
            pagination.insert(
                key.to_string(),
                json!({
                    "offset": guardrails.offset,
                    "limit": guardrails.limit,
                    "returned": end.saturating_sub(start),
                    "total": total,
                    "has_more": end < total,
                }),
            );
        }
    }

    if let Some(obj) = value.as_object_mut() {
        if guardrails.include_graph_summary && !obj.contains_key("graph_summary") {
            obj.insert("graph_summary".to_string(), graph_summary_json(graph));
        }
        obj.insert(
            "guardrails".to_string(),
            json!({
                "response_limit": guardrails.limit,
                "offset": guardrails.offset,
                "hard_limit": ResponseGuardrails::HARD_LIMIT,
                "pagination": pagination,
            }),
        );
    }
    value
}

const PAGEABLE_RESPONSE_KEYS: &[&str] = &[
    "hits",
    "nodes",
    "paths",
    "impacted",
    "changed_files",
    "changed_ranges",
    "changed_symbols",
    "changed_nodes",
    "snippets",
    "communities",
    "cross_community_coupling",
    "bridge_nodes",
    "cycles",
    "core_nodes",
    "articulation_points",
    "warnings",
    "questions",
    "omitted",
];

fn graph_summary_json(graph: &Graph) -> Value {
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    for (_, node) in graph.nodes() {
        *kind_counts.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
        if let Some(source) = node.source_uri.as_ref() {
            *source_counts.entry(source.clone()).or_insert(0) += 1;
        }
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut sources: Vec<_> = source_counts.into_iter().collect();
    sources.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "kind_counts": kinds.into_iter().map(|(kind, count)| json!({
            "kind": kind,
            "count": count,
        })).collect::<Vec<_>>(),
        "top_sources": sources.into_iter().take(5).map(|(source, nodes)| json!({
            "source": source,
            "nodes": nodes,
        })).collect::<Vec<_>>(),
    })
}

fn community_objective_from_params(params: &Value) -> Option<CommunityObjective> {
    params
        .get("objective")
        .and_then(Value::as_str)
        .and_then(|value| parse_community_objective(value).ok())
}

fn parse_community_objective(value: &str) -> Result<CommunityObjective> {
    match value.to_lowercase().as_str() {
        "modularity" => Ok(CommunityObjective::Modularity),
        "cpm" => Ok(CommunityObjective::Cpm),
        other => bail!(
            "unknown community objective {}; use modularity or cpm",
            other
        ),
    }
}

fn community_options_from_params(params: &Value) -> CommunityOptions {
    community_options(
        params
            .get("resolution")
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32,
        params
            .get("well_connectedness")
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32,
        params
            .get("max_passes")
            .and_then(Value::as_u64)
            .unwrap_or(50) as usize,
        params
            .get("max_levels")
            .and_then(Value::as_u64)
            .unwrap_or(10) as usize,
        !params
            .get("no_parallel")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        community_objective_from_params(params).unwrap_or(CommunityObjective::Modularity),
    )
}

fn community_quality_json(quality: &ariadne_graph::query::CommunityQuality) -> Value {
    json!({
        "community_count": quality.community_count,
        "singleton_count": quality.singleton_count,
        "min_size": quality.min_size,
        "max_size": quality.max_size,
        "mean_size": quality.mean_size,
        "objective": match quality.objective {
            CommunityObjective::Cpm => "cpm",
            CommunityObjective::Modularity => "modularity",
        },
        "score": quality.score,
        "modularity": quality.score,
        "disconnected_communities": quality.disconnected_communities,
        "mean_conductance": quality.mean_conductance,
        "max_conductance": quality.max_conductance,
    })
}

fn communities_json(
    graph: &Graph,
    algorithm: &str,
    options: CommunityOptions,
    limit: usize,
) -> Result<Value> {
    let communities = detect_communities(graph, algorithm, options)?;
    let quality = community_quality(graph, &communities, options);
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in &communities {
        by_comm.entry(community).or_default().push(node);
    }

    let mut rows: Vec<_> = by_comm
        .into_iter()
        .map(|(community, members)| {
            let display = ranked_display_members(graph, &members);
            let key_nodes: Vec<_> = display
                .into_iter()
                .take(8)
                .filter_map(|id| {
                    let node = graph.node(id)?;
                    Some(json!({
                        "id": id.0,
                        "qualified_name": node.qualified_name,
                        "kind": node.kind,
                        "source_uri": node.source_uri,
                    }))
                })
                .collect();
            json!({
                "community": community,
                "title": community_title(graph, &members),
                "size": members.len(),
                "key_nodes": key_nodes,
            })
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row["size"].as_u64().unwrap_or_default()));
    rows.truncate(limit);

    Ok(json!({
        "operation": "communities",
        "algorithm": algorithm,
        "options": {
            "resolution": options.resolution,
            "well_connectedness": options.well_connectedness,
            "max_passes": options.max_passes,
            "max_levels": options.max_levels,
            "parallel": options.parallel,
            "objective": match options.objective {
                CommunityObjective::Cpm => "cpm",
                CommunityObjective::Modularity => "modularity",
            },
        },
        "quality": community_quality_json(&quality),
        "communities": rows,
    }))
}

fn architecture_overview_json(graph: &Graph, detail: DetailLevel) -> Value {
    let communities = leiden(graph);
    let quality = community_quality(graph, &communities, CommunityOptions::default());
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in &communities {
        by_comm.entry(community).or_default().push(node);
    }

    let mut summaries: Vec<_> = by_comm
        .iter()
        .map(|(community, members)| {
            let mut files: HashMap<String, usize> = HashMap::new();
            let mut kinds: HashMap<String, usize> = HashMap::new();
            for id in members {
                if let Some(node) = graph.node(*id) {
                    if let Some(source) = &node.source_uri {
                        *files.entry(source.clone()).or_insert(0) += 1;
                    }
                    *kinds.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
                }
            }
            let mut top_files: Vec<_> = files.into_iter().collect();
            top_files.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            let mut kind_counts: Vec<_> = kinds.into_iter().collect();
            kind_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            json!({
                "community": community,
                "size": members.len(),
                "top_files": top_files.into_iter().take(detail.limit(5)).map(|(path, count)| json!({"path": path, "nodes": count})).collect::<Vec<_>>(),
                "kind_counts": kind_counts.into_iter().map(|(kind, count)| json!({"kind": kind, "count": count})).collect::<Vec<_>>(),
            })
        })
        .collect();
    summaries.sort_by_key(|v| std::cmp::Reverse(v["size"].as_u64().unwrap_or_default()));
    summaries.truncate(detail.limit(12));

    let mut coupling: HashMap<(usize, usize), usize> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        let Some(a) = communities.get(&src).copied() else {
            continue;
        };
        let Some(b) = communities.get(&dst).copied() else {
            continue;
        };
        if a != b {
            let key = if a < b { (a, b) } else { (b, a) };
            *coupling.entry(key).or_insert(0) += 1;
        }
    }
    let mut coupling_rows: Vec<_> = coupling
        .into_iter()
        .map(|((a, b), edges)| json!({"from": a, "to": b, "edges": edges}))
        .collect();
    coupling_rows.sort_by_key(|v| std::cmp::Reverse(v["edges"].as_u64().unwrap_or_default()));
    coupling_rows.truncate(detail.limit(10));

    let bridges = bridge_nodes_json(graph, detail.limit(10));
    let cycles = cycles_json(graph, detail.limit(8));
    let core = core_json(graph, detail.limit(10));
    let articulations = articulation_json(graph, detail.limit(10));
    let warnings: Vec<_> = coupling_rows
        .iter()
        .take(5)
        .filter_map(|row| {
            let edges = row["edges"].as_u64().unwrap_or_default();
            (edges >= 5).then(|| {
                json!({
                    "kind": "cross_community_coupling",
                    "severity": if edges >= 20 { "high" } else { "medium" },
                    "communities": [row["from"].clone(), row["to"].clone()],
                    "edges": edges,
                })
            })
        })
        .collect();

    json!({
        "operation": "architecture_overview",
        "detail_level": match detail {
            DetailLevel::Minimal => "minimal",
            DetailLevel::Standard => "standard",
            DetailLevel::Full => "full",
        },
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "community_count": by_comm.len(),
        "community_quality": community_quality_json(&quality),
        "communities": summaries,
        "cross_community_coupling": coupling_rows,
        "bridge_nodes": bridges["hits"].clone(),
        "cycles": cycles["hits"].clone(),
        "core_nodes": core["hits"].clone(),
        "articulation_points": articulations["hits"].clone(),
        "warnings": warnings,
        "suggested_next_tools": ["bridge_nodes", "cycles", "core", "articulation_points", "traverse", "impact", "gaps"]
    })
}

fn detect_changes_json(db: &Path, base: &str, max_depth: usize) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let (changed_files, changed_nodes, changed_ranges, mapping_precision, temporal) =
        if let Some(head) = git_commit_hash("HEAD")? {
            if store.has_temporal_history()? {
                if let Some(base_hash) = git_commit_hash(base)? {
                    let diff = git_changed_diff(base).unwrap_or_default();
                    let temporal = store_temporal_diff(&store, &base_hash, &head)?;
                    let changed_nodes = temporal_active_node_ids(&graph, &temporal);
                    let changed_files = temporal.changed_files.clone();
                    let mapping_precision = if diff.iter().any(|f| !f.hunks.is_empty()) {
                        "temporal+line_ranges"
                    } else {
                        "temporal"
                    };
                    (
                        changed_files,
                        changed_nodes,
                        changed_ranges_json(&graph, &diff),
                        mapping_precision.to_string(),
                        Some(store_temporal_diff_json(&temporal, 50)),
                    )
                } else {
                    old_changed_diff(&graph, base)
                }
            } else {
                old_changed_diff(&graph, base)
            }
        } else {
            old_changed_diff(&graph, base)
        };
    let mut impacted = Vec::new();
    let mut seen = HashSet::new();
    for seed in &changed_nodes {
        for hit in analyze_impact(
            &graph,
            ImpactQuery {
                seed: *seed,
                max_hops: max_depth,
                limit: 8,
            },
        ) {
            if seen.insert(hit.id) {
                if let Some(n) = graph.node(hit.id) {
                    impacted.push(json!({
                        "id": hit.id.0,
                        "score": hit.score,
                        "distance": hit.distance,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                        "source_uri": n.source_uri,
                    }));
                }
            }
        }
    }
    impacted.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let impacted_total = impacted.len();
    impacted.truncate(25);
    let test_coverage = test_coverage_json(&graph, &changed_nodes);
    let missing_count = test_coverage["missing_count"].as_u64().unwrap_or(0) as usize;
    let affected_flows = affected_flows_json(&graph, &changed_nodes, 10);
    let top_flow_criticality = affected_flows["hits"]
        .as_array()
        .and_then(|hits| hits.first())
        .and_then(|f| f["criticality"].as_f64())
        .unwrap_or(0.0);
    let risk_score = risk_score(
        &graph,
        &changed_nodes,
        impacted.len(),
        missing_count,
        top_flow_criticality,
    );
    Ok(json!({
        "operation": "detect_changes",
        "base": base,
        "changed_files": changed_files,
        "changed_ranges": changed_ranges,
        "changed_symbol_total": changed_nodes.len(),
        "changed_symbols": nodes_json(&graph, &changed_nodes, 50),
        "changed_nodes": nodes_json(&graph, &changed_nodes, 50),
        "temporal": temporal,
        "impacted_total": impacted_total,
        "impacted": impacted,
        "test_coverage": test_coverage,
        "affected_flows": affected_flows,
        "risk_score": risk_score,
        "risk": risk_label(risk_score),
        "mapping_precision": mapping_precision,
        "suggested_next_tools": ["review_context", "impact", "traverse", "suggested_questions"]
    }))
}

fn blast_radius_json(db: &Path, base: &str, max_depth: usize, limit: usize) -> Result<Value> {
    let limit = limit.max(1);
    let analysis = detect_changes_json(db, base, max_depth)?;
    let changed_files = analysis["changed_files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let changed_nodes = analysis["changed_nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let impacted = analysis["impacted"].as_array().cloned().unwrap_or_default();
    let affected_file_count = blast_affected_file_total(&changed_files, &changed_nodes, &impacted);
    let affected_files = blast_affected_files(&changed_files, &changed_nodes, &impacted, limit);
    let changed_symbol_count = analysis["changed_symbol_total"]
        .as_u64()
        .unwrap_or(changed_nodes.len() as u64);
    let impacted_count = analysis["impacted_total"]
        .as_u64()
        .unwrap_or(impacted.len() as u64) as usize;
    let test_gap_count = analysis["test_coverage"]["missing_count"]
        .as_u64()
        .unwrap_or(0);
    let affected_flow_count = analysis["affected_flows"]["total"].as_u64().unwrap_or(0);
    let wide = impacted_count > 20 || affected_file_count > 3;
    let guidance = blast_radius_guidance(impacted_count, affected_file_count, test_gap_count);

    Ok(json!({
        "operation": "blast_radius",
        "base": base,
        "max_depth": max_depth,
        "summary": {
            "changed_file_count": changed_files.len(),
            "changed_symbol_count": changed_symbol_count,
            "impacted_symbol_count": impacted_count,
            "affected_file_count": affected_file_count,
            "affected_flow_count": affected_flow_count,
            "test_gap_count": test_gap_count,
            "wide": wide,
            "risk": analysis["risk"].clone(),
            "risk_score": analysis["risk_score"].clone(),
            "mapping_precision": analysis["mapping_precision"].clone(),
        },
        "changed": blast_symbol_groups(&changed_nodes, limit),
        "impacted": blast_symbol_groups(&impacted, limit),
        "affected_files": affected_files,
        "changed_hunks": blast_changed_hunks(&analysis["changed_ranges"], limit),
        "affected_flows": analysis["affected_flows"].clone(),
        "test_coverage": blast_test_coverage_summary(&analysis["test_coverage"], limit),
        "guidance": guidance,
        "suggested_next_tools": ["review_context", "impact", "affected_flows", "test_coverage"]
    }))
}

fn blast_symbol_groups(nodes: &[Value], limit: usize) -> Value {
    let mut functions = Vec::new();
    let mut classes = Vec::new();
    let mut files = Vec::new();
    let mut other = Vec::new();

    for node in nodes {
        let target = match node_kind_str(node) {
            "function" | "method" => &mut functions,
            "class" | "type" | "trait" | "impl" | "enum" | "struct" => &mut classes,
            "file" | "document" => &mut files,
            _ => &mut other,
        };
        if target.len() < limit {
            target.push(node.clone());
        }
    }

    json!({
        "functions": functions,
        "classes": classes,
        "files": files,
        "other": other,
    })
}

fn blast_affected_files(
    changed_files: &[Value],
    changed_nodes: &[Value],
    impacted: &[Value],
    limit: usize,
) -> Value {
    let mut files = blast_affected_file_map(changed_files, changed_nodes, impacted);
    let mut rows: Vec<_> = files
        .drain()
        .map(|(path, (changed_symbols, impacted_symbols, max_score))| {
            json!({
                "path": path,
                "changed_symbols": changed_symbols,
                "impacted_symbols": impacted_symbols,
                "max_score": max_score,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b["changed_symbols"]
            .as_u64()
            .cmp(&a["changed_symbols"].as_u64())
            .then_with(|| {
                b["impacted_symbols"]
                    .as_u64()
                    .cmp(&a["impacted_symbols"].as_u64())
            })
            .then_with(|| {
                b["max_score"]
                    .as_f64()
                    .partial_cmp(&a["max_score"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    rows.truncate(limit);
    json!(rows)
}

fn blast_affected_file_total(
    changed_files: &[Value],
    changed_nodes: &[Value],
    impacted: &[Value],
) -> usize {
    blast_affected_file_map(changed_files, changed_nodes, impacted).len()
}

fn blast_affected_file_map(
    changed_files: &[Value],
    changed_nodes: &[Value],
    impacted: &[Value],
) -> HashMap<String, (usize, usize, f64)> {
    let mut files: HashMap<String, (usize, usize, f64)> = HashMap::new();
    for file in changed_files {
        if let Some(path) = file.as_str() {
            files.entry(path.to_string()).or_insert((0, 0, 0.0));
        }
    }
    for node in changed_nodes {
        if let Some(path) = node["source_uri"].as_str() {
            files.entry(path.to_string()).or_insert((0, 0, 0.0)).0 += 1;
        }
    }
    for hit in impacted {
        if let Some(path) = hit["source_uri"].as_str() {
            let score = hit["score"].as_f64().unwrap_or_default();
            let entry = files.entry(path.to_string()).or_insert((0, 0, 0.0));
            entry.1 += 1;
            entry.2 = entry.2.max(score);
        }
    }
    files
}

fn blast_radius_guidance(
    impacted_count: usize,
    affected_file_count: usize,
    test_gap_count: u64,
) -> Vec<String> {
    let mut guidance = Vec::new();
    if test_gap_count > 0 {
        guidance.push(format!(
            "{} changed symbol(s) lack direct or nearby test coverage.",
            test_gap_count
        ));
    }
    if impacted_count > 20 {
        guidance.push(format!(
            "Wide blast radius: {} impacted symbol(s). Review callers and dependents carefully.",
            impacted_count
        ));
    }
    if affected_file_count > 3 {
        guidance.push(format!(
            "Cross-file impact: {} affected file(s). Consider whether the change should be split or staged.",
            affected_file_count
        ));
    }
    if guidance.is_empty() {
        guidance.push("Changes appear contained with minimal blast radius.".to_string());
    }
    guidance
}

fn blast_changed_hunks(changed_ranges: &Value, limit: usize) -> Value {
    let mut files = Vec::new();
    for file in changed_ranges.as_array().into_iter().flatten() {
        let mut hunks = Vec::new();
        for hunk in file["hunks"].as_array().into_iter().flatten() {
            if hunk["symbols"]
                .as_array()
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                continue;
            }
            hunks.push(hunk.clone());
            if hunks.len() >= limit {
                break;
            }
        }
        if !hunks.is_empty() {
            files.push(json!({
                "path": file["path"].clone(),
                "hunks": hunks,
            }));
        }
        if files.len() >= limit {
            break;
        }
    }
    json!(files)
}

fn blast_test_coverage_summary(test_coverage: &Value, limit: usize) -> Value {
    let missing: Vec<Value> = test_coverage["missing"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect();
    json!({
        "covered_count": test_coverage["covered_count"].clone(),
        "missing_count": test_coverage["missing_count"].clone(),
        "total_symbols": test_coverage["total_symbols"].clone(),
        "coverage_ratio": test_coverage["coverage_ratio"].clone(),
        "missing": missing,
    })
}

fn node_kind_str(node: &Value) -> &str {
    node.get("kind").and_then(Value::as_str).unwrap_or("")
}

fn is_rankable_node(node: &ariadne_graph::Node) -> bool {
    !node.qualified_name.starts_with("call::")
        && !matches!(
            node.kind,
            NodeKind::Module
                | NodeKind::File
                | NodeKind::Document
                | NodeKind::Section
                | NodeKind::Concept
                | NodeKind::Diagram
                | NodeKind::Image
                | NodeKind::Flow
                | NodeKind::Hyperedge
                | NodeKind::Author
                | NodeKind::Commit
        )
}

fn is_test_like_node(node: &ariadne_graph::Node) -> bool {
    node.properties
        .get("is_test")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || node.qualified_name.contains("::tests::")
        || node.name.starts_with("test_")
        || node
            .source_uri
            .as_deref()
            .map(|source| source.contains("/tests/") || source.contains("\\tests\\"))
            .unwrap_or(false)
}

fn is_actionable_unresolved_call(node: &ariadne_graph::Node, incoming: usize) -> bool {
    if !node.qualified_name.starts_with("call::") || incoming < 2 {
        return false;
    }
    !node.name.starts_with("cmd_")
        && !is_generic_utility_name(&node.name)
        && !is_low_signal_call_name(&node.name)
}

fn is_low_signal_call_name(name: &str) -> bool {
    !name.contains('_')
        && name.len() <= 12
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn is_generic_utility_name(name: &str) -> bool {
    matches!(
        name,
        "Ok" | "Err"
            | "as_deref"
            | "as_ref"
            | "as_str"
            | "cloned"
            | "clone"
            | "collect"
            | "contains"
            | "default"
            | "display"
            | "edge_count"
            | "enumerate"
            | "exists"
            | "expect"
            | "extend"
            | "filter"
            | "filter_map"
            | "find"
            | "from_secs"
            | "get"
            | "into_iter"
            | "insert"
            | "is_empty"
            | "iter"
            | "len"
            | "load"
            | "map"
            | "new"
            | "node_count"
            | "ok"
            | "open"
            | "parse"
            | "push"
            | "save"
            | "sleep"
            | "sort"
            | "sort_by"
            | "sort_by_key"
            | "to_string"
            | "to_str"
            | "to_string_pretty"
            | "unwrap"
            | "unwrap_or"
            | "unwrap_or_default"
            | "unwrap_or_else"
            | "zip"
    )
}

fn token_benchmark_json(
    db: &Path,
    base: &str,
    token_budget: usize,
    max_lines_per_file: usize,
) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let analysis = detect_changes_json(db, base, 2)?;
    let changed_files: Vec<String> = analysis["changed_files"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();

    let all_sources = git_tracked_files().unwrap_or_else(|_| graph_source_files(&graph));
    let full_repo_tokens = source_files_tokens(&all_sources);
    let changed_file_tokens = source_files_tokens(&changed_files);
    let detect_changes_tokens = approx_tokens(&analysis.to_string());
    let review_context = review_context_json(db, base, max_lines_per_file, token_budget)?;
    let review_context_tokens = review_context["used_tokens"]
        .as_u64()
        .unwrap_or_else(|| approx_tokens(&review_context.to_string()) as u64)
        as usize;
    let blast_radius = blast_radius_json(db, base, 2, 25)?;
    let blast_radius_tokens = approx_tokens(&blast_radius.to_string());
    let suggested = suggested_questions_json(&analysis, 10);
    let suggested_tokens = approx_tokens(&suggested.to_string());

    let scenarios = vec![
        token_scenario("naive_full_repo", full_repo_tokens, full_repo_tokens),
        token_scenario("naive_changed_files", changed_file_tokens, full_repo_tokens),
        token_scenario(
            "detect_changes_json",
            detect_changes_tokens,
            changed_file_tokens,
        ),
        token_scenario("review_context", review_context_tokens, changed_file_tokens),
        token_scenario("blast_radius", blast_radius_tokens, changed_file_tokens),
        token_scenario("suggested_questions", suggested_tokens, changed_file_tokens),
    ];

    Ok(json!({
        "operation": "token_benchmark",
        "base": base,
        "estimator": "approx_bytes_div_4",
        "summary": {
            "source_file_count": all_sources.len(),
            "changed_file_count": changed_files.len(),
            "full_repo_tokens": full_repo_tokens,
            "changed_file_tokens": changed_file_tokens,
            "review_context_budget": token_budget,
            "best_ratio_vs_changed_files": scenarios.iter()
                .filter_map(|s| s["ratio_vs_baseline"].as_f64())
                .filter(|ratio| ratio.is_finite())
                .fold(0.0, f64::max),
        },
        "scenarios": scenarios,
        "notes": [
            "Token counts are approximate and intended for relative comparisons.",
            "Graph-query payloads include structured JSON overhead; review_context used_tokens counts emitted snippets."
        ],
    }))
}

fn graph_source_files(graph: &Graph) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, node) in graph.nodes() {
        let Some(source) = &node.source_uri else {
            continue;
        };
        if seen.insert(source.clone()) {
            out.push(source.clone());
        }
    }
    out.sort();
    out
}

fn source_files_tokens(files: &[String]) -> usize {
    let mut total = 0usize;
    for file in files {
        if let Ok(content) = fs::read_to_string(file) {
            total += approx_tokens(&content);
        }
    }
    total
}

fn git_tracked_files() -> Result<Vec<String>> {
    let output = Command::new("git").args(["ls-files"]).output()?;
    if !output.status.success() {
        bail!("git ls-files failed");
    }
    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| Path::new(line).is_file())
        .map(ToOwned::to_owned)
        .collect();
    Ok(files)
}

fn token_scenario(name: &str, tokens: usize, baseline: usize) -> Value {
    let ratio = if tokens > 0 && baseline > 0 {
        baseline as f64 / tokens as f64
    } else {
        0.0
    };
    json!({
        "name": name,
        "tokens": tokens,
        "baseline_tokens": baseline,
        "ratio_vs_baseline": ratio,
        "savings_percent": if baseline > 0 {
            ((baseline.saturating_sub(tokens)) as f64 / baseline as f64) * 100.0
        } else {
            0.0
        },
    })
}

fn append_unique_nodes(nodes: &mut Vec<NodeId>, extra: Vec<NodeId>) {
    let mut seen: HashSet<NodeId> = nodes.iter().copied().collect();
    for id in extra {
        if seen.insert(id) {
            nodes.push(id);
        }
    }
}

fn old_changed_diff(
    graph: &Graph,
    base: &str,
) -> (Vec<String>, Vec<NodeId>, Vec<Value>, String, Option<Value>) {
    let diff = git_changed_diff(base).unwrap_or_default();
    let changed_files: Vec<String> = diff.iter().map(|file| file.path.clone()).collect();
    let line_changed_nodes = nodes_for_changed_ranges(graph, &diff);
    let fallback_to_file_scope = line_changed_nodes.is_empty() && !changed_files.is_empty();
    let changed_nodes = if fallback_to_file_scope {
        nodes_for_files(graph, &changed_files)
    } else {
        line_changed_nodes
    };
    let mapping_precision = if fallback_to_file_scope {
        "file"
    } else if diff.iter().any(|f| !f.hunks.is_empty()) && !changed_nodes.is_empty() {
        "line"
    } else {
        "none"
    };
    (
        changed_files,
        changed_nodes,
        changed_ranges_json(graph, &diff),
        mapping_precision.to_string(),
        None,
    )
}

#[derive(Default)]
struct StoreTemporalDiff {
    added_nodes: Vec<StoredNodeRow>,
    removed_nodes: Vec<StoredNodeRow>,
    added_edges: Vec<StoredEdgeRow>,
    removed_edges: Vec<StoredEdgeRow>,
    changed_files: Vec<String>,
}

fn graph_diff_json(
    store: &Store,
    graph: &Graph,
    base: &str,
    head: &str,
    limit: usize,
) -> Result<Value> {
    if !store.has_temporal_history()? {
        return Ok(json!({
            "operation": "graph_diff",
            "available": false,
            "base": base,
            "head": head,
            "reason": "graph has no temporal validity data",
        }));
    }

    let base_resolved = git_commit_hash(base)?.unwrap_or_else(|| base.to_string());
    let head_resolved = git_commit_hash(head)?.unwrap_or_else(|| head.to_string());
    let diff = store_temporal_diff(store, &base_resolved, &head_resolved)?;
    let changed_nodes = temporal_active_node_ids(graph, &diff);

    Ok(json!({
        "operation": "graph_diff",
        "available": true,
        "base": base,
        "base_resolved": base_resolved,
        "head": head,
        "head_resolved": head_resolved,
        "summary": {
            "changed_nodes": changed_nodes.len(),
            "added_nodes": diff.added_nodes.len(),
            "removed_nodes": diff.removed_nodes.len(),
            "added_edges": diff.added_edges.len(),
            "removed_edges": diff.removed_edges.len(),
            "changed_files": diff.changed_files.len(),
        },
        "changed_files": diff.changed_files,
        "nodes_by_kind": {
            "added": node_kind_counts_json(&diff.added_nodes),
            "removed": node_kind_counts_json(&diff.removed_nodes),
        },
        "edges_by_kind": {
            "added": edge_kind_counts_json(&diff.added_edges),
            "removed": edge_kind_counts_json(&diff.removed_edges),
        },
        "diff": store_temporal_diff_json(&diff, limit),
    }))
}

fn store_temporal_diff(store: &Store, base: &str, head: &str) -> Result<StoreTemporalDiff> {
    let mut diff = StoreTemporalDiff::default();
    let mut changed_files = HashSet::new();
    let mut ancestor_cache = HashMap::new();
    let mut is_ancestor_cached = |ancestor: &str, descendant: &str| {
        *ancestor_cache
            .entry((ancestor.to_string(), descendant.to_string()))
            .or_insert_with(|| git_is_ancestor(ancestor, descendant))
    };

    for row in store.temporal_nodes()? {
        let active_at_base = is_active_at(
            row.node.valid_from.as_deref(),
            row.node.valid_to.as_deref(),
            base,
            &mut is_ancestor_cached,
        );
        let active_at_head = is_active_at(
            row.node.valid_from.as_deref(),
            row.node.valid_to.as_deref(),
            head,
            &mut is_ancestor_cached,
        );
        match (active_at_base, active_at_head) {
            (false, true) => {
                if let Some(source) = row.node.source_uri.clone() {
                    changed_files.insert(source);
                }
                diff.added_nodes.push(row);
            }
            (true, false) => {
                if let Some(source) = row.node.source_uri.clone() {
                    changed_files.insert(source);
                }
                diff.removed_nodes.push(row);
            }
            _ => {}
        }
    }

    for row in store.temporal_edges()? {
        let active_at_base = is_active_at(
            row.edge.valid_from.as_deref(),
            row.edge.valid_to.as_deref(),
            base,
            &mut is_ancestor_cached,
        );
        let active_at_head = is_active_at(
            row.edge.valid_from.as_deref(),
            row.edge.valid_to.as_deref(),
            head,
            &mut is_ancestor_cached,
        );
        match (active_at_base, active_at_head) {
            (false, true) => {
                if let Some(source) = row.source_uri.clone() {
                    changed_files.insert(source);
                }
                diff.added_edges.push(row);
            }
            (true, false) => {
                if let Some(source) = row.source_uri.clone() {
                    changed_files.insert(source);
                }
                diff.removed_edges.push(row);
            }
            _ => {}
        }
    }

    diff.added_nodes
        .sort_by(|a, b| a.node.qualified_name.cmp(&b.node.qualified_name));
    diff.removed_nodes
        .sort_by(|a, b| a.node.qualified_name.cmp(&b.node.qualified_name));
    diff.added_edges.sort_by(|a, b| {
        edge_identity(&a.src_qname, &a.dst_qname, a.edge.kind).cmp(&edge_identity(
            &b.src_qname,
            &b.dst_qname,
            b.edge.kind,
        ))
    });
    diff.removed_edges.sort_by(|a, b| {
        edge_identity(&a.src_qname, &a.dst_qname, a.edge.kind).cmp(&edge_identity(
            &b.src_qname,
            &b.dst_qname,
            b.edge.kind,
        ))
    });
    diff.changed_files = changed_files.into_iter().collect();
    diff.changed_files.sort();
    Ok(diff)
}

fn temporal_active_node_ids(graph: &Graph, diff: &StoreTemporalDiff) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let add_qname = |qname: &str, out: &mut Vec<NodeId>, seen: &mut HashSet<NodeId>| {
        if let Some(id) = graph.find_by_qname(qname) {
            if seen.insert(id) {
                out.push(id);
            }
        }
    };
    for row in diff.added_nodes.iter().chain(diff.removed_nodes.iter()) {
        add_qname(&row.node.qualified_name, &mut out, &mut seen);
    }
    for row in diff.added_edges.iter().chain(diff.removed_edges.iter()) {
        add_qname(&row.src_qname, &mut out, &mut seen);
        add_qname(&row.dst_qname, &mut out, &mut seen);
    }
    out
}

fn store_temporal_diff_json(diff: &StoreTemporalDiff, limit: usize) -> Value {
    json!({
        "added_nodes": diff.added_nodes.iter().take(limit).map(node_row_json).collect::<Vec<_>>(),
        "removed_nodes": diff.removed_nodes.iter().take(limit).map(node_row_json).collect::<Vec<_>>(),
        "added_edges": diff.added_edges.iter().take(limit).map(edge_row_json).collect::<Vec<_>>(),
        "removed_edges": diff.removed_edges.iter().take(limit).map(edge_row_json).collect::<Vec<_>>(),
    })
}

fn node_row_json(row: &StoredNodeRow) -> Value {
    json!({
        "qualified_name": row.node.qualified_name,
        "kind": row.node.kind,
        "source_uri": row.node.source_uri,
        "line_start": row.node.line_start,
        "line_end": row.node.line_end,
        "valid_from": row.node.valid_from,
        "valid_to": row.node.valid_to,
    })
}

fn edge_row_json(row: &StoredEdgeRow) -> Value {
    json!({
        "src": row.src_qname,
        "dst": row.dst_qname,
        "kind": row.edge.kind,
        "source_uri": row.source_uri,
        "valid_from": row.edge.valid_from,
        "valid_to": row.edge.valid_to,
    })
}

fn node_kind_counts_json(rows: &[StoredNodeRow]) -> Value {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let key = serde_json::to_value(row.node.kind)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| format!("{:?}", row.node.kind));
        *counts.entry(key).or_insert(0) += 1;
    }
    json!(counts)
}

fn edge_kind_counts_json(edges: &[StoredEdgeRow]) -> Value {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for edge in edges {
        let key = serde_json::to_value(edge.edge.kind)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| format!("{:?}", edge.edge.kind));
        *counts.entry(key).or_insert(0) += 1;
    }
    json!(counts)
}

fn git_commit_hash(rev: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", rev])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn git_is_ancestor(ancestor: &str, descendant: &str) -> bool {
    if ancestor == descendant {
        return true;
    }
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn review_context_json(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<Value> {
    let analysis = detect_changes_json(db, base, 2)?;
    let changed_files: Vec<String> = analysis["changed_files"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    let mut files = Vec::<ReviewContextFile>::new();
    for file in changed_files {
        upsert_review_context_file(&mut files, file, true, false);
    }
    for item in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(source) = item["source_uri"].as_str() {
            upsert_review_context_file(&mut files, source.to_string(), false, true);
        }
    }
    for file in &mut files {
        file.ranges = ranges_for_file_from_analysis(&analysis, &file.path);
        file.priority = review_context_priority(file);
    }
    files.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut used_tokens = 0usize;
    let mut snippets = Vec::new();
    let mut omitted = Vec::new();
    for file in files {
        if used_tokens >= token_budget {
            omitted.push(review_context_omission(&file, "token_budget_exhausted"));
            continue;
        }
        let line_limit = review_context_line_limit(max_lines_per_file, token_budget);
        if let Ok(snippet) =
            review_context_snippet_for_file(&file.path, &file.ranges, line_limit, token_budget)
        {
            let tokens = approx_tokens(&snippet);
            if used_tokens + tokens > token_budget {
                omitted.push(review_context_omission(&file, "token_budget"));
                continue;
            }
            used_tokens += tokens;
            snippets.push(json!({
                "path": file.path,
                "tokens": tokens,
                "changed": file.changed,
                "impacted": file.impacted,
                "priority": file.priority,
                "changed_ranges": file.ranges,
                "snippet": snippet
            }));
        } else {
            omitted.push(review_context_omission(&file, "unreadable"));
        }
    }
    Ok(json!({
        "operation": "review_context",
        "base": base,
        "token_budget": token_budget,
        "used_tokens": used_tokens,
        "context_strategy": "changed_hunks_and_source_files_first",
        "analysis": review_context_analysis_summary(&analysis),
        "snippets": snippets,
        "omitted": omitted,
    }))
}

fn review_context_analysis_summary(analysis: &Value) -> Value {
    let changed_files = analysis["changed_files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    json!({
        "base": analysis["base"].clone(),
        "risk": analysis["risk"].clone(),
        "risk_score": analysis["risk_score"].clone(),
        "mapping_precision": analysis["mapping_precision"].clone(),
        "changed_file_count": changed_files.len(),
        "changed_files_sample": changed_files.into_iter().take(20).collect::<Vec<_>>(),
        "changed_symbol_total": analysis["changed_symbol_total"].clone(),
        "impacted_total": analysis["impacted_total"].clone(),
        "affected_flow_count": analysis["affected_flows"]["total"].clone(),
        "test_coverage": {
            "covered_count": analysis["test_coverage"]["covered_count"].clone(),
            "missing_count": analysis["test_coverage"]["missing_count"].clone(),
            "total_symbols": analysis["test_coverage"]["total_symbols"].clone(),
            "coverage_ratio": analysis["test_coverage"]["coverage_ratio"].clone(),
        },
        "suggested_next_tools": analysis["suggested_next_tools"].clone(),
        "full_analysis_tool": "detect_changes",
    })
}

#[derive(Clone, Debug)]
struct ReviewContextFile {
    path: String,
    changed: bool,
    impacted: bool,
    ranges: Vec<(u32, u32)>,
    priority: i32,
}

fn upsert_review_context_file(
    files: &mut Vec<ReviewContextFile>,
    path: String,
    changed: bool,
    impacted: bool,
) {
    if let Some(existing) = files
        .iter_mut()
        .find(|file| source_matches(&file.path, &path) || source_matches(&path, &file.path))
    {
        existing.changed |= changed;
        existing.impacted |= impacted;
        if existing.path.len() < path.len() {
            existing.path = path;
        }
        return;
    }
    files.push(ReviewContextFile {
        path,
        changed,
        impacted,
        ranges: Vec::new(),
        priority: 0,
    });
}

fn review_context_priority(file: &ReviewContextFile) -> i32 {
    let mut score = 0;
    if file.changed {
        score += 40;
    }
    if file.impacted {
        score += 25;
    }
    if !file.ranges.is_empty() {
        score += 45;
    }
    if is_source_like_path(&file.path) {
        score += 35;
    }
    if is_test_like_path(&file.path) {
        score += 8;
    }
    if is_doc_like_path(&file.path) {
        score -= 10;
    }
    if is_low_signal_review_path(&file.path) {
        score -= 80;
    }
    score
}

fn review_context_omission(file: &ReviewContextFile, reason: &str) -> Value {
    json!({
        "path": file.path,
        "reason": reason,
        "changed": file.changed,
        "impacted": file.impacted,
        "priority": file.priority,
        "changed_ranges": file.ranges,
    })
}

fn review_context_line_limit(max_lines_per_file: usize, token_budget: usize) -> usize {
    let budget_based = (token_budget / 32).clamp(16, 80);
    max_lines_per_file.max(1).min(budget_based)
}

fn review_context_snippet_for_file(
    path: &str,
    ranges: &[(u32, u32)],
    line_limit: usize,
    token_budget: usize,
) -> Result<String> {
    let per_file_budget = review_context_per_file_budget(token_budget);
    let mut limit = line_limit.max(1);
    loop {
        let snippet = file_snippet_for_ranges(path, ranges, limit)?;
        if approx_tokens(&snippet) <= per_file_budget || limit <= 1 {
            return Ok(snippet);
        }
        limit = (limit / 2).max(1);
    }
}

fn review_context_per_file_budget(token_budget: usize) -> usize {
    if token_budget < 200 {
        token_budget.max(1)
    } else {
        (token_budget / 3).clamp(200, token_budget)
    }
}

fn traverse_json(
    graph: &Graph,
    seed: NodeId,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Value {
    let mut queue = VecDeque::from([(seed, 0usize)]);
    let mut seen = HashSet::from([seed]);
    let mut nodes = Vec::new();
    let mut used = 0usize;
    while let Some((id, depth)) = queue.pop_front() {
        if used >= token_budget {
            break;
        }
        if let Some(n) = graph.node(id) {
            let item = json!({
                "id": id.0,
                "depth": depth,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
                "in_degree": graph.in_neighbors(id).count(),
                "out_degree": graph.out_neighbors(id).count(),
            });
            used += approx_tokens(&item.to_string());
            nodes.push(item);
        }
        if depth >= max_depth {
            continue;
        }
        let mut neighbors = Vec::new();
        if direction == "out" || direction == "both" {
            neighbors.extend(graph.out_neighbors(id).map(|(n, _)| n));
        }
        if direction == "in" || direction == "both" {
            neighbors.extend(graph.in_neighbors(id).map(|(n, _)| n));
        }
        for next in neighbors {
            if seen.insert(next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    json!({ "operation": "traverse", "direction": direction, "used_tokens": used, "nodes": nodes })
}

fn large_functions_json(graph: &Graph, min_lines: u32, limit: usize) -> Value {
    let mut rows: Vec<_> = graph
        .nodes()
        .filter_map(|(id, n)| {
            if !matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Trait
            ) {
                return None;
            }
            let lines = n
                .line_start
                .zip(n.line_end)
                .map(|(s, e)| e.saturating_sub(s) + 1)?;
            (lines >= min_lines).then(|| {
                json!({
                    "id": id.0,
                    "lines": lines,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["lines"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "large_functions", "hits": rows })
}

fn bridge_nodes_json(graph: &Graph, limit: usize) -> Value {
    let communities = leiden(graph);
    let rows: Vec<_> = bridge_scores(graph, &communities, limit.saturating_mul(4).max(limit))
        .into_iter()
        .filter_map(|row| {
            let n = graph.node(row.node)?;
            if !is_rankable_node(n) {
                return None;
            }
            Some(json!({
                "id": row.node.0,
                "score": row.score,
                "communities_touched": row.communities_touched,
                "degree": row.degree,
                "approx_betweenness": row.approx_betweenness,
                "articulation": row.articulation,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
            }))
        })
        .take(limit)
        .collect();
    json!({ "operation": "bridge_nodes", "hits": rows })
}

fn cycles_json(graph: &Graph, limit: usize) -> Value {
    let mut cycles = cyclic_components(graph);
    cycles.sort_by_key(|c| std::cmp::Reverse(c.nodes.len()));
    let hits: Vec<_> = cycles
        .into_iter()
        .take(limit)
        .map(|component| {
            let nodes = component
                .nodes
                .into_iter()
                .filter_map(|id| {
                    graph.node(id).map(|n| {
                        json!({
                            "id": id.0,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                            "source_uri": n.source_uri,
                        })
                    })
                })
                .collect::<Vec<_>>();
            json!({ "size": nodes.len(), "nodes": nodes })
        })
        .collect();
    json!({ "operation": "cycles", "hits": hits })
}

fn core_json(graph: &Graph, limit: usize) -> Value {
    let core = core_numbers(graph);
    let mut rows: Vec<_> = core
        .into_iter()
        .filter_map(|(id, coreness)| {
            let n = graph.node(id)?;
            if !is_rankable_node(n) {
                return None;
            }
            Some(json!({
                "id": id.0,
                "core": coreness,
                "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
            }))
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["core"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "core", "hits": rows })
}

fn articulation_json(graph: &Graph, limit: usize) -> Value {
    let points = articulation_points(graph);
    let mut rows: Vec<_> = points
        .into_iter()
        .filter_map(|id| {
            let n = graph.node(id)?;
            if !is_rankable_node(n) {
                return None;
            }
            Some(json!({
                "id": id.0,
                "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
            }))
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["degree"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "articulation_points", "hits": rows })
}

fn gaps_json(graph: &Graph, limit: usize) -> Value {
    let mut rows = Vec::new();
    for (id, n) in graph.nodes() {
        let indeg = graph.in_neighbors(id).count();
        let outdeg = graph.out_neighbors(id).count();
        let lines = n
            .line_start
            .zip(n.line_end)
            .map(|(s, e)| e.saturating_sub(s) + 1)
            .unwrap_or(0);
        if matches!(n.kind, NodeKind::Function | NodeKind::Method)
            && indeg == 0
            && is_rankable_node(n)
        {
            rows.push(json!({"kind":"orphan_symbol","severity":"medium","qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if matches!(n.kind, NodeKind::Function | NodeKind::Method)
            && outdeg == 0
            && lines > 40
            && is_rankable_node(n)
        {
            rows.push(json!({"kind":"large_leaf","severity":"low","lines":lines,"qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if is_actionable_unresolved_call(n, indeg) {
            rows.push(json!({
                "kind":"unresolved_call",
                "severity": if indeg >= 5 { "high" } else { "medium" },
                "call":n.name,
                "incoming":indeg
            }));
        }
        if rows.len() >= limit {
            break;
        }
    }
    json!({ "operation": "gaps", "hits": rows })
}

fn surprises_json(graph: &Graph, limit: usize) -> Value {
    let communities = leiden(graph);
    let degrees: HashMap<NodeId, usize> = graph
        .nodes()
        .map(|(id, _)| {
            (
                id,
                graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
            )
        })
        .collect();
    let avg_degree = if degrees.is_empty() {
        0.0
    } else {
        degrees.values().sum::<usize>() as f64 / degrees.len() as f64
    };
    let hub_threshold = ((avg_degree * 3.0).ceil() as usize).max(12);

    let mut rows = Vec::new();
    for (edge_id, src, dst, edge) in graph.edges() {
        let Some(src_node) = graph.node(src) else {
            continue;
        };
        let Some(dst_node) = graph.node(dst) else {
            continue;
        };
        if src_node.qualified_name.starts_with("call::")
            || dst_node.qualified_name.starts_with("call::")
            || is_test_like_node(src_node)
            || is_test_like_node(dst_node)
            || (matches!(edge.kind, ariadne_graph::EdgeKind::Calls)
                && src_node.source_uri != dst_node.source_uri
                && is_generic_utility_name(&dst_node.name))
        {
            continue;
        }

        let src_comm = communities.get(&src).copied().unwrap_or(0);
        let dst_comm = communities.get(&dst).copied().unwrap_or(0);
        let src_degree = degrees.get(&src).copied().unwrap_or(0);
        let dst_degree = degrees.get(&dst).copied().unwrap_or(0);
        let src_language = source_language(src_node.source_uri.as_deref());
        let dst_language = source_language(dst_node.source_uri.as_deref());
        let cross_language =
            src_language.is_some() && dst_language.is_some() && src_language != dst_language;
        let src_category = source_category(src_node.source_uri.as_deref());
        let dst_category = source_category(dst_node.source_uri.as_deref());
        let code_doc_boundary = matches!(
            (src_category, dst_category),
            (Some("code"), Some("doc")) | (Some("doc"), Some("code"))
        );
        let placeholder_resolution = edge
            .properties
            .get("resolved_from")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("call_placeholder::"));
        if matches!(edge.kind, ariadne_graph::EdgeKind::Calls)
            && placeholder_resolution
            && (cross_language || code_doc_boundary || is_generic_utility_name(&dst_node.name))
        {
            continue;
        }
        let mut score = 0.0;
        let mut reasons = Vec::new();

        if src_comm != dst_comm {
            score += 4.0;
            reasons.push("cross_community");
        }
        if src_node.source_uri != dst_node.source_uri
            && src_node.source_uri.is_some()
            && dst_node.source_uri.is_some()
        {
            score += 1.0;
            reasons.push("cross_file");
        }
        if cross_language {
            score += 3.0;
            reasons.push("cross_language");
        }
        if src_degree <= 2 && dst_degree >= hub_threshold {
            score += 3.0;
            reasons.push("peripheral_to_hub");
        }
        if matches!(edge.confidence, ariadne_graph::Confidence::Ambiguous) {
            score += 3.0;
            reasons.push("ambiguous_edge");
        } else if edge.confidence.class_str() == "inferred" {
            score += (1.0 - edge.confidence.score() as f64).max(0.0) * 2.0 + 1.0;
            reasons.push("inferred_edge");
        }

        if reasons.is_empty() {
            continue;
        }

        rows.push(json!({
            "id": edge_id.0,
            "score": score,
            "reasons": reasons,
            "edge_kind": edge.kind,
            "confidence": edge.confidence.score(),
            "confidence_class": edge.confidence.class_str(),
            "source": node_ref_json(src, src_node, src_comm, src_degree),
            "target": node_ref_json(dst, dst_node, dst_comm, dst_degree),
        }));
    }
    rows.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = rows.len();
    rows.truncate(limit);
    json!({
        "operation": "surprises",
        "summary": {
            "total": total,
            "returned": rows.len(),
            "hub_threshold": hub_threshold,
        },
        "hits": rows,
        "suggested_next_tools": ["traverse", "impact", "review_context", "architecture_overview"]
    })
}

fn diagnostics_json(
    graph: &Graph,
    fts_count: usize,
    embedding_count: usize,
    embedding_model: Option<String>,
    limit: usize,
) -> Value {
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    let mut missing_source_nodes = Vec::new();
    let mut unresolved_calls = Vec::new();
    let mut isolated_rankable = 0usize;

    for (id, node) in graph.nodes() {
        *kind_counts.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
        if let Some(source) = &node.source_uri {
            *source_counts.entry(source.clone()).or_insert(0) += 1;
            if missing_source_nodes.len() < limit && !Path::new(source).exists() {
                missing_source_nodes.push(json!({
                    "id": id.0,
                    "qualified_name": node.qualified_name,
                    "source_uri": source,
                }));
            }
        }
        if node.qualified_name.starts_with("call::") {
            let incoming = graph.in_neighbors(id).count();
            if incoming > 0 {
                unresolved_calls.push((
                    incoming,
                    id,
                    node.qualified_name.clone(),
                    node.name.clone(),
                ));
            }
        }
        if is_rankable_node(node)
            && graph.in_neighbors(id).count() + graph.out_neighbors(id).count() == 0
        {
            isolated_rankable += 1;
        }
    }

    let mut confidence_counts: HashMap<String, usize> = HashMap::new();
    let mut edge_kind_counts: HashMap<String, usize> = HashMap::new();
    let mut pair_counts: HashMap<(NodeId, NodeId, String), usize> = HashMap::new();
    let mut self_loop_edges = 0usize;
    for (_, src, dst, edge) in graph.edges() {
        *confidence_counts
            .entry(edge.confidence.class_str().to_string())
            .or_insert(0) += 1;
        *edge_kind_counts
            .entry(format!("{:?}", edge.kind))
            .or_insert(0) += 1;
        *pair_counts
            .entry((src, dst, format!("{:?}", edge.kind)))
            .or_insert(0) += 1;
        if src == dst {
            self_loop_edges += 1;
        }
    }

    unresolved_calls.sort_by_key(|(incoming, _, _, _)| std::cmp::Reverse(*incoming));
    let unresolved_call_hits: Vec<_> = unresolved_calls
        .iter()
        .take(limit)
        .map(|(incoming, id, qualified_name, name)| {
            json!({
                "id": id.0,
                "call": name,
                "qualified_name": qualified_name,
                "incoming": incoming,
                "actionable": graph.node(*id).is_some_and(|n| is_actionable_unresolved_call(n, *incoming)),
            })
        })
        .collect();

    let duplicate_endpoint_groups = pair_counts.values().filter(|&&count| count > 1).count();
    let duplicate_endpoint_edges: usize = pair_counts
        .values()
        .filter(|&&count| count > 1)
        .map(|count| count - 1)
        .sum();

    let mut top_sources: Vec<_> = source_counts.into_iter().collect();
    top_sources.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let top_sources: Vec<_> = top_sources
        .into_iter()
        .take(limit)
        .map(|(source, nodes)| json!({ "source": source, "nodes": nodes }))
        .collect();

    let mut kind_counts: Vec<_> = kind_counts.into_iter().collect();
    kind_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let kind_counts: Vec<_> = kind_counts
        .into_iter()
        .map(|(kind, count)| json!({ "kind": kind, "count": count }))
        .collect();

    let mut edge_kind_counts: Vec<_> = edge_kind_counts.into_iter().collect();
    edge_kind_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let edge_kind_counts: Vec<_> = edge_kind_counts
        .into_iter()
        .map(|(kind, count)| json!({ "kind": kind, "count": count }))
        .collect();

    let node_count = graph.node_count();
    let edge_count = graph.edge_count();
    let fts_coverage = ratio(fts_count, node_count);
    let embedding_coverage = ratio(embedding_count, node_count);
    let unresolved_count = unresolved_calls.len();
    let ambiguous_edges = *confidence_counts.get("ambiguous").unwrap_or(&0);
    let inferred_edges = *confidence_counts.get("inferred").unwrap_or(&0);

    let mut warnings = Vec::new();
    if fts_count < node_count {
        warnings.push(json!({
            "kind": "fts_incomplete",
            "severity": if fts_coverage < 0.8 { "medium" } else { "low" },
            "message": "FTS index has fewer rows than graph nodes; run rebuild_fts.",
        }));
    }
    if embedding_count > 0 && embedding_count < node_count {
        warnings.push(json!({
            "kind": "embedding_incomplete",
            "severity": if embedding_coverage < 0.8 { "medium" } else { "low" },
            "message": "Semantic embeddings cover only part of the graph; rerun embed.",
        }));
    }
    if embedding_model
        .as_deref()
        .is_some_and(|model| model != DEFAULT_EMBEDDING_MODEL)
    {
        warnings.push(json!({
            "kind": "embedding_model_stale",
            "severity": "medium",
            "message": format!("Embeddings use an older model; rerun embed to rebuild with {}.", DEFAULT_EMBEDDING_MODEL),
        }));
    }
    if unresolved_count > node_count / 10 {
        warnings.push(json!({
            "kind": "many_unresolved_calls",
            "severity": "medium",
            "message": "A large share of nodes are unresolved call placeholders.",
        }));
    }
    if ambiguous_edges > edge_count / 5 && edge_count > 0 {
        warnings.push(json!({
            "kind": "high_ambiguity",
            "severity": "medium",
            "message": "More than 20% of edges are ambiguous.",
        }));
    }
    if duplicate_endpoint_groups > 0 {
        warnings.push(json!({
            "kind": "duplicate_edge_endpoints",
            "severity": "low",
            "message": "Multiple edges share the same source, target, and kind.",
        }));
    }

    json!({
        "operation": "diagnostics",
        "summary": {
            "nodes": node_count,
            "edges": edge_count,
            "rankable_isolated_nodes": isolated_rankable,
            "unresolved_call_nodes": unresolved_count,
            "self_loop_edges": self_loop_edges,
            "duplicate_endpoint_groups": duplicate_endpoint_groups,
            "duplicate_endpoint_extra_edges": duplicate_endpoint_edges,
        },
        "confidence": {
            "counts": confidence_counts,
            "ambiguous_edges": ambiguous_edges,
            "inferred_edges": inferred_edges,
        },
        "indexes": {
            "fts5": {
                "indexed_nodes": fts_count,
                "coverage": fts_coverage,
            },
            "embeddings": {
                "indexed_nodes": embedding_count,
                "coverage": embedding_coverage,
                "model": embedding_model,
            },
        },
        "node_kinds": kind_counts,
        "edge_kinds": edge_kind_counts,
        "top_sources": top_sources,
        "unresolved_calls": unresolved_call_hits,
        "missing_source_nodes": missing_source_nodes,
        "warnings": warnings,
        "suggested_next_tools": ["gaps", "surprises", "bridge_nodes", "rebuild_fts", "embed"]
    })
}

fn graph_report_markdown(
    graph: &Graph,
    fts_count: usize,
    embedding_count: usize,
    embedding_model: Option<String>,
    top: usize,
) -> String {
    let diagnostics = diagnostics_json(graph, fts_count, embedding_count, embedding_model, top);
    let bridges = bridge_nodes_json(graph, top);
    let gaps = gaps_json(graph, top);
    let surprises = surprises_json(graph, top);
    let questions = suggested_questions_json(&json!({}), top);
    let god_nodes = god_nodes_report_rows(graph, top);

    let mut out = String::new();
    let _ = writeln!(&mut out, "# Ariadne Graph Report");
    let _ = writeln!(&mut out);
    if let Ok(Some(commit)) = git_commit_hash("HEAD") {
        let _ = writeln!(&mut out, "- Git commit: `{}`", commit);
    }
    let _ = writeln!(
        &mut out,
        "- Nodes: {}",
        diagnostics["summary"]["nodes"].as_u64().unwrap_or_default()
    );
    let _ = writeln!(
        &mut out,
        "- Edges: {}",
        diagnostics["summary"]["edges"].as_u64().unwrap_or_default()
    );
    let _ = writeln!(
        &mut out,
        "- FTS coverage: {:.1}%",
        diagnostics["indexes"]["fts5"]["coverage"]
            .as_f64()
            .unwrap_or_default()
            * 100.0
    );
    let _ = writeln!(
        &mut out,
        "- Embedding coverage: {:.1}%",
        diagnostics["indexes"]["embeddings"]["coverage"]
            .as_f64()
            .unwrap_or_default()
            * 100.0
    );

    append_warning_section(&mut out, &diagnostics);
    append_report_hits(&mut out, "God Nodes", &god_nodes, |row| {
        format!(
            "`{}` ({}, score {:.4})",
            row["qualified_name"].as_str().unwrap_or(""),
            row["kind"].as_str().unwrap_or("unknown"),
            row["score"].as_f64().unwrap_or_default()
        )
    });
    append_report_hits(&mut out, "Bridge Nodes", &bridges["hits"], |row| {
        format!(
            "`{}` (score {:.2}, degree {})",
            row["qualified_name"].as_str().unwrap_or(""),
            row["score"].as_f64().unwrap_or_default(),
            row["degree"].as_u64().unwrap_or_default()
        )
    });
    append_report_hits(&mut out, "Surprising Edges", &surprises["hits"], |row| {
        format!(
            "`{}` -> `{}` ({})",
            row["source"]["qualified_name"].as_str().unwrap_or(""),
            row["target"]["qualified_name"].as_str().unwrap_or(""),
            row["reasons"]
                .as_array()
                .map(|reasons| {
                    reasons
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        )
    });
    append_report_hits(&mut out, "Knowledge Gaps", &gaps["hits"], |row| {
        format!(
            "{}: `{}`",
            row["kind"].as_str().unwrap_or("gap"),
            row["qualified_name"]
                .as_str()
                .or_else(|| row["call"].as_str())
                .unwrap_or("")
        )
    });
    append_report_hits(
        &mut out,
        "Suggested Questions",
        &questions["questions"],
        |row| row.as_str().unwrap_or("").to_string(),
    );

    out
}

fn god_nodes_report_rows(graph: &Graph, limit: usize) -> Value {
    let ranks = pagerank(graph, 0.85, 50);
    let mut sorted: Vec<_> = ranks.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let hits: Vec<_> = sorted
        .into_iter()
        .filter_map(|(id, score)| {
            let node = graph.node(id)?;
            is_rankable_node(node).then(|| {
                json!({
                    "id": id.0,
                    "score": score,
                    "qualified_name": node.qualified_name,
                    "kind": format!("{:?}", node.kind),
                    "source_uri": node.source_uri,
                })
            })
        })
        .take(limit)
        .collect();
    Value::Array(hits)
}

fn append_warning_section(out: &mut String, diagnostics: &Value) {
    let _ = writeln!(out);
    let _ = writeln!(out, "## Graph Health");
    if let Some(warnings) = diagnostics["warnings"].as_array() {
        if warnings.is_empty() {
            let _ = writeln!(out, "- No health warnings.");
            return;
        }
        for warning in warnings {
            let _ = writeln!(
                out,
                "- **{}**: {}",
                warning["severity"].as_str().unwrap_or("info"),
                warning["message"].as_str().unwrap_or("")
            );
        }
    }
}

fn append_report_hits<F>(out: &mut String, title: &str, hits: &Value, render: F)
where
    F: Fn(&Value) -> String,
{
    let _ = writeln!(out);
    let _ = writeln!(out, "## {}", title);
    let Some(rows) = hits.as_array() else {
        let _ = writeln!(out, "- None.");
        return;
    };
    if rows.is_empty() {
        let _ = writeln!(out, "- None.");
        return;
    }
    for row in rows {
        let rendered = render(row);
        if !rendered.trim().is_empty() {
            let _ = writeln!(out, "- {}", rendered);
        }
    }
}

fn ratio(part: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        part as f64 / total as f64
    }
}

fn node_ref_json(id: NodeId, node: &ariadne_graph::Node, community: usize, degree: usize) -> Value {
    json!({
        "id": id.0,
        "qualified_name": node.qualified_name,
        "kind": node.kind,
        "source_uri": node.source_uri,
        "community": community,
        "degree": degree,
    })
}

fn suggested_questions_json(analysis: &Value, limit: usize) -> Value {
    let mut questions = Vec::new();
    for file in analysis["changed_files"].as_array().unwrap_or(&Vec::new()) {
        if let Some(file) = file.as_str() {
            questions.push(format!(
                "What behavior changed in {} and is it covered by tests?",
                file
            ));
        }
    }
    for hit in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(name) = hit["qualified_name"].as_str() {
            questions.push(format!(
                "Does the change alter assumptions relied on by {}?",
                name
            ));
        }
    }
    questions
        .push("Are any unresolved calls or large functions involved in this change?".to_string());
    questions.truncate(limit);
    json!({ "operation": "suggested_questions", "questions": questions })
}

#[derive(Debug, Clone, Default)]
struct ChangedFile {
    path: String,
    hunks: Vec<ChangedHunk>,
}

#[derive(Debug, Clone)]
struct ChangedHunk {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

impl ChangedHunk {
    fn new_end(&self) -> u32 {
        if self.new_count == 0 {
            self.new_start
        } else {
            self.new_start + self.new_count.saturating_sub(1)
        }
    }

    fn overlaps_node(&self, line_start: u32, line_end: u32) -> bool {
        let hunk_start = self.new_start.max(1);
        let hunk_end = self.new_end().max(hunk_start);
        line_start <= hunk_end && line_end >= hunk_start
    }
}

fn git_changed_diff(base: &str) -> Result<Vec<ChangedFile>> {
    let output = Command::new("git")
        .args(["diff", "--unified=0", "--no-ext-diff", base, "--"])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_git_diff_hunks(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_git_diff_hunks(diff: &str) -> Vec<ChangedFile> {
    let mut files = Vec::<ChangedFile>::new();
    let mut current: Option<ChangedFile> = None;

    for line in diff.lines() {
        if let Some(path) = parse_diff_git_path(line) {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(ChangedFile {
                path,
                hunks: Vec::new(),
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(file) = current.as_mut() {
                if rest != "/dev/null" {
                    file.path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("@@ ") {
            if let (Some(file), Some(hunk)) = (current.as_mut(), parse_hunk_header(rest)) {
                file.hunks.push(hunk);
            }
        }
    }

    if let Some(file) = current {
        files.push(file);
    }

    files
        .into_iter()
        .filter(|file| !file.path.is_empty())
        .collect()
}

fn parse_diff_git_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let mut parts = rest.split_whitespace();
    let _old = parts.next()?;
    let new = parts.next()?;
    Some(new.strip_prefix("b/").unwrap_or(new).to_string())
}

fn parse_hunk_header(rest: &str) -> Option<ChangedHunk> {
    let end = rest.find(" @@")?;
    let header = &rest[..end];
    let mut parts = header.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_count) = parse_hunk_range(old)?;
    let (new_start, new_count) = parse_hunk_range(new)?;
    Some(ChangedHunk {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

fn parse_hunk_range(range: &str) -> Option<(u32, u32)> {
    let mut parts = range.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let count = parts.next().map(|s| s.parse().ok()).unwrap_or(Some(1))?;
    Some((start, count))
}

fn changed_ranges_json(graph: &Graph, diff: &[ChangedFile]) -> Vec<Value> {
    diff.iter()
        .map(|file| {
            json!({
                "path": file.path,
                "hunks": file.hunks.iter().map(|hunk| {
                    json!({
                        "old_start": hunk.old_start,
                        "old_count": hunk.old_count,
                        "new_start": hunk.new_start,
                        "new_count": hunk.new_count,
                        "new_end": hunk.new_end(),
                        "symbols": nodes_json(
                            graph,
                            &nodes_for_changed_hunk(graph, &file.path, hunk),
                            20
                        ),
                    })
                }).collect::<Vec<_>>()
            })
        })
        .collect()
}

fn nodes_for_changed_ranges(graph: &Graph, diff: &[ChangedFile]) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    for file in diff {
        for hunk in &file.hunks {
            append_unique_nodes(&mut nodes, nodes_for_changed_hunk(graph, &file.path, hunk));
        }
    }
    nodes
}

fn nodes_for_changed_hunk(graph: &Graph, path: &str, hunk: &ChangedHunk) -> Vec<NodeId> {
    let mut scored = Vec::<(u8, u32, NodeId)>::new();
    for (id, node) in graph.nodes() {
        let Some(source) = node.source_uri.as_ref() else {
            continue;
        };
        if !source_matches(source, path) {
            continue;
        }
        let Some((line_start, line_end)) = normalized_node_span(node.line_start, node.line_end)
        else {
            continue;
        };
        if !hunk.overlaps_node(line_start, line_end) {
            continue;
        }
        scored.push((
            node_kind_specificity(node.kind),
            line_end.saturating_sub(line_start),
            id,
        ));
    }
    if scored.iter().any(|(_, _, id)| {
        graph
            .node(*id)
            .map(|node| !matches!(node.kind, NodeKind::File | NodeKind::Document))
            .unwrap_or(false)
    }) {
        scored.retain(|(_, _, id)| {
            graph
                .node(*id)
                .map(|node| !matches!(node.kind, NodeKind::File | NodeKind::Document))
                .unwrap_or(false)
        });
    }
    scored.sort_by_key(|(specificity, span, id)| (std::cmp::Reverse(*specificity), *span, id.0));
    scored.into_iter().map(|(_, _, id)| id).collect()
}

fn nodes_for_files(graph: &Graph, files: &[String]) -> Vec<NodeId> {
    graph
        .nodes()
        .filter(|(_, n)| {
            n.source_uri
                .as_ref()
                .map(|src| files.iter().any(|f| source_matches(src, f)))
                .unwrap_or(false)
        })
        .map(|(id, _)| id)
        .collect()
}

fn source_matches(source: &str, path: &str) -> bool {
    source == path || source.ends_with(path) || path.ends_with(source)
}

fn node_kind_specificity(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Function | NodeKind::Method => 5,
        NodeKind::Class | NodeKind::Trait | NodeKind::Impl => 4,
        NodeKind::Module | NodeKind::Type => 3,
        NodeKind::File | NodeKind::Document => 1,
        _ => 2,
    }
}

fn normalized_node_span(line_start: Option<u32>, line_end: Option<u32>) -> Option<(u32, u32)> {
    let (start, end) = line_start.zip(line_end)?;
    if start == 0 {
        Some((1, end.saturating_add(1).max(1)))
    } else {
        Some((start, end.max(start)))
    }
}

fn nodes_json(graph: &Graph, ids: &[NodeId], limit: usize) -> Vec<Value> {
    ids.iter()
        .take(limit)
        .filter_map(|id| {
            graph.node(*id).map(|n| {
                json!({
                    "id": id.0,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                    "line_start": n.line_start,
                    "line_end": n.line_end,
                })
            })
        })
        .collect()
}

/// Audit each changed symbol for test coverage via incoming `TestedBy`
/// edges. Returns a summary suitable for surfacing in `review_context`.
///
/// We only consider Function/Method nodes — flagging an uncovered File
/// or Module would be noise. Tests themselves are skipped: changing a
/// test never needs another test.
fn test_coverage_json(graph: &Graph, changed_nodes: &[NodeId]) -> Value {
    let is_callable =
        |n: &ariadne_graph::Node| matches!(n.kind, NodeKind::Function | NodeKind::Method);
    let is_test_node = |n: &ariadne_graph::Node| {
        n.properties
            .get("is_test")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for &id in changed_nodes {
        let Some(node) = graph.node(id) else { continue };
        if !is_callable(node) || is_test_node(node) {
            continue;
        }
        // Edge convention: `production -[TestedBy]-> test`. So all
        // tests covering `id` are in its outgoing TestedBy neighbours.
        let tests: Vec<Value> = graph
            .out_neighbors(id)
            .filter(|(_, edge)| edge.kind == ariadne_graph::EdgeKind::TestedBy)
            .filter_map(|(test_id, _)| {
                graph.node(test_id).map(|t| {
                    json!({
                        "id": test_id.0,
                        "qualified_name": t.qualified_name,
                        "source_uri": t.source_uri,
                    })
                })
            })
            .collect();
        let entry = json!({
            "id": id.0,
            "qualified_name": node.qualified_name,
            "kind": node.kind,
            "source_uri": node.source_uri,
            "tests": tests,
        });
        if tests.is_empty() {
            let mut missing_entry = entry;
            missing_entry["nearby_tests"] = Value::Array(nearby_tests_json(graph, id, 6));
            missing.push(missing_entry);
        } else {
            covered.push(entry);
        }
    }
    let covered_count = covered.len();
    let missing_count = missing.len();
    let total = covered_count + missing_count;
    json!({
        "covered": covered,
        "covered_count": covered_count,
        "missing": missing,
        "missing_count": missing_count,
        "total_symbols": total,
        "coverage_ratio": if total == 0 {
            1.0
        } else {
            covered_count as f64 / total as f64
        },
    })
}

fn nearby_tests_json(graph: &Graph, id: NodeId, limit: usize) -> Vec<Value> {
    let mut seen_neighbors = HashSet::new();
    let mut neighbors = Vec::new();
    for (neighbor, edge) in graph.out_neighbors(id) {
        if edge.kind == ariadne_graph::EdgeKind::Calls && seen_neighbors.insert(neighbor) {
            neighbors.push(neighbor);
        }
    }
    for (neighbor, edge) in graph.in_neighbors(id) {
        if edge.kind == ariadne_graph::EdgeKind::Calls && seen_neighbors.insert(neighbor) {
            neighbors.push(neighbor);
        }
    }

    let mut seen_tests = HashSet::new();
    let mut out = Vec::new();
    for neighbor in neighbors {
        let via = graph
            .node(neighbor)
            .map(|node| node.qualified_name.clone())
            .unwrap_or_else(|| format!("node:{}", neighbor.0));
        for (test_id, edge) in graph.out_neighbors(neighbor) {
            if edge.kind != ariadne_graph::EdgeKind::TestedBy || !seen_tests.insert(test_id) {
                continue;
            }
            if let Some(test) = graph.node(test_id) {
                out.push(json!({
                    "id": test_id.0,
                    "qualified_name": test.qualified_name,
                    "source_uri": test.source_uri,
                    "via": via,
                }));
            }
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// Flows touched by the changed symbols. Ranked by criticality
/// descending. Each entry includes the entry name and the flow's
/// criticality so reviewers can quickly identify the riskiest paths the
/// change sits on.
fn affected_flows_json(graph: &Graph, changed_nodes: &[NodeId], limit: usize) -> Value {
    let flow_ids = ariadne_graph::extract::flows::affected_flows(graph, changed_nodes);
    let total = flow_ids.len();
    let hits: Vec<Value> = flow_ids
        .into_iter()
        .take(limit)
        .filter_map(|flow_id| {
            let node = graph.node(flow_id)?;
            Some(json!({
                "id": flow_id.0,
                "qualified_name": node.qualified_name,
                "entry_name": node.properties.get("entry_name"),
                "entry_qualified_name": node.properties.get("entry_qualified_name"),
                "criticality": node.properties.get("criticality"),
                "node_count": node.properties.get("node_count"),
                "depth": node.properties.get("depth"),
                "is_test_flow": node.properties.get("is_test_flow"),
            }))
        })
        .collect();
    json!({
        "hits": hits,
        "total": total,
        "truncated": total > limit,
    })
}

fn risk_score(
    graph: &Graph,
    changed_nodes: &[NodeId],
    impacted_count: usize,
    missing_tests: usize,
    top_flow_criticality: f64,
) -> f64 {
    let degree: usize = changed_nodes
        .iter()
        .map(|id| graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count())
        .sum();
    // top_flow_criticality is in [0, 1]; scale to a 0-15 contribution
    // so a maxed-out flow tilts risk meaningfully but never dominates.
    let flow_bump = top_flow_criticality.clamp(0.0, 1.0) * 15.0;
    ((changed_nodes.len() as f64 * 0.8)
        + (impacted_count as f64 * 1.2)
        + (degree as f64 * 0.25)
        + (missing_tests as f64 * 2.5)
        + flow_bump)
        .min(100.0)
}

fn risk_label(score: f64) -> &'static str {
    if score >= 35.0 {
        "high"
    } else if score >= 12.0 {
        "medium"
    } else {
        "low"
    }
}

fn file_snippet(path: &str, max_lines: usize) -> Result<String> {
    let content = fs::read_to_string(path)?;
    Ok(content
        .lines()
        .take(max_lines)
        .enumerate()
        .map(|(i, line)| format!("{:>4}: {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn file_snippet_for_ranges(path: &str, ranges: &[(u32, u32)], max_lines: usize) -> Result<String> {
    if ranges.is_empty() {
        return file_snippet(path, max_lines);
    }

    let content = fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(String::new());
    }

    let mut windows = Vec::<(usize, usize)>::new();
    let context = 4usize;
    for (start, end) in ranges {
        let range_start = (*start).max(1) as usize;
        let range_end = (*end).max(*start).max(1) as usize;
        let from = range_start.saturating_sub(context + 1);
        let to = (range_end + context).min(lines.len());
        if from < to {
            windows.push((from, to));
        }
    }
    windows.sort_unstable();

    let mut merged = Vec::<(usize, usize)>::new();
    for (from, to) in windows {
        if let Some((_, last_to)) = merged.last_mut() {
            if from <= *last_to + 1 {
                *last_to = (*last_to).max(to);
                continue;
            }
        }
        merged.push((from, to));
    }

    let mut emitted = 0usize;
    let mut out = Vec::new();
    for (idx, (from, to)) in merged.into_iter().enumerate() {
        if emitted >= max_lines {
            break;
        }
        if idx > 0 {
            out.push("   ...".to_string());
        }
        for line_idx in from..to {
            if emitted >= max_lines {
                break;
            }
            emitted += 1;
            out.push(format!("{:>4}: {}", line_idx + 1, lines[line_idx]));
        }
    }
    Ok(out.join("\n"))
}

fn ranges_for_file_from_analysis(analysis: &Value, file: &str) -> Vec<(u32, u32)> {
    analysis["changed_ranges"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|entry| {
            entry["path"]
                .as_str()
                .map(|path| source_matches(path, file))
                .unwrap_or(false)
        })
        .flat_map(|entry| entry["hunks"].as_array().into_iter().flatten())
        .filter_map(|hunk| {
            let start = hunk["new_start"].as_u64()? as u32;
            let end = hunk["new_end"].as_u64()? as u32;
            Some((start.max(1), end.max(start).max(1)))
        })
        .collect()
}

fn approx_tokens(s: &str) -> usize {
    (s.len() / 4).max(1)
}

fn install_agents_md(repo: &Path, db: &Path) -> Result<()> {
    let path = repo.join("AGENTS.md");
    let block = format!(
        r#"# Ariadne Agent Instructions

- Start exploration with `ariadne --db {} tool minimal_context --params '{{"target":"...","mode":"review"}}'`.
- For code review, run `ariadne --db {} tool detect_changes --params '{{"base":"HEAD~1"}}'` before reading files.
- Use `impact`, `traverse`, and `review_context` to gather bounded context before broad grep/read.
- Use `gaps`, `bridge_nodes`, and `large_functions` to find risky areas and review questions.
- Fall back to direct file reads only after Ariadne identifies the relevant files or symbols.
"#,
        db.display(),
        db.display()
    );
    fs::write(&path, block)?;
    println!("installed {}", path.display());
    Ok(())
}

fn install_mcp_config(repo: &Path, db: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let claude_path = repo.join(".mcp.json");
    fs::write(
        &claude_path,
        serde_json::to_string_pretty(&mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", claude_path.display());

    let cursor_dir = repo.join(".cursor");
    fs::create_dir_all(&cursor_dir)?;
    let cursor_path = cursor_dir.join("mcp.json");
    fs::write(
        &cursor_path,
        serde_json::to_string_pretty(&mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", cursor_path.display());

    let vscode_dir = repo.join(".vscode");
    fs::create_dir_all(&vscode_dir)?;
    let vscode_path = vscode_dir.join("mcp.json");
    fs::write(
        &vscode_path,
        serde_json::to_string_pretty(&vscode_mcp_config(&exe, db))?,
    )?;
    println!("installed {}", vscode_path.display());

    let codex_dir = repo.join(".codex");
    let codex_path = if codex_dir.exists() && !codex_dir.is_dir() {
        repo.join(".codex-ariadne-mcp.toml")
    } else {
        fs::create_dir_all(&codex_dir)?;
        codex_dir.join("ariadne-mcp.toml")
    };
    fs::write(&codex_path, codex_mcp_toml(&exe, db))?;
    println!("installed {}", codex_path.display());
    Ok(())
}

fn mcp_servers_config(exe: &Path, db: &Path) -> Value {
    json!({
        "mcpServers": {
            "ariadne": ariadne_stdio_server_config(exe, db)
        }
    })
}

fn vscode_mcp_config(exe: &Path, db: &Path) -> Value {
    json!({
        "servers": {
            "ariadne": {
                "type": "stdio",
                "command": exe,
                "args": ["--db", db, "mcp-server"]
            }
        }
    })
}

fn ariadne_stdio_server_config(exe: &Path, db: &Path) -> Value {
    json!({
        "type": "stdio",
        "command": exe,
        "args": ["--db", db, "mcp-server"]
    })
}

fn codex_mcp_toml(exe: &Path, db: &Path) -> String {
    format!(
        r#"# Ariadne MCP server for Codex.
# Add this table to ~/.codex/config.toml if your Codex build does not load project-local snippets.

[mcp_servers.ariadne]
command = {}
args = ["--db", {}, "mcp-server"]
"#,
        toml_string(&exe.display().to_string()),
        toml_string(&db.display().to_string())
    )
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn export_graph(graph: &Graph, format: &str, output: &Path) -> Result<Value> {
    match format {
        "graphml" => write_single_export(output, &graphml_export(graph), format),
        "cypher" | "neo4j" => write_single_export(output, &cypher_export(graph), "cypher"),
        "obsidian" => write_obsidian_export(graph, output),
        other => bail!(
            "unknown export format {}; use graphml, cypher, or obsidian",
            other
        ),
    }
}

fn write_single_export(output: &Path, content: &str, format: &str) -> Result<Value> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(output, content)?;
    Ok(json!({
        "operation": "export",
        "format": format,
        "output": output,
        "files_written": 1,
        "bytes": content.len(),
    }))
}

fn graphml_export(graph: &Graph) -> String {
    let mut out = String::new();
    let _ = writeln!(&mut out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        &mut out,
        r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns">"#
    );
    let _ = writeln!(
        &mut out,
        r#"<key id="qname" for="node" attr.name="qualified_name" attr.type="string"/>"#
    );
    let _ = writeln!(
        &mut out,
        r#"<key id="kind" for="all" attr.name="kind" attr.type="string"/>"#
    );
    let _ = writeln!(
        &mut out,
        r#"<key id="source" for="node" attr.name="source_uri" attr.type="string"/>"#
    );
    let _ = writeln!(
        &mut out,
        r#"<key id="confidence" for="edge" attr.name="confidence" attr.type="double"/>"#
    );
    let _ = writeln!(&mut out, r#"<graph id="ariadne" edgedefault="directed">"#);
    for (id, node) in graph.nodes() {
        let _ = writeln!(&mut out, r#"<node id="n{}">"#, id.0);
        let _ = writeln!(
            &mut out,
            r#"<data key="qname">{}</data>"#,
            xml_escape(&node.qualified_name)
        );
        let _ = writeln!(&mut out, r#"<data key="kind">{:?}</data>"#, node.kind);
        if let Some(source) = &node.source_uri {
            let _ = writeln!(
                &mut out,
                r#"<data key="source">{}</data>"#,
                xml_escape(source)
            );
        }
        let _ = writeln!(&mut out, "</node>");
    }
    for (id, src, dst, edge) in graph.edges() {
        let _ = writeln!(
            &mut out,
            r#"<edge id="e{}" source="n{}" target="n{}">"#,
            id.0, src.0, dst.0
        );
        let _ = writeln!(&mut out, r#"<data key="kind">{:?}</data>"#, edge.kind);
        let _ = writeln!(
            &mut out,
            r#"<data key="confidence">{}</data>"#,
            edge.confidence.score()
        );
        let _ = writeln!(&mut out, "</edge>");
    }
    let _ = writeln!(&mut out, "</graph>");
    let _ = writeln!(&mut out, "</graphml>");
    out
}

fn cypher_export(graph: &Graph) -> String {
    let mut out = String::new();
    let _ = writeln!(&mut out, "CREATE CONSTRAINT ariadne_node_id IF NOT EXISTS FOR (n:AriadneNode) REQUIRE n.id IS UNIQUE;");
    for (id, node) in graph.nodes() {
        let _ = writeln!(
            &mut out,
            "MERGE (n:AriadneNode {{id: {}}}) SET n.qualified_name = {}, n.name = {}, n.kind = {}, n.source_uri = {};",
            id.0,
            cypher_string(&node.qualified_name),
            cypher_string(&node.name),
            cypher_string(&format!("{:?}", node.kind)),
            cypher_opt_string(node.source_uri.as_deref()),
        );
    }
    for (_, src, dst, edge) in graph.edges() {
        let rel = cypher_rel_type(&format!("{:?}", edge.kind));
        let _ = writeln!(
            &mut out,
            "MATCH (a:AriadneNode {{id: {}}}), (b:AriadneNode {{id: {}}}) MERGE (a)-[:{} {{kind: {}, confidence: {}, confidence_class: {}}}]->(b);",
            src.0,
            dst.0,
            rel,
            cypher_string(&format!("{:?}", edge.kind)),
            edge.confidence.score(),
            cypher_string(edge.confidence.class_str()),
        );
    }
    out
}

fn write_obsidian_export(graph: &Graph, output: &Path) -> Result<Value> {
    fs::create_dir_all(output)?;
    let mut filenames = HashMap::new();
    for (id, node) in graph.nodes() {
        filenames.insert(id, format!("{}-{}.md", id.0, slugify(&node.name)));
    }
    let mut written = 0usize;
    for (id, node) in graph.nodes() {
        let mut page = String::new();
        let _ = writeln!(&mut page, "# {}", node.name);
        let _ = writeln!(&mut page);
        let _ = writeln!(&mut page, "- Kind: `{:?}`", node.kind);
        let _ = writeln!(&mut page, "- Qualified name: `{}`", node.qualified_name);
        if let Some(source) = &node.source_uri {
            let _ = writeln!(&mut page, "- Source: `{}`", source);
        }
        let outgoing: Vec<_> = graph.out_neighbors(id).collect();
        if !outgoing.is_empty() {
            let _ = writeln!(&mut page);
            let _ = writeln!(&mut page, "## Outgoing");
            for (target, edge) in outgoing.into_iter().take(50) {
                if let Some(target_node) = graph.node(target) {
                    let link = filenames
                        .get(&target)
                        .map(|f| f.trim_end_matches(".md"))
                        .unwrap_or("");
                    let _ = writeln!(
                        &mut page,
                        "- `{:?}` -> [[{}|{}]]",
                        edge.kind, link, target_node.name
                    );
                }
            }
        }
        let file = output.join(filenames.get(&id).expect("filename exists"));
        fs::write(file, page)?;
        written += 1;
    }
    fs::write(output.join("Home.md"), obsidian_home(graph, &filenames))?;
    Ok(json!({
        "operation": "export",
        "format": "obsidian",
        "output": output,
        "files_written": written + 1,
    }))
}

fn obsidian_home(graph: &Graph, filenames: &HashMap<NodeId, String>) -> String {
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Ariadne Graph");
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "- Nodes: {}", graph.node_count());
    let _ = writeln!(&mut out, "- Edges: {}", graph.edge_count());
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "## High Degree Nodes");
    let mut nodes: Vec<_> = graph
        .nodes()
        .filter(|(_, node)| is_rankable_node(node))
        .collect();
    nodes.sort_by_key(|(id, _)| {
        std::cmp::Reverse(graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count())
    });
    for (id, node) in nodes.into_iter().take(25) {
        let Some(file) = filenames.get(&id) else {
            continue;
        };
        let link = file.trim_end_matches(".md");
        let degree = graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
        let _ = writeln!(
            &mut out,
            "- [[{}|{}]] ({:?}, degree {})",
            link, node.name, node.kind, degree
        );
    }
    out
}

fn write_wiki(graph: &Graph, output: &Path, top: usize) -> Result<Value> {
    fs::create_dir_all(output)?;
    let communities = leiden(graph);
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in &communities {
        by_comm.entry(community).or_default().push(node);
    }
    let mut communities_sorted: Vec<_> = by_comm.into_iter().collect();
    communities_sorted.sort_by_key(|(_, nodes)| std::cmp::Reverse(nodes.len()));
    communities_sorted.truncate(top);

    let mut index = String::new();
    let _ = writeln!(&mut index, "# Ariadne Wiki");
    let _ = writeln!(&mut index);
    let _ = writeln!(&mut index, "- Nodes: {}", graph.node_count());
    let _ = writeln!(&mut index, "- Edges: {}", graph.edge_count());
    let _ = writeln!(&mut index);
    let _ = writeln!(&mut index, "## Communities");

    let mut written = 0usize;
    for (community, members) in communities_sorted {
        let title = community_title(graph, &members);
        let filename = format!("community-{}.md", community);
        let _ = writeln!(
            &mut index,
            "- [{}]({}) - {} nodes (community {})",
            title,
            filename,
            members.len(),
            community
        );
        let page = wiki_community_page(graph, community, &title, &members);
        fs::write(output.join(&filename), page)?;
        written += 1;
    }
    fs::write(output.join("index.md"), index)?;
    Ok(json!({
        "operation": "wiki",
        "output": output,
        "files_written": written + 1,
    }))
}

fn wiki_community_page(graph: &Graph, community: usize, title: &str, members: &[NodeId]) -> String {
    let mut out = String::new();
    let _ = writeln!(&mut out, "# {}", title);
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "- Community: {}", community);
    let _ = writeln!(&mut out, "- Nodes: {}", members.len());

    let mut files: HashMap<String, usize> = HashMap::new();
    let mut kinds: HashMap<String, usize> = HashMap::new();
    for id in members {
        if let Some(node) = graph.node(*id) {
            if let Some(source) = &node.source_uri {
                *files.entry(source.clone()).or_insert(0) += 1;
            }
            *kinds.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
        }
    }
    let mut top_files: Vec<_> = files.into_iter().collect();
    top_files.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut top_kinds: Vec<_> = kinds.into_iter().collect();
    top_kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "## Shape");
    for (kind, count) in top_kinds {
        let _ = writeln!(&mut out, "- {}: {}", kind, count);
    }

    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "## Top Files");
    for (file, count) in top_files.into_iter().take(10) {
        let _ = writeln!(&mut out, "- `{}`: {} nodes", file, count);
    }

    let ranked = ranked_display_members(graph, members);
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "## Key Nodes");
    for id in ranked.into_iter().take(25) {
        if let Some(node) = graph.node(id) {
            let degree = graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
            let _ = writeln!(
                &mut out,
                "- `{}` ({:?}, degree {})",
                node.qualified_name, node.kind, degree
            );
        }
    }
    out
}

fn community_title(graph: &Graph, members: &[NodeId]) -> String {
    let mut file_scores: HashMap<String, usize> = HashMap::new();
    let mut module_scores: HashMap<String, usize> = HashMap::new();
    for id in members {
        let Some(node) = graph.node(*id) else {
            continue;
        };
        if let Some(source) = &node.source_uri {
            let label = source_label(source);
            *file_scores.entry(label.clone()).or_insert(0) += 1;
            if let Some(module) = module_label(&label) {
                *module_scores.entry(module).or_insert(0) += 1;
            }
        }
    }

    let mut modules: Vec<_> = module_scores.into_iter().collect();
    modules.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut files: Vec<_> = file_scores.into_iter().collect();
    files.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    if let Some((module, count)) = modules.first() {
        if *count >= 3 {
            return title_case_words(module);
        }
    }
    if let Some((file, _)) = files.first() {
        return title_case_words(file);
    }

    for id in ranked_display_members(graph, members) {
        if let Some(node) = graph.node(id) {
            if !node.qualified_name.starts_with("call::") {
                return title_case_words(&node.name);
            }
        }
    }
    "Unclassified Community".to_string()
}

fn ranked_display_members(graph: &Graph, members: &[NodeId]) -> Vec<NodeId> {
    let mut ranked: Vec<_> = members
        .iter()
        .copied()
        .filter(|id| {
            graph
                .node(*id)
                .map(|node| is_rankable_node(node) && !is_test_like_node(node))
                .unwrap_or(false)
        })
        .collect();
    if ranked.is_empty() {
        ranked = members.to_vec();
    }
    ranked.sort_by_key(|id| {
        std::cmp::Reverse(graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count())
    });
    ranked
}

fn source_label(source: &str) -> String {
    let path = Path::new(source);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(source)
        .trim_matches('.');
    if stem == "mod" || stem == "lib" {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or(stem)
            .to_string()
    } else {
        stem.to_string()
    }
}

fn module_label(file_label: &str) -> Option<String> {
    let normalized = file_label.replace('-', "_");
    let head = normalized.split('_').next()?.trim();
    (head.len() >= 3).then(|| head.to_string())
}

fn title_case_words(value: &str) -> String {
    let words: Vec<_> = value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = String::new();
                    out.push(first.to_ascii_uppercase());
                    out.push_str(&chars.as_str().to_ascii_lowercase());
                    out
                }
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        "Unclassified Community".to_string()
    } else {
        words.join(" ")
    }
}

fn source_language(source: Option<&str>) -> Option<String> {
    let ext = Path::new(source?).extension()?.to_string_lossy();
    let lang = match ext.as_ref() {
        "rs" => "rust",
        "py" => "python",
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "md" => "markdown",
        "tex" => "latex",
        "svg" => "svg",
        other => other,
    };
    Some(lang.to_string())
}

fn source_category(source: Option<&str>) -> Option<&'static str> {
    let ext = Path::new(source?).extension()?.to_str()?;
    match ext {
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "py" | "rs" => Some("code"),
        "md" | "markdown" | "tex" => Some("doc"),
        "svg" => Some("image"),
        _ => None,
    }
}

fn is_source_like_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or(""),
        "c" | "cc"
            | "cpp"
            | "cxx"
            | "go"
            | "h"
            | "hpp"
            | "java"
            | "js"
            | "jsx"
            | "kt"
            | "py"
            | "rs"
            | "scala"
            | "swift"
            | "ts"
            | "tsx"
    )
}

fn is_doc_like_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or(""),
        "adoc" | "md" | "mdx" | "rst" | "txt"
    )
}

fn is_low_signal_review_path(path: &str) -> bool {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    matches!(
        file_name,
        "Cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "poetry.lock"
            | "Pipfile.lock"
            | "go.sum"
    ) || path.contains("/target/")
        || path.contains("\\target\\")
        || path.contains("/dist/")
        || path.contains("\\dist\\")
        || path.contains("/build/")
        || path.contains("\\build\\")
}

fn is_test_like_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/tests/")
        || normalized.contains("/test/")
        || normalized.ends_with("_test.rs")
        || normalized.ends_with("_test.py")
        || normalized.ends_with(".test.js")
        || normalized.ends_with(".test.ts")
        || normalized.ends_with(".spec.js")
        || normalized.ends_with(".spec.ts")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn cypher_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn cypher_opt_string(value: Option<&str>) -> String {
    value
        .map(cypher_string)
        .unwrap_or_else(|| "null".to_string())
}

fn cypher_rel_type(value: &str) -> String {
    let rel: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if rel.is_empty() {
        "RELATED".to_string()
    } else {
        rel
    }
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "node".to_string()
    } else {
        trimmed.to_string()
    }
}

fn handle_http(mut stream: TcpStream, db: &Path, algorithm: &str) -> Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path == "/" {
        write_response(&mut stream, "text/html; charset=utf-8", INDEX_HTML)
    } else if path == "/app.js" {
        write_response(&mut stream, "application/javascript; charset=utf-8", APP_JS)
    } else if path == "/style.css" {
        write_response(&mut stream, "text/css; charset=utf-8", STYLE_CSS)
    } else if path.starts_with("/api/graph") {
        let body = graph_json(db, algorithm, path)?;
        write_response(&mut stream, "application/json", &body)
    } else if path.starts_with("/api/search") {
        let q = query_param(path, "q").unwrap_or_default();
        let body = search_json(db, &q, path)?;
        write_response(&mut stream, "application/json", &body)
    } else {
        write_not_found(&mut stream)
    }
}

fn write_response(stream: &mut TcpStream, content_type: &str, body: &str) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        content_type,
        body.len(),
        body
    )?;
    Ok(())
}

fn write_not_found(stream: &mut TcpStream) -> Result<()> {
    let body = "not found";
    write!(
        stream,
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    Ok(())
}

fn graph_json(db: &Path, algorithm: &str, request_path: &str) -> Result<String> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let node_offset = query_usize(request_path, "offset").unwrap_or(0);
    let node_limit = query_usize(request_path, "limit")
        .unwrap_or(1000)
        .clamp(1, 5000);
    let edge_offset = query_usize(request_path, "edge_offset").unwrap_or(0);
    let edge_limit = query_usize(request_path, "edge_limit")
        .unwrap_or(node_limit.saturating_mul(2))
        .clamp(1, 10000);
    let communities = match algorithm {
        "louvain" => louvain(&graph),
        "leiden" => leiden(&graph),
        _ => leiden(&graph),
    };
    let all_nodes: Vec<_> = graph
        .nodes()
        .map(|(id, n)| {
            let degree = graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
            json!({
                "id": id.0,
                "label": n.name,
                "qname": n.qualified_name,
                "kind": n.kind,
                "source": n.source_uri,
                "degree": degree,
                "community": communities.get(&id).copied().unwrap_or(0),
            })
        })
        .collect();
    let all_edges: Vec<_> = graph
        .edges()
        .map(|(_, src, dst, e)| {
            json!({
                "source": src.0,
                "target": dst.0,
                "kind": e.kind,
                "confidence": e.confidence.score(),
            })
        })
        .collect();
    let total_nodes = all_nodes.len();
    let total_edges = all_edges.len();
    let nodes = paged_values(&all_nodes, node_offset, node_limit);
    let edges = paged_values(&all_edges, edge_offset, edge_limit);
    let returned_nodes = nodes.len();
    let returned_edges = edges.len();
    Ok(json!({
        "nodes": nodes,
        "links": edges,
        "graph_summary": graph_summary_json(&graph),
        "guardrails": {
            "nodes": pagination_json(node_offset, node_limit, returned_nodes, total_nodes),
            "links": pagination_json(edge_offset, edge_limit, returned_edges, total_edges),
        }
    })
    .to_string())
}

fn search_json(db: &Path, query: &str, request_path: &str) -> Result<String> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let offset = query_usize(request_path, "offset").unwrap_or(0);
    let limit = query_usize(request_path, "limit")
        .unwrap_or(20)
        .clamp(1, 100);
    let hits: Vec<_> = fts_ranked_search(&store, &graph, query, offset.saturating_add(limit))
        .into_iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "id": hit.id.0,
                    "score": hit.score,
                    "label": n.name,
                    "qname": n.qualified_name,
                    "kind": n.kind,
                    "signals": hit.signals,
                })
            })
        })
        .collect();
    let total = hits.len();
    let page = paged_values(&hits, offset, limit);
    let returned = page.len();
    Ok(json!({
        "hits": page,
        "graph_summary": graph_summary_json(&graph),
        "guardrails": {
            "hits": pagination_json(offset, limit, returned, total),
        }
    })
    .to_string())
}

fn paged_values(values: &[Value], offset: usize, limit: usize) -> Vec<Value> {
    let start = offset.min(values.len());
    let end = (start + limit).min(values.len());
    values[start..end].to_vec()
}

fn pagination_json(offset: usize, limit: usize, returned: usize, total: usize) -> Value {
    json!({
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "total": total,
        "has_more": offset.saturating_add(returned) < total,
    })
}

fn query_usize(path: &str, name: &str) -> Option<usize> {
    query_param(path, name)?.parse().ok()
}

fn query_param(path: &str, name: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            return Some(url_decode(value));
        }
    }
    None
}

fn url_decode(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '+' => out.push(' '),
            '%' => {
                let hex: String = chars.by_ref().take(2).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    out.push(byte as char);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

fn resolve(graph: &Graph, name: &str) -> Result<NodeId> {
    use ariadne_graph::NodeKind;
    if let Some(id) = graph.find_by_qname(name) {
        return Ok(id);
    }
    let results = search_by_name(graph, name);
    match results.len() {
        0 => bail!("no symbol found matching {}", name),
        1 => Ok(results[0]),
        _ => {
            // Prefer real definitions over `call::` placeholders.
            let defs: Vec<_> = results
                .iter()
                .copied()
                .filter(|id| {
                    graph
                        .node(*id)
                        .map(|n| !n.qualified_name.starts_with("call::"))
                        .unwrap_or(false)
                })
                .collect();
            // Among real defs, prefer Function/Class/Method/Type over Module.
            let callable: Vec<_> = defs
                .iter()
                .copied()
                .filter(|id| {
                    graph
                        .node(*id)
                        .map(|n| {
                            matches!(
                                n.kind,
                                NodeKind::Function
                                    | NodeKind::Method
                                    | NodeKind::Class
                                    | NodeKind::Type
                            )
                        })
                        .unwrap_or(false)
                })
                .collect();
            let pool = if !callable.is_empty() {
                &callable
            } else if !defs.is_empty() {
                &defs
            } else {
                &results
            };
            if pool.len() == 1 {
                return Ok(pool[0]);
            }
            // Exact-name match within the chosen pool.
            let exact: Vec<_> = pool
                .iter()
                .copied()
                .filter(|id| graph.node(*id).map(|n| n.name == name).unwrap_or(false))
                .collect();
            if exact.len() == 1 {
                return Ok(exact[0]);
            }
            let names: Vec<String> = pool
                .iter()
                .take(5)
                .filter_map(|id| graph.node(*id).map(|n| n.qualified_name.clone()))
                .collect();
            bail!("ambiguous symbol {}: matches {:?}", name, names);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_graph::{Edge, Node};

    #[test]
    fn parses_zero_context_git_diff_hunks() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,2 +10,3 @@ fn parse()
+let changed = true;
diff --git a/src/old.rs b/src/old.rs
deleted file mode 100644
--- a/src/old.rs
+++ /dev/null
@@ -4,2 +0,0 @@
-old();
"#;
        let files = parse_git_diff_hunks(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].hunks[0].old_start, 10);
        assert_eq!(files[0].hunks[0].old_count, 2);
        assert_eq!(files[0].hunks[0].new_start, 10);
        assert_eq!(files[0].hunks[0].new_count, 3);
        assert_eq!(files[1].path, "src/old.rs");
        assert_eq!(files[1].hunks[0].new_count, 0);
    }

    #[test]
    fn changed_hunks_overlap_node_spans() {
        let hunk = ChangedHunk {
            old_start: 20,
            old_count: 1,
            new_start: 20,
            new_count: 2,
        };
        assert!(hunk.overlaps_node(18, 21));
        assert!(hunk.overlaps_node(21, 25));
        assert!(!hunk.overlaps_node(1, 10));
    }

    #[test]
    fn maps_changed_hunks_to_specific_symbols() {
        let mut graph = Graph::new();
        graph.add_node(Node::new(NodeKind::File, "file::src/lib.rs").with_source(
            "src/lib.rs",
            1,
            80,
        ));
        let outer = graph.add_node(
            Node::new(NodeKind::Function, "file::src/lib.rs::outer").with_source(
                "src/lib.rs",
                10,
                40,
            ),
        );
        let inner = graph.add_node(
            Node::new(NodeKind::Function, "file::src/lib.rs::inner").with_source(
                "src/lib.rs",
                20,
                24,
            ),
        );

        let diff = vec![ChangedFile {
            path: "src/lib.rs".to_string(),
            hunks: vec![ChangedHunk {
                old_start: 22,
                old_count: 1,
                new_start: 22,
                new_count: 1,
            }],
        }];

        let nodes = nodes_for_changed_ranges(&graph, &diff);
        assert_eq!(nodes, vec![inner, outer]);
    }

    #[test]
    fn changed_ranges_include_hunk_symbol_metadata() {
        let mut graph = Graph::new();
        graph.add_node(
            Node::new(NodeKind::Function, "file::src/lib.rs::parse").with_source(
                "./src/lib.rs",
                10,
                14,
            ),
        );
        let diff = vec![ChangedFile {
            path: "src/lib.rs".to_string(),
            hunks: vec![ChangedHunk {
                old_start: 12,
                old_count: 1,
                new_start: 12,
                new_count: 1,
            }],
        }];

        let ranges = changed_ranges_json(&graph, &diff);
        assert_eq!(
            ranges[0]["hunks"][0]["symbols"][0]["qualified_name"],
            "file::src/lib.rs::parse"
        );
        assert_eq!(ranges[0]["hunks"][0]["symbols"][0]["line_start"], 10);
    }

    #[test]
    fn normalizes_zero_based_extractor_spans_for_git_diff_lines() {
        assert_eq!(normalized_node_span(Some(0), Some(1)), Some((1, 2)));
        assert_eq!(normalized_node_span(Some(3), Some(4)), Some((3, 4)));
    }

    #[test]
    fn renders_editor_mcp_templates() {
        let exe = Path::new("/opt/ariadne/bin/ariadne");
        let db = Path::new("/repo/ariadne.db");

        let shared = mcp_servers_config(exe, db);
        assert_eq!(shared["mcpServers"]["ariadne"]["type"], "stdio");
        assert_eq!(
            shared["mcpServers"]["ariadne"]["command"],
            "/opt/ariadne/bin/ariadne"
        );
        assert_eq!(
            shared["mcpServers"]["ariadne"]["args"],
            json!(["--db", "/repo/ariadne.db", "mcp-server"])
        );

        let vscode = vscode_mcp_config(exe, db);
        assert_eq!(vscode["servers"]["ariadne"]["type"], "stdio");
        assert_eq!(
            vscode["servers"]["ariadne"]["command"],
            "/opt/ariadne/bin/ariadne"
        );

        let codex = codex_mcp_toml(exe, db);
        assert!(codex.contains("[mcp_servers.ariadne]"));
        assert!(codex.contains("args = [\"--db\", \"/repo/ariadne.db\", \"mcp-server\"]"));
    }

    #[test]
    fn install_mcp_config_falls_back_when_codex_is_file() {
        let root = std::env::temp_dir().join(format!("ariadne-install-mcp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".codex"), "existing config file").unwrap();

        install_mcp_config(&root, Path::new("/repo/ariadne.db")).unwrap();

        assert!(root.join(".mcp.json").is_file());
        assert!(root.join(".cursor/mcp.json").is_file());
        assert!(root.join(".vscode/mcp.json").is_file());
        assert!(root.join(".codex-ariadne-mcp.toml").is_file());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_framed_mcp_message() {
        let mut reader = BufReader::new(
            b"Content-Length: 46\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}"
                .as_slice(),
        );
        let message = read_mcp_message(&mut reader).unwrap().unwrap();
        assert_eq!(message.framing, McpFraming::ContentLength);
        assert!(message.body.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn reads_json_line_mcp_message() {
        let mut reader = BufReader::new(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n".as_slice(),
        );
        let message = read_mcp_message(&mut reader).unwrap().unwrap();
        assert_eq!(message.framing, McpFraming::JsonLine);
        assert!(message.body.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn writes_json_line_mcp_response() {
        let mut output = Vec::new();
        write_mcp_message(
            &mut output,
            &json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
            McpFraming::JsonLine,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with('{'));
        assert!(text.ends_with('\n'));
        assert!(!text.contains("Content-Length"));
    }

    #[test]
    fn writes_framed_mcp_response() {
        let mut output = Vec::new();
        write_mcp_message(
            &mut output,
            &json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
            McpFraming::ContentLength,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("Content-Length:"));
        assert!(text.contains("\r\n\r\n{"));
    }

    #[test]
    fn response_guardrails_paginate_and_add_graph_summary() {
        let mut graph = Graph::new();
        graph.add_node(Node::new(NodeKind::Function, "m::a"));
        graph.add_node(Node::new(NodeKind::Class, "m::B"));

        let response = json!({
            "operation": "search",
            "hits": [
                {"id": 1},
                {"id": 2},
                {"id": 3}
            ]
        });
        let guarded = apply_response_guardrails(
            response,
            &graph,
            &json!({"offset": 1, "response_limit": 1}),
            DetailLevel::Standard,
        );

        assert_eq!(guarded["hits"].as_array().unwrap().len(), 1);
        assert_eq!(guarded["hits"][0]["id"], 2);
        assert_eq!(guarded["guardrails"]["pagination"]["hits"]["total"], 3);
        assert_eq!(
            guarded["guardrails"]["pagination"]["hits"]["has_more"],
            true
        );
        assert_eq!(guarded["graph_summary"]["node_count"], 2);
    }

    #[test]
    fn blast_radius_groups_symbols_and_files() {
        let changed = vec![
            json!({
                "kind": "function",
                "qualified_name": "src/lib.rs::parse",
                "source_uri": "src/lib.rs",
            }),
            json!({
                "kind": "class",
                "qualified_name": "src/model.rs::User",
                "source_uri": "src/model.rs",
            }),
        ];
        let impacted = vec![json!({
            "kind": "method",
            "qualified_name": "src/main.rs::App::run",
            "source_uri": "src/main.rs",
            "score": 0.8,
        })];

        let groups = blast_symbol_groups(&changed, 10);
        assert_eq!(groups["functions"].as_array().unwrap().len(), 1);
        assert_eq!(groups["classes"].as_array().unwrap().len(), 1);

        let files = blast_affected_files(&[json!("src/lib.rs")], &changed, &impacted, 10);
        assert_eq!(files.as_array().unwrap().len(), 3);
        let lib = files
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "src/lib.rs")
            .unwrap();
        assert_eq!(lib["changed_symbols"], 1);
    }

    #[test]
    fn surprises_rank_cross_file_edges() {
        let mut graph = Graph::new();
        let a = graph.add_node(
            Node::new(NodeKind::Function, "file::src/a.rs::a").with_source("src/a.rs", 1, 3),
        );
        let b = graph.add_node(
            Node::new(NodeKind::Function, "file::src/b.py::b").with_source("src/b.py", 1, 3),
        );
        graph.add_edge(a, b, Edge::inferred(ariadne_graph::EdgeKind::Calls, 0.5));

        let out = surprises_json(&graph, 10);
        assert_eq!(out["operation"], "surprises");
        assert!(!out["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn surprises_suppress_cross_language_placeholder_resolution_noise() {
        let mut graph = Graph::new();
        let src = graph.add_node(
            Node::new(NodeKind::Function, "file::src/a.rs::parse").with_source("src/a.rs", 1, 3),
        );
        let dst = graph.add_node(
            Node::new(NodeKind::Function, "file::src/b.py::parse").with_source("src/b.py", 1, 3),
        );
        let mut edge = Edge::extracted(ariadne_graph::EdgeKind::Calls);
        edge.properties.insert(
            "resolved_from".to_string(),
            json!("call_placeholder::unique_name"),
        );
        graph.add_edge(src, dst, edge);

        let out = surprises_json(&graph, 10);
        assert!(out["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn diagnostics_reports_graph_health_signals() {
        let mut graph = Graph::new();
        let a = graph.add_node(
            Node::new(NodeKind::Function, "file::src/a.rs::a").with_source("src/a.rs", 1, 3),
        );
        let call = graph.add_node(Node::new(
            NodeKind::Function,
            "call::domain_specific_helper",
        ));
        graph.add_edge(a, call, Edge::ambiguous(ariadne_graph::EdgeKind::Calls));

        let out = diagnostics_json(&graph, 1, 1, Some("old-model".to_string()), 10);

        assert_eq!(out["operation"], "diagnostics");
        assert_eq!(out["summary"]["nodes"], 2);
        assert_eq!(out["summary"]["unresolved_call_nodes"], 1);
        assert_eq!(out["confidence"]["ambiguous_edges"], 1);
        assert!(out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["kind"] == "embedding_model_stale"));
    }

    #[test]
    fn graph_report_includes_health_and_report_sections() {
        let mut graph = Graph::new();
        let a = graph.add_node(
            Node::new(NodeKind::Function, "file::src/a.rs::a").with_source("src/a.rs", 1, 3),
        );
        let b = graph.add_node(
            Node::new(NodeKind::Function, "file::src/b.rs::b").with_source("src/b.rs", 1, 3),
        );
        graph.add_edge(a, b, Edge::inferred(ariadne_graph::EdgeKind::Calls, 0.5));

        let report = graph_report_markdown(&graph, 2, 0, None, 5);

        assert!(report.contains("# Ariadne Graph Report"));
        assert!(report.contains("## Graph Health"));
        assert!(report.contains("## God Nodes"));
        assert!(report.contains("## Knowledge Gaps"));
    }

    #[test]
    fn export_helpers_escape_content() {
        assert_eq!(xml_escape("a<&>\""), "a&lt;&amp;&gt;&quot;");
        assert_eq!(cypher_string("can't"), "'can\\'t'");
        assert_eq!(cypher_rel_type("Calls"), "CALLS");
        assert_eq!(slugify("Graph::Node!"), "graph-node");
    }

    #[test]
    fn community_title_uses_dominant_source() {
        let mut graph = Graph::new();
        let a = graph.add_node(
            Node::new(NodeKind::Function, "file::src/query/search.rs::rank").with_source(
                "src/query/search.rs",
                1,
                3,
            ),
        );
        let b = graph.add_node(
            Node::new(NodeKind::Function, "file::src/query/search.rs::score").with_source(
                "src/query/search.rs",
                5,
                8,
            ),
        );
        let c = graph.add_node(
            Node::new(NodeKind::Function, "file::src/query/paths.rs::path").with_source(
                "src/query/paths.rs",
                1,
                3,
            ),
        );

        assert_eq!(community_title(&graph, &[a, b, c]), "Search");
    }

    #[test]
    fn communities_json_includes_quality_metrics_and_options() {
        let mut graph = Graph::new();
        let a = graph.add_node(Node::new(NodeKind::Function, "a"));
        let b = graph.add_node(Node::new(NodeKind::Function, "b"));
        graph.add_edge(a, b, Edge::extracted(ariadne_graph::EdgeKind::Calls));
        graph.add_edge(b, a, Edge::extracted(ariadne_graph::EdgeKind::Calls));

        let options = community_options(1.25, 0.75, 10, 3, false, CommunityObjective::Modularity);
        let out = communities_json(&graph, "leiden", options, 5).unwrap();

        assert_eq!(out["operation"], "communities");
        assert_eq!(out["algorithm"], "leiden");
        assert_eq!(out["options"]["resolution"], 1.25);
        assert_eq!(out["options"]["parallel"], false);
        assert_eq!(out["quality"]["disconnected_communities"], 0);
        assert_eq!(out["communities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn token_scenario_reports_savings_ratio() {
        let scenario = token_scenario("review_context", 25, 100);
        assert_eq!(scenario["ratio_vs_baseline"], 4.0);
        assert_eq!(scenario["savings_percent"], 75.0);
    }

    #[test]
    fn review_context_prioritizes_changed_source_over_lockfiles() {
        let mut lockfile = ReviewContextFile {
            path: "Cargo.lock".to_string(),
            changed: true,
            impacted: false,
            ranges: vec![(10, 30)],
            priority: 0,
        };
        let mut source = ReviewContextFile {
            path: "crates/ariadne-graph/src/main.rs".to_string(),
            changed: true,
            impacted: true,
            ranges: vec![(120, 140)],
            priority: 0,
        };
        lockfile.priority = review_context_priority(&lockfile);
        source.priority = review_context_priority(&source);

        assert!(source.priority > lockfile.priority);
        assert!(is_low_signal_review_path(&lockfile.path));
        assert!(is_source_like_path(&source.path));
    }

    #[test]
    fn review_context_upsert_merges_changed_and_impacted_paths() {
        let mut files = Vec::new();
        upsert_review_context_file(&mut files, "src/lib.rs".to_string(), true, false);
        upsert_review_context_file(&mut files, "/repo/src/lib.rs".to_string(), false, true);

        assert_eq!(files.len(), 1);
        assert!(files[0].changed);
        assert!(files[0].impacted);
        assert_eq!(files[0].path, "/repo/src/lib.rs");
    }

    #[test]
    fn review_context_budgets_per_file_context() {
        assert_eq!(review_context_line_limit(200, 1600), 50);
        assert_eq!(review_context_line_limit(8, 1600), 8);
        assert_eq!(review_context_per_file_budget(1600), 533);
        assert_eq!(review_context_per_file_budget(100), 100);
    }

    #[test]
    fn review_context_analysis_summary_avoids_full_nested_payload() {
        let analysis = json!({
            "base": "HEAD~1",
            "risk": "medium",
            "risk_score": 12.0,
            "mapping_precision": "line",
            "changed_files": ["a.rs", "b.rs"],
            "changed_symbol_total": 3,
            "impacted_total": 5,
            "affected_flows": {"total": 1, "hits": [{"large": true}]},
            "test_coverage": {
                "covered_count": 1,
                "missing_count": 2,
                "total_symbols": 3,
                "coverage_ratio": 0.33,
                "missing": [{"large": true}]
            },
            "suggested_next_tools": ["review_context"],
            "changed_nodes": [{"large": true}],
        });

        let summary = review_context_analysis_summary(&analysis);
        assert_eq!(summary["changed_file_count"], 2);
        assert!(summary.get("changed_nodes").is_none());
        assert!(summary["test_coverage"].get("missing").is_none());
        assert_eq!(summary["full_analysis_tool"], "detect_changes");
    }

    #[test]
    fn graph_heuristics_filter_noisy_symbols() {
        let call = Node::new(NodeKind::Function, "call::new");
        assert!(!is_rankable_node(&call));
        assert!(!is_actionable_unresolved_call(&call, 12));

        let file = Node::new(NodeKind::File, "file::src/lib.rs");
        assert!(!is_rankable_node(&file));

        let domain_call = Node::new(NodeKind::Function, "call::resolve_call_placeholders");
        assert!(is_actionable_unresolved_call(&domain_call, 2));

        let test = Node::new(NodeKind::Function, "file::src/lib.rs::tests::does_it");
        assert!(is_test_like_node(&test));
    }
}
