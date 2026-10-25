// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-agent preflight` (§2 of docs/18-operations.md): what this machine lacks of what the
//! node is configured to offer, and the exact command that fixes each, read from `/proc`, `/sys`,
//! and `/dev` and from the helper and the driver's runtime, and never fixed here: a preflight
//! that loads modules or rewrites `sysctl` on someone's machine is a configuration management
//! system. A node that lacks something offers less rather than failing to start, so only a lack
//! that leaves it nothing to offer makes the report a failure.
//!
//! Each check is a function of what was read (`Machine`) and of what had to be asked of the
//! kernel, a command, or a socket (`Probes`), gathered first by `probe`, so every check is
//! tested against a machine that is not this one.

pub mod cores;
pub mod kernel;
pub mod network;
pub mod node;
pub mod probe;
#[cfg(test)]
mod tests;

pub use korpis_core::preflight::{Check, Outcome, Report};
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

/// Where the machine's kernel interfaces are: the real ones, or a test's copy.
#[derive(Debug, Clone)]
pub struct Machine {
    /// `/proc`.
    pub proc: PathBuf,
    /// `/sys`.
    pub sys: PathBuf,
    /// `/dev`.
    pub dev: PathBuf,
}

impl Machine {
    /// This machine.
    #[must_use]
    pub fn this() -> Self {
        Self { proc: "/proc".into(), sys: "/sys".into(), dev: "/dev".into() }
    }

    /// `rel` under `/proc`, trimmed; `None` where it cannot be read.
    #[must_use]
    pub fn proc(&self, rel: &str) -> Option<String> {
        read(&self.proc, rel)
    }

    /// `rel` under `/sys`, trimmed; `None` where it cannot be read.
    #[must_use]
    pub fn sys(&self, rel: &str) -> Option<String> {
        read(&self.sys, rel)
    }
}

fn read(under: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(under.join(rel)).ok().map(|s| s.trim().to_owned())
}

/// What a node is configured to offer, which is what preflight checks against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The runtime driver: `oci` or `memory`.
    pub driver: String,
    /// The physical cores `--dedicated-cores` names for dedication; `None`, every core but the
    /// one holding CPU 0.
    pub dedicated_cores: Option<std::collections::BTreeSet<u32>>,
}

/// Everything preflight asked of things other than files: the kernel, commands, and sockets.
#[derive(Debug, Clone, Default)]
pub struct Probes {
    /// The Landlock ABI the kernel speaks; `None` without Landlock.
    pub landlock_abi: Option<u8>,
    /// What `nft --version` printed, and whether `nft -j list tables` answered JSON.
    pub nft: Option<(String, bool)>,
    /// Whether containerd's socket answered, when the oci driver is configured.
    pub containerd: Option<Result<(), String>>,
    /// The tiers the node's drivers declare, or why one could not be asked; `None` when no
    /// driver process is configured.
    pub tiers: Option<Result<Vec<pb::IsolationTier>, String>>,
    /// The storage classes the helper declares, or why it could not be asked.
    pub storage: Option<Result<Vec<pb::StorageClassInfo>, String>>,
    /// The block device the volumes are on, as `major:minor`, and the disk it is part of.
    pub volume_disk: Option<(String, String)>,
    /// Whether the helper can shape bandwidth, `tc` and the HTB qdisc, or why not.
    pub shaping: Option<Result<(), String>>,
}

/// Every check, in the order of the table in §2 of docs/18-operations.md.
#[must_use]
pub fn checks(m: &Machine, p: &Probes, offer: &Offer) -> Report {
    let mut all = kernel::checks(m, p);
    all.extend(node::checks(m, p, offer));
    all.extend(network::checks(m, p));
    all.extend(cores::checks(m, offer));
    Report(all)
}
