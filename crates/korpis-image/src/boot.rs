// SPDX-License-Identifier: AGPL-3.0-or-later

//! The boot drive every guest of a node shares (§6 of docs/04-runtimes.md): `korpis-guest` and the
//! directories it mounts on, as an ext4 filesystem the guest's kernel mounts read-only as its
//! root. It is named by the digest of the `korpis-guest` it holds, so a node with a newer one
//! makes a new drive, which guests take at their next boot, and one already running keeps its
//! own. Nothing in it comes from a tenant, so it is made by the driver directly.

use korpis_core::content::digest_of;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The directories `korpis-guest` mounts on: the kernel's `/dev`, the image, the overlay's
/// layer, and the workload's root.
const DIRS: [&str; 4] = ["dev", "lower", "upper", "newroot"];

/// The boot drive for the `korpis-guest` at `guest`, in `dir`: made if the directory has none
/// for that binary.
pub fn drive(guest: &Path, dir: &Path) -> io::Result<PathBuf> {
    let binary = std::fs::read(guest)?;
    let digest = digest_of(&binary);
    let out = dir.join(format!("boot-{}.ext4", digest.trim_start_matches("sha256:")));
    if out.is_file() {
        return Ok(out);
    }
    let work = work_dir(dir)?;
    let tree = work.join("tree");
    for d in DIRS {
        std::fs::create_dir_all(tree.join(d))?;
    }
    let init = tree.join("korpis-guest");
    std::fs::write(&init, &binary)?;
    std::fs::set_permissions(&init, std::fs::Permissions::from_mode(0o755))?;
    let made = work.join("boot.ext4");
    let size_kib = binary.len() / 1024 + 8 * 1024;
    let status = Command::new("mke2fs")
        .args(["-q", "-F", "-t", "ext4", "-O", "^has_journal", "-m", "0", "-L", "korpis-boot"])
        .args(["-E", "root_owner=0:0", "-d"])
        .arg(&tree)
        .arg(&made)
        .arg(format!("{size_kib}k"))
        .stdout(std::process::Stdio::null())
        .status();
    let renamed = match status {
        Ok(s) if s.success() => {
            std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o444))?;
            std::fs::rename(&made, &out)
        }
        Ok(s) => Err(io::Error::other(format!("mke2fs failed: {s}"))),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(&work);
    renamed.map(|()| out)
}

/// A fresh directory in `dir` to make a drive in.
fn work_dir(dir: &Path) -> io::Result<PathBuf> {
    let work = dir.join(format!(".boot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    Ok(work)
}
