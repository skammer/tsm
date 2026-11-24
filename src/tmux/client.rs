use anyhow::{Context, Result, bail, ensure};
use std::{env, ffi::OsStr, os::unix::ffi::OsStrExt, path::Path, process::Command};

fn command() -> Command {
    let mut command = Command::new("tmux");
    if let Some(socket) = env::var_os("TSM_TMUX_SOCKET") {
        command.arg("-S").arg(socket);
    }
    command
}

pub fn run(args: &[&str]) -> Result<String> {
    let output = command()
        .args(args)
        .output()
        .context("run tmux (is it installed?)")?;
    if !output.status.success() {
        bail!(
            "tmux {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut value = String::from_utf8(output.stdout).context("tmux output is not UTF-8")?;
    // Remove the protocol newline only, preserving whitespace in names/commands.
    if value.ends_with('\n') {
        value.pop();
    }
    Ok(value)
}

pub fn field(target: &str, name: &str) -> Result<String> {
    run(&[
        "display-message",
        "-p",
        "-t",
        target,
        &format!("#{{{name}}}"),
    ])
}

pub fn current_pane() -> Result<String> {
    current_pane_optional()?.context("run inside tmux: TMUX_PANE is missing")
}

pub fn current_pane_optional() -> Result<Option<String>> {
    let pane = match env::var("TMUX_PANE") {
        Ok(pane) => pane,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(error) => return Err(error).context("read TMUX_PANE"),
    };
    let tmux = env::var_os("TMUX").context("cannot verify TMUX_PANE without TMUX")?;
    let (expected_pid, _) = tmux_context(&tmux)?;
    ensure!(!pane.is_empty(), "invalid TMUX_PANE environment value");
    let context = run(&["display-message", "-p", "-t", &pane, "#{pid} #{pane_id}"])?;
    ensure!(
        context == format!("{expected_pid} {pane}"),
        "TMUX_PANE does not belong to the active tmux server"
    );
    Ok(Some(pane))
}

fn tmux_context(tmux: &OsStr) -> Result<(u32, &[u8])> {
    // A socket path may itself contain commas. The last two fields are numeric.
    let mut parts = tmux.as_bytes().rsplitn(3, |&byte| byte == b',');
    let session = parts.next().unwrap_or_default();
    let pid = parts.next().unwrap_or_default();
    let socket = parts.next().context("invalid TMUX environment value")?;
    let expected_pid = std::str::from_utf8(pid)?
        .parse::<u32>()
        .context("invalid server PID in TMUX")?;
    ensure!(
        !session.is_empty() && !socket.is_empty(),
        "invalid TMUX environment value"
    );
    if let Some(override_socket) = env::var_os("TSM_TMUX_SOCKET") {
        let inherited = Path::new(OsStr::from_bytes(socket));
        let selected = Path::new(&override_socket);
        ensure!(
            inherited == selected
                || inherited
                    .canonicalize()
                    .is_ok_and(|path| { selected.canonicalize().is_ok_and(|other| other == path) }),
            "TMUX_PANE belongs to a different tmux server than TSM_TMUX_SOCKET"
        );
    }
    Ok((expected_pid, session))
}

pub fn attach_or_switch(session: &str) -> Result<()> {
    let origin = if let Some(pane) = current_pane_optional()? {
        field(&pane, "session_id")?
    } else if let Some(tmux) = env::var_os("TMUX") {
        // A tmux popup has TMUX but no TMUX_PANE.
        let (pid, id) = tmux_context(&tmux)?;
        let id = std::str::from_utf8(id)?
            .parse::<u32>()
            .context("invalid session ID in TMUX")?;
        let origin = format!("${id}");
        ensure!(
            field(&origin, "pid")? == pid.to_string(),
            "TMUX does not belong to the active tmux server"
        );
        origin
    } else {
        let status = command()
            .args(["attach-session", "-t", session])
            .status()
            .context("run tmux attach-session")?;
        ensure!(status.success(), "tmux attach-session exited with {status}");
        return Ok(());
    };
    let clients = run(&[
        "list-clients",
        "-t",
        &origin,
        "-F",
        "#{client_activity} #{client_name}",
    ])?;
    let (_, client) = clients
        .lines()
        .filter_map(|line| {
            let (activity, client) = line.split_once(' ')?;
            Some((activity.parse::<u64>().ok()?, client))
        })
        .max_by_key(|&(activity, _)| activity)
        .context("no attached tmux client in the calling session")?;
    run(&["switch-client", "-c", client, "-t", session])?;
    Ok(())
}

pub fn format_literal(value: &str) -> String {
    value.replace('#', "##")
}
