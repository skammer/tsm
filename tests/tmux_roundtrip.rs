use serde_json::Value;
use std::{
    fs,
    process::{Command, Output},
    thread,
    time::Duration,
};
use tempfile::TempDir;

struct Server {
    dir: TempDir,
}
impl Server {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn socket(&self) -> std::path::PathBuf {
        self.dir.path().join("tmux.sock")
    }
    fn tmux_command(&self) -> Command {
        let mut command = Command::new("tmux");
        command
            .arg("-S")
            .arg(self.socket())
            .args(["-f", "/dev/null"])
            .env("HOME", self.dir.path())
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }
    fn tmux(&self, args: &[&str]) -> String {
        output(self.tmux_command().args(args).output().unwrap())
    }
    fn tsm_output(&self, args: &[&str], pane: Option<&str>) -> Output {
        self.tsm_output_with_context(args, pane.map(|pane| (self, pane)))
    }
    fn tsm_output_with_context(&self, args: &[&str], context: Option<(&Server, &str)>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tsm"));
        command
            .args(args)
            .env("TSM_TMUX_SOCKET", self.socket())
            .env("TSM_DATA_DIR", self.dir.path().join("store"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        if let Some((origin, pane)) = context {
            let server =
                origin.tmux(&["display-message", "-p", "-t", pane, "#{pid} #{session_id}"]);
            let (pid, session) = server.split_once(' ').unwrap();
            command.env("TMUX_PANE", pane).env(
                "TMUX",
                format!(
                    "{},{pid},{}",
                    origin.socket().display(),
                    session.trim_start_matches('$')
                ),
            );
        }
        command.output().unwrap()
    }
    fn tsm(&self, args: &[&str], pane: Option<&str>) -> String {
        output(self.tsm_output(args, pane))
    }
    fn snapshot(&self, id: &str) -> Value {
        let entry = fs::read_dir(self.dir.path().join("store"))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .replace('-', "")
                    .starts_with(id)
            })
            .unwrap();
        serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.socket())
            .args(["kill-server"])
            .output();
    }
}
fn output(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim_end().into()
}
fn saved_id(output: &str) -> &str {
    output.split_whitespace().next().unwrap()
}
fn geometry(server: &Server, window: &str) -> String {
    server.tmux(&[
        "list-panes",
        "-t",
        window,
        "-F",
        "#{pane_left},#{pane_top},#{pane_width},#{pane_height}:#{pane_title}:#{pane_active}",
    ])
}
fn rewrite_snapshot(server: &Server, snapshot: &Value) {
    let path = server
        .dir
        .path()
        .join("store")
        .join(format!("{}.json", snapshot["id"].as_str().unwrap()));
    fs::write(path, serde_json::to_vec(snapshot).unwrap()).unwrap();
}

