//! TUI node-detail formatting and ratatui widget rendering.

use crate::core::{EdgeKind, Graph, NodeId, NodeKind};
use crate::extract::flows::flows_through;
use crate::query::{callees_of, callers_of};

use ratatui::{prelude::*, widgets::*};

use super::app::{App, FlowsPane, SearchPane, Tab};
use super::theme::{focus_color, kind_label, kind_style, trunc};

// ── node detail builder ───────────────────────────────────────────────────────

pub(super) fn build_node_detail(
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

pub(super) fn draw(f: &mut Frame, app: &App) {
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
