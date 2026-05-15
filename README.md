# ariadne

<p align="center">
  <img src="assets/ariadne-logo.svg" alt="Ariadne" width="480">
</p>

A graph-based semantic system for code, documents, and diagrams. Written in Rust, end-to-end.

> Ariadne gave Theseus the thread that let him navigate the labyrinth. This is that thread for your codebase.

Ariadne builds a typed property graph of a project and exposes agent-friendly reasoning operations for search, review, impact analysis, traversal, graph visualization, and continuous indexing.

## Why Ariadne

Ariadne is designed for AI coding agents that need compact, high-signal context before reading files.

- **One external tool, many internal primitives.** Agents can call `ariadne tool <operation>` or connect to the stdio MCP server, which exposes a single `ariadne` tool backed by many internal operations.
- **Graph-first review context.** `detect-changes`, `review-context`, `impact`, and `traverse` produce bounded context for code review and debugging.
- **Incremental by default.** `update`, `watch`, git hooks, and daemon mode keep the graph fresh.
- **Typed, weighted reasoning.** Nodes and edges carry kinds and confidence; paths, search, PageRank, communities, and impact ranking use those signals.
- **Local-first.** SQLite storage, tree-sitter extraction, and a self-contained D3 explorer.

## Status

| Component | State |
| --- | --- |
| AST pass: Rust | working, with traits, methods, scoped impl/module names |
| AST pass: Python | working, with scoped classes/functions/methods |
| AST pass: C/C++ | working, via tree-sitter-cpp |
| Markdown concept extractor | minimal |
| LaTeX concept extractor | stub |
| SVG diagram extractor | working |
| Vision-LLM diagram extractor | stub |
| SQLite persistence | working |
| Incremental updates | working, file-hash based |
| Git auto-update hooks | working |
| Watch/daemon mode | working, polling based |
| Hybrid/fuzzy search | working |
| Weighted top-k paths | working |
| Personalized PageRank | working |
| Louvain/Leiden-style communities | working |
| Impact analysis | working |
| Review/change analysis | working, with hunk-to-symbol diff mapping |
| Agent one-tool interface | working |
| Stdio MCP server | working |
| Editor MCP installers | working for Claude Code, Cursor, VS Code, and Codex snippets |
| Legacy JSON-lines loop | working |
| D3 graph explorer | working |
| True temporal diff queries | working via `valid_from` / `valid_to` |
| Performance guardrails | working, with response budgets, pagination metadata, and graph summaries |
| Motifs / counterfactuals | scaffolded |

## Quick Start

```bash
cargo build --release
./target/release/ariadne --db ariadne.db build .
./target/release/ariadne --db ariadne.db status
./target/release/ariadne --db ariadne.db serve --host 127.0.0.1 --port 8787
```

Open the graph explorer:

```text
http://127.0.0.1:8787
```

Keep the graph fresh:

```bash
./target/release/ariadne --db ariadne.db update .
./target/release/ariadne --db ariadne.db watch .
./target/release/ariadne --db ariadne.db install --repo . --agents --mcp
```

## Agent Workflow

Agents should start with compact graph context, then expand only when needed.

```bash
./target/release/ariadne --db ariadne.db tool minimal_context \
  --params '{"target":"some_symbol","mode":"review"}'
```

For code review:

```bash
./target/release/ariadne --db ariadne.db detect-changes --base HEAD~1
./target/release/ariadne --db ariadne.db review-context --base HEAD~1 --token-budget 1600
./target/release/ariadne --db ariadne.db suggested-questions --base HEAD~1
```

For impact/debugging:

```bash
./target/release/ariadne --db ariadne.db impact some_symbol --max-hops 4 --top 25
./target/release/ariadne --db ariadne.db traverse some_symbol --direction both --max-depth 3 --token-budget 1200
./target/release/ariadne --db ariadne.db paths from_symbol to_symbol --max-hops 6 --top 10
```

For structural risks:

```bash
./target/release/ariadne --db ariadne.db bridge-nodes
./target/release/ariadne --db ariadne.db large-functions --min-lines 80
./target/release/ariadne --db ariadne.db gaps
```

## One-Tool Interface

The CLI exposes one JSON operation surface for agents:

