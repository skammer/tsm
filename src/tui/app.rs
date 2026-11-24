use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::Result;
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    widgets::ListState,
};
use uuid::Uuid;

use crate::{
    model::{Scope, Snapshot},
    query::{search, short_id},
    store::Store,
    tmux,
};

use super::inspect::Outline;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    List,
    Inspector,
}

#[derive(Default)]
pub(super) enum Mode {
    #[default]
    Browse,
    Filter {
        original: String,
        selection: Option<Uuid>,
    },
    Save {
        scope: Scope,
        name: String,
    },
    Delete,
    Help {
        scroll: u16,
    },
    ErrorDetails {
        scroll: u16,
    },
    RestoreWorking {
        receiver: Receiver<RestoreUpdate>,
        message: String,
        jump: bool,
    },
}

pub(super) enum RestoreUpdate {
    Progress(String),
    Finished(Result<tmux::RestoreReport>),
}

pub(super) struct Status {
    pub message: String,
    pub details: Option<String>,
}

pub(super) struct App {
    pub all: Vec<Snapshot>,
    pub ids: Vec<String>,
    pub visible: Vec<usize>,
    pub filter: String,
    pub mode: Mode,
    pub status: Option<Status>,
    pub warnings: Vec<String>,
    pub list_state: ListState,
    pub scroll: u16,
    pub page_height: u16,
    pub list_page: usize,
    pub compact: bool,
    pub focus: Focus,
    pub details: bool,
    pub outline: Outline,
}

impl App {
    pub fn new(all: Vec<Snapshot>) -> Self {
        let mut app = Self {
            all,
            ids: Vec::new(),
            visible: Vec::new(),
            filter: String::new(),
            mode: Mode::Browse,
            status: None,
            warnings: Vec::new(),
            list_state: ListState::default(),
            scroll: 0,
            page_height: 1,
            list_page: 1,
            compact: false,
            focus: Focus::List,
            details: false,
            outline: Outline::default(),
        };
        app.update_catalog(None);
        app
    }

    pub fn selected(&self) -> Option<&Snapshot> {
        let index = *self.visible.get(self.list_state.selected()?)?;
        self.all.get(index)
    }

