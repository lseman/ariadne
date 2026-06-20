//! Context-aware hints system for MCP tool responses.
//!
//! Tracks session state (in-memory) and generates intelligent next-step
//! suggestions after each tool call. Hints are appended as ``_hints`` to
//! responses so the LLM can propose follow-up actions without the user
//! having to discover them.
//!
//! Pattern: after ``detect_changes`` → suggest ``review_context``,
//! ``affected_flows``, ``blast_radius``. After ``search`` → suggest
//! ``traverse``, ``impact``, ``paths``. Etc.

use serde_json::{json, Map, Value};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Intent categories mapped to Ariadne operations
// ---------------------------------------------------------------------------

const _INTENT_TOOLS: &[( &str, &[&str] )] = &[
    (
        "reviewing",
        &[
            "detect_changes",
            "review_context",
            "affected_flows",
            "blast_radius",
            "test_coverage",
        ],
    ),
    (
        "debugging",
        &["search", "flows", "traverse", "paths"],
    ),
    (
        "refactoring",
        &["impact", "god_nodes", "large_functions", "gaps"],
    ),
    (
        "exploring",
        &[
            "architecture_overview",
            "community",
            "list_communities",
            "bridge_nodes",
            "cycles",
            "core",
            "surprises",
            "diagnostics",
            "health",
            "status",
        ],
    ),
];

// ---------------------------------------------------------------------------
// Workflow adjacency: for each operation, what are useful next steps
// ---------------------------------------------------------------------------

