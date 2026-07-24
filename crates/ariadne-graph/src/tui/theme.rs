use crate::core::NodeKind;
use ratatui::style::{Color, Modifier, Style};

pub(super) fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "fn",
        NodeKind::Method => "meth",
        NodeKind::Class => "cls",
        NodeKind::Module => "mod",
        NodeKind::Flow => "flow",
        NodeKind::Trait => "trait",
        NodeKind::Impl => "impl",
        NodeKind::Type => "type",
        NodeKind::Variable => "var",
        NodeKind::File => "file",
        _ => "·",
    }
}

pub(super) fn kind_style(kind: NodeKind) -> Style {
    match kind {
        NodeKind::Function => Style::default().fg(Color::Cyan),
        NodeKind::Method => Style::default().fg(Color::LightBlue),
        NodeKind::Class => Style::default().fg(Color::Green),
        NodeKind::Module => Style::default().fg(Color::Yellow),
        NodeKind::Flow => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        NodeKind::Trait => Style::default().fg(Color::LightGreen),
        NodeKind::Impl => Style::default().fg(Color::LightYellow),
        NodeKind::Type => Style::default().fg(Color::LightCyan),
        _ => Style::default(),
    }
}

pub(super) fn focus_color(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Yellow
    } else {
        Color::DarkGray
    })
}

pub(super) fn trunc(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max);
    for (count, c) in s.chars().enumerate() {
        if count >= max.saturating_sub(1) {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}
