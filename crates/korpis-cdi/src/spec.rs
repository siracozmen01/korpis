// SPDX-License-Identifier: AGPL-3.0-or-later

//! A Container Device Interface spec file, as version 1.1.0 of the specification defines it (§5
//! of docs/research/evidence.md), read as strictly as its reference implementation reads it: a
//! field it does not define refuses the file, so no edit a vendor wrote is silently dropped.

use serde::Deserialize;
use std::collections::BTreeMap;

/// The newest specification version read, as major and minor; any patch of it is the same.
pub const NEWEST: (u64, u64) = (1, 1);

/// One spec file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Spec {
    /// The specification's version the file is written to.
    pub cdi_version: String,
    /// `vendor.com/kind`.
    pub kind: String,
    /// The file's annotations, which every device of it carries.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    /// Its devices; at least one.
    pub devices: Vec<SpecDevice>,
    /// The edits given with every device of the file.
    #[serde(default)]
    pub container_edits: Edits,
}

/// One device of a file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecDevice {
    /// Its name within the kind.
    pub name: String,
    /// Its own annotations.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    /// Its own edits.
    #[serde(default)]
    pub container_edits: Edits,
}

/// The edits CDI makes to a container's spec.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Edits {
    /// Appended to the environment, as `NAME=VALUE`.
    #[serde(default)]
    pub env: Vec<String>,
    /// The device nodes it gives.
    #[serde(default)]
    pub device_nodes: Vec<Node>,
    /// The mounts it makes.
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// The hooks it runs, on the host, as root.
    #[serde(default)]
    pub hooks: Vec<Hook>,
    /// Added to the process's groups; 0 is ignored.
    #[serde(default, rename = "additionalGIDs")]
    pub additional_gids: Vec<u32>,
    /// An Intel RDT class, its settings not read, since no driver applies one.
    #[serde(default)]
    pub intel_rdt: Option<serde_json::Value>,
    /// Network interfaces moved into the container, which no driver does.
    #[serde(default)]
    pub net_devices: Vec<NetDevice>,
}

/// A device node.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    /// Inside the container.
    pub path: String,
    /// On the host; `path` when unset.
    #[serde(default)]
    pub host_path: Option<String>,
    /// `c`, `u`, `b`, or `p`; the host node's when unset.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Its major number; the host node's when unset.
    #[serde(default)]
    pub major: Option<i64>,
    /// Its minor number; the host node's when unset.
    #[serde(default)]
    pub minor: Option<i64>,
    /// Its mode inside the container.
    #[serde(default)]
    pub file_mode: Option<u32>,
    /// The cgroup's access, of `r`, `w`, and `m`; `rwm` when unset.
    #[serde(default)]
    pub permissions: Option<String>,
    /// Its owner inside the container.
    #[serde(default)]
    pub uid: Option<u32>,
    /// Its group inside the container.
    #[serde(default)]
    pub gid: Option<u32>,
}

impl Node {
    /// The node on the host.
    #[must_use]
    pub fn on_host(&self) -> &str {
        self.host_path.as_deref().filter(|p| !p.is_empty()).unwrap_or(&self.path)
    }
}

/// A mount.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    /// On the host.
    pub host_path: String,
    /// Inside the container.
    pub container_path: String,
    /// The filesystem's type; for a bind, a placeholder.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Its options.
    #[serde(default)]
    pub options: Vec<String>,
}

/// A hook, run on the host as root.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hook {
    /// The OCI hook point: `createRuntime`, `createContainer`, `startContainer`, `poststart`,
    /// or `poststop`.
    pub hook_name: String,
    /// Its program; absolute.
    pub path: String,
    /// Its arguments, the first its name.
    #[serde(default)]
    pub args: Vec<String>,
    /// Its environment.
    #[serde(default)]
    pub env: Vec<String>,
    /// Seconds before it is abandoned; none when unset.
    #[serde(default)]
    pub timeout: Option<u32>,
}

/// A network interface moved into the container.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetDevice {
    /// Its name on the host.
    pub host_interface_name: String,
    /// Its name inside the container.
    pub name: String,
}

impl Edits {
    /// Every path the edits name that must be absolute, by what it is.
    pub fn paths(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let nodes = self.device_nodes.iter().flat_map(|n| {
            [("a device node", n.path.as_str()), ("a device node's host path", n.on_host())]
        });
        let mounts = self.mounts.iter().flat_map(|m| {
            [("a mount's host path", m.host_path.as_str()), ("a mount", m.container_path.as_str())]
        });
        nodes.chain(mounts).chain(self.hooks.iter().map(|h| ("a hook", h.path.as_str())))
    }

    /// These edits, then `then`'s, as a runtime merges a file's edits and a device's.
    #[must_use]
    pub fn merged(&self, then: &Self) -> Self {
        fn both<T: Clone>(a: &[T], b: &[T]) -> Vec<T> {
            a.iter().chain(b).cloned().collect()
        }
        let mut additional_gids = both(&self.additional_gids, &then.additional_gids);
        additional_gids.retain(|g| *g != 0);
        Self {
            env: both(&self.env, &then.env),
            device_nodes: both(&self.device_nodes, &then.device_nodes),
            mounts: both(&self.mounts, &then.mounts),
            hooks: both(&self.hooks, &then.hooks),
            additional_gids,
            intel_rdt: then.intel_rdt.clone().or_else(|| self.intel_rdt.clone()),
            net_devices: both(&self.net_devices, &then.net_devices),
        }
    }
}

/// Whether `version`, `major.minor.patch` with no leading `v`, is one this reader reads: from
/// 0.3.0, the first tagged, to [`NEWEST`].
#[must_use]
pub fn readable(version: &str) -> bool {
    let mut parts = version.split('.').map(str::parse::<u64>);
    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(_)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    (major, minor) >= (0, 3) && (major, minor) <= NEWEST
}