const _WORKFLOW: &[(&str, &[(&str, &str)])] = &[
    // search → deeper analysis
    (
        "search",
        &[
            ("traverse", "Walk the call graph around a matched symbol"),
            ("impact", "Check the blast radius of a matched symbol"),
            ("paths", "Find call paths between a matched symbol and another"),
            ("flows", "See which execution flows contain the result"),
        ],
    ),
    // traverse → related operations
    (
        "traverse",
        &[
            ("search", "Semantic search across the graph"),
            ("impact", "Check how much a traversed symbol affects the codebase"),
            ("flows", "See execution flows through traversed nodes"),
        ],
    ),
    // paths → related operations
    (
        "paths",
        &[
            ("search", "Find symbols related to a path endpoint"),
            ("traverse", "BFS/DFS between two symbols"),
            ("impact", "Check the impact of nodes along a path"),
        ],
    ),
    // flows → deeper analysis
    (
        "flows",
        &[
            ("search", "Search for symbols in these flows"),
            ("affected_flows", "Check which flows are affected by recent changes"),
            ("impact", "Check the impact of nodes in a flow"),
        ],
    ),
    // affected_flows → review
    (
        "affected_flows",
        &[
            ("detect_changes", "Get risk-scored change analysis"),
            ("review_context", "Build a review context with source snippets"),
            ("blast_radius", "Expand the impact analysis"),
        ],
    ),
    // blast_radius → review
    (
        "blast_radius",
        &[
            ("detect_changes", "Get risk-scored change analysis"),
            ("review_context", "Build a review context with source snippets"),
            ("test_coverage", "Check which tests cover impacted nodes"),
        ],
    ),
    // detect_changes → review
    (
        "detect_changes",
        &[
            ("review_context", "Build a review context with source snippets"),
            ("affected_flows", "See which execution flows are affected"),
            ("blast_radius", "Expand the impact analysis"),
            ("test_coverage", "Check test coverage gaps in changed code"),
        ],
    ),
    // review_context → related actions
    (
        "review_context",
        &[
            ("test_coverage", "Check test coverage gaps"),
            ("affected_flows", "See which flows are affected"),
            ("suggested_questions", "Get AI-generated follow-up questions"),
        ],
    ),
    // impact → deeper analysis
    (
        "impact",
        &[
            ("search", "Find symbols related to an impacted node"),
            ("traverse", "Walk the call graph around an impacted node"),
            ("test_coverage", "Check test coverage for impacted nodes"),
            ("gaps", "Find structural weaknesses in impacted areas"),
        ],
    ),
    // god_nodes → related operations
    (
        "god_nodes",
        &[
            ("large_functions", "Find other large functions in god nodes"),
            ("bridge_nodes", "Check if god nodes are also bridge nodes"),
            ("gaps", "Find structural weaknesses near god nodes"),
        ],
    ),
    // large_functions → related operations
    (
        "large_functions",
        &[
            ("impact", "Check the impact of large functions"),
            ("gaps", "Find structural weaknesses near large functions"),
            ("test_coverage", "Check test coverage of large functions"),
        ],
    ),
    // architecture_overview → drills
    (
        "architecture_overview",
        &[
            ("bridge_nodes", "Find cross-community bridge nodes"),
            ("cycles", "Detect dependency cycles"),
            ("surprises", "Find surprising cross-community connections"),
            ("gaps", "Find structural weaknesses"),
        ],
    ),
    // bridge_nodes → related operations
    (
        "bridge_nodes",
        &[
            ("impact", "Check the impact of bridge nodes"),
            ("architecture_overview", "See the broader architecture"),
            ("surprises", "Find surprising connections through bridges"),
        ],
    ),
    // gaps → related operations
    (
        "gaps",
        &[
            ("impact", "Check the impact of gap nodes"),
            ("architecture_overview", "See the broader architecture"),
            ("bridge_nodes", "Check for bridge nodes near gaps"),
        ],
    ),
    // surprises → related operations
    (
        "surprises",
        &[
            ("architecture_overview", "See the broader architecture"),
            ("bridge_nodes", "Check bridge nodes around surprising connections"),
            ("impact", "Check the impact of surprising nodes"),
        ],
    ),
    // diagnostics → related operations
    (
        "diagnostics",
        &[
            ("search", "Search for symbols in the graph"),
            ("architecture_overview", "See the architecture"),
            ("flows", "Explore execution flows"),
        ],
    ),
    // test_coverage → related operations
    (
        "test_coverage",
        &[
            ("large_functions", "Find uncovered large functions"),
            ("impact", "Check the impact of uncovered nodes"),
            ("detect_changes", "See what recently changed in tested areas"),
        ],
    ),
    // report → related operations
    (
        "report",
        &[
            ("architecture_overview", "See the architecture"),
            ("detect_changes", "Check recent changes"),
            ("diagnostics", "Check graph health"),
        ],
    ),
    // cycles → related operations
    (
        "cycles",
        &[
            ("impact", "Check the impact of cyclic nodes"),
            ("architecture_overview", "See how cycles affect the architecture"),
            ("bridge_nodes", "Check if cycles involve bridge nodes"),
        ],
    ),
    // core → related operations
    (
        "core",
        &[
            ("bridge_nodes", "Check bridge nodes near core nodes"),
            ("gaps", "Find gaps near core nodes"),
            ("surprises", "Find surprising connections near core nodes"),
        ],
    ),
    // articulation → related operations
    (
        "articulation",
        &[
            ("impact", "Check the impact of articulation points"),
            ("gaps", "Find gaps near articulation points"),
            ("architecture_overview", "See articulation in architecture"),
        ],
    ),
    // suggested_questions → related operations
    (
        "suggested_questions",
        &[
            ("detect_changes", "Get more detailed change analysis"),
            ("review_context", "Build a review context"),
            ("affected_flows", "See affected flows"),
        ],
    ),
    // graph_diff → related operations
    (
        "graph_diff",
        &[
            ("detect_changes", "Get risk-scored change analysis"),
            ("review_context", "Build a review context"),
            ("suggested_questions", "Get AI-generated follow-up questions"),
        ],
    ),
    // counterfactual → related operations
    (
        "counterfactual",
        &[
            ("impact", "Check the impact of removed nodes"),
            ("affected_flows", "See affected flows from counterfactual"),
        ],
    ),
    // motifs → related operations
    (
        "motifs",
        &[
            ("architecture_overview", "See how motifs fit the architecture"),
            ("surprises", "Find surprising motif instances"),
        ],
    ),
    // status → related operations
    (
        "status",
        &[
            ("diagnostics", "Get detailed graph health"),
            ("architecture_overview", "See the architecture"),
            ("search", "Start querying the graph"),
        ],
    ),
];

const _MAX_PER_CATEGORY: usize = 3;
const _MAX_TOOLS_HISTORY: usize = 100;

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

