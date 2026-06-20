# response.rs File Split Plan

## Full Symbol-to-Line Mapping (56 symbols)

### Imports (lines 1-15)
```
Lines 1-3:  use anyhow::{bail, Result};
Lines 4-8:  use ariadne_graph::query::{..., PathQuery, ImpactQuery, TemporalDiff};
Line 9:     use ariadne_graph::store::Store;
Line 10:    use ariadne_graph::{Graph, NodeId, NodeKind};
Line 11:    use serde_json::{json, Value};
Line 12:    use std::collections::{HashMap, HashSet};
Line 13:    use std::path::Path;
Line 14:    use super::git::{git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile};
Line 15:    use super::helpers::{nodes_for_changed_hunk, nodes_for_changed_ranges, nodes_for_files};
Line 16:    use super::helpers::{resolve, source_matches};
```

### Core Response (lines 18-686)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 18    | `tool_response`                 | `pub fn`   |
| 188   | `handle_search`                 | `fn`       |
| 210   | `handle_paths`                  | `fn`       |
| 239   | `handle_impact`                 | `fn`       |
| 270   | `handle_god_nodes`              | `fn`       |
| 299   | `handle_flows`                  | `fn`       |
| 328   | `handle_affected_flows`         | `fn`       |
| 354   | `handle_blast_radius`           | `fn`       |
| 402   | `handle_test_coverage`          | `fn`       |
| 465   | `minimal_context_json`          | `pub fn`   |
| 505   | `DetailLevel` (enum)            | `pub enum` |
| 511   | `impl DetailLevel`              | `impl`     |
| 545   | `compact_for_detail`            | `fn`       |
| 560   | `ResponseGuardrails` (struct)   | `struct`   |
| 566   | `impl ResponseGuardrails`       | `impl`     |
| 592   | `apply_response_guardrails`     | `fn`       |
| 637   | `PAGEABLE_RESPONSE_KEYS`        | `const`    |
| 658   | `graph_summary_json`            | `pub fn`   |

### Architecture (lines 688-851)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 688   | `architecture_overview_json`    | `pub fn`   |
| 721   | `community_summaries_json`      | `fn`       |
| 758   | `cross_community_coupling_json` | `fn`       |
| 785   | `bridge_rows_json`              | `fn`       |
| 810   | `architecture_warnings_json`    | `fn`       |

### Temporal / Git Diff (lines 853-1034)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 853   | `detect_changes_json`           | `pub fn`   |
| 961   | `old_changed_diff`              | `fn`       |
| 996   | `temporal_diff_json`            | `fn`       |
| 1005  | `changed_edges_json`            | `fn`       |
| 1026  | `graph_has_temporal_data`       | `fn`       |

### Review Context (lines 1036-1134)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1036  | `review_context_json`           | `pub fn`   |
| 1088  | `traverse_json`                 | `pub fn`   |

### Analysis / Health (lines 1136-1409)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1136  | `large_functions_json`          | `pub fn`   |
| 1167  | `bridge_nodes_json`             | `pub fn`   |
| 1191  | `cycles_json`                   | `pub fn`   |
| 1219  | `core_json`                     | `pub fn`   |
| 1242  | `articulation_json`             | `pub fn`   |
| 1264  | `gaps_json`                     | `pub fn`   |
| 1294  | `language_of`                   | `fn`       |
| 1310  | `is_doc_language`               | `fn`       |
| 1317  | `surprises_json`                | `pub fn`   |

### Diagnostics (lines 1411-1567)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1411  | `diagnostics_json`              | `pub fn`   |

### Graph Diff (lines 1569-1639)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1569  | `graph_diff_json`               | `pub fn`   |

### Counterfactual (lines 1640-1770)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1640  | `counterfactual_json`           | `pub fn`   |

### Motifs / Questions (lines 1771-1802)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1771  | `motifs_json`                   | `pub fn`   |
| 1792  | `suggested_questions_json`      | `pub fn`   |

### Test Coverage / Risk (lines 1792-1801)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1803  | `test_coverage_json`            | `fn`       |
| 1854  | `affected_flows_json`           | `fn`       |
| 1882  | `risk_score`                    | `fn`       |
| 1902  | `risk_label`                    | `fn`       |

