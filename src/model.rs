use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Window,
    Session,
    All,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Session => "session",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub scope: Scope,
    pub sessions: Vec<SessionSnapshot>,
}

impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported snapshot version: {}",
            self.version
        );
        ensure!(!self.name.trim().is_empty(), "snapshot name is empty");
        ensure!(!self.sessions.is_empty(), "snapshot has no sessions");
        if self.scope != Scope::All {
            ensure!(
                self.sessions.len() == 1,
                "scope requires exactly one session"
            );
        }
        for session in &self.sessions {
            ensure!(!session.name.is_empty(), "session name is empty");
            ensure!(!session.windows.is_empty(), "session has no windows");
            ensure!(
                session.active_window < session.windows.len(),
                "invalid active window"
            );
            if self.scope == Scope::Window {
                ensure!(
                    session.windows.len() == 1,
                    "window snapshot requires one window"
                );
            }
            let mut indexes = std::collections::HashSet::new();
            for window in &session.windows {
                ensure!(
                    indexes.insert(window.index),
                    "duplicate window index: {}",
                    window.index
                );
                ensure!(!window.panes.is_empty(), "window has no panes");
                ensure!(
                    window.active_pane < window.panes.len(),
                    "invalid active pane"
                );
                ensure!(!window.layout.is_empty(), "window has no layout");
            }
        }
        Ok(())
    }
    pub fn pane_count(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .map(|window| window.panes.len())
            .sum()
    }

    pub fn window_count(&self) -> usize {
        self.sessions
            .iter()
            .map(|session| session.windows.len())
            .sum()
    }
}

#[cfg(test)]
pub fn test_snapshot() -> Snapshot {
    Snapshot {
        version: 1,
        id: Uuid::new_v4(),
        name: "Personal projects".into(),
        created_at: Utc::now(),
        scope: Scope::Session,
        sessions: vec![SessionSnapshot {
            name: "projects".into(),
            active_window: 0,
            windows: vec![WindowSnapshot {
                index: 3,
                name: "editor".into(),
                layout: "b25d,80x24,0,0,0".into(),
                active_pane: 0,
                zoomed: false,
                panes: vec![PaneSnapshot {
                    title: "shell".into(),
                    cwd: "/tmp".into(),
                    command: "sh".into(),
                    command_line: String::new(),
                    argv: vec![],
                    start_command: String::new(),
                    active: true,
                }],
            }],
        }],
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub name: String,
    pub windows: Vec<WindowSnapshot>,
    pub active_window: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowSnapshot {
    #[serde(default)]
    pub index: usize,
    pub name: String,
    pub layout: String,
    pub panes: Vec<PaneSnapshot>,
    pub active_pane: usize,
    #[serde(default)]
    pub zoomed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub title: String,
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub command_line: String,
    #[serde(default)]
    pub argv: Vec<String>,
    pub start_command: String,
    pub active: bool,
}
