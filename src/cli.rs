use crate::{
    color::{ColorMode, Colors},
    merge,
    model::{Scope, Snapshot, WindowSnapshot},
    query::{search, short_id},
    store::Store,
    tmux, tui,
};
use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "tsm",
    version,
    about = "Save, inspect, and restore tmux sessions"
)]
pub struct Cli {
    /// Color output: auto respects NO_COLOR and disables colors in pipes
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: ColorMode,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a window (default), a session, or all sessions
    Save {
        #[command(flatten)]
        capture: CaptureArgs,
        /// Show what would be saved without writing a snapshot (same as peek)
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect live window (default), session, or all sessions without saving
    Peek(CaptureArgs),
    /// List snapshots with fuzzy name search or UUID prefix filtering
    List {
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// Inspect by exact name, UUID, or unique UUID prefix
    Inspect {
        snapshot: String,
        /// Show only this window: index, name, or unique name prefix
        window: Option<String>,
    },
    /// Restore latest snapshot (default), or select by name/UUID
    Restore { snapshot: Option<String> },
    /// Delete a saved snapshot (does not affect live tmux sessions)
    #[command(visible_alias = "rm")]
    Delete { snapshot: String },
    /// Merge saved snapshots, keeping sources; combine equally named sessions
    Merge {
        #[arg(required = true)]
        snapshots: Vec<String>,
        /// Update this snapshot instead of creating a new one
        #[arg(long)]
        into: Option<String>,
        /// Override the merged snapshot name
        #[arg(short, long)]
        name: Option<String>,
    },
}

#[derive(Args)]
struct CaptureArgs {
    /// Scope: window (default), session, or all. Any other value is treated
    /// as a window target: `save 3`, `save logs`, `save work:2`
    #[arg(value_name = "SCOPE")]
    scope: Option<String>,
    /// Window (index, name, or session:window) or session (name or unique
    /// prefix) to capture instead of the current one; not valid with scope 'all'
    #[arg(value_name = "TARGET")]
    target: Option<String>,
    /// Snapshot name; preview only when using peek or --dry-run
    #[arg(short, long)]
    name: Option<String>,
}

/// A non-scope first positional becomes the window target, so `save 3`
/// and `save logs` work without spelling out the `window` scope.
fn resolve_capture(
    scope: Option<String>,
    target: Option<String>,
) -> Result<(Scope, Option<String>)> {
    match scope.as_deref() {
        None => Ok((Scope::Window, target)),
        Some(value) => match SaveScope::from_str(value, true) {
            Ok(scope) => Ok((scope.into(), target)),
            Err(_) => {
                anyhow::ensure!(
                    target.is_none(),
                    "invalid scope '{value}'; expected window, session, or all"
                );
                Ok((Scope::Window, Some(value.to_owned())))
            }
        },
    }
}

#[derive(Clone, ValueEnum)]
enum SaveScope {
    Window,
    Session,
    All,
}
impl From<SaveScope> for Scope {
    fn from(scope: SaveScope) -> Self {
        match scope {
            SaveScope::Window => Self::Window,
            SaveScope::Session => Self::Session,
            SaveScope::All => Self::All,
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let colors = cli.color.stdout();
    let errors = cli.color.stderr();
    match cli.command {
        None => tui::run(&Store::discover()?),
        Some(Commands::Peek(capture))
        | Some(Commands::Save {
            capture,
            dry_run: true,
        }) => {
            let (scope, target) = resolve_capture(capture.scope, capture.target)?;
            let snapshot = tmux::capture(scope, target.as_deref(), capture.name)?;
            println!(
                "{}\n{}",
                colors.paint("33", "Preview only — nothing saved. UUID is temporary."),
                describe(&snapshot, None, colors)?
            );
            Ok(())
        }
        Some(Commands::Save {
            capture,
            dry_run: false,
        }) => {
            let (scope, target) = resolve_capture(capture.scope, capture.target)?;
            let snapshot = tmux::capture(scope, target.as_deref(), capture.name)?;
            let store = Store::discover()?;
            store.save(&snapshot)?;
            let catalog = store.load()?;
            print_saved(&snapshot, &catalog.snapshots, colors);
            Ok(())
        }
        Some(command) => {
            let store = Store::discover()?;
            let catalog = store.load()?;
            for warning in &catalog.warnings {
                eprintln!("{} {warning}", errors.paint("33", "warning:"));
            }
            match command {
                Commands::List { filter } => {
                    println!(
                        "{}",
                        colors.paint(
                            "1",
                            "ID        TYPE     CREATED (UTC)        WINDOWS PANES NAME"
                        )
                    );
                    for index in search(&catalog.snapshots, filter.as_deref().unwrap_or("")) {
                        let snapshot = &catalog.snapshots[index];
                        println!(
                            "{}  {}  {}  {:>7} {:>5} {}",
                            colors.paint(
                                "36",
                                format!("{:<8}", short_id(snapshot, &catalog.snapshots))
                            ),
                            colors.paint("35", format!("{:<7}", snapshot.scope.as_str())),
                            colors.paint("2", snapshot.created_at.format("%Y-%m-%d %H:%M:%S")),
                            snapshot.window_count(),
                            snapshot.pane_count(),
                            colors.paint("1", &snapshot.name)
                        );
                    }
                }
                Commands::Inspect { snapshot, window } => {
                    let snapshot = catalog.resolve(&snapshot)?;
                    println!("{}", describe(snapshot, window.as_deref(), colors)?);
                }
                Commands::Restore { snapshot } => {
                    let snapshot = match snapshot {
                        Some(query) => catalog.resolve(&query)?,
                        None => catalog.latest()?,
                    };
                    println!(
                        "{} {}: {}",
                        colors.paint("32", "restored"),
                        colors.paint("36", short_id(snapshot, &catalog.snapshots)),
                        tmux::restore(snapshot)?.join(", ")
                    );
                }
                Commands::Delete { snapshot } => {
                    let snapshot = catalog.resolve(&snapshot)?;
                    store.delete(snapshot)?;
                    println!(
                        "{} {}  {}",
                        colors.paint("32", "deleted"),
                        colors.paint("36", short_id(snapshot, &catalog.snapshots)),
                        snapshot.name
                    );
                }
                Commands::Merge {
                    snapshots,
                    into,
                    name,
                } => {
                    let sources = snapshots
                        .iter()
                        .map(|query| catalog.resolve(query))
                        .collect::<Result<Vec<_>>>()?;
                    let target = into
                        .as_deref()
                        .map(|query| catalog.resolve(query))
                        .transpose()?;
                    let snapshot = merge::merge(&sources, target, name)?;
                    if target.is_some() {
                        store.replace(&snapshot)?;
                    } else {
                        store.save(&snapshot)?;
                    }
                    let catalog = store.load()?;
                    print_saved(&snapshot, &catalog.snapshots, colors);
                }
                Commands::Save { .. } | Commands::Peek(_) => unreachable!(),
            }
            Ok(())
        }
    }
}

fn print_saved(snapshot: &Snapshot, all: &[Snapshot], colors: Colors) {
    println!(
        "{}  {}  {}",
        colors.paint("36", short_id(snapshot, all)),
        colors.paint("35", snapshot.scope.as_str()),
        colors.paint("1", &snapshot.name)
    );
}

fn window_matches(selector: &str, window: &WindowSnapshot) -> bool {
    selector.parse::<usize>().ok() == Some(window.index)
        || window.name == selector
        || window
            .name
            .to_lowercase()
            .starts_with(&selector.to_lowercase())
}

fn describe(snapshot: &Snapshot, window: Option<&str>, colors: Colors) -> Result<String> {
    let mut lines = vec![
        format!(
            "{}  {}",
            colors.paint("36", snapshot.id),
            colors.paint("1", &snapshot.name)
        ),
        colors.paint(
            "2",
            format!(
                "{} · {}",
                snapshot.scope.as_str(),
                snapshot.created_at.to_rfc3339()
            ),
        ),
    ];
    let mut matched = false;
    for session in &snapshot.sessions {
        let windows: Vec<_> = session
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| window.is_none_or(|selector| window_matches(selector, w)))
            .collect();
        if windows.is_empty() {
            continue;
        }
        matched = true;
        lines.push(colors.paint("1;36", format!("Session: {}", session.name)));
        for (index, window) in windows {
            lines.push(colors.paint(
                "1",
                format!(
                    "  {}Window {}: {}{}",
                    if index == session.active_window {
                        "* "
                    } else {
                        "  "
                    },
                    window.index,
                    window.name,
                    if window.zoomed { " (zoomed)" } else { "" }
                ),
            ));
            lines.push(colors.paint("2", format!("    Layout: {}", window.layout)));
            for (index, pane) in window.panes.iter().enumerate() {
                lines.push(colors.paint(
                    "35",
                    format!(
                        "    {}Pane {}: {}",
                        if index == window.active_pane {
                            "* "
                        } else {
                            "  "
                        },
                        index,
                        pane.title
                    ),
                ));
                lines.push(format!("      {} {}", colors.paint("36", "cwd:"), pane.cwd));
                lines.push(format!(
                    "      {} {}",
                    colors.paint("32", "command:"),
                    if pane.command_line.is_empty() {
                        &pane.command
                    } else {
                        &pane.command_line
                    }
                ));
                if !pane.start_command.is_empty() {
                    lines.push(
                        colors.paint("2", format!("      started as: {}", pane.start_command)),
                    );
                }
            }
        }
    }
    anyhow::ensure!(
        window.is_none() || matched,
        "no window '{}' in snapshot '{}'",
        window.unwrap_or_default(),
        snapshot.name
    );
    Ok(lines.join("\n"))
}
