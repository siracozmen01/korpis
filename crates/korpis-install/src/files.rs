// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the verbs do to one name in a directory already resolved beneath its volume. Nothing
//! here follows a link at the name: a file is created exclusively and renamed over its target, a
//! mode or an owner is set on what the name is and refused on a link, and a tree is removed
//! without descending through a link into somewhere else.

use rustix::fs::{
    AtFlags, Dir, FileType, Gid, Mode, OFlags, Uid, chownat, fchmod, fstat, fsync, openat,
    renameat, unlinkat,
};
use rustix::io::Errno;
use std::ffi::{CStr, CString};
use std::io::{Read, Write as _};
use std::os::fd::OwnedFd;

/// A name for something made beside its target before it takes its place.
pub(crate) fn temp_name(what: &str) -> String {
    let mut nonce = [0u8; 8];
    // A name that fails to be random is still unique enough with the process id; creation is
    // exclusive, so a collision is an error, never an overwrite.
    let _ = getrandom::fill(&mut nonce);
    format!(".korpis-{what}.{}.{:x}", std::process::id(), u64::from_le_bytes(nonce))
}

/// Writes what `src` holds as `name` in `dir`, mode `mode`, replacing what was there whole: the
/// new file is written beside it, synced, and renamed over it.
pub(crate) fn replace_file(
    dir: &OwnedFd,
    name: &str,
    src: &mut dyn Read,
    mode: u32,
) -> std::io::Result<()> {
    let tmp = temp_name("write");
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat(dir, &tmp, flags, Mode::from_raw_mode(0o600))?;
    let written = (|| {
        let mut file = std::fs::File::from(fd.try_clone()?);
        std::io::copy(src, &mut file)?;
        file.flush()?;
        fchmod(&fd, Mode::from_raw_mode(mode))?;
        fsync(&fd)?;
        renameat(dir, &tmp, dir, name)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = unlinkat(dir, &tmp, AtFlags::empty());
    }
    written
}

/// Makes the directory `name` in `dir`, or finds it there.
pub(crate) fn make_dir(dir: &OwnedFd, name: &str) -> std::io::Result<()> {
    match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o755)) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) if super::beneath::kind(dir, name)? == Some(FileType::Directory) => {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Sets the mode of `name` in `dir`, a file or a directory and never a link: it is looked at
/// without being opened, then opened without following a link, and changed only if what was
/// opened is what was looked at.
pub(crate) fn set_mode(dir: &OwnedFd, name: &str, mode: u32) -> std::io::Result<()> {
    let seen = openat(dir, name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
    let before = fstat(&seen)?;
    let ty = FileType::from_raw_mode(before.st_mode);
    if !matches!(ty, FileType::RegularFile | FileType::Directory) {
        return Err(std::io::Error::other(format!("{name} is not a file or a directory")));
    }
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let opened = openat(dir, name, flags, Mode::empty())?;
    let now = fstat(&opened)?;
    if (now.st_dev, now.st_ino) != (before.st_dev, before.st_ino) {
        return Err(std::io::Error::other(format!("{name} changed while its mode was set")));
    }
    fchmod(&opened, Mode::from_raw_mode(mode))?;
    Ok(())
}

/// Sets the owner of `name` in `dir`; a link's own owner, if it is one.
pub(crate) fn set_owner(dir: &OwnedFd, name: &str, uid: u32, gid: u32) -> std::io::Result<()> {
    let (u, g) = (Uid::from_raw(uid), Gid::from_raw(gid));
    chownat(dir, name, Some(u), Some(g), AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(())
}

/// Removes `name` from `dir`, and all of it if it is a directory; nothing if it is absent.
pub(crate) fn remove(dir: &OwnedFd, name: &str) -> std::io::Result<()> {
    let name = CString::new(name).map_err(std::io::Error::other)?;
    remove_named(dir, &name)
}

/// [`remove`], for a name as the directory holds it, which need not be UTF-8.
fn remove_named(dir: &OwnedFd, name: &CStr) -> std::io::Result<()> {
    let st = match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
        return Ok(unlinkat(dir, name, AtFlags::empty())?);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let inner = openat(dir, name, flags, Mode::empty())?;
    let names: Vec<CString> = Dir::read_from(&inner)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_owned())
        .filter(|n| n.as_bytes() != b"." && n.as_bytes() != b"..")
        .collect();
    for n in &names {
        remove_named(&inner, n)?;
    }
    unlinkat(dir, name, AtFlags::REMOVEDIR)?;
    Ok(())
}
