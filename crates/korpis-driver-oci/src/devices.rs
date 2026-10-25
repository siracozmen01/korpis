// SPDX-License-Identifier: AGPL-3.0-or-later

//! The devices a container is given (§4.2 of docs/04-runtimes.md). The agent names each by id,
//! class, and mode, and the driver reads the node's Container Device Interface files itself, as
//! the agent does, and gives a device only with the edits they give it: a supervisor that is
//! compromised can name a device the host described, and never a mount or a hook of its own
//! (Finding 169 of docs/23-walkthroughs.md).
//!
//! A device node is bound from the host node its edit names, since the container is in a user
//! namespace and cannot make one, with a cgroup rule allowing its type and numbers as the host's
//! node has them; runc would bind a node from its container path, which a vendor's file need not
//! name on the host. A node must be a character device, and an edit this driver does not apply
//! (an Intel RDT class, a network interface moved in, a node's mode or owner, which a bound node
//! keeps from the host) refuses the device rather than giving it half.

use crate::validate::Violation;
use korpis_cdi::spec::{Hook, Mount};
use korpis_core::node::device::is_cdi_name;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use rustix::fs::FileType;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The most devices one container is given.
const MOST: usize = 64;
/// The OCI hook points a CDI hook may name.
const HOOK_POINTS: [&str; 5] =
    ["createRuntime", "createContainer", "startContainer", "poststart", "poststop"];

/// Where the node's spec files are, and who owns them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specs {
    /// The spec directories, a later one's device replacing an earlier one's.
    pub dirs: Vec<PathBuf>,
    /// Who must own them, their files, and their hooks' programs: root on a node.
    pub owner: u32,
}

/// A device node bound into the container, and the cgroup rule allowing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// On the host.
    pub host: String,
    /// Inside the container.
    pub path: String,
    /// Its major and minor numbers, as the host's node has them.
    pub numbers: (i64, i64),
    /// The cgroup's access, of `r`, `w`, and `m`.
    pub access: String,
}

/// What a container's devices give it, in the order their edits name them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Given {
    /// Its device nodes.
    pub nodes: Vec<Bound>,
    /// Its mounts.
    pub mounts: Vec<Mount>,
    /// Appended to its environment.
    pub env: Vec<String>,
    /// Added to its process's groups.
    pub gids: Vec<u32>,
    /// Its hooks, run on the host as root.
    pub hooks: Vec<Hook>,
}

/// The devices of `spec`, as text alone: at most 64, each a CDI name given once, of a class, in
/// `passthrough` or `timeslice`, the modes this driver declares.
///
/// # Errors
/// The first that is not.
pub fn lexical(spec: &driver::PreparedSpec) -> Result<(), Violation> {
    let refuse = |why: &str| Err(Violation { field: "devices".into(), why: why.into() });
    if spec.devices.len() > MOST {
        return refuse("too many");
    }
    let mut seen = BTreeSet::new();
    for d in &spec.devices {
        if !is_cdi_name(&d.id) || !seen.insert(d.id.as_str()) {
            return refuse("each a CDI name, given once");
        }
        if d.class.as_known().is_none_or(|c| c == pb::DeviceClass::DEVICE_CLASS_UNSPECIFIED) {
            return refuse("each of a class");
        }
        let passthrough = d.mode == pb::DeviceMode::DEVICE_MODE_PASSTHROUGH;
        if !passthrough && d.mode != pb::DeviceMode::DEVICE_MODE_TIMESLICE {
            return refuse("each in passthrough or timeslice, the modes this driver gives");
        }
    }
    Ok(())
}

/// What the devices `spec` names give, read from the spec files `specs` names now.
///
/// # Errors
/// Why a device cannot be given: the files do not describe it as one, or it needs an edit this
/// driver does not apply, or a node of it is not a character device on the host.
pub fn given(spec: &driver::PreparedSpec, specs: &Specs) -> Result<Given, String> {
    let mut given = Given::default();
    if spec.devices.is_empty() {
        return Ok(given);
    }
    let described = korpis_cdi::describe(&specs.dirs, specs.owner);
    for d in &spec.devices {
        let found = match described.devices.get(&d.id) {
            Some(Ok(f)) => f,
            Some(Err(why)) => return Err(format!("the device {} is not given: {why}", d.id)),
            None => return Err(format!("the device {} is not described on this node", d.id)),
        };
        add(&d.id, &found.edits, &mut given)?;
    }
    Ok(given)
}

/// The edits of the device `id` added to `given`.
fn add(id: &str, edits: &korpis_cdi::spec::Edits, given: &mut Given) -> Result<(), String> {
    let refuse = |what: &str| Err(format!("the device {id} needs {what}, which is not applied"));
    if edits.intel_rdt.is_some() {
        return refuse("an Intel RDT class");
    }
    if !edits.net_devices.is_empty() {
        return refuse("a network interface moved into the container");
    }
    if let Some(h) = edits.hooks.iter().find(|h| !HOOK_POINTS.contains(&h.hook_name.as_str())) {
        return refuse(&format!("a hook at {:?}, not an OCI hook point", h.hook_name));
    }
    let typeless = |m: &Mount| {
        m.kind.as_deref().is_none_or(str::is_empty)
            && !m.options.iter().any(|o| o == "bind" || o == "rbind")
    };
    if edits.mounts.iter().any(typeless) {
        return refuse("a mount of no type that is not a bind");
    }
    for n in &edits.device_nodes {
        if n.file_mode.is_some() || n.uid.is_some() || n.gid.is_some() {
            return refuse("a node's mode or owner, which a node bound from the host keeps");
        }
        given.nodes.push(bound(id, n)?);
    }
    given.mounts.extend(edits.mounts.iter().cloned());
    given.env.extend(edits.env.iter().cloned());
    given.gids.extend(edits.additional_gids.iter().copied());
    given.hooks.extend(edits.hooks.iter().cloned());
    Ok(())
}

/// The node `n` of the device `id`, as the host has it.
fn bound(id: &str, n: &korpis_cdi::spec::Node) -> Result<Bound, String> {
    let host = n.on_host();
    let st = rustix::fs::stat(host).map_err(|e| format!("the device {id}'s node {host}: {e}"))?;
    let char_type = n.kind.as_deref().is_none_or(|k| k == "c" || k == "u");
    if FileType::from_raw_mode(st.st_mode) != FileType::CharacterDevice || !char_type {
        return Err(format!("the device {id}'s node {host} is not a character device"));
    }
    let numbers =
        (i64::from(rustix::fs::major(st.st_rdev)), i64::from(rustix::fs::minor(st.st_rdev)));
    if n.major.is_some_and(|m| m != numbers.0) || n.minor.is_some_and(|m| m != numbers.1) {
        return Err(format!("the device {id}'s node {host} has other numbers than its file says"));
    }
    let access = n.permissions.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "rwm".into());
    if !access.chars().all(|c| matches!(c, 'r' | 'w' | 'm')) {
        return Err(format!("the device {id}'s node {host} asks for access {access:?}"));
    }
    Ok(Bound { host: host.to_owned(), path: n.path.clone(), numbers, access })
}

#[cfg(test)]
mod tests;
