// SPDX-License-Identifier: AGPL-3.0-or-later

//! A path of a block device's backup restored into the live volume, inside a guest (§5.4 of
//! docs/06-storage.md): the backup's image is mounted read-only beside the volume, and the entry
//! at a path in it is copied under its own name into a new directory at the volume's root, as a
//! restore of a volume of files lays it out. Every name is opened beneath the directory holding
//! it and never through a link, so a link the backup holds is copied as a link and never
//! followed. A file, a directory, and a link are copied with their mode, owner, and times, a file
//! with its holes; anything else (a device, a fifo, a socket) is refused by name, and a file
//! linked twice is copied twice.
//!
//! The copy is made in a fresh directory beside the target, named for the job, and renamed into
//! place last, so a copy that fails leaves no partial target, and running the job again starts it
//! over. The finished directory says which job made it, so a job asked again once it has
//! finished finds its own work and is done, and a target that exists otherwise is refused: a
//! restore makes a new directory and never merges into one.

use rustix::fs::{
    AtFlags, Dir, FileType, Gid, Mode, OFlags, RenameFlags, SeekFrom, Timestamps, Uid, chownat,
    fchmod, fstat, fsync, mkdirat, openat, readlinkat, renameat_with, statat, symlinkat, utimensat,
};
use rustix::io::Errno;
use std::ffi::{CStr, CString};
use std::os::fd::OwnedFd;

/// The attribute that names the job a restored directory is the work of.
const MADE_BY: &str = "user.korpis.restore";
/// How deep a copied tree may go.
const DEPTH: usize = 256;
/// How much of a file is read at once.
const CHUNK: usize = 1 << 20;
/// A directory, opened to list and to name things in it, never through a link.
const DIR: OFlags =
    OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// What a copy made.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    /// Files, directories, and links.
    pub entries: u64,
    /// The bytes of the files' data.
    pub bytes: u64,
}

/// Why a copy stopped, naming what it was at.
fn at(name: &CStr) -> impl Fn(Errno) -> String + '_ {
    move |e| format!("{}: {e}", name.to_string_lossy())
}

/// Copies the entry at `from`, a `/`-separated path beneath `source`, under its own name into
/// the new directory `into` at the root of `target`, as job `job`.
///
/// # Errors
/// `from` is absent or is the root, `into` exists and is not this job's work, the tree holds
/// something other than a file, a directory, or a link, or a copy failed.
pub fn copy_tree(
    (source, from): (&OwnedFd, &str),
    (target, into): (&OwnedFd, &str),
    job: &str,
) -> Result<Copied, String> {
    let parts: Vec<&str> = from.split('/').filter(|p| !p.is_empty()).collect();
    let (name, up) = parts.split_last().ok_or("a path of the backup, not its root")?;
    if !parts.iter().all(|p| super::beneath::plain(p)) || !super::beneath::plain(into) {
        return Err(format!("{from:?} into {into:?} is not a path beneath a volume"));
    }
    if let Some(done) = finished(target, into, job)? {
        return Ok(done);
    }
    let holder = super::beneath::dir(source, &up.join("/"), false)
        .and_then(|d| openat(&d, ".", DIR, Mode::empty()))
        .map_err(|e| format!("{from}: {e}"))?;
    let fresh = format!(".korpis-restore.{job}");
    super::files::remove(target, &fresh).map_err(|e| format!("{fresh}: {e}"))?;
    mkdirat(target, fresh.as_str(), Mode::from_raw_mode(0o700)).map_err(|e| format!("{e}"))?;
    let name = CString::new(*name).map_err(|e| e.to_string())?;
    let copied = (|| {
        let made = openat(target, fresh.as_str(), DIR, Mode::empty()).map_err(|e| e.to_string())?;
        let mut copied = Copied::default();
        entry((&holder, &made), &name, &mut copied, 0)?;
        fchmod(&made, Mode::from_raw_mode(0o755)).map_err(|e| e.to_string())?;
        rustix::fs::fsetxattr(&made, MADE_BY, job.as_bytes(), rustix::fs::XattrFlags::empty())
            .map_err(|e| format!("{fresh}: {e}"))?;
        fsync(&made).map_err(|e| e.to_string())?;
        renameat_with(target, fresh.as_str(), target, into, RenameFlags::NOREPLACE)
            .map_err(|e| format!("{into}: {e}"))?;
        // A volume's root is held to name things in it, which cannot be synced.
        let root = openat(target, ".", DIR, Mode::empty()).map_err(|e| e.to_string())?;
        fsync(&root).map_err(|e| e.to_string())?;
        Ok(copied)
    })();
    if copied.is_err() {
        let _ = super::files::remove(target, &fresh);
    }
    copied
}

/// What job `job` made at `into`, when it has already finished there; `None` when nothing is
/// there.
///
/// # Errors
/// Something else is there.
fn finished(target: &OwnedFd, into: &str, job: &str) -> Result<Option<Copied>, String> {
    let there = match openat(target, into, DIR, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(format!("{into}: {e}")),
    };
    let mut by = [0u8; 128];
    let n = rustix::fs::fgetxattr(&there, MADE_BY, &mut by).unwrap_or(0);
    if by.get(..n) == Some(job.as_bytes()) {
        return Ok(Some(Copied::default()));
    }
    Err(format!("{into} is there already: a restore makes a new directory"))
}

