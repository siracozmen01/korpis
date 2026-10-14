// SPDX-License-Identifier: AGPL-3.0-or-later

//! A job's confinement (§6 of docs/02-architecture.md, §5.5 of docs/06-storage.md). The helper
//! starts a job as root, because reading every tenant file or restoring every owner needs some of
//! root; the job's first act, before it reads a byte of the volume and while it is still one
//! thread, is to give up all of root it does not need: it joins the cgroup the helper made for it,
//! confines its filesystem access with Landlock to the directory it works in (and to reading the
//! certificate and resolver files a connection needs), confines its connections to the store's
//! port, and drops every capability but its work's from every set, the bounding set included.
//! Each is per thread, and every thread the job starts afterwards inherits them. What the kernel
//! cannot apply is reported as absent, never assumed.

use korpis_proto::korpis::backup::v1 as pb;
use landlock::{
    ABI, Access as _, AccessFs, AccessNet, LandlockStatus, NetPort, PathBeneath, Ruleset,
    RulesetAttr as _, RulesetCreatedAttr as _, RulesetStatus, path_beneath_rules,
};
use rustix::fs::{Mode, OFlags};
use rustix::thread::{CapabilitySet, CapabilitySets};
use std::path::{Path, PathBuf};

/// Files a connection reads: certificate roots, the resolver's configuration, and the libraries
/// the resolver loads.
const SYSTEM: &[&str] = &[
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/usr/share/ca-certificates",
    "/usr/lib/ssl",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/gai.conf",
    "/etc/host.conf",
    "/run/systemd/resolve",
    "/etc/ld.so.cache",
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
];

/// What a job is: which capabilities its work needs, and what it may do in its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// Reads every file of a volume, whoever owns it.
    Backup,
    /// Creates a tree with any owner, mode, attribute, and device.
    Restore,
}

impl Work {
    fn capabilities(self) -> (CapabilitySet, &'static [&'static str]) {
        match self {
            Self::Backup => (CapabilitySet::DAC_READ_SEARCH, &["dac_read_search"]),
            Self::Restore => (
                CapabilitySet::CHOWN
                    | CapabilitySet::FOWNER
                    | CapabilitySet::DAC_OVERRIDE
                    | CapabilitySet::FSETID
                    | CapabilitySet::MKNOD
                    | CapabilitySet::SETFCAP,
                &["chown", "fowner", "dac_override", "fsetid", "mknod", "setfcap"],
            ),
        }
    }
}

/// Where a job may reach.
#[derive(Debug, Clone)]
pub struct Bounds {
    pub work: Work,
    /// The directory it reads or writes.
    pub directory: PathBuf,
    /// Its result's directory, which it writes.
    pub state: PathBuf,
    /// Recipe content it reads, for a restore.
    pub content: Option<PathBuf>,
    /// The cgroup it joins.
    pub cgroup: Option<PathBuf>,
    /// The store's port.
    pub port: u16,
}

/// Why a confinement could not be applied that the kernel offers.
#[derive(Debug, thiserror::Error)]
#[error("confinement: {0}")]
pub struct ConfineError(String);

fn err(e: impl std::fmt::Display) -> ConfineError {
    ConfineError(e.to_string())
}

/// Confines the calling thread, which must be the job's only one, to `b`.
pub fn confine(b: &Bounds) -> Result<pb::Confinement, ConfineError> {
    let mut held = pb::Confinement::default();
    if let Some(cgroup) = &b.cgroup {
        std::fs::write(cgroup.join("cgroup.procs"), "0\n")
            .map_err(|e| err(format!("joining {}: {e}", cgroup.display())))?;
        held.cgroup = true;
    }
    (held.landlock_filesystem, held.landlock_network) = landlock(b)?;
    held.capabilities = drop_capabilities(b.work)?;
    Ok(held)
}

fn existing(paths: &[&Path]) -> Vec<PathBuf> {
    paths.iter().filter(|p| p.exists()).map(|p| p.to_path_buf()).collect()
}

/// The directory at `path`, opened without following a link in its last component: the job's
/// directory may be in a volume a running workload writes, which could put a link where the
/// helper made a directory. Its parents are the helper's.
fn directory(path: &Path) -> Result<std::os::fd::OwnedFd, ConfineError> {
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::open(path, flags, Mode::empty())
        .map_err(|e| err(format!("{}: {e}", path.display())))
}

/// Applies the Landlock ruleset; whether the filesystem and the network rules are enforced.
fn landlock(b: &Bounds) -> Result<(bool, bool), ConfineError> {
    let abi = ABI::V5;
    let (all, read) = (AccessFs::from_all(abi), AccessFs::from_read(abi));
    let here = if b.work == Work::Backup { read } else { all };
    let system: Vec<&Path> = SYSTEM.iter().map(Path::new).collect();
    let mut readable = existing(&system);
    readable.extend(b.content.iter().cloned());
    let created = Ruleset::default()
        .handle_access(all)
        .and_then(|r| r.handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp))
        .and_then(Ruleset::create)
        .map_err(err)?;
    let (work, state) = (directory(&b.directory)?, directory(&b.state)?);
    let created = created
        .add_rule(PathBeneath::new(work, here))
        .and_then(|r| r.add_rule(PathBeneath::new(state, all)))
        .and_then(|r| r.add_rules(path_beneath_rules(&readable, read)))
        .and_then(|r| r.add_rule(NetPort::new(b.port, AccessNet::ConnectTcp)))
        // A resolver that falls back to TCP asks on 53.
        .and_then(|r| r.add_rule(NetPort::new(53, AccessNet::ConnectTcp)))
        .map_err(err)?;
    let status = created.restrict_self().map_err(err)?;
    let enforced = status.ruleset != RulesetStatus::NotEnforced;
    // The network rules came with ABI 4; a kernel with Landlock but not those enforces only the
    // filesystem's.
    let net = matches!(status.landlock, LandlockStatus::Available { effective_abi, .. } if effective_abi >= ABI::V4);
    Ok((enforced, enforced && net))
}

/// Keeps `work`'s capabilities and drops every other from every set; the names kept.
fn drop_capabilities(work: Work) -> Result<Vec<String>, ConfineError> {
    let (keep, names) = work.capabilities();
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if keep.contains(cap) {
            continue;
        }
        match rustix::thread::remove_capability_from_bounding_set(cap) {
            // A number the kernel has no capability for.
            Ok(()) | Err(rustix::io::Errno::INVAL) => {}
            Err(e) => return Err(err(format!("the bounding set: {e}"))),
        }
    }
    let sets =
        CapabilitySets { effective: keep, permitted: keep, inheritable: CapabilitySet::empty() };
    rustix::thread::set_capabilities(None, sets).map_err(|e| err(format!("capabilities: {e}")))?;
    Ok(names.iter().map(|n| (*n).to_owned()).collect())
}
