use super::{client::format_literal, command::Replay};
use anyhow::{Context, Result, bail, ensure};
use std::{
    env,
    io::{self, BufReader, ErrorKind, Read, Write},
    net::Shutdown,
    os::unix::{
        net::{SocketAddr, UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const LAUNCH_ARG: &str = "__tsm-launch";
const REPLAY_ARG: &str = "__tsm-replay";
const START_TIMEOUT: Duration = Duration::from_secs(5);

pub fn is_launch_helper(args: &[String]) -> bool {
    matches!(args, [_, operation, _] if operation == LAUNCH_ARG)
}

pub fn is_launch_start_command(command: &str) -> bool {
    command.contains(LAUNCH_ARG)
        && shell_words::split(command).is_ok_and(|args| is_launch_helper(&args))
}

pub struct Launch<'a> {
    executable: &'a str,
    cwd: String,
    command: Option<&'a Replay>,
    socket: String,
    startup: Startup,
}

impl<'a> Launch<'a> {
    pub fn new(executable: &'a str, cwd: &str, command: Option<&'a Replay>) -> Result<Self> {
        let (startup, socket) = Startup::new()?;
        Ok(Self {
            executable,
            cwd: format_literal(cwd),
            command,
            socket: socket
                .into_os_string()
                .into_string()
                .ok()
                .context("startup socket is not UTF-8")?,
            startup,
        })
    }

    pub fn args(&self) -> [&str; 6] {
        [
            "-c",
            &self.cwd,
            "--",
            self.executable,
            LAUNCH_ARG,
            &self.socket,
        ]
    }

    pub fn start(&mut self) -> Result<Option<&Startup>> {
        // The pane already owns its PTY. Let it exec the shell only after
        // every window and its layout have been created successfully.
        let message = serde_json::to_vec(&self.command).context("encode pane launch")?;
        self.startup.deadline = Instant::now() + START_TIMEOUT;
        let (mut stream, _) = self
            .startup
            .wait_for(|| self.startup.listener.accept())
            .context("accept pane launch")?;
        stream.set_nonblocking(true)?;
        let mut remaining = message.as_slice();
        while !remaining.is_empty() {
            let count = self
                .startup
                .wait_for(|| stream.write(remaining))
                .context("send pane launch")?;
            ensure!(count != 0, "pane launch socket stopped accepting data");
            remaining = &remaining[count..];
        }
        stream
            .shutdown(Shutdown::Write)
            .context("release pane launch")?;
        self.startup.deadline = Instant::now() + START_TIMEOUT;
        Ok(self.command.map(|_| &self.startup))
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

    pub fn wait(&self) -> Result<()> {
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

pub fn run_pane_helper() -> Option<Result<()>> {
    let mut args = env::args_os();
    let executable = args.next()?;
    match args.next()?.to_str()? {
        LAUNCH_ARG => Some((|| {
            let socket = args.next().context("missing pane startup socket")?;
            let stream = UnixStream::connect(&socket).context("connect to pane launch")?;
            let replay: Option<Replay> =
                serde_json::from_reader(BufReader::new(stream)).context("read pane launch")?;
            let executable = executable.to_str().context("tsm executable is not UTF-8")?;
            let socket = socket.to_str().context("startup socket is not UTF-8")?;
            launch_shell(executable, socket, replay)
        })()),
        REPLAY_ARG => Some((|| {
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
        })()),
        _ => None,
    }
}

fn launch_shell(executable: &str, socket: &str, replay: Option<Replay>) -> Result<()> {
    // tmux sets SHELL to the resolved default-shell for each new pane.
    let shell = env::var("SHELL").context("read tmux default shell")?;
    let mut process = Command::new(&shell);
    if let Some(replay) = &replay {
        let mut words = vec![executable, REPLAY_ARG, socket];
        match replay {
            Replay::Arguments(argv) => words.extend(argv.iter().map(String::as_str)),
            Replay::Script(script) => words.extend([shell.as_str(), "-c", script]),
        }
        let environment = format!("exec {}", shell_words::join(words));

        // Keep the job in the foreground, then exec the prompt to preserve
        // the pane's PID, tty, and working directory when the job returns.
        match shell.rsplit('/').next() {
            Some("sh" | "bash" | "zsh" | "dash" | "ksh") => {
                process.args(["-l", "-i", "-c", POSIX_REPLAY, &shell]);
            }
            Some("fish") => {
                process.args(["-l", "-i", "-c", FISH_REPLAY, &shell]);
            }
            _ => {
                process = Command::new("/bin/sh");
                process.args(["-i", "-c", POSIX_REPLAY, &shell]);
            }
        }
        process.env("TSM_RESTORE_COMMAND", environment);
    } else {
        process.arg("-l");
    }
    let error = process.exec();
    Err(error).with_context(|| format!("start pane shell {shell}"))
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
        drop(startup);
        assert!(!path.exists());
    }
}
