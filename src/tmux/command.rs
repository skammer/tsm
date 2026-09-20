use crate::model::PaneSnapshot;
use anyhow::{Context, Result};

pub fn restore_command(pane: &PaneSnapshot) -> Result<Option<String>> {
    if !pane.argv.is_empty() {
        if ignored(&pane.argv) {
            return Ok(None);
        }
        return Ok(Some(shell_words::join(&pane.argv)));
    }
    if !pane.start_command.is_empty() {
        // tmux serializes its argv as shell-quoted words. One word is a shell
        // command; several words are executable + arguments.
        let args =
            shell_words::split(&pane.start_command).context("invalid saved start command")?;
        if args.len() == 1 {
            let inner = shell_words::split(&args[0]).context("invalid shell command")?;
            return Ok((!ignored(&inner)).then(|| args[0].clone()));
        }
        if !ignored(&args) {
            return Ok(Some(shell_words::join(args)));
        }
    }
    // Legacy snapshots do not contain argv. Retain their best-effort command.
    if !pane.command_line.is_empty() {
        let args = shell_words::split(&pane.command_line).context("invalid saved command line")?;
        if !ignored(&args) {
            return Ok(Some(shell_words::join(args)));
        }
    }
    Ok(
        (!pane.command.is_empty() && !ignored(std::slice::from_ref(&pane.command)))
            .then(|| shell_words::quote(&pane.command).into_owned()),
    )
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
            shell_words::split(&restore_command(&pane).unwrap().unwrap()).unwrap(),
            pane.argv
        );
        pane.argv = vec!["/tmp/tsm".into(), "save".into()];
        assert_eq!(restore_command(&pane).unwrap(), None);
    }
    #[test]
    fn start_command_round_trip() {
        let mut pane = pane();
        pane.start_command = "\"sleep 120\"".into();
        assert_eq!(restore_command(&pane).unwrap().unwrap(), "sleep 120");
        pane.start_command = "\"printf\" \"a b\"".into();
        assert_eq!(
            shell_words::split(&restore_command(&pane).unwrap().unwrap()).unwrap(),
            ["printf", "a b"]
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
