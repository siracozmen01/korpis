// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's volumes as its driver takes them (§3.1 of docs/06-storage.md). Each is the image of a
//! block device the helper made: named exactly `<volume root>/<volume>/block.ext4`, with no link on
//! the way, in a directory the helper marked as a block device's, so the driver, which is root,
//! acts on nothing else under the volume root. Each is attached to one workload's guests at a time,
//! since two kernels mounting one filesystem corrupt it. It is given to the workload's first id,
//! and later bound into the guest's chroot, through a descriptor of the file checked, so the file
//! given and bound is the one that was checked.

use korpis_core::workload::volume::{BLOCK_IMAGE, BLOCK_MARK};
use rustix::fs::{FileType, Mode, OFlags};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// The image `source` names, if it is a block device's under the volume root `root`, which is
/// canonical; otherwise why not.
pub fn checked(root: &Path, source: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(source);
    let dir = path.parent().unwrap_or(Path::new(""));
    let named = path.file_name().is_some_and(|n| n == BLOCK_IMAGE) && dir.parent() == Some(root);
    if !named || path.canonicalize().ok().as_ref() != Some(&path) {
        return Err(format!("{source} is not a block device's image under the volume root"));
    }
    let mut value = [0u8; 16];
    let marked = rustix::fs::getxattr(dir, BLOCK_MARK.0, &mut value)
        .is_ok_and(|n| value.get(..n) == Some(BLOCK_MARK.1));
    if !marked {
        return Err(format!("{} holds no block device", dir.display()));
    }
    Ok(path)
}

/// The regular file at `path`, opened as itself with `flags`, never through a link.
fn opened(path: &Path, flags: OFlags) -> io::Result<OwnedFd> {
    let fd = rustix::fs::open(path, flags | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
        return Err(io::Error::other(format!("{} is not a regular file", path.display())));
    }
    Ok(fd)
}

/// Gives the image at `path` to `uid`, its guest's own id, readable by it alone.
pub fn give(path: &Path, uid: u32) -> io::Result<()> {
    let fd = opened(path, OFlags::RDONLY | OFlags::NONBLOCK)?;
    let owner = Some(rustix::process::Uid::from_raw(uid));
    rustix::fs::fchown(&fd, owner, Some(rustix::process::Gid::from_raw(uid)))?;
    rustix::fs::fchmod(&fd, Mode::RUSR | Mode::WUSR)?;
    Ok(())
}

/// The image at `path`, held to be bound where `/proc/self/fd` names it.
pub fn held(path: &Path) -> io::Result<OwnedFd> {
    opened(path, OFlags::PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_marked_block_device_s_image_is_taken() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let dir = root.join("wl_1.data");
        std::fs::create_dir(&dir).unwrap();
        let image = dir.join(BLOCK_IMAGE);
        std::fs::write(&image, b"").unwrap();
        let source = image.to_string_lossy().into_owned();
        assert!(checked(&root, &source).is_err(), "unmarked: a volume of files");
        let marking =
            rustix::fs::setxattr(&dir, BLOCK_MARK.0, BLOCK_MARK.1, rustix::fs::XattrFlags::empty());
        if marking.is_err() {
            return; // trusted.* needs CAP_SYS_ADMIN: the rest needs root.
        }
        assert_eq!(checked(&root, &source).unwrap(), image);
        let other = dir.join("other.ext4");
        std::fs::write(&other, b"").unwrap();
        assert!(checked(&root, &other.to_string_lossy()).is_err(), "another name");
        let link = root.join("wl_2.data");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let through = link.join(BLOCK_IMAGE).to_string_lossy().into_owned();
        assert!(checked(&root, &through).is_err(), "a link on the way");
        let nested = dir.join("deeper");
        std::fs::create_dir(&nested).unwrap();
        let deep = nested.join(BLOCK_IMAGE).to_string_lossy().into_owned();
        assert!(checked(&root, &deep).is_err(), "not directly under the root");
    }
}
