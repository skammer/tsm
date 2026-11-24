# tsm

[Русский](README.ru.md)

A tmux snapshot manager written in Rust. macOS / Linux, tmux 3.3+.

## Getting started

```sh
cargo build --release
./target/release/tsm
```

Install into `~/.cargo/bin`:

```sh
cargo install --path . --locked
```

## CLI

```sh
tsm save
tsm save window -n "Marginalia"
tsm save session -n "Personal projects"
tsm save session work            # session by name or unique prefix
tsm save window 2                # window by index
tsm save 3                       # same: a non-scope value is a window target
tsm save window logs             # window by name or unique prefix
tsm save window work:2           # window in another session
tsm save all -n "Everything"
tsm peek
tsm peek session -n "Personal projects"
tsm peek work:logs
tsm peek all
tsm save window -n "Marginalia" --dry-run
tsm list
tsm list -f "Pers"
tsm inspect "Personal projects"
tsm inspect "Personal projects" logs   # only one window of a snapshot
tsm restore "Marginalia"
tsm restore
tsm delete "Marginalia"
tsm rm "Personal projects"
tsm merge id-1 id-2 -n "Merged sessions"
tsm merge id-3 --into id-4 -n "Updated id-4"
tsm list --color always
```

`save` and `peek` capture the current window or session by default, or an explicit target: a window by index, name, or unique name prefix (optionally `session:window`), a session by name or unique name prefix. A first positional that is not a scope (`window`/`session`/`all`) is treated as a window target, so `tsm save 3` works. Named targets (`session:window`, a session name) only need an accessible tmux server, not `TMUX_PANE`. A target is not valid with scope `all`.

`peek [window|session|all] [TARGET] [-n NAME]` displays the current state in the same format as `inspect`, using the same capture logic as `save`. `save --dry-run` does the same. Neither command reads or creates the snapshot store, and no files are written. The displayed UUID is temporary; a subsequent `save` generates a new one. Window/session capture of the current window or session must run inside tmux; named targets and `all` only require an accessible tmux server.

`inspect SNAPSHOT [SESSION:]WINDOW` shows a single window. Select by index, exact name, or unique case-insensitive name prefix. Qualify the window with a session when the selector matches more than one session.

`inspect`, `restore`, `delete`, and `merge` accept an exact name, a full UUID, or any unique UUID prefix, with or without hyphens. A matching full UUID takes priority over a snapshot name. `list` displays at least 8 characters and extends the ID when a prefix matches another ID or name. Ambiguous selectors report the matching UUIDs. Each save creates a new snapshot, and duplicate names are allowed.

Colors are enabled automatically in a terminal and disabled when output is redirected, `NO_COLOR` is nonempty, or `TERM=dumb`. The global `--color auto|always|never` option controls command output colors.

`delete` (alias `rm`) removes the selected snapshot without further confirmation. Running tmux sessions are unaffected.

Without `--into`, `merge` requires at least two snapshots and creates a new UUID. Without `-n`, their names are joined with `", "`. With `--into`, one source snapshot is enough: the target is updated atomically, retaining its UUID, creation date, and name; `-n` overrides its name. Source snapshots remain in the store. Supplying the same UUID more than once, including the target among the sources, produces an error.

Tmux sessions with the same name are combined by appending their windows; differently named sessions remain separate. All windows and panes are retained, even when their contents are identical. If a window index conflicts, the incoming window receives the lowest available index; other indexes are preserved. Pane layouts and commands are unchanged. The active window comes from the first snapshot containing that session (the `--into` target comes first). The result has scope `session` if it contains one session, otherwise `all`.

Search matches case-insensitive names, including Cyrillic, or a UUID prefix. `pprj` finds `Personal projects` by matching letters in order. Exact names, UUID prefixes, name prefixes, and substrings rank ahead of fuzzy matches.

`save` / `save window` and `save session` without a target use the pane from the calling tmux server. `save all` captures all sessions on the selected server. If `TSM_TMUX_SOCKET` points to a different server, use an explicit capture target. To restore a window to that server as a new session, run outside tmux.

`restore` without an argument restores the most recent snapshot:

- Window snapshot inside tmux → a new window in the current session.
- Window snapshot outside tmux → a new session named after the snapshot.
- Session snapshot → a new session named after the snapshot.
- All snapshot → new sessions with their original tmux session names.
- Name conflicts are resolved by appending `-restored-N`.
- Existing windows and sessions are not replaced. CLI restore and TUI `r` keep the current focus.
- If creation fails, only objects created by that restore are removed. Side effects of programs already started cannot be undone.
- If starting a saved executable fails, restore keeps the new layout and an interactive shell in that pane. The CLI lists the affected panes and exits with an error. Slow shell startup can time out while the command is still pending. Programs that fail after launch report their errors inside their panes.

