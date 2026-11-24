use super::{
    client::{current_pane, run},
    launch::{is_launch_helper, is_launch_start_command},
    process::ProcessTable,
};
use crate::{
    model::{PaneSnapshot, Scope, SessionSnapshot, Snapshot, WindowSnapshot},
    query::pick_target,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use uuid::Uuid;

const FIELD_SEPARATOR: char = '\x1f';

struct SessionRow {
    id: String,
    name: String,
}

struct WindowRow {
    id: String,
    index: usize,
    name: String,
    layout: String,
    active: bool,
    zoomed: bool,
}

fn row_format(fields: &[&str]) -> String {
    fields
        .iter()
        // Encode the percent sign before encoding record and field delimiters.
        .map(|field| format!("#{{s|%|%25|;s|\n|%0A|;s|{FIELD_SEPARATOR}|%1F|:{field}}}"))
        .collect::<Vec<_>>()
        .join(&FIELD_SEPARATOR.to_string())
}

fn decode_field(value: &str) -> Result<String> {
    let mut parts = value.split('%');
    let mut decoded = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let (escape, rest) = part
            .split_at_checked(2)
            .context("truncated tmux field escape")?;
        decoded.push(match escape {
            "25" => '%',
            "0A" => '\n',
            "1F" => FIELD_SEPARATOR,
            _ => bail!("invalid tmux field escape"),
        });
        decoded.push_str(rest);
    }
    Ok(decoded)
}

fn rows<const N: usize>(output: &str) -> impl Iterator<Item = Result<[String; N]>> + '_ {
    output.split_terminator('\n').map(|line| {
        line.split(FIELD_SEPARATOR)
            .map(decode_field)
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .ok()
            .context("unexpected number of fields in tmux output")
    })
}

fn list_sessions() -> Result<Vec<SessionRow>> {
    let format = row_format(&["session_id", "session_name"]);
    let output = run(&["list-sessions", "-F", &format])?;
    rows(&output)
        .map(|row| row.map(|[id, name]| SessionRow { id, name }))
        .collect()
}

fn list_windows(session: &str) -> Result<Vec<WindowRow>> {
    let format = row_format(&[
        "window_id",
        "window_index",
        "window_name",
        "window_layout",
        "window_active",
        "window_zoomed_flag",
    ]);
    let output = run(&["list-windows", "-t", session, "-F", &format])?;
    rows(&output)
        .map(|row| {
            let [id, index, name, layout, active, zoomed] = row?;
            Ok(WindowRow {
                id,
                index: index.parse().context("invalid tmux window index")?,
                name,
                layout,
                active: active == "1",
                zoomed: zoomed == "1",
            })
        })
        .collect()
}

fn target_for_pane(pane: &str) -> Result<(SessionRow, String)> {
    let format = row_format(&["session_id", "session_name", "window_id"]);
    let output = run(&["display-message", "-p", "-t", pane, &format])?;
    let [id, name, window] = rows(&output).next().context("no tmux session for pane")??;
    Ok((SessionRow { id, name }, window))
}

fn select_session(selector: &str) -> Result<SessionRow> {
    pick_target(
        selector,
        "session",
        list_sessions()?,
        |session| (None, session.name.as_str()),
        |session| session.name.clone(),
    )
}

fn select_window(
    session: &SessionRow,
    selector: &str,
    windows: Vec<WindowRow>,
) -> Result<WindowRow> {
    pick_target(
        selector,
        "window",
        windows,
        |window| (Some(window.index), window.name.as_str()),
        |window| format!("{}:{} ({})", session.name, window.index, window.name),
    )
}

