// SPDX-License-Identifier: AGPL-3.0-or-later

//! The storage classes the helper offers (§2 of docs/06-storage.md), each a directory it owns
//! with the backend that holds its volumes. The `directory` backend is a directory on a
//! filesystem without project quota: it enforces nothing, and every capability it declares says
//! so, so a size asked of it is kept nowhere and shown nowhere (Rule K-3).

use korpis_proto::korpis::v1 as pb;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// How a class keeps its volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Plain directories; no quota, no accounting, no snapshots.
    Directory,
}

/// A storage class.
#[derive(Debug, Clone)]
pub struct Class {
    name: String,
    root: PathBuf,
    backend: Backend,
}

/// The root's mode: root's own, and traversable by the driver that mounts what is in it.
const ROOT_MODE: u32 = 0o711;

impl Class {
    /// The class `name` keeping its volumes under `root`, which is created if missing and must
    /// be a directory, not a link to one.
    pub fn open(name: &str, root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        if !std::fs::symlink_metadata(root)?.is_dir() {
            let why = format!("{} is not a directory", root.display());
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
        }
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(ROOT_MODE))?;
        Ok(Self { name: name.to_owned(), root: root.to_owned(), backend: Backend::Directory })
    }

    /// Its name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its backend.
    #[must_use]
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Where the volume `volume`, an identifier already checked, lives.
    #[must_use]
    pub fn path_of(&self, volume: &str) -> PathBuf {
        self.root.join(volume)
    }

    /// Its root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What it offers and what it holds, as the filesystem reports it.
    #[must_use]
    pub fn describe(&self) -> pb::StorageClassInfo {
        let (capacity, free) = rustix::fs::statvfs(&self.root).map_or((0, 0), |s| {
            (s.f_blocks.saturating_mul(s.f_frsize), s.f_bavail.saturating_mul(s.f_frsize))
        });
        // Every capability false: a plain directory has none of them.
        let capabilities = pb::StorageCapabilities {
            fencing: pb::Fencing::FENCING_NONE.into(),
            ..Default::default()
        };
        pb::StorageClassInfo {
            name: self.name.clone(),
            backend: pb::StorageBackend::STORAGE_BACKEND_DIRECTORY.into(),
            capabilities: capabilities.into(),
            capacity_bytes: capacity,
            free_bytes: free,
            ..Default::default()
        }
    }
}
