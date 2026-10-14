// SPDX-License-Identifier: AGPL-3.0-or-later

//! Project quota on ext4 and XFS (§3 of docs/06-storage.md): a volume's directory is given a
//! project identifier that everything created in it inherits, and the project is given byte and
//! inode limits the kernel checks in the write path, where exceeding them is `EDQUOT`. Usage is
//! read from the same accounting, never computed.
//!
//! This is one of the crate's two unsafe modules, with [`crate::btrfs`]: neither the ioctl that
//! sets a directory's project nor `quotactl_fd(2)` has a safe wrapper in the libraries the helper
//! may link. Each call passes a descriptor borrowed for its duration and a pointer to one
//! correctly laid out structure owned by the caller, and nothing else. `quotactl_fd` takes any
//! file on the filesystem rather than its block device, so no device path is ever looked up; it
//! needs Linux 5.14, below the kernel floor of §2 of docs/18-operations.md.

#![expect(unsafe_code, reason = "the project-quota ioctl and quotactl_fd have no safe wrapper")]

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;

/// The quota type of projects (`PRJQUOTA` in `linux/quota.h`).
const PRJQUOTA: u32 = 2;
/// Children inherit the directory's project (`FS_XFLAG_PROJINHERIT`).
const PROJINHERIT: u32 = 0x200;
/// `_IOR('X', 31, struct fsxattr)` and `_IOW('X', 32, struct fsxattr)`, in the generic ioctl
/// encoding that amd64 and arm64 share.
const FS_IOC_FSGETXATTR: libc::c_ulong = 0x801c_581f;
const FS_IOC_FSSETXATTR: libc::c_ulong = 0x401c_5820;
/// Quota block limits are counted in units of this many bytes (`QIF_DQBLKSIZE`).
const BLOCK: u64 = 1024;

/// `struct fsxattr` from `linux/fs.h`.
#[repr(C)]
#[derive(Debug, Default)]
struct FsXattr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    projid: u32,
    cowextsize: u32,
    pad: [u8; 8],
}

/// `struct if_dqinfo` from `linux/quota.h`.
#[repr(C)]
#[derive(Debug, Default)]
struct DqInfo {
    bgrace: u64,
    igrace: u64,
    flags: u32,
    valid: u32,
}

/// What a project holds and may hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes used.
    pub bytes: u64,
    /// Inodes used.
    pub inodes: u64,
    /// The byte limit; zero for none.
    pub limit_bytes: u64,
    /// The inode limit; zero for none.
    pub limit_inodes: u64,
}

/// `QCMD(cmd, PRJQUOTA)`, as the kernel reads it: the bits of an unsigned command.
fn qcmd(cmd: libc::c_int) -> libc::c_long {
    let cmd = u32::from_ne_bytes(cmd.to_ne_bytes());
    libc::c_long::from((cmd << 8) | PRJQUOTA)
}

/// `quotactl_fd(fd, QCMD(cmd, PRJQUOTA), id, addr)`.
fn quotactl<T>(on: &File, cmd: libc::c_int, id: u32, addr: &mut T) -> io::Result<()> {
    let fd = libc::c_long::from(on.as_raw_fd());
    let addr: *mut T = addr;
    // SAFETY: `fd` is open for the whole call, because `on` is borrowed for it. `addr` points to
    // one `T`, exclusively borrowed for the call, and every caller passes the structure the
    // command reads or writes: `libc::dqblk` (`struct if_dqblk`) for Q_GETQUOTA and Q_SETQUOTA,
    // `DqInfo` (`struct if_dqinfo`) for Q_GETINFO, both `repr(C)` with the kernel's layout.
    let r = unsafe {
        libc::syscall(libc::SYS_quotactl_fd, fd, qcmd(cmd), libc::c_long::from(id), addr)
    };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Runs the fsxattr ioctl `request` on `dir` with `x`.
fn fsxattr(dir: &File, request: libc::c_ulong, x: &mut FsXattr) -> io::Result<()> {
    let ptr: *mut FsXattr = x;
    // SAFETY: `dir` is open for the whole call, and `ptr` points to one `FsXattr`, exclusively
    // borrowed for it, which is `struct fsxattr` with the kernel's layout: 28 bytes, as the size
    // encoded in both request numbers says.
    let r = unsafe { libc::ioctl(dir.as_raw_fd(), request, ptr) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Whether the filesystem holding `root` has project quota on.
#[must_use]
pub fn enabled(root: &File) -> bool {
    quotactl(root, libc::Q_GETINFO, 0, &mut DqInfo::default()).is_ok()
}

/// Puts `dir` in project `id`, and everything later created in it.
pub fn assign(dir: &File, id: u32) -> io::Result<()> {
    let mut x = FsXattr::default();
    fsxattr(dir, FS_IOC_FSGETXATTR, &mut x)?;
    if x.projid == id && x.xflags & PROJINHERIT != 0 {
        return Ok(());
    }
    x.projid = id;
    x.xflags |= PROJINHERIT;
    fsxattr(dir, FS_IOC_FSSETXATTR, &mut x)
}

/// The project `dir` is in.
pub fn project_of(dir: &File) -> io::Result<u32> {
    let mut x = FsXattr::default();
    fsxattr(dir, FS_IOC_FSGETXATTR, &mut x)?;
    Ok(x.projid)
}

/// Limits project `id` on the filesystem holding `root` to `bytes` and `inodes`; zero lifts
/// a limit. Bytes are rounded up to the next whole quota block.
pub fn limit(root: &File, id: u32, bytes: u64, inodes: u64) -> io::Result<()> {
    let blocks = bytes.div_ceil(BLOCK);
    let mut q = libc::dqblk {
        dqb_bhardlimit: blocks,
        dqb_bsoftlimit: blocks,
        dqb_curspace: 0,
        dqb_ihardlimit: inodes,
        dqb_isoftlimit: inodes,
        dqb_curinodes: 0,
        dqb_btime: 0,
        dqb_itime: 0,
        dqb_valid: libc::QIF_LIMITS,
    };
    quotactl(root, libc::Q_SETQUOTA, id, &mut q)
}

/// What project `id` holds, as the filesystem accounts it.
pub fn usage(root: &File, id: u32) -> io::Result<Usage> {
    let mut q = libc::dqblk {
        dqb_bhardlimit: 0,
        dqb_bsoftlimit: 0,
        dqb_curspace: 0,
        dqb_ihardlimit: 0,
        dqb_isoftlimit: 0,
        dqb_curinodes: 0,
        dqb_btime: 0,
        dqb_itime: 0,
        dqb_valid: 0,
    };
    quotactl(root, libc::Q_GETQUOTA, id, &mut q)?;
    Ok(Usage {
        bytes: q.dqb_curspace,
        inodes: q.dqb_curinodes,
        limit_bytes: q.dqb_bhardlimit.saturating_mul(BLOCK),
        limit_inodes: q.dqb_ihardlimit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structures_have_the_kernels_sizes() {
        assert_eq!(std::mem::size_of::<FsXattr>(), 28, "the size in the ioctl numbers");
        assert_eq!(std::mem::size_of::<DqInfo>(), 24);
        assert_eq!(std::mem::size_of::<libc::dqblk>(), 72);
        assert_eq!(qcmd(libc::Q_GETQUOTA), 0x8000_0702);
    }
}