fn capture_window(
    session: &SessionRow,
    window: WindowRow,
    processes: &ProcessTable,
) -> Result<WindowSnapshot> {
    let target = format!("{}:{}", session.id, window.id);
    let format = row_format(&[
        "pane_active",
        "pane_pid",
        "pane_title",
        "pane_current_path",
        "pane_current_command",
        "pane_start_command",
    ]);
    let output = run(&["list-panes", "-t", &target, "-F", &format])?;
    let mut active_pane = 0;
    let mut panes = Vec::new();
    for row in rows(&output) {
        let [active, pid, title, cwd, mut command, mut start_command] = row?;
        let active = active == "1";
        if active {
            active_pane = panes.len();
        }
        let pid = pid.parse().context("invalid pane PID")?;
        if is_launch_start_command(&start_command) {
            start_command.clear();
        }
        // Use ancestry so aliases, renamed binaries and wrappers aren't replayed.
        let argv = if processes.foreground_is_current_call(pid) {
            command.clear();
            start_command.clear();
            Vec::new()
        } else {
            let argv = processes.foreground_argv(pid)?;
            if is_launch_helper(&argv) {
                command.clear();
                Vec::new()
            } else {
                argv
            }
        };
        panes.push(PaneSnapshot {
            title,
            cwd,
            command,
            command_line: shell_words::join(&argv),
            argv,
            start_command,
            active,
        });
    }

    Ok(WindowSnapshot {
        index: window.index,
        name: window.name,
        layout: window.layout,
        panes,
        active_pane,
        zoomed: window.zoomed,
    })
}

pub fn capture(
    scope: Scope,
    target: Option<&str>,
    custom_name: Option<String>,
) -> Result<Snapshot> {
    let plan = match (&scope, target) {
        (Scope::All, Some(_)) => bail!("a target is not valid for scope 'all'"),
        (Scope::All, None) => list_sessions()?
            .into_iter()
            .map(|session| {
                let windows = list_windows(&session.id)?;
                Ok((session, windows))
            })
            .collect::<Result<Vec<_>>>()?,
        (Scope::Session, target) => {
            let session = match target {
                Some(selector) => select_session(selector)?,
                None => target_for_pane(&current_pane()?)?.0,
            };
            let windows = list_windows(&session.id)?;
            vec![(session, windows)]
        }
        (Scope::Window, Some(selector)) => {
            let (session, window) = match selector.split_once(':') {
                Some((session, window)) => (select_session(session)?, window),
                None => (target_for_pane(&current_pane()?)?.0, selector),
            };
            let windows = list_windows(&session.id)?;
            let selected = select_window(&session, window, windows)?;
            vec![(session, vec![selected])]
        }
        (Scope::Window, None) => {
            let (session, id) = target_for_pane(&current_pane()?)?;
            let window = list_windows(&session.id)?
                .into_iter()
                .find(|window| window.id == id)
                .context("window disappeared during capture")?;
            vec![(session, vec![window])]
        }
    };

    let processes = ProcessTable::load()?;
    let sessions = plan
        .into_iter()
        .map(|(session, windows)| {
            let active_window = windows.iter().position(|window| window.active).unwrap_or(0);
            let windows = windows
                .into_iter()
                .map(|window| capture_window(&session, window, &processes))
                .collect::<Result<Vec<_>>>()?;
            Ok(SessionSnapshot {
                name: session.name,
                windows,
                active_window,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let default_name = match scope {
        Scope::Window => sessions
            .first()
            .and_then(|session| session.windows.first())
            .map(|window| window.name.clone())
            .unwrap_or_default(),
        Scope::Session => sessions
            .first()
            .map(|session| session.name.clone())
            .unwrap_or_default(),
        Scope::All => format!("All sessions {}", Utc::now().format("%Y-%m-%d %H:%M")),
    };
    let snapshot = Snapshot {
        version: 1,
        id: Uuid::new_v4(),
        name: custom_name.unwrap_or(default_name),
        created_at: Utc::now(),
        scope,
        sessions,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_rows_preserve_control_characters_and_literal_escapes() {
        let output =
            format!("two%1Ffields{FIELD_SEPARATOR}%250A%0A tab\t\r\\字\n{FIELD_SEPARATOR}");
        let batches = rows::<2>(&output).collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(batches[0], ["two\x1ffields", "%0A\n tab\t\r\\字"]);
        assert_eq!(batches[1], ["", ""]);
        assert!(rows::<2>("incomplete").next().unwrap().is_err());
    }
}
