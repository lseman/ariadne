//! Structural graph algorithms for architecture analysis.

use crate::core::{Graph, NodeId};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct Component {
    pub nodes: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub struct CoreNumber {
    pub node: NodeId,
    pub core: usize,
}

#[derive(Debug, Clone)]
pub struct BridgeScore {
    pub node: NodeId,
    pub score: f32,
    pub communities_touched: usize,
    pub degree: usize,
    pub approx_betweenness: f32,
    pub articulation: bool,
}

/// A hub node — highest total degree (in + out), excluding File nodes.
///
/// Hub nodes are architectural hotspots: changes to them have
/// disproportionate blast radius. Different from bridge nodes which
/// use betweenness centrality.
#[derive(Debug, Clone)]
pub struct HubNode {
    pub node: NodeId,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub in_degree: usize,
    pub out_degree: usize,
    pub total_degree: usize,
    pub community_id: Option<usize>,
}

pub fn strongly_connected_components(graph: &Graph) -> Vec<Component> {
    let mut state = TarjanState::default();
    for (node, _) in graph.nodes() {
        if !state.indices.contains_key(&node) {
            strong_connect(graph, node, &mut state);
        }
    }
    state
        .components
        .into_iter()
        .map(|nodes| Component { nodes })
        .collect()
}

pub fn cyclic_components(graph: &Graph) -> Vec<Component> {
    strongly_connected_components(graph)
        .into_iter()
        .filter(|c| {
            c.nodes.len() > 1
                || c.nodes
                    .first()
                    .map(|n| graph.out_neighbors(*n).any(|(m, _)| m == *n))
                    .unwrap_or(false)
        })
        .collect()
}

pub fn core_numbers(graph: &Graph) -> HashMap<NodeId, usize> {
    let adj = undirected_adjacency(graph);
    let mut remaining: HashSet<NodeId> = graph.nodes().map(|(id, _)| id).collect();
    let mut degree: HashMap<NodeId, usize> = remaining
        .iter()
        .map(|id| (*id, adj.get(id).map(HashSet::len).unwrap_or(0)))
        .collect();
    let mut core = HashMap::new();
    let mut current_core = 0usize;

    while !remaining.is_empty() {
        let (&node, &min_degree) = degree
            .iter()
            .filter(|(id, _)| remaining.contains(id))
            .min_by_key(|(_, d)| *d)
            .unwrap();
        current_core = current_core.max(min_degree);
        core.insert(node, current_core);
        remaining.remove(&node);
        if let Some(neighbors) = adj.get(&node) {
            for neighbor in neighbors {
                if remaining.contains(neighbor) {
                    let entry = degree.entry(*neighbor).or_insert(0);
                    *entry = entry.saturating_sub(1);
                }
            }
        }
    }
    core
}

pub fn articulation_points(graph: &Graph) -> HashSet<NodeId> {
    let adj = undirected_adjacency(graph);
    let mut state = ArticulationState::default();
    for (node, _) in graph.nodes() {
        if !state.visited.contains(&node) {
            articulation_dfs(node, None, &adj, &mut state);
        }
    }
    state.points
}

pub fn approx_betweenness(graph: &Graph, max_sources: usize) -> HashMap<NodeId, f32> {
    let adj = undirected_adjacency(graph);
    let sources: Vec<NodeId> = graph.nodes().map(|(id, _)| id).take(max_sources).collect();
    let mut scores: HashMap<NodeId, f32> = HashMap::new();

    for source in sources {
        let mut queue = VecDeque::from([source]);
        let mut pred: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut dist: HashMap<NodeId, usize> = HashMap::from([(source, 0)]);
        let mut order = Vec::new();
        while let Some(node) = queue.pop_front() {
            order.push(node);
            let next_dist = dist[&node] + 1;
            for neighbor in adj.get(&node).into_iter().flatten() {
                if !dist.contains_key(neighbor) {
                    dist.insert(*neighbor, next_dist);
                    queue.push_back(*neighbor);
                }
                if dist.get(neighbor) == Some(&next_dist) {
                    pred.entry(*neighbor).or_default().push(node);
                }
            }
        }

        let mut dependency: HashMap<NodeId, f32> = HashMap::new();
        for node in order.into_iter().rev() {
            let coeff = (1.0 + dependency.get(&node).copied().unwrap_or(0.0))
                / pred.get(&node).map(Vec::len).unwrap_or(1).max(1) as f32;
            for p in pred.get(&node).into_iter().flatten() {
                *dependency.entry(*p).or_insert(0.0) += coeff;
            }
            if node != source {
                *scores.entry(node).or_insert(0.0) += dependency.get(&node).copied().unwrap_or(0.0);
            }
        }
    }
    scores
}

/// Find the most connected nodes (highest in+out degree), excluding File nodes.
///
/// Hub nodes are architectural hotspots -- changes to them have
/// disproportionate blast radius. Different from bridge nodes which
/// use betweenness centrality.
pub fn hub_nodes(graph: &Graph, top_n: usize) -> Vec<HubNode> {
    // Build degree counts from all edges
    let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
    let mut out_degree: HashMap<NodeId, usize> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        *out_degree.entry(src).or_default() += 1;
        *in_degree.entry(dst).or_default() += 1;
    }

    let mut scored: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.kind != crate::core::NodeKind::File)
        .filter_map(|(id, n)| {
            let ind = in_degree.get(&id).copied().unwrap_or(0);
            let outd = out_degree.get(&id).copied().unwrap_or(0);
            let total = ind + outd;
            if total == 0 {
                return None;
            }
            Some(HubNode {
                node: id,
                name: n.name.clone(),
                qualified_name: n.qualified_name.clone(),
                kind: n.kind.as_str().to_string(),
                file: n.source_uri.clone().unwrap_or_default(),
                in_degree: ind,
                out_degree: outd,
                total_degree: total,
                community_id: None,
            })
        })
        .collect();
    scored.sort_by_key(|h| std::cmp::Reverse(h.total_degree));
    scored.truncate(top_n);
    scored
}

