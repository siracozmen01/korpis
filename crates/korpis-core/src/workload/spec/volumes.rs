// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes a workload mounts (docs/06-storage.md).

use super::{SpecError, check_name};
use std::collections::BTreeSet;
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
}

fn check_path(field: &str, path: &str) -> Result<(), SpecError> {
    let ok = path.starts_with('/')
        && !path.split('/').any(|p| p == ".." || p == ".")
        && !path.chars().any(char::is_control);
    if ok { Ok(()) } else { Err(SpecError::new(field, "an absolute path without . or ..")) }
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
    }
    Ok(())
}