    pub fn apply_filter(&mut self, selection: Option<Uuid>) {
        self.visible = search(&self.all, &self.filter);
        let index = selection
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.all[*index].id == id)
            })
            .unwrap_or(0);
        *self.list_state.offset_mut() = 0;
        self.select(index);
    }

    fn update_catalog(&mut self, selection: Option<Uuid>) {
        self.ids = self
            .all
            .iter()
            .map(|snapshot| short_id(snapshot, &self.all))
            .collect();
        self.apply_filter(selection);
    }

    fn select(&mut self, index: usize) {
        self.list_state
            .select((!self.visible.is_empty()).then_some(index));
        self.scroll = 0;
        self.outline = Outline::default();
    }

    fn move_selection(&mut self, delta: isize) {
        let index = self.list_state.selected().unwrap_or(0);
        let next = index
            .saturating_add_signed(delta)
            .min(self.visible.len().saturating_sub(1));
        if next != index {
            self.select(next);
        }
    }

    pub fn reload(&mut self, store: &Store) -> Result<()> {
        let selection = self.selected().map(|snapshot| snapshot.id);
        let catalog = store.load()?;
        self.all = catalog.snapshots;
        self.warnings = catalog.warnings;
        self.update_catalog(selection);
        Ok(())
    }

    pub fn handle_key(&mut self, store: &Store, key: KeyEvent) -> bool {
        if self.restore_working() {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if matches!(self.mode, Mode::Browse) {
            return self.browse(store, key);
        }
        let mut mode = std::mem::take(&mut self.mode);
        match &mut mode {
            Mode::Filter {
                original,
                selection,
            } => match key.code {
                KeyCode::Enter => return false,
                KeyCode::Esc => {
                    self.filter = std::mem::take(original);
                    self.apply_filter(*selection);
                    return false;
                }
                KeyCode::Down => self.move_selection(1),
                KeyCode::Up => self.move_selection(-1),
                code => {
                    if edit(&mut self.filter, code, key.modifiers) {
                        self.apply_filter(None);
                    }
                }
            },
            Mode::Save { scope, name } => match key.code {
                KeyCode::Enter if key.kind == KeyEventKind::Press => {
                    self.save(store, scope.clone(), name);
                    return false;
                }
                KeyCode::Esc => return false,
                code => {
                    edit(name, code, key.modifiers);
                }
            },
            Mode::Delete => match key.code {
                KeyCode::Char('y' | 'Y') if key.kind == KeyEventKind::Press => {
                    self.delete(store);
                    return false;
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => return false,
                _ => {}
            },
            Mode::Help { .. } if matches!(key.code, KeyCode::Char('?' | 'q')) => return false,
            Mode::ErrorDetails { .. } if key.code == KeyCode::Char('e') => return false,
            Mode::Help { scroll } | Mode::ErrorDetails { scroll } => {
                if key.code == KeyCode::Esc {
                    return false;
                }
                scroll_document(scroll, key.code, self.page_height);
            }
            Mode::Browse | Mode::RestoreWorking { .. } => {}
        }
        self.mode = mode;
        false
    }

    fn browse(&mut self, store: &Store, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        let in_inspector = self.focus == Focus::Inspector;
        match key.code {
            KeyCode::Esc if in_inspector => self.focus = Focus::List,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.apply_filter(self.selected().map(|snapshot| snapshot.id));
            }
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Down | KeyCode::Char('j') if in_inspector => self.outline.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') if in_inspector => self.outline.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown | KeyCode::PageUp if in_inspector || !self.compact => {
                scroll_document(&mut self.scroll, key.code, self.page_height)
            }
            KeyCode::PageDown => self.move_selection(self.list_page as isize),
            KeyCode::PageUp => self.move_selection(-(self.list_page as isize)),
            KeyCode::Home if in_inspector => self.scroll = 0,
            KeyCode::End if in_inspector => self.scroll = u16::MAX,
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(self.visible.len().saturating_sub(1)),
            _ if key.kind != KeyEventKind::Press => {}
            KeyCode::Right if in_inspector => self.outline.expand(),
            KeyCode::Left if in_inspector => self.outline.collapse(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter if self.selected().is_some() => {
                self.focus = if in_inspector {
                    Focus::List
                } else {
                    self.outline.reveal = true;
                    Focus::Inspector
                };
            }
            KeyCode::Char('g') => {
                self.status = None;
                if let Err(error) = self.reload(store) {
                    self.report(Err(error), "Reload failed");
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if self.selected().is_some() => {
                self.mode = Mode::Delete
            }
            KeyCode::Char('i') if self.selected().is_some() => {
                self.details = !self.details;
                self.outline.reveal = in_inspector || self.compact;
                if self.compact {
                    self.focus = Focus::Inspector;
                }
            }
            KeyCode::Char(c @ ('w' | 's' | 'a')) => {
                self.mode = Mode::Save {
                    scope: match c {
                        'w' => Scope::Window,
                        's' => Scope::Session,
                        _ => Scope::All,
                    },
                    name: String::new(),
                };
            }
            KeyCode::Char('/') => {
                self.focus = Focus::List;
                self.mode = Mode::Filter {
                    original: self.filter.clone(),
                    selection: self.selected().map(|snapshot| snapshot.id),
                };
            }
            KeyCode::Char('?') => self.mode = Mode::Help { scroll: 0 },
            KeyCode::Char('e')
                if self
                    .status
                    .as_ref()
                    .is_some_and(|status| status.details.is_some()) =>
            {
                self.mode = Mode::ErrorDetails { scroll: 0 };
            }
            KeyCode::Char(c @ ('r' | 'R')) => {
                if let Some(snapshot) = self.selected().cloned() {
                    self.start_restore(snapshot, c == 'R');
                }
            }
            _ => {}
        }
        false
    }

    fn report(&mut self, result: Result<String>, summary: &str) {
        self.status = Some(match result {
            Ok(message) => Status {
                message,
                details: None,
            },
            Err(error) => Status {
                message: summary.into(),
                details: Some(format!("{error:#}")),
            },
        });
    }

    fn start_restore(&mut self, snapshot: Snapshot, jump: bool) {
        let (sender, receiver) = mpsc::channel();
        self.status = None;
        match thread::Builder::new()
            .name("tsm-restore".into())
            .spawn(move || {
                let result = tmux::plan_restore(&snapshot).and_then(|plan| {
                    tmux::execute_restore(plan, |message| {
                        let _ = sender.send(RestoreUpdate::Progress(message));
                    })
                });
                let _ = sender.send(RestoreUpdate::Finished(result));
            }) {
            Ok(_) => {
                self.mode = Mode::RestoreWorking {
                    receiver,
                    message: "Checking saved panes…".into(),
                    jump,
                };
            }
            Err(error) => self.report(Err(error.into()), "Restore failed"),
        }
    }

    pub fn restore_working(&self) -> bool {
        matches!(self.mode, Mode::RestoreWorking { .. })
    }

    pub fn poll_restore(&mut self) -> Option<tmux::RestoreReport> {
        let Mode::RestoreWorking {
            receiver,
            message,
            jump,
        } = &mut self.mode
        else {
            return None;
        };
        loop {
            match receiver.try_recv() {
                Ok(RestoreUpdate::Progress(progress)) => {
                    *message = progress;
                    continue;
                }
                Ok(RestoreUpdate::Finished(Ok(report))) if *jump => return Some(report),
                Ok(RestoreUpdate::Finished(Ok(report))) => {
                    self.mode = Mode::Browse;
                    let restored = format!("Restored: {}", report.targets.join(", "));
                    match report.failures.len() {
                        0 => self.report(Ok(restored), ""),
                        failed => {
                            self.status = Some(Status {
                                message: if failed == 1 {
                                    "Restored · startup issue in 1 pane".into()
                                } else {
                                    format!("Restored · startup issues in {failed} panes")
                                },
                                details: Some(format!(
                                    "{restored}\n\n{}",
                                    report.failures.join("\n\n")
                                )),
                            });
                        }
                    }
                }
                Ok(RestoreUpdate::Finished(Err(error))) => {
                    self.mode = Mode::Browse;
                    self.report(Err(error), "Restore failed");
                }
                Err(TryRecvError::Disconnected) => {
                    self.mode = Mode::Browse;
                    self.report(
                        Err(anyhow::anyhow!("Restore worker stopped")),
                        "Restore interrupted",
                    );
                }
                Err(TryRecvError::Empty) => {}
            }
            break;
        }
        None
    }

    fn save(&mut self, store: &Store, scope: Scope, name: &str) {
        let name = (!name.trim().is_empty()).then(|| name.to_owned());
        let result = tmux::capture(scope, None, name).and_then(|snapshot| {
            store.save(&snapshot)?;
            self.reload(store)?;
            if !self
                .visible
                .iter()
                .any(|&index| self.all[index].id == snapshot.id)
            {
                self.filter.clear();
            }
            self.apply_filter(Some(snapshot.id));
            self.focus = Focus::List;
            Ok(format!(
                "Saved: {} · {}",
                short_id(&snapshot, &self.all),
                snapshot.name
            ))
        });
        self.report(result, "Save failed");
    }

    fn delete(&mut self, store: &Store) {
        let Some(snapshot) = self.selected() else {
            return;
        };
        let name = snapshot.name.clone();
        let previous = self.list_state.selected().unwrap_or(0);
        let result = store.delete(snapshot).and_then(|()| {
            self.reload(store)?;
            self.select(previous.min(self.visible.len().saturating_sub(1)));
            if self.visible.is_empty() {
                self.focus = Focus::List;
            }
            Ok(format!("Deleted: {name}"))
        });
        self.report(result, "Delete failed");
    }
}

fn scroll_document(scroll: &mut u16, code: KeyCode, page_height: u16) {
    *scroll = match code {
        KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
        KeyCode::PageDown => scroll.saturating_add(page_height),
        KeyCode::PageUp => scroll.saturating_sub(page_height),
        KeyCode::Home => 0,
        KeyCode::End => u16::MAX,
        _ => *scroll,
    };
}

fn edit(text: &mut String, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char(c) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            text.push(c);
        }
        _ => return false,
    }
    true
}
