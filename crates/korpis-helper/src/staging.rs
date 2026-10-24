// SPDX-License-Identifier: AGPL-3.0-or-later

//! A replacing restore's staging place (§5.4 of docs/06-storage.md): beside the volume in the
//! class root, under `.<volume>.restore`, a name no volume identifier can take. It is made empty,
//! with the volume's limit, as a subvolume on btrfs and in a quota project of its own where the
//! class keeps projects, so the restore is never counted against the live volume while both
//! exist. Committing exchanges it with the volume in one rename and removes what was the volume;
//! discarding removes it. Either finishes what an interrupted earlier call began, because the
//! staging place's identity, recorded when it was made, says whether the exchange happened. A
//! reading restore's place, `.<volume>.read+<job>`, is made the same way beside a block device for
//! its backup's image to be read in a service guest, and is only ever discarded.

use crate::btrfs;
use crate::error::{HelperError, HelperResult, identifier};
use crate::quota;
use crate::storage::{Backend, Class};
use rustix::fs::{RenameFlags, renameat_with};
use std::fs::File;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// A staging place as it was made: which volume it stands beside, what it is, and the limits
/// the volume had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Staging<'a> {
    /// The volume.
    pub volume: &'a str,
    /// Its subvolume identifier on btrfs, its inode elsewhere.
    pub identity: u64,
    /// The volume's limits when it was made, set on whichever holds the volume's name after.
    pub limits: (u64, u64),
}

fn name(volume: &str) -> String {
    format!(".{volume}.restore")
}

/// A reading restore's staging place, the job's own: `.<volume>.read+<job>`, which no volume
/// identifier can take either, and which, since no identifier holds a `+`, names one volume.
fn reading(volume: &str, job: &str) -> String {
    format!(".{volume}.read+{job}")
}

