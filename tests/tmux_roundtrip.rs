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
    fn tmux(&self, args: &[&str]) -> String {
        output(
            Command::new("tmux")
                .arg("-S")
                .arg(self.socket())
                .args(["-f", "/dev/null"])
                .args(args)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .output()
                .unwrap(),
        )
    }
    fn tsm_output(&self, args: &[&str], pane: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tsm"));
        command
            .args(args)
            .env("TSM_TMUX_SOCKET", self.socket())
            .env("TSM_DATA_DIR", self.dir.path().join("store"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        if let Some(pane) = pane {
            command.env("TMUX_PANE", pane);
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
        let restored = server.tmux(&[
            "list-panes",
            "-t",
            "self-capture-restored-1:",
            "-F",
            "#{pane_id}",
        ]);
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
        geometry(&server, "alpha:3"),
        geometry(&server, "alpha-restored-1:3")
    );
    assert_eq!(
        geometry(&server, "alpha:7"),
        geometry(&server, "alpha-restored-1:7")
    );
    assert_eq!(
        server.tmux(&[
            "display-message",
            "-p",
            "-t",
            "alpha-restored-1:",
            "#{window_index}"
        ]),
        "3"
    );
    let restored_pane = server.tmux(&[
        "display-message",
        "-p",
        "-t",
        "alpha-restored-1:3",
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
    broken["sessions"][0]["windows"][1]["layout"] = "ffff,80x24,0,0,0".into();
    rewrite_snapshot(&server, &broken);
    let failed = server.tsm_output(&["restore", id], None);
    assert!(!failed.status.success());
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
        "env",
        &format!("TSM_TMUX_SOCKET={}", server.socket().display()),
        &format!("TSM_DATA_DIR={}", server.dir.path().join("store").display()),
        env!("CARGO_BIN_EXE_tsm"),
    ]);
    server.tmux(&["resize-window", "-t", &tui, "-x", "120", "-y", "24"]);
    wait_screen(&server, &tui, "Saved snapshots");
    server.tmux(&["send-keys", "-t", &tui, "/Pers", "Enter"]);
    wait_screen(&server, &tui, "Personal projects");
    server.tmux(&[
        "send-keys",
        "-t",
        &tui,
        "NPage",
        "NPage",
        "NPage",
        "NPage",
        "NPage",
    ]);
    wait_screen(&server, &tui, "logs pane");
    server.tmux(&["send-keys", "-t", &tui, "r"]);
    wait_screen(&server, &tui, "Restored:");
    server.tmux(&["send-keys", "-t", &tui, "w", "TUI snapshot", "Enter"]);
    let screen = wait_screen(&server, &tui, "· TUI snapshot");
    println!("TUI verified:\n{screen}");
    assert!(
        server
            .tsm(&["inspect", "TUI snapshot"], None)
            .contains("tsm-ui")
    );
    server.tmux(&["send-keys", "-t", &tui, "/"]);
    // Clear the previous filter through the TUI's supported editing keys.
    server.tmux(&[
        "send-keys",
        "-t",
        &tui,
        "BSpace",
        "BSpace",
        "BSpace",
        "BSpace",
        "TUI snapshot",
        "Enter",
    ]);
    let screen = wait_screen(&server, &tui, "tsm-ui");
    assert!(!screen.contains("Layout:"));
    server.tmux(&["send-keys", "-t", &tui, "i"]);
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
