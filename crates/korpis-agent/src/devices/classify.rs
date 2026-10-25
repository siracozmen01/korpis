// SPDX-License-Identifier: AGPL-3.0-or-later

//! From the spec files read to the devices a node offers (§6.1 of docs/05-scheduling.md): each
//! device's id, class, attributes, and the host nodes its own edits give; an aggregate, a device
//! giving no node of its own, and a device of a kind with no class are not offered, and neither
//! is a name two files of one directory describe. A later directory's file replaces an earlier
//! one's device of the same name, as CDI's reference implementation does.

use super::cdi::{Edits, Spec, readable};
use super::file::{Read, hook_program};
use super::{Catalog, Config};
use korpis_core::node::device::{Device, is_cdi_name};
use korpis_core::node::driver::DeviceClass;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};

/// The most devices a node offers, as its facts carry them (korpis-wire's bound).
const MOST: usize = 256;
/// The longest id and reason a device not offered is reported with.
const LONGEST: usize = 512;

/// A device as its file describes it, before its class and hooks are checked.
#[derive(Debug)]
struct Found {
    kind: String,
    nodes: BTreeSet<String>,
    attributes: BTreeMap<String, String>,
    edits: driver::DeviceEdits,
    hooks: Vec<String>,
}

/// Each device of `spec` by id, as found or why not.
type Described = Vec<(String, Result<Found, String>)>;

/// The catalog of the spec files of `dirs`, each directory's files or why it is not read, in
/// the order of `config.dirs`.
#[must_use]
pub fn catalog(dirs: Vec<(String, Result<Vec<Read>, String>)>, config: &Config) -> Catalog {
    let mut unoffered = Vec::new();
    let mut found: BTreeMap<String, Result<Found, String>> = BTreeMap::new();
    for (dir, read) in dirs {
        match read {
            Ok(files) => found.extend(directory(&dir, files, &mut unoffered)),
            Err(why) => unoffered.push((dir, format!("the directory is not read: {why}"))),
        }
    }
    let mut devices = Vec::new();
    let mut edits = BTreeMap::new();
    for (id, f) in found {
        match f.and_then(|f| offered(&id, f, config)) {
            Ok((d, e)) if devices.len() < MOST => {
                devices.push(d);
                edits.insert(id, e);
            }
            Ok(_) => unoffered.push((id, format!("the node offers at most {MOST} devices"))),
            Err(why) => unoffered.push((id, why)),
        }
    }
    unoffered.truncate(MOST);
    let unoffered = unoffered
        .into_iter()
        .map(|(id, reason)| pb::UnofferedDevice {
            id: cut(id),
            reason: cut(reason),
            ..Default::default()
        })
        .collect();
    Catalog { devices, unoffered, edits }
}

/// The devices the `files` of `dir` describe, by id, a file not read added to `unoffered`.
fn directory(
    dir: &str,
    files: Vec<Read>,
    unoffered: &mut Vec<(String, String)>,
) -> BTreeMap<String, Result<Found, String>> {
    let mut here = BTreeMap::new();
    let described = files.into_iter().filter_map(|(path, spec)| {
        let at = path.display().to_string();
        spec.and_then(|s| described(&s))
            .map_err(|why| unoffered.push((at, format!("the file is not read: {why}"))))
            .ok()
    });
    for (id, f) in described.flatten().collect::<Vec<_>>() {
        let f =
            if here.contains_key(&id) { Err(format!("it is described twice in {dir}")) } else { f };
        here.insert(id, f);
    }
    here
}

/// `s`, cut to at most [`LONGEST`] bytes at a character's edge.
fn cut(mut s: String) -> String {
    let mut end = s.len().min(LONGEST);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

/// The devices `spec` describes, each found or why not.
///
/// # Errors
/// The file as a whole cannot be read: a version too new, a kind that is not one, no device, or
/// a relative path in the edits it gives with every device.
fn described(spec: &Spec) -> Result<Described, String> {
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
                device(spec, d, nodes, &own)
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
    d: &super::cdi::SpecDevice,
    nodes: &BTreeSet<String>,
    all: &[BTreeSet<String>],
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
    let mut edits = driver::DeviceEdits::default();
    spec.container_edits.merge_into(&mut edits);
    d.container_edits.merge_into(&mut edits);
    let mut attributes = spec.annotations.clone();
    attributes.extend(d.annotations.iter().map(|(k, v)| (k.clone(), v.clone())));
    let hooks = edits.hooks.iter().map(|h| h.path.clone()).collect();
    Ok(Found { kind: spec.kind.clone(), nodes: nodes.clone(), attributes, edits, hooks })
}

/// Whether every path `edits` names is absolute.
fn absolute(edits: &Edits) -> Result<(), String> {
    match edits.paths().find(|(_, p)| !p.starts_with('/')) {
        Some((what, p)) => Err(format!("{what} {p:?} is not absolute")),
        None => Ok(()),
    }
}

/// `f`, offered as `id`, with the edits given with it.
fn offered(
    id: &str,
    f: Found,
    config: &Config,
) -> Result<(pb::Device, driver::DeviceEdits), String> {
    let name = f.kind.split_once('/').map_or("", |(_, n)| n);
    let class =
        DeviceClass::named(name).or_else(|| config.classes.get(&f.kind).copied()).ok_or_else(
            || format!("unclassified: {} names no class; map it with --device-class", f.kind),
        )?;
    for program in &f.hooks {
        hook_program(program, config.owner).map_err(|e| format!("a hook is not run: {e}"))?;
    }
    let device = Device {
        id: id.to_owned(),
        class,
        attributes: f.attributes,
        host_nodes: f.nodes.into_iter().collect(),
        shares: config.shares.get(&f.kind).copied().unwrap_or(0),
    };
    let wire = korpis_wire::node::device_to_wire(&device);
    korpis_wire::node::devices_from_pb("device", std::slice::from_ref(&wire))
        .map_err(|e| e.to_string())?;
    Ok((wire, f.edits))
}
