// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes a workload mounts (docs/06-storage.md).

use super::{SpecError, check_name};
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

/// How a volume is mounted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VolumeMode {
    /// Read and write, by this workload only.
    #[default]
    ReadWrite,
    /// Read only.
    ReadOnly,
    /// Read and write, shared with other workloads.
    Shared,
}

/// One volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSpec {
    /// Unique within the workload.
    pub name: String,
    /// The absolute mount path inside the workload.
    pub path: String,
    /// The size to create; zero only when attaching an existing volume.
    pub size_bytes: u64,
    /// The storage class; empty for the node's default.
    pub storage_class: String,
    /// How it is mounted.
    pub mode: VolumeMode,
    /// Attach this existing volume instead of creating one.
    pub existing: Option<Uuid>,
    /// Create it holding what this backup held, restored before the workload first runs on it
    /// (§5.4 of docs/06-storage.md); only for a volume created, never one attached.
    pub from_backup: Option<Uuid>,
    /// Copies of it kept on other nodes (§2.2 of docs/06-storage.md); only for a volume
    /// created, since a volume attached keeps the replicas its own declaration asked for.
    pub replication: Option<Replication>,
}

/// Copies of a volume kept on other nodes, brought up to date on an interval (§2.2 of
/// docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replication {
    /// How many other nodes keep a copy; at least one.
    pub replicas: u32,
    /// How often the copies are brought up to date.
    pub interval: Duration,
}

impl Replication {
    /// The shortest interval: a copy sent more often is mostly the cost of sending it.
    pub const MIN_INTERVAL: Duration = Duration::from_secs(60);
    /// The interval when none is declared.
    pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);
}

/// Where the node keeps what it gives a workload's install worker (§4 of docs/09-recipes.md): no
/// volume is mounted, and no recipe names a path, at or beneath it.
pub const RESERVED: &str = "/.korpis";

/// Whether `path` is at or beneath [`RESERVED`].
#[must_use]
pub fn reserved(path: &str) -> bool {
    path.strip_prefix(RESERVED).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn check_path(field: &str, path: &str) -> Result<(), SpecError> {
    let ok = path.starts_with('/')
        && !path.split('/').any(|p| p == ".." || p == ".")
        && !path.chars().any(char::is_control);
    if !ok {
        return Err(SpecError::new(field, "an absolute path without . or .."));
    }
    if reserved(path) {
        return Err(SpecError::new(field, format!("{RESERVED} is the node's")));
    }
    Ok(())
}

/// Checks every volume and that names and paths are unique.
pub fn validate(volumes: &[VolumeSpec]) -> Result<(), SpecError> {
    let (mut names, mut paths) = (BTreeSet::new(), BTreeSet::new());
    for (i, v) in volumes.iter().enumerate() {
        let at = |f: &str| format!("volumes[{i}].{f}");
        check_name(&at("name"), &v.name)?;
        check_path(&at("path"), &v.path)?;
        if !names.insert(&v.name) {
            return Err(SpecError::new(&at("name"), "declared twice"));
        }
        if !paths.insert(&v.path) {
            return Err(SpecError::new(&at("path"), "mounted twice"));
        }
        if v.existing.is_none() && v.size_bytes == 0 {
            return Err(SpecError::new(&at("size_bytes"), "a new volume needs a size"));
        }
        if v.existing.is_some() && v.from_backup.is_some() {
            return Err(SpecError::new(&at("from_backup"), "a volume attached is not restored"));
        }
        if let Some(r) = v.replication {
            if v.existing.is_some() {
                let why = "a volume attached keeps the replicas it was made with";
                return Err(SpecError::new(&at("replicas"), why));
            }
            if r.replicas == 0 {
                return Err(SpecError::new(&at("replicas"), "at least one"));
            }
            if r.interval < Replication::MIN_INTERVAL {
                return Err(SpecError::new(&at("replication_interval"), "a minute at least"));
            }
        }
    }
    Ok(())
}
