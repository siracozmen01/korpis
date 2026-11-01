// SPDX-License-Identifier: AGPL-3.0-or-later

//! The user a receiver becomes, and how it gives up root to become it.

use korpis_core::workload::assignment::{FIRST_WORKLOAD_ID, IDS_PER_WORKLOAD, IdRange};
use rustix::process::{Gid, Uid};
use rustix::thread::{CapabilitySet, CapabilitySets};
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt as _;

/// The ids a receiver may run as: the last range of [`IDS_PER_WORKLOAD`] ids above
/// [`FIRST_WORKLOAD_ID`], which reaches `u32::MAX` and so is never a workload's, and which no
/// host user takes; the first, and how many, stopping below `u32::MAX - 1`.
fn ids() -> (u32, u32) {
    let first = FIRST_WORKLOAD_ID + IdRange::slots() * IDS_PER_WORKLOAD;
    (first, u32::MAX - 1 - first)
}

/// The lowest receiver id no live process holds as any of its user ids, so two receivers running
/// at once, or one a restarted helper no longer knows of, are never one user and cannot signal or
/// trace each other. The helper answers one request at a time and a child takes its id before
/// `spawn` returns, so no other receiver can take the id between this reading and that.
///
/// # Errors
///
/// `/proc` cannot be read, or every id is held.
pub(super) fn free_id() -> std::io::Result<u32> {
    let (first, count) = ids();
    let range = first..first + count;
    let mut held = BTreeSet::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        if !entry.file_name().as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        // A process that ended meanwhile has no status left to read.
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else { continue };
        held.extend(user_ids(&status).filter(|id| range.contains(id)));
    }
    range.clone().find(|id| !held.contains(id)).ok_or_else(|| {
        std::io::Error::other(format!("all {count} receiver ids are held by running processes"))
    })
}

/// The real, effective, saved and filesystem user ids a `/proc/<pid>/status` names.
fn user_ids(status: &str) -> impl Iterator<Item = u32> + '_ {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .into_iter()
        .flat_map(|ids| ids.split_whitespace().filter_map(|id| id.parse().ok()))
}

/// Gives up root: the child becomes `id`, user and group, with no other groups, keeping `kept`
/// as ambient capabilities through its exec and nothing else, with root's special treatment
/// locked off and `no_new_privs` set. A user that is not root signals no process of the host's.
/// Allocates nothing, so it may run between fork and exec.
pub(super) fn become_receiver(id: u32, kept: CapabilitySet) -> rustix::io::Result<()> {
    use rustix::thread::CapabilitiesSecureBits as Bits;
    rustix::thread::set_capabilities_secure_bits(
        Bits::NO_ROOT
            | Bits::NO_ROOT_LOCKED
            | Bits::NO_SETUID_FIXUP
            | Bits::NO_SETUID_FIXUP_LOCKED
            | Bits::KEEP_CAPS_LOCKED,
    )?;
    drop_bounding(kept)?;
    rustix::thread::set_thread_groups(&[])?;
    let (uid, gid) = (Uid::from_raw_unchecked(id), Gid::from_raw_unchecked(id));
    rustix::thread::set_thread_res_gid(gid, gid, gid)?;
    rustix::thread::set_thread_res_uid(uid, uid, uid)?;
    let sets = CapabilitySets { effective: kept, permitted: kept, inheritable: kept };
    rustix::thread::set_capabilities(None, sets)?;
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if kept.contains(cap) {
            rustix::thread::configure_capability_in_ambient_set(cap, true)?;
        }
    }
    rustix::thread::set_no_new_privs(true)
}

/// Drops every capability but `kept` from the bounding set, so nothing executed regains one.
fn drop_bounding(kept: CapabilitySet) -> rustix::io::Result<()> {
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if kept.contains(cap) {
            continue;
        }
        match rustix::thread::remove_capability_from_bounding_set(cap) {
            // A number the kernel has no capability for.
            Ok(()) | Err(rustix::io::Errno::INVAL) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