fn exists(path: &Path) -> HelperResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(HelperError::Failed(format!("{} is not a directory", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl Class {
    fn identity(&self, path: &Path) -> HelperResult<u64> {
        if self.backend() == Backend::Btrfs {
            Ok(btrfs::subvolume_id(&File::open(path)?)?)
        } else {
            Ok(std::fs::symlink_metadata(path)?.ino())
        }
    }

    /// The limits `volume` has, as its class keeps them.
    fn limits_of(&self, volume: &str) -> HelperResult<(u64, u64)> {
        match self.backend() {
            Backend::Btrfs => {
                let id = btrfs::subvolume_id(&File::open(self.path_of(volume))?)?;
                Ok((btrfs::limit_of(&self.handle, id)?, 0))
            }
            Backend::Ext4Project | Backend::XfsProject => match self.projects.of(volume) {
                Some(p) => {
                    let u = quota::usage(&self.handle, p)?;
                    Ok((u.limit_bytes, u.limit_inodes))
                }
                None => Ok((0, 0)),
            },
            Backend::Directory => Ok((0, 0)),
        }
    }

    /// Removes whatever is at `staged`, a subvolume or a tree, without following a link.
    fn remove(&self, staged: &str) -> HelperResult<()> {
        let path = self.root().join(staged);
        if !exists(&path)? {
            return Ok(());
        }
        if std::fs::symlink_metadata(&path)?.ino() == btrfs::SUBVOLUME_INODE {
            btrfs::destroy_subvolume(&self.handle, staged)?;
        } else {
            std::fs::remove_dir_all(&path)?;
        }
        Ok(())
    }

    /// Makes the empty staging place `staged` beside `volume`: a subvolume on btrfs, marked a block
    /// device's as its volume is, with the volume's limits, in a quota project of its own where
    /// the class keeps projects; where it is, and the limits.
    fn make_place(&mut self, staged: &str, volume: &str) -> HelperResult<(PathBuf, (u64, u64))> {
        let path = self.root().join(staged);
        if self.backend() == Backend::Btrfs {
            btrfs::create_subvolume(&self.handle, staged)?;
        } else {
            std::fs::create_dir(&path)?;
        }
        std::os::unix::fs::chown(&path, Some(0), Some(0))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        // A block device's restore is its image, and what takes the volume's name is one too
        // (Finding 159 of docs/23-walkthroughs.md).
        if crate::block::is_block(&self.path_of(volume)) {
            crate::block::mark(&path)?;
        }
        let limits = self.limits_of(volume)?;
        if matches!(self.backend(), Backend::Ext4Project | Backend::XfsProject) {
            let project = self.projects.ensure(staged)?;
            quota::assign(&File::open(&path)?, project)?;
            quota::limit(&self.handle, project, limits.0, limits.1)?;
        }
        self.handle.sync_all()?;
        Ok((path, limits))
    }

    /// Removes the staging place `staged` and its project, if it has them.
    fn drop_place(&mut self, staged: &str) -> HelperResult<()> {
        self.remove(staged)?;
        if let Some(p) = self.projects.of(staged) {
            quota::limit(&self.handle, p, 0, 0)?;
            self.projects.forget(staged)?;
        }
        Ok(())
    }

    /// Makes an empty staging place for `volume`, removing any an earlier restore left: where
    /// the restore goes, and what to record of it.
    pub fn stage(&mut self, volume: &str) -> HelperResult<(PathBuf, u64, (u64, u64))> {
        identifier("volume", volume)?;
        if !exists(&self.path_of(volume))? {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        let staged = name(volume);
        self.discard(volume)?;
        let (path, limits) = self.make_place(&staged, volume)?;
        Ok((path, self.identity(&self.root().join(&staged))?, limits))
    }

    /// Removes `volume`'s staging place and its project, if it has one.
    pub(crate) fn discard(&mut self, volume: &str) -> HelperResult<()> {
        self.drop_place(&name(volume))
    }

    /// Makes an empty staging place for job `job` to restore block device `volume`'s backup into
    /// for reading, removing any an earlier start of the job left; it is bounded as the volume is
    /// and never exchanged with it. Where the restore goes.
    ///
    /// # Errors
    /// `volume` is not a block device, whose backup is browsed as it is, or the place could not
    /// be made.
    pub fn stage_reading(&mut self, volume: &str, job: &str) -> HelperResult<PathBuf> {
        identifier("volume", volume)?;
        identifier("job", job)?;
        if !crate::block::is_block(&self.path_of(volume)) {
            let why = format!("volume {volume} is no block device: its backup is read as it is");
            return Err(HelperError::Refused(why));
        }
        let staged = reading(volume, job);
        self.drop_place(&staged)?;
        Ok(self.make_place(&staged, volume)?.0)
    }

    /// Removes job `job`'s reading place beside `volume`, and its project, if it has them.
    ///
    /// # Errors
    /// It could not be removed.
    pub fn discard_reading(&mut self, volume: &str, job: &str) -> HelperResult<()> {
        identifier("volume", volume)?;
        identifier("job", job)?;
        self.drop_place(&reading(volume, job))
    }

    /// Removes every reading place beside `volume`, with their projects: they hold its data.
    pub(crate) fn discard_readings(&mut self, volume: &str) -> HelperResult<()> {
        let prefix = reading(volume, "");
        let places: Vec<String> = std::fs::read_dir(self.root())?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        for place in places {
            self.drop_place(&place)?;
        }
        Ok(())
    }

    /// The image job `job` restores beside `volume` for reading.
    #[must_use]
    pub fn reading_image(&self, volume: &str, job: &str) -> PathBuf {
        self.root().join(reading(volume, job)).join(crate::block::IMAGE)
    }

    /// Commits `s`, making the volume what it holds, or discards it.
    pub fn settle(&mut self, s: &Staging<'_>, commit: bool) -> HelperResult<()> {
        identifier("volume", s.volume)?;
        let staged = name(s.volume);
        let target = self.path_of(s.volume);
        let mut exchanged = exists(&target)? && self.identity(&target)? == s.identity;
        if commit && !exchanged {
            let at = self.root().join(&staged);
            if !exists(&at)? || self.identity(&at)? != s.identity {
                return Err(HelperError::Failed(format!("{} is not the restore", at.display())));
            }
            renameat_with(
                &self.handle,
                staged.as_str(),
                &self.handle,
                s.volume,
                RenameFlags::EXCHANGE,
            )
            .map_err(std::io::Error::from)?;
            exchanged = true;
            tracing::info!(volume = %s.volume, "volume replaced by its restore");
        }
        if exchanged {
            self.took_over(s)?;
        } else {
            self.discard(s.volume)?;
        }
        self.handle.sync_all()?;
        Ok(())
    }

    /// After the exchange: the old data removed, and the volume's limits on what is now it.
    fn took_over(&mut self, s: &Staging<'_>) -> HelperResult<()> {
        let staged = name(s.volume);
        self.remove(&staged)?;
        let target = File::open(self.path_of(s.volume))?;
        match self.backend() {
            Backend::Btrfs => btrfs::limit(&target, s.limits.0)?,
            Backend::Ext4Project | Backend::XfsProject => {
                // The volume's directory is the staging place's, and carries its project.
                let now = quota::project_of(&target)?;
                if let Some(old) = self.projects.of(s.volume).filter(|p| *p != now) {
                    quota::limit(&self.handle, old, 0, 0)?;
                }
                self.projects.set(s.volume, now)?;
                quota::limit(&self.handle, now, s.limits.0, s.limits.1)?;
                self.projects.forget(&staged)?;
            }
            Backend::Directory => {}
        }
        Ok(())
    }
}
