use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, Paragraph, Wrap},
};

use super::{
    app::{App, Focus, Mode},
    inspect::snapshot_lines,
    single_line,
};

const COMPACT_WIDTH: u16 = 76;
const WRAP: Wrap = Wrap { trim: false };
const HINT: Style = Style::new().fg(Color::Gray);
const ACCENT: Style = Style::new().fg(Color::Cyan);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);

pub(super) fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.compact = area.width < COMPACT_WIDTH;

    let filtering = matches!(app.mode, Mode::Filter { .. });
    let query = if app.filter.is_empty() && !filtering {
        "Search snapshots by name or ID".into()
    } else {
        input_tail(&app.filter, area.width.saturating_sub(8))
    };
    let search = Line::from(vec![
        Span::styled("tsm", ACCENT.patch(BOLD)),
        Span::raw("  / "),
        Span::raw(query),
    ]);
    let search_width = search.width() as u16;
    let (footer, save_cursor) = footer(app, area.width);
    let footer_height = (footer
        .line_count(area.width)
        .min(usize::from(area.height / 2)) as u16)
        .max(1);
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(search), head);

    if matches!(app.mode, Mode::Help { .. }) {
        render_help(frame, body, app);
    } else if app.compact {
        match app.focus {
            Focus::List => render_list(frame, body, app),
            Focus::Inspector => render_inspector(frame, body, app),
        }
    } else {
        let [list, inspector] = Layout::horizontal([
            Constraint::Length((body.width / 3).max(28)),
            Constraint::Fill(1),
        ])
        .areas(body);
        render_list(frame, list, app);
        render_inspector(frame, inspector, app);
    }
    frame.render_widget(footer, foot);
    if filtering && !head.is_empty() && search_width < head.width {
        frame.set_cursor_position((head.x + search_width, head.y));
    }
    if let Some(x) = save_cursor
        && !foot.is_empty()
        && x < foot.width
    {
        frame.set_cursor_position((foot.x + x, foot.y));
    }
}

fn panel(title: String, focused: bool) -> Block<'static> {
    let border = if focused { ACCENT } else { HINT };
    let title_style = if focused { ACCENT.patch(BOLD) } else { BOLD };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::styled(format!(" {title} "), title_style))
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let title = if app.filter.is_empty() {
        format!("Saved snapshots ({})", app.all.len())
    } else {
        format!("Saved snapshots ({}/{})", app.visible.len(), app.all.len())
    };
    let focused = app.focus == Focus::List;
    let block = panel(title, focused);
    let inner = block.inner(area);
    let two_lines = inner.height >= 5;
    app.list_page = usize::from(inner.height / if two_lines { 2 } else { 1 }).max(1);

    if app.visible.is_empty() {
        let message = if app.all.is_empty() {
            "No snapshots yet.\nPress w, s or a to save."
        } else {
            "No matches.\nPress Esc to clear the filter."
        };
        frame.render_widget(Paragraph::new(message).wrap(WRAP).block(block), area);
        return;
    }

    let rows = app.visible.iter().map(|&index| {
        let snapshot = &app.all[index];
        let name = Line::from(single_line(&snapshot.name));
        if !two_lines {
            return ListItem::new(name);
        }
        let metadata = Line::styled(
            format!(
                "{} · {} · {}",
                snapshot.scope.as_str(),
                snapshot.created_at.format("%m-%d %H:%M"),
                app.ids[index]
            ),
            HINT,
        );
        ListItem::new(vec![name, metadata])
    });
    let highlight = if focused {
        BOLD.add_modifier(Modifier::REVERSED)
    } else {
        BOLD
    };
    let list = List::new(rows)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(highlight);
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Inspector;
    let title = if app.expanded {
        "Inspector · details"
    } else {
        "Inspector"
    };
    let block = panel(title.into(), focused);
    let inner = block.inner(area);
    let lines = app
        .selected()
        .map_or_else(Vec::new, |snapshot| snapshot_lines(snapshot, app.expanded));
    let inspector = Paragraph::new(lines).wrap(WRAP);
    let hidden_rows = inspector
        .line_count(inner.width)
        .saturating_sub(usize::from(inner.height));
    app.scroll = app
        .scroll
        .min(hidden_rows.min(usize::from(u16::MAX)) as u16);
    app.page_height = inner.height.saturating_sub(1).max(1);
    let block = if hidden_rows > 0 && !inner.is_empty() {
        block.title_bottom(
            Line::styled(
                format!(" {}/{} ", usize::from(app.scroll) + 1, hidden_rows + 1),
                HINT,
            )
            .right_aligned(),
        )
    } else {
        block
    };
    frame.render_widget(inspector.block(block).scroll((app.scroll, 0)), area);
}

