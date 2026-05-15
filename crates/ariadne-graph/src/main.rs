use anyhow::{bail, Result};
use ariadne_graph::extract::{
    extract_directory, extract_file, ignore_set, is_supported, resolve_call_placeholders,
};
use ariadne_graph::query::{
    analyze_impact, articulation_points, bridge_scores, callees_of, callers_of, core_numbers,
    cyclic_components, find_top_paths, fts_ranked_search, leiden, louvain, pagerank,
    paths::PathQuery, personalized_pagerank, ranked_search, search_by_name, temporal_diff,
    ImpactQuery, TemporalDiff,
};
use ariadne_graph::store::Store;
use ariadne_graph::{Graph, NodeId, NodeKind};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// Summarize communities, bridges, and coupling at architecture level.
    Architecture {
        #[arg(long, default_value = "standard")]
        detail_level: String,
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
        Commands::Architecture { detail_level } => cmd_architecture(&cli.db, &detail_level),
        Commands::Tool { operation, params } => cmd_tool(&cli.db, &operation, &params),
        Commands::Mcp => cmd_mcp(&cli.db),
        Commands::McpServer => cmd_mcp_server(&cli.db),
        Commands::GodNodes { top, seed } => cmd_god_nodes(&cli.db, top, seed.as_deref()),
        Commands::Communities { top, algorithm } => cmd_communities(&cli.db, top, &algorithm),
        Commands::Flows { top } => cmd_flows(&cli.db, top),
        Commands::AffectedFlows { base, top } => cmd_affected_flows(&cli.db, &base, top),
        Commands::Search { query } => cmd_search(&cli.db, &query),
        Commands::Tui => cmd_tui(&cli.db),
    }
}

fn cmd_build(db: &Path, path: &Path) -> Result<()> {
    let mut graph = Graph::new();
    tracing::info!("extracting from {}", path.display());
    let n = extract_directory(path, &mut graph)?;
    tracing::info!(
        "extracted {} files: {} nodes, {} edges",
        n,
        graph.node_count(),
        graph.edge_count()
    );
    let mut store = Store::open(db)?;
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
    store.delete_sources(&stale)?;

    let mut graph = store.load()?;
    for source in &changed {
        let file = Path::new(source);
        if file.exists() {
            extract_file(file, &mut graph)?;
        }
    }
    resolve_call_placeholders(&mut graph);
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

    for hook in ["post-commit", "post-merge", "post-checkout"] {
        let path = hooks_dir.join(hook);
        if path.exists() && !force {
            bail!(
                "{} already exists; rerun with --force to replace it",
                path.display()
            );
        }
        let script = format!(
            "#!/bin/sh\n\"{}\" --db \"{}\" update \"{}\" >/dev/null 2>&1 || true\n",
            exe.display(),
            db.display(),
            root.display()
        );
        fs::write(&path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
    }

    println!(
        "installed Ariadne auto-update hooks in {}",
        hooks_dir.display()
    );
    if agents {
        install_agents_md(repo, &db)?;
    }
    if mcp {
        install_mcp_config(repo, &db)?;
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
    println!("ariadne db: {}", db.display());
    println!("  nodes: {}", n);
    println!("  edges: {}", e);
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

fn cmd_tool(db: &Path, operation: &str, params: &str) -> Result<()> {
    let params: Value = serde_json::from_str(params)?;
    let response = tool_response(db, operation, &params)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn tool_response(db: &Path, operation: &str, params: &Value) -> Result<Value> {
    let store = Store::open(db)?;
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
            json!({ "operation": operation, "nodes": nodes, "edges": edges })
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
        "suggested_questions" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let analysis = detect_changes_json(db, base, 2)?;
            compact_for_detail(suggested_questions_json(&analysis, limit), detail)
        }
        "architecture_overview" | "architecture" => architecture_overview_json(&graph, detail),
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
                .take(limit)
                .filter_map(|(id, score)| {
                    graph.node(id).map(|n| {
                        json!({
                            "id": id.0,
                            "score": score,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                        })
                    })
                })
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
        let request: Value = serde_json::from_str(&message)?;
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
        write_mcp_message(&mut stdout, &response)?;
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
                    "description": "Operation name, e.g. minimal_context, search, detect_changes, review_context, impact, paths, traverse, architecture_overview, cycles, core, bridge_nodes, gaps, flows, affected_flows."
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

fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
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
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8(buf)?))
}

fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
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
    for (id, rank) in sorted.iter().take(top) {
        if let Some(n) = graph.node(**id) {
            println!("  {:.6}  {}  ({:?})", rank, n.qualified_name, n.kind);
        }
    }
    Ok(())
}

