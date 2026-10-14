// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a workload path to the volume it lies in, and opening what is beneath that volume's
//! root by the kernel's rules alone: `RESOLVE_BENEATH` refuses any step out of the root, by `..` or
//! by a link; `RESOLVE_NO_MAGICLINKS` refuses `/proc` links; `RESOLVE_NO_XDEV` refuses crossing into
//! another mount, so a volume holds exactly what is beneath it.

use rustix::fs::{AtFlags, Mode, OFlags, ResolveFlags, mkdirat, openat2, statat};
use rustix::io::Errno;
use std::os::fd::OwnedFd;
use std::path::Path;

/// How every path beneath a volume is resolved.
pub const RESOLVE: ResolveFlags =
    ResolveFlags::BENEATH.union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);

/// A directory, held to name things in it.
const DIR: OFlags = OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);

/// The workload's volumes, each by the path it is mounted at and its root.
#[derive(Debug)]
pub struct Volumes {
    /// Longest path first, so a volume mounted inside another is found before it.
    roots: Vec<(String, OwnedFd)>,
}

/// Whether `name` is one plain component.
pub(crate) fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

impl Volumes {
    /// The volumes at `mounts`, each a workload path and the directory it is.
    ///
    /// # Errors
    /// A directory cannot be opened.
    pub fn open(mounts: &[(String, &Path)]) -> std::io::Result<Self> {
        let mut roots = Vec::new();
        for (at, dir) in mounts {
            let fd = rustix::fs::open(*dir, DIR, Mode::empty())?;
            roots.push((at.trim_end_matches('/').to_owned(), fd));
        }
        roots.sort_by_key(|(at, _)| std::cmp::Reverse(at.len()));
        Ok(Self { roots })
    }

    /// The volume `path` lies in, and the rest of it beneath that volume's root: empty for the
    /// root itself.
    ///
    /// # Errors
    /// It lies in no volume.
    pub fn locate(&self, path: &str) -> Result<(usize, &OwnedFd, String), String> {
        for (i, (at, fd)) in self.roots.iter().enumerate() {
            let rest = if path == at { Some("") } else { path.strip_prefix(at.as_str()) };
            if let Some(rest) = rest.filter(|r| r.is_empty() || r.starts_with('/')) {
                return Ok((i, fd, rest.trim_start_matches('/').to_owned()));
            }
        }
        Err(format!("{path} lies in no volume"))
    }
}

/// Opens the directory `rel` beneath `root`, making each missing one (mode 0755) when `make`.
pub(crate) fn dir(root: &OwnedFd, rel: &str, make: bool) -> rustix::io::Result<OwnedFd> {
    if rel.is_empty() {
        return openat2(root, ".", DIR, Mode::empty(), RESOLVE);
    }
    match openat2(root, rel, DIR, Mode::empty(), RESOLVE) {
        Err(Errno::NOENT) if make => {}
        other => return other,
    }
    let mut so_far = String::new();
    for part in rel.split('/') {
        if !plain(part) {
            return Err(Errno::INVAL);
        }
        let parent = openat2(
            root,
            if so_far.is_empty() { "." } else { &so_far },
            DIR,
            Mode::empty(),
            RESOLVE,
        )?;
        match mkdirat(&parent, part, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e),
        }
        if !so_far.is_empty() {
            so_far.push('/');
        }
        so_far.push_str(part);
    }
    openat2(root, rel, DIR, Mode::empty(), RESOLVE)
}

/// The directory holding `rel` beneath `root` (made when `make`), and its last component.
pub(crate) fn parent(root: &OwnedFd, rel: &str, make: bool) -> Result<(OwnedFd, String), String> {
    let (up, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    if !plain(name) {
        return Err(format!("{rel:?} names no file"));
    }
    let dir = dir(root, up, make).map_err(|e| format!("{up:?}: {e}"))?;
    Ok((dir, name.to_owned()))
}

/// What `name` in `dir` is, not following a link; `None` when it is absent.
pub(crate) fn kind(dir: &OwnedFd, name: &str) -> rustix::io::Result<Option<rustix::fs::FileType>> {
    match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => Ok(Some(rustix::fs::FileType::from_raw_mode(st.st_mode))),
        Err(Errno::NOENT) => Ok(None),
        Err(e) => Err(e),
    }
}
