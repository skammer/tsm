use anyhow::{Context, Result, bail};
use std::{env, process::Command};

pub fn run(args: &[&str]) -> Result<String> {
    let mut command = Command::new("tmux");
    if let Some(socket) = env::var_os("TSM_TMUX_SOCKET") {
        command.arg("-S").arg(socket);
    }
    let output = command
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
    env::var("TMUX_PANE").context("run inside tmux: TMUX_PANE is missing")
}

pub fn format_literal(value: &str) -> String {
    value.replace('#', "##")
}
