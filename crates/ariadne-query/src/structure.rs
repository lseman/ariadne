//! Structural graph algorithms for architecture analysis.

use ariadne_core::{Graph, NodeId};
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

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_core::{Edge, EdgeKind, Node, NodeKind};

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
