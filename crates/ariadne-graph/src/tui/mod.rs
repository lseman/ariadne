//! Interactive terminal UI for navigating the Ariadne graph.
//!
//! Launch with `ariadne --db path.db tui`.
//!
//! Three tabs:
//! - **Search** — live FTS5 + ranked search; results list + node detail panel.
//! - **Flows** — all execution flows ranked by criticality; members list.
//! - **Browse** — full node list; detail panel with callers / callees / flows.
//!
//! Keybindings:
//! - `1` / `2` / `3` — switch tabs
//! - `/` or `i` — focus search input
//! - `↑` / `↓` or `j` / `k` — navigate lists
//! - `Tab` / `→` / `←` — move between panes
//! - `Enter` — select / confirm
//! - `g` — jump to selected node in Browse tab
//! - `q` / `Ctrl-C` — quit

use crate::core::{EdgeKind, Graph, NodeId, NodeKind};
use crate::extract::flows::{all_flows, flows_through};
use crate::query::{callees_of, callers_of, fts_ranked_search, SearchHit};
use crate::store::Store;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use std::io::stdout;

mod theme;
use theme::{focus_color, kind_label, kind_style, trunc};

// ── identifiers ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Search = 0,
    Flows = 1,
    Browse = 2,
}

#[derive(Clone, Copy, PartialEq)]
enum SearchPane {
    Input,
    Results,
    Detail,
}

#[derive(Clone, Copy, PartialEq)]
enum FlowsPane {
    List,
    Members,
}

// ── application state ─────────────────────────────────────────────────────────

struct App<'a> {
    graph: &'a Graph,
    store: &'a Store,
    tab: Tab,
    quit: bool,

    // Search tab
    s_pane: SearchPane,
    s_input: String,
    s_hits: Vec<SearchHit>,
    s_list: ListState,
    s_detail: Vec<Line<'static>>,
    s_scroll: usize,

    // Flows tab
    f_pane: FlowsPane,
    f_ids: Vec<NodeId>,
    f_list: ListState,
    f_members: Vec<(NodeId, String, NodeKind)>,
    f_mstate: ListState,

    // Browse tab
    b_nodes: Vec<(NodeId, String, NodeKind)>,
    b_list: ListState,
    b_detail: Vec<Line<'static>>,
    b_scroll: usize,
    b_right: bool,
}

impl<'a> App<'a> {
    fn new(store: &'a Store, graph: &'a Graph) -> Self {
        let mut b_nodes: Vec<(NodeId, String, NodeKind)> = graph
            .nodes()
            .filter(|(_, n)| !matches!(n.kind, NodeKind::Flow))
            .filter(|(_, n)| !n.qualified_name.starts_with("call::"))
            .map(|(id, n)| (id, n.qualified_name.clone(), n.kind))
            .collect();
        b_nodes.sort_by(|a, b| a.1.cmp(&b.1));

        let f_ids = all_flows(graph);

        let mut app = App {
            graph,
            store,
            tab: Tab::Search,
            quit: false,

            s_pane: SearchPane::Input,
            s_input: String::new(),
            s_hits: Vec::new(),
            s_list: ListState::default(),
            s_detail: Vec::new(),
            s_scroll: 0,

            f_pane: FlowsPane::List,
            f_ids,
            f_list: ListState::default(),
            f_members: Vec::new(),
            f_mstate: ListState::default(),

            b_nodes,
            b_list: ListState::default(),
            b_detail: Vec::new(),
            b_scroll: 0,
            b_right: false,
        };

        if !app.b_nodes.is_empty() {
            app.b_list.select(Some(0));
            app.refresh_browse_detail();
        }
        if !app.f_ids.is_empty() {
            app.f_list.select(Some(0));
            app.refresh_flow_members();
        }
        app
    }

    // ── search ─────────────────────────────────────────────────────────────────

    fn run_search(&mut self) {
        let q = self.s_input.trim().to_string();
        if q.is_empty() {
            self.s_hits.clear();
            self.s_list.select(None);
            self.s_detail.clear();
            return;
        }
        self.s_hits = fts_ranked_search(self.store, self.graph, &q, 60);
        if !self.s_hits.is_empty() {
            self.s_list.select(Some(0));
            self.refresh_search_detail();
        } else {
            self.s_list.select(None);
            self.s_detail.clear();
        }
    }

