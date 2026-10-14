// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where sealed objects are kept: an object store reached under one workload's prefix, with the
//! three things a job may do there (§5.6 of docs/06-storage.md): put an object, get one, and list
//! what is there. Nothing here deletes; pruning runs elsewhere, with a credential a job never
//! holds. The names given are relative to the prefix, which the store was opened on.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Why the store failed a call.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The store answered, refusing.
    #[error("the store refused: {0}")]
    Refused(String),
    /// The store could not be reached, or the transfer broke.
    #[error("the store failed: {0}")]
    Failed(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

/// An object store, beneath one prefix.
pub trait ObjectStore: Send + Sync {
    /// Writes `bytes` as `name`.
    fn put(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The bytes of `name`, or `None` when there is no such object.
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;

    /// Every name beginning with `prefix`, in order.
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send;
}

/// A store in memory, for tests and for a job's dry run.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl MemoryStore {
    fn objects(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>>, StoreError> {
        self.objects.lock().map_err(|_| StoreError::Failed("the store's lock is poisoned".into()))
    }

    /// Replaces the bytes of `name`, as a hostile or broken store would.
    pub fn tamper(&self, name: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.objects()?.insert(name.to_owned(), bytes);
        Ok(())
    }

    /// Removes `name`, as a prune's sweep would with the target's own credential.
    pub fn remove(&self, name: &str) -> Result<(), StoreError> {
        self.objects()?.remove(name);
        Ok(())
    }
}

impl ObjectStore for MemoryStore {
    fn put(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let done = self.objects().map(|mut o| {
            o.insert(name.to_owned(), bytes);
        });
        std::future::ready(done)
    }

    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        std::future::ready(self.objects().map(|o| o.get(name).cloned()))
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send {
        let names = self.objects().map(|o| {
            let after = o.range(prefix.to_owned()..).map(|(k, _)| k);
            after.take_while(|k| k.starts_with(prefix)).cloned().collect()
        });
        std::future::ready(names)
    }
}

/// A store in a directory: one file per object, written beside its name and renamed into place,
/// so a reader never sees half an object.
#[derive(Debug, Clone)]
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    /// A store kept beneath `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where `name` is kept, once it is known to be a relative name of segments.
    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        let segments_ok = name
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.starts_with(".tmp"));
        if !segments_ok {
            return Err(StoreError::Refused(format!("{name:?} is not an object name")));
        }
        Ok(self.root.join(name))
    }
}

impl ObjectStore for DirStore {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let path = self.path(name)?;
        let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            std::fs::create_dir_all(&dir)?;
            let mut file = tempfile_in(&dir)?;
            std::io::Write::write_all(&mut file.1, &bytes)?;
            file.1.sync_all()?;
            std::fs::rename(&file.0, &path)?;
            Ok(())
        })
        .await
        .map_err(|e| StoreError::Failed(e.to_string()))?
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match tokio::fs::read(self.path(name)?).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let root = self.root.clone();
        let prefix = prefix.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut names = Vec::new();
            walk_names(&root, "", &mut names)?;
            names.retain(|n| n.starts_with(&prefix));
            names.sort();
            Ok(names)
        })
        .await
        .map_err(|e| StoreError::Failed(e.to_string()))?
    }
}

/// A new file in `dir` that no object name can be, open for writing.
fn tempfile_in(dir: &std::path::Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|e| std::io::Error::other(e.to_string()))?;
    let name = format!(".tmp{}", u64::from_le_bytes(suffix));
    let path = dir.join(name);
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok((path, file))
}

fn walk_names(dir: &std::path::Path, at: &str, out: &mut Vec<String>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if name.starts_with(".tmp") {
            continue;
        }
        let full = format!("{at}{name}");
        if entry.file_type()?.is_dir() {
            walk_names(&entry.path(), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}