### File Snippets (lines 1903-1944)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1903  | `file_snippet`                  | `fn`       |
| 1914  | `file_snippet_for_ranges`       | `fn`       |
| 1964  | `ranges_for_file_from_analysis` | `fn`       |
| 1984  | `approx_tokens`                 | `fn`       |

### Helpers (lines 1989-2009)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 1989  | `required_str`                  | `fn`       |
| 1994  | `nodes_json`                    | `fn`       |
| 2013  | `changed_ranges_json`           | `fn`       |

### Report (lines 2015-2274)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 2015  | `generate_report_markdown`      | `pub fn`   |

### Tests (lines 2276-2323)
| Line  | Symbol                          | Visibility |
|-------|---------------------------------|------------|
| 2276  | `mod tests`                     | `mod`      |
| 2279  | `surprises_flags_cross_language_edge` | `#[test]` |
| 2303  | `surprises_suppresses_inferred_cross_language_calls` | `#[test]` |
| 2323  | `surprises_suppresses_code_to_doc_calls` | `#[test]` |
| 2355  | `diagnostics_for`               | `fn`       |
| 2370  | `diagnostics_reports_documented_sections` | `#[test]` |

---

## Proposed File Split

### 1. `response.rs` — Dispatch layer only
**Lines: 1-27 (imports) + 18-185 (tool_response) + 505-543 (DetailLevel) + 545-559 (compact_for_detail) + 560-590 (ResponseGuardrails) + 592-657 (apply_response_guardrails) + 637-656 (PAGEABLE_RESPONSE_KEYS) + 658-686 (graph_summary_json)**

**Pub symbols:**
- `pub fn tool_response` (dispatch)
- `pub enum DetailLevel` (used by architecture_overview_json, generate_report_markdown)
- `pub fn graph_summary_json` (used by http.rs)

**Private symbols:**
- `fn compact_for_detail` (used by tool_response)
- `struct ResponseGuardrails` (internal)
- `impl DetailLevel` (internal)
- `impl ResponseGuardrails` (internal)
- `fn apply_response_guardrails` (used by tool_response)
- `const PAGEABLE_RESPONSE_KEYS` (used by apply_response_guardrails)

**Imports from other files (super::*):**
- `super::helpers::resolve` — needed for tool_response ("search", "paths", "impact", "traverse" operations)
- `super::helpers::source_matches` — needed by handle_search indirectly

**Imports from dependencies:**
- `anyhow::{bail, Result}`
- `ariadne_graph::query::{...}` — for handle_search, handle_paths, handle_impact, handle_god_nodes (will be moved out)
- `ariadne_graph::store::Store` — for tool_response status
- `ariadne_graph::{Graph, NodeId, NodeKind}` — for graph_summary_json, tool_response
- `serde_json::{json, Value}`
- `std::collections::{HashMap, HashSet}`
- `std::path::Path`

**Cross-file imports NEEDED from other new modules:**
- `super::context::minimal_context_json`
- `super::search::handle_search`
- `super::paths::handle_paths`
- `super::impact::{handle_impact, handle_god_nodes}`
- `super::flows::{handle_flows, handle_affected_flows, handle_blast_radius, handle_test_coverage}`
- `super::temporal::{detect_changes_json, review_context_json}`
- `super::analysis::{large_functions_json, bridge_nodes_json, cycles_json, core_json, articulation_json, gaps_json, surprises_json, diagnostics_json}`
- `super::reviews::{traverse_json, graph_diff_json, counterfactual_json, motifs_json, suggested_questions_json, test_coverage_json, affected_flows_json, risk_score, risk_label, file_snippet_for_ranges, ranges_for_file_from_analysis, approx_tokens, required_str, nodes_json, changed_ranges_json}`
- `super::architecture::{architecture_overview_json, community_summaries_json, cross_community_coupling_json, bridge_rows_json, architecture_warnings_json}`
- `super::reports::generate_report_markdown`

---

### 2. `context.rs` — Minimal context
**Lines: 465-510 (`minimal_context_json`)**

**Pub symbols:**
- `pub fn minimal_context_json(graph: &Graph, target: Option<&str>, mode: &str) -> Value`

**Imports from other files (super::*):**
- `super::helpers::resolve` — NOT needed for minimal_context_json directly, it uses `ranked_search`

**Imports from dependencies:**
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::ranked_search`

---

### 3. `search.rs` — Search operation
**Lines: 188-209 (`handle_search`)**

**Pub symbols:**
- None (private, called from tool_response)

**Imports from other files (super::*):**
- `super::helpers::resolve` — NOT needed for handle_search

**Imports from dependencies:**
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::ranked_search`

