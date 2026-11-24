use std::collections::HashSet;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::{
    model::{PaneSnapshot, Snapshot},
    tmux,
};

use super::{display_value, single_line};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Section {
    Session(usize),
    Window(usize, usize),
}

impl Section {
    fn default_open(self) -> bool {
        matches!(self, Self::Session(_))
    }
}

#[derive(Default)]
pub(super) struct Outline {
    overrides: HashSet<Section>,
    sections: Vec<Section>,
    selected: usize,
    pub reveal: bool,
}

impl Outline {
    fn is_open(&self, section: Section) -> bool {
        section.default_open() != self.overrides.contains(&section)
    }

    fn set_open(&mut self, section: Section, open: bool) {
        if open == section.default_open() {
            self.overrides.remove(&section);
        } else {
            self.overrides.insert(section);
        }
    }

    pub fn selected(&self) -> Option<Section> {
        self.sections.get(self.selected).copied()
    }

    pub fn sync(&mut self, sections: impl IntoIterator<Item = Section>) {
        let selected = self.selected();
        self.sections = sections.into_iter().collect();
        self.selected = selected
            .and_then(|section| self.sections.iter().position(|item| *item == section))
            .unwrap_or(0);
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.sections.len().saturating_sub(1));
        self.reveal = true;
    }

    pub fn expand(&mut self) {
        if let Some(section) = self.selected() {
            self.set_open(section, true);
            self.reveal = true;
        }
    }

    pub fn collapse(&mut self) {
        if let Some(section) = self.selected() {
            if self.is_open(section) {
                self.set_open(section, false);
            } else if let Section::Window(session, _) = section
                && let Some(index) = self
                    .sections
                    .iter()
                    .position(|item| *item == Section::Session(session))
            {
                self.selected = index;
            }
            self.reveal = true;
        }
    }
}

pub(super) struct InspectorLines {
    pub lines: Vec<Line<'static>>,
    pub headings: Vec<(Section, usize)>,
}

pub(super) fn snapshot_lines(
    snapshot: &Snapshot,
    outline: &Outline,
    details: bool,
) -> InspectorLines {
    let mut lines = vec![Line::styled(
        single_line(&snapshot.name),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let snapshot_cwd_shown = show_shared_cwd(
        &mut lines,
        "",
        snapshot
            .sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes),
    );
    if details || snapshot.sessions.len() > 1 {
        lines.push(Line::styled(
            format!(
                "{} · {} · {}",
                snapshot.scope.as_str(),
                count(snapshot.window_count(), "window"),
                count(snapshot.pane_count(), "pane"),
            ),
            Style::default().fg(Color::Gray),
        ));
    }
    if details {
        lines.push(Line::from(format!(
            "Saved {}",
            snapshot.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        )));
        lines.push(Line::from(format!("UUID  {}", snapshot.id)));
    }

    let mut headings = Vec::new();
    for (session_index, session) in snapshot.sessions.iter().enumerate() {
        let section = Section::Session(session_index);
        let open = outline.is_open(section);
        let pane_count = session
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum();
        headings.push((section, lines.len()));
        lines.push(Line::styled(
            format!(
                "{} Session {} · {} · {}",
                disclosure(open),
                single_line(&session.name),
                count(session.windows.len(), "window"),
                count(pane_count, "pane"),
            ),
            Style::default().fg(Color::Cyan),
        ));
        let session_cwd_shown = snapshot_cwd_shown
            || show_shared_cwd(
                &mut lines,
                "  ",
                session.windows.iter().flat_map(|window| &window.panes),
            );
        if !open {
            continue;
        }

        for (window_index, window) in session.windows.iter().enumerate() {
            let section = Section::Window(session_index, window_index);
            let open = outline.is_open(section);
            headings.push((section, lines.len()));
            lines.push(Line::from(format!(
                "  {} {}Window {}: {} · {}{}",
                disclosure(open),
                active(window_index == session.active_window),
                window.index,
                single_line(&window.name),
                count(window.panes.len(), "pane"),
                if window.zoomed { " · zoomed" } else { "" },
            )));
            let window_cwd_shown =
                session_cwd_shown || show_shared_cwd(&mut lines, "    ", window.panes.iter());
            if open && details {
                lines.push(Line::from(format!(
                    "    Layout: {}",
                    single_line(&window.layout)
                )));
            }
            for (pane_index, pane) in window.panes.iter().enumerate() {
                let command = tmux::replay_description(pane)
                    .unwrap_or_else(|error| format!("Command unavailable ({error:#})"));
                let command = display_value(&command);
                let is_active = pane_index == window.active_pane;
                if !open {
                    let marker = if is_active { "* " } else { "  " };
                    let mut line = Line::from(format!("    {marker}{command}"));
                    if !window_cwd_shown {
                        line.spans.push(cwd_span(" · ", &pane.cwd));
                    }
                    lines.push(line);
                    continue;
                }

                let title = if pane.title.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", single_line(&pane.title))
                };
                let marker = active(is_active);
                lines.push(Line::from(format!("    {marker}Pane {pane_index}{title}")));
                lines.push(Line::from(format!("      {command}")));
                if !window_cwd_shown {
                    lines.push(Line::from(cwd_span("      ", &pane.cwd)));
                }
                if details && !pane.start_command.is_empty() {
                    lines.push(Line::from(format!(
                        "      started as: {}",
                        display_value(&pane.start_command)
                    )));
                }
            }
        }
    }
    InspectorLines { lines, headings }
}

fn show_shared_cwd<'a>(
    lines: &mut Vec<Line<'static>>,
    indent: &str,
    mut panes: impl Iterator<Item = &'a PaneSnapshot>,
) -> bool {
    let Some(first) = panes.next() else {
        return false;
    };
    if first.cwd.is_empty() || !panes.all(|pane| pane.cwd == first.cwd) {
        return false;
    }
    lines.push(Line::from(cwd_span(indent, &first.cwd)));
    true
}

fn cwd_span(prefix: &str, cwd: &str) -> Span<'static> {
    Span::styled(
        format!("{prefix}cwd: {}", display_value(cwd)),
        Style::default().fg(Color::Gray),
    )
}

fn active(active: bool) -> &'static str {
    if active { "* " } else { "" }
}

fn disclosure(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

fn count(number: usize, noun: &str) -> String {
    format!("{number} {noun}{}", if number == 1 { "" } else { "s" })
}
