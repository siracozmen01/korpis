// SPDX-License-Identifier: AGPL-3.0-or-later

//! The boot drive every guest of a node shares (§6 of docs/04-runtimes.md): `korpis-guest`, the
//! file worker it runs for a file session (`korpis-fsworker`, §4.4), the install job a service
//! guest runs (`korpis-install-job`), and the directories it mounts on, as an ext4 filesystem the
//! guest's kernel mounts read-only as its root. It is named by the digest of the binaries it
//! holds, so a node with a newer one makes a new drive, which guests take at their next boot, and
//! one already running keeps its own. Nothing in it comes from a tenant, so it is made by the
//! driver directly.

use korpis_core::content::digest_of;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// The directories `korpis-guest` mounts on: the kernel's `/dev`, the image, the overlay's
/// layer, and the workload's root.
const DIRS: [&str; 4] = ["dev", "lower", "upper", "newroot"];

/// The programs a boot drive carries, each by where the node keeps it.
#[derive(Debug, Clone, Copy)]
pub struct Programs<'a> {
    /// `korpis-guest`, the guest's first process.
    pub guest: &'a Path,
    /// `korpis-fsworker`, run for each file session.
    pub fsworker: &'a Path,
    /// `korpis-install-job`, run for an install in a service guest (§4 of docs/09-recipes.md).
    pub install_job: &'a Path,
}

/// The boot drive for `programs`, in `dir`: made if the directory has none for those binaries.
pub fn drive(programs: &Programs<'_>, dir: &Path) -> io::Result<PathBuf> {
    let held = [
        ("korpis-guest", std::fs::read(programs.guest)?),
        ("korpis-fsworker", std::fs::read(programs.fsworker)?),
        ("korpis-install-job", std::fs::read(programs.install_job)?),
    ];
    let named: Vec<String> = held.iter().map(|(_, bytes)| digest_of(bytes)).collect();
    let digest = digest_of(named.join("\n").as_bytes());
    let out = dir.join(format!("boot-{}.ext4", digest.trim_start_matches("sha256:")));
    if out.is_file() {
        return Ok(out);
    }
    let work = crate::ext4::work_dir(dir, "boot")?;
    let tree = work.join("tree");
    for d in DIRS {
        std::fs::create_dir_all(tree.join(d))?;
    }
    for (name, bytes) in &held {
        let at = tree.join(name);
        std::fs::write(&at, bytes)?;
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755))?;
    }
    let made = work.join("boot.ext4");
    let size_kib = held.iter().map(|(_, b)| b.len()).sum::<usize>() / 1024 + 8 * 1024;
    let renamed = crate::ext4::make(&tree, &made, size_kib, "korpis-boot").and_then(|()| {
        std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o444))?;
        std::fs::rename(&made, &out)
    });
    let _ = std::fs::remove_dir_all(&work);
    renamed.map(|()| out)
}
