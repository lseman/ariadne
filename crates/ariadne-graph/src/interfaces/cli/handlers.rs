//! CLI argument definitions and `Commands` enum.
//!
//! Command implementations are extracted into sibling modules declared in `mod.rs`.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
        /// Polling interval in seconds, used only when OS file events
        /// are unavailable.
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
    /// Rank unexpected cross-community, cross-language, and hub-coupling edges.
    Surprises {
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Rebuild the SQLite FTS5 node index.
    RebuildFts,
    /// Build lightweight local embeddings for semantic search.
    Embed {
        #[arg(long, default_value = "ariadne-hash-v2")]
        model: String,
    },
    /// Open the interactive terminal UI.
    Tui,
    /// Diff graph snapshots using temporal valid_from / valid_to rows.
    GraphDiff {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long, default_value_t = 50)]
        top: usize,
    },
    /// Drop edges from a symbol and re-run BFS: what breaks if removed?
    Counterfactual {
        symbol: String,
        #[arg(long, default_value = "out")]
        direction: String,
        #[arg(long, default_value_t = 5)]
        max_depth: usize,
    },
    /// Match subgraph motifs like security-audit chains or diamond inheritance.
    Motifs {
        #[arg(long, default_value = "security_audit")]
        built_in: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
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
    /// Detect communities with Louvain, Leiden, or Infomap.
    Communities {
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Community algorithm: louvain, leiden, or infomap.
        #[arg(long, default_value = "louvain")]
        algorithm: String,
    },
    /// Deduplicate semantically equivalent concept/document nodes.
    ///
    /// Runs a multi-pass pipeline (normalization → entropy gate → MinHash/LSH
    /// → Jaro-Winkler) to find and merge nodes with similar labels across
    /// extraction sources. Only affects Concept, Document, Section, Diagram,
    /// Image, and Hyperedge nodes — code nodes already have unique qualified
    /// names.
    Dedup {
        /// Minimum Jaro-Winkler threshold for merging. Default: 0.92.
        #[arg(long, default_value_t = 0.92)]
        threshold: f32,
        /// Community similarity boost for same-community pairs. Default: 0.05.
        #[arg(long, default_value_t = 0.05)]
        community_boost: f32,
        /// Community algorithm for boost: louvain, leiden, or infomap (empty = no boost).
        #[arg(long)]
        community_algo: Option<String>,
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
    /// Compact summary of changed files, symbols, and impacted nodes.
    BlastRadius {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long, default_value_t = 2)]
        max_depth: usize,
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Report test coverage for changed symbols or a specific target.
    TestCoverage {
        /// Analyze changed symbols since this base.
        #[arg(long)]
        base: Option<String>,
        /// Analyze this specific target symbol.
        target: Option<String>,
    },
    /// Write a Markdown report to a file.
    Report {
        /// Output file path.
        output: String,
        /// Number of items per section.
        #[arg(long, default_value_t = 25)]
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
    /// Start watching all registered repositories.
    Start {
        /// Polling interval in seconds, used only when OS file events
        /// are unavailable.
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Show registered repositories.
    Status,
}
