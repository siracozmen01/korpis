// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's filesystem grown to fill its drive (§3.1 of docs/06-storage.md). The helper grows a
//! block device's image when its workload is given more, and never touches the filesystem in it;
//! the guest's own kernel grows the ext4 to the drive's size as it mounts it, online, so nothing
//! on the host ever parses what the guest wrote. A filesystem already as large as its drive is
//! left as it is.
//!
//! It holds this program's second `unsafe` block: the resize is an `ioctl` with no safe form.

use crate::failed;
use rustix::ioctl::{Opcode, Setter, ioctl, opcode};
use std::path::Path;

/// `EXT4_IOC_RESIZE_FS`: the filesystem's new size, in its blocks.
const RESIZE: Opcode = opcode::write::<u64>(b'f', 16);

/// The size in bytes of the drive `device`, as the kernel the guest runs reads it under `sys`.
fn size_of(sys: &Path, device: &str) -> Result<u64, String> {
    let name = Path::new(device).file_name().unwrap_or_default();
    let sectors = std::fs::read_to_string(sys.join("class/block").join(name).join("size"));
    let sectors = sectors.map_err(failed("reading a drive's size"))?;
    let sectors: u64 = sectors.trim().parse().map_err(failed("reading a drive's size"))?;
    Ok(sectors.saturating_mul(512))
}

/// Grows the ext4 mounted at `mount` from the drive `device` to the drive's size, reading the
/// drive's size from the sysfs mounted at `sys`.
pub fn grow(sys: &Path, device: &str, mount: &Path) -> Result<(), String> {
    let bytes = size_of(sys, device)?;
    let stat = rustix::fs::statfs(mount).map_err(failed("reading a volume's filesystem"))?;
    let block = u64::try_from(stat.f_bsize).map_err(failed("a volume's block size"))?;
    let blocks = bytes / block.max(1);
    let dir = std::fs::File::open(mount).map_err(failed("opening a volume"))?;
    #[allow(unsafe_code, reason = "EXT4_IOC_RESIZE_FS has no safe form")]
    // SAFETY: RESIZE is EXT4_IOC_RESIZE_FS, which the kernel defines as _IOW('f', 16, __u64), and
    // the value it reads is the u64 the Setter holds for the call's whole length; the descriptor
    // is the volume's root, open for the call.
    let resized = unsafe { ioctl(&dir, Setter::<RESIZE, u64>::new(blocks)) };
    resized.map_err(failed("growing a volume's filesystem to its drive"))
}