```bash
./target/release/ariadne --db ariadne.db tool search \
  --params '{"query":"Graph","limit":5}'
```

Tool responses include a `graph_summary` and a `guardrails` object with pagination metadata. Use `response_limit`, `offset`, `detail_level`, and `include_graph_summary` to control response size:

```bash
./target/release/ariadne --db ariadne.db tool search \
  --params '{"query":"Graph","response_limit":10,"offset":20,"detail_level":"minimal"}'
```

Supported operations:

```text
minimal_context
status
search
paths
impact
detect_changes
review_context
traverse
large_functions
bridge_nodes
cycles
core
articulation_points
gaps
suggested_questions
architecture_overview
god_nodes
```

For MCP clients, use the real stdio protocol server:

```bash
./target/release/ariadne --db ariadne.db mcp-server
```

`install --mcp` writes editor-ready MCP templates for this server:

```text
.mcp.json                  # Claude Code project scope and other mcpServers clients
.cursor/mcp.json           # Cursor project tools
.vscode/mcp.json           # VS Code workspace MCP
.codex/ariadne-mcp.toml    # Codex config snippet for ~/.codex/config.toml
```

Ariadne exposes one external MCP tool named `ariadne`; pass an `operation` and optional `params` object:

```json
{
  "operation": "review_context",
  "params": {
    "base": "HEAD~1",
    "token_budget": 1600
  }
}
```

The older `mcp` command remains available as a newline-delimited JSON loop for simple wrappers:

```bash
printf '%s\n' '{"operation":"status"}' \
  | ./target/release/ariadne --db ariadne.db mcp
```

## CLI Commands

```text
ariadne build <path>
ariadne update <path>
ariadne watch <path>
ariadne daemon add|start|status
ariadne install --repo . [--agents] [--mcp] [--force]
ariadne serve [--host 0.0.0.0] [--port 8787]
ariadne status
ariadne search <query>
ariadne paths <from> <to> [--max-hops N] [--top N]
ariadne callers <target>
ariadne callees <source>
ariadne impact <target>
ariadne detect-changes [--base HEAD~1]
ariadne review-context [--base HEAD~1] [--token-budget N]
ariadne traverse <target>
ariadne large-functions
ariadne bridge-nodes
ariadne gaps
ariadne suggested-questions
ariadne tool <operation> --params '{...}'
ariadne mcp
ariadne mcp-server
ariadne god-nodes [--seed SYMBOL]
ariadne communities [--algorithm louvain|leiden]
```

All commands accept:

```bash
--db path/to/ariadne.db
```

## Workspace

```text
ariadne/
├── Cargo.toml
├── crates/
│   ├── ariadne-core/       Node/Edge/Graph types
│   ├── ariadne-extract/    tree-sitter, prose, diagram extraction
│   ├── ariadne-store/      SQLite persistence
│   ├── ariadne-query/      search, paths, centrality, communities, impact
│   └── ariadne-cli/        binary, agent interface, server, static UI
└── examples/
```

## Agent-Facing Improvements And Roadmap

Recent agent-facing improvements:

- **Richer diff mapping.** `detect-changes` now maps zero-context git hunks to overlapping graph symbols and includes per-hunk `symbols` in `changed_ranges`.
- **True temporal queries.** Differential queries now classify added/removed nodes and edges from `valid_from` / `valid_to` windows when temporal data is present.
- **Stable editor installers.** `install --mcp` now writes templates for Claude Code, Cursor, VS Code, and a Codex TOML snippet.
- **Performance guardrails.** Tool and HTTP JSON responses now include graph summaries, bounded response sizes, and pagination metadata for large repositories.

The next improvements should focus on deeper precision and integration:

- **Test awareness.** Ariadne should identify tests for changed symbols and report likely missing tests.
- **Better language resolution.** Rust/Python/C++ extraction is useful, but deeper resolver integration with rust-analyzer, Pyright/Jedi, and clangd would improve call/type edges.
- **Semantic embeddings.** Hybrid search is lexical/fuzzy/graph-aware; embeddings would improve concept-level search and doc/code matching.
- **Path explanations.** `paths` returns ranked paths; agents would benefit from natural-language explanations of why a path matters.

## Design Notes

See [ARCHITECTURE.md](ARCHITECTURE.md) for the longer rationale.

## License

MIT