#[test]
#[ignore = "starts dedicated tmux servers; needs process inspection permissions"]
fn batched_capture_preserves_text_and_rejects_foreign_pane_ids() {
    let server = Server::new();
    let other = Server::new();
    let name = "  editor\t%1F\nlogs\x1f 字\\";
    let cwd = server.dir.path().join(name);
    fs::create_dir(&cwd).unwrap();
    let pane = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "source",
        "-n",
        name,
        "-c",
        cwd.to_str().unwrap(),
        "/bin/sleep",
        "120",
    ]);
    let other_pane = other.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "foreign",
        "/bin/sleep",
        "120",
    ]);
    assert_eq!(pane, other_pane, "pane IDs are local to each server");
    let saved = server.tsm(&["save", "window", "-n", "batched"], Some(&pane));
    let snapshot = server.snapshot(saved_id(&saved));
    let window = &snapshot["sessions"][0]["windows"][0];
    assert_eq!(window["name"], name);
    assert_eq!(
        window["panes"][0]["cwd"],
        cwd.canonicalize().unwrap().to_str().unwrap()
    );
    let selected = server.tsm(&["peek", "window", &format!("source:{name}")], None);
    assert!(selected.contains(name));

    for args in [
        &["peek"][..],
        &["peek", "session"][..],
        &["restore", "batched"][..],
    ] {
        let failed = server.tsm_output_with_context(args, Some((&other, &other_pane)));
        assert!(!failed.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("different tmux server"),
            "{args:?}: {:?}",
            failed.stderr
        );
    }
    assert_eq!(
        server
            .tmux(&["list-windows", "-F", "#{window_id}"])
            .lines()
            .count(),
        1
    );
    assert!(
        server
            .tsm_output_with_context(&["peek", "window", "source:0"], Some((&other, &other_pane)))
            .status
            .success(),
        "explicitly qualified targets can still use the socket override"
    );
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn restore_reports_partial_launch_failure_and_keeps_both_panes_usable() {
    let server = Server::new();
    let pane = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "source",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&["split-window", "-d", "-t", &pane, "/bin/sleep", "120"]);
    let saved = server.tsm(&["save", "session", "-n", "replay"], Some(&pane));
    let mut snapshot = server.snapshot(saved_id(&saved));
    let panes = snapshot["sessions"][0]["windows"][0]["panes"]
        .as_array_mut()
        .unwrap();
    let missing = server.dir.path().join("missing-program");
    panes[0]["argv"] = serde_json::json!([missing]);
    panes[0]["command_line"] = "stale preview".into();
    panes[1]["argv"] =
        serde_json::json!(["/bin/sh", "-c", "printf 'REPLAY_CHECK\\n'; read answer"]);
    rewrite_snapshot(&server, &snapshot);

    let preview = server.tsm(&["inspect", "replay"], None);
    assert!(preview.contains("missing-program"));
    assert!(!preview.contains("stale preview"));
    let restored = server.tsm_output(&["restore", "replay"], None);
    assert!(!restored.status.success());
    assert!(String::from_utf8_lossy(&restored.stdout).contains("created layout"));
    let error = String::from_utf8_lossy(&restored.stderr);
    assert!(error.contains("program startup issues"), "{error}");
    assert!(error.contains("missing-program"), "{error}");

    wait_screen(&server, "replay:0.1", "REPLAY_CHECK");
    server.tmux(&["send-keys", "-t", "replay:0.1", "Enter"]);
    for target in ["replay:0.0", "replay:0.1"] {
        wait_screen(&server, target, "$");
        server.tmux(&[
            "send-keys",
            "-t",
            target,
            "-l",
            "printf 'SHELL_%s\\n' ALIVE",
        ]);
        server.tmux(&["send-keys", "-t", target, "Enter"]);
        wait_screen(&server, target, "SHELL_ALIVE");
    }
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn restored_programs_leave_a_working_shell_in_the_same_pane() {
    let server = Server::new();
    let cwd = server.dir.path().join("work #{session_name}");
    fs::create_dir(&cwd).unwrap();
    let cwd = cwd.to_str().unwrap();
    let initial = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "source",
        "/bin/sleep",
        "120",
    ]);
    for hook in ["after-new-session", "after-new-window"] {
        server.tmux(&[
            "set-hook",
            "-g",
            hook,
            "set-option -Fp @tsm-test-birth-pid '#{pane_pid}'",
        ]);
    }
    let saved = server.tsm(
        &["save", "window", "-n", "Replay #{session_name}"],
        Some(&initial),
    );
    let id = saved_id(&saved);
    let mut snapshot = server.snapshot(id);
    snapshot["sessions"][0]["windows"][0]["name"] = "Window #{session_name}".into();
    let home = server.dir.path().to_str().unwrap();
    server.tmux(&["set-environment", "-g", "ZDOTDIR", home]);
    fs::write(server.dir.path().join(".zshrc"), "PROMPT='tsm-test> '\n").unwrap();

    for (shell, exit_key) in [
        ("/bin/sh", "Enter"),
        ("/bin/bash", "C-c"),
        ("/bin/zsh", "Enter"),
    ] {
        if !std::path::Path::new(shell).is_file() {
            continue;
        }
        server.tmux(&["set-option", "-g", "default-shell", shell]);
        let pane = &mut snapshot["sessions"][0]["windows"][0]["panes"][0];
        pane["cwd"] = cwd.into();
        pane["title"] = "Pane #{session_name}".into();
        let arg = "a b;$(no)'\nline\t";
        pane["argv"] = serde_json::json!([
            "/bin/sh",
            "-c",
            "test \"$1\" = \"$2\" && test -z \"${TSM_RESTORE_COMMAND+x}\" || exit 3; pwd; printf 'APP_WAITING\\n'; read -r answer; exit 7",
            "replayed-app",
            arg,
            arg,
        ]);
        rewrite_snapshot(&server, &snapshot);
        let context = (shell == "/bin/zsh").then_some(initial.as_str());
        let restored = server.tsm(&["restore", id], context);
        let pane = if context.is_some() {
            server.tmux(&["list-panes", "-t", "source:1", "-F", "#{pane_id}"])
        } else {
            let session = restored.split_once(": ").unwrap().1;
            assert!(session.starts_with("Replay #{session_name}"));
            server.tmux(&["list-panes", "-t", session, "-F", "#{pane_id}"])
        };
        let birth_pid = server.tmux(&["show-options", "-pv", "-t", &pane, "@tsm-test-birth-pid"]);
        assert_eq!(
            server.tmux(&["display-message", "-p", "-t", &pane, "#{pane_pid}"]),
            birth_pid
        );
        assert_eq!(
            server.tmux(&[
                "display-message",
                "-p",
                "-t",
                &pane,
                "#{window_name}:#{pane_title}"
            ]),
            "Window #{session_name}:Pane #{session_name}",
        );
        let screen = wait_screen(&server, &pane, "APP_WAITING");
        assert!(screen.contains("work #{session_name}"), "{screen}");
        server.tmux(&["send-keys", "-t", &pane, exit_key]);
        wait_screen(
            &server,
            &pane,
            if shell == "/bin/zsh" {
                "tsm-test>"
            } else {
                "$"
            },
        );
        server.tmux(&[
            "send-keys",
            "-t",
            &pane,
            "-l",
            "printf 'SHELL_%s\\n' ALIVE; pwd",
        ]);
        server.tmux(&["send-keys", "-t", &pane, "Enter"]);
        let screen = wait_screen(&server, &pane, "SHELL_ALIVE");
        assert!(
            screen.matches("work #{session_name}").count() >= 2,
            "{screen}"
        );
        assert_eq!(
            server.tmux(&[
                "display-message",
                "-p",
                "-t",
                &pane,
                "#{pane_pid}:#{pane_dead}"
            ]),
            format!("{birth_pid}:0"),
        );
    }
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn current_save_call_is_not_archived_or_replayed() {
    for wrapped in [false, true] {
        let server = Server::new();
        let first = server.tmux(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            "self-capture",
            "-n",
            "editor",
            "-c",
            "/tmp",
            "/bin/sh",
        ]);
        server.tmux(&["set-option", "-w", "-t", &first, "remain-on-exit", "on"]);
        server.tmux(&["select-pane", "-t", &first, "-T", "caller"]);
        let other = server.tmux(&[
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &first,
            "/bin/sleep",
            "120",
        ]);
        server.tmux(&["select-pane", "-t", &other, "-T", "other"]);

        let binary = if wrapped {
            let renamed = server.dir.path().join("tms");
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_tsm"), &renamed).unwrap();
            renamed
        } else {
            env!("CARGO_BIN_EXE_tsm").into()
        };
        let invocation = shell_words::join([
            "env".to_owned(),
            format!("TSM_TMUX_SOCKET={}", server.socket().display()),
            format!("TSM_DATA_DIR={}", server.dir.path().join("store").display()),
            binary.to_string_lossy().into_owned(),
            "save".into(),
            "session".into(),
            "-n".into(),
            "Self capture".into(),
        ]);
        if wrapped {
            // A shell is foreground group leader; start_command also contains
            // the save invocation, so filtering only argv would be insufficient.
            server.tmux(&[
                "respawn-pane",
                "-k",
                "-t",
                &first,
                "/bin/sh",
                "-c",
                &format!("{invocation}; sleep 120"),
            ]);
        } else {
            server.tmux(&["send-keys", "-t", &first, "-l", &invocation]);
            server.tmux(&["send-keys", "-t", &first, "Enter"]);
        }
        let store = server.dir.path().join("store");
        let mut captured = None;
        for _ in 0..100 {
            if let Ok(entries) = fs::read_dir(&store) {
                for entry in entries.map(Result::unwrap) {
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        captured = Some(
                            serde_json::from_slice::<Value>(&fs::read(entry.path()).unwrap())
                                .unwrap(),
                        );
                    }
                }
            }
            if captured.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let snapshot = captured.unwrap_or_else(|| {
            panic!(
                "save failed: {}",
                server.tmux(&["capture-pane", "-p", "-t", &first])
            )
        });
        let panes = snapshot["sessions"][0]["windows"][0]["panes"]
            .as_array()
            .unwrap();
        assert_eq!(panes.len(), 2);
        for field in ["command", "command_line", "start_command"] {
            assert_eq!(panes[0][field], "", "wrapped={wrapped}, field={field}");
        }
        assert_eq!(panes[0]["argv"], serde_json::json!([]));
        assert_eq!(panes[0]["title"], "caller");
        assert!(!panes[0]["cwd"].as_str().unwrap().is_empty());
        assert_eq!(panes[1]["argv"], serde_json::json!(["/bin/sleep", "120"]));

        server.tsm(&["restore", "Self capture"], None);
        let restored = server.tmux(&["list-panes", "-t", "Self capture:", "-F", "#{pane_id}"]);
        let restored_caller = restored.lines().next().unwrap();
        let mut shell = String::new();
        for _ in 0..40 {
            shell = server.tmux(&[
                "display-message",
                "-p",
                "-t",
                restored_caller,
                "#{pane_current_command}",
            ]);
            if ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "nu"].contains(&shell.as_str())
            {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "nu"].contains(&shell.as_str()),
            "restored command: {shell}"
        );
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            fs::read_dir(&store).unwrap().count(),
            1,
            "restore must not replay save"
        );
    }
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn peek_and_dry_run_match_capture_without_touching_storage() {
    let server = Server::new();
    let first = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "alpha",
        "-n",
        "editor",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&[
        "new-window",
        "-d",
        "-t",
        "alpha",
        "-n",
        "logs",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "-n",
        "other",
        "/bin/sleep",
        "120",
    ]);
    let store_path = server.dir.path().join("store");
    for (scope, windows, sessions) in [("window", 1, 1), ("session", 2, 1), ("all", 3, 2)] {
        let pane = (scope != "all").then_some(first.as_str());
        let peek = server.tsm(&["peek", scope, "-n", "Preview name"], pane);
        let dry_run = server.tsm(&["save", "--dry-run", scope, "-n", "Preview name"], pane);
        assert!(peek.starts_with("Preview only"));
        assert!(peek.contains("Preview name"));
        assert_eq!(peek.matches("Session:").count(), sessions);
        assert_eq!(peek.matches("Window ").count(), windows);
        // Header contains a new UUID/time; captured data must be identical.
        assert_eq!(
            peek.lines().skip(3).collect::<Vec<_>>(),
            dry_run.lines().skip(3).collect::<Vec<_>>()
        );
        assert!(!store_path.exists(), "preview must not create the store");
    }
    // A path that cannot be a directory proves preview does not read storage.
    fs::write(&store_path, "do not touch").unwrap();
    assert!(
        server
            .tsm(&["peek"], Some(&first))
            .contains("Window 0: editor")
    );
    assert!(
        server
            .tsm(&["save", "--dry-run"], Some(&first))
            .contains("Window 0: editor")
    );
    assert_eq!(fs::read_to_string(&store_path).unwrap(), "do not touch");
    fs::remove_file(&store_path).unwrap();

    let saved = server.tsm(&["save", "session", "-n", "Stored"], Some(&first));
    let inspected = server.tsm(&["inspect", saved_id(&saved)], None);
    let file = fs::read_dir(&store_path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = fs::read(&file).unwrap();
    for args in [
        vec!["peek", "session"],
        vec!["save", "session", "--dry-run"],
    ] {
        let preview = server.tsm(&args, Some(&first));
        assert_eq!(
            preview.lines().skip(3).collect::<Vec<_>>(),
            inspected.lines().skip(2).collect::<Vec<_>>()
        );
        assert_eq!(fs::read(&file).unwrap(), before);
        assert_eq!(fs::read_dir(&store_path).unwrap().count(), 1);
    }
    assert!(!server.tsm_output(&["peek"], None).status.success());
    assert!(
        !server
            .tsm_output(&["save", "--dry-run"], None)
            .status
            .success()
    );
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn save_and_peek_accept_window_and_session_targets() {
    let server = Server::new();
    let first = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "alpha",
        "-n",
        "editor",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&[
        "new-window",
        "-d",
        "-t",
        "alpha",
        "-n",
        "logs",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "-n",
        "other",
        "/bin/sleep",
        "120",
    ]);

    // Window by index and by name in the current session.
    for selector in ["1", "logs", "LOG"] {
        let saved = server.tsm(&["save", "window", selector], Some(&first));
        let snapshot = server.snapshot(saved_id(&saved));
        assert_eq!(snapshot["scope"], "window");
        assert_eq!(snapshot["sessions"][0]["name"], "alpha");
        assert_eq!(snapshot["sessions"][0]["windows"][0]["name"], "logs");
        assert_eq!(snapshot["sessions"][0]["windows"][0]["index"], 1);
    }
    // A bare first positional that is not a scope is a window target.
    let saved = server.tsm(&["save", "1"], Some(&first));
    assert_eq!(
        server.snapshot(saved_id(&saved))["sessions"][0]["windows"][0]["name"],
        "logs"
    );
    // session:window works from outside tmux (no TMUX_PANE).
    let saved = server.tsm(&["save", "window", "beta:other"], None);
    let snapshot = server.snapshot(saved_id(&saved));
    assert_eq!(snapshot["sessions"][0]["name"], "beta");
    assert_eq!(snapshot["sessions"][0]["windows"][0]["name"], "other");
    // Session by exact name and by unique prefix, without TMUX_PANE.
    for (selector, session) in [("beta", "beta"), ("alp", "alpha")] {
        let saved = server.tsm(&["save", "session", selector], None);
        let snapshot = server.snapshot(saved_id(&saved));
        assert_eq!(snapshot["scope"], "session");
        assert_eq!(snapshot["sessions"][0]["name"], session);
    }
    // peek accepts the same targets and shows the non-current window.
    let peek = server.tsm(&["peek", "window", "beta:0"], None);
    assert!(peek.contains("Preview only"));
    assert!(peek.contains("Window 0: other"));
    // inspect narrows a saved session to a single window.
    let saved = server.tsm(&["save", "session", "alpha"], Some(&first));
    let id = saved_id(&saved).to_owned();
    let inspected = server.tsm(&["inspect", &id, "logs"], None);
    assert!(inspected.contains("Window 1: logs"));
    assert!(!inspected.contains("Window 0: editor"));

    for args in [
        vec!["save", "session", "missing"],
        vec!["save", "window", "alpha:missing"],
        vec!["save", "window", "alpha:"],
        vec!["save", "all", "alpha"],
        vec!["save", "bogus", "extra"],
        vec!["peek", "session", "missing"],
        vec!["inspect", &id, "missing"],
    ] {
        let failed = server.tsm_output(&args, None);
        assert!(!failed.status.success(), "{args:?}");
    }
    // Ambiguous session prefix lists the matches.
    server.tmux(&["new-session", "-d", "-s", "alpine", "/bin/sleep", "120"]);
    let failed = server.tsm_output(&["save", "session", "alp"], None);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("ambiguous"), "{stderr}");
    assert!(stderr.contains("alpha"), "{stderr}");
    assert!(stderr.contains("alpine"), "{stderr}");
}

