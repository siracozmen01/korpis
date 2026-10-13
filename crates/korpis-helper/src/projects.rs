// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which quota project each volume of a class is, kept beside the volumes in the class root
//! (`.projects`, a name no volume identifier can take), so the record moves with the data.
//! Identifiers are allocated from [`FIRST`] upward, the lowest free one first, far above the
//! small numbers an operator gives projects by hand on the same filesystem.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The first project identifier the helper gives.
pub const FIRST: u32 = 1 << 24;

/// The record.
#[derive(Debug)]
pub struct Projects {
    path: PathBuf,
    by_volume: BTreeMap<String, u32>,
}

impl Projects {
    /// The record of the class rooted at `root`.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        let path = root.join(".projects");
        let by_volume = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter_map(|l| l.split_once(' '))
                .filter_map(|(id, volume)| Some((volume.to_owned(), id.parse().ok()?)))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, by_volume })
    }

    /// The project of `volume`, if it has one.
    #[must_use]
    pub fn of(&self, volume: &str) -> Option<u32> {
        self.by_volume.get(volume).copied()
    }

    /// The project of `volume`, given the lowest free one if it has none.
    pub fn ensure(&mut self, volume: &str) -> std::io::Result<u32> {
        if let Some(id) = self.of(volume) {
            return Ok(id);
        }
        let taken: std::collections::BTreeSet<u32> = self.by_volume.values().copied().collect();
        let id = (FIRST..u32::MAX)
            .find(|id| !taken.contains(id))
            .ok_or_else(|| std::io::Error::other("no project identifier is free"))?;
        self.by_volume.insert(volume.to_owned(), id);
        self.save()?;
        Ok(id)
    }

    /// Forgets the project of `volume`.
    pub fn forget(&mut self, volume: &str) -> std::io::Result<()> {
        if self.by_volume.remove(volume).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// Writes the record whole and durably, replacing the old one only once it is on disk.
    fn save(&self) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)?;
        for (volume, id) in &self.by_volume {
            writeln!(f, "{id} {volume}")?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        if let Some(dir) = self.path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_volume_has_its_own_project_and_a_freed_one_is_given_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Projects::open(dir.path()).unwrap();
        let a = p.ensure("wl_1.a").unwrap();
        let b = p.ensure("wl_1.b").unwrap();
        assert_eq!((a, b), (FIRST, FIRST + 1));
        assert_eq!(p.ensure("wl_1.a").unwrap(), a, "stable");
        p.forget("wl_1.a").unwrap();
        assert_eq!(Projects::open(dir.path()).unwrap().of("wl_1.b"), Some(b), "on disk");
        assert_eq!(p.ensure("wl_2.c").unwrap(), a, "the lowest free");
    }
}
