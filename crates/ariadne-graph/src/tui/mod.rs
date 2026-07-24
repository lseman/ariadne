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

use crate::core::Graph;
use crate::store::Store;

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::stdout;

mod app;
mod render;
mod theme;

use app::App;
use render::draw;

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
