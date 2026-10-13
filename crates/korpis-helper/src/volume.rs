// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes in a class: created as a directory owned by id 0 as the workload sees it (files carry
//! workload-relative ids, §4.5 of docs/04-runtimes.md, and the driver's idmapped mount maps them
//! onto the workload's range), made durable before the answer, and removed without following a
//! link. A path is only ever the class root joined with a checked identifier.

use crate::error::{HelperError, HelperResult, identifier};
use crate::storage::{Backend, Class};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::PermissionsExt as _;
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
    /// Creates the volume `r` names, or finds the one already there.
    pub fn ensure(
        &self,
        r: &helper::EnsureVolumeRequest,
    ) -> HelperResult<helper::EnsureVolumeResponse> {
        identifier("volume", &r.volume)?;
        let path = self.path_of(&r.volume);
        let created = match std::fs::create_dir(&path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        existing_dir(&path)?;
        if created {
            std::os::unix::fs::chown(&path, Some(0), Some(0))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(VOLUME_MODE))?;
            sync_dir(self.root())?;
            tracing::info!(volume = %r.volume, workload = %r.workload, "volume created");
        }
        let quota_enforced = match self.backend() {
            Backend::Directory => false,
        };
        if !quota_enforced && (r.size_bytes > 0 || r.inodes > 0) {
            tracing::info!(volume = %r.volume, class = %self.name(), "no quota: none enforced");
        }
        Ok(helper::EnsureVolumeResponse {
            host_path: path.to_string_lossy().into_owned(),
            created,
            quota_enforced,
            ..Default::default()
        })
    }

    /// Removes the volume `volume` and everything in it; whether there was one.
    pub fn delete(&self, volume: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        let path = self.path_of(volume);
        if !existing_dir(&path)? {
            return Ok(false);
        }
        // Removes links rather than following them, so nothing outside the volume is reached.
        std::fs::remove_dir_all(&path)?;
        sync_dir(self.root())?;
        tracing::info!(%volume, "volume deleted");
        Ok(true)
    }

    /// What the filesystem accounts for `volume`.
    pub fn usage(&self, volume: &str) -> HelperResult<helper::VolumeUsageResponse> {
        identifier("volume", volume)?;
        if !existing_dir(&self.path_of(volume))? {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        match self.backend() {
            // Nothing accounts a plain directory, and walking it would be computing, not
            // reading (§3 of docs/06-storage.md).
            Backend::Directory => Ok(helper::VolumeUsageResponse::default()),
        }
    }
}
