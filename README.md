# ariadne

<p align="center">
  <img src="assets/ariadne-logo.svg" alt="Ariadne" width="480">
</p>

A graph-based semantic system for code, documents, and diagrams. Written in Rust, end-to-end.

> Ariadne gave Theseus the thread that let him navigate the labyrinth. This is that thread for your codebase.

Ariadne builds a typed property graph of a project and exposes agent-friendly reasoning operations for search, review, impact analysis, traversal, graph visualisation, and continuous indexing.

## Why Ariadne

Ariadne is designed for AI coding agents that need compact, high-signal context before reading files.

- **One external tool, many internal primitives.** Agents call `ariadne tool <operation>` or connect to the stdio MCP server, which exposes a single `ariadne` tool backed by many composable operations.
- **Graph-first review context.** `detect-changes`, `review-context`, `impact`, and `traverse` produce bounded context for code review and debugging, including hunk-to-symbol diff mapping.
- **Incremental by default.** `update`, `watch`, git hooks, and daemon mode keep the graph fresh after every commit.
- **Typed, weighted reasoning.** Nodes and edges carry kinds and confidence; paths, search, PageRank, communities, and impact ranking use those signals.
- **FTS5 full-text search.** SQLite FTS5 with a BM25-ranked, unicode61 tokeniser (underscore-aware) blended with in-memory fuzzy/topology scoring.
- **Execution flows.** Entry-point detection and forward-BFS flow tracing, materialised as `Flow` nodes ranked by criticality. Cap trimming preserves the most central nodes rather than cutting by BFS order.
- **Interactive TUI.** Three-tab terminal UI (Search / Flows / Browse) with live FTS5 search, callers/callees/flows detail panels, and cross-tab node navigation.
- **Local-first.** SQLite storage, tree-sitter extraction, and a self-contained D3 explorer. No external services required.

## Status

| Component | State |
|---|---|
| AST pass: Rust | working — traits, methods, scoped impl/module names |
| AST pass: Python | working — scoped classes, functions, methods |
| AST pass: C/C++ | working — via tree-sitter-cpp |
| Markdown concept extractor | minimal |
| LaTeX concept extractor | stub |
| SVG diagram extractor | working |
| Vision-LLM diagram extractor | stub |
| SQLite persistence | working |
| FTS5 full-text search | working — BM25, unicode61+underscore tokeniser, blended ranking |
| Incremental updates | working — file-hash based |
| Git auto-update hooks | working |
| Watch / daemon mode | working — polling based |
| Hybrid / fuzzy search | working |
| Weighted top-k paths | working |
| Personalized PageRank | working |
| Louvain / Leiden communities | working |
| Impact analysis | working |
| Review / change analysis | working — hunk-to-symbol diff mapping |
| True temporal diff queries | working — `valid_from` / `valid_to` windows |
| Execution flows | working — criticality-ranked, relevance-trimmed cap |
| Interactive TUI | working — Search / Flows / Browse, ratatui |
| Agent one-tool interface | working |
| Stdio MCP server | working |
| Editor MCP installers | working — Claude Code, Cursor, VS Code, Codex |
| D3 graph explorer | working |
| Performance guardrails | working — response budgets, pagination, graph summaries |
| Motifs / counterfactuals | scaffolded |

## Quick Start

```bash
cargo build --release
./target/release/ariadne --db ariadne.db build .
./target/release/ariadne --db ariadne.db status
```

Launch the interactive terminal UI:

```bash
./target/release/ariadne --db ariadne.db tui
```

Launch the D3 graph explorer:

```bash
./target/release/ariadne --db ariadne.db serve --host 127.0.0.1 --port 8787
# open http://127.0.0.1:8787
```

Keep the graph fresh:

```bash
./target/release/ariadne --db ariadne.db update .
./target/release/ariadne --db ariadne.db watch .
./target/release/ariadne --db ariadne.db install --repo . --agents --mcp
```

## Interactive TUI

```bash
ariadne --db ariadne.db tui
```

Three tabs, switched with `1` / `2` / `3`:

| Tab | What it shows |
|---|---|
| **Search** | Live FTS5 + ranked search as you type; results list + node detail panel |
| **Flows** | All execution flows ranked by criticality; member list |
| **Browse** | Full node list sorted by qualified name; callers / callees / flows detail |

Key bindings:

| Key | Action |
|---|---|
| `1` / `2` / `3` | Switch tabs |
| `↑↓` / `j`/`k` | Navigate lists |
| `PgUp` / `PgDn` | Jump 10–15 rows |
| `Tab` / `→` / `←` | Move between panes |
| `g` or `Enter` | Jump to selected node in Browse tab |
| `Ctrl+Q` / `Ctrl+C` / `q` | Quit (`q` is safe inside the search input) |

