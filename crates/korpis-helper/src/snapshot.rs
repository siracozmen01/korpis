// SPDX-License-Identifier: AGPL-3.0-or-later

//! Snapshots (§4 of docs/06-storage.md), in a class whose backend can take them: on btrfs, a
//! read-only snapshot subvolume kept under the class root's `.snapshots/<volume>/`, a directory no
//! volume identifier can name, because none starts with a dot. A class that cannot snapshot says
//! so and does nothing.
//!
//! Rolling back makes a volume what a snapshot holds, crash-safely: a writable copy of the
//! snapshot is made beside the volume, the two are exchanged in one rename, and the copy, now
//! the old data, is deleted. Interrupted anywhere, doing it again ends in the same place. The
//! volume's limit, which belongs to its old subvolume's qgroup, is set again on the new one.

use crate::btrfs;
use crate::error::{HelperError, HelperResult, identifier};
use crate::storage::{Backend, Class};
use rustix::fs::{RenameFlags, renameat_with};
use std::fs::File;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::PathBuf;

/// Where a class keeps its volumes' snapshots.
const SNAPSHOTS: &str = ".snapshots";

pub(crate) fn is_subvolume(path: &std::path::Path) -> HelperResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(m.is_dir() && m.ino() == btrfs::SUBVOLUME_INODE),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl Class {
    /// That this class can snapshot, or why not.
    fn snapshots_here(&self) -> HelperResult<()> {
        if self.backend() == Backend::Btrfs {
            return Ok(());
        }
        let why = format!("storage class {:?} ({:?}) cannot snapshot", self.name(), self.backend());
        Err(HelperError::Refused(why))
    }

    /// The directory holding `volume`'s snapshots.
    pub(crate) fn snapshot_dir(&self, volume: &str) -> PathBuf {
        self.root().join(SNAPSHOTS).join(volume)
    }

    /// Takes snapshot `snapshot` of `volume`, read-only; whether it was taken now.
    pub fn snapshot(&mut self, volume: &str, snapshot: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        self.snapshots_here()?;
        let source = self.path_of(volume);
        if !is_subvolume(&source)? {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        let dir = self.snapshot_dir(volume);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        if is_subvolume(&dir.join(snapshot))? {
            return Ok(false);
        }
        btrfs::snapshot(&File::open(&source)?, &File::open(&dir)?, snapshot, true)?;
        File::open(&dir)?.sync_all()?;
        tracing::info!(%volume, %snapshot, "snapshot taken");
        Ok(true)
    }

    /// Makes `volume` what its snapshot `snapshot` holds, keeping its limit.
    pub fn rollback(&mut self, volume: &str, snapshot: &str) -> HelperResult<()> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        self.snapshots_here()?;
        let taken = self.snapshot_dir(volume).join(snapshot);
        let target = self.path_of(volume);
        if !is_subvolume(&taken)? || !is_subvolume(&target)? {
            return Err(HelperError::NotFound(format!("snapshot {snapshot} of volume {volume}")));
        }
        let limit = btrfs::limit_of(&self.handle, btrfs::subvolume_id(&File::open(&target)?)?)?;
        let staging = format!(".{volume}.rollback");
        if is_subvolume(&self.root().join(&staging))? {
            btrfs::destroy_subvolume(&self.handle, &staging)?;
        }
        btrfs::snapshot(&File::open(&taken)?, &self.handle, &staging, false)?;
        renameat_with(&self.handle, &staging, &self.handle, volume, RenameFlags::EXCHANGE)
            .map_err(std::io::Error::from)?;
        btrfs::destroy_subvolume(&self.handle, &staging)?;
        btrfs::limit(&File::open(&target)?, limit)?;
        self.handle.sync_all()?;
        tracing::info!(%volume, %snapshot, "volume rolled back");
        Ok(())
    }

    /// Removes snapshot `snapshot` of `volume`; whether there was one.
    pub fn delete_snapshot(&mut self, volume: &str, snapshot: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        let dir = self.snapshot_dir(volume);
        if !is_subvolume(&dir.join(snapshot))? {
            return Ok(false);
        }
        btrfs::destroy_subvolume(&File::open(&dir)?, snapshot)?;
        File::open(&dir)?.sync_all()?;
        tracing::info!(%volume, %snapshot, "snapshot deleted");
        Ok(true)
    }

    /// Removes every snapshot of `volume`, as the volume itself is deleted.
    pub(crate) fn drop_snapshots(&mut self, volume: &str) -> HelperResult<()> {
        let dir = self.snapshot_dir(volume);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let name = entry?.file_name();
            let name =
                name.to_str().ok_or_else(|| HelperError::Failed("a snapshot name".into()))?;
            self.delete_snapshot(volume, name)?;
        }
        std::fs::remove_dir(&dir)?;
        Ok(())
    }
}
