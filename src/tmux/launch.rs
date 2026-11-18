use super::{
    client::{format_literal, run},
    command::Replay,
};
use anyhow::{Result, ensure};

pub struct Launch {
    pub pane: String,
    pub cwd: String,
    pub command: Option<Replay>,
    pub title: String,
}

impl Launch {
    pub fn start(&self) -> Result<()> {
        let shell = run(&[
            "show-options",
            "-A",
            "-v",
            "-t",
            &self.pane,
            "default-shell",
        ])?;
        ensure!(!shell.is_empty(), "tmux default-shell is empty");

        let cwd = format_literal(&self.cwd);
        let mut args = vec!["respawn-pane", "-k", "-t", &self.pane, "-c", &cwd];
        let environment;
        if let Some(command) = &self.command {
            let argv = match command {
                Replay::Arguments(argv) => shell_words::join(argv),
                Replay::Script(script) => shell_words::join([shell.as_str(), "-c", script]),
            };
            environment = format!("TSM_RESTORE_COMMAND=exec /usr/bin/env -- {argv}");
            args.extend(["-e", &environment, "--"]);

            // A non-exec foreground job inherits the pane's tty. Re-enter the
            // prompt with exec to preserve the pane's PID, tty, and working
            // directory when the job returns.
            match shell.rsplit('/').next() {
                Some("sh" | "bash" | "zsh" | "dash" | "ksh") => {
                    args.extend([&shell, "-l", "-i", "-c", POSIX_REPLAY, &shell]);
                }
                Some("fish") => {
                    args.extend([&shell, "-l", "-i", "-c", FISH_REPLAY, &shell]);
                }
                _ => args.extend(["/bin/sh", "-i", "-c", POSIX_REPLAY, &shell]),
            }
        } else {
            args.extend(["--", &shell, "-l"]);
        }
        run(&args)?;
        run(&[
            "select-pane",
            "-t",
            &self.pane,
            "-T",
            &format_literal(&self.title),
        ])?;
        Ok(())
    }
}

const POSIX_REPLAY: &str = r#"
_tsm_replay=$TSM_RESTORE_COMMAND
unset TSM_RESTORE_COMMAND
/bin/sh -c "$_tsm_replay" || :
exec "$0" -i
"#;

const FISH_REPLAY: &str = r#"
set -l tsm_shell $argv[1]
set -l tsm_replay "$TSM_RESTORE_COMMAND"
set -e TSM_RESTORE_COMMAND
/bin/sh -c "$tsm_replay"
exec "$tsm_shell" -i
"#;
