// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which of a node's devices a workload is given (§6.3 of docs/05-scheduling.md): those free in
//! the mode it asks, those sharing the least hardware first, a device held by another placement
//! or the same hardware as one never given in `passthrough`, and a shared one never past its
//! shares.

use super::Request;
use crate::node::Node;
use crate::node::device::{Device, DeviceHold, DeviceMode};
use crate::workload::spec::devices::DeviceRequest;
use std::collections::BTreeSet;

/// Whether `a` and `b` are the same hardware: one's own host nodes contain the other's (§6.1 of
/// docs/05-scheduling.md), as a GPU and a partition of it, and never two partitions.
#[must_use]
pub fn same_hardware(a: &Device, b: &Device) -> bool {
    let (x, y): (BTreeSet<&str>, BTreeSet<&str>) = (
        a.host_nodes.iter().map(String::as_str).collect(),
        b.host_nodes.iter().map(String::as_str).collect(),
    );
    !x.is_empty() && !y.is_empty() && (x.is_subset(&y) || y.is_subset(&x))
}

/// Whether `d` may be given in `mode` beside `held`, the holds already on its node.
fn free(d: &Device, mode: DeviceMode, held: &[(&Device, DeviceMode)]) -> bool {
    let mut shared = 0u32;
    for (other, how) in held {
        let same = other.id == d.id;
        if same && mode == DeviceMode::Timeslice && *how == DeviceMode::Timeslice {
            shared = shared.saturating_add(1);
        } else if same || same_hardware(other, d) {
            return false;
        }
    }
    match mode {
        DeviceMode::Passthrough => true,
        DeviceMode::Timeslice => shared < d.shares,
        // A partition a hypervisor makes is a `vm` driver's, and no node of this release has
        // one to give (§6.2 of docs/05-scheduling.md).
        DeviceMode::Mediated => false,
    }
}

/// Whether `d` is of the class `r` asks and its selector matches it, its `id` among its
/// attributes.
fn matches(r: &DeviceRequest, d: &Device) -> bool {
    d.class == r.class
        && r.selector.as_ref().is_none_or(|s| {
            let mut labels = d.attributes.clone();
            labels.insert("id".to_owned(), d.id.clone());
            s.matches(&labels)
        })
}

/// The devices of `devices` that `requests` are given, beside `held`, the holds of the node's
/// other placements: each request in turn, from the devices `keep` names first, so a placement
/// following its target in place keeps what it holds, then those sharing the least hardware with
/// others, then the lowest id. Greedy, so a node whose devices fit only another pairing of
/// overlapping selectors is passed over.
///
/// # Errors
/// What the first request that cannot be met finds, for the explanation.
pub fn choose(
    devices: &[Device],
    held: &[DeviceHold],
    requests: &[DeviceRequest],
    keep: &[DeviceHold],
) -> Result<Vec<DeviceHold>, String> {
    let by_id = |id: &str| devices.iter().find(|d| d.id == id);
    let mut taken: Vec<(&Device, DeviceMode)> =
        held.iter().filter_map(|h| by_id(&h.device).map(|d| (d, h.mode))).collect();
    // What a placement keeps first; then the devices that share the least hardware with others,
    // so a partition is given before the whole device it is part of and the most stay free; then
    // the lowest id.
    let sharing =
        |d: &Device| devices.iter().filter(|o| o.id != d.id && same_hardware(o, d)).count();
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    sorted.sort_by_key(|d| sharing(d));
    sorted.sort_by_key(|d| !keep.iter().any(|k| k.device == d.id));
    let mut given = Vec::new();
    for r in requests {
        let mut found = 0u32;
        for d in &sorted {
            if found == r.count {
                break;
            }
            if matches(r, d) && free(d, r.mode, &taken) {
                taken.push((d, r.mode));
                given.push(DeviceHold { device: d.id.clone(), mode: r.mode });
                found += 1;
            }
        }
        if found < r.count {
            let mode = match r.mode {
                DeviceMode::Passthrough => "passthrough",
                DeviceMode::Timeslice => "timeslice",
                DeviceMode::Mediated => "mediated",
            };
            return Err(format!(
                "{found} of {} {} devices free in {mode} that match",
                r.count,
                r.class.as_str()
            ));
        }
    }
    Ok(given)
}

/// The devices `req` is given on `node`, beside what the node's other placements hold, keeping
/// what its current placement there holds: what the filter finds free, and the bind records.
///
/// # Errors
/// What the first request that cannot be met finds.
pub fn given(req: &Request<'_>, node: &Node) -> Result<Vec<DeviceHold>, String> {
    let held = req.devices.get(&node.id).map_or(&[][..], Vec::as_slice);
    let keep = req.current.filter(|c| c.node == node.id).map_or(&[][..], |c| c.devices);
    choose(&node.devices, held, &req.body.devices, keep)
}

#[cfg(test)]
mod tests;
