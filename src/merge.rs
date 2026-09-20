use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use uuid::Uuid;

use crate::model::{Scope, Snapshot};

/// Combine windows of equally named sessions, preserving every pane and layout.
pub fn merge(
    sources: &[&Snapshot],
    target: Option<&Snapshot>,
    name: Option<String>,
) -> Result<Snapshot> {
    ensure!(!sources.is_empty(), "provide at least one source snapshot");
    ensure!(
        target.is_some() || sources.len() >= 2,
        "merge needs at least two snapshots, or --into"
    );
    let inputs: Vec<_> = target.into_iter().chain(sources.iter().copied()).collect();
    let mut ids = HashSet::new();
    for snapshot in &inputs {
        snapshot.validate()?;
        ensure!(
            ids.insert(snapshot.id),
            "snapshot supplied more than once: {}",
            snapshot.id
        );
    }
    let mut merged = Snapshot {
        version: 1,
        id: target.map_or_else(Uuid::new_v4, |snapshot| snapshot.id),
        name: name.unwrap_or_else(|| {
            target.map_or_else(
                || {
                    sources
                        .iter()
                        .map(|snapshot| snapshot.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                |snapshot| snapshot.name.clone(),
            )
        }),
        created_at: target.map_or_else(Utc::now, |snapshot| snapshot.created_at),
        scope: Scope::Session,
        sessions: vec![],
    };
    for snapshot in inputs {
        for incoming in &snapshot.sessions {
            if let Some(session) = merged
                .sessions
                .iter_mut()
                .find(|session| session.name == incoming.name)
            {
                // Reserve incoming indexes too, so only actual conflicts move.
                let mut used: HashSet<_> = session
                    .windows
                    .iter()
                    .chain(&incoming.windows)
                    .map(|window| window.index)
                    .collect();
                let existing: HashSet<_> =
                    session.windows.iter().map(|window| window.index).collect();
                for window in &incoming.windows {
                    let mut window = window.clone();
                    if existing.contains(&window.index) {
                        let mut index = 0;
                        while used.contains(&index) {
                            index = index.checked_add(1).context("no free window index")?;
                        }
                        window.index = index;
                        used.insert(index);
                    }
                    session.windows.push(window);
                }
            } else {
                merged.sessions.push(incoming.clone());
            }
        }
    }
    merged.scope = if merged.sessions.len() > 1 {
        Scope::All
    } else {
        Scope::Session
    };
    merged.validate()?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_snapshot;

    #[test]
    fn combines_windows_without_losing_layouts_or_nonconflicting_indexes() {
        let first = test_snapshot();
        let mut second = test_snapshot();
        second.name = "Second".into();
        let mut extra = second.sessions[0].windows[0].clone();
        extra.index = 0;
        second.sessions[0].windows.push(extra);
        let merged = merge(&[&first, &second], None, None).unwrap();
        assert_eq!(merged.name, "Personal projects, Second");
        assert_ne!(merged.id, first.id);
        assert_eq!(merged.scope, Scope::Session);
        assert_eq!(merged.pane_count(), 3);
        let windows = &merged.sessions[0].windows;
        assert_eq!(
            windows.iter().map(|w| w.index).collect::<Vec<_>>(),
            [3, 1, 0]
        );
        assert_eq!(windows[1].layout, second.sessions[0].windows[0].layout);
        assert_eq!(first.window_count(), 1);
    }

    #[test]
    fn into_preserves_identity_date_name_and_focus_unless_name_overridden() {
        let mut target = test_snapshot();
        let mut extra = target.sessions[0].windows[0].clone();
        extra.index = 7;
        target.sessions[0].windows.push(extra);
        target.sessions[0].active_window = 1;
        let source = test_snapshot();
        let merged = merge(&[&source], Some(&target), None).unwrap();
        assert_eq!(merged.id, target.id);
        assert_eq!(merged.name, target.name);
        assert_eq!(merged.created_at, target.created_at);
        assert_eq!(merged.sessions[0].active_window, 1);
        assert_eq!(
            merge(&[&source], Some(&target), Some("Updated".into()))
                .unwrap()
                .name,
            "Updated"
        );
    }

    #[test]
    fn different_sessions_become_all_and_invalid_inputs_fail() {
        let first = test_snapshot();
        let mut second = test_snapshot();
        second.sessions[0].name = "other".into();
        let merged = merge(&[&first, &second], None, Some("Combined".into())).unwrap();
        assert_eq!(merged.scope, Scope::All);
        assert_eq!(merged.sessions.len(), 2);
        assert_eq!(merged.name, "Combined");
        assert!(merge(&[&first], None, None).is_err());
        assert!(merge(&[], Some(&first), None).is_err());
        assert!(merge(&[&first, &first], None, None).is_err());
        assert!(merge(&[&first], Some(&first), None).is_err());
        assert!(merge(&[&first, &second], None, Some("  ".into())).is_err());
    }
}