#[test]
#[ignore = "starts a dedicated tmux server; needs process inspection permissions"]
fn all_scopes_commands_layouts_queries_and_rollback() {
    let server = Server::new();
    let first = server.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "alpha",
        "-n",
        "editor",
        "-x",
        "180",
        "-y",
        "60",
        "-c",
        "/tmp",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&["move-window", "-s", "alpha:0", "-t", "alpha:3"]);
    let argument_file = server.dir.path().join("a b;$(no).txt");
    fs::write(&argument_file, "test\n").unwrap();
    let second = server.tmux(&[
        "split-window",
        "-d",
        "-h",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &first,
        "/usr/bin/tail",
        "-f",
        argument_file.to_str().unwrap(),
    ]);
    for _ in 0..6 {
        server.tmux(&["split-window", "-d", "-t", &first, "/bin/sleep", "120"]);
        server.tmux(&["select-layout", "-t", &first, "tiled"]);
    }
    let panes = server.tmux(&["list-panes", "-t", "alpha:3", "-F", "#{pane_id}"]);
    for (index, pane) in panes.lines().enumerate() {
        server.tmux(&["select-pane", "-t", pane, "-T", &format!("pane {index}")]);
    }
    server.tmux(&["select-layout", "-t", &first, "main-vertical"]);
    server.tmux(&["select-pane", "-t", &second]);
    let extra = server.tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "alpha:7",
        "-n",
        "logs",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&["select-pane", "-t", &extra, "-T", "logs pane"]);
    server.tmux(&["select-window", "-t", "alpha:3"]);
    thread::sleep(Duration::from_millis(100));

    let saved = server.tsm(
        &["save", "session", "-n", "Personal projects"],
        Some(&first),
    );
    let id = saved_id(&saved);
    let snapshot = server.snapshot(id);
    assert_eq!(
        snapshot["sessions"][0]["windows"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        snapshot["sessions"][0]["windows"][0]["panes"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let argv = snapshot["sessions"][0]["windows"][0]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["command"] == "tail")
        .unwrap()["argv"]
        .as_array()
        .unwrap();
    assert_eq!(argv.last().unwrap(), argument_file.to_str().unwrap());
    assert!(
        server
            .tsm(&["list", "-f", "pers"], None)
            .contains("Personal projects")
    );
    assert!(
        server
            .tsm(&["inspect", id], None)
            .contains("Window 7: logs")
    );
    server.tsm(&["restore", "Personal projects"], None);
    assert_eq!(
        server.tmux(&[
            "display-message",
            "-p",
            "-t",
            "=Personal projects:",
            "#{session_name}"
        ]),
        "Personal projects"
    );
    assert_eq!(
        geometry(&server, "alpha:3"),
        geometry(&server, "Personal projects:3")
    );
    assert_eq!(
        geometry(&server, "alpha:7"),
        geometry(&server, "Personal projects:7")
    );
    assert_eq!(
        server.tmux(&[
            "display-message",
            "-p",
            "-t",
            "Personal projects:",
            "#{window_index}"
        ]),
        "3"
    );
    let restored_pane = server.tmux(&[
        "display-message",
        "-p",
        "-t",
        "Personal projects:3",
        "#{pane_id}",
    ]);
    let restored = server.tsm(&["save", "session", "-n", "Restored"], Some(&restored_pane));
    let restored_snapshot = server.snapshot(saved_id(&restored));
    let original_commands: Vec<_> = snapshot["sessions"][0]["windows"][0]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| &pane["argv"])
        .collect();
    let restored_commands: Vec<_> = restored_snapshot["sessions"][0]["windows"][0]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| &pane["argv"])
        .collect();
    assert_eq!(original_commands, restored_commands);

    let window = server.tsm(&["save", "-n", "Marginalia"], Some(&extra));
    assert_eq!(server.snapshot(saved_id(&window))["scope"], "window");
    server.tsm(&["restore", saved_id(&window)], Some(&first));
    assert_eq!(
        server
            .tmux(&["list-windows", "-t", "alpha", "-F", "#{window_id}"])
            .lines()
            .count(),
        3
    );
    server.tsm(&["restore"], None);
    assert_eq!(
        server.tmux(&[
            "display-message",
            "-p",
            "-t",
            "=Marginalia:",
            "#{session_name}"
        ]),
        "Marginalia"
    );

    let all = server.tsm(&["save", "all", "-n", "Everything"], None);
    assert_eq!(
        server.snapshot(saved_id(&all))["sessions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    server.tsm(&["restore", saved_id(&all)], None);
    assert_eq!(
        server
            .tmux(&["list-sessions", "-F", "#{session_id}"])
            .lines()
            .count(),
        6
    );

    let before = server.tmux(&["list-sessions", "-F", "#{session_id}"]);
    let mut broken = snapshot.clone();
    let marker = server.dir.path().join("started-before-layout");
    broken["sessions"][0]["windows"][0]["panes"][0]["argv"] =
        serde_json::json!(["/usr/bin/touch", marker]);
    broken["sessions"][0]["windows"][1]["layout"] = "ffff,80x24,0,0,0".into();
    rewrite_snapshot(&server, &broken);
    let failed = server.tsm_output(&["restore", id], None);
    assert!(!failed.status.success());
    assert!(!marker.exists());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("newly created tmux objects removed"));
    assert_eq!(
        server.tmux(&["list-sessions", "-F", "#{session_id}"]),
        before
    );

    fs::write(server.dir.path().join("store/broken.json"), "{invalid").unwrap();
    let listed = server.tsm_output(&["list"], None);
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stderr).contains("broken.json"));
    assert!(!server.tsm_output(&["save"], None).status.success());
    assert!(!server.tsm_output(&["inspect", ""], None).status.success());
    rewrite_snapshot(&server, &snapshot);

    // Real terminal interaction: filter, scroll, restore, save and exit.
    let tui = server.tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "alpha",
        "-n",
        "tsm-ui",
        "/bin/sleep",
        "120",
    ]);
    server.tmux(&["resize-window", "-t", &tui, "-x", "120", "-y", "24"]);
    server.tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        &tui,
        "env",
        &format!("TSM_TMUX_SOCKET={}", server.socket().display()),
        &format!("TSM_DATA_DIR={}", server.dir.path().join("store").display()),
        env!("CARGO_BIN_EXE_tsm"),
    ]);
    wait_screen(&server, &tui, "Saved snapshots");
    server.tmux(&["send-keys", "-t", &tui, "/Pprj", "Enter"]);
    wait_screen(&server, &tui, "› Personal projects");
    let inspector = wait_screen(&server, &tui, "/usr/bin/tail -f");
    assert!(inspector.contains("Window 7: logs"), "{inspector}");
    assert!(!inspector.contains("logs pane"), "{inspector}");
    server.tmux(&["send-keys", "-t", &tui, "Tab", "Down", "Down", "Right"]);
    wait_screen(&server, &tui, "logs pane");
    let sessions_before = server.tmux(&["list-sessions", "-F", "#{session_id}"]);
    server.tmux(&["send-keys", "-t", &tui, "r"]);
    wait_screen(&server, &tui, "Restored:");
    assert_ne!(
        server.tmux(&["list-sessions", "-F", "#{session_id}"]),
        sessions_before
    );
    server.tmux(&["send-keys", "-t", &tui, "w", "TUI snapshot", "Enter"]);
    let screen = wait_screen(&server, &tui, "Saved: ");
    println!("TUI verified:\n{screen}");
    assert!(
        server
            .tsm(&["inspect", "TUI snapshot"], None)
            .contains("tsm-ui")
    );
    server.tmux(&["send-keys", "-t", &tui, "/"]);
    server.tmux(&["send-keys", "-t", &tui, "C-u", "TUI snapshot", "Enter"]);
    let screen = wait_screen(&server, &tui, "Saved snapshots (1/");
    assert!(!screen.contains("Layout:"));
    server.tmux(&["send-keys", "-t", &tui, "/", "C-u", "missing filter"]);
    wait_screen(&server, &tui, "No matches.");
    server.tmux(&["send-keys", "-t", &tui, "Escape"]);
    wait_screen(&server, &tui, "Saved snapshots (1/");
    server.tmux(&["resize-window", "-t", &tui, "-x", "44", "-y", "12"]);
    let compact = wait_screen(&server, &tui, "Tab inspect");
    assert!(!compact.contains("Inspector"));
    server.tmux(&["send-keys", "-t", &tui, "Enter"]);
    let compact = wait_screen(&server, &tui, "Interactive shell");
    assert!(compact.contains("Inspector"));
    assert!(compact.contains("Session alpha"));
    assert!(compact.contains("tsm-ui"));
    assert_eq!(compact.matches("cwd:").count(), 1, "{compact}");
    println!("Compact TUI verified:\n{compact}");
    server.tmux(&["send-keys", "-t", &tui, "i"]);
    let details = wait_screen(&server, &tui, "Inspector · details");
    assert_eq!(details.matches("cwd:").count(), 1, "{details}");
    server.tmux(&["send-keys", "-t", &tui, "i"]);
    server.tmux(&["send-keys", "-t", &tui, "?"]);
    wait_screen(&server, &tui, "Keyboard shortcuts");
    server.tmux(&["send-keys", "-t", &tui, "End"]);
    wait_screen(&server, &tui, "broken.json");
    server.tmux(&["send-keys", "-t", &tui, "Escape"]);
    wait_screen(&server, &tui, "Inspector");
    server.tmux(&["resize-window", "-t", &tui, "-x", "120", "-y", "24"]);
    server.tmux(&["send-keys", "-t", &tui, "Down", "Right", "i"]);
    wait_screen(&server, &tui, "Layout:");
    server.tmux(&["send-keys", "-t", &tui, "i", "d"]);
    wait_screen(&server, &tui, "y confirm");
    server.tmux(&["send-keys", "-t", &tui, "Escape"]);
    wait_screen(&server, &tui, "i details");
    assert!(
        server
            .tsm_output(&["inspect", "TUI snapshot"], None)
            .status
            .success()
    );
    server.tmux(&["send-keys", "-t", &tui, "d", "y"]);
    wait_screen(&server, &tui, "Deleted: TUI snapshot");
    assert!(
        !server
            .tsm_output(&["inspect", "TUI snapshot"], None)
            .status
            .success()
    );
    assert!(
        server
            .tsm_output(&["inspect", "Personal projects"], None)
            .status
            .success()
    );
    server.tmux(&["send-keys", "-t", &tui, "q"]);

    // Merged windows remain restorable even when original indexes collide.
    let merged = server.tsm(&["merge", id, saved_id(&window), "-n", "Merged"], None);
    let restored = server.tsm(&["restore", saved_id(&merged)], None);
    let session = restored.split_once(": ").unwrap().1;
    assert_eq!(
        server
            .tmux(&["list-windows", "-t", session, "-F", "#{window_id}"])
            .lines()
            .count(),
        3
    );
    let merged_snapshot = server.snapshot(saved_id(&merged));
    for window in merged_snapshot["sessions"][0]["windows"]
        .as_array()
        .unwrap()
    {
        let index = window["index"].as_u64().unwrap();
        let expected = if window["name"] == "editor" {
            "alpha:3"
        } else {
            "alpha:7"
        };
        assert_eq!(
            geometry(&server, &format!("{session}:{index}")),
            geometry(&server, expected)
        );
    }
}