    fn refresh_search_detail(&mut self) {
        let Some(idx) = self.s_list.selected() else {
            self.s_detail.clear();
            return;
        };
        let Some(hit) = self.s_hits.get(idx) else {
            self.s_detail.clear();
            return;
        };
        self.s_detail = build_node_detail(self.graph, hit.id, Some(&hit.signals));
        self.s_scroll = 0;
    }

    // ── flows ──────────────────────────────────────────────────────────────────

    fn refresh_flow_members(&mut self) {
        let Some(idx) = self.f_list.selected() else {
            self.f_members.clear();
            return;
        };
        let Some(&flow_id) = self.f_ids.get(idx) else {
            return;
        };
        let mut members: Vec<(NodeId, String, NodeKind)> = self
            .graph
            .in_neighbors(flow_id)
            .filter(|(_, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
            .filter_map(|(id, _)| {
                self.graph
                    .node(id)
                    .map(|n| (id, n.qualified_name.clone(), n.kind))
            })
            .collect();
        members.sort_by(|a, b| a.1.cmp(&b.1));
        self.f_members = members;
        if !self.f_members.is_empty() {
            self.f_mstate.select(Some(0));
        }
    }

    // ── browse ─────────────────────────────────────────────────────────────────

    fn refresh_browse_detail(&mut self) {
        let Some(idx) = self.b_list.selected() else {
            self.b_detail.clear();
            return;
        };
        let Some(&(id, _, _)) = self.b_nodes.get(idx) else {
            return;
        };
        self.b_detail = build_node_detail(self.graph, id, None);
        self.b_scroll = 0;
    }

    // ── list helpers ───────────────────────────────────────────────────────────

    fn list_down(state: &mut ListState, len: usize) {
        if len == 0 {
            return;
        }
        let next = state.selected().map(|i| (i + 1).min(len - 1)).unwrap_or(0);
        state.select(Some(next));
    }

    fn list_up(state: &mut ListState) {
        let next = state.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
        state.select(Some(next));
    }

    fn list_page_down(state: &mut ListState, len: usize, page: usize) {
        if len == 0 {
            return;
        }
        let next = state
            .selected()
            .map(|i| (i + page).min(len - 1))
            .unwrap_or(0);
        state.select(Some(next));
    }

    fn list_page_up(state: &mut ListState, page: usize) {
        let next = state
            .selected()
            .map(|i| i.saturating_sub(page))
            .unwrap_or(0);
        state.select(Some(next));
    }

    // ── jump to node in Browse ─────────────────────────────────────────────────

    fn goto_node_in_browse(&mut self, id: NodeId) {
        if let Some(idx) = self.b_nodes.iter().position(|(bid, _, _)| *bid == id) {
            self.b_list.select(Some(idx));
            self.refresh_browse_detail();
            self.tab = Tab::Browse;
            self.b_right = false;
        }
    }

    // ── event routing ──────────────────────────────────────────────────────────

    fn on_key(&mut self, key: event::KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        // Global: quit and tab switching (except when typing in input)
        if self.s_pane != SearchPane::Input {
            match key.code {
                KeyCode::Char('q') if key.modifiers.is_empty() => {
                    self.quit = true;
                    return;
                }
                _ => {}
            }
        }
        if key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            self.quit = true;
            return;
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char('1') => {
                    self.tab = Tab::Search;
                    return;
                }
                KeyCode::Char('2') => {
                    self.tab = Tab::Flows;
                    return;
                }
                KeyCode::Char('3') => {
                    self.tab = Tab::Browse;
                    return;
                }
                _ => {}
            }
        }

        match self.tab {
            Tab::Search => self.on_key_search(key),
            Tab::Flows => self.on_key_flows(key),
            Tab::Browse => self.on_key_browse(key),
        }
    }

