// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's volumes as one tree, the way the workload sees them: each path is served by the
//! volume mounted at the longest prefix of it, beneath that volume's own descriptor, so a walk
//! never crosses from one mount onto another (`RESOLVE_NO_XDEV`) and a volume mounted inside
//! another is reached as itself. What is in no volume is the worker's read-only root, which holds
//! only the directories the volumes are mounted on: it can be listed, to find them, and nothing
//! in it can be changed.

use crate::fs::{Entry, Fault, Root};
use std::os::fd::OwnedFd;

/// The volumes a session serves, and the root they are mounted in.
#[derive(Debug)]
pub struct Volumes {
    top: Root,
    /// By the path each is mounted at, longest first.
    volumes: Vec<(String, Root)>,
}

/// Where a path is served: by a volume, at a path relative to it, or by the root.
enum At<'v> {
    Volume(&'v Root, String),
    Top(String),
}

/// `path` with its empty components dropped; refused when it is not absolute, and outside when
/// it climbs.
fn normal(path: &str) -> Result<String, Fault> {
    let rest = path.strip_prefix('/').ok_or(Fault::Refused)?;
    let mut out = String::with_capacity(path.len());
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(Fault::Outside);
        }
        out.push('/');
        out.push_str(part);
    }
    Ok(if out.is_empty() { "/".to_owned() } else { out })
}

impl Volumes {
    /// The root `top`, read-only, and each of `volumes` by the path it is mounted at; each volume
    /// served for writing when `write`.
    #[must_use]
    pub fn new(top: OwnedFd, volumes: Vec<(String, OwnedFd)>, write: bool) -> Self {
        let mut volumes: Vec<(String, Root)> = volumes
            .into_iter()
            .filter_map(|(p, fd)| Some((normal(&p).ok()?, Root::new(fd, write))))
            .collect();
        volumes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        Self { top: Root::new(top, false), volumes }
    }