#[test]
#[ignore = "starts dedicated tmux servers; needs process inspection permissions"]
fn tui_restore_and_jump_opens_the_restored_target() {
    let server = Server::new();
    let terminal = Server::new();
    server.tmux(&["new-session", "-d", "-s", "source"]);
    server.tmux(&["new-window", "-t", "source:", "-n", "work"]);
    server.tmux(&["new-session", "-d", "-s", "z-other"]);
    server.tsm(
        &["save", "window", "source:work", "-n", "WindowSaved"],
        None,
    );
    server.tsm(&["save", "session", "source"], None);
    server.tsm(&["save", "all", "-n", "Everything"], None);
    server.tmux(&[
        "set-option",
        "-g",
        "status-left",
        "#{session_name} #{window_name}",
    ]);
    server.tmux(&["set-option", "-g", "status-left-length", "80"]);

    let socket = server.socket();
    let display = terminal.tmux(&[
        "new-session",
        "-dP",
        "-F",
        "#{pane_id}",
        "-x",
        "100",
        "-s",
        "terminal",
        "env",
        "-u",
        "TMUX",
        "-u",
        "TMUX_PANE",
        "tmux",
        "-S",
        socket.to_str().unwrap(),
        "attach-session",
        "-t",
        "source",
    ]);
    wait_screen(&terminal, &display, "source work");
    let client = server.tmux(&["list-clients", "-t", "source", "-F", "#{client_name}"]);
    let socket_env = format!("TSM_TMUX_SOCKET={}", socket.display());
    let store_env = format!("TSM_DATA_DIR={}", server.dir.path().join("store").display());
    for (snapshot, expected_session) in [("WindowSaved", "source"), ("source", "source-restored-1")]
    {
        server.tmux(&["switch-client", "-c", &client, "-t", "source"]);
        let tui = server.tmux(&[
            "new-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "source:",
            "-n",
            "manager",
            "env",
            &socket_env,
            &store_env,
            env!("CARGO_BIN_EXE_tsm"),
        ]);
        wait_screen(&server, &tui, "Saved snapshots");
        server.tmux(&["send-keys", "-t", &tui, "/", snapshot, "Enter"]);
        wait_screen(&server, &tui, &format!("› {snapshot}"));
        let background_window = if snapshot == "WindowSaved" {
            server.tmux(&["send-keys", "-t", &tui, "r"]);
            wait_screen(&server, &tui, "Restored:");
            assert_eq!(
                server.tmux(&["display-message", "-p", "-t", "source:", "#{pane_id}"]),
                tui
            );
            server.tmux(&["display-message", "-p", "-t", "source:$", "#{window_id}"])
        } else {
            String::new()
        };
        server.tmux(&["send-keys", "-t", &tui, "R"]);
        wait_screen(&terminal, &display, &format!("{expected_session} work"));
        assert_eq!(
            server.tmux(&["display-message", "-p", "-c", &client, "#{client_session}"]),
            expected_session
        );
        assert_ne!(
            server.tmux(&[
                "display-message",
                "-p",
                "-t",
                &format!("{expected_session}:"),
                "#{window_id}",
            ]),
            background_window
        );
    }
    server.tmux(&["switch-client", "-c", &client, "-t", "source"]);
    let mut popup = server
        .tmux_command()
        .args([
            "display-popup",
            "-c",
            &client,
            "-t",
            "source:",
            "-E",
            "-w",
            "90%",
            "-h",
            "90%",
            "env",
            &socket_env,
            &store_env,
            env!("CARGO_BIN_EXE_tsm"),
        ])
        .spawn()
        .unwrap();
    wait_screen(&terminal, &display, "Saved snapshots");
    terminal.tmux(&["send-keys", "-t", &display, "/Everything", "Enter"]);
    wait_screen(&terminal, &display, "› Everything");
    terminal.tmux(&["send-keys", "-t", &display, "R"]);
    wait_screen(&terminal, &display, "source-restored-2 work");
    assert!(popup.wait().unwrap().success());
    server.tmux(&["has-session", "-t", "=z-other-restored-1"]);

    let outside = terminal.tmux(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "terminal:",
        "env",
        "-u",
        "TMUX",
        "-u",
        "TMUX_PANE",
        &socket_env,
        &store_env,
        env!("CARGO_BIN_EXE_tsm"),
    ]);
    wait_screen(&terminal, &outside, "Saved snapshots");
    terminal.tmux(&["send-keys", "-t", &outside, "/WindowSaved", "Enter"]);
    wait_screen(&terminal, &outside, "› WindowSaved");
    terminal.tmux(&["send-keys", "-t", &outside, "R"]);
    wait_screen(&terminal, &outside, "WindowSaved work");
    assert!(
        !server
            .tmux(&["list-clients", "-t", "WindowSaved", "-F", "#{client_name}"])
            .is_empty()
    );
}

fn wait_screen(server: &Server, pane: &str, expected: &str) -> String {
    let mut screen = String::new();
    for _ in 0..80 {
        screen = server.tmux(&["capture-pane", "-p", "-t", pane]);
        if screen.contains(expected) {
            return screen;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("terminal did not show {expected:?}:\n{screen}");
}
