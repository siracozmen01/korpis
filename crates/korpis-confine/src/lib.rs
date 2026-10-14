// SPDX-License-Identifier: AGPL-3.0-or-later

//! The confinement every job the helper starts applies to itself (§6 of
//! docs/02-architecture.md): a backup or restore job (§5.5 of docs/06-storage.md) and an install
//! job (§4 of docs/09-recipes.md). Each is started as root because its work needs some of root;
//! these are the steps by which it gives up the rest, written once so that every job gives it up
//! the same way. Each applies to the calling thread and to every thread or process it starts
//! afterwards, so a job applies them while it is still one thread.

use rustix::thread::{CapabilitySet, CapabilitySets};
use std::path::Path;

/// A capability, by the name the kernel's documentation gives it.
pub type Capability = (CapabilitySet, &'static str);

/// `CAP_CHOWN`: change any file's owner.
pub const CHOWN: Capability = (CapabilitySet::CHOWN, "chown");
/// `CAP_FOWNER`: act as any file's owner.
pub const FOWNER: Capability = (CapabilitySet::FOWNER, "fowner");
/// `CAP_DAC_OVERRIDE`: read, write, and search whatever the mode bits say.
pub const DAC_OVERRIDE: Capability = (CapabilitySet::DAC_OVERRIDE, "dac_override");
/// `CAP_DAC_READ_SEARCH`: read and search whatever the mode bits say.
pub const DAC_READ_SEARCH: Capability = (CapabilitySet::DAC_READ_SEARCH, "dac_read_search");
/// `CAP_FSETID`: keep set-user-ID and set-group-ID bits a write would clear.
pub const FSETID: Capability = (CapabilitySet::FSETID, "fsetid");
/// `CAP_MKNOD`: make device files.
pub const MKNOD: Capability = (CapabilitySet::MKNOD, "mknod");
/// `CAP_SETFCAP`: set file capabilities.
pub const SETFCAP: Capability = (CapabilitySet::SETFCAP, "setfcap");

/// Why a confinement the kernel offers could not be applied.
#[derive(Debug)]
pub struct ConfineError(pub String);

impl std::fmt::Display for ConfineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "confinement: {}", self.0)
    }
}

impl std::error::Error for ConfineError {}

fn err(what: &str, e: impl std::fmt::Display) -> ConfineError {
    ConfineError(format!("{what}: {e}"))
}

/// Moves the calling process into the cgroup at `cgroup`, which the helper made for the job.
///
/// # Errors
/// It could not be joined.
pub fn join_cgroup(cgroup: &Path) -> Result<(), ConfineError> {
    std::fs::write(cgroup.join("cgroup.procs"), "0\n")
        .map_err(|e| err(&format!("joining {}", cgroup.display()), e))
}

/// Keeps `keep` and drops every other capability from every set, the bounding set included, so
/// none can be regained; the names of those kept.
///
/// # Errors
/// A set could not be changed.
pub fn keep_capabilities(keep: &[Capability]) -> Result<Vec<String>, ConfineError> {
    let kept = keep.iter().fold(CapabilitySet::empty(), |all, (c, _)| all | *c);
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if kept.contains(cap) {
            continue;
        }
        match rustix::thread::remove_capability_from_bounding_set(cap) {
            // A number the kernel has no capability for.
            Ok(()) | Err(rustix::io::Errno::INVAL) => {}
            Err(e) => return Err(err("the bounding set", e)),
        }
    }
    let sets =
        CapabilitySets { effective: kept, permitted: kept, inheritable: CapabilitySet::empty() };
    rustix::thread::set_capabilities(None, sets).map_err(|e| err("capabilities", e))?;
    Ok(keep.iter().map(|(_, n)| (*n).to_owned()).collect())
}

/// Sets `no_new_privs`: nothing the job executes, if it executes anything, gains a privilege by
/// a set-user-ID bit or a file capability.
///
/// # Errors
/// It could not be set.
pub fn no_new_privileges() -> Result<(), ConfineError> {
    rustix::thread::set_no_new_privs(true).map_err(|e| err("no_new_privs", e))
}
