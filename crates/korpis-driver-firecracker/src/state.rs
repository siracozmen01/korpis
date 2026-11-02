// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's execution state as a checkpoint leaves it: a directory directly beneath one of the
//! driver's volume roots holding what its Firecracker wrote (the machine's state and its memory),
//! the drives the guest booted from and holds open (its boot, image and config drives, since a
//! resumed guest reads them as the bytes it booted from, and the destination's own are built apart
//! and differ), and a record of where the state restores.
//!
//! The directory crosses between nodes, so whatever is in it is the source's to have written: each
//! file is opened beneath the directory, through no link and as a regular file alone, and copied,
//! never linked or bound, so what is resumed is what was checked and nothing outside it is named.

use rustix::fs::{FileType, Mode, OFlags, ResolveFlags};
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::path::Path;

/// The machine's state a checkpoint writes, by the name it has in the guest's chroot and in the
/// state's directory alike.
pub const VMSTATE: &str = "vmstate";
/// The guest's memory a checkpoint writes, named as [`VMSTATE`] is.
pub const MEMORY: &str = "memory";
/// The drives a resumed guest holds, as its chroot names them.
pub const DRIVES: [&str; 3] = ["boot.ext4", "image.ext4", "config"];
/// The record of where the state restores.
const RECORD: &str = "state.json";
/// The longest record read.
const RECORD_MOST: u64 = 64 << 10;

/// Where a state was taken, as what restores it: the hypervisor that wrote it, the CPU template
/// its guest saw, and the host kernel's version, `major.minor`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Taken {
    /// The Firecracker version, as `firecracker --version` says it.
    pub hypervisor_version: String,
    /// The CPU template the guest saw; empty for none.
    pub cpu_template: String,
    /// The host kernel's release, `major.minor`.
    pub kernel: String,
}

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);

/// The directory `at`, if it is one directly beneath one of `roots`, opened beneath that root.
pub fn dir(roots: &[&Path], at: &Path) -> io::Result<OwnedFd> {
    let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
        return Err(io::Error::other(format!("{} names no directory", at.display())));
    };
    let root = roots.iter().find(|r| **r == parent).ok_or_else(|| {
        io::Error::other(format!("{} is not directly beneath a volume root", at.display()))
    })?;
    let root = rustix::fs::open(*root, OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())?;
    let flags = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC;
    Ok(rustix::fs::openat2(&root, name, flags, Mode::empty(), RESOLVE)?)
}

/// The regular file `name` beneath `dir`, opened to read.
fn file_in(dir: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::openat2(dir, name, flags, Mode::empty(), RESOLVE)?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
        return Err(io::Error::other(format!("{name} is not a regular file")));
    }
    Ok(std::fs::File::from(fd))
}

/// The file `name` beneath `dir`, made anew for root alone to write: whatever had that name is
/// removed first, and what is opened is refused unless it is the regular file just made, so a
/// name planted as a link to another file is never written through.
fn made_in(dir: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
    match rustix::fs::unlinkat(dir, name, rustix::fs::AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(e) => return Err(e.into()),
    }
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat2(dir, name, flags, Mode::RUSR | Mode::WUSR, RESOLVE)?;
    let stat = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(io::Error::other(format!("{name} is not a file of its own")));
    }
    Ok(std::fs::File::from(fd))
}

/// Writes `bytes` from `source` into `name` beneath `to`, replacing what is there only once
/// whole; the bytes written.
fn written(source: &mut impl io::Read, to: &OwnedFd, name: &str) -> io::Result<u64> {
    let part = format!(".{name}.part");
    let mut target = made_in(to, &part)?;
    let n = io::copy(source, &mut target)?;
    target.sync_all()?;
    rustix::fs::renameat(to, &part, to, name)?;
    Ok(n)
}

/// Copies `name` from beneath `from` to beneath `to`, under the same name, readable by root
/// alone; the bytes copied.
fn copy(from: &OwnedFd, to: &OwnedFd, name: &str) -> io::Result<u64> {
    written(&mut file_in(from, name)?, to, name)
}

/// What a state's record says: the workload it was taken of, and where it was taken.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The workload, as its node names it.
    pub workload: String,
    /// Where it was taken.
    pub taken: Taken,
}

/// Writes what a checkpoint took into the directory `to`: the state, the memory and the drives
/// from the guest's chroot `chroot`, then the record `record`, last, so a directory with a record
/// holds a whole state. The bytes written.
pub fn keep(chroot: &OwnedFd, to: &OwnedFd, record: &Record) -> io::Result<u64> {
    let mut bytes = 0;
    for name in [VMSTATE, MEMORY].into_iter().chain(DRIVES) {
        bytes += copy(chroot, to, name)?;
    }
    let said = serde_json::to_vec(record).map_err(io::Error::other)?;
    Ok(bytes + written(&mut said.as_slice(), to, RECORD)?)
}

/// What the record of the state in `from` says; none is not a state.
pub fn record(from: &OwnedFd) -> io::Result<Record> {
    let mut said = Vec::new();
    file_in(from, RECORD)?.take(RECORD_MOST).read_to_end(&mut said)?;
    serde_json::from_slice(&said).map_err(|e| io::Error::other(format!("its record: {e}")))
}

/// Copies the state in `from` into the directory `to`, as the files a resumed guest is given.
pub fn take(from: &OwnedFd, to: &OwnedFd) -> io::Result<()> {
    for name in [VMSTATE, MEMORY].into_iter().chain(DRIVES) {
        copy(from, to, name)?;
    }
    Ok(())
}

/// The host kernel's version as a state records it: its release's `major.minor`.
#[must_use]
pub fn kernel() -> String {
    let uname = rustix::system::uname();
    let release = uname.release().to_string_lossy();
    release.split('.').take(2).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(p: &Path) -> OwnedFd {
        rustix::fs::open(p, OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap()
    }

    #[test]
    fn a_state_is_taken_whole_and_only_from_regular_files_beneath_it() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (chroot, state) = (root.join("chroot"), root.join("wl_1.state"));
        std::fs::create_dir_all(&chroot).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        for name in [VMSTATE, MEMORY].into_iter().chain(DRIVES) {
            std::fs::write(chroot.join(name), name).unwrap();
        }
        let taken = Taken {
            hypervisor_version: "1.17.0".into(),
            cpu_template: String::new(),
            kernel: kernel(),
        };
        let kept = Record { workload: "wl_1".into(), taken };
        let into = dir(&[&root], &state).unwrap();
        // A name planted as a link to another file is replaced, never written through.
        std::fs::write(root.join("elsewhere"), b"untouched").unwrap();
        std::fs::hard_link(root.join("elsewhere"), state.join(".memory.part")).unwrap();
        keep(&opened(&chroot), &into, &kept).unwrap();
        assert_eq!(record(&into).unwrap(), kept);
        assert_eq!(std::fs::read(root.join("elsewhere")).unwrap(), b"untouched");
        assert_eq!(std::fs::read(state.join(MEMORY)).unwrap(), MEMORY.as_bytes());

        // What crossed from another node names nothing outside it.
        std::fs::remove_file(state.join(MEMORY)).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", state.join(MEMORY)).unwrap();
        let resumed = root.join("object");
        std::fs::create_dir(&resumed).unwrap();
        assert!(take(&into, &opened(&resumed)).is_err(), "a link is not followed");
        assert!(dir(&[&root], &root.join("chroot/../wl_1.state")).is_err(), "nor a path around");
        assert!(dir(&[&chroot], &state).is_err(), "nor a directory beneath another root");
    }
}