    fn on_key_search(&mut self, key: event::KeyEvent) {
        match self.s_pane {
            SearchPane::Input => match key.code {
                KeyCode::Esc if !self.s_hits.is_empty() => {
                    self.s_pane = SearchPane::Results;
                }
                KeyCode::Tab | KeyCode::Down if !self.s_hits.is_empty() => {
                    self.s_pane = SearchPane::Results;
                }
                KeyCode::Enter => {
                    self.run_search();
                    if !self.s_hits.is_empty() {
                        self.s_pane = SearchPane::Results;
                    }
                }
                KeyCode::Char(c) => {
                    self.s_input.push(c);
                    self.run_search();
                }
                KeyCode::Backspace => {
                    self.s_input.pop();
                    self.run_search();
                }
                _ => {}
            },
            SearchPane::Results => match key.code {
                KeyCode::Char('/') | KeyCode::Char('i') => {
                    self.s_pane = SearchPane::Input;
                }
                KeyCode::Esc => {
                    self.s_pane = SearchPane::Input;
                }
                KeyCode::Tab if !self.s_detail.is_empty() => {
                    self.s_pane = SearchPane::Detail;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    Self::list_down(&mut self.s_list, self.s_hits.len());
                    self.refresh_search_detail();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    Self::list_up(&mut self.s_list);
                    self.refresh_search_detail();
                }
                KeyCode::PageDown => {
                    Self::list_page_down(&mut self.s_list, self.s_hits.len(), 10);
                    self.refresh_search_detail();
                }
                KeyCode::PageUp => {
                    Self::list_page_up(&mut self.s_list, 10);
                    self.refresh_search_detail();
                }
                KeyCode::Char('g') => {
                    if let Some(idx) = self.s_list.selected() {
                        if let Some(hit) = self.s_hits.get(idx) {
                            let id = hit.id;
                            self.goto_node_in_browse(id);
                        }
                    }
                }
                _ => {}
            },
            SearchPane::Detail => match key.code {
                KeyCode::Tab | KeyCode::Esc => {
                    self.s_pane = SearchPane::Results;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.s_scroll = self.s_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.s_scroll = self.s_scroll.saturating_sub(1);
                }
                _ => {}
            },
        }
    }

    fn on_key_flows(&mut self, key: event::KeyEvent) {
        match self.f_pane {
            FlowsPane::List => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    Self::list_down(&mut self.f_list, self.f_ids.len());
                    self.refresh_flow_members();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    Self::list_up(&mut self.f_list);
                    self.refresh_flow_members();
                }
                KeyCode::PageDown => {
                    Self::list_page_down(&mut self.f_list, self.f_ids.len(), 10);
                    self.refresh_flow_members();
                }
                KeyCode::PageUp => {
                    Self::list_page_up(&mut self.f_list, 10);
                    self.refresh_flow_members();
                }
                KeyCode::Tab | KeyCode::Right | KeyCode::Enter if !self.f_members.is_empty() => {
                    self.f_pane = FlowsPane::Members;
                }
                _ => {}
            },
            FlowsPane::Members => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    Self::list_down(&mut self.f_mstate, self.f_members.len());
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    Self::list_up(&mut self.f_mstate);
                }
                KeyCode::Tab | KeyCode::Left | KeyCode::Esc => {
                    self.f_pane = FlowsPane::List;
                }
                KeyCode::Char('g') | KeyCode::Enter => {
                    if let Some(idx) = self.f_mstate.selected() {
                        if let Some(&(mid, _, _)) = self.f_members.get(idx) {
                            self.goto_node_in_browse(mid);
                        }
                    }
                }
                _ => {}
            },
        }
    }

    fn on_key_browse(&mut self, key: event::KeyEvent) {
        if !self.b_right {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    Self::list_down(&mut self.b_list, self.b_nodes.len());
                    self.refresh_browse_detail();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    Self::list_up(&mut self.b_list);
                    self.refresh_browse_detail();
                }
                KeyCode::PageDown => {
                    Self::list_page_down(&mut self.b_list, self.b_nodes.len(), 15);
                    self.refresh_browse_detail();
                }
                KeyCode::PageUp => {
                    Self::list_page_up(&mut self.b_list, 15);
                    self.refresh_browse_detail();
                }
                KeyCode::Tab | KeyCode::Right if !self.b_detail.is_empty() => {
                    self.b_right = true;
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.b_scroll = self.b_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.b_scroll = self.b_scroll.saturating_sub(1);
                }
                KeyCode::Tab | KeyCode::Left | KeyCode::Esc => {
                    self.b_right = false;
                }
                _ => {}
            }
        }
    }
}

// ── node detail builder ───────────────────────────────────────────────────────

