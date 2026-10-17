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

pub mod kernel;
pub mod network;
pub mod node;
pub mod probe;
#[cfg(test)]
mod tests;

use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;
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

/// What a check found of what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Present as needed.
    Met,
    /// Not needed for what this node is configured to offer, and why.
    NotNeeded(String),
    /// Missing: what the node offers less of without it, and the command that fixes it.
    Lacking {
        /// What is not offered, or not enforced, without it.
        costs: String,
        /// What to run, or do, to have it.
        fix: String,
    },
    /// Not known: why it could not be checked, and what would let it be.
    Unchecked(String),
}

/// One requirement, as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What it is.
    pub name: &'static str,
    /// What needs it.
    pub needed_for: &'static str,
    /// What was found, in words.
    pub found: String,
    /// Whether it is there.
    pub outcome: Outcome,
    /// Whether the node can offer anything without it.
    pub essential: bool,
}

/// What a node is configured to offer, which is what preflight checks against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The runtime driver: `oci` or `memory`.
    pub driver: String,
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
    /// The storage classes the helper declares, or why it could not be asked.
    pub storage: Option<Result<Vec<pb::StorageClassInfo>, String>>,
    /// The block device the volumes are on, as `major:minor`, and the disk it is part of.
    pub volume_disk: Option<(String, String)>,
}

/// Every check, in the order of the table in §2 of docs/18-operations.md.
#[must_use]
pub fn checks(m: &Machine, p: &Probes, offer: &Offer) -> Report {
    let mut all = kernel::checks(m, p);
    all.extend(node::checks(m, p, offer));
    all.extend(network::checks(m, p));
    Report(all)
}

/// What preflight found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report(pub Vec<Check>);

impl Report {
    /// Whether the node has anything to offer: nothing essential is lacking.
    #[must_use]
    pub fn offers_anything(&self) -> bool {
        !self.0.iter().any(|c| c.essential && matches!(c.outcome, Outcome::Lacking { .. }))
    }

    /// The report for a person: each check, what was found, and what to run where it lacks.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.0 {
            let mark = match &c.outcome {
                Outcome::Met => "ok",
                Outcome::NotNeeded(_) => "--",
                Outcome::Lacking { .. } if c.essential => "NO",
                Outcome::Lacking { .. } => "no",
                Outcome::Unchecked(_) => "??",
            };
            let _ = writeln!(out, "[{mark}] {}: {} (for {})", c.name, c.found, c.needed_for);
            match &c.outcome {
                Outcome::Met => {}
                Outcome::NotNeeded(why) | Outcome::Unchecked(why) => {
                    let _ = writeln!(out, "     {why}");
                }
                Outcome::Lacking { costs, fix } => {
                    let _ = writeln!(out, "     without it: {costs}\n     to fix: {fix}");
                }
            }
        }
        let verdict = if self.offers_anything() {
            "This node can run; it offers what is not marked no."
        } else {
            "This node cannot run anything until what is marked NO is fixed."
        };
        let _ = writeln!(out, "\n{verdict} Nothing on this machine was changed.");
        out
    }

    /// The report for a program.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        let checks: Vec<_> = self.0.iter().map(check_json).collect();
        serde_json::json!({ "offers_anything": self.offers_anything(), "checks": checks })
    }
}

fn check_json(c: &Check) -> serde_json::Value {
    let (status, detail) = match &c.outcome {
        Outcome::Met => ("met", serde_json::json!({})),
        Outcome::NotNeeded(why) => ("not_needed", serde_json::json!({ "why": why })),
        Outcome::Lacking { costs, fix } => {
            ("lacking", serde_json::json!({ "costs": costs, "fix": fix }))
        }
        Outcome::Unchecked(why) => ("unchecked", serde_json::json!({ "why": why })),
    };
    serde_json::json!({
        "name": c.name,
        "needed_for": c.needed_for,
        "found": c.found,
        "essential": c.essential,
        "status": status,
        "detail": detail,
    })
}
