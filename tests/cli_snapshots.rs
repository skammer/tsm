use std::{
    fs,
    process::{Command, Output},
};

use serde_json::{Value, json};
use tempfile::TempDir;

const FIRST: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const SECOND: &str = "bbbbbbbb-2222-4222-8222-222222222222";

fn fixture(dir: &TempDir, id: &str, name: &str) -> Value {
    let snapshot = json!({
        "version": 1, "id": id, "name": name,
        "created_at": "2026-09-01T00:00:00Z", "scope": "window",
        "sessions": [{"name": "work", "active_window": 0, "windows": [{
            "index": 3, "name": name, "layout": "b25d,80x24,0,0,0",
            "active_pane": 0, "zoomed": false,
            "panes": [{"title": "shell", "cwd": "/tmp", "command": "sh",
                "command_line": "sh", "argv": [], "start_command": "", "active": true}]
        }]}]
    });
    fs::write(
        dir.path().join(format!("{id}.json")),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    snapshot
}

fn run(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tsm"))
        .args(args)
        .env("TSM_DATA_DIR", dir.path())
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("FORCE_COLOR")
        .output()
        .unwrap()
}

fn success(dir: &TempDir, args: &[&str]) -> String {
    let result = run(dir, args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

fn read(dir: &TempDir, id: &str) -> Value {
    serde_json::from_slice(&fs::read(dir.path().join(format!("{id}.json"))).unwrap()).unwrap()
}

#[test]
fn merge_creates_new_snapshot_then_updates_target_without_touching_sources() {
    let dir = tempfile::tempdir().unwrap();
    let first = fixture(&dir, FIRST, "First");
    let second = fixture(&dir, SECOND, "Second");
    let output = success(&dir, &["merge", "aaaaaaaa", "Second"]);
    assert!(output.contains("First, Second"));
    let id = output.split_whitespace().next().unwrap();
    assert!(!["aaaaaaaa", "bbbbbbbb"].contains(&id));
    let merged_file = fs::read_dir(dir.path())
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
    let merged: Value = serde_json::from_slice(&fs::read(merged_file.path()).unwrap()).unwrap();
    assert_eq!(merged["scope"], "session");
    assert_eq!(
        merged["sessions"][0]["windows"].as_array().unwrap().len(),
        2
    );
    assert_eq!(read(&dir, FIRST), first);
    assert_eq!(read(&dir, SECOND), second);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);

    success(&dir, &["merge", "aaaaaaaa", "--into", "bbbbbbbb"]);
    let updated = read(&dir, SECOND);
    assert_eq!(updated["name"], "Second");
    assert_eq!(updated["id"], SECOND);
    assert_eq!(updated["created_at"], second["created_at"]);
    assert_eq!(
        updated["sessions"][0]["windows"].as_array().unwrap().len(),
        2
    );
    success(
        &dir,
        &["merge", "First", "--into", "bbbbbbbb", "-n", "Updated id-4"],
    );
    assert_eq!(read(&dir, SECOND)["name"], "Updated id-4");
    assert_eq!(read(&dir, FIRST), first);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.path().join(format!("{SECOND}.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_merges_and_ambiguous_delete_leave_files_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let first = fixture(&dir, FIRST, "Same");
    let second = fixture(&dir, SECOND, "Same");
    for args in [
        vec!["merge"],
        vec!["merge", "aaaaaaaa"],
        vec!["merge", "aaaaaaaa", "missing"],
        vec!["merge", "aaaaaaaa", "--into", "missing"],
        vec!["merge", "aaaaaaaa", "--into", "aaaaaaaa"],
        vec!["merge", "aaaaaaaa", FIRST],
        vec!["merge", "aaaaaaaa", "bbbbbbbb", "-n", " "],
        vec!["merge", "aaaaaaaa", "--into", "bbbbbbbb", "-n", " "],
        vec!["delete", "Same"],
        vec!["delete", "missing"],
        vec!["delete", ""],
    ] {
        assert!(!run(&dir, &args).status.success(), "{args:?}");
        assert_eq!(read(&dir, FIRST), first);
        assert_eq!(read(&dir, SECOND), second);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    assert!(success(&dir, &["delete", "aaaaaaaa"]).contains("deleted aaaaaaaa"));
    assert!(!dir.path().join(format!("{FIRST}.json")).exists());
    assert_eq!(read(&dir, SECOND), second);
    success(&dir, &["rm", "Same"]);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn inspect_filters_windows_by_index_name_and_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = fixture(&dir, FIRST, "First");
    snapshot["scope"] = json!("session");
    snapshot["sessions"][0]["windows"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "index": 7, "name": "logs", "layout": "b25d,80x24,0,0,0",
            "active_pane": 0, "zoomed": false,
            "panes": [{"title": "tail", "cwd": "/var", "command": "tail",
                "command_line": "tail -f x", "argv": ["tail", "-f", "x"],
                "start_command": "", "active": true}]
        }));
    fs::write(
        dir.path().join(format!("{FIRST}.json")),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    for selector in ["3", "First", "fir"] {
        let shown = success(&dir, &["inspect", "First", selector]);
        assert!(shown.contains("Window 3: First"), "{selector}");
        assert!(!shown.contains("Window 7: logs"), "{selector}");
    }
    for selector in ["7", "logs", "LOG"] {
        let shown = success(&dir, &["inspect", "First", selector]);
        assert!(shown.contains("Window 7: logs"), "{selector}");
        assert!(!shown.contains("Window 3: First"), "{selector}");
    }
    let everything = success(&dir, &["inspect", "First"]);
    assert!(everything.contains("Window 3: First"));
    assert!(everything.contains("Window 7: logs"));
    for selector in ["missing", "9"] {
        let failed = run(&dir, &["inspect", "First", selector]);
        assert!(!failed.status.success(), "{selector}");
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("no window"),
            "{selector}"
        );
    }
}

#[test]
fn colors_are_opt_in_for_pipes_and_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    fixture(&dir, FIRST, "First");
    for command in [vec!["list"], vec!["inspect", "First"]] {
        let plain = success(&dir, &command);
        assert!(!plain.contains('\x1b'));
        let mut args = command.clone();
        args.extend(["--color", "always"]);
        assert!(success(&dir, &args).contains("\x1b["));
        let mut args = command;
        args.extend(["--color", "never"]);
        assert_eq!(success(&dir, &args), plain);
    }
    let failed = run(&dir, &["inspect", "missing", "--color", "always"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("\x1b[1;31mError:"));
}