    fn at(&self, path: &str) -> Result<At<'_>, Fault> {
        let path = normal(path)?;
        for (mounted, root) in &self.volumes {
            if *mounted == path {
                return Ok(At::Volume(root, "/".to_owned()));
            }
            if let Some(rest) = path.strip_prefix(mounted.as_str()).filter(|r| r.starts_with('/')) {
                return Ok(At::Volume(root, rest.to_owned()));
            }
        }
        Ok(At::Top(path))
    }

    /// The volume serving `path` for a change, and the path within it; outside when none does.
    fn changing(&self, path: &str) -> Result<(&Root, String), Fault> {
        match self.at(path)? {
            At::Volume(root, rel) => Ok((root, rel)),
            At::Top(_) => Err(Fault::Outside),
        }
    }

    /// The metadata of `path`; a volume's own root is named as the directory it is mounted on.
    pub fn stat(&self, path: &str) -> Result<Entry, Fault> {
        match self.at(path)? {
            At::Volume(root, rel) if rel == "/" => {
                let mut e = root.stat("/")?;
                let path = normal(path)?;
                path.rsplit('/').next().unwrap_or_default().clone_into(&mut e.name);
                Ok(e)
            }
            At::Volume(root, rel) => root.stat(&rel),
            At::Top(rel) => self.top.stat(&rel),
        }
    }

    /// The entries of directory `path` after `after`, at most `limit`, and where the next page
    /// starts.
    pub fn list(
        &self,
        path: &str,
        after: &str,
        limit: usize,
    ) -> Result<(Vec<Entry>, String), Fault> {
        match self.at(path)? {
            At::Volume(root, rel) => root.list(&rel, after, limit),
            At::Top(rel) => self.top.list(&rel, after, limit),
        }
    }

    /// At most `length` bytes of `path` from `offset`.
    pub fn read(&self, path: &str, offset: u64, length: usize) -> Result<Vec<u8>, Fault> {
        match self.at(path)? {
            At::Volume(root, rel) => root.read(&rel, offset, length),
            At::Top(rel) => self.top.read(&rel, offset, length),
        }
    }

    /// Writes `data` to `path` at `offset`; how many bytes.
    pub fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
        (create, truncate): (bool, bool),
    ) -> Result<u64, Fault> {
        let (root, rel) = self.changing(path)?;
        root.write(&rel, offset, data, create, truncate)
    }

    /// Makes directory `path`.
    pub fn mkdir(&self, path: &str) -> Result<(), Fault> {
        let (root, rel) = self.changing(path)?;
        root.mkdir(&rel)
    }

    /// Renames `from` to `to` within one volume; across two, outside.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), Fault> {
        let ((a, from), (b, to)) = (self.changing(from)?, self.changing(to)?);
        if !std::ptr::eq(a, b) {
            return Err(Fault::Outside);
        }
        a.rename(&from, &to)
    }

    /// Removes `path`, and with `recursive` all in it; how many entries went.
    pub fn remove(&self, path: &str, recursive: bool) -> Result<u64, Fault> {
        let (root, rel) = self.changing(path)?;
        root.remove(&rel, recursive)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failing test should fail loudly"
    )]

    use super::Volumes;
    use crate::fs::{Fault, Kind};
    use rustix::fs::{Mode, OFlags};
    use std::path::Path;

    fn dir(p: &Path) -> std::os::fd::OwnedFd {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        rustix::fs::open(p, flags, Mode::empty()).unwrap()
    }

    /// A root holding `srv/data` and `srv/data/plugins`, each a volume (stood in for by the
    /// directories of a second tree, so the root's own are only where they are mounted).
    fn bench(write: bool) -> (tempfile::TempDir, Volumes) {
        let t = tempfile::tempdir().unwrap();
        for d in ["top/srv/data/plugins", "data/plugins", "plugins"] {
            std::fs::create_dir_all(t.path().join(d)).unwrap();
        }
        std::fs::write(t.path().join("data/level.dat"), b"world").unwrap();
        std::fs::write(t.path().join("plugins/a.jar"), b"jar").unwrap();
        let vols = vec![
            ("/srv/data".to_owned(), dir(&t.path().join("data"))),
            ("/srv/data/plugins/".to_owned(), dir(&t.path().join("plugins"))),
        ];
        let v = Volumes::new(dir(&t.path().join("top")), vols, write);
        (t, v)
    }

    #[test]
    fn each_path_is_served_by_the_innermost_volume_and_the_root_only_lists() {
        let (t, v) = bench(true);
        assert_eq!(v.read("/srv/data/level.dat", 0, 10).unwrap(), b"world");
        assert_eq!(v.read("//srv/data/plugins/a.jar", 0, 10).unwrap(), b"jar");
        // The nested volume's file is its own, not the outer volume's directory's.
        assert_eq!(v.read("/srv/data/plugins/a.jar", 0, 10).unwrap(), b"jar");
        let names = |p: &str| -> Vec<String> {
            v.list(p, "", 100).unwrap().0.into_iter().map(|e| e.name).collect()
        };
        assert_eq!(names("/"), ["srv"]);
        assert_eq!(names("/srv/data/plugins"), ["a.jar"]);
        let root = v.stat("/srv/data/plugins").unwrap();
        assert_eq!((root.name.as_str(), root.kind), ("plugins", Kind::Directory));
        v.write("/srv/data/new.txt", 0, b"hi", (true, false)).unwrap();
        assert_eq!(std::fs::read(t.path().join("data/new.txt")).unwrap(), b"hi");
        assert_eq!(v.write("/srv/x", 0, b"hi", (true, false)), Err(Fault::Outside));
        assert_eq!(v.mkdir("/etc"), Err(Fault::Outside));
        assert_eq!(v.read("/srv/data/../../etc/passwd", 0, 10), Err(Fault::Outside));
        assert_eq!(v.read("srv/data/level.dat", 0, 10), Err(Fault::Refused));
        // A volume's root is neither removed nor renamed, and nothing moves across volumes.
        assert_eq!(v.remove("/srv/data/plugins", true), Err(Fault::Refused));
        assert_eq!(v.rename("/srv/data/new.txt", "/srv/data/plugins/n"), Err(Fault::Outside));
        v.rename("/srv/data/new.txt", "/srv/data/moved.txt").unwrap();
        assert!(t.path().join("data/moved.txt").exists());
    }

    #[test]
    fn a_session_opened_to_read_changes_nothing() {
        let (_t, v) = bench(false);
        assert_eq!(v.read("/srv/data/level.dat", 0, 10).unwrap(), b"world");
        assert_eq!(v.write("/srv/data/level.dat", 0, b"x", (false, false)), Err(Fault::Refused));
        assert_eq!(v.remove("/srv/data/level.dat", false), Err(Fault::Refused));
    }
}
