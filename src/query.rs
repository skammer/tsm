use crate::model::Snapshot;
use anyhow::{Result, bail};

pub struct SnapshotQuery {
    name: String,
    id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    Exact,
    IdPrefix,
    NamePrefix,
    Substring,
    Fuzzy,
}

pub fn search(snapshots: &[Snapshot], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    if filter.is_empty() {
        return (0..snapshots.len()).collect();
    }
    let query = SnapshotQuery::new(filter);
    let mut matches: Vec<_> = snapshots
        .iter()
        .enumerate()
        .filter_map(|(index, snapshot)| query.rank(snapshot).map(|rank| (rank, index)))
        .collect();
    matches.sort_unstable(); // The catalog index breaks ties by recency.
    matches.into_iter().map(|(_, index)| index).collect()
}

impl SnapshotQuery {
    pub fn new(value: &str) -> Self {
        let normalized = value.replace('-', "").to_ascii_lowercase();
        let id = (!normalized.is_empty()
            && normalized.len() <= 32
            && normalized.bytes().all(|c| c.is_ascii_hexdigit()))
        .then_some(normalized);
        Self {
            name: value.to_lowercase(),
            id,
        }
    }

    fn rank(&self, snapshot: &Snapshot) -> Option<Match> {
        let name = snapshot.name.to_lowercase();
        if name == self.name {
            return Some(Match::Exact);
        }
        if self.matches_id(snapshot) {
            return Some(Match::IdPrefix);
        }
        if name.starts_with(&self.name) {
            return Some(Match::NamePrefix);
        }
        if name.contains(&self.name) {
            return Some(Match::Substring);
        }
        let mut letters = name.chars();
        self.name
            .chars()
            .filter(|letter| !letter.is_whitespace())
            .all(|wanted| letters.by_ref().any(|letter| letter == wanted))
            .then_some(Match::Fuzzy)
    }

    fn matches_id(&self, snapshot: &Snapshot) -> bool {
        self.id
            .as_ref()
            .is_some_and(|id| snapshot.id.simple().to_string().starts_with(id))
    }

    pub fn resolve<'a>(&self, snapshots: &'a [Snapshot]) -> Result<&'a Snapshot> {
        if self.name.is_empty() {
            bail!("snapshot selector is empty");
        }
        let found: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| {
                snapshot.name.to_lowercase() == self.name || self.matches_id(snapshot)
            })
            .collect();
        match found.as_slice() {
            [snapshot] => Ok(snapshot),
            [] => bail!("snapshot not found: {}", self.name),
            _ => bail!(
                "ambiguous snapshot '{}'; use a longer UUID: {}",
                self.name,
                found
                    .iter()
                    .map(|s| s.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

pub fn short_id(snapshot: &Snapshot, all: &[Snapshot]) -> String {
    let full = snapshot.id.simple().to_string();
    for length in 8..32 {
        let prefix = &full[..length];
        if !all.iter().any(|other| {
            other.id != snapshot.id && other.id.simple().to_string().starts_with(prefix)
        }) {
            return prefix.to_owned();
        }
    }
    full
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Scope;
    fn snapshot(id: &str, name: &str) -> Snapshot {
        Snapshot {
            version: 1,
            id: id.parse().unwrap(),
            name: name.into(),
            created_at: chrono::Utc::now(),
            scope: Scope::All,
            sessions: vec![],
        }
    }
    #[test]
    fn collisions_names_and_unicode() {
        let all = vec![
            snapshot("aaaaaaaa-1111-4111-8111-111111111111", "Личные-проекты"),
            snapshot("aaaaaaaa-2222-4222-8222-222222222222", "Other"),
        ];
        assert!(SnapshotQuery::new("aaaaaaaa").resolve(&all).is_err());
        assert_eq!(short_id(&all[0], &all), "aaaaaaaa1");
        assert_eq!(
            SnapshotQuery::new("aaaaaaaa1").resolve(&all).unwrap().id,
            all[0].id
        );
        assert_eq!(search(&all, "ЛИЧНЫЕ-"), [0]);
        assert_eq!(search(&all, " ЛЧ ПРК "), [0]);
        assert_eq!(search(&all, "aaaaAAAA-1"), [0]);
        assert!(SnapshotQuery::new("").resolve(&all).is_err());
        assert!(SnapshotQuery::new("---").resolve(&all).is_err());
    }

    #[test]
    fn ranks_exact_prefix_substring_and_subsequence_matches() {
        let all: Vec<_> = [
            "Personal projects",
            "My PPR",
            "PPR Tools",
            "PPR",
            "Unrelated",
            "Paper",
        ]
        .into_iter()
        .map(|name| {
            let mut snapshot = crate::model::test_snapshot();
            snapshot.name = name.into();
            snapshot
        })
        .collect();
        assert_eq!(search(&all, "ppr"), [3, 2, 1, 0, 5]);
        assert_eq!(search(&all, "  "), [0, 1, 2, 3, 4, 5]);
        assert!(search(&all, "rpz").is_empty());
        assert!(SnapshotQuery::new("pprj").resolve(&all).is_err());
    }
}
