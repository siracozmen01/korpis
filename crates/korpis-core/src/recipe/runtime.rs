// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a recipe runs, and the shape it proposes (§3 of docs/09-recipes.md).

use super::reference::is_digest;
use super::{RecipeError, artifact_path};
use crate::workload::spec::endpoints::{Exposure, Protocol};
use crate::workload::spec::{ConsoleMode, IsolationTier, Lifecycle};

/// Where a target's root filesystem comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSource {
    /// An OCI image, pinned by digest: `registry/repo@sha256:...`.
    Image(String),
    /// A path inside the recipe artifact: an OCI layer unpacked rather than run.
    Rootfs(String),
    /// A disk image inside the recipe artifact, for `vm` and `microvm`.
    DiskImage(String),
}

/// One way to run the recipe: per isolation tier and architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTarget {
    /// The tier.
    pub tier: IsolationTier,
    /// The architectures it runs on, e.g. `amd64`.
    pub arch: Vec<String>,
    /// Its root filesystem.
    pub source: TargetSource,
}

/// The process to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// The command and its arguments; empty to use the image's own.
    pub command: Vec<String>,
    /// The working directory; empty for the image's own.
    pub working_dir: String,
}

/// How the recipe runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRuntime {
    /// The ways it can run; at least one.
    pub targets: Vec<RuntimeTarget>,
    /// The process to start.
    pub entry: Entry,
}

const ARCHES: [&str; 6] = ["amd64", "arm64", "arm", "386", "riscv64", "s390x"];

fn check_target(i: usize, t: &RuntimeTarget) -> Result<(), RecipeError> {
    let at = |f: &str| format!("runtime.targets[{i}].{f}");
    if t.arch.is_empty() || t.arch.iter().any(|a| !ARCHES.contains(&a.as_str())) {
        return Err(RecipeError::new(&at("arch"), format!("one or more of {ARCHES:?}")));
    }
    match &t.source {
        TargetSource::Image(r) => {
            let pinned =
                r.rsplit_once('@').is_some_and(|(name, d)| !name.is_empty() && is_digest(d));
            if !pinned {
                return Err(RecipeError::new(
                    &at("image"),
                    "an image is pinned by digest: name@sha256:...",
                ));
            }
        }
        TargetSource::Rootfs(p) => artifact_path(&at("rootfs"), p)?,
        TargetSource::DiskImage(p) => {
            artifact_path(&at("disk_image"), p)?;
            if t.tier < IsolationTier::MicroVm {
                return Err(RecipeError::new(
                    &at("disk_image"),
                    "a disk image needs a vm or microvm tier",
                ));
            }
        }
    }
    Ok(())
}

impl RecipeRuntime {
    /// Checks every target.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.targets.is_empty() {
            return Err(RecipeError::new("runtime.targets", "at least one"));
        }
        self.targets.iter().enumerate().try_for_each(|(i, t)| check_target(i, t))?;
        if !self.entry.working_dir.is_empty() && !self.entry.working_dir.starts_with('/') {
            return Err(RecipeError::new("runtime.entry.working_dir", "an absolute path"));
        }
        Ok(())
    }

    /// The target for `tier` on `arch`, if the recipe has one.
    #[must_use]
    pub fn target(&self, tier: IsolationTier, arch: &str) -> Option<&RuntimeTarget> {
        self.targets.iter().find(|t| t.tier == tier && t.arch.iter().any(|a| a == arch))
    }
}

/// An endpoint the recipe proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEndpoint {
    /// Its name.
    pub name: String,
    /// What it speaks.
    pub protocol: Protocol,
    /// The port inside the workload.
    pub port: u16,
    /// How it is reached unless the intent says otherwise.
    pub exposure_default: Exposure,
}

/// A volume the recipe proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetVolume {
    /// Its name.
    pub name: String,
    /// Where it is mounted.
    pub path: String,
    /// Its size unless the intent says otherwise.
    pub size_default_bytes: u64,
}

/// What the software needs, and what it runs well with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PresetResources {
    /// Below this it does not run.
    pub memory_min_bytes: u64,
    /// What it runs well with.
    pub memory_recommended_bytes: u64,
    /// Below this it does not run, in millicores.
    pub cpu_min_millis: u64,
    /// What it runs well with, in millicores.
    pub cpu_recommended_millis: u64,
}

/// The workload shape the recipe proposes; every field is overridable by the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    /// How long it runs.
    pub lifecycle: Lifecycle,
    /// Its console.
    pub console: ConsoleMode,
    /// Its endpoints.
    pub endpoints: Vec<PresetEndpoint>,
    /// Its volumes.
    pub volumes: Vec<PresetVolume>,
    /// Its resources.
    pub resources: PresetResources,
}

impl Preset {
    /// Checks names, ports, paths, and that minimums do not exceed recommendations.
    pub fn validate(&self) -> Result<(), RecipeError> {
        for (i, e) in self.endpoints.iter().enumerate() {
            if e.port == 0 || self.endpoints.iter().take(i).any(|o| o.name == e.name) {
                return Err(RecipeError::new(
                    &format!("preset.endpoints[{i}]"),
                    "a unique name and a port",
                ));
            }
        }
        for (i, v) in self.volumes.iter().enumerate() {
            let dup = self.volumes.iter().take(i).any(|o| o.name == v.name || o.path == v.path);
            if dup || !v.path.starts_with('/') {
                return Err(RecipeError::new(
                    &format!("preset.volumes[{i}]"),
                    "a unique name and absolute path",
                ));
            }
        }
        let r = self.resources;
        let over = |min: u64, rec: u64| rec > 0 && min > rec;
        if over(r.memory_min_bytes, r.memory_recommended_bytes)
            || over(r.cpu_min_millis, r.cpu_recommended_millis)
        {
            return Err(RecipeError::new("preset.resources", "a minimum above its recommendation"));
        }
        Ok(())
    }
}
