// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tree a layer is applied to. Every path is resolved by the kernel as if the tree were the
//! root (`RESOLVE_IN_ROOT`): a symlink an earlier layer left, absolute or climbing, is followed
//! within the tree and never out of it, as the image will see it once it is a root filesystem.

use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, ResolveFlags, mkdirat, openat, openat2, readlinkat, unlinkat,
};
use rustix::io::Errno;
use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

/// How a path is resolved beneath the tree.
const RESOLVE: ResolveFlags =
    ResolveFlags::IN_ROOT.union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);

/// A directory opened to work in.
const DIR: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);

/// `raw`, an archive's path, as components beneath the tree: its leading `/` and `.` dropped, and
/// refused if it climbs with `..`, which no layer needs.
pub fn normal(raw: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                let why = format!("{} climbs out of the tree", raw.display());
                return Err(io::Error::new(io::ErrorKind::InvalidData, why));
            }
        }
    }
    Ok(out)
}

/// How many links a path may pass through on its way to a directory, as the kernel's own bound.
const HOPS: u32 = 40;

/// `target`, a link's, as a path beneath the tree, read from `at`: an absolute one from the
/// tree's root, a relative one from `at`, and `..` going no higher than the root, as the kernel
/// resolves it beneath the tree.
fn within(at: &Path, target: &Path) -> PathBuf {
    let mut out = if target.is_absolute() { PathBuf::new() } else { at.to_path_buf() };
    for c in target.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The directory at `rel` beneath `root`, each missing component made `0755`; a component that
/// is a link to nothing has its target made, as an image's later layer writing through an
/// earlier layer's link expects.
pub fn dir(root: &OwnedFd, rel: &Path) -> io::Result<OwnedFd> {
    dir_within(root, rel, HOPS)
}

fn dir_within(root: &OwnedFd, rel: &Path, hops: u32) -> io::Result<OwnedFd> {
    if rel.as_os_str().is_empty() {
        return Ok(openat2(root, ".", DIR, Mode::empty(), RESOLVE)?);
    }
    match openat2(root, rel, DIR, Mode::empty(), RESOLVE) {
        Err(Errno::NOENT) => {}
        opened => return Ok(opened?),
    }
    let name = rel.file_name().ok_or_else(|| invalid(rel, "names nothing"))?;
    let above = rel.parent().unwrap_or_else(|| Path::new(""));
    let parent = dir_within(root, above, hops)?;
    match mkdirat(&parent, name, Mode::from_raw_mode(0o755)) {
        Ok(()) => {}
        Err(Errno::EXIST) => {
            let target =
                readlinkat(&parent, name, Vec::new()).map_err(|_| invalid(rel, "exists"))?;
            let target = Path::new(OsStr::from_bytes(target.to_bytes()));
            let hops = hops.checked_sub(1).ok_or_else(|| invalid(rel, "too many links"))?;
            dir_within(root, &within(above, target), hops)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(openat2(root, rel, DIR, Mode::empty(), RESOLVE)?)
}

fn invalid(rel: &Path, why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{}: {why}", rel.display()))
}

/// The directory `rel` is in, made if missing, and its last component.
pub fn parent(root: &OwnedFd, rel: &Path) -> io::Result<(OwnedFd, PathBuf)> {
    let name = rel.file_name().map(PathBuf::from).ok_or_else(|| invalid(rel, "names nothing"))?;
    let above = rel.parent().unwrap_or_else(|| Path::new(""));
    Ok((dir(root, above)?, name))
}

/// Removes `name` from `dir`, whatever it is, and with a directory everything beneath it; a name
/// that is not there is already removed.
pub fn remove(dir: &OwnedFd, name: &Path) -> io::Result<()> {
    match unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => return Ok(()),
        Err(Errno::ISDIR | Errno::PERM) => {}
        Err(e) => return Err(e.into()),
    }
    let flags = DIR | OFlags::NOFOLLOW;
    let inner = openat(dir, name, flags, Mode::empty())?;
    empty(&inner, |_| false)?;
    unlinkat(dir, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

/// Removes everything in `dir` but the names `keep` holds to.
pub fn empty(dir: &OwnedFd, keep: impl Fn(&Path) -> bool) -> io::Result<()> {
    let mut names = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = Path::new(OsStr::from_bytes(entry.file_name().to_bytes()));
        if name != Path::new(".") && name != Path::new("..") && !keep(name) {
            names.push(name.to_path_buf());
        }
    }
    names.iter().try_for_each(|n| remove(dir, n))
}
