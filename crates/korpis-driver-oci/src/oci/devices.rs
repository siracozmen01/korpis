// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a container's devices add to its runtime specification (§4.2 of docs/04-runtimes.md):
//! each node bound from the host and allowed by a cgroup rule after the rule denying every
//! device, each mount as its edit gives it, the environment and groups appended, and each hook at
//! the point it names.

use super::{DeviceRule, Mount, RuntimeSpec, mount};
use crate::devices::Given;
use serde::Serialize;

/// One OCI hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Hook {
    path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    env: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u32>,
}

/// The hooks of a container, by the point each runs at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Hooks {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    create_runtime: Vec<Hook>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    create_container: Vec<Hook>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    start_container: Vec<Hook>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    poststart: Vec<Hook>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    poststop: Vec<Hook>,
}

impl Hooks {
    /// Whether it holds none.
    pub(super) fn is_empty(&self) -> bool {
        [
            &self.create_runtime,
            &self.create_container,
            &self.start_container,
            &self.poststart,
            &self.poststop,
        ]
        .iter()
        .all(|h| h.is_empty())
    }

    /// The hooks run at `point`, an OCI hook point `crate::devices` has checked.
    fn at(&mut self, point: &str) -> Option<&mut Vec<Hook>> {
        match point {
            "createRuntime" => Some(&mut self.create_runtime),
            "createContainer" => Some(&mut self.create_container),
            "startContainer" => Some(&mut self.start_container),
            "poststart" => Some(&mut self.poststart),
            "poststop" => Some(&mut self.poststop),
            _ => None,
        }
    }
}

/// A mount a device's edit gives: a bind when its options say so, whatever type it names.
fn edit_mount(m: &korpis_cdi::spec::Mount) -> Mount {
    let bind = m.options.iter().any(|o| o == "bind" || o == "rbind");
    let kind = m.kind.clone().filter(|k| !k.is_empty() && !bind).unwrap_or_else(|| "bind".into());
    let options: Vec<&str> = m.options.iter().map(String::as_str).collect();
    mount(&m.container_path, &kind, &m.host_path, &options)
}

/// `given` added to `spec`.
pub(super) fn apply(spec: &mut RuntimeSpec, given: &Given) {
    for n in &given.nodes {
        // Bound, never made: the container's user namespace cannot make a node, and `nodev` is
        // left off so the node opens.
        spec.mounts.push(mount(&n.path, "bind", &n.host, &["bind", "nosuid", "noexec"]));
        spec.linux.resources.devices.push(DeviceRule {
            allow: true,
            kind: Some("c"),
            major: Some(n.numbers.0),
            minor: Some(n.numbers.1),
            access: n.access.clone(),
        });
    }
    spec.mounts.extend(given.mounts.iter().map(edit_mount));
    spec.process.env.extend(given.env.iter().cloned());
    spec.process.user.additional_gids.extend(given.gids.iter().copied());
    for h in &given.hooks {
        if let Some(at) = spec.hooks.at(&h.hook_name) {
            at.push(Hook {
                path: h.path.clone(),
                args: h.args.clone(),
                env: h.env.clone(),
                timeout: h.timeout,
            });
        }
    }
}