fn build_node_detail(
    graph: &Graph,
    id: NodeId,
    search_signals: Option<&[&'static str]>,
) -> Vec<Line<'static>> {
    let Some(node) = graph.node(id) else {
        return vec![];
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header
    lines.push(Line::from(Span::styled(
        node.qualified_name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());

    // Kind
    lines.push(Line::from(vec![
        Span::styled("Kind:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(kind_label(node.kind), kind_style(node.kind)),
    ]));

    if let Some(signals) = search_signals {
        if !signals.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Signals: ", Style::default().fg(Color::DarkGray)),
                Span::styled(signals.join(", "), Style::default().fg(Color::LightCyan)),
            ]));
        }
    }

    // Source location
    if let Some(src) = &node.source_uri {
        let loc = if let Some(ls) = node.line_start {
            format!("{}:{}", src, ls + 1)
        } else {
            src.clone()
        };
        lines.push(Line::from(vec![
            Span::styled("Source:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(loc, Style::default().fg(Color::Blue)),
        ]));
    }

    lines.push(Line::default());

    let tests: Vec<_> = graph
        .out_neighbors(id)
        .filter(|(_, edge)| edge.kind == EdgeKind::TestedBy)
        .filter_map(|(test_id, _)| graph.node(test_id).map(|node| node.qualified_name.clone()))
        .collect();
    lines.push(Line::from(Span::styled(
        format!("── Tests ({}) ───────────────", tests.len()),
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )));
    if tests.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no direct TestedBy coverage)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for test in tests.iter().take(10) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(test.clone(), Style::default().fg(Color::Green)),
            ]));
        }
        if tests.len() > 10 {
            lines.push(Line::from(Span::styled(
                format!("  … {} more", tests.len() - 10),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    lines.push(Line::default());

    // Callers
    let callers = callers_of(graph, id);
    lines.push(Line::from(Span::styled(
        format!("── Callers ({}) ─────────────", callers.len()),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    if callers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for &cid in callers.iter().take(20) {
        if let Some(cn) = graph.node(cid) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(cn.qualified_name.clone(), kind_style(cn.kind)),
            ]));
        }
    }
    if callers.len() > 20 {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", callers.len() - 20),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::default());

    // Callees
    let callees = callees_of(graph, id);
    lines.push(Line::from(Span::styled(
        format!("── Callees ({}) ─────────────", callees.len()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if callees.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for &cid in callees.iter().take(20) {
        if let Some(cn) = graph.node(cid) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(cn.qualified_name.clone(), kind_style(cn.kind)),
            ]));
        }
    }
    if callees.len() > 20 {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", callees.len() - 20),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::default());

    // Flows
    let node_flows = flows_through(graph, id);
    if !node_flows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("── Flows ({}) ───────────────", node_flows.len()),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        for &fid in node_flows.iter().take(8) {
            if let Some(fn_) = graph.node(fid) {
                let crit = fn_
                    .properties
                    .get("criticality")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let short_name = fn_
                    .qualified_name
                    .strip_prefix("flow::")
                    .unwrap_or(&fn_.qualified_name);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(short_name.to_string(), Style::default().fg(Color::Magenta)),
                    Span::styled(
                        format!("  {:.2}", crit),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
    }

    lines
}

// ── drawing ───────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);

    draw_tab_bar(f, app, chunks[0]);
    match app.tab {
        Tab::Search => draw_search(f, app, chunks[1]),
        Tab::Flows => draw_flows(f, app, chunks[1]),
        Tab::Browse => draw_browse(f, app, chunks[1]),
    }
    draw_status(f, app, chunks[2]);
}

fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let titles = vec![" Search (1) ", " Flows (2) ", " Browse (3) "];
    let tabs = Tabs::new(titles)
        .select(app.tab as usize)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    f.render_widget(tabs, area);
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);

    // Input
    let focused = app.s_pane == SearchPane::Input;
    let cursor = if focused { "█" } else { "" };
    let input_para = Paragraph::new(format!("> {}{}", app.s_input, cursor)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Search ")
            .border_style(focus_color(focused)),
    );
    f.render_widget(input_para, rows[0]);

    // Results + detail
    let cols =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(rows[1]);

    let results_focused = app.s_pane == SearchPane::Results;
    let items: Vec<ListItem> = app
        .s_hits
        .iter()
        .map(|h| {
            let (qname, kind) = app
                .graph
                .node(h.id)
                .map(|n| (n.qualified_name.as_str(), n.kind))
                .unwrap_or(("?", NodeKind::Function));
            let kl = kind_label(kind);
            let label = format!("{:<46} {:>4}  {:>5.0}", trunc(qname, 46), kl, h.score,);
            ListItem::new(label).style(kind_style(kind))
        })
        .collect();
    let results_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Results ({}) ", app.s_hits.len()))
                .border_style(focus_color(results_focused)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(results_list, cols[0], &mut app.s_list.clone());

    let detail_focused = app.s_pane == SearchPane::Detail;
    let visible: Vec<Line> = app.s_detail.iter().skip(app.s_scroll).cloned().collect();
    let detail = Paragraph::new(Text::from(visible)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Details ")
            .border_style(focus_color(detail_focused)),
    );
    f.render_widget(detail, cols[1]);
}

fn draw_flows(f: &mut Frame, app: &App, area: Rect) {
    let cols =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);

    let list_focused = app.f_pane == FlowsPane::List;
    let items: Vec<ListItem> = app
        .f_ids
        .iter()
        .map(|&id| {
            if let Some(n) = app.graph.node(id) {
                let name = n
                    .qualified_name
                    .strip_prefix("flow::")
                    .unwrap_or(&n.qualified_name);
                let crit = n
                    .properties
                    .get("criticality")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let size = n
                    .properties
                    .get("node_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let is_test = n
                    .properties
                    .get("is_test_flow")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let style = if is_test {
                    Style::default().fg(Color::Magenta)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                ListItem::new(format!("{:<42} {:.2}  {:>3}", trunc(name, 42), crit, size,))
                    .style(style)
            } else {
                ListItem::new("???")
            }
        })
        .collect();
    let flows_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Flows ({})  name · crit · size ", app.f_ids.len()))
                .border_style(focus_color(list_focused)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(flows_list, cols[0], &mut app.f_list.clone());

    let members_focused = app.f_pane == FlowsPane::Members;
    let mitems: Vec<ListItem> = app
        .f_members
        .iter()
        .map(|(_, qname, kind)| {
            ListItem::new(format!("{:<52} {}", trunc(qname, 52), kind_label(*kind)))
                .style(kind_style(*kind))
        })
        .collect();
    let members_list = List::new(mitems)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Members ({}) ", app.f_members.len()))
                .border_style(focus_color(members_focused)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(members_list, cols[1], &mut app.f_mstate.clone());
}

fn draw_browse(f: &mut Frame, app: &App, area: Rect) {
    let cols =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(area);

    let list_focused = !app.b_right;
    let items: Vec<ListItem> = app
        .b_nodes
        .iter()
        .map(|(_, qname, kind)| {
            ListItem::new(format!("{:<52} {}", trunc(qname, 52), kind_label(*kind)))
                .style(kind_style(*kind))
        })
        .collect();
    let node_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Nodes ({}) ", app.b_nodes.len()))
                .border_style(focus_color(list_focused)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(node_list, cols[0], &mut app.b_list.clone());

    let detail_focused = app.b_right;
    let visible: Vec<Line> = app.b_detail.iter().skip(app.b_scroll).cloned().collect();
    let detail = Paragraph::new(Text::from(visible)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Details ")
            .border_style(focus_color(detail_focused)),
    );
    f.render_widget(detail, cols[1]);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let help = match app.tab {
        Tab::Search => match app.s_pane {
            SearchPane::Input => {
                " type to search  ↓/Tab: results  Esc: blur  1/2/3: tabs  Ctrl+Q: quit "
            }
            SearchPane::Results => {
                " ↑↓/jk: nav  Tab: details  g: goto node  /: input  q/Ctrl+Q: quit "
            }
            SearchPane::Detail => " ↑↓/jk: scroll  Tab: back  q/Ctrl+Q: quit ",
        },
        Tab::Flows => match app.f_pane {
            FlowsPane::List => " ↑↓/jk: nav  Tab/→: members  1/2/3: tabs  q/Ctrl+Q: quit ",
            FlowsPane::Members => " ↑↓/jk: nav  Tab/←: back  g/Enter: goto node  q/Ctrl+Q: quit ",
        },
        Tab::Browse => {
            if app.b_right {
                " ↑↓/jk: scroll  Tab/←: back  q/Ctrl+Q: quit "
            } else {
                " ↑↓/jk: nav  Tab/→: details  1/2/3: tabs  q/Ctrl+Q: quit "
            }
        }
    };
    let status = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, area);
}

// ── public entry point ────────────────────────────────────────────────────────

/// Launch the interactive TUI. Blocks until the user quits.
pub fn run(store: &Store, graph: &Graph) -> anyhow::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = App::new(store, graph);

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(std::time::Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}
