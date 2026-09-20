use std::time::Duration;

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    model::{Scope, Snapshot},
    query::{SnapshotQuery, short_id},
    store::Store,
    tmux,
};

struct App {
    all: Vec<Snapshot>,
    visible: Vec<usize>,
    selected: usize,
    filter: String,
    filtering: bool,
    status: String,
    list_state: ListState,
    scroll: u16,
    inspector: bool,
    saving: Option<Scope>,
    name: String,
    deleting: Option<Snapshot>,
    expanded: bool,
}

impl App {
    fn new(all: Vec<Snapshot>) -> Self {
        let visible = (0..all.len()).collect();
        Self {
            all,
            visible,
            selected: 0,
            filter: String::new(),
            filtering: false,
            status: String::new(),
            list_state: ListState::default(),
            scroll: 0,
            inspector: false,
            saving: None,
            name: String::new(),
            deleting: None,
            expanded: false,
        }
    }

    fn selected(&self) -> Option<&Snapshot> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.all.get(*index))
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.scroll = 0;
    }

    fn apply_filter(&mut self) {
        let query = SnapshotQuery::new(&self.filter);
        self.visible = self
            .all
            .iter()
            .enumerate()
            .filter_map(|(index, snapshot)| query.matches(snapshot).then_some(index))
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.scroll = 0;
    }

    fn reload(&mut self, store: &Store) -> Result<()> {
        let catalog = store.load()?;
        self.all = catalog.snapshots;
        self.status = catalog.warnings.join(" | ");
        self.apply_filter();
        Ok(())
    }

    fn confirm_delete(&mut self, store: &Store, key: KeyCode) -> Result<()> {
        match key {
            KeyCode::Char('y' | 'Y') => {
                if let Some(snapshot) = self.deleting.take() {
                    match store.delete(&snapshot) {
                        Ok(()) => {
                            self.reload(store)?;
                            self.status = format!("Deleted: {}", snapshot.name);
                        }
                        Err(error) => self.status = format!("Error: {error:#}"),
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') => self.deleting = None,
            _ => {}
        }
        Ok(())
    }
}

pub fn run(store: &Store) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = run_loop(&mut terminal, store);
    let restored = ratatui::try_restore();
    result?;
    Ok(restored?)
}

fn run_loop(terminal: &mut DefaultTerminal, store: &Store) -> Result<()> {
    let mut app = App::new(vec![]);
    app.reload(store)?;
    loop {
        terminal.draw(|frame| render(frame, &mut app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
        if app.deleting.is_some() {
            app.confirm_delete(store, key.code)?;
            continue;
        }
        if let Some(scope) = app.saving.clone() {
            match key.code {
                KeyCode::Esc => app.saving = None,
                KeyCode::Backspace => {
                    app.name.pop();
                }
                KeyCode::Char(c) => app.name.push(c),
                KeyCode::Enter => {
                    let name = (!app.name.trim().is_empty()).then(|| app.name.clone());
                    let result = tmux::capture(scope, name).and_then(|snapshot| {
                        store.save(&snapshot)?;
                        Ok(snapshot)
                    });
                    app.saving = None;
                    match result {
                        Ok(snapshot) => {
                            app.reload(store)?;
                            app.status = format!(
                                "Saved {} · {}",
                                short_id(&snapshot, &app.all),
                                snapshot.name
                            );
                        }
                        Err(error) => app.status = format!("Error: {error:#}"),
                    }
                }
                _ => {}
            }
            continue;
        }

        if app.filtering {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => app.filtering = false,
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.apply_filter();
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.apply_filter();
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Down | KeyCode::Char('j') if app.inspector => {
                app.scroll = app.scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if app.inspector => {
                app.scroll = app.scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => app.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_by(-1),
            KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
            KeyCode::Home => app.scroll = 0,
            KeyCode::Tab | KeyCode::Enter => app.inspector = !app.inspector,
            KeyCode::Char('g') => app.reload(store)?,
            KeyCode::Char('d') | KeyCode::Delete => app.deleting = app.selected().cloned(),
            KeyCode::Char('i') => {
                app.expanded = !app.expanded;
                app.scroll = 0;
            }
            KeyCode::Char(c @ ('w' | 's' | 'a')) => {
                app.saving = Some(match c {
                    'w' => Scope::Window,
                    's' => Scope::Session,
                    _ => Scope::All,
                });
                app.name.clear();
            }
            KeyCode::Char('/') => app.filtering = true,
            KeyCode::Char('r') => {
                if let Some(snapshot) = app.selected().cloned() {
                    app.status = match tmux::restore(&snapshot) {
                        Ok(names) => format!("Restored: {}", names.join(", ")),
                        Err(error) => format!("Error: {error:#}"),
                    };
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn render(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(frame.area());
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body);

    let items: Vec<_> = app
        .visible
        .iter()
        .map(|index| {
            let snapshot = &app.all[*index];
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        short_id(snapshot, &app.all),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(format!("  {}", snapshot.name)),
                ]),
                Line::styled(
                    format!(
                        "{} · {} windows · {} panes",
                        snapshot.scope.as_str(),
                        snapshot.window_count(),
                        snapshot.pane_count()
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let title = if app.filter.is_empty() {
        " Saved snapshots ".to_owned()
    } else {
        format!(" Saved snapshots · {} ", app.filter)
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    app.list_state
        .select((!app.visible.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(list, list_area, &mut app.list_state);

    let details = app
        .selected()
        .map(|snapshot| snapshot_lines(snapshot, app.expanded))
        .unwrap_or_else(|| vec![Line::from("No snapshots")]);
    let inspector = Paragraph::new(details).wrap(Wrap { trim: false });
    app.scroll = app.scroll.min(
        inspector
            .line_count(detail_area.width.saturating_sub(2))
            .saturating_sub(detail_area.height.saturating_sub(2) as usize)
            .min(u16::MAX as usize) as u16,
    );
    frame.render_widget(
        inspector
            .scroll((app.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if app.inspector {
                        " Inspector · focused "
                    } else {
                        " Inspector "
                    }),
            ),
        detail_area,
    );

    let help = if let Some(snapshot) = &app.deleting {
        format!(
            "Delete {} · {}?\ny confirm · n/Esc cancel",
            short_id(snapshot, &app.all),
            snapshot.name
        )
    } else if let Some(scope) = &app.saving {
        format!(
            "Save {} · name: {}_   Enter save · Esc cancel",
            scope.as_str(),
            app.name
        )
    } else if app.filtering {
        format!("Filter: {}_   Enter/Esc done", app.filter)
    } else if app.status.is_empty() {
        "↑/↓ navigate · Tab inspector · PgUp/PgDn scroll · / filter · i details\nw window · s session · a all · r restore · d delete · g refresh · q quit".to_owned()
    } else {
        format!(
            "{}\nw/s/a save · / filter · Tab inspector · i details · r restore · d delete · g refresh · q quit",
            app.status
        )
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

fn snapshot_lines(snapshot: &Snapshot, expanded: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            snapshot.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!(
            "Saved {}",
            snapshot.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        )),
        Line::from(""),
    ];
    if expanded {
        lines.insert(1, Line::from(format!("UUID  {}", snapshot.id)));
    }
    for session in &snapshot.sessions {
        lines.push(Line::styled(
            format!("Session  {}", session.name),
            Style::default().fg(Color::Cyan),
        ));
        for (window_index, window) in session.windows.iter().enumerate() {
            lines.push(Line::from(format!(
                "  Window {}: {}",
                window.index, window.name
            )));
            if expanded {
                lines.push(Line::from(format!("    Layout: {}", window.layout)));
                lines.push(Line::from(format!(
                    "    Active: {} · Zoomed: {}",
                    window_index == session.active_window,
                    window.zoomed
                )));
            }
            for (pane_index, pane) in window.panes.iter().enumerate() {
                let command = if !pane.command_line.is_empty() {
                    &pane.command_line
                } else {
                    &pane.command
                };
                lines.push(Line::from(format!("    {}  {}", command, pane.cwd)));
                if !pane.title.is_empty() {
                    lines.push(Line::styled(
                        format!("      {}", pane.title),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if expanded {
                    lines.push(Line::from(format!(
                        "      Pane {} · Active: {}",
                        pane_index,
                        pane_index == window.active_pane
                    )));
                    if !pane.start_command.is_empty() {
                        lines.push(Line::from(format!(
                            "      started as: {}",
                            pane.start_command
                        )));
                    }
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
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
        app.apply_filter();
        assert!(app.selected().is_none());
    }
}
