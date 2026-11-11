use super::{
    client::{current_pane, field, run},
    process::ProcessTable,
};
use crate::model::{PaneSnapshot, Scope, SessionSnapshot, Snapshot, WindowSnapshot};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use uuid::Uuid;

/// A live tmux object (session or window) that a selector can refer to.
struct Listed {
    id: String,
    index: Option<usize>,
    name: String,
}

/// Resolve a selector against listed objects: numeric index first, then exact
/// name, then a unique case-insensitive name prefix.
fn pick(listed: &[Listed], selector: &str, kind: &str) -> Result<String> {
    ensure!(!selector.is_empty(), "{kind} selector is empty");
    if let Ok(index) = selector.parse::<usize>()
        && let Some(item) = listed.iter().find(|item| item.index == Some(index))
    {
        return Ok(item.id.clone());
    }
    if let Some(item) = listed.iter().find(|item| item.name == selector) {
        return Ok(item.id.clone());
    }
    let lower = selector.to_lowercase();
    let prefixed: Vec<_> = listed
        .iter()
        .filter(|item| item.name.to_lowercase().starts_with(&lower))
        .collect();
    match prefixed.as_slice() {
        [item] => Ok(item.id.clone()),
        [] => bail!("{kind} not found: {selector}"),
        _ => bail!(
            "ambiguous {kind} '{selector}', matches: {}",
            prefixed
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn resolve_session(selector: &str) -> Result<String> {
    let listing = run(&["list-sessions", "-F", "#{session_id}\t#{session_name}"])?;
    let listed: Vec<Listed> = listing
        .lines()
        .map(|line| {
            let (id, name) = line.split_once('\t').unwrap_or((line, ""));
            Listed {
                id: id.to_owned(),
                index: None,
                name: name.to_owned(),
            }
        })
        .collect();
    pick(&listed, selector, "session")
}

/// Resolve `[session:]window` into `(session_id, window_id)`.
/// Without a session part the current pane's session is used.
fn resolve_window(selector: &str) -> Result<(String, String)> {
    let (session_selector, window_selector) = match selector.split_once(':') {
        Some((session, window)) => (Some(session), window),
        None => (None, selector),
    };
    let session = match session_selector {
        Some(selector) => resolve_session(selector)?,
        None => field(&current_pane()?, "session_id")?,
    };
    let listing = run(&[
        "list-windows",
        "-t",
        &session,
        "-F",
        "#{window_id}\t#{window_index}\t#{window_name}",
    ])?;
    let listed: Vec<Listed> = listing
        .lines()
        .map(|line| {
            let mut parts = line.splitn(3, '\t');
            Listed {
                id: parts.next().unwrap_or_default().to_owned(),
                index: parts.next().and_then(|index| index.parse().ok()),
                name: parts.next().unwrap_or_default().to_owned(),
            }
        })
        .collect();
    let window = pick(&listed, window_selector, "window")?;
    Ok((session, window))
}

enum Windows {
    All,
    One(String),
}

pub fn capture(
    scope: Scope,
    target: Option<&str>,
    custom_name: Option<String>,
) -> Result<Snapshot> {
    let plan: Vec<(String, Windows)> = match (&scope, target) {
        (Scope::All, Some(_)) => bail!("a target is not valid for scope 'all'"),
        (Scope::All, None) => run(&["list-sessions", "-F", "#{session_id}"])?
            .lines()
            .map(|session| (session.to_owned(), Windows::All))
            .collect(),
        (Scope::Session, None) => {
            let pane = current_pane()?;
            vec![(field(&pane, "session_id")?, Windows::All)]
        }
        (Scope::Session, Some(selector)) => vec![(resolve_session(selector)?, Windows::All)],
        (Scope::Window, None) => {
            let pane = current_pane()?;
            vec![(
                field(&pane, "session_id")?,
                Windows::One(field(&pane, "window_id")?),
            )]
        }
        (Scope::Window, Some(selector)) => {
            let (session, window) = resolve_window(selector)?;
            vec![(session, Windows::One(window))]
        }
    };
    let processes = ProcessTable::load()?;
    let mut saved_sessions = Vec::new();
    for (session, filter) in plan {
        let name = field(&session, "session_name")?;
        let windows: Vec<String> = match &filter {
            Windows::One(window) => vec![window.clone()],
            Windows::All => run(&["list-windows", "-t", &session, "-F", "#{window_id}"])?
                .lines()
                .map(str::to_owned)
                .collect(),
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