## TUI

Run `tsm` without arguments:

The inspector shows each window, the command that will run in each pane, and its working directory. Shared directories appear once at the snapshot, session, or window level. Expand a window to see its pane titles.

| Key | Action |
| --- | --- |
| ↑/↓, j/k | Select a snapshot or an inspector section |
| ←/→ | Collapse or expand the focused session or window |
| Tab / Enter | Switch focus between the list and inspector |
| PgUp / PgDn | Page through the inspector or the list in a narrow terminal |
| Home / End | Jump to the start or end of the focused panel |
| / | Search snapshots. ↑/↓ select, Enter applies, Esc cancels, Ctrl-U clears |
| w / s / a | Save window / session / all; enter a name or press Enter for the default |
| r | Restore the selected snapshot immediately |
| R | Restore and jump to the restored window or session |
| e | Open scrollable error details when available |
| d / Delete | Delete the selected snapshot: y confirms, n/Esc cancels |
| i | Toggle technical details, including saved time, UUID, layout, and original command |
| g | Reload snapshots from disk |
| ? | Show keyboard shortcuts and storage warnings |
| q / Ctrl-C | Quit |

`R` closes the TUI after restoring. For an all-sessions snapshot, it opens the first restored session at its saved active window. From outside tmux, it attaches the current terminal to the restored session.

## What is saved

Session and window names, window indexes, layouts, active windows and panes, zoom state, pane titles, working directories, running programs, and arguments. Arguments are read separately from the process environment using `KERN_PROCARGS2` on macOS and `/proc/PID/cmdline` on Linux. Shell quoting preserves arguments containing spaces and special characters.

The current `tsm` invocation is excluded from snapshot commands using its PID and process tree, including when launched through a shell wrapper or a renamed binary. That pane retains its layout, title, and working directory, but all command fields (`command`, `command_line`, `argv`, `start_command`) are cleared. Restoring it opens a regular shell. The same rule applies to `peek`, `save --dry-run`, and saving from the TUI.

Restoring **restarts** programs. Process memory, terminal scrollback, unsaved editor buffers, shell history, environment variables, pipelines, and background jobs are not saved. For an arbitrary pipeline, one foreground process group leader is identified; the complete original shell command is not guaranteed. Interactive shells and `tsm` itself are replaced with a new shell from `default-shell`; shell scripts with arguments are restarted.

A missing working directory causes an error before any objects are created. Restore starts a shell in the saved directory and runs the saved program as a foreground job. After the program exits, the same pane returns to an interactive prompt. Interactive shell startup files may run again when the prompt opens. Programs may subsequently change pane titles themselves. Geometry may be scaled to fit a different terminal size. Windows linked across sessions are restored as separate copies. Tmux options other than those needed for restoration are not archived.

## Storage

One JSON file per UUID, written atomically with `0600` permissions. `merge --into` atomically replaces its target. Concurrent merges and deletes through `tsm` are serialized in the same store.

- macOS: `~/Library/Application Support/dev.tsm.tsm/snapshots/`
- Linux: `$XDG_DATA_HOME/tsm/snapshots/`, usually `~/.local/share/tsm/snapshots/`.
- `TSM_DATA_DIR` overrides the snapshot directory directly.
- `TSM_TMUX_SOCKET` specifies a tmux socket path for a separate server.

Corrupt and unsupported snapshots are skipped with a warning. Commands may contain secrets in their arguments; the JSON stores those arguments, but not the full environment.

## Verification

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test --test tmux_roundtrip -- --ignored
```

The regular tests also cover CLI colors, deletion, merging, and preservation of files when operations fail. The final command runs tests on isolated temporary tmux servers: layouts, arguments, all scopes, queries, rollback, restoring merged snapshots, and TUI interaction. It requires permission to launch tmux and inspect processes. Your existing tmux server is not used.

## Structure

`model` validates snapshots, `store` handles files, and `query` resolves names and IDs. Under `tmux`, `capture` reads live state, `restore` rebuilds layouts, and `launch` starts shells and foreground jobs. The TUI separates input and state (`app`), layout (`view`), and snapshot details (`inspect`).

Ratatui is pinned to 0.30.2: accurate scrolling uses the `unstable-rendered-line-info` feature. Pane creation behavior was checked against the [tmux source](https://github.com/tmux/tmux/blob/master/cmd-split-window.c).