fn cmd_communities(db: &Path, top: usize, algorithm: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let comm = match algorithm {
        "louvain" => louvain(&graph),
        "leiden" => leiden(&graph),
        other => bail!(
            "unknown community algorithm {}; use louvain or leiden",
            other
        ),
    };
    let mut by_comm: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    for (id, &c) in &comm {
        if let Some(n) = graph.node(*id) {
            by_comm.entry(c).or_default().push(n.qualified_name.clone());
        }
    }
    let mut entries: Vec<_> = by_comm.into_iter().collect();
    entries.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
    println!(
        "detected {} {} communities (showing top {}):",
        entries.len(),
        algorithm,
        top
    );
    for (c, members) in entries.into_iter().take(top) {
        println!("  community {} ({} members):", c, members.len());
        for m in members.iter().take(5) {
            println!("    {}", m);
        }
        if members.len() > 5 {
            println!("    ... and {} more", members.len() - 5);
        }
    }
    Ok(())
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

fn architecture_overview_json(graph: &Graph, detail: DetailLevel) -> Value {
    let communities = leiden(graph);
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
            if graph_has_temporal_data(&graph) {
                if let Some(base_hash) = git_commit_hash(base)? {
                    let diff = git_changed_diff(base).unwrap_or_default();
                    let mut cache = HashMap::new();
                    let temporal =
                        temporal_diff(&graph, &base_hash, &head, &mut |ancestor, descendant| {
                            *cache
                                .entry((ancestor.to_string(), descendant.to_string()))
                                .or_insert_with(|| git_is_ancestor(ancestor, descendant))
                        });
                    let changed_nodes = temporal.changed_nodes();
                    let changed_files = changed_nodes
                        .iter()
                        .filter_map(|id| graph.node(*id).and_then(|n| n.source_uri.clone()))
                        .collect::<HashSet<_>>();
                    let mut changed_files: Vec<String> = changed_files.into_iter().collect();
                    changed_files.sort();
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
                        Some(temporal),
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
        "changed_symbols": nodes_json(&graph, &changed_nodes, 50),
        "changed_nodes": nodes_json(&graph, &changed_nodes, 50),
        "temporal": temporal.as_ref().map(|diff| temporal_diff_json(&graph, diff)),
        "impacted": impacted,
        "test_coverage": test_coverage,
        "affected_flows": affected_flows,
        "risk_score": risk_score,
        "risk": risk_label(risk_score),
        "mapping_precision": mapping_precision,
        "suggested_next_tools": ["review_context", "impact", "traverse", "suggested_questions"]
    }))
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
) -> (
    Vec<String>,
    Vec<NodeId>,
    Vec<Value>,
    String,
    Option<TemporalDiff>,
) {
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

fn temporal_diff_json(graph: &Graph, diff: &TemporalDiff) -> Value {
    json!({
        "added_nodes": nodes_json(graph, &diff.added_nodes, 50),
        "removed_nodes": nodes_json(graph, &diff.removed_nodes, 50),
        "added_edges": changed_edges_json(graph, &diff.added_edges),
        "removed_edges": changed_edges_json(graph, &diff.removed_edges),
    })
}

fn changed_edges_json(graph: &Graph, edges: &[ariadne_graph::query::ChangedEdge]) -> Vec<Value> {
    edges
        .iter()
        .map(|edge| {
            let src = graph.node(edge.src);
            let dst = graph.node(edge.dst);
            json!({
                "id": edge.id.0,
                "kind": edge.edge_kind,
                "change": edge.change,
                "src_id": edge.src.0,
                "dst_id": edge.dst.0,
                "src": src.map(|node| node.qualified_name.clone()),
                "dst": dst.map(|node| node.qualified_name.clone()),
                "source_uri": src.and_then(|node| node.source_uri.clone())
                    .or_else(|| dst.and_then(|node| node.source_uri.clone())),
            })
        })
        .collect()
}

fn graph_has_temporal_data(graph: &Graph) -> bool {
    graph
        .nodes()
        .any(|(_, n)| n.valid_from.is_some() || n.valid_to.is_some())
        || graph
            .edges()
            .any(|(_, _, _, e)| e.valid_from.is_some() || e.valid_to.is_some())
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
    let mut files: Vec<String> = analysis["changed_files"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    for item in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(source) = item["source_uri"].as_str() {
            if !files.iter().any(|f| source_matches(f, source)) {
                files.push(source.to_string());
            }
        }
    }
    let mut used_tokens = 0usize;
    let mut snippets = Vec::new();
    for file in files {
        if used_tokens >= token_budget {
            break;
        }
        let ranges = ranges_for_file_from_analysis(&analysis, &file);
        if let Ok(snippet) = file_snippet_for_ranges(&file, &ranges, max_lines_per_file) {
            let tokens = approx_tokens(&snippet);
            if used_tokens + tokens > token_budget && !snippets.is_empty() {
                continue;
            }
            used_tokens += tokens;
            snippets.push(json!({
                "path": file,
                "tokens": tokens,
                "changed_ranges": ranges,
                "snippet": snippet
            }));
        }
    }
    Ok(json!({
        "operation": "review_context",
        "base": base,
        "token_budget": token_budget,
        "used_tokens": used_tokens,
        "analysis": analysis,
        "snippets": snippets,
    }))
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
    let rows: Vec<_> = bridge_scores(graph, &communities, limit)
        .into_iter()
        .filter_map(|row| {
            graph.node(row.node).map(|n| {
                json!({
                    "id": row.node.0,
                    "score": row.score,
                    "communities_touched": row.communities_touched,
                    "degree": row.degree,
                    "approx_betweenness": row.approx_betweenness,
                    "articulation": row.articulation,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
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
            graph.node(id).map(|n| {
                json!({
                    "id": id.0,
                    "core": coreness,
                    "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
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
            graph.node(id).map(|n| {
                json!({
                    "id": id.0,
                    "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
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
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) && indeg == 0 {
            rows.push(json!({"kind":"orphan_symbol","severity":"medium","qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) && outdeg == 0 && lines > 40 {
            rows.push(json!({"kind":"large_leaf","severity":"low","lines":lines,"qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if n.qualified_name.starts_with("call::") && indeg > 0 {
            rows.push(
                json!({"kind":"unresolved_call","severity":"high","call":n.name,"incoming":indeg}),
            );
        }
        if rows.len() >= limit {
            break;
        }
    }
    json!({ "operation": "gaps", "hits": rows })
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
            missing.push(entry);
        } else {
            covered.push(entry);
        }
    }
    json!({
        "covered": covered,
        "missing": missing,
        "missing_count": missing.len(),
    })
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
    fs::create_dir_all(&codex_dir)?;
    let codex_path = codex_dir.join("ariadne-mcp.toml");
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
    use ariadne_graph::Node;

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
}
