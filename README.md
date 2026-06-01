# ariadne

<p align="center">
  <img src="assets/ariadne-logo.svg" alt="Ariadne" width="480">
</p>

A graph-based semantic system for code, documents, and diagrams. Written in Rust, end-to-end.

> Ariadne gave Theseus the thread that let him navigate the labyrinth. This is that thread for your codebase.

Ariadne builds a typed property graph of a project and exposes agent-friendly reasoning operations for search, review, impact analysis, traversal, graph visualisation, continuous indexing, and MCP integration.

## Why Ariadne

Ariadne is designed for AI coding agents that need compact, high-signal context before reading files.

- **One external tool, many internal primitives.** Agents call `ariadne tool <operation>` or connect to the stdio MCP server, which exposes a single `ariadne` tool backed by many composable operations.
- **Graph-first review context.** `detect-changes`, `review-context`, `impact`, and `traverse` produce bounded context for code review and debugging, including hunk-to-symbol diff mapping.
- **Incremental by default.** `update`, `watch`, git hooks, and daemon mode keep the graph fresh after every commit.
- **Typed, weighted reasoning.** Nodes and edges carry kinds and confidence; paths, search, PageRank, communities, and impact ranking use those signals.
- **Hybrid + semantic search.** SQLite FTS5 with a BM25-ranked, unicode61 tokeniser (underscore-aware), blended with in-memory fuzzy/topology scoring and optional local embeddings.
- **Execution flows.** Entry-point detection and forward-BFS flow tracing, materialised as `Flow` nodes ranked by criticality. Cap trimming preserves the most central nodes rather than cutting by BFS order.
- **Counterfactual reachability.** `counterfactual` drops edges and reruns BFS to answer "what breaks if I remove this dependency?" with graph-level reachability math, not conservative blast-radius approximation.
- **Interactive TUI. Three-tab terminal UI (Search / Flows / Browse) with live hybrid search, signal-aware search details, direct test coverage hints, callers/callees/flows panels, and cross-tab node navigation.
- **Local-first.** SQLite storage, tree-sitter extraction, and a self-contained D3 explorer. No external services required.

## What It Can Do Today

Ariadne is already useful as a local codebase map, an agent context server, and a review assistant.

| Area | Features |
|---|---|
| Extraction | Rust, Python, C/C++; Markdown sections and symbol mentions; SVG diagram text/concept extraction; file, symbol, import, call, inheritance, mention, flow, and test edges |
| Search | SQLite FTS5, BM25, unicode-aware tokenisation, fuzzy identifier matching, topology signals, and optional local semantic embeddings |
| Review | Git diff analysis, hunk-to-symbol mapping, risk scoring, blast radius, suggested review questions, token-budgeted context, affected flows, and test coverage gaps |
| Graph reasoning | Weighted paths, callers/callees, impact ranking, traversal, PageRank, personalized PageRank, communities, bridge nodes, k-core, cycles, articulation points, surprises, architecture summaries, and counterfactual reachability |
| History | Incremental updates, file hashes, active/archive rows, temporal `valid_from` / `valid_to` windows, and graph diff between git refs |
| Interfaces | CLI, one-operation JSON tool, real stdio MCP server, legacy JSON-lines MCP loop, TUI, D3 web explorer, graph health reports, wiki export, GraphML/Cypher/Obsidian exports |
| Automation | Polling watch mode, daemon-managed repositories, git hooks, and editor config installers for Claude Code, Cursor, VS Code, and Codex |

## How It Works

1. **Walk the workspace.** `build`, `update`, `watch`, hooks, or daemon mode scan supported files while respecting `.gitignore`, `.ariadneignore`, and common generated directories.
2. **Extract typed nodes and edges.** Tree-sitter passes create files, functions, methods, classes, traits, types, modules, imports, call edges, inheritance, test markers, Markdown sections, and SVG concepts.
3. **Resolve and enrich.** Ariadne resolves placeholder calls when names are unique or file-local, derives `TestedBy` edges from test calls, and materialises execution flows from entry points.
4. **Persist locally.** The graph is saved to SQLite with JSON properties, FTS5 search rows, optional embedding vectors, file hashes, and temporal validity columns.
5. **Answer bounded questions.** Query commands load the graph and return compact, ranked context instead of asking an agent to read the whole tree.

