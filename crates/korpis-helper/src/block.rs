// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block-device volume (§3.1 of docs/06-storage.md): a volume directory, made and limited as
//! any other, holding one image the guest owns as its disk. The helper makes the image sparse at
//! the volume's size and formats it empty as ext4 with `mke2fs`, which reads nothing a tenant
//! wrote, and never mounts it: what the guest later writes in it is parsed by the guest's kernel
//! alone (§4.4 of docs/04-runtimes.md). A larger size grows the image, and the guest's kernel
//! grows its filesystem to fill it when it next mounts it; a smaller one is refused, since only
//! the guest could shrink what it holds. The directory says what it is in an extended attribute
//! in the `trusted` namespace, which no workload can set, so a volume of one kind is never taken
//! for the other.

use crate::error::{HelperError, HelperResult};
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// The image, in its volume's directory.
pub const IMAGE: &str = korpis_core::workload::volume::BLOCK_IMAGE;

/// The attribute that says a volume is a block device, and its value.
const KIND: &str = korpis_core::workload::volume::BLOCK_MARK.0;
const BLOCK: &[u8] = korpis_core::workload::volume::BLOCK_MARK.1;

/// The most a guest's own filesystem keeps beyond the image's bytes in the volume's quota: the
/// image file's own extent blocks, a sliver of its size.
#[must_use]
pub fn quota_for(size: u64) -> u64 {
    size.saturating_add((size / 64).max(1 << 20))
}

/// `dir`, the volume `volume`'s directory, unless it holds a block device, whose files only its
/// guest's kernel may parse (§4.4 of docs/04-runtimes.md): a file session or an install reaches
/// them through a worker in the guest, which is not built yet, and is refused rather than shown
/// the image.
pub fn files_of(volume: &str, dir: PathBuf) -> HelperResult<PathBuf> {
    if is_block(&dir) {
        let why = format!("volume {volume} is a block device: its files are reached in its guest");
        return Err(HelperError::Refused(why));
    }
    Ok(dir)
}

/// Whether the volume directory at `dir` holds a block device.
#[must_use]
pub fn is_block(dir: &Path) -> bool {
    let mut value = [0u8; 16];
    rustix::fs::getxattr(dir, KIND, &mut value).is_ok_and(|n| value.get(..n) == Some(BLOCK))
}

/// Makes the image of `size` bytes in the new volume directory `dir`, formatted empty, and says
/// what the directory holds; where the image is.
pub fn make(dir: &Path, size: u64) -> HelperResult<PathBuf> {
    if size == 0 {
        return Err(HelperError::Failed("a block device needs a size".into()));
    }
    let image = dir.join(IMAGE);
    // Readable by its owner alone: the driver gives it to the guest's own id.
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&image)?;
    file.set_len(size)?;
    let made = std::process::Command::new("mke2fs")
        .args(["-q", "-F", "-t", "ext4", "-m", "0", "-E", "root_owner=0:0"])
        .arg(&image)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()?;
    if !made.status.success() {
        let said = String::from_utf8_lossy(&made.stderr).into_owned();
        return Err(HelperError::Failed(format!("formatting {}: {said}", image.display())));
    }
    file.sync_all()?;
    rustix::fs::setxattr(dir, KIND, BLOCK, rustix::fs::XattrFlags::CREATE)
        .map_err(io::Error::from)?;
    Ok(image)
}

/// The image in the block-device volume at `dir`, grown to `size` if that is larger; a smaller
/// size is refused, and none keeps the size it has.
pub fn resize(dir: &Path, size: u64) -> HelperResult<PathBuf> {
    let image = dir.join(IMAGE);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(&image)?;
    let now = file.metadata()?.len();
    if size > now {
        file.set_len(size)?;
        file.sync_all()?;
    } else if size != 0 && size < now {
        return Err(HelperError::Failed(format!(
            "a block device cannot shrink, from {now} bytes to {size}: only its guest could"
        )));
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quota_leaves_room_for_the_image_s_own_blocks() {
        assert_eq!(quota_for(16 << 20), (16 << 20) + (1 << 20));
        assert_eq!(quota_for(1 << 30), (1 << 30) + (16 << 20));
    }
}