---

### 4. `paths.rs` — Paths operation
**Lines: 210-238 (`handle_paths`)**

**Pub symbols:**
- None

**Imports from dependencies:**
- `anyhow::Result`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::{find_top_paths, PathQuery}`
- `super::helpers::resolve` (needed: `resolve(graph, from)` and `resolve(graph, to)`)

---

### 5. `impact.rs` — Impact & god nodes
**Lines: 239-268 (`handle_impact`), 270-305 (`handle_god_nodes`)**

**Pub symbols:**
- None

**Imports from dependencies:**
- `anyhow::Result`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::{analyze_impact, ImpactQuery, pagerank, personalized_pagerank, is_rank_noise}`
- `ariadne_graph::query::pagerank` (for handle_god_nodes)
- `ariadne_graph::query::personalized_pagerank` (for handle_god_nodes)
- `super::helpers::resolve` (needed)

---

### 6. `architecture.rs` — Community-level analysis
**Lines: 688-718, 721-756, 758-783, 785-808, 810-851**
(architecture_overview_json, community_summaries_json, cross_community_coupling_json, bridge_rows_json, architecture_warnings_json)

**Pub symbols:**
- `pub fn architecture_overview_json(graph: &Graph, detail: DetailLevel) -> Value`

**Private symbols:**
- `fn community_summaries_json`
- `fn cross_community_coupling_json`
- `fn bridge_rows_json`
- `fn architecture_warnings_json`

**Imports from other files (super::*):**
- `super::response::DetailLevel` (architecture_overview_json takes DetailLevel param)
- `super::response::graph_summary_json` — NOT used directly but architecture_overview_json calls it internally
- `super::analysis::cycles_json` (called within architecture_overview_json)
- `super::analysis::core_json` (called within architecture_overview_json)
- `super::analysis::articulation_json` (called within architecture_overview_json)

**Imports from dependencies:**
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::{leiden, bridge_scores, community_cohesion, LOW_COHESION_THRESHOLD}`
- `std::collections::HashMap`

---

### 7. `temporal.rs` — Git diff / temporal analysis
**Lines: 853-959, 961-1003, 1005-1025, 1026-1034, 1569-1639**
(detect_changes_json, old_changed_diff, temporal_diff_json, changed_edges_json, graph_has_temporal_data, graph_diff_json)

Note: `review_context_json` (1036-1086) and `traverse_json` (1088-1134) are better placed in reviews.rs since they use file snippet helpers.

**Pub symbols:**
- `pub fn detect_changes_json(db: &Path, base: &str, max_depth: usize) -> Result<Value>`
- `pub fn graph_diff_json(db: &Path, base: &str, head: &str, top: usize) -> Result<Value>`

**Private symbols:**
- `fn old_changed_diff`
- `fn temporal_diff_json`
- `fn changed_edges_json`
- `fn graph_has_temporal_data`

**Imports from other files (super::*):**
- `super::helpers::{nodes_for_changed_ranges, nodes_for_files, source_matches}` (for old_changed_diff)
- `super::git::{git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile}` (for detect_changes_json)

**Imports from dependencies:**
- `anyhow::{bail, Result}`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::query::{temporal_diff, TemporalDiff, analyze_impact, ImpactQuery}`
- `ariadne_graph::store::Store`
- `std::collections::{HashMap, HashSet}`
- `std::path::Path`

**Cross-file imports:**
- `super::flows::test_coverage_json` (called within detect_changes_json)
- `super::flows::affected_flows_json` (called within detect_changes_json)
- `super::impact::nodes_json` — actually this is a helper, see below

---

### 8. `analysis.rs` — Graph health analysis
**Lines: 1136-1165, 1167-1189, 1191-1217, 1219-1240, 1242-1262, 1264-1292, 1294-1311, 1310-1315, 1317-1409, 1411-1567**
(large_functions_json, bridge_nodes_json, cycles_json, core_json, articulation_json, gaps_json, language_of, is_doc_language, surprises_json, diagnostics_json)

**Pub symbols:**
- `pub fn large_functions_json`
- `pub fn bridge_nodes_json`
- `pub fn cycles_json`
- `pub fn core_json`
- `pub fn articulation_json`
- `pub fn gaps_json`
- `pub fn surprises_json`
- `pub fn diagnostics_json`