## Status

| Component | State |
|---|---|
| AST pass: Rust | working — traits, methods, scoped impl/module names |
| AST pass: Python | working — scoped classes, functions, methods |
| AST pass: C/C++ | working — via tree-sitter-cpp |
| Markdown concept extractor | minimal |
| LaTeX concept extractor | stub |
| SVG diagram extractor | working |
| SQLite persistence | working |
| FTS5 full-text search | working — BM25, unicode61+underscore tokeniser, blended ranking |
| Optional embeddings | working — local `ariadne-hash-v2` semantic search boost |
| Incremental updates | working — file-hash based |
| Git auto-update hooks | working |
| Watch / daemon mode | working — polling based |
| Hybrid / fuzzy search | working |
| Weighted top-k paths | working |
| Personalized PageRank | working |
| Louvain / Leiden communities | working — tunable resolution/well-connectedness plus quality metrics |
| Impact analysis | working |
| Review / change analysis | working — hunk-to-symbol diff mapping |
| Test awareness | working — `TestedBy` edges and missing-test reporting |
| True temporal diff queries | working — `valid_from` / `valid_to` windows |
| First-class graph diff | working — active + archived store-backed history |
| Execution flows | working — criticality-ranked, relevance-trimmed cap |
| Interactive TUI | working — Search / Flows / Browse, ratatui, signal/test detail |
| Agent one-tool interface | working |
| Stdio MCP server | working |
| Editor MCP installers | working — Claude Code, Cursor, VS Code, Codex |
| D3 graph explorer | working |
| Performance guardrails | working — response budgets, pagination, graph summaries |
| Motifs / counterfactuals | counterfactual working; motifs working (VF2 subgraph isomorphism) |

## Quick Start

```bash
cargo build --release
./target/release/ariadne --db ariadne.db build .
./target/release/ariadne --db ariadne.db status
./target/release/ariadne --db ariadne.db rebuild-fts
./target/release/ariadne --db ariadne.db embed --model ariadne-hash-v2
```

Supported inputs today:

```text
Rust       .rs
Python     .py
C / C++    .c .cc .cpp .cxx .h .hh .hpp .hxx
Docs       .md .markdown (.tex stub)
Diagrams   .svg
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

`install --repo .` writes git hooks for `pre-commit`, `post-commit`, `post-merge`, and `post-checkout`. The pre-commit hook writes a brief `detect-changes --base HEAD` report to `.git/hooks/ariadne-pre-commit.json`; post-commit and checkout hooks refresh the graph after the commit SHA is settled. The hooks are informational and do not block the commit.

Use the graph for review:

```bash
./target/release/ariadne --db ariadne.db detect-changes --base HEAD~1 --brief
./target/release/ariadne --db ariadne.db review-context --base HEAD~1 --token-budget 1600
./target/release/ariadne --db ariadne.db suggested-questions --base HEAD~1
```

## Core Concepts

| Concept | Meaning |
|---|---|
| Node | A typed thing Ariadne can reason about: file, function, method, class, trait, type, module, document, section, diagram concept, or execution flow |
| Edge | A typed relationship: defines, imports, calls, inherits, mentions, illustrates, member-of, entry-of, or tested-by |
| Confidence | Extracted, inferred, or ambiguous; queries can prefer structural edges without throwing away uncertain evidence |
| Flow | A synthetic node representing a forward call trace from an entry point, ranked by criticality |
| Temporal row | A stored node or edge with `valid_from` and `valid_to`, so diffs can compare graph states across refs |
| Guardrails | Response limits, pagination, detail levels, and graph summaries built into agent-facing JSON responses |

## Interactive TUI

```bash
ariadne --db ariadne.db tui
```

Three tabs, switched with `1` / `2` / `3`:

| Tab | What it shows |
|---|---|
| **Search** | Live hybrid search as you type; result signals and node detail panel |
| **Flows** | All execution flows ranked by criticality; member list |
| **Browse** | Full node list sorted by qualified name; callers / callees / flows / direct tests detail |

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
ariadne --db ariadne.db blast-radius --base HEAD~1 --top 25
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

For test awareness and temporal history:

```bash
ariadne --db ariadne.db test-coverage --base HEAD~1
ariadne --db ariadne.db test-coverage some_symbol
ariadne --db ariadne.db graph-diff --base HEAD~1 --head HEAD --top 25
ariadne --db ariadne.db embed --model ariadne-hash-v2
```

For architecture and graph structure:

```bash
ariadne --db ariadne.db architecture --detail-level standard
ariadne --db ariadne.db communities --algorithm leiden --objective cpm --top 20 --resolution 1.0
ariadne --db ariadne.db surprises --top 25
ariadne --db ariadne.db diagnostics --top 25
ariadne --db ariadne.db report ARIADNE_REPORT.md --top 10
ariadne --db ariadne.db token-benchmark --base HEAD~1
```

## One-Tool Interface

```bash
ariadne --db ariadne.db tool search --params '{"query":"Graph","limit":5}'
```

The `tool` command is the agent-facing JSON interface. It loads the graph, runs one named operation, applies response guardrails, and prints JSON. It is the same implementation used by `mcp` and `mcp-server`.

Tool responses include a `graph_summary` and a `guardrails` object with pagination metadata by default. Use `response_limit`, `offset`, `detail_level`, and `include_graph_summary` to control response size:

```bash
ariadne --db ariadne.db tool search \
  --params '{"query":"Graph","response_limit":10,"offset":20,"detail_level":"minimal"}'
