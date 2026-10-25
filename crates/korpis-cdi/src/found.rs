// SPDX-License-Identifier: AGPL-3.0-or-later

//! The devices a node's spec files describe (§6.1 of docs/05-scheduling.md), each found or why
//! not: a device is found with the host nodes its own edits give and its edits, its file's then
//! its own; an aggregate, a device giving no node of its own, a device whose hook's program
//! anyone but the owner may write, and a name two files of one directory describe are not. A
//! later directory's device replaces an earlier one's of the same name, as CDI's reference
//! implementation orders them.

use crate::file::{Read, hook_program, specs_in};
use crate::spec::{Edits, Spec, SpecDevice, readable};
use korpis_core::node::device::is_cdi_name;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// A device as its spec file describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// Its kind, `vendor.com/kind`.
    pub kind: String,
    /// The host device nodes its own edits give, in order.
    pub nodes: Vec<String>,
    /// Its file's annotations, then its own.
    pub attributes: BTreeMap<String, String>,
    /// Its file's edits, then its own.
    pub edits: Edits,
}

/// A device found, or why not.
pub type Outcome = Result<Found, String>;

/// What a node's spec files describe.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Described {
    /// Each device by id, found or why not.
    pub devices: BTreeMap<String, Outcome>,
    /// Each directory or file not read, by path, and why.
    pub unread: Vec<(String, String)>,
}

/// What the spec files of `dirs` describe, read as `owner`'s alone (§6.5 of
/// docs/05-scheduling.md), a later directory's device replacing an earlier one's.
#[must_use]
pub fn describe(dirs: &[PathBuf], owner: u32) -> Described {
    let mut described = Described::default();
    for dir in dirs {
        let at = dir.display().to_string();
        match specs_in(dir, owner) {
            Ok(files) => {
                let here = directory(&at, files, owner, &mut described.unread);
                described.devices.extend(here);
            }
            Err(why) => described.unread.push((at, format!("the directory is not read: {why}"))),
        }
    }
    described
}

/// The devices the `files` of `dir` describe, by id, a file not read added to `unread`.
fn directory(
    dir: &str,
    files: Vec<Read>,
    owner: u32,
    unread: &mut Vec<(String, String)>,
) -> BTreeMap<String, Outcome> {
    let mut here = BTreeMap::new();
    let devices: Vec<_> = files
        .into_iter()
        .filter_map(|(path, spec)| {
            let at = path.display().to_string();
            spec.and_then(|s| of_spec(&s, owner))
                .map_err(|why| unread.push((at, format!("the file is not read: {why}"))))
                .ok()
        })
        .flatten()
        .collect();
    for (id, f) in devices {
        let f =
            if here.contains_key(&id) { Err(format!("it is described twice in {dir}")) } else { f };
        here.insert(id, f);
    }
    here
}

/// The devices `spec` describes, each found or why not.
///
/// # Errors
/// The file as a whole cannot be read: a version too new, a kind that is not one, no device, or
/// a relative path in the edits it gives with every device.
fn of_spec(spec: &Spec, owner: u32) -> Result<Vec<(String, Outcome)>, String> {
    if !readable(&spec.cdi_version) {
        return Err(format!("CDI version {:?} is not one from 0.3.0 to 1.1", spec.cdi_version));
    }
    if !is_cdi_name(&format!("{}=x", spec.kind)) {
        return Err(format!("{:?} is not a CDI kind", spec.kind));
    }
    if spec.devices.is_empty() {
        return Err("it describes no device".to_owned());
    }
    absolute(&spec.container_edits)?;
    let own: Vec<BTreeSet<String>> = spec
        .devices
        .iter()
        .map(|d| d.container_edits.device_nodes.iter().map(|n| n.on_host().to_owned()).collect())
        .collect();
    Ok(spec
        .devices
        .iter()
        .zip(&own)
        .map(|(d, nodes)| {
            let id = format!("{}={}", spec.kind, d.name);
            let found = if is_cdi_name(&id) {
                device(spec, d, (nodes, &own), owner)
            } else {
                Err("it is not a CDI name".to_owned())
            };
            (id, found)
        })
        .collect())
}

/// One device of `spec`, whose own host nodes are `nodes`, among its file's devices' `all`.
fn device(
    spec: &Spec,
    d: &SpecDevice,
    (nodes, all): (&BTreeSet<String>, &[BTreeSet<String>]),
    owner: u32,
) -> Result<Found, String> {
    absolute(&d.container_edits)?;
    if nodes.is_empty() {
        return Err("its own edits give no device node, so it cannot be told from another".into());
    }
    let smaller: Vec<&BTreeSet<String>> =
        all.iter().filter(|o| !o.is_empty() && o.is_subset(nodes) && *o != nodes).collect();
    let union: BTreeSet<&String> = smaller.iter().flat_map(|o| o.iter()).collect();
    if smaller.len() >= 2 && union.len() == nodes.len() {
        return Err(format!(
            "it is an aggregate of {} devices of its file; a workload asks for a count instead",
            smaller.len()
        ));
    }
    let edits = spec.container_edits.merged(&d.container_edits);
    for hook in &edits.hooks {
        hook_program(&hook.path, owner).map_err(|e| format!("a hook is not run: {e}"))?;
    }
    let mut attributes = spec.annotations.clone();
    attributes.extend(d.annotations.iter().map(|(k, v)| (k.clone(), v.clone())));
    let nodes = nodes.iter().cloned().collect();
    Ok(Found { kind: spec.kind.clone(), nodes, attributes, edits })
}

/// Whether every path `edits` names is absolute.
fn absolute(edits: &Edits) -> Result<(), String> {
    match edits.paths().find(|(_, p)| !p.starts_with('/')) {
        Some((what, p)) => Err(format!("{what} {p:?} is not absolute")),
        None => Ok(()),
    }
}
