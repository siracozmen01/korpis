// SPDX-License-Identifier: AGPL-3.0-or-later

//! The storage classes the helper offers (§2 of docs/06-storage.md), each a directory it owns
//! with the backend that holds its volumes, chosen from what the filesystem can do rather than
//! from what anyone says it can. On ext4 or XFS with project quota on, a volume is a project with
//! byte and inode limits in the write path. On btrfs with qgroups on and consistent, a volume is
//! a subvolume with a byte limit, and the class can snapshot and send. Anywhere else a class is a
//! `directory`: it enforces nothing, and every capability it declares says so, so a size asked of
//! it is kept nowhere and shown nowhere (Rule K-3).

use crate::projects::Projects;
use crate::{btrfs, quota};
use korpis_proto::korpis::v1 as pb;
use std::fs::File;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// `statfs` magic numbers of the filesystems with project quota.
const EXT4_MAGIC: u64 = 0xEF53;
const XFS_MAGIC: u64 = 0x5846_5342;

/// How a class keeps its volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Plain directories; no quota, no accounting, no snapshots.
    Directory,
    /// One quota project per volume, on ext4.
    Ext4Project,
    /// One quota project per volume, on XFS.
    XfsProject,
    /// One subvolume per volume, its bytes limited by its qgroup.
    Btrfs,
}

impl Backend {
    /// Whether limits are enforced in the write path.
    #[must_use]
    pub fn enforces(self) -> bool {
        !matches!(self, Self::Directory)
    }
}

/// A storage class.
#[derive(Debug)]
pub struct Class {
    name: String,
    root: PathBuf,
    backend: Backend,
    /// The root, held open: quota calls name the filesystem through it.
    pub(crate) handle: File,
    pub(crate) projects: Projects,
}

/// The root's mode: root's own, and traversable by the driver that mounts what is in it.
const ROOT_MODE: u32 = 0o711;

/// What the filesystem holding `root` can do.
fn backend_of(root: &File) -> Backend {
    let magic = rustix::fs::fstatfs(root).ok().and_then(|s| u64::try_from(s.f_type).ok());
    match magic {
        Some(EXT4_MAGIC) if quota::enabled(root) => Backend::Ext4Project,
        Some(XFS_MAGIC) if quota::enabled(root) => Backend::XfsProject,
        Some(btrfs::MAGIC) => match btrfs::quota_enforcing(root) {
            Ok(true) => Backend::Btrfs,
            Ok(false) => {
                tracing::warn!(
                    "btrfs without qgroups on and consistent enforces nothing: \
                     `btrfs quota enable`, or `btrfs quota rescan -w`, and restart the helper"
                );
                Backend::Directory
            }
            Err(e) => {
                tracing::warn!(error = %e, "btrfs qgroup status unreadable; enforcing nothing");
                Backend::Directory
            }
        },
        _ => Backend::Directory,
    }
}

/// What `backend` can do, as it is declared (§2 of docs/06-storage.md).
fn capabilities(backend: Backend) -> pb::StorageCapabilities {
    let mut c =
        pb::StorageCapabilities { fencing: pb::Fencing::FENCING_NONE.into(), ..Default::default() };
    match backend {
        Backend::Directory => {}
        // Quota is all a project backend has (§2 of 06-storage).
        Backend::Ext4Project | Backend::XfsProject => {
            (c.quota_enforced, c.quota_capacity, c.quota_inodes) = (true, true, true);
        }
        // Bytes alone: btrfs has no inode quota. A qgroup limit changes either way at any time.
        Backend::Btrfs => {
            (c.quota_enforced, c.quota_capacity) = (true, true);
            (c.snapshot, c.snapshot_atomic, c.incremental_send) = (true, true, true);
            (c.thin_provision, c.reflink_copy) = (true, true);
            (c.online_grow, c.online_shrink) = (true, true);
        }
    }
    c
}

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
        let handle = File::open(root)?;
        let backend = backend_of(&handle);
        tracing::info!(class = %name, root = %root.display(), ?backend, "storage class");
        let projects = Projects::open(root)?;
        Ok(Self { name: name.to_owned(), root: root.to_owned(), backend, handle, projects })
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
        let (capacity, free) = rustix::fs::fstatvfs(&self.handle).map_or((0, 0), |s| {
            (s.f_blocks.saturating_mul(s.f_frsize), s.f_bavail.saturating_mul(s.f_frsize))
        });
        let backend = match self.backend {
            Backend::Directory => pb::StorageBackend::STORAGE_BACKEND_DIRECTORY,
            Backend::Ext4Project => pb::StorageBackend::STORAGE_BACKEND_EXT4_PROJECT,
            Backend::XfsProject => pb::StorageBackend::STORAGE_BACKEND_XFS_PROJECT,
            Backend::Btrfs => pb::StorageBackend::STORAGE_BACKEND_BTRFS,
        };
        let capabilities = capabilities(self.backend);
        pb::StorageClassInfo {
            name: self.name.clone(),
            backend: backend.into(),
            capabilities: capabilities.into(),
            capacity_bytes: capacity,
            free_bytes: free,
            ..Default::default()
        }
    }
}
