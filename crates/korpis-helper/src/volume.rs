// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes in a class: created as a directory owned by id 0 as the workload sees it (files carry
//! workload-relative ids, §4.5 of docs/04-runtimes.md, and the driver's idmapped mount maps them
//! onto the workload's range), or as a subvolume on btrfs; put in a quota project of their own,
//! or limited by their qgroup, where the class enforces quota; made durable before the answer;
//! and removed without following a link. A path is only ever the class root joined with a
//! checked identifier.

use crate::error::{HelperError, HelperResult, identifier};
use crate::storage::{Backend, Class};
use crate::{btrfs, quota};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

/// A volume root's mode: the workload's root may write it, everyone in it may read it.
const VOLUME_MODE: u32 = 0o755;

/// Makes the directory entries under `dir` durable.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// That `path` exists as a directory and not as anything else; whether it exists.
fn existing_dir(path: &Path) -> HelperResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(HelperError::Failed(format!("{} is not a directory", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl Class {
    /// Creates the volume `r` names, or finds the one already there, with its limits as asked.
    pub fn ensure(
        &mut self,
        r: &helper::EnsureVolumeRequest,
    ) -> HelperResult<helper::EnsureVolumeResponse> {
        identifier("volume", &r.volume)?;
        let path = self.path_of(&r.volume);
        let created = self.make(&r.volume, &path)?;
        if created {
            std::os::unix::fs::chown(&path, Some(0), Some(0))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(VOLUME_MODE))?;
        }
        // An existing volume attached to another workload is asked for no size, and keeps the
        // limits it has; any other ask sets them, so a volume resized by a new intent gets its
        // new limits.
        if created || r.size_bytes > 0 || r.inodes > 0 {
            self.limit(r, &path)?;
        }
        if created {
            sync_dir(self.root())?;
            tracing::info!(volume = %r.volume, workload = %r.workload, "volume created");
        }
        Ok(helper::EnsureVolumeResponse {
            host_path: path.to_string_lossy().into_owned(),
            created,
            quota_enforced: self.backend().enforces(),
            storage_class: self.name().to_owned(),
            ..Default::default()
        })
    }

    /// Makes the volume `volume` at `path`, or finds it there; whether it was made. On btrfs it
    /// is a subvolume, and anything else already at its path is refused.
    fn make(&self, volume: &str, path: &Path) -> HelperResult<bool> {
        let made = if self.backend() == Backend::Btrfs {
            btrfs::create_subvolume(&self.handle, volume)
        } else {
            std::fs::create_dir(path)
        };
        let created = match made {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        existing_dir(path)?;
        let subvolume = std::fs::symlink_metadata(path)?.ino() == btrfs::SUBVOLUME_INODE;
        if self.backend() == Backend::Btrfs && !subvolume {
            let why = format!("{} is a directory, not a subvolume", path.display());
            return Err(HelperError::Failed(why));
        }
        Ok(created)
    }

    /// Sets the limits `r` asks of the volume at `path`, where the class enforces them.
    fn limit(&mut self, r: &helper::EnsureVolumeRequest, path: &Path) -> HelperResult<()> {
        match self.backend() {
            Backend::Ext4Project | Backend::XfsProject => {
                let project = self.projects.ensure(&r.volume)?;
                quota::assign(&std::fs::File::open(path)?, project)?;
                quota::limit(&self.handle, project, r.size_bytes, r.inodes)?;
            }
            Backend::Btrfs => {
                btrfs::limit(&std::fs::File::open(path)?, r.size_bytes)?;
                if r.inodes > 0 {
                    tracing::info!(volume = %r.volume, "btrfs limits bytes alone: no inode limit");
                }
            }
            Backend::Directory if r.size_bytes > 0 || r.inodes > 0 => {
                tracing::info!(volume = %r.volume, class = %self.name(), "no quota: none enforced");
            }
            Backend::Directory => {}
        }
        Ok(())
    }

    /// Removes the volume `volume` and everything in it; whether there was one.
    pub fn delete(&mut self, volume: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        let path = self.path_of(volume);
        let existed = existing_dir(&path)?;
        // Its snapshots, and a restore staged for it, hold its data too, and go with it.
        self.drop_snapshots(volume)?;
        self.discard(volume)?;
        if existed {
            let subvolume = std::fs::symlink_metadata(&path)?.ino() == btrfs::SUBVOLUME_INODE;
            if self.backend() == Backend::Btrfs && subvolume {
                // Its empty qgroup is left behind: destroying one is what marks the accounting
                // inconsistent (the module header of `btrfs`).
                btrfs::destroy_subvolume(&self.handle, volume)?;
            } else {
                // Removes links rather than following them, so nothing outside is reached.
                std::fs::remove_dir_all(&path)?;
            }
            sync_dir(self.root())?;
            tracing::info!(%volume, "volume deleted");
        }
        if let Some(project) = self.projects.of(volume) {
            quota::limit(&self.handle, project, 0, 0)?;
            self.projects.forget(volume)?;
        }
        Ok(existed)
    }

    /// What the filesystem accounts for `volume`.
    pub fn usage(&self, volume: &str) -> HelperResult<helper::VolumeUsageResponse> {
        identifier("volume", volume)?;
        if !existing_dir(&self.path_of(volume))? {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        if self.backend() == Backend::Btrfs {
            return self.btrfs_usage(volume);
        }
        // Nothing accounts a plain directory, and walking it would be computing, not reading
        // (§3 of docs/06-storage.md).
        let Some(project) = self.projects.of(volume).filter(|_| self.backend().enforces()) else {
            return Ok(helper::VolumeUsageResponse::default());
        };
        let u = quota::usage(&self.handle, project)?;
        Ok(helper::VolumeUsageResponse {
            used_bytes: u.bytes,
            used_inodes: u.inodes,
            limit_bytes: u.limit_bytes,
            limit_inodes: u.limit_inodes,
            accounted: true,
            ..Default::default()
        })
    }

    /// What btrfs accounts for `volume`: its subvolume's referenced bytes and limit; nothing it
    /// accounts while the accounting is inconsistent, when it would not be believed.
    fn btrfs_usage(&self, volume: &str) -> HelperResult<helper::VolumeUsageResponse> {
        let id = btrfs::subvolume_id(&std::fs::File::open(self.path_of(volume))?)?;
        let Some((used, limit)) = btrfs::usage(&self.handle, id)? else {
            return Ok(helper::VolumeUsageResponse::default());
        };
        Ok(helper::VolumeUsageResponse {
            used_bytes: used,
            limit_bytes: limit,
            accounted: true,
            ..Default::default()
        })
    }
}
