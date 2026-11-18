use super::{
    client::{current_pane, field, format_literal, run},
    command::restore_command,
    launch::Launch,
};
use crate::model::{Scope, Snapshot, WindowSnapshot};
use anyhow::{Context, Result, bail, ensure};
use std::path::Path;

#[derive(Default)]
struct Transaction {
    created: Vec<(&'static str, String)>,
}

impl Transaction {
    fn rollback(&self) -> Vec<String> {
        self.created
            .iter()
            .rev()
            .filter_map(|(command, target)| {
                run(&[command, "-t", target])
                    .err()
                    .map(|error| error.to_string())
            })
            .collect()
    }
}

pub fn restore(snapshot: &Snapshot) -> Result<Vec<String>> {
    snapshot.validate()?;
    for pane in snapshot
        .sessions
        .iter()
        .flat_map(|s| &s.windows)
        .flat_map(|w| &w.panes)
    {
        ensure!(
            Path::new(&pane.cwd).is_dir(),
            "saved directory is unavailable: {}",
            pane.cwd
        );
        restore_command(pane)?;
    }
    let mut transaction = Transaction::default();
    let result = restore_inner(snapshot, &mut transaction);
    if let Err(error) = result {
        let failures = transaction.rollback();
        if failures.is_empty() {
            return Err(error.context("restore failed; newly created tmux objects removed"));
        }
        bail!(
            "restore failed: {error:#}; cleanup failed: {}",
            failures.join("; ")
        );
    }
    result
}

fn restore_inner(snapshot: &Snapshot, tx: &mut Transaction) -> Result<Vec<String>> {
    let mut launches = Vec::new();
    let mut restored = Vec::new();
    let mut focuses = Vec::new();
    let existing_session = if snapshot.scope == Scope::Window {
        current_pane()
            .ok()
            .map(|pane| field(&pane, "session_id"))
            .transpose()?
    } else {
        None
    };

    for session in &snapshot.sessions {
        let (session_id, first_window, owns_session) = if let Some(id) = &existing_session {
            let window = create_window(id, &session.windows[0], None)?;
            tx.created.push(("kill-window", window.clone()));
            restored.push(format!(
                "window {} in {}",
                session.windows[0].name,
                field(id, "session_name")?
            ));
            (id.clone(), window, false)
        } else {
            let preferred = if snapshot.scope == Scope::All {
                &session.name
            } else {
                &snapshot.name
            };
            let name = unique_name(preferred)?;
            let value = run(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{session_id} #{window_id}",
                "-s",
                &format_literal(&name),
                "-n",
                &format_literal(&session.windows[0].name),
                "-c",
                &format_literal(&session.windows[0].panes[0].cwd),
                "/bin/sleep",
                "86400",
            ])?;
            let (id, window) = value
                .split_once(' ')
                .context("invalid new-session result")?;
            tx.created.push(("kill-session", id.into()));
            run(&["set-option", "-t", id, "renumber-windows", "off"])?;
            let first_index = session.windows[0].index.to_string();
            if field(window, "window_index")? != first_index {
                run(&[
                    "move-window",
                    "-s",
                    window,
                    "-t",
                    &format!("{id}:{first_index}"),
                ])?;
            }
            restored.push(name);
            (id.into(), window.into(), true)
        };
        let mut window_ids = vec![first_window];
        for window in session.windows.iter().skip(1) {
            let id = create_window(&session_id, window, Some(window.index))?;
            if !owns_session {
                tx.created.push(("kill-window", id.clone()));
            }
            window_ids.push(id);
        }
        for (id, window) in window_ids.iter().zip(&session.windows) {
            let pane = prepare_window(id, window, &mut launches)?;
            focuses.push((id.clone(), pane, window.zoomed));
        }
        if owns_session {
            run(&["select-window", "-t", &window_ids[session.active_window]])?;
        }
    }
    for launch in launches {
        launch.start()?;
    }
    for (window, pane, zoomed) in focuses {
        run(&["select-pane", "-t", &pane])?;
        if zoomed {
            run(&["resize-pane", "-Z", "-t", &pane])?;
        }
        run(&["set-option", "-w", "-t", &window, "window-size", "latest"])?;
    }
    Ok(restored)
}

fn create_window(session: &str, window: &WindowSnapshot, index: Option<usize>) -> Result<String> {
    let target = index
        .map(|index| format!("{session}:{index}"))
        .unwrap_or_else(|| session.into());
    run(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        &target,
        "-n",
        &format_literal(&window.name),
        "-c",
        &format_literal(&window.panes[0].cwd),
        "/bin/sleep",
        "86400",
    ])
}

fn prepare_window(id: &str, window: &WindowSnapshot, launches: &mut Vec<Launch>) -> Result<String> {
    let (width, height) = dimensions(&window.layout)?;
    run(&["set-option", "-w", "-t", id, "automatic-rename", "off"])?;
    run(&["set-option", "-w", "-t", id, "allow-rename", "off"])?;
    run(&["set-option", "-w", "-t", id, "window-size", "manual"])?;
    run(&[
        "resize-window",
        "-t",
        id,
        "-x",
        &width.to_string(),
        "-y",
        &height.max(window.panes.len() * 4).to_string(),
    ])?;
    let mut ids = vec![run(&["list-panes", "-t", id, "-F", "#{pane_id}"])?];
    for _ in window.panes.iter().skip(1) {
        let next = run(&[
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            ids.last().unwrap(),
            "",
        ])?;
        ids.push(next);
        run(&["select-layout", "-t", id, "even-vertical"])?;
    }
    run(&[
        "resize-window",
        "-t",
        id,
        "-x",
        &width.to_string(),
        "-y",
        &height.to_string(),
    ])?;
    run(&["select-layout", "-t", id, &window.layout])?;
    for (id, pane) in ids.iter().zip(&window.panes) {
        launches.push(Launch {
            pane: id.clone(),
            cwd: pane.cwd.clone(),
            command: restore_command(pane)?,
            title: pane.title.clone(),
        });
    }
    Ok(ids[window.active_pane].clone())
}

fn dimensions(layout: &str) -> Result<(usize, usize)> {
    let size = layout.split(',').nth(1).context("invalid layout header")?;
    let (width, height) = size.split_once('x').context("invalid layout dimensions")?;
    let (width, height) = (width.parse()?, height.parse()?);
    ensure!(width > 0 && height > 0, "invalid layout size");
    Ok((width, height))
}

fn unique_name(preferred: &str) -> Result<String> {
    let base = preferred.replace(['.', ':'], "_");
    for suffix in 0..10_000 {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-restored-{suffix}")
        };
        if run(&["has-session", "-t", &format!("={name}")]).is_err() {
            return Ok(name);
        }
    }
    bail!("no available session name for {preferred}")
}
