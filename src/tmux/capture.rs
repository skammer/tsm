use super::{
    client::{current_pane, field, run},
    process::ProcessTable,
};
use crate::model::{PaneSnapshot, Scope, SessionSnapshot, Snapshot, WindowSnapshot};
use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

pub fn capture(scope: Scope, custom_name: Option<String>) -> Result<Snapshot> {
    let current = if scope != Scope::All {
        Some(current_pane()?)
    } else {
        None
    };
    let sessions = if let Some(pane) = &current {
        vec![field(pane, "session_id")?]
    } else {
        run(&["list-sessions", "-F", "#{session_id}"])?
            .lines()
            .map(str::to_owned)
            .collect()
    };
    let processes = ProcessTable::load()?;
    let mut saved_sessions = Vec::new();
    for session in sessions {
        let name = field(&session, "session_name")?;
        let windows: Vec<String> = if scope == Scope::Window {
            vec![field(current.as_deref().unwrap(), "window_id")?]
        } else {
            run(&["list-windows", "-t", &session, "-F", "#{window_id}"])?
                .lines()
                .map(str::to_owned)
                .collect()
        };
        let mut saved_windows = Vec::new();
        let mut active_window = 0;
        for window in windows {
            let target = format!("{session}:{window}");
            if field(&target, "window_active")? == "1" {
                active_window = saved_windows.len();
            }
            let pane_ids = run(&["list-panes", "-t", &target, "-F", "#{pane_id}"])?;
            let mut panes = Vec::new();
            let mut active_pane = 0;
            for pane in pane_ids.lines() {
                let active = field(pane, "pane_active")? == "1";
                if active {
                    active_pane = panes.len();
                }
                let pid = field(pane, "pane_pid")?
                    .parse()
                    .context("invalid pane PID")?;
                // Identify this invocation by PID/ancestry, not executable name:
                // aliases, renamed binaries and shell wrappers must not replay.
                let current_call = processes.foreground_is_current_call(pid);
                let argv = if current_call {
                    vec![]
                } else {
                    processes.foreground_argv(pid)?
                };
                panes.push(PaneSnapshot {
                    title: field(pane, "pane_title")?,
                    cwd: field(pane, "pane_current_path")?,
                    command: if current_call {
                        String::new()
                    } else {
                        field(pane, "pane_current_command")?
                    },
                    command_line: shell_words::join(&argv),
                    argv,
                    start_command: if current_call {
                        String::new()
                    } else {
                        field(pane, "pane_start_command")?
                    },
                    active,
                });
            }
            saved_windows.push(WindowSnapshot {
                index: field(&target, "window_index")?.parse()?,
                name: field(&target, "window_name")?,
                layout: field(&target, "window_layout")?,
                zoomed: field(&target, "window_zoomed_flag")? == "1",
                panes,
                active_pane,
            });
        }
        saved_sessions.push(SessionSnapshot {
            name,
            windows: saved_windows,
            active_window,
        });
    }
    let default_name = match scope {
        Scope::Window => saved_sessions
            .first()
            .and_then(|s| s.windows.first())
            .map(|w| w.name.clone())
            .unwrap_or_default(),
        Scope::Session => saved_sessions
            .first()
            .map(|s| s.name.clone())
            .unwrap_or_default(),
        Scope::All => format!("All sessions {}", Utc::now().format("%Y-%m-%d %H:%M")),
    };
    let snapshot = Snapshot {
        version: 1,
        id: Uuid::new_v4(),
        name: custom_name.unwrap_or(default_name),
        created_at: Utc::now(),
        scope,
        sessions: saved_sessions,
    };
    snapshot.validate()?;
    Ok(snapshot)
}
