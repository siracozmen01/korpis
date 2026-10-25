// SPDX-License-Identifier: AGPL-3.0-or-later

//! From the devices the spec files describe to those the node offers (§6.1 of
//! docs/05-scheduling.md): each with its class, the kind's name where it names one or the
//! operator's mapping, its shares, and within the bounds the node's facts carry; a device of a
//! kind with no class is not offered, and nor is any device the files do not describe as one.

use super::{Catalog, Config};
use korpis_cdi::{Described, Found};
use korpis_core::node::device::Device;
use korpis_core::node::driver::DeviceClass;
use korpis_proto::korpis::v1 as pb;

/// The most devices a node offers, as its facts carry them (korpis-wire's bound).
const MOST: usize = 256;
/// The longest id and reason a device not offered is reported with.
const LONGEST: usize = 512;

/// The catalog of what the spec files describe, as `config` classifies it.
#[must_use]
pub fn catalog(described: Described, config: &Config) -> Catalog {
    let mut unoffered = described.unread;
    let mut devices = Vec::new();
    for (id, f) in described.devices {
        match f.and_then(|f| offered(&id, f, config)) {
            Ok(d) if devices.len() < MOST => devices.push(d),
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
    Catalog { devices, unoffered }
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

/// `f`, offered as `id`.
fn offered(id: &str, f: Found, config: &Config) -> Result<pb::Device, String> {
    let name = f.kind.split_once('/').map_or("", |(_, n)| n);
    let class =
        DeviceClass::named(name).or_else(|| config.classes.get(&f.kind).copied()).ok_or_else(
            || format!("unclassified: {} names no class; map it with --device-class", f.kind),
        )?;
    let device = Device {
        id: id.to_owned(),
        class,
        attributes: f.attributes,
        host_nodes: f.nodes,
        shares: config.shares.get(&f.kind).copied().unwrap_or(0),
    };
    let wire = korpis_wire::node::device_to_wire(&device);
    korpis_wire::node::devices_from_pb("device", std::slice::from_ref(&wire))
        .map_err(|e| e.to_string())?;
    Ok(wire)
}
