#!/usr/bin/env bash
# Self-bootstrap demo: index Ariadne with Ariadne, then run a tour
# of the reasoning kernel against the resulting graph.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -x target/release/ariadne ]]; then
	echo ">>> building release binary..."
	cargo build --release
fi

BIN=target/release/ariadne
DB=/tmp/ariadne-selfdemo.db
rm -f "$DB" "$DB-wal" "$DB-shm"

echo
echo ">>> building the graph from ./crates"
"$BIN" --db "$DB" build ./crates

echo
echo ">>> graph status"
"$BIN" --db "$DB" status

echo
echo ">>> top 10 god-nodes (PageRank)"
"$BIN" --db "$DB" god-nodes --top 10 || "$BIN" --db "$DB" god-nodes --top 10

echo
echo ">>> top 5 communities"
"$BIN" --db "$DB" communities --top 5

echo
echo ">>> callers of add_node"
"$BIN" --db "$DB" callers call::add_node | head -10

echo
echo ">>> paths cmd_build -> call::extract_directory"
"$BIN" --db "$DB" paths cmd_build call::extract_directory --max-hops 3

echo
echo ">>> done. database at: $DB"