fn footer(app: &App, width: u16) -> (Paragraph<'static>, Option<u16>) {
    let hint = |text| Line::styled(text, HINT);
    let mut cursor = None;
    let lines = match &app.mode {
        Mode::Filter { .. } => vec![hint("↑↓ select · Enter apply · Esc cancel · Ctrl-U clear")],
        Mode::Save { scope, name } => {
            let prefix = Line::styled(format!("Save {} · ", scope.as_str()), ACCENT);
            let remaining = width.saturating_sub(prefix.width() as u16 + 1);
            let field = prefix + Span::raw(input_tail(name, remaining));
            cursor = Some(field.width() as u16);
            vec![
                field,
                hint("Enter save (blank uses default) · Esc cancel · Ctrl-U clear"),
            ]
        }
        Mode::Delete => vec![
            Line::styled(
                format!(
                    "Delete “{}”?",
                    app.selected()
                        .map_or_else(String::new, |snapshot| single_line(&snapshot.name))
                ),
                Style::new().fg(Color::Yellow),
            ),
            hint("y confirm · n/Esc cancel"),
        ],
        Mode::Help { .. } => vec![hint("↑↓ scroll · ? / Esc close")],
        Mode::Browse => {
            let mut lines = Vec::new();
            if let Some(status) = &app.status {
                let color = if status.error {
                    Color::Red
                } else {
                    Color::Green
                };
                lines.push(Line::styled(
                    single_line(&status.message),
                    Style::new().fg(color),
                ));
            }
            if !app.warnings.is_empty() {
                lines.push(Line::styled(
                    format!("{} storage warnings · ? view", app.warnings.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
            lines.push(if app.compact && app.focus == Focus::Inspector {
                hint("↑↓ scroll · Tab list · i details · ?")
            } else if app.compact {
                hint("↑↓ move · Tab inspect · / filter · ?")
            } else {
                hint("↑↓ move · Tab focus · / filter · i details · r restore · w/s/a save · ? help · q quit")
            });
            lines
        }
    };
    (Paragraph::new(lines).wrap(WRAP), cursor)
}

fn render_help(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut lines = vec![
        Line::from("↑/↓ or j/k      Move selection or scroll"),
        Line::from("PgUp/PgDn       Page through inspector or compact list"),
        Line::from("Home/End        Jump to start/end"),
        Line::from("Tab/Enter       Switch list and inspector"),
        Line::from("/               Fuzzy name search or ID prefix"),
        Line::from("i               Toggle details"),
        Line::from("r               Restore selected snapshot"),
        Line::from("w / s / a       Save window / session / all"),
        Line::from("d / Delete      Delete selected snapshot"),
        Line::from("g               Reload snapshots"),
        Line::from("q / Ctrl-C      Quit"),
    ];
    if !app.warnings.is_empty() {
        lines.push(Line::styled(
            "Storage warnings",
            Style::new().fg(Color::Yellow),
        ));
        lines.extend(
            app.warnings
                .iter()
                .map(|warning| Line::from(single_line(warning))),
        );
    }
    let block = panel("Keyboard shortcuts".into(), true);
    let inner = block.inner(area);
    let help = Paragraph::new(lines).wrap(WRAP);
    let hidden_rows = help
        .line_count(inner.width)
        .saturating_sub(usize::from(inner.height));
    let Mode::Help { scroll } = &mut app.mode else {
        return;
    };
    *scroll = (*scroll).min(hidden_rows.min(usize::from(u16::MAX)) as u16);
    app.page_height = inner.height.saturating_sub(1).max(1);
    frame.render_widget(help.block(block).scroll((*scroll, 0)), area);
}

fn input_tail(value: &str, width: u16) -> String {
    let value = single_line(value);
    let mut remaining = usize::from(width);
    for (index, c) in value.char_indices().rev() {
        let mut bytes = [0; 4];
        let char_width = Span::raw(c.encode_utf8(&mut bytes) as &str).width();
        if char_width > remaining {
            return value[index + c.len_utf8()..].into();
        }
        remaining -= char_width;
    }
    value
}