pub fn bridge_scores(
    graph: &Graph,
    communities: &HashMap<NodeId, usize>,
    limit: usize,
) -> Vec<BridgeScore> {
    let articulation = articulation_points(graph);
    let between = approx_betweenness(graph, 64);
    let mut rows: Vec<_> = graph
        .nodes()
        .map(|(id, _)| {
            let mut touched = HashSet::new();
            for (neighbor, _) in graph.out_neighbors(id).chain(graph.in_neighbors(id)) {
                if let Some(c) = communities.get(&neighbor) {
                    touched.insert(*c);
                }
            }
            let degree = graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
            let approx = between.get(&id).copied().unwrap_or(0.0);
            let is_articulation = articulation.contains(&id);
            let score = touched.len() as f32 * 3.0
                + degree as f32
                + approx.sqrt()
                + if is_articulation { 12.0 } else { 0.0 };
            BridgeScore {
                node: id,
                score,
                communities_touched: touched.len(),
                degree,
                approx_betweenness: approx,
                articulation: is_articulation,
            }
        })
        .filter(|row| row.score > 0.0)
        .collect();
    rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    rows.truncate(limit);
    rows
}

fn undirected_adjacency(graph: &Graph) -> HashMap<NodeId, HashSet<NodeId>> {
    let mut adj: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        if src == dst {
            continue;
        }
        adj.entry(src).or_default().insert(dst);
        adj.entry(dst).or_default().insert(src);
    }
    adj
}

#[derive(Default)]
struct TarjanState {
    index: usize,
    indices: HashMap<NodeId, usize>,
    lowlink: HashMap<NodeId, usize>,
    stack: Vec<NodeId>,
    on_stack: HashSet<NodeId>,
    components: Vec<Vec<NodeId>>,
}

