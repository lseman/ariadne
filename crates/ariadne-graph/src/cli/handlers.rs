use anyhow::{bail, Result};
use ariadne_graph::Graph;
use ariadne_graph::extract::{extract_directory, extract_file, resolve_call_placeholders};
use ariadne_graph::query::{
    analyze_impact, callees_of, callers_of, ImpactQuery,
};
use ariadne_graph::store::Store;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use super::response::{
    architecture_overview_json, detect_changes_json, DetailLevel, large_functions_json,
    review_context_json, tool_response, traverse_json,
};
use super::git::collect_file_hashes;

#[derive(Parser)]
#[command(
    name = "ariadne",
    about = "A graph-based semantic system for code, documents, and diagrams.",
    version
)]
pub struct Cli {
    /// Path to the SQLite database file.
    #[arg(short, long, default_value = "ariadne.db", global = true)]
    pub db: PathBuf,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
    /// Report graph health, index coverage, confidence mix, and unresolved calls.
    Diagnostics {
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
}

#[derive(Clone, Subcommand)]
pub enum DaemonCommands {
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

/// Build the graph from a directory of source files.
pub fn cmd_build(db: &Path, path: &Path) -> Result<()> {
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

/// Incrementally update the graph from changed files.
pub fn cmd_update(db: &Path, path: &Path) -> Result<()> {
    let current = collect_file_hashes(path)?;
    let current_map: std::collections::HashMap<String, String> = current.iter().cloned().collect();
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

/// Watch a path and incrementally update when supported files change.
pub fn cmd_watch(db: &Path, path: &Path, interval: u64) -> Result<()> {
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

/// Manage registered repositories for continuous updates.
pub fn cmd_daemon(db: &Path, command: DaemonCommands) -> Result<()> {
    use super::git::{load_daemon_repos, save_daemon_repos};
    
    match command {
        DaemonCommands::Add { path, alias } => {
            let mut repos = load_daemon_repos()?;
            let path = super::git::absolute_path(&path)?;
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

/// Install auto-update git hooks for this repository.
pub fn cmd_install(db: &Path, repo: &Path, force: bool, agents: bool, mcp: bool) -> Result<()> {
    let git_dir = repo.join(".git");
    if !git_dir.is_dir() {
        bail!("{} is not a git repository", repo.display());
    }
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let exe = std::env::current_exe()?;
    let db = super::git::absolute_path(db)?;
    let root = super::git::absolute_path(repo)?;

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
        std::fs::write(&path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
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
    std::fs::write(&path, block)?;
    println!("installed {}", path.display());
    Ok(())
}

fn install_mcp_config(repo: &Path, db: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let claude_path = repo.join(".mcp.json");
    std::fs::write(
        &claude_path,
        serde_json::to_string_pretty(&mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", claude_path.display());

    let cursor_dir = repo.join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    let cursor_path = cursor_dir.join("mcp.json");
    std::fs::write(
        &cursor_path,
        serde_json::to_string_pretty(&mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", cursor_path.display());

    let vscode_dir = repo.join(".vscode");
    std::fs::create_dir_all(&vscode_dir)?;
    let vscode_path = vscode_dir.join("mcp.json");
    std::fs::write(
        &vscode_path,
        serde_json::to_string_pretty(&super::mcp::vscode_mcp_config(&exe, db))?,
    )?;
    println!("installed {}", vscode_path.display());

    let codex_dir = repo.join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let codex_path = codex_dir.join("ariadne-mcp.toml");
    std::fs::write(&codex_path, super::mcp::codex_mcp_toml(&exe, db))?;
    println!("installed {}", codex_path.display());
    Ok(())
}

fn mcp_servers_config(exe: &Path, db: &Path) -> serde_json::Value {
    json!({
        "mcpServers": {
            "ariadne": super::mcp::ariadne_stdio_server_config(exe, db)
        }
    })
}

/// Serve an interactive D3 graph explorer.
pub fn cmd_serve(db: &Path, bind: &str, algorithm: &str) -> Result<()> {
    use std::net::TcpListener;
    
    let listener = TcpListener::bind(bind)?;
    println!("Ariadne graph explorer listening on http://{}", bind);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = super::http::handle_http(stream, db, algorithm) {
                    tracing::warn!("serve request failed: {}", e);
                }
            }
            Err(e) => tracing::warn!("serve connection failed: {}", e),
        }
    }
    Ok(())
}

/// Show graph statistics.
pub fn cmd_status(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let (n, e) = store.stats()?;
    println!("ariadne db: {}", db.display());
    println!("  nodes: {}", n);
    println!("  edges: {}", e);
    Ok(())
}

/// Find paths between two symbols.
pub fn cmd_paths(
    db: &Path,
    from: &str,
    to: &str,
    max_hops: usize,
    top: usize,
    structural_only: bool,
) -> Result<()> {
    use ariadne_graph::query::paths::PathQuery;
    use super::helpers::resolve;
    
    let store = Store::open(db)?;
    let graph = store.load()?;
    let from_id = resolve(&graph, from)?;
    let to_id = resolve(&graph, to)?;
    let mut q = PathQuery::between(from_id, to_id, max_hops);
    if structural_only {
        q = q.with_min_confidence(1.0);
    }
    let paths = ariadne_graph::query::find_top_paths(&graph, &q, top);
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

/// Find callers of a function.
pub fn cmd_callers(db: &Path, target: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = super::helpers::resolve(&graph, target)?;
    let callers = callers_of(&graph, id);
    println!("callers of {} ({} total):", target, callers.len());
    for c in callers {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

/// Find callees of a function.
pub fn cmd_callees(db: &Path, source: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = super::helpers::resolve(&graph, source)?;
    let callees = callees_of(&graph, id);
    println!("callees of {} ({} total):", source, callees.len());
    for c in callees {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

/// Rank symbols, files, and docs likely affected by a target.
pub fn cmd_impact(db: &Path, target: &str, max_hops: usize, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = super::helpers::resolve(&graph, target)?;
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

/// Risk-scored change analysis from a git diff base.
pub fn cmd_detect_changes(db: &Path, base: &str, max_depth: usize, brief: bool) -> Result<()> {
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

/// Token-budgeted review context for changed and impacted files.
pub fn cmd_review_context(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<()> {
    let context = review_context_json(db, base, max_lines_per_file, token_budget)?;
    println!("{}", serde_json::to_string_pretty(&context)?);
    Ok(())
}

/// Traverse graph relationships from a target with a token budget.
pub fn cmd_traverse(
    db: &Path,
    target: &str,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = super::helpers::resolve(&graph, target)?;
    let out = traverse_json(&graph, seed, direction, max_depth, token_budget);
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Find large functions/classes by source span.
pub fn cmd_large_functions(db: &Path, min_lines: u32, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&large_functions_json(&graph, min_lines, top))?
    );
    Ok(())
}

/// Find bridge/chokepoint nodes.
pub fn cmd_bridge_nodes(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::bridge_nodes_json(&graph, top))?
    );
    Ok(())
}

/// Find dependency cycles via strongly connected components.
pub fn cmd_cycles(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::cycles_json(&graph, top))?
    );
    Ok(())
}

/// Rank nodes by k-core/coreness.
pub fn cmd_core(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!("{}", serde_json::to_string_pretty(&super::response::core_json(&graph, top))?);
    Ok(())
}

/// Find articulation points whose removal disconnects graph regions.
pub fn cmd_articulation(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&super::response::articulation_json(&graph, top))?
    );
    Ok(())
}

/// Identify structural weaknesses and likely review blind spots.
pub fn cmd_gaps(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    println!("{}", serde_json::to_string_pretty(&super::response::gaps_json(&graph, top))?);
    Ok(())
}

/// Report graph health, index coverage, confidence mix, and unresolved calls.
pub fn cmd_diagnostics(db: &Path, top: usize) -> Result<()> {
    let report = super::response::diagnostics_json(db, top)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Generate prioritized review questions from graph analysis.
pub fn cmd_suggested_questions(db: &Path, base: &str, top: usize) -> Result<()> {
    let analysis = detect_changes_json(db, base, 2)?;
    let questions = super::response::suggested_questions_json(&analysis, top);
    println!("{}", serde_json::to_string_pretty(&questions)?);
    Ok(())
}

/// Summarize communities, bridges, and coupling at architecture level.
pub fn cmd_architecture(db: &Path, detail_level: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let detail = DetailLevel::parse(detail_level);
    println!(
        "{}",
        serde_json::to_string_pretty(&architecture_overview_json(&graph, detail))?
    );
    Ok(())
}

/// One-operation JSON interface for agents and MCP wrappers.
pub fn cmd_tool(db: &Path, operation: &str, params: &str) -> Result<()> {
    let params: serde_json::Value = serde_json::from_str(params)?;
    let response = tool_response(db, operation, &params)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// JSON-lines one-tool loop for MCP adapters and editor wrappers.
pub fn cmd_mcp(db: &Path) -> Result<()> {
    use super::mcp::{required_str};
    
    eprintln!(
        "Ariadne MCP-style JSON loop ready. Send {{\"operation\":\"search\",\"params\":{{...}}}}."
    );
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(&line)?;
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

/// Real stdio MCP server exposing Ariadne as one tool.
pub fn cmd_mcp_server(db: &Path) -> Result<()> {
    use super::mcp::{
        ariadne_mcp_tool_schema, mcp_error, read_mcp_message, write_mcp_message,
    };
    
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let request: serde_json::Value = serde_json::from_str(&message)?;
        let method = request.get("method").and_then(serde_json::Value::as_str).unwrap_or("");
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
                let name = params.get("name").and_then(serde_json::Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name != "ariadne" {
                    mcp_error(id, -32602, "unknown tool")
                } else {
                    let operation = args
                        .get("operation")
                        .and_then(serde_json::Value::as_str)
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

/// Top-ranked nodes by PageRank.
pub fn cmd_god_nodes(db: &Path, top: usize, seed: Option<&str>) -> Result<()> {
    use ariadne_graph::query::{personalized_pagerank, pagerank};
    
    let store = Store::open(db)?;
    let graph = store.load()?;
    let ranks = if let Some(seed) = seed {
        let seed_id = super::helpers::resolve(&graph, seed)?;
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

/// Detect communities with Louvain or Leiden-style refinement.
pub fn cmd_communities(db: &Path, top: usize, algorithm: &str) -> Result<()> {
    use ariadne_graph::query::{leiden, louvain};
    
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

/// List execution flows ranked by criticality.
pub fn cmd_flows(db: &Path, top: usize) -> Result<()> {
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

/// Show flows touched by changes since `base`.
pub fn cmd_affected_flows(db: &Path, base: &str, top: usize) -> Result<()> {
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

/// Search nodes by name.
pub fn cmd_search(db: &Path, query: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let results = ariadne_graph::query::ranked_search(&graph, query, 50);
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
