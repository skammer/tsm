use super::{
    client::{attach_or_switch, current_pane_optional, field, format_literal, run},
    command::{Replay, restore_command},
    launch::Launch,
};
use crate::model::{Scope, Snapshot, WindowSnapshot};
use anyhow::{Context, Result, bail, ensure};
use std::{env, path::Path};

pub struct RestorePlan {
    panes: Vec<RestorePane>,
    snapshot: Snapshot,
    target: Destination,
}

struct RestorePane {
    label: String,
    cwd: String,
    title: String,
    replay: Option<Replay>,
}

pub struct RestoreReport {
    pub targets: Vec<String>,
    pub failures: Vec<String>,
    pub jump_target: RestoredTarget,
}

pub enum RestoredTarget {
    Window(String),
    Session(String),
}

impl RestoredTarget {
    pub fn jump(&self) -> Result<()> {
        match self {
            Self::Window(target) => {
                run(&["select-window", "-t", target])?;
                Ok(())
            }
            Self::Session(id) => attach_or_switch(id),
        }
    }
}

#[derive(PartialEq, Eq)]
enum Destination {
    CurrentSession { id: String, name: String },
    NewSessions(Vec<String>),
}

impl Destination {
    fn resolve(snapshot: &Snapshot) -> Result<Self> {
        if snapshot.scope == Scope::Window
            && let Some(pane) = current_pane_optional()?
        {
            let id = field(&pane, "session_id")?;
            let name = field(&id, "session_name")?;
            return Ok(Self::CurrentSession { id, name });
        }
        let mut names = Vec::new();
        for session in &snapshot.sessions {
            let preferred = if snapshot.scope == Scope::All {
                &session.name
            } else {
                &snapshot.name
            };
            names.push(unique_name(preferred, &names)?);
        }
        Ok(Self::NewSessions(names))
    }

    fn check(&self, snapshot: &Snapshot) -> Result<()> {
        let unchanged = match self {
            Self::CurrentSession { .. } => Self::resolve(snapshot)? == *self,
            Self::NewSessions(names) => names.iter().all(|name| session_name_available(name)),
        };
        ensure!(unchanged, "restore destination changed; retry restore");
        Ok(())
    }
}

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

pub fn plan_restore(snapshot: &Snapshot) -> Result<RestorePlan> {
    snapshot.validate()?;
    let mut panes = Vec::new();
    for session in &snapshot.sessions {
        for window in &session.windows {
            for (index, pane) in window.panes.iter().enumerate() {
                let label = format!(
                    "{}:{} {} · pane {index}",
                    session.name, window.index, window.name
                );
                let replay = restore_command(pane).with_context(|| label.clone())?;
                panes.push(RestorePane {
                    label,
                    cwd: pane.cwd.clone(),
                    title: pane.title.clone(),
                    replay,
                });
            }
        }
    }
    check_directories(&panes)?;
    let target = Destination::resolve(snapshot)?;
    Ok(RestorePlan {
        panes,
        snapshot: snapshot.clone(),
        target,
    })
}

