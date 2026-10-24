// SPDX-License-Identifier: Apache-2.0

//! A workload's volumes, made as the node's helper makes them, so a driver is given what a node
//! gives it (§3.1 of docs/06-storage.md). For a driver whose volumes are files, a volume is a
//! directory under the volume root, given to the workload's first id; the mount's source is the
//! directory. For one that takes them as block devices, the same directory holds `block.ext4`, an
//! image of the volume's size made sparse and formatted empty as ext4 by `mke2fs`, and says so in
//! the extended attribute `trusted.korpis.volume`, whose value is `block_device`; the mount's
//! source is the image. The suite needs `mke2fs` for such a driver, and root for the attribute.

use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A block device's image, in its volume's directory.
pub const BLOCK_IMAGE: &str = "block.ext4";

/// The attribute on a volume's directory that says it holds a block device, and its value.
pub const BLOCK_MARK: (&str, &[u8]) = ("trusted.korpis.volume", b"block_device");

/// How a driver takes a volume, as it declares in `volume_semantics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directory of files.
    Files,
    /// A block device, of this many bytes.
    Block(u64),
}

/// Makes the volume `name` under `root` as `kind`, for a workload whose first id is `owner`; the
/// source a mount of it names.
///
/// # Errors
/// The directory exists, or could not be made, formatted, or marked.
pub fn make(root: &Path, name: &str, kind: Kind, owner: u32) -> io::Result<PathBuf> {
    let dir = root.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    match kind {
        Kind::Files => {
            let id = Some(rustix::process::Uid::from_raw(owner));
            rustix::fs::chown(&dir, id, Some(rustix::process::Gid::from_raw(owner)))?;
            Ok(dir)
        }
        Kind::Block(size) => {
            let image = dir.join(BLOCK_IMAGE);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&image)?;
            file.set_len(size)?;
            let made = Command::new("mke2fs")
                .args(["-q", "-F", "-t", "ext4", "-m", "0", "-E", "root_owner=0:0"])
                .arg(&image)
                .stdin(Stdio::null())
                .output()?;
            if !made.status.success() {
                let said = String::from_utf8_lossy(&made.stderr).into_owned();
                return Err(io::Error::other(format!("mke2fs {}: {said}", image.display())));
            }
            file.sync_all()?;
            let flags = rustix::fs::XattrFlags::CREATE;
            rustix::fs::setxattr(&dir, BLOCK_MARK.0, BLOCK_MARK.1, flags)?;
            Ok(image)
        }
    }
}

/// Removes the volume `name` under `root`, and everything in it.
///
/// # Errors
/// It could not be removed; one already gone is no error.
pub fn remove(root: &Path, name: &str) -> io::Result<()> {
    match std::fs::remove_dir_all(root.join(name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
