mod app;
mod inspect;
mod view;

use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyEventKind},
};

use crate::{store::Store, tmux::RestoreReport};

use self::{app::App, view::render};

pub fn run(store: &Store) -> Result<()> {
    let mut app = App::new(Vec::new());
    app.reload(store)?;
    let mut terminal = ratatui::try_init()?;
    let result = run_loop(&mut terminal, store, &mut app);
    let restored = ratatui::try_restore();
    let jump = result?;
    restored?;
    if let Some(report) = jump {
        for failure in &report.failures {
            eprintln!("{failure}");
        }
        report
            .jump_target
            .jump()
            .with_context(|| format!("Restored: {}\nJump failed", report.targets.join(", ")))?;
    }
    Ok(())
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    store: &Store,
    app: &mut App,
) -> Result<Option<RestoreReport>> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        let restoring = app.restore_working();
        if (!restoring || event::poll(Duration::from_millis(50))?)
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && app.handle_key(store, key)
        {
            return Ok(None);
        }
        if restoring && let Some(report) = app.poll_restore() {
            return Ok(Some(report));
        }
    }
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn display_value(value: &str) -> String {
    let mut displayed = String::new();
    for ch in value.chars() {
        if ch.is_control() {
            displayed.extend(ch.escape_default());
        } else {
            displayed.push(ch);
        }
    }
    displayed
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{
        app::{Focus, Mode, Status},
        inspect::{Outline, Section, snapshot_lines},
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn inspector_previews_replay_before_expanding_pane_details() {
        let mut snapshot = crate::model::test_snapshot();
        let window = &mut snapshot.sessions[0].windows[0];
        window.panes[0].command_line = "old command".into();
        window.panes[0].argv = vec!["/usr/bin/vim".into(), "work notes.md".into()];
        let mut outline = Outline::default();
        let document = snapshot_lines(&snapshot, &outline, false);
        assert_eq!(
            document
                .headings
                .iter()
                .map(|(section, _)| *section)
                .collect::<Vec<_>>(),
            [Section::Session(0), Section::Window(0, 0)]
        );
        let text = document
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("* Window 3"));
        assert!(text.contains("1 pane"));
        assert!(text.contains("* /usr/bin/vim 'work notes.md'"));
        assert!(!text.contains("old command"));
        assert!(text.contains("cwd: /tmp"));

        outline.sync(document.headings.iter().map(|(section, _)| *section));
        outline.move_selection(1);
        outline.expand();
        for details in [false, true] {
            let text = snapshot_lines(&snapshot, &outline, details)
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.contains("Layout:"), details);
            assert_eq!(text.contains("UUID"), details);
            assert_eq!(text.contains("Saved "), details);
            assert!(text.contains("* Pane 0"));
            assert!(text.contains("/tmp"));
            assert!(text.contains("/usr/bin/vim 'work notes.md'"));
        }
        outline.collapse();
        outline.collapse();
        assert_eq!(outline.selected(), Some(Section::Session(0)));
        outline.collapse();
        let collapsed = snapshot_lines(&snapshot, &outline, false);
        assert_eq!(collapsed.headings.len(), 1);
        outline.expand();
        assert_eq!(snapshot_lines(&snapshot, &outline, false).headings.len(), 2);
    }

    #[test]
    fn inspector_scrolls_and_small_terminals_render() {
        let mut snapshot = crate::model::test_snapshot();
        let mut last = snapshot.sessions[0].windows[0].panes[0].clone();
        last.title = "LAST PANE".into();
        snapshot.sessions[0].windows[0].panes.resize(20, last);
        let mut app = App::new(vec![snapshot]);
        app.outline
            .sync([Section::Session(0), Section::Window(0, 0)]);
        app.outline.move_selection(1);
        app.outline.expand();
        app.outline.reveal = false;
        for (width, height) in [(120, 24), (40, 10), (1, 1)] {
            app.focus = Focus::Inspector;
            app.scroll = u16::MAX;
            let screen = draw(&mut app, width, height);
            if width == 120 {
                assert!(screen.contains("LAST PANE"));
                assert!(app.scroll > 0);
            }
        }
        app.filter = "absent".into();
        app.apply_filter(None);
        assert!(app.selected().is_none());
    }

    #[test]
    fn inspector_shows_shared_cwd_once_at_the_highest_level() {
        let mut snapshot = crate::model::test_snapshot();
        let mut second_window = snapshot.sessions[0].windows[0].clone();
        second_window.index += 1;
        snapshot.sessions[0].windows.push(second_window);
        let mut second_session = snapshot.sessions[0].clone();
        second_session.name = "other".into();
        snapshot.scope = crate::model::Scope::All;
        snapshot.sessions.push(second_session);

        let mut outline = Outline::default();
        let document = snapshot_lines(&snapshot, &outline, true);
        outline.sync(document.headings.iter().map(|(section, _)| *section));
        outline.move_selection(1);
        outline.expand();
        let directories = |snapshot: &crate::model::Snapshot, details| {
            snapshot_lines(snapshot, &outline, details)
                .lines
                .iter()
                .map(ToString::to_string)
                .filter(|line| line.contains("cwd:"))
                .collect::<Vec<_>>()
        };
        for details in [false, true] {
            assert_eq!(directories(&snapshot, details), ["cwd: /tmp"]);
        }

        for window in &mut snapshot.sessions[0].windows {
            window.panes[0].cwd = "/work/session".into();
        }
        let mixed_window = &mut snapshot.sessions[1].windows[1];
        let mut second_pane = mixed_window.panes[0].clone();
        mixed_window.panes[0].cwd = "/work/editor".into();
        second_pane.cwd = "/work/logs".into();
        mixed_window.panes.push(second_pane);
        for details in [false, true] {
            assert_eq!(
                directories(&snapshot, details),
                [
                    "  cwd: /work/session",
                    "    cwd: /tmp",
                    "    * Interactive shell · cwd: /work/editor",
                    "      Interactive shell · cwd: /work/logs",
                ]
            );
        }
    }

    #[test]
    fn short_terminal_keeps_confirmation_and_error_details_accessible() {
        let mut snapshot = crate::model::test_snapshot();
        snapshot.name = "Personal projects and terminal automation with a long name".into();
        let mut app = App::new(vec![snapshot]);
        app.mode = Mode::Delete;
        let screen = draw(&mut app, 32, 6);
        assert!(screen.contains("Delete snapshot?"));
        assert!(screen.contains("y confirm · n/Esc cancel"));

        app.mode = Mode::Browse;
        let path = "/a/long/path/to/a/deep/worktree/that/has/a/missing/final-directory";
        app.status = Some(Status {
            message: "Restore failed".into(),
            details: Some(format!("Saved directory is unavailable\n{path}")),
        });
        assert!(draw(&mut app, 32, 6).contains("e details · Restore failed"));
        app.mode = Mode::ErrorDetails { scroll: u16::MAX };
        let screen = draw(&mut app, 32, 6);
        assert!(screen.contains("Error details"));
        let wrapped_path: String = screen
            .lines()
            .filter_map(|line| line.strip_prefix('│')?.strip_suffix('│'))
            .map(str::trim_end)
            .collect();
        assert_eq!(wrapped_path, path, "{screen}");
        assert!(screen.contains("Esc close"));
    }

    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let cells = terminal.backend().buffer().content();
        cells
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>() + "\n")
            .collect()
    }
}
