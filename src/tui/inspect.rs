use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
};

use crate::model::Snapshot;

use super::single_line;

pub(super) fn snapshot_lines(snapshot: &Snapshot, expanded: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            single_line(&snapshot.name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!(
            "Saved {}",
            snapshot.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        )),
        Line::styled(
            format!(
                "{} · {} win · {} pane{}",
                snapshot.scope.as_str(),
                snapshot.window_count(),
                snapshot.pane_count(),
                if snapshot.pane_count() == 1 { "" } else { "s" },
            ),
            Style::new().fg(Color::Gray),
        ),
        Line::from(""),
    ];
    if expanded {
        lines.insert(1, Line::from(format!("UUID  {}", snapshot.id)));
    }
    for session in &snapshot.sessions {
        lines.push(Line::styled(
            format!("Session  {}", single_line(&session.name)),
            Style::default().fg(Color::Cyan),
        ));
        for (window_index, window) in session.windows.iter().enumerate() {
            lines.push(Line::from(format!(
                "  Window {}: {}",
                window.index,
                single_line(&window.name)
            )));
            if expanded {
                lines.push(Line::from(format!(
                    "    Layout: {}",
                    single_line(&window.layout)
                )));
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
                lines.push(Line::from(format!(
                    "    {}",
                    single_line(if command.is_empty() { "shell" } else { command })
                )));
                lines.push(Line::styled(
                    format!("      {}", single_line(&pane.cwd)),
                    Style::default().fg(Color::Gray),
                ));
                if !pane.title.is_empty() {
                    lines.push(Line::styled(
                        format!("      {}", single_line(&pane.title)),
                        Style::default().fg(Color::Gray),
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
                            single_line(&pane.start_command)
                        )));
                    }
                }
            }
        }
    }
    lines
}
