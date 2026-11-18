use crate::{model::Snapshot, query::SnapshotQuery};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::{
    cmp::Reverse,
    env, fs,
    io::{ErrorKind, Write},
    path::PathBuf,
};

pub struct Store {
    dir: PathBuf,
}

pub struct Catalog {
    pub snapshots: Vec<Snapshot>,
    pub warnings: Vec<String>,
}

impl Catalog {
    pub fn resolve(&self, query: &str) -> Result<&Snapshot> {
        SnapshotQuery::new(query).resolve(&self.snapshots)
    }
    pub fn latest(&self) -> Result<&Snapshot> {
        self.snapshots.first().context("no saved snapshots")
    }
}

impl Store {
    pub fn discover() -> Result<Self> {
        let dir = match env::var_os("TSM_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => ProjectDirs::from("dev", "tsm", "tsm")
                .context("could not determine data directory")?
                .data_dir()
                .join("snapshots"),
        };
        Ok(Self { dir })
    }

    pub fn save(&self, snapshot: &Snapshot) -> Result<()> {
        self.write(snapshot, false)
    }

    pub fn replace(&self, snapshot: &Snapshot) -> Result<()> {
        self.write(snapshot, true)
    }

    pub fn delete(&self, snapshot: &Snapshot) -> Result<()> {
        let path = self.dir.join(format!("{}.json", snapshot.id));
        fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))
    }

    fn write(&self, snapshot: &Snapshot, replace: bool) -> Result<()> {
        snapshot.validate()?;
        let path = self.dir.join(format!("{}.json", snapshot.id));
        if replace {
            fs::metadata(&path).with_context(|| format!("replace {}", path.display()))?;
        }
        fs::create_dir_all(&self.dir).with_context(|| format!("create {}", self.dir.display()))?;
        // Private file permissions (0600 on Unix); atomic publish.
        let mut temporary = tempfile::NamedTempFile::new_in(&self.dir)?;
        temporary.write_all(&serde_json::to_vec_pretty(snapshot)?)?;
        temporary.as_file().sync_all()?;
        if replace {
            temporary.persist(path)?;
        } else {
            temporary.persist_noclobber(path)?;
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Catalog> {
        let mut catalog = Catalog {
            snapshots: vec![],
            warnings: vec![],
        };
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(catalog),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", self.dir.display()));
            }
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let result = (|| -> Result<Snapshot> {
                let snapshot: Snapshot = serde_json::from_slice(&fs::read(&path)?)?;
                snapshot.validate()?;
                anyhow::ensure!(
                    path.file_stem().and_then(|s| s.to_str()) == Some(&snapshot.id.to_string()),
                    "filename and UUID differ"
                );
                Ok(snapshot)
            })();
            match result {
                Ok(snapshot) => catalog.snapshots.push(snapshot),
                Err(error) => catalog
                    .warnings
                    .push(format!("{}: {error:#}", path.display())),
            }
        }
        catalog
            .snapshots
            .sort_by_key(|snapshot| (Reverse(snapshot.created_at), snapshot.id));
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_snapshot;

    #[test]
    fn atomic_private_storage_preserves_valid_snapshots_despite_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store {
            dir: temp.path().into(),
        };
        let snapshot = test_snapshot();
        store.save(&snapshot).unwrap();
        assert!(
            store.save(&snapshot).is_err(),
            "must never overwrite a UUID"
        );
        fs::write(temp.path().join("bad.json"), "bad JSON").unwrap();
        let catalog = store.load().unwrap();
        assert_eq!(catalog.snapshots.len(), 1);
        assert_eq!(catalog.warnings.len(), 1);
        assert_eq!(
            catalog.resolve("Personal projects").unwrap().id,
            snapshot.id
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(temp.path().join(format!("{}.json", snapshot.id)))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let mut invalid = snapshot;
        invalid.sessions[0].active_window = 999;
        assert!(invalid.validate().is_err());
    }
}
