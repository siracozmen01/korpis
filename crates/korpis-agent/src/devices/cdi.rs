// SPDX-License-Identifier: AGPL-3.0-or-later

//! A Container Device Interface spec file, as version 1.1.0 of the specification defines it (§5
//! of docs/research/evidence.md), read as strictly as its reference implementation reads it: a
//! field it does not define refuses the file, so no edit a vendor wrote is silently dropped.

use korpis_proto::korpis::driver::v1 as driver;
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
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Edits {
    #[serde(default)]
    env: Vec<String>,
    /// The device nodes it gives.
    #[serde(default)]
    pub device_nodes: Vec<Node>,
    #[serde(default)]
    mounts: Vec<Mount>,
    /// The hooks it runs.
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default, rename = "additionalGIDs")]
    additional_gids: Vec<u32>,
    /// An Intel RDT class; its settings are not read, since no driver applies one.
    #[serde(default)]
    intel_rdt: Option<serde_json::Value>,
    #[serde(default)]
    net_devices: Vec<NetDevice>,
}

/// A device node.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    /// Inside the container.
    pub path: String,
    /// On the host; `path` when unset.
    #[serde(default)]
    pub host_path: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    major: Option<i64>,
    #[serde(default)]
    minor: Option<i64>,
    #[serde(default)]
    file_mode: Option<u32>,
    #[serde(default)]
    permissions: Option<String>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
}

impl Node {
    /// The node on the host.
    #[must_use]
    pub fn on_host(&self) -> &str {
        self.host_path.as_deref().filter(|p| !p.is_empty()).unwrap_or(&self.path)
    }
}

/// A mount.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    /// On the host.
    pub host_path: String,
    /// Inside the container.
    pub container_path: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    options: Vec<String>,
}

/// A hook, run on the host as root.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hook {
    /// The OCI hook point.
    pub hook_name: String,
    /// Its program; absolute.
    pub path: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    timeout: Option<u32>,
}

/// A network interface moved into the container.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NetDevice {
    host_interface_name: String,
    #[allow(dead_code, reason = "read so a file naming it is valid; no driver moves one")]
    name: String,
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

    /// These edits appended to `into`, as a runtime merges a file's edits and then a device's.
    pub fn merge_into(&self, into: &mut driver::DeviceEdits) {
        into.env.extend(self.env.iter().cloned());
        into.device_nodes.extend(self.device_nodes.iter().map(|n| driver::DeviceNode {
            path: n.path.clone(),
            host_path: n.host_path.clone().unwrap_or_default(),
            r#type: n.kind.clone().unwrap_or_default(),
            major: n.major,
            minor: n.minor,
            file_mode: n.file_mode,
            permissions: n.permissions.clone().unwrap_or_default(),
            uid: n.uid,
            gid: n.gid,
            ..Default::default()
        }));
        into.mounts.extend(self.mounts.iter().map(|m| driver::DeviceMount {
            host_path: m.host_path.clone(),
            container_path: m.container_path.clone(),
            r#type: m.kind.clone().unwrap_or_default(),
            options: m.options.clone(),
            ..Default::default()
        }));
        into.hooks.extend(self.hooks.iter().map(|h| driver::DeviceHook {
            hook_name: h.hook_name.clone(),
            path: h.path.clone(),
            args: h.args.clone(),
            env: h.env.clone(),
            timeout: h.timeout,
            ..Default::default()
        }));
        into.additional_gids.extend(self.additional_gids.iter().filter(|g| **g != 0));
        into.intel_rdt |= self.intel_rdt.is_some();
        into.net_devices.extend(self.net_devices.iter().map(|d| d.host_interface_name.clone()));
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