**Private symbols:**
- `fn language_of`
- `fn is_doc_language`

**Imports from other files (super::*):**
- `super::git::{git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile}` — NOT needed directly
- `super::helpers::{nodes_for_changed_hunk, source_matches}` — NOT needed for analysis functions
- `super::helpers::resolve` — NOT needed for analysis functions

**Imports from dependencies:**
- `anyhow::Result`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::core::{Node, Confidence}` (for diagnostics_json)
- `ariadne_graph::query::{articulation_points, bridge_scores, core_numbers, cyclic_components, leiden, call_resolution_stats}`
- `ariadne_graph::store::Store` (for diagnostics_json)
- `std::collections::HashMap`

---

### 9. `reviews.rs` — Review context, traversal, file snippets
**Lines: 1036-1086, 1088-1134, 1803-1867, 1869-1887, 1889-1892, 1894-1898, 1902-1918, 1921-1944, 1641-1770**
(review_context_json, traverse_json, counterfactual_json, file_snippet_for_ranges, ranges_for_file_from_analysis, approx_tokens, required_str, nodes_json, changed_ranges_json, motifs_json, suggested_questions_json, test_coverage_json, affected_flows_json, risk_score, risk_label)

Note: `counterfactual_json` (1641-1770) uses file snippets via `nodes_json` helper.

**Pub symbols:**
- `pub fn review_context_json`
- `pub fn traverse_json`
- `pub fn counterfactual_json`
- `pub fn motifs_json`
- `pub fn suggested_questions_json`

**Private symbols:**
- `fn file_snippet`
- `fn file_snippet_for_ranges`
- `fn ranges_for_file_from_analysis`
- `fn approx_tokens`
- `fn required_str`
- `fn nodes_json`
- `fn changed_ranges_json`
- `fn test_coverage_json`
- `fn affected_flows_json`
- `fn risk_score`
- `fn risk_label`

**Imports from other files (super::*):**
- `super::git::{git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile}` (for counterfactual_json, changed_ranges_json)
- `super::helpers::{resolve, source_matches, nodes_for_changed_hunk}` (for review_context_json, counterfactual_json, changed_ranges_json)

**Imports from dependencies:**
- `anyhow::{bail, Result}`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::store::Store` (for counterfactual_json)
- `ariadne_graph::core::Confidence` (for counterfactual_json edge handling)
- `ariadne_graph::query::{counterfactual::run_without_edges, motifs::{..., security_audit_motif, diamond_inheritance_motif, doc_function_triangle, find_motifs}}`
- `ariadne_graph::extract::flows::affected_flows` (for affected_flows_json)
- `ariadne_graph::query::{analyze_impact, ImpactQuery}` (for review_context_json)
- `ariadne_graph::EdgeKind` (for test_coverage_json)
- `std::collections::{HashMap, HashSet}`

**Cross-file imports:**
- `super::temporal::detect_changes_json` (called by review_context_json)

---

### 10. `flows.rs` — Flow operations
**Lines: 299-326, 328-352, 354-400, 402-463**
(handle_flows, handle_affected_flows, handle_blast_radius, handle_test_coverage)

Note: `test_coverage_json` helper (1792-1853) is used by detect_changes_json (temporal.rs), so it stays in reviews.rs or gets its own small module.

Actually, re-reading: `handle_test_coverage` (line 402) is the handler called from tool_response. `test_coverage_json` (line 1792) is the helper used by `detect_changes_json`.

Let me reconsider. The `handle_*` functions (4 args, take `db` and `graph` and `params`) are called from `tool_response`. The `*_json` helpers (simpler signatures) are called from other `*_json` functions.

**Pub symbols:**
- None (all handle_* are private, called from tool_response)

**Private symbols:**
- `fn handle_flows`
- `fn handle_affected_flows`
- `fn handle_blast_radius`
- `fn handle_test_coverage`

**Imports from other files (super::*):**
- `super::temporal::detect_changes_json` (for handle_affected_flows, handle_blast_radius)