fn strong_connect(graph: &Graph, node: NodeId, state: &mut TarjanState) {
    state.indices.insert(node, state.index);
    state.lowlink.insert(node, state.index);
    state.index += 1;
    state.stack.push(node);
    state.on_stack.insert(node);

    for (neighbor, _) in graph.out_neighbors(node) {
        if !state.indices.contains_key(&neighbor) {
            strong_connect(graph, neighbor, state);
            let low = state.lowlink[&node].min(state.lowlink[&neighbor]);
            state.lowlink.insert(node, low);
        } else if state.on_stack.contains(&neighbor) {
            let low = state.lowlink[&node].min(state.indices[&neighbor]);
            state.lowlink.insert(node, low);
        }
    }

    if state.lowlink[&node] == state.indices[&node] {
        let mut component = Vec::new();
        while let Some(w) = state.stack.pop() {
            state.on_stack.remove(&w);
            component.push(w);
            if w == node {
                break;
            }
        }
        state.components.push(component);
    }
}

#[derive(Default)]
struct ArticulationState {
    visited: HashSet<NodeId>,
    discovery: HashMap<NodeId, usize>,
    low: HashMap<NodeId, usize>,
    time: usize,
    points: HashSet<NodeId>,
}

fn articulation_dfs(
    node: NodeId,
    parent: Option<NodeId>,
    adj: &HashMap<NodeId, HashSet<NodeId>>,
    state: &mut ArticulationState,
) {
    state.visited.insert(node);
    state.discovery.insert(node, state.time);
    state.low.insert(node, state.time);
    state.time += 1;
    let mut children = 0usize;

    for neighbor in adj.get(&node).into_iter().flatten() {
        if Some(*neighbor) == parent {
            continue;
        }
        if !state.visited.contains(neighbor) {
            children += 1;
            articulation_dfs(*neighbor, Some(node), adj, state);
            let low = state.low[&node].min(state.low[neighbor]);
            state.low.insert(node, low);
            if parent.is_some() && state.low[neighbor] >= state.discovery[&node] {
                state.points.insert(node);
            }
        } else {
            let low = state.low[&node].min(state.discovery[neighbor]);
            state.low.insert(node, low);
        }
    }

    if parent.is_none() && children > 1 {
        state.points.insert(node);
    }
}

/// Call-resolution coverage: how many `Calls` edges land on real
/// definitions versus unresolved `call::*` placeholders.
///
/// This is the single number that tells you whether the graph's
/// reachability-based queries (paths, impact, flows, counterfactual) can
/// be trusted. A low rate means many call sites dead-end at placeholders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallResolution {
    pub resolved: usize,
    pub unresolved: usize,
}

impl CallResolution {
    pub fn total(&self) -> usize {
        self.resolved + self.unresolved
    }

    /// Fraction of call edges that reach a real definition, in `[0, 1]`.
    /// Returns 1.0 when there are no call edges at all.
    pub fn rate(&self) -> f32 {
        let total = self.total();
        if total == 0 {
            return 1.0;
        }
        self.resolved as f32 / total as f32
    }
}

pub fn call_resolution_stats(graph: &Graph) -> CallResolution {
    use crate::core::EdgeKind;
    let mut resolved = 0;
    let mut unresolved = 0;
    for (_, _, dst, edge) in graph.edges() {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        let is_placeholder = graph
            .node(dst)
            .map(|n| n.qualified_name.starts_with("call::"))
            .unwrap_or(false);
        if is_placeholder {
            unresolved += 1;
        } else {
            resolved += 1;
        }
    }
    CallResolution {
        resolved,
        unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn call_resolution_counts_placeholders() {
        let mut g = Graph::new();
        let caller = g.add_node(Node::new(NodeKind::Function, "caller"));
        let real = g.add_node(Node::new(NodeKind::Function, "real"));
        let placeholder = g.add_node(Node::new(NodeKind::Function, "call::external"));
        g.add_edge(caller, real, Edge::extracted(EdgeKind::Calls));
        g.add_edge(caller, placeholder, Edge::ambiguous(EdgeKind::Calls));

        let stats = call_resolution_stats(&g);
        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.unresolved, 1);
        assert_eq!(stats.rate(), 0.5);
    }

    #[test]
    fn finds_cycle_and_articulation() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "a"));
        let b = g.add_node(Node::new(NodeKind::Function, "b"));
        let c = g.add_node(Node::new(NodeKind::Function, "c"));
        let d = g.add_node(Node::new(NodeKind::Function, "d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        assert_eq!(cyclic_components(&g).len(), 1);
        assert!(articulation_points(&g).contains(&c));
    }
}
