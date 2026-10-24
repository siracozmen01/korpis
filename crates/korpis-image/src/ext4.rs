// SPDX-License-Identifier: AGPL-3.0-or-later

//! An ext4 filesystem made from a tree the driver wrote, for a drive a guest mounts read-only:
//! `mke2fs` copies the tree in as it formats, so the host writes a filesystem and never parses
//! one a guest wrote.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Makes `out`, an ext4 filesystem of `size_kib` holding `tree`, owned by root, labelled `label`,
/// with no journal, since it is never written once made.
///
/// # Errors
/// `mke2fs` could not be run or failed.
pub fn make(tree: &Path, out: &Path, size_kib: usize, label: &str) -> io::Result<()> {
    let status = Command::new("mke2fs")
        .args(["-q", "-F", "-t", "ext4", "-O", "^has_journal", "-m", "0", "-L", label])
        .args(["-E", "root_owner=0:0", "-d"])
        .arg(tree)
        .arg(out)
        .arg(format!("{size_kib}k"))
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("mke2fs failed: {status}")))
    }
}

/// A fresh directory in `dir` to make a `kind` of drive in, its own however many are made at
/// once.
///
/// # Errors
/// It could not be made.
pub fn work_dir(dir: &Path, kind: &str) -> io::Result<PathBuf> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let n = MADE.fetch_add(1, Ordering::Relaxed);
    let work = dir.join(format!(".{kind}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    Ok(work)
}