**Imports from dependencies:**
- `anyhow::Result`
- `serde_json::{json, Value}`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::store::Store` (for handle_test_coverage)
- `ariadne_graph::extract::flows::{all_flows, affected_flows}`
- `ariadne_graph::EdgeKind` (for handle_test_coverage)
- `std::collections::HashSet`

---

### 11. `motifs.rs` — (merged into reviews.rs)
The motifs and questions functions are small enough to live in reviews.rs alongside the review_context_json and traverse_json. No separate file needed.

### 12. `reports.rs` — Markdown report generation
**Lines: 1946-2274 (`generate_report_markdown`)**

**Pub symbols:**
- `pub fn generate_report_markdown(db: &Path, top: usize) -> Result<String>`

**Private symbols:**
- None (generate_report_markdown is self-contained)

**Imports from other files (super::*):**
- `super::analysis::diagnostics_json` (called within generate_report_markdown)
- `super::architecture::architecture_overview_json` (called within generate_report_markdown)
- `super::analysis::bridge_nodes_json` — actually it computes bridges inline using leiden+bridge_scores
- `super::analysis::gaps_json` (called within generate_report_markdown)
- `super::analysis::surprises_json` (called within generate_report_markdown)

**Imports from dependencies:**
- `anyhow::Result`
- `serde_json::Value`
- `ariadne_graph::{Graph, NodeId, NodeKind}`
- `ariadne_graph::store::Store`
- `ariadne_graph::query::{leiden, pagerank, bridge_scores}`
- `std::collections::BTreeMap`

---

### 13. `response.rs` tests (remaining)
Tests for `surprises_json` and `diagnostics_json` should live in their respective modules:
- `surprises_flags_cross_language_edge` → `analysis.rs` test
- `surprises_suppresses_inferred_cross_language_calls` → `analysis.rs` test
- `surprises_suppresses_code_to_doc_calls` → `analysis.rs` test
- `diagnostics_for` → `analysis.rs` test helper
- `diagnostics_reports_documented_sections` → `analysis.rs` test

---

## Module Dependency Graph

```
response.rs (dispatch)
├── context.rs (minimal_context_json)
├── search.rs (handle_search)
├── paths.rs (handle_paths)
├── impact.rs (handle_impact, handle_god_nodes)
├── flows.rs (handle_flows, handle_affected_flows, handle_blast_radius, handle_test_coverage)
├── architecture.rs (architecture_overview_json + helpers)
│   ├── analysis.rs (cycles_json, core_json, articulation_json)
│   └── response.rs (DetailLevel, graph_summary_json)
├── temporal.rs (detect_changes_json, graph_diff_json)
│   ├── analysis.rs (surprises_json, diagnostics_json) — for test_coverage_json helper
│   └── flows.rs (test_coverage_json, affected_flows_json helpers)
├── reviews.rs (review_context_json, traverse_json, counterfactual_json, motifs_json, suggested_questions_json + file helpers)
│   ├── temporal.rs (detect_changes_json)
│   └── analysis.rs (surprises_json)
├── analysis.rs (large_functions_json, bridge_nodes_json, cycles_json, core_json, articulation_json, gaps_json, surprises_json, diagnostics_json)
├── reports.rs (generate_report_markdown)
│   ├── analysis.rs (diagnostics_json, gaps_json, surprises_json)
│   └── architecture.rs (architecture_overview_json)
└── mod.rs (exports all modules)
```

---

## Summary Statistics

| Module            | Lines   | Pub Fns | Private Fns | Structs/Enums |
|-------------------|---------|---------|-------------|---------------|
| response.rs       | ~200    | 2       | 5           | 2             |
| context.rs        | ~50     | 1       | 0           | 0             |
| search.rs         | ~25     | 0       | 1           | 0             |
| paths.rs          | ~30     | 0       | 1           | 0             |
| impact.rs         | ~70     | 0       | 2           | 0             |
| architecture.rs   | ~170    | 1       | 4           | 0             |
| temporal.rs       | ~350    | 2       | 4           | 0             |
| reviews.rs        | ~550    | 5       | 12          | 0             |
| flows.rs          | ~180    | 0       | 4           | 0             |
| analysis.rs       | ~600    | 8       | 2           | 0             |
| reports.rs        | ~300    | 1       | 0           | 0             |
| **Total**         | **~2525** | **20** | **35**      | **2**         |

Note: Total exceeds 2323 because some code is shared/referenced across modules (e.g., `nodes_json` helper is needed by both temporal.rs and reviews.rs, but only lives in reviews.rs). The original file has ~2323 lines; the split files should total ~2000-2100 lines after removing duplication of imports and consolidating.