## Agent Workflow

Agents should start with compact graph context, then expand only when needed.

```bash
ariadne --db ariadne.db tool minimal_context \
  --params '{"target":"some_symbol","mode":"review"}'
```

For code review:

```bash
ariadne --db ariadne.db detect-changes --base HEAD~1
ariadne --db ariadne.db review-context --base HEAD~1 --token-budget 1600
ariadne --db ariadne.db suggested-questions --base HEAD~1
```

For impact / debugging:

```bash
ariadne --db ariadne.db impact some_symbol --max-hops 4 --top 25
ariadne --db ariadne.db traverse some_symbol --direction both --max-depth 3 --token-budget 1200
ariadne --db ariadne.db paths from_symbol to_symbol --max-hops 6 --top 10
```

For structural risks:

```bash
ariadne --db ariadne.db bridge-nodes
ariadne --db ariadne.db large-functions --min-lines 80
ariadne --db ariadne.db gaps
```

For flows:

```bash
ariadne --db ariadne.db flows --top 20
ariadne --db ariadne.db affected-flows --base HEAD~1
```

## One-Tool Interface

```bash
ariadne --db ariadne.db tool search --params '{"query":"Graph","limit":5}'
```

Tool responses include a `graph_summary` and a `guardrails` object with pagination metadata. Use `response_limit`, `offset`, `detail_level`, and `include_graph_summary` to control response size:

```bash
ariadne --db ariadne.db tool search \
  --params '{"query":"Graph","response_limit":10,"offset":20,"detail_level":"minimal"}'
```

Supported operations:

```text
minimal_context      status              search
paths                impact              detect_changes
review_context       traverse            large_functions
bridge_nodes         cycles              core
articulation_points  gaps                suggested_questions
architecture_overview god_nodes          flows
affected_flows
```

## MCP Server

```bash
ariadne --db ariadne.db mcp-server        # stdio, for editors
ariadne --db ariadne.db install --mcp     # write editor config files
```

`install --mcp` writes:

```text
.mcp.json                  # Claude Code / generic mcpServers clients
.cursor/mcp.json           # Cursor project tools
.vscode/mcp.json           # VS Code workspace MCP
.codex/ariadne-mcp.toml    # Codex config snippet
```

Ariadne exposes one external MCP tool named `ariadne`; pass `operation` and optional `params`:

```json
{
  "operation": "review_context",
  "params": { "base": "HEAD~1", "token_budget": 1600 }
}
```

The legacy `mcp` command is a newline-delimited JSON loop for simple wrappers:

```bash
printf '%s\n' '{"operation":"status"}' \
  | ariadne --db ariadne.db mcp
```

## CLI Reference

```text
ariadne build <path>
ariadne update <path>
ariadne watch <path>
ariadne daemon add|start|status
ariadne install --repo . [--agents] [--mcp] [--force]
ariadne serve [--host 0.0.0.0] [--port 8787]
ariadne tui
ariadne status
ariadne search <query>
ariadne paths <from> <to> [--max-hops N] [--top N]
ariadne callers <target>
ariadne callees <source>
ariadne impact <target>
ariadne detect-changes [--base HEAD~1]
ariadne review-context [--base HEAD~1] [--token-budget N]
ariadne traverse <target> [--direction in|out|both] [--max-depth N]
ariadne large-functions [--min-lines N]
ariadne bridge-nodes
ariadne gaps
ariadne suggested-questions [--base HEAD~1]
ariadne god-nodes [--seed SYMBOL]
ariadne communities [--algorithm louvain|leiden]
ariadne flows [--top N]
ariadne affected-flows [--base HEAD~1]
ariadne tool <operation> --params '{...}'
ariadne mcp
ariadne mcp-server
```

All commands accept `--db path/to/ariadne.db` (default: `ariadne.db`).

## Workspace

```text
ariadne/
├── Cargo.toml
├── crates/
│   └── ariadne-graph/       single crate — core, extract, query, store, tui, CLI binary
│       ├── src/
│       │   ├── core/        Node / Edge / Graph types
│       │   ├── extract/     tree-sitter extraction, flows, test detection
│       │   ├── query/       search, paths, centrality, communities, impact, differential
│       │   ├── store/       SQLite persistence, FTS5
│       │   ├── tui.rs       ratatui interactive UI
│       │   └── main.rs      CLI binary, agent interface, MCP server, D3 server
└── examples/
```

## Design Notes

See [ARCHITECTURE.md](ARCHITECTURE.md) for the longer rationale.

## License

MIT
