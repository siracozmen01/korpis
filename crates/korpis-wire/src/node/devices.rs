// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's devices and the holds placements keep on them (§6 of docs/05-scheduling.md). Read
//! strictly and bounded: what an agent reports becomes what the scheduler gives workloads.

use super::{device_from_pb, device_to_pb};
use crate::{Result, invalid};
use korpis_core::node::device::{Device, DeviceHold, DeviceMode, UnofferedDevice, is_cdi_name};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;

/// The most devices, and unoffered devices, a node reports.
const MOST_DEVICES: usize = 256;
/// The most host nodes one device gives, and attributes it carries.
const MOST_NODES: usize = 64;
const MOST_ATTRIBUTES: usize = 64;
/// The longest path, attribute, and reason kept.
const LONGEST_PATH: usize = 4096;
const LONGEST_TEXT: usize = 512;

enum_map!(mode_to_pb, mode_from_pb, DeviceMode, pb::DeviceMode, "a device mode", {
    DeviceMode::Passthrough => pb::DeviceMode::DEVICE_MODE_PASSTHROUGH,
    DeviceMode::Mediated => pb::DeviceMode::DEVICE_MODE_MEDIATED,
    DeviceMode::Timeslice => pb::DeviceMode::DEVICE_MODE_TIMESLICE,
});

/// A node's devices as its agent reports them in `field`: at most 256, each a CDI name of its
/// own, a class, one to 64 absolute host nodes, and at most 64 attributes.
///
/// # Errors
/// The first device that breaks a bound, named by its place.
pub fn devices_from_pb(field: &str, v: &[pb::Device]) -> Result<Vec<Device>> {
    if v.len() > MOST_DEVICES {
        return Err(invalid(field, "at most 256 devices"));
    }
    let mut seen = BTreeSet::new();
    v.iter()
        .enumerate()
        .map(|(i, d)| {
            let at = format!("{field}[{i}]");
            if !is_cdi_name(&d.id) || !seen.insert(d.id.as_str()) {
                return Err(invalid(&at, format!("{:?} is not a CDI name of its own", d.id)));
            }
            device_from(&at, d)
        })
        .collect()
}

fn device_from(at: &str, d: &pb::Device) -> Result<Device> {
    let path = |p: &String| p.starts_with('/') && p.len() <= LONGEST_PATH && !p.contains('\0');
    if d.host_nodes.is_empty() || d.host_nodes.len() > MOST_NODES || !d.host_nodes.iter().all(path)
    {
        return Err(invalid(&format!("{at}.host_nodes"), "one to 64 absolute paths"));
    }
    let text = |s: &String| s.len() <= LONGEST_TEXT;
    if d.attributes.len() > MOST_ATTRIBUTES || !d.attributes.iter().all(|(k, v)| text(k) && text(v))
    {
        return Err(invalid(&format!("{at}.attributes"), "at most 64, each at most 512 bytes"));
    }
    Ok(Device {
        id: d.id.clone(),
        class: device_from_pb(&format!("{at}.class"), d.class)?,
        attributes: d.attributes.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        host_nodes: d.host_nodes.clone(),
        shares: d.shares,
    })
}

/// The devices a node reports and does not offer, in `field`: at most 256, each an id and a
/// reason of at most 512 bytes.
///
/// # Errors
/// There are too many, or one is too long.
pub fn unoffered_from_pb(field: &str, v: &[pb::UnofferedDevice]) -> Result<Vec<UnofferedDevice>> {
    if v.len() > MOST_DEVICES {
        return Err(invalid(field, "at most 256 devices"));
    }
    if let Some(i) =
        v.iter().position(|u| u.id.len() > LONGEST_TEXT || u.reason.len() > LONGEST_TEXT)
    {
        return Err(invalid(&format!("{field}[{i}]"), "an id and a reason of at most 512 bytes"));
    }
    Ok(v.iter().map(|u| UnofferedDevice { id: u.id.clone(), reason: u.reason.clone() }).collect())
}

/// The wire form of a node's device.
#[must_use]
pub fn device_to_wire(d: &Device) -> pb::Device {
    pb::Device {
        id: d.id.clone(),
        class: device_to_pb(d.class).into(),
        attributes: d.attributes.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        host_nodes: d.host_nodes.clone(),
        shares: d.shares,
        ..Default::default()
    }
}

/// The wire form of a device a node does not offer.
#[must_use]
pub fn unoffered_to_wire(u: &UnofferedDevice) -> pb::UnofferedDevice {
    pb::UnofferedDevice { id: u.id.clone(), reason: u.reason.clone(), ..Default::default() }
}

/// The wire form of a placement's holds.
#[must_use]
pub fn holds_to_pb(holds: &[DeviceHold]) -> Vec<pb::DeviceHold> {
    holds
        .iter()
        .map(|h| pb::DeviceHold {
            device: h.device.clone(),
            mode: mode_to_pb(h.mode).into(),
            ..Default::default()
        })
        .collect()
}

/// A placement's holds as `field` carries them: each a CDI name and a mode, none twice.
///
/// # Errors
/// A hold names no CDI device, names one twice, or no mode.
pub fn holds_from_pb(field: &str, v: &[pb::DeviceHold]) -> Result<Vec<DeviceHold>> {
    let mut seen = BTreeSet::new();
    v.iter()
        .enumerate()
        .map(|(i, h)| {
            let at = format!("{field}[{i}]");
            if !is_cdi_name(&h.device) || !seen.insert(h.device.as_str()) {
                return Err(invalid(&at, format!("{:?} is not a device held once", h.device)));
            }
            Ok(DeviceHold { device: h.device.clone(), mode: mode_from_pb(&at, h.mode)? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(id: &str) -> pb::Device {
        pb::Device {
            id: id.into(),
            class: pb::DeviceClass::DEVICE_CLASS_GPU.into(),
            host_nodes: vec!["/dev/nvidia0".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_reported_device_reads_back_as_it_was_and_a_bad_one_is_refused() {
        let read = devices_from_pb("d", &[gpu("nvidia.com/gpu=0")]).unwrap();
        assert_eq!(device_to_wire(&read[0]), gpu("nvidia.com/gpu=0"));
        assert!(devices_from_pb("d", &[gpu("gpu0")]).is_err(), "not a CDI name");
        assert!(devices_from_pb("d", &[gpu("a.com/g=0"), gpu("a.com/g=0")]).is_err(), "twice");
        let mut relative = gpu("a.com/g=0");
        relative.host_nodes = vec!["dev/x".into()];
        assert!(devices_from_pb("d", &[relative]).is_err(), "a relative node");
        let mut classless = gpu("a.com/g=0");
        classless.class = pb::DeviceClass::DEVICE_CLASS_UNSPECIFIED.into();
        assert!(devices_from_pb("d", &[classless]).is_err(), "no class");
    }

    #[test]
    fn a_hold_names_a_device_once_with_a_mode() {
        let held = [DeviceHold { device: "a.com/g=0".into(), mode: DeviceMode::Timeslice }];
        assert_eq!(holds_from_pb("h", &holds_to_pb(&held)).unwrap(), held);
        let twice = holds_to_pb(&[held[0].clone(), held[0].clone()]);
        assert!(holds_from_pb("h", &twice).is_err());
        let mut modeless = holds_to_pb(&held);
        modeless[0].mode = pb::DeviceMode::DEVICE_MODE_UNSPECIFIED.into();
        assert!(holds_from_pb("h", &modeless).is_err());
    }
}
