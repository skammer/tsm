mod app;
mod inspect;
mod view;

use anyhow::Result;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyEventKind},
};

use crate::store::Store;

use self::{app::App, view::render};

pub fn run(store: &Store) -> Result<()> {
    let mut app = App::new(Vec::new());
    app.reload(store)?;
    let mut terminal = ratatui::try_init()?;
    let result = run_loop(&mut terminal, store, &mut app);
    let restored = ratatui::try_restore();
    result?;
    Ok(restored?)
}

fn run_loop(terminal: &mut DefaultTerminal, store: &Store, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && app.handle_key(store, key)
        {
            return Ok(());
        }
    }
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::inspect::snapshot_lines;
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn layout_and_metadata_are_only_visible_when_expanded() {
        let snapshot = crate::model::test_snapshot();
        for expanded in [false, true] {
            let text = snapshot_lines(&snapshot, expanded)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.contains("Layout:"), expanded);
            assert_eq!(text.contains("UUID"), expanded);
            assert_eq!(text.contains("Active:"), expanded);
            assert!(text.contains("/tmp"));
            assert!(text.contains("shell"));
        }
    }

    #[test]
    fn inspector_scrolls_and_small_terminals_render() {
        let mut snapshot = crate::model::test_snapshot();
        let mut last = snapshot.sessions[0].windows[0].panes[0].clone();
        last.title = "LAST PANE".into();
        snapshot.sessions[0].windows[0].panes.resize(20, last);
        let mut app = App::new(vec![snapshot]);
        for (width, height) in [(120, 24), (40, 10), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.focus = app::Focus::Inspector;
            app.scroll = u16::MAX;
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            if width == 120 {
                let buffer = terminal.backend().buffer();
                let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
                assert!(text.contains("LAST PANE"));
                assert!(app.scroll > 0);
            }
        }
        app.filter = "absent".into();
        app.apply_filter(None);
        assert!(app.selected().is_none());
    }
}
