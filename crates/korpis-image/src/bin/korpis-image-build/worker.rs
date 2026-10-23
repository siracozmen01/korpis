// SPDX-License-Identifier: AGPL-3.0-or-later

//! The worker: the process that reads a publisher's layers, holding nothing but its own user
//! namespace, whose ids are the workload's 65,536 and whose network is loopback.

use crate::{DRIVE, awaited, signal, unshare};
use korpis_image::layer;
use korpis_image::stream::{self, Next};
use rustix::fs::{Mode, OFlags};
use rustix::thread::UnshareFlags;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::Command;

/// The tree the layers are applied to, in the work directory.
const TREE: &str = "tree";

/// What a filesystem needs beyond its files: inodes, directories, and room, in bytes.
const SLACK: u64 = 32 << 20;

/// The bytes and inodes the tree at `path` takes, counting nothing twice but hard links.
fn measure(path: &Path) -> std::io::Result<(u64, u64)> {
    let (mut bytes, mut inodes) = (0u64, 0u64);
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
            inodes += 1;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok((bytes, inodes))
}

/// The filesystem of what the layers made, by `mke2fs`, with no journal since it is mounted
/// read-only, and every file's owner as this namespace sees it.
fn filesystem(dir: &Path) -> Result<(), String> {
    let tree = dir.join(TREE);
    let (bytes, inodes) = measure(&tree).map_err(|e| format!("measuring the tree: {e}"))?;
    let size_kib = (bytes + bytes / 4 + SLACK) / 1024;
    let status = Command::new("mke2fs")
        .args(["-q", "-F", "-t", "ext4", "-O", "^has_journal", "-m", "0", "-L", "korpis-image"])
        .args(["-E", "root_owner=0:0", "-N", &(inodes + 1024).to_string(), "-d"])
        .arg(&tree)
        .arg(dir.join(DRIVE))
        .arg(format!("{size_kib}k"))
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("mke2fs: {e}"))?;
    if !status.success() {
        return Err(format!("mke2fs failed: {status}"));
    }
    Ok(())
}

/// Applies each layer the first process sends to the tree in `dir`.
fn layers(dir: &Path) -> Result<(), String> {
    let tree_path = dir.join(TREE);
    std::fs::create_dir(&tree_path).map_err(|e| format!("the tree: {e}"))?;
    let root = rustix::fs::open(&tree_path, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map_err(|e| format!("the tree: {e}"))?;
    let mut input = std::io::stdin().lock();
    let mut skipped = 0;
    while let Next::Layer(packing, len) = stream::next(&mut input).map_err(|e| e.to_string())? {
        let mut bytes = (&mut input).take(len);
        let mut tar = stream::unpacked(&mut bytes, packing).map_err(|e| e.to_string())?;
        let applied = layer::apply(&root, &mut tar).map_err(|e| format!("a layer: {e}"))?;
        std::io::copy(&mut tar, &mut std::io::sink()).map_err(|e| e.to_string())?;
        drop(tar);
        std::io::copy(&mut bytes, &mut std::io::sink()).map_err(|e| e.to_string())?;
        skipped += applied.devices_skipped;
    }
    if skipped > 0 {
        tracing::info!(devices = skipped, "device nodes in the image were not made");
    }
    Ok(())
}

/// The worker, building in `dir`.
pub fn main(dir: &Path) -> Result<(), String> {
    // It dies with the first process, which is the one its driver waits for.
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .map_err(|e| e.to_string())?;
    // Groups are kept across a user namespace whether it maps them or not: dropped first.
    rustix::thread::set_thread_groups(&[]).map_err(|e| format!("groups: {e}"))?;
    unshare(UnshareFlags::NEWUSER | UnshareFlags::NEWNET | UnshareFlags::NEWIPC)?;
    signal(&mut std::io::stdout().lock())?;
    awaited(&mut std::io::stdin().lock())?;
    let root = (rustix::fs::Gid::ROOT, rustix::fs::Uid::ROOT);
    rustix::thread::set_thread_res_gid(root.0, root.0, root.0).map_err(|e| format!("gid: {e}"))?;
    rustix::thread::set_thread_res_uid(root.1, root.1, root.1).map_err(|e| format!("uid: {e}"))?;
    rustix::thread::set_no_new_privs(true).map_err(|e| format!("no_new_privs: {e}"))?;
    layers(dir)?;
    filesystem(dir)
}