/// Copies `name` from directory `from` into directory `to`, at `depth`.
fn entry(
    (from, to): (&OwnedFd, &OwnedFd),
    name: &CStr,
    copied: &mut Copied,
    depth: usize,
) -> Result<(), String> {
    if depth > DEPTH {
        return Err(format!("{}: deeper than {DEPTH}", name.to_string_lossy()));
    }
    let st = statat(from, name, AtFlags::SYMLINK_NOFOLLOW).map_err(at(name))?;
    match FileType::from_raw_mode(st.st_mode) {
        FileType::RegularFile => copied.bytes += file((from, to), name)?,
        FileType::Directory => directory((from, to), name, copied, depth)?,
        FileType::Symlink => {
            let target = readlinkat(from, name, Vec::new()).map_err(at(name))?;
            symlinkat(target.as_c_str(), to, name).map_err(at(name))?;
        }
        _ => {
            return Err(format!(
                "{}: neither a file, a directory, nor a link",
                name.to_string_lossy()
            ));
        }
    }
    copied.entries += 1;
    kept(to, name, &st)
}

/// Copies the directory `name` and everything beneath it.
fn directory(
    (from, to): (&OwnedFd, &OwnedFd),
    name: &CStr,
    copied: &mut Copied,
    depth: usize,
) -> Result<(), String> {
    let inner = openat(from, name, DIR, Mode::empty()).map_err(at(name))?;
    mkdirat(to, name, Mode::from_raw_mode(0o700)).map_err(at(name))?;
    let made = openat(to, name, DIR, Mode::empty()).map_err(at(name))?;
    let names: Vec<CString> = Dir::read_from(&inner)
        .map_err(at(name))?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_owned())
        .filter(|n| n.as_bytes() != b"." && n.as_bytes() != b"..")
        .collect();
    for n in &names {
        entry((&inner, &made), n, copied, depth + 1)?;
    }
    Ok(())
}

/// Copies the file `name`'s data, its holes left as holes; how many bytes it holds.
fn file((from, to): (&OwnedFd, &OwnedFd), name: &CStr) -> Result<u64, String> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NOCTTY | OFlags::CLOEXEC;
    let src = openat(from, name, flags, Mode::empty()).map_err(at(name))?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let dst = openat(to, name, flags, Mode::from_raw_mode(0o600)).map_err(at(name))?;
    let size = u64::try_from(fstat(&src).map_err(at(name))?.st_size).unwrap_or(0);
    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK];
    while offset < size {
        let data = match rustix::fs::seek(&src, SeekFrom::Data(offset)) {
            Ok(at) => at,
            Err(Errno::NXIO) => break,
            Err(e) => return Err(at(name)(e)),
        };
        let hole = rustix::fs::seek(&src, SeekFrom::Hole(data)).map_err(at(name))?;
        run((&src, &dst), (data, hole), &mut buf).map_err(at(name))?;
        offset = hole;
    }
    rustix::fs::ftruncate(&dst, size).map_err(at(name))?;
    fsync(&dst).map_err(at(name))?;
    Ok(size)
}

/// Copies bytes `from..to` of `src` to the same place in `dst` through `buf`: read and written,
/// since a copy between two filesystems, as from a backup's image to a volume, is refused one
/// offloaded to the kernel.
fn run(
    (src, dst): (&OwnedFd, &OwnedFd),
    (from, to): (u64, u64),
    buf: &mut [u8],
) -> rustix::io::Result<()> {
    let mut at = from;
    while at < to {
        let want = usize::try_from(to - at).unwrap_or(usize::MAX).min(buf.len());
        let n = rustix::io::pread(src, buf.get_mut(..want).unwrap_or_default(), at)?;
        if n == 0 {
            break;
        }
        let mut written = 0;
        while written < n {
            let part = buf.get(written..n).unwrap_or_default();
            let offset = at + u64::try_from(written).unwrap_or(u64::MAX);
            written += rustix::io::pwrite(dst, part, offset)?;
        }
        at += u64::try_from(n).unwrap_or(u64::MAX);
    }
    Ok(())
}

/// Gives `name` in `to` the owner, mode, and times `st` says; a link keeps its own mode.
fn kept(to: &OwnedFd, name: &CStr, st: &rustix::fs::Stat) -> Result<(), String> {
    let (uid, gid) = (Uid::from_raw(st.st_uid), Gid::from_raw(st.st_gid));
    chownat(to, name, Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW).map_err(at(name))?;
    if FileType::from_raw_mode(st.st_mode) != FileType::Symlink {
        let mode = Mode::from_raw_mode(st.st_mode & 0o7777);
        rustix::fs::chmodat(to, name, mode, AtFlags::empty()).map_err(at(name))?;
    }
    let when = |sec, nsec: u64| rustix::fs::Timespec {
        tv_sec: sec,
        tv_nsec: nsec.try_into().unwrap_or_default(),
    };
    let times = Timestamps {
        last_access: when(st.st_atime, st.st_atime_nsec),
        last_modification: when(st.st_mtime, st.st_mtime_nsec),
    };
    utimensat(to, name, &times, AtFlags::SYMLINK_NOFOLLOW).map_err(at(name))
}
