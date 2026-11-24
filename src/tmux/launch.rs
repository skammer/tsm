use super::{
    client::{format_literal, run},
    command::Replay,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    env,
    io::{self, ErrorKind, Read, Write},
    os::unix::{
        net::{SocketAddr, UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const REPLAY_ARG: &str = "__tsm-replay";
const START_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Launch<'a> {
    pub pane: String,
    pub cwd: &'a str,
    pub command: Option<&'a Replay>,
    pub title: &'a str,
}

impl Launch<'_> {
    pub fn start(&self) -> Result<Option<Startup>> {
        let shell = run(&[
            "show-options",
            "-A",
            "-v",
            "-t",
            &self.pane,
            "default-shell",
        ])?;
        ensure!(!shell.is_empty(), "tmux default-shell is empty");

        let cwd = format_literal(self.cwd);
        let mut args = vec!["respawn-pane", "-k", "-t", &self.pane, "-c", &cwd];
        let environment;
        let startup = if let Some(command) = self.command {
            let (startup, socket) = Startup::new()?;
            let executable = env::current_exe()
                .and_then(|path| path.canonicalize())
                .context("resolve tsm executable for pane replay")?;
            let executable = executable.to_str().context("tsm executable is not UTF-8")?;
            let socket = socket.to_str().context("startup socket is not UTF-8")?;
            let mut words = vec![executable, REPLAY_ARG, socket];
            match command {
                Replay::Arguments(argv) => words.extend(argv.iter().map(String::as_str)),
                Replay::Script(script) => words.extend([shell.as_str(), "-c", script]),
            }
            environment = format!("TSM_RESTORE_COMMAND=exec {}", shell_words::join(words));
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
            Some(startup)
        } else {
            args.extend(["--", &shell, "-l"]);
            None
        };
        run(&args)?;
        run(&[
            "select-pane",
            "-t",
            &self.pane,
            "-T",
            &format_literal(self.title),
        ])?;
        Ok(startup)
    }
}

pub struct Startup {
    listener: UnixListener,
    _directory: tempfile::TempDir,
    deadline: Instant,
}

impl Startup {
    fn new() -> Result<(Self, PathBuf)> {
        Self::new_in(&env::temp_dir())
    }

    fn new_in(temp_root: &Path) -> Result<(Self, PathBuf)> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("tsm-");
        let mut directory = builder.tempdir_in(std::path::absolute(temp_root)?)?;
        let mut socket = directory.path().join("ack");
        let address = match SocketAddr::from_pathname(&socket) {
            Ok(address) => address,
            Err(error) if error.kind() == ErrorKind::InvalidInput => {
                // sockaddr_un can be much shorter than a valid TMPDIR path.
                directory = builder.tempdir_in("/tmp")?;
                socket = directory.path().join("ack");
                SocketAddr::from_pathname(&socket)?
            }
            Err(error) => return Err(error).context("resolve pane startup socket"),
        };
        let listener = UnixListener::bind_addr(&address).context("bind pane startup socket")?;
        listener.set_nonblocking(true)?;
        Ok((
            Self {
                listener,
                _directory: directory,
                deadline: Instant::now() + START_TIMEOUT,
            },
            socket,
        ))
    }

    pub fn wait(self) -> Result<()> {
        let (mut stream, _) = self
            .wait_for(|| self.listener.accept())
            .context("accept pane startup")?;
        // macOS does not support socket read timeouts on Unix-domain sockets.
        stream.set_nonblocking(true)?;
        let mut error = Vec::new();
        let mut buffer = [0; 256];
        loop {
            let count = self
                .wait_for(|| stream.read(&mut buffer))
                .context("read pane startup acknowledgement")?;
            if count == 0 {
                break;
            }
            error.extend_from_slice(&buffer[..count]);
        }
        // UnixStream is close-on-exec. EOF with no error means exec succeeded.
        ensure!(error.is_empty(), "{}", String::from_utf8_lossy(&error));
        Ok(())
    }

    fn wait_for<T>(&self, mut operation: impl FnMut() -> io::Result<T>) -> Result<T> {
        loop {
            match operation() {
                Ok(value) => return Ok(value),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    let remaining = self.deadline.saturating_duration_since(Instant::now());
                    ensure!(
                        !remaining.is_zero(),
                        "timed out waiting for program startup"
                    );
                    thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub fn run_pane_replay() -> Option<Result<()>> {
    let mut args = env::args_os().skip(1);
    if args.next()?.as_os_str() != REPLAY_ARG {
        return None;
    }
    Some((|| {
        let socket = args.next().context("missing pane startup socket")?;
        let program = args.next().context("missing pane program")?;
        // Login files can outlast the reporting deadline. Keep the already
        // confirmed replay running if its listener has been removed.
        let startup = UnixStream::connect(socket);
        let error = Command::new(&program).args(args).exec();
        let message = format!("{}: {error}", program.to_string_lossy());
        if let Ok(mut startup) = startup {
            let _ = startup.write_all(message.as_bytes());
        }
        bail!("{message}");
    })())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_socket_falls_back_when_temp_directory_is_too_long() {
        let temp = tempfile::tempdir().unwrap();
        let long_path = temp.path().join("long".repeat(32));
        std::fs::create_dir(&long_path).unwrap();

        let (startup, path) = Startup::new_in(&long_path).unwrap();
        assert!(path.starts_with("/tmp"));
        assert!(!path.starts_with(&long_path));
        drop(UnixStream::connect(&path).unwrap());
        startup.wait().unwrap();
        assert!(!path.exists());
    }
}
