use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, Paragraph, Wrap},
};

use super::{
    app::{App, Focus, Mode},
    display_value,
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
    let footer_height = footer
        .line_count(area.width)
        .min(usize::from((area.height / 2).max(1))) as u16;
    let show_search = matches!(
        app.mode,
        Mode::Browse | Mode::Filter { .. } | Mode::Save { .. }
    ) && area.height > 1;
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(u16::from(show_search)),
        Constraint::Fill(1),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    if show_search {
        frame.render_widget(Paragraph::new(search), head);
    }

    match &mut app.mode {
        Mode::Help { scroll } => {
            let document = Paragraph::new(help_lines(&app.warnings)).wrap(WRAP);
            app.page_height = render_document(
                frame,
                body,
                panel("Keyboard shortcuts".into(), true),
                document,
                scroll,
            );
        }
        Mode::Delete => {
            let name = app
                .selected()
                .map_or_else(String::new, |snapshot| single_line(&snapshot.name));
            frame.render_widget(
                Paragraph::new(name)
                    .style(Style::new().fg(Color::Yellow))
                    .wrap(WRAP)
                    .block(panel("Delete snapshot?".into(), true)),
                body,
            );
        }
        Mode::ErrorDetails { scroll } => {
            let details = app
                .status
                .as_ref()
                .and_then(|status| status.details.as_deref())
                .unwrap_or_default();
            let lines: Vec<_> = details
                .lines()
                .map(|line| Line::from(display_value(line)))
                .collect();
            app.page_height = render_document(
                frame,
                body,
                panel("Error details".into(), true),
                Paragraph::new(lines).wrap(WRAP),
                scroll,
            );
        }
        Mode::RestoreWorking { message, .. } => {
            frame.render_widget(
                Paragraph::new(single_line(message))
                    .wrap(WRAP)
                    .block(panel("Restoring".into(), true)),
                body,
            );
        }
        _ if app.compact => match app.focus {
            Focus::List => render_list(frame, body, app),
            Focus::Inspector => render_inspector(frame, body, app),
        },
        _ => {
            let [list, inspector] = Layout::horizontal([
                Constraint::Length((body.width / 3).max(28)),
                Constraint::Fill(1),
            ])
            .areas(body);
            render_list(frame, list, app);
            render_inspector(frame, inspector, app);
        }
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
        } else if matches!(app.mode, Mode::Filter { .. }) {
            "No matches.\nTry a different name or ID."
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
    let title = if app.details {
        "Inspector · details"
    } else {
        "Inspector"
    };
    let block = panel(title.into(), focused);
    let inner = block.inner(area);
    let lines = if let Some(snapshot) = app.selected() {
        let mut document = snapshot_lines(snapshot, &app.outline, app.details);
        app.outline
            .sync(document.headings.iter().map(|(section, _)| *section));
        if focused
            && let Some(selected) = app.outline.selected()
            && let Some((_, heading)) = document
                .headings
                .iter()
                .find(|(section, _)| *section == selected)
        {
            if std::mem::take(&mut app.outline.reveal) && !inner.is_empty() {
                let row = Paragraph::new(document.lines[..*heading].to_vec())
                    .wrap(WRAP)
                    .line_count(inner.width)
                    .min(usize::from(u16::MAX)) as u16;
                app.scroll = app
                    .scroll
                    .min(row)
                    .max(row.saturating_sub(inner.height - 1));
            }
            document.lines[*heading] = document.lines[*heading]
                .clone()
                .patch_style(BOLD.add_modifier(Modifier::REVERSED));
        }
        document.lines
    } else {
        vec![Line::from("Select a snapshot to inspect it.")]
    };
    app.page_height = render_document(
        frame,
        area,
        block,
        Paragraph::new(lines).wrap(WRAP),
        &mut app.scroll,
    );
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
        Mode::Delete => vec![hint(if width < 24 {
            "y yes · Esc no"
        } else {
            "y confirm · n/Esc cancel"
        })],
        Mode::Help { .. } => vec![hint("↑↓ scroll · ? / Esc close")],
        Mode::ErrorDetails { .. } => vec![hint("↑↓ scroll · Esc close")],
        Mode::RestoreWorking { .. } => vec![],
        Mode::Browse => {
            let mut lines = Vec::new();
            if let Some(status) = &app.status {
                let message = single_line(&status.message);
                let (message, color) = if status.details.is_some() {
                    (format!("e details · {message}"), Color::Red)
                } else {
                    (message, Color::Green)
                };
                lines.push(Line::styled(message, Style::new().fg(color)));
            }
            if !app.warnings.is_empty() {
                lines.push(Line::styled(
                    format!("{} storage warnings · ? view", app.warnings.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
            lines.push(if app.compact && app.focus == Focus::Inspector {
                hint("↑↓ · ←→ fold/open · Tab list · R jump · ?")
            } else if app.compact {
                hint("↑↓ move · Tab inspect · R jump · / · ?")
            } else if app.focus == Focus::Inspector {
                hint("↑↓ sections · ←→ fold/open · Tab list · i details · r restore · R jump · ? help")
            } else {
                hint("↑↓ move · Tab focus · / filter · r restore · R jump · w/s/a save · ? help · q quit")
            });
            lines
        }
    };
    (Paragraph::new(lines).wrap(WRAP), cursor)
}

fn help_lines(warnings: &[String]) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("↑/↓ or j/k      Select a snapshot or section"),
        Line::from("←/→             Fold/open an inspector section"),
        Line::from("PgUp/PgDn       Scroll inspector or compact list"),
        Line::from("Home/End        Jump to start/end"),
        Line::from("Tab/Enter       Switch list and inspector"),
        Line::from("/               Fuzzy name search or ID prefix"),
        Line::from("i               Toggle technical details"),
        Line::from("r               Restore selected snapshot"),
        Line::from("R               Restore and jump to the result"),
        Line::from("e               View error details"),
        Line::from("w / s / a       Save window / session / all"),
        Line::from("d / Delete      Delete selected snapshot"),
        Line::from("g               Reload snapshots"),
        Line::from("q / Ctrl-C      Quit"),
    ];
    if !warnings.is_empty() {
        lines.push(Line::styled(
            "Storage warnings",
            Style::new().fg(Color::Yellow),
        ));
        lines.extend(
            warnings
                .iter()
                .map(|warning| Line::from(single_line(warning))),
        );
    }
    lines
}

fn render_document(
    frame: &mut Frame,
    area: Rect,
    block: Block<'static>,
    document: Paragraph<'static>,
    scroll: &mut u16,
) -> u16 {
    let inner = block.inner(area);
    let hidden_rows = document
        .line_count(inner.width)
        .saturating_sub(usize::from(inner.height));
    *scroll = (*scroll).min(hidden_rows.min(usize::from(u16::MAX)) as u16);
    let block = if hidden_rows > 0 && !inner.is_empty() {
        block.title_bottom(
            Line::styled(
                format!(" {}/{} ", usize::from(*scroll) + 1, hidden_rows + 1),
                HINT,
            )
            .right_aligned(),
        )
    } else {
        block
    };
    frame.render_widget(document.block(block).scroll((*scroll, 0)), area);
    inner.height.saturating_sub(1).max(1)
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
