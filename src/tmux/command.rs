use crate::model::PaneSnapshot;
use anyhow::{Context, Result, ensure};

#[derive(Debug, PartialEq, Eq)]
pub enum Replay {
    Arguments(Vec<String>),
    Script(String),
}

pub fn replay_description(pane: &PaneSnapshot) -> Result<String> {
    Ok(describe_replay(restore_command(pane)?.as_ref()))
}

pub(super) fn describe_replay(replay: Option<&Replay>) -> String {
    match replay {
        Some(Replay::Arguments(argv)) => shell_words::join(argv),
        Some(Replay::Script(script)) => format!("Shell script · {script}"),
        None => "Interactive shell".into(),
    }
}

pub fn restore_command(pane: &PaneSnapshot) -> Result<Option<Replay>> {
    ensure!(
        !pane.argv.iter().any(|arg| arg.contains('\0'))
            && [&pane.command, &pane.command_line, &pane.start_command]
                .iter()
                .all(|value| !value.contains('\0')),
        "saved command contains a NUL byte"
    );
    if !pane.argv.is_empty() {
        ensure!(!pane.argv[0].is_empty(), "saved executable is empty");
        return Ok(arguments(pane.argv.clone()));
    }
    if !pane.start_command.is_empty() {
        // tmux serializes its argv as shell-quoted words. One word is a shell
        // command; several words are executable + arguments.
        let args =
            shell_words::split(&pane.start_command).context("invalid saved start command")?;
        if args.len() == 1 {
            let inner = shell_words::split(&args[0]).context("invalid shell command")?;
            return Ok((!ignored(&inner)).then(|| Replay::Script(args[0].clone())));
        }
        if !ignored(&args) {
            return Ok(Some(Replay::Arguments(args)));
        }
    }
    // Legacy snapshots do not contain argv. Retain their best-effort command.
    if !pane.command_line.is_empty() {
        let args = shell_words::split(&pane.command_line).context("invalid saved command line")?;
        if !ignored(&args) {
            return Ok(Some(Replay::Arguments(args)));
        }
    }
    Ok(arguments(
        (!pane.command.is_empty())
            .then(|| pane.command.clone())
            .into_iter()
            .collect(),
    ))
}

fn arguments(args: Vec<String>) -> Option<Replay> {
    (!ignored(&args)).then_some(Replay::Arguments(args))
}

fn ignored(args: &[String]) -> bool {
    let Some(executable) = args.first() else {
        return true;
    };
    let base = executable
        .rsplit('/')
        .next()
        .unwrap_or(executable)
        .trim_start_matches('-');
    base == "tsm"
        || (["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "nu"].contains(&base)
            && args.iter().skip(1).all(|arg| {
                ["-l", "-i", "-il", "-li", "--login", "--interactive"].contains(&arg.as_str())
            }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pane() -> PaneSnapshot {
        PaneSnapshot {
            title: String::new(),
            cwd: "/".into(),
            command: "zsh".into(),
            command_line: String::new(),
            argv: vec![],
            start_command: String::new(),
            active: true,
        }
    }
    #[test]
    fn actual_arguments_take_precedence_and_are_quoted() {
        let mut pane = pane();
        pane.start_command = "old-command".into();
        pane.argv = vec!["/bin/tool".into(), "a b".into(), "$(no)".into(), "".into()];
        assert_eq!(
            restore_command(&pane).unwrap(),
            Some(Replay::Arguments(pane.argv.clone()))
        );
        assert_eq!(
            replay_description(&pane).unwrap(),
            shell_words::join(&pane.argv)
        );
        pane.argv = vec!["/tmp/tsm".into(), "save".into()];
        assert_eq!(restore_command(&pane).unwrap(), None);
        assert_eq!(replay_description(&pane).unwrap(), "Interactive shell");
    }
    #[test]
    fn start_command_round_trip() {
        let mut pane = pane();
        pane.start_command = "\"sleep 120\"".into();
        assert_eq!(
            restore_command(&pane).unwrap(),
            Some(Replay::Script("sleep 120".into()))
        );
        assert_eq!(
            replay_description(&pane).unwrap(),
            "Shell script · sleep 120"
        );
        pane.start_command = "\"printf\" \"a b\"".into();
        assert_eq!(
            restore_command(&pane).unwrap(),
            Some(Replay::Arguments(vec!["printf".into(), "a b".into()]))
        );
    }

    #[test]
    fn shell_scripts_are_commands_but_login_shells_are_not() {
        let mut pane = pane();
        pane.argv = vec!["/bin/bash".into(), "build script.sh".into()];
        assert!(restore_command(&pane).unwrap().is_some());
        pane.argv = vec!["-zsh".into(), "-l".into()];
        assert!(restore_command(&pane).unwrap().is_none());
    }
}