```

Common parameters:

| Parameter | Applies to | Meaning |
|---|---|---|
| `response_limit` | most operations | Maximum returned rows after guardrails |
| `offset` | most list operations | Pagination offset |
| `detail_level` | most operations | `minimal`, `standard`, or `full` |
| `include_graph_summary` | all operations | Set `false` to omit graph-level counts |

Supported operations:

| Operation | Aliases | Parameters | What it returns |
|---|---|---|---|
| `minimal_context` | `context` | `target`, `mode` | Best matching target symbols plus suggested next tools |
| `status` | | | Node/edge counts, FTS5 count, embedding count/model |
| `diagnostics` | `health` | `limit` | Graph health, index coverage, confidence mix, unresolved calls, and warnings |
| `search` | | `query`, `limit` | Hybrid FTS5, fuzzy, topology, and optional semantic search hits |
| `rebuild_fts` | `rebuild_fts_index` | | Rebuilds the SQLite FTS5 index |
| `paths` | | `from`, `to`, `max_hops`, `limit` | Ranked paths between resolved symbols |
| `impact` | | `target`, `max_hops`, `limit` | Nodes likely affected by a symbol/file |
| `detect_changes` | | `base`, `max_depth` | Git diff mapping, risk score, changed symbols, impacted nodes, flows, coverage |
| `blast_radius` | `impact_radius` | `base`, `max_depth`, `limit` | Compact changed and impacted symbols/files |
| `review_context` | | `base`, `max_lines_per_file`, `token_budget` | Token-budgeted snippets from changed and impacted files |
| `traverse` | | `target`, `direction`, `max_depth`, `token_budget` | Bounded graph walk from a target |
| `large_functions` | | `min_lines`, `limit` | Long functions/classes by source span |
| `bridge_nodes` | | `limit` | Bridge/chokepoint nodes |
| `cycles` | | `limit` | Strongly connected dependency cycles |
| `core` | `k_core` | `limit` | Nodes ranked by k-core/coreness |
| `articulation` | `articulation_points` | `limit` | Nodes whose removal disconnects graph regions |
| `gaps` | | `limit` | Structural weaknesses and likely review blind spots |
| `surprises` | `surprise_scoring` | `limit` | Unexpected cross-community, cross-language, and hub-coupling edges |
| `suggested_questions` | | `base`, `limit` | Review questions derived from change analysis |
| `communities` | | `algorithm`, `limit`, `resolution`, `well_connectedness`, `max_passes`, `max_levels`, `no_parallel` | Community summaries plus modularity, conductance, connectedness, and size metrics |
| `architecture_overview` | `architecture` | `detail_level` | Communities, bridges, coupling, and architecture summary |
| `token_benchmark` | `benchmark_tokens` | `base`, `token_budget`, `max_lines_per_file` | Token comparison between naive file reading and graph context |
| `export` | | `format`, `output` | Writes `graphml`, `cypher`, or `obsidian` export |
| `wiki` | | `output`, `top` | Writes a Markdown wiki organized by communities |
| `report` | `graph_report` | `output`, `top` | Writes a Markdown graph health and architecture report |
| `god_nodes` | | `limit`, `seed` | PageRank or personalized PageRank nodes |
| `flows` | | `limit` | Execution flows ranked by criticality |
| `affected_flows` | | `base`, `limit` | Flows touched by recent changes |
| `test_coverage` | | `target` or `base` | Direct and nearby test coverage for a target or changed symbols |
| `graph_diff` | | `base`, `head`, `limit` | Temporal diff between graph states |
| `counterfactual` | | `target`, `direction`, `max_depth` | Drops edges from a symbol, reruns BFS, reports reachable nodes lost |
| `motifs` | | `pattern`, `built_in`, `limit` | VF2 subgraph pattern matching — built-in queries: `security_audit`, `diamond`, `doc_triangle` |
| `embed_graph` | | `model` | Builds local embeddings for semantic search |

## MCP Server

```bash
ariadne --db ariadne.db mcp-server        # stdio, for editors
ariadne --db ariadne.db install --mcp     # write editor config files
```

Manual MCP config for an installed `ariadne` binary:

```json
{
  "mcpServers": {
    "ariadne": {
      "command": "ariadne",
      "args": [
        "--db",
        "ariadne.db",
        "mcp-server"
      ],
      "type": "stdio"
    }
  }
}
```

For a local checkout, point `command` at the built binary:

```json
{
  "mcpServers": {
    "ariadne": {
      "command": "./target/release/ariadne",
      "args": [
        "--db",
        "/absolute/path/to/ariadne.db",
        "mcp-server"
      ],
      "type": "stdio"
    }
  }
}
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