pub fn execute_restore(
    plan: RestorePlan,
    mut progress: impl FnMut(String),
) -> Result<RestoreReport> {
    progress("Checking restore destination".into());
    check_directories(&plan.panes)?;
    plan.target.check(&plan.snapshot)?;
    let executable = env::current_exe()
        .and_then(|path| path.canonicalize())
        .context("resolve tsm executable for pane launch")?;
    let executable = executable.to_str().context("tsm executable is not UTF-8")?;
    let mut launches = plan
        .panes
        .iter()
        .map(|pane| Launch::new(executable, &pane.cwd, pane.replay.as_ref()))
        .collect::<Result<Vec<_>>>()?;

    // Keep the launch sockets alive through rollback so waiting panes are
    // removed by tmux along with their windows or sessions.
    let mut transaction = Transaction::default();
    let result = restore_inner(&plan, &mut launches, &mut transaction, &mut progress);
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

fn check_directories(panes: &[RestorePane]) -> Result<()> {
    for pane in panes {
        ensure!(
            Path::new(&pane.cwd).is_dir(),
            "saved directory is unavailable: {}",
            pane.cwd
        );
    }
    Ok(())
}

fn restore_inner(
    plan: &RestorePlan,
    launches: &mut [Launch<'_>],
    tx: &mut Transaction,
    progress: &mut impl FnMut(String),
) -> Result<RestoreReport> {
    let snapshot = &plan.snapshot;
    let mut restored = Vec::new();
    let mut focuses = Vec::new();
    let mut jump_target = None;

    let mut pane_offset = 0;
    for (session_index, session) in snapshot.sessions.iter().enumerate() {
        let first = &session.windows[0];
        let first_launch = &launches[pane_offset];
        let (session_id, first_window, owns_session) = match &plan.target {
            Destination::CurrentSession { id, name } => {
                let window = create_window(id, first, None, first_launch)?;
                tx.created.push(("kill-window", window.clone()));
                restored.push(format!("window {} in {name}", first.name));
                (id.clone(), window, false)
            }
            Destination::NewSessions(names) => {
                let name = &names[session_index];
                progress(format!("Creating session {name}"));
                let mut args = vec![
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id} #{window_id}",
                    "-s",
                ];
                let session_name = format_literal(name);
                let window_name = format_literal(&first.name);
                args.extend([&session_name, "-n", &window_name]);
                args.extend(first_launch.args());
                let value = run(&args)?;
                let (id, window) = value
                    .split_once(' ')
                    .context("invalid new-session result")?;
                tx.created.push(("kill-session", id.into()));
                run(&["set-option", "-t", id, "renumber-windows", "off"])?;
                let first_index = first.index.to_string();
                if field(window, "window_index")? != first_index {
                    run(&[
                        "move-window",
                        "-s",
                        window,
                        "-t",
                        &format!("{id}:{first_index}"),
                    ])?;
                }
                restored.push(name.clone());
                (id.into(), window.into(), true)
            }
        };
        jump_target.get_or_insert_with(|| {
            if owns_session {
                RestoredTarget::Session(session_id.clone())
            } else {
                RestoredTarget::Window(format!("{session_id}:{first_window}"))
            }
        });
        let mut window_ids = Vec::new();
        for (index, window) in session.windows.iter().enumerate() {
            let id = if index == 0 {
                first_window.clone()
            } else {
                create_window(
                    &session_id,
                    window,
                    Some(window.index),
                    &launches[pane_offset],
                )?
            };
            let range = pane_offset..pane_offset + window.panes.len();
            pane_offset = range.end;
            progress(format!("Rebuilding window {}", window.name));
            let ids = prepare_window(&id, window, &launches[range.clone()])?;
            focuses.push((id.clone(), ids[window.active_pane].clone(), window.zoomed));
            window_ids.push(id);
            for (id, pane) in ids.iter().zip(&plan.panes[range]) {
                run(&["select-pane", "-t", id, "-T", &format_literal(&pane.title)])?;
            }
        }
        if owns_session {
            run(&["select-window", "-t", &window_ids[session.active_window]])?;
        }
    }
    let mut startups = Vec::new();
    let count = launches.len();
    for (index, (pane, launch)) in plan.panes.iter().zip(launches).enumerate() {
        let label = &pane.label;
        progress(format!("Starting pane {} of {count} · {label}", index + 1));
        if let Some(startup) = launch.start().with_context(|| label.clone())? {
            startups.push((label, startup));
        }
    }
    for (window, pane, zoomed) in focuses {
        run(&["select-pane", "-t", &pane])?;
        if zoomed {
            run(&["resize-pane", "-Z", "-t", &pane])?;
        }
        run(&["set-option", "-w", "-t", &window, "window-size", "latest"])?;
    }
    let failures = startups
        .into_iter()
        .filter_map(|(label, startup)| {
            progress(format!("Checking startup · {label}"));
            startup
                .wait()
                .err()
                .map(|error| format!("{label}: {error:#}"))
        })
        .collect();
    Ok(RestoreReport {
        targets: restored,
        failures,
        jump_target: jump_target.context("restore created no sessions or windows")?,
    })
}

fn create_window(
    session: &str,
    window: &WindowSnapshot,
    index: Option<usize>,
    launch: &Launch<'_>,
) -> Result<String> {
    let target = index
        .map(|index| format!("{session}:{index}"))
        .unwrap_or_else(|| session.into());
    let name = format_literal(&window.name);
    let mut args = vec![
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        &target,
        "-n",
        &name,
    ];
    args.extend(launch.args());
    run(&args)
}

fn prepare_window(
    id: &str,
    window: &WindowSnapshot,
    launches: &[Launch<'_>],
) -> Result<Vec<String>> {
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
    for launch in launches.iter().skip(1) {
        let mut args = vec![
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            ids.last().unwrap(),
        ];
        args.extend(launch.args());
        let next = run(&args)?;
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
    Ok(ids)
}

fn dimensions(layout: &str) -> Result<(usize, usize)> {
    let size = layout.split(',').nth(1).context("invalid layout header")?;
    let (width, height) = size.split_once('x').context("invalid layout dimensions")?;
    let (width, height) = (width.parse()?, height.parse()?);
    ensure!(width > 0 && height > 0, "invalid layout size");
    Ok((width, height))
}

fn unique_name(preferred: &str, reserved: &[String]) -> Result<String> {
    let base = preferred.replace(['.', ':'], "_");
    for suffix in 0..10_000 {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-restored-{suffix}")
        };
        if !reserved.contains(&name) && session_name_available(&name) {
            return Ok(name);
        }
    }
    bail!("no available session name for {preferred}")
}

fn session_name_available(name: &str) -> bool {
    run(&["has-session", "-t", &format!("={name}")]).is_err()
}
