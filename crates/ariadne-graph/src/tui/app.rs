//! TUI application state and keyboard event handling.

use crate::core::{EdgeKind, Graph, NodeId, NodeKind};
use crate::extract::flows::all_flows;
use crate::query::{fts_ranked_search, SearchHit};
use crate::store::Store;

use crossterm::event::{self, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui::text::Line;

use super::render::build_node_detail;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Tab {
    Search = 0,
    Flows = 1,
    Browse = 2,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SearchPane {
    Input,
    Results,
    Detail,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum FlowsPane {
    List,
    Members,
}

pub(super) struct App<'a> {
    pub(super) graph: &'a Graph,
    pub(super) store: &'a Store,
    pub(super) tab: Tab,
    pub(super) quit: bool,

    // Search tab
    pub(super) s_pane: SearchPane,
    pub(super) s_input: String,
    pub(super) s_hits: Vec<SearchHit>,
    pub(super) s_list: ListState,
    pub(super) s_detail: Vec<Line<'static>>,
    pub(super) s_scroll: usize,

    // Flows tab
    pub(super) f_pane: FlowsPane,
    pub(super) f_ids: Vec<NodeId>,
    pub(super) f_list: ListState,
    pub(super) f_members: Vec<(NodeId, String, NodeKind)>,
    pub(super) f_mstate: ListState,

    // Browse tab
    pub(super) b_nodes: Vec<(NodeId, String, NodeKind)>,
    pub(super) b_list: ListState,
    pub(super) b_detail: Vec<Line<'static>>,
    pub(super) b_scroll: usize,
    pub(super) b_right: bool,
}

impl<'a> App<'a> {
    pub(super) fn new(store: &'a Store, graph: &'a Graph) -> Self {
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

    pub(super) fn on_key(&mut self, key: event::KeyEvent) {
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
