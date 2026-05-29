# Ariadne — architecture

## Goals

Ariadne builds a typed property graph of a codebase and exposes a reasoning kernel over it. Its design targets one outcome: an AI coding assistant (Claude Code, Logician, or any MCP-speaking agent) should be able to answer architecture and impact questions by composing graph primitives, not by re-reading source files.

It is consciously modeled after `graphify` (multimodal extraction, NetworkX, Leiden) and `code-review-graph` (AST-only, MCP, blast-radius), and consciously diverges in three places:

1. **Composable query primitives** instead of point lookups.
2. **Temporal by default** — every row carries `valid_from` / `valid_to` SHA columns.
3. **Confidence as a typed enum** — structural / inferred / ambiguous edges stay distinguishable forever and queries can filter on them.

## Extraction pipeline

Three passes, each runnable independently.

### Pass 1 — AST (deterministic, parallel)

`ariadne-extract::ast` uses [tree-sitter](https://tree-sitter.github.io/) with per-language grammars. The Rust extractor emits:

- `File` nodes (one per source file)
- `Function` nodes from `function_item`
- `Class` nodes from `struct_item`
- `Type` nodes from `enum_item`
- `Module` nodes from `use_declaration` paths
- `Defines` edges from file → symbol
- `Imports` edges from file → module
- `Calls` edges from function → callee (resolved heuristically by trailing identifier; unresolved callees become `call::<name>` placeholder nodes so call sites are preserved)

Every edge from this pass carries `Confidence::Extracted` (score 1.0). Adding a new language is one module: implement `extract_file(path, &mut Graph)` and dispatch from `walker.rs`.

### Pass 2 — Concept (prose)

`ariadne-extract::concept` reads documentation. The current markdown extractor splits on ATX headings, emits one `Document` and one `Section` per heading, scans inline code spans, and emits `Mentions` edges (confidence 0.85) to any `Function` / `Class` / `Type` / `Method` whose `qualified_name` ends with the matched token.

LaTeX is a stub. The plan: a `nom` grammar over a small subset (preamble, sections, `\verb`, `\texttt`, `\cite`) so an academic paper sitting next to its implementation becomes part of the graph.

### Pass 3 — Vision (diagrams)

`ariadne-extract::vision` handles diagrams. Text-based formats (SVG, Mermaid, PlantUML) parse directly with no network. SVG is implemented and registers a `Diagram` node plus one `Concept` per `<text>` label, linked by `Illustrates` edges.

Bitmap formats live in `vision::llm` as a stub. When wired up, that path will:

1. Hash the image bytes (sha256) and consult a local cache.
2. On miss, POST to the Anthropic Messages API (or OpenAI Vision / Gemini) with a structured prompt asking for `(concept, related)` triples.
3. Emit `Concept` / `Image` nodes and `Illustrates` / `Mentions` edges with `Confidence::Inferred`.

Caching by content hash matters: re-runs over an unchanged image are free.

## Storage

Two-tier.

The hot path is `petgraph::stable_graph::StableDiGraph<Node, Edge>` wrapped by `ariadne_core::Graph`, plus a `HashMap<String, NodeIndex>` for symbol resolution by qualified name. Stable indices mean query results can cache `NodeId`s across mutations.

The cold path is SQLite (`rusqlite` with the `bundled` feature). The schema is four tables:

- `nodes(id, kind, name, qualified_name UNIQUE, source_uri, line_start, line_end, properties JSON, valid_from, valid_to)`
- `edges(id, src_id, dst_id, kind, confidence REAL, conf_class, properties JSON, valid_from, valid_to)`
- `embeddings(node_id PRIMARY KEY, model, vector BLOB)` — populated by the local `ariadne-hash-v2` embedding pass today; `sqlite-vec` remains a natural backend if the vector search surface grows.
- `meta(key, value)` — schema version and other singletons.

WAL mode and `synchronous=NORMAL` give durable writes without daemon overhead. The whole graph fits in a single file you can `scp` and re-open anywhere.

Why not Neo4j: single-file > daemon; ACID; trivial backup; composes with a terminal workflow. Past 10M edges this would creak, but no codebase you'll index is anywhere near that.

## Schema choices worth flagging

**Confidence is an enum, not a float.** A naked `f32` would force every query to inspect properties to know whether an edge is structural. Making `Confidence::Extracted` and `Confidence::Inferred(s)` distinct variants lets `PathQuery::with_min_confidence(1.0)` mean "structural only" via the same field. `Ambiguous` is a third variant — surfaced for human review, never silently dropped.

**Temporal columns on every row.** `valid_from` and `valid_to` hold git SHAs. A node introduced at SHA `abc` and removed at SHA `def` has `valid_from = 'abc'`, `valid_to = 'def'`. Differential queries reduce to `WHERE valid_from <= ? AND (valid_to IS NULL OR valid_to > ?)`. No re-parse needed.

**Hyperedges as nodes.** `NodeKind::Hyperedge` is a synthetic node whose incident edges express n-ary relationships ("these three functions implement the concept described in this document"). Petgraph has no native hyperedge support; trying to bolt it on later is painful, so the slot is reserved up front.

**Qualified names as primary keys.** `qualified_name` is `UNIQUE` in SQLite and used as the lookup key in the in-memory index. Re-inserting a node with the same `qualified_name` updates in place rather than duplicating, which makes incremental rebuilds idempotent.

## Reasoning kernel

`ariadne-query` is where Ariadne earns its keep relative to the references.

`paths::find_paths` takes a `PathQuery { from, to, max_hops, edge_kinds, min_confidence }` and returns all simple paths under the constraints. This single primitive subsumes `callers_of`, `callees_of`, transitive-call analysis, and structural-only traversal.

`centrality::pagerank` runs the standard random-walk-with-damping iteration with dangling-node handling. Graphify and CRG compute "god nodes" once at extraction time; here it's a query, so you can run it on a community subgraph or a SHA-bounded snapshot.

`communities::louvain` is currently a single-phase greedy local-move algorithm — each node moves to the community most represented among its neighbours, iterated to a fixed point. Full multi-level Louvain (with community-aggregation phase) and Leiden (with refinement) are the next two implementations; the API stays the same.

`motifs::find_motifs` will be VF2-style subgraph isomorphism (Phase 3). The most useful queries it unlocks aren't generic graph patterns but typed ones — "function that calls `untrusted_input` and later `sql_exec` without an intervening `sanitize_*` call" — which is why the pattern type carries `NodeKind` and `EdgeKind` constraints.

`counterfactual::run_without_edges` will clone the in-memory graph, drop the supplied edges, and rerun a query (Phase 3). Answers "if I delete this function, what stops being reachable?" with reachability math rather than CRG's deliberately-conservative blast-radius approximation.

`differential` will operate directly on the SQLite store and emit `{added, removed, modified}` buckets driven by the temporal columns.

## CLI

The binary is `ariadne` (from `ariadne-cli`). Eight commands are wired up today:

```
ariadne build   <path>                      Build the graph from a directory.
ariadne status                              Show graph statistics.
ariadne paths   <from> <to> [--max-hops N]  Enumerate paths between symbols.
ariadne callers <target>                    Find callers of a function.
ariadne callees <source>                    Find callees of a function.
ariadne god-nodes [--top N]                 PageRank top-N.
ariadne communities [--top N]               Greedy communities.
ariadne search  <query>                     Substring name search.
```

All commands accept `--db <path>` (default `ariadne.db`).

## Phasing

**Phase 1 (this MVP)** — AST extraction (Rust, Python), petgraph store, SQLite persistence, the CLI, working `paths` / `pagerank` / greedy communities.

**Phase 2** — Markdown + LaTeX done properly, optional transformer embeddings (`fastembed` or external API), full Louvain, deeper `differential` queries on the temporal columns, and richer MCP resources around the kernel.

**Phase 3** — Leiden and Infomap, VF2 motif matching with a typed pattern DSL, counterfactual queries with graph cloning, `explain(path)` that walks a path and synthesises a natural-language trace from node properties.

**Phase 4** — Vision LLM pass for bitmap diagrams, hyperedge materialisation in the query layer, a Ratatui TUI explorer.

## License

MIT.