pub struct SessionState {
    tools_called: Vec<String>,
    nodes_queried: HashSet<String>,
    files_touched: HashSet<String>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            tools_called: Vec::with_capacity(_MAX_TOOLS_HISTORY),
            nodes_queried: HashSet::new(),
            files_touched: HashSet::new(),
        }
    }

    pub fn record_tool_call(&mut self, tool_name: &str) {
        self.tools_called.push(tool_name.to_string());
        if self.tools_called.len() > _MAX_TOOLS_HISTORY {
            self.tools_called.remove(0);
        }
    }

    pub fn record_nodes(&mut self, qnames: &[&str]) {
        for qn in qnames {
            if self.nodes_queried.len() < 1000 {
                self.nodes_queried.insert(qn.to_string());
            }
        }
    }

    pub fn record_files(&mut self, files: &[&str]) {
        for f in files {
            if self.files_touched.len() < 500 {
                self.files_touched.insert(f.to_string());
            }
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Intent inference
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn infer_intent(tools_called: &[String]) -> &str {
    if tools_called.is_empty() {
        return "exploring";
    }

    let recent = &tools_called[tools_called.len().saturating_sub(10)..];
    let mut scores: std::collections::HashMap<&str, i32> =
        std::collections::HashMap::from([("reviewing", 0), ("debugging", 0), ("refactoring", 0), ("exploring", 0)]);
    for tool in recent {
        for (intent, _tools) in _INTENT_TOOLS {
            if _tools.contains(&tool.as_str()) {
                *scores.entry(*intent).or_default() += 1;
            }
        }
    }

    scores
        .into_iter()
        .max_by_key(|(_, score)| *score)
        .map(|(intent, _)| intent)
        .unwrap_or("exploring")
}

// ---------------------------------------------------------------------------
// Hints generation
// ---------------------------------------------------------------------------

pub fn generate_hints(
    tool_name: &str,
    result: &Value,
    session: &mut SessionState,
) -> Value {
    session.record_tool_call(tool_name);

    let next_steps = build_next_steps(tool_name, session);
    let warnings = extract_warnings(result);
    let related = build_related(tool_name, result, session);

    track_result(result, session);

    let mut hints = Map::new();
    if !next_steps.is_empty() {
        hints.insert("next_steps".into(), json!(next_steps));
    }
    if !related.is_empty() {
        hints.insert("related".into(), json!(related));
    }
    if !warnings.is_empty() {
        hints.insert("warnings".into(), json!(warnings));
    }

    if hints.is_empty() {
        Value::Null
    } else {
        Value::Object(hints)
    }
}

fn build_next_steps(tool_name: &str, session: &SessionState) -> Vec<Value> {
    let called: HashSet<&str> = session.tools_called.iter().map(|t| t.as_str()).collect();
    let candidates = _WORKFLOW.iter().filter(|(n, _)| *n == tool_name).next();
    let suggestions: &[(&str, &str)] = match candidates {
        Some(&(_, s)) => s,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for &(next_tool, suggestion) in suggestions {
        if !called.contains(next_tool) {
            out.push(json!({
                "tool": next_tool,
                "suggestion": suggestion,
            }));
            if out.len() >= _MAX_PER_CATEGORY {
                break;
            }
        }
    }
    out
}

fn extract_warnings(result: &Value) -> Vec<String> {
    let mut warnings = Vec::new();

    // Test gaps
    if let Some(Value::Array(gaps)) = result.get("test_gaps") {
        let names: Vec<String> = gaps.iter().take(5).filter_map(|g| {
            g.as_str().or_else(|| g.get("name").and_then(Value::as_str))
        }).map(String::from).collect();
        if !names.is_empty() {
            warnings.push(format!("Test coverage gaps: {}", names.join(", ")));
        }
    }

    // Risk score
    if let Some(Value::Number(risk)) = result.get("risk_score") {
        if let Some(r) = risk.as_f64() {
            if r > 0.7 {
                warnings.push(format!("High risk score ({:.2}) — review carefully", r));
            }
        }
    }

    // Architecture warnings
    if let Some(Value::Array(arch_warnings)) = result.get("warnings") {
        for w in arch_warnings.iter().take(3) {
            if let Some(s) = w.as_str() {
                warnings.push(s.to_string());
            } else if let Some(obj) = w.as_object() {
                if let Some(msg) = obj.get("message").and_then(Value::as_str) {
                    warnings.push(msg.to_string());
                }
            }
        }
    }

    warnings
}

fn build_related(_tool_name: &str, result: &Value, session: &SessionState) -> Vec<String> {
    let mut related = Vec::new();
    let mut seen = HashSet::new();

    // Suggest impacted files not yet touched
    if let Some(Value::Array(impacted)) = result.get("impacted_files") {
        for item in impacted {
            if let Some(f) = item.as_str() {
                if !session.files_touched.contains(f) && seen.insert(f.to_string()) {
                    related.push(f.to_string());
                    if related.len() >= _MAX_PER_CATEGORY {
                        break;
                    }
                }
            }
        }
    }

    related
}

fn track_result(result: &Value, session: &mut SessionState) {
    // Files
    for key in &["changed_files", "impacted_files", "files"] {
        if let Some(Value::Array(arr)) = result.get(key) {
            let files: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
            session.record_files(&files);
        }
    }

    // Nodes — look in common result shapes
    let mut qnames: Vec<&str> = Vec::new();
    for key in &["results", "changed_nodes", "impacted_nodes", "nodes", "nodes_list"] {
        if let Some(Value::Array(arr)) = result.get(key) {
            for item in arr {
                if let Some(obj) = item.as_object() {
                    if let Some(qn) = obj.get("qualified_name").and_then(Value::as_str) {
                        qnames.push(qn);
                    }
                }
            }
        }
    }
    if !qnames.is_empty() {
        session.record_nodes(&qnames);
    }
}