All commands accept `--db path/to/ariadne.db` (default: `ariadne.db`).

### Indexing and Automation

| Command | Main options | What it does |
|---|---|---|
| `ariadne build <path>` | | Builds a fresh graph from supported files under `path`, stamps active rows with `HEAD` when available, resets the SQLite store, saves file hashes, and rebuilds derived call/test/flow edges |
| `ariadne update <path>` | | Incrementally re-extracts changed supported files, removes deleted sources, archives removed temporal rows, recomputes placeholder calls, `TestedBy` edges, and flows, then updates file hashes |
| `ariadne watch <path>` | `--interval 2` | Polls `path` and runs `update` repeatedly |
| `ariadne daemon add <path>` | `--alias name` | Registers a repository path in Ariadne's daemon registry |
| `ariadne daemon start` | `--interval 5` | Polls every registered repository and runs `update` |
| `ariadne daemon status` | | Prints registered daemon repositories as JSON |
| `ariadne install` | `--repo .`, `--force`, `--agents`, `--mcp` | Installs non-blocking git hooks; optionally writes `AGENTS.md` and editor MCP configs |

`build`, `update`, `watch`, hooks, and daemon mode respect `.gitignore`, `.ariadneignore`, hidden/generated directories, and the supported-file list.

### Interactive Interfaces

| Command | Main options | What it does |
|---|---|---|
| `ariadne serve` | `--host 127.0.0.1`, `--port 8787`, `--bind`, `--algorithm leiden` | Serves the browser D3 graph explorer with search and graph JSON endpoints |
| `ariadne tui` | | Opens the ratatui terminal UI with Search, Flows, and Browse tabs |
| `ariadne mcp-server` | | Runs the real stdio MCP server exposing one external tool named `ariadne` |
| `ariadne mcp` | | Runs the legacy newline-delimited JSON loop |

### Search and Navigation

| Command | Main options | What it does |
|---|---|---|
| `ariadne status` | | Prints database path, node/edge counts, FTS5 indexed nodes, and embedding status |
| `ariadne diagnostics` | `--top 25` | Prints graph health, index coverage, confidence mix, unresolved calls, and warnings as JSON |
| `ariadne rebuild-fts` | | Rebuilds the SQLite FTS5 node index |
| `ariadne embed` | `--model ariadne-hash-v2` | Builds lightweight local embeddings used as a semantic search boost |
| `ariadne search <query>` | | Prints up to 50 hybrid search hits with score, kind, source, and ranking signals |
| `ariadne paths <from> <to>` | `--max-hops 5`, `--top 10`, `--structural-only` | Finds ranked paths between resolved nodes |
| `ariadne callers <target>` | | Lists functions that call `target` |
| `ariadne callees <source>` | | Lists functions called by `source` |
| `ariadne impact <target>` | `--max-hops 4`, `--top 25` | Ranks symbols/files/docs likely affected by `target` |
| `ariadne traverse <target>` | `--direction both`, `--max-depth 3`, `--token-budget 1200` | Traverses graph relationships from a target with budgeted JSON output |

