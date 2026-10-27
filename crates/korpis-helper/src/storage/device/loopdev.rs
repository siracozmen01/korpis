// SPDX-License-Identifier: AGPL-3.0-or-later

//! A loop class's device: a file in the directory the operator named, attached through a free
//! loop device, which the kernel then reads and writes the file through. The target is a bare file
//! name, and the file is opened beneath the directory without following a link, so a provider
//! naming `..` or a link names nothing. A file attached already is found by the backing file the
//! kernel reports for each loop device, and is let go of by clearing that device once its
//! filesystem is unmounted; the kernel clears it at once, or, while something else such as udev
//! still holds it open, as soon as that closes.
//!
//! This is one of the crate's unsafe modules, listed in the crate's header: the loop ioctls have
//! no safe wrapper in the libraries the helper may link. Each call passes a descriptor borrowed
//! for its duration and, for `LOOP_CONFIGURE`, a pointer to one correctly laid out structure
//! owned by the caller.

#![expect(unsafe_code, reason = "the loop ioctls have no safe wrapper")]

use crate::error::{HelperError, HelperResult};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

/// `LOOP_CTL_GET_FREE`, `LOOP_CONFIGURE`, and `LOOP_CLR_FD` (`linux/loop.h`).
const CTL_GET_FREE: libc::c_ulong = 0x4C82;
const CONFIGURE: libc::c_ulong = 0x4C0A;
const CLR_FD: libc::c_ulong = 0x4C01;
/// The size of `struct loop_config`: the backing descriptor, the block size, a
/// `struct loop_info64` of 232 bytes, and 64 reserved.
const CONFIG: usize = 304;

/// Runs `request` on `fd` with the number `arg`; what the kernel answered.
fn ioctl(fd: &File, request: libc::c_ulong, arg: libc::c_ulong) -> io::Result<libc::c_int> {
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it, and the two
    // requests passed here, `LOOP_CTL_GET_FREE` and `LOOP_CLR_FD`, take a number and read no
    // memory.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request, arg) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

/// Runs `LOOP_CONFIGURE` on `fd` with the bytes of `config`, which are its structure.
fn ioctl_configure(fd: &File, config: &mut [u8; CONFIG]) -> io::Result<()> {
    let ptr = config.as_mut_ptr();
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it. `ptr` points to
    // `config`, exclusively borrowed for the call and exactly `CONFIG` bytes, laid out as
    // `struct loop_config`, which the kernel reads and does not keep.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), CONFIGURE, ptr) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// The file `target` names in `dir`: a bare name, neither `.` nor `..`.
fn file_of(dir: &Path, target: &str) -> HelperResult<PathBuf> {
    super::plain("loop target", target, "")?;
    if target.starts_with('.') {
        return Err(HelperError::Refused(format!("loop target {target:?} is not a file's name")));
    }
    Ok(dir.join(target))
}

/// The loop device whose backing file is `file`, if one is. A device cleared while something
/// else held it open is only marked to clear itself once that closes, and is not one.
fn attached_to(file: &Path) -> io::Result<Option<PathBuf>> {
    for entry in std::fs::read_dir("/sys/block")? {
        let name = entry?.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with("loop")) else { continue };
        let read = |f: &str| std::fs::read_to_string(format!("/sys/block/{name}/loop/{f}"));
        let Ok(backing) = read("backing_file") else { continue };
        if read("autoclear").is_ok_and(|a| a.trim() == "1") {
            continue;
        }
        if Path::new(backing.trim_end()) == file {
            return Ok(Some(PathBuf::from(format!("/dev/{name}"))));
        }
    }
    Ok(None)
}

/// Opens `target` beneath `dir` for reading and writing, following no link.
fn backing(dir: &Path, target: &str) -> HelperResult<File> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    let root = File::open(dir)?;
    let how = ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat2(&root, target, flags, Mode::empty(), how)
        .map_err(|e| HelperError::Failed(format!("loop target {target:?}: {e}")))?;
    let file = File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(HelperError::Refused(format!("loop target {target:?} is not a file")));
    }
    Ok(file)
}

/// Configures the free loop device `device` to read and write `file`.
fn configure(device: &File, file: &File) -> io::Result<()> {
    let mut config = [0u8; CONFIG];
    let fd = u32::try_from(file.as_raw_fd()).map_err(|_| io::Error::other("descriptor"))?;
    if let Some(slot) = config.get_mut(..4) {
        slot.copy_from_slice(&fd.to_ne_bytes());
    }
    ioctl_configure(device, &mut config)
}

/// Attaches `target` in `dir` to a loop device, or finds the one it is attached to; the device.
pub(super) fn attach(dir: &Path, target: &str) -> HelperResult<PathBuf> {
    let path = file_of(dir, target)?;
    if let Some(device) = attached_to(&path)? {
        return Ok(device);
    }
    let file = backing(dir, target)?;
    let control = std::fs::OpenOptions::new().read(true).write(true).open("/dev/loop-control")?;
    // Another process may take the free device first; it is asked again.
    for _ in 0..8 {
        let number = ioctl(&control, CTL_GET_FREE, 0)?;
        let device = PathBuf::from(format!("/dev/loop{number}"));
        let opened = std::fs::OpenOptions::new().read(true).write(true).open(&device)?;
        match configure(&opened, &file) {
            Ok(()) => return Ok(device),
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(HelperError::Failed("no loop device stayed free".into()))
}

/// Clears the loop device `target` in `dir` is attached to, if one is.
pub(super) fn release(dir: &Path, target: &str) -> HelperResult<()> {
    let Some(device) = attached_to(&file_of(dir, target)?)? else { return Ok(()) };
    let opened = std::fs::OpenOptions::new().read(true).open(&device)?;
    match ioctl(&opened, CLR_FD, 0) {
        // Cleared by another between the look and the clear.
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => Ok(()),
        r => r.map(|_| ()).map_err(Into::into),
    }
}