### Review and Change Analysis

| Command | Main options | What it does |
|---|---|---|
| `ariadne detect-changes` | `--base HEAD~1`, `--max-depth 2`, `--brief` | Maps git diff hunks to symbols, scores risk, reports impacted nodes, affected flows, coverage, and suggested next tools |
| `ariadne blast-radius` | `--base HEAD~1`, `--max-depth 2`, `--top 25` | Summarizes changed files/symbols and top impacted nodes |
| `ariadne review-context` | `--base HEAD~1`, `--max-lines-per-file 200`, `--token-budget 1600` | Emits bounded snippets from changed and impacted files for review |
| `ariadne suggested-questions` | `--base HEAD~1`, `--top 10` | Generates prioritized review questions from change analysis |
| `ariadne token-benchmark` | `--base HEAD~1`, `--token-budget 1600`, `--max-lines-per-file 200` | Compares naive source-reading context with graph-guided context |
| `ariadne test-coverage [target]` | `--base HEAD~1` | Reports direct `TestedBy` edges and nearby tests for one target, or for changed callables when no target is supplied |
| `ariadne affected-flows` | `--base HEAD~1`, `--top 10` | Lists execution flows touched by recent changes |
| `ariadne graph-diff` | `--base HEAD~1`, `--head HEAD`, `--top 50` | Diffs graph snapshots using temporal `valid_from` / `valid_to` rows |

### Graph Structure and Architecture

| Command | Main options | What it does |
|---|---|---|
| `ariadne large-functions` | `--min-lines 80`, `--top 50` | Finds long functions/classes by source span |
| `ariadne bridge-nodes` | `--top 25` | Ranks bridge/chokepoint nodes |
| `ariadne cycles` | `--top 25` | Finds strongly connected dependency cycles |
| `ariadne core` | `--top 25` | Ranks nodes by k-core/coreness |
| `ariadne articulation` | `--top 25` | Finds articulation points whose removal disconnects graph regions |
| `ariadne gaps` | `--top 25` | Identifies weakly tested, high-impact, or structurally risky areas |
| `ariadne surprises` | `--top 25` | Ranks unexpected cross-community, cross-language, and hub-coupling edges |
| `ariadne architecture` | `--detail-level standard` | Summarizes communities, bridges, coupling, and architecture-level signals |
| `ariadne god-nodes` | `--top 10`, `--seed SYMBOL` | Ranks global or seed-biased PageRank nodes |
| `ariadne communities` | `--top 20`, `--algorithm louvain|leiden`, `--objective modularity|cpm`, `--resolution 1.0`, `--well-connectedness 1.0`, `--max-passes 50`, `--max-levels 10`, `--no-parallel` | Detects graph communities and prints ranked summaries plus quality metrics |
| `ariadne flows` | `--top 20` | Lists execution flows ranked by criticality |
| `ariadne counterfactual <SYMBOL>` | `--direction out|in|both`, `--max-depth 5` | Drops edges from a symbol and re-runs BFS: answers "what breaks if I remove this dependency?" |
| `ariadne motifs` | `--built-in security_audit|diamond|doc_triangle`, `--pattern FILE`, `--query JSON`, `--limit 50` | VF2 subgraph motif matching — find patterns like security-audit chains or diamond inheritance |

### Export and Agent JSON

| Command | Main options | What it does |
|---|---|---|
| `ariadne export <format> <output>` | `graphml`, `cypher`, `obsidian` | Exports the graph to GraphML, Cypher statements, or Obsidian Markdown |
| `ariadne wiki <output>` | `--top 25` | Generates a Markdown wiki organized around community structure |
| `ariadne report <output>` | `--top 10` | Generates a Markdown graph report with health warnings, god nodes, bridges, surprises, gaps, and questions |
| `ariadne tool <operation>` | `--params '{...}'` | Runs one JSON operation for agents and MCP wrappers |

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
