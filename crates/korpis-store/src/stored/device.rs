// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's devices and a placement's holds on them, as stored in the `node` and `placement`
//! tables (§6 of docs/05-scheduling.md).

use super::node::{DeviceDoc, ModeDoc};
use super::{from_json, to_json};
use korpis_app::ports::StoreResult;
use korpis_core::node::device::{Device, DeviceHold, UnofferedDevice};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeDeviceDoc {
    id: String,
    class: DeviceDoc,
    attributes: BTreeMap<String, String>,
    host_nodes: Vec<String>,
    shares: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnofferedDoc {
    id: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HoldDoc {
    device: String,
    mode: ModeDoc,
}

/// A node's devices and those it does not offer, to store.
pub(crate) fn devices_doc(
    devices: &[Device],
    unoffered: &[UnofferedDevice],
) -> StoreResult<(Value, Value)> {
    let devices: Vec<NodeDeviceDoc> = devices
        .iter()
        .map(|d| NodeDeviceDoc {
            id: d.id.clone(),
            class: d.class.into(),
            attributes: d.attributes.clone(),
            host_nodes: d.host_nodes.clone(),
            shares: d.shares,
        })
        .collect();
    let unoffered: Vec<UnofferedDoc> = unoffered
        .iter()
        .map(|u| UnofferedDoc { id: u.id.clone(), reason: u.reason.clone() })
        .collect();
    Ok((to_json("node devices", &devices)?, to_json("node unoffered devices", &unoffered)?))
}

/// A node's stored devices and those it does not offer.
pub(crate) fn devices(
    devices: Value,
    unoffered: Value,
) -> StoreResult<(Vec<Device>, Vec<UnofferedDevice>)> {
    let devices: Vec<NodeDeviceDoc> = from_json("node devices", devices)?;
    let unoffered: Vec<UnofferedDoc> = from_json("node unoffered devices", unoffered)?;
    Ok((
        devices
            .into_iter()
            .map(|d| Device {
                id: d.id,
                class: d.class.into(),
                attributes: d.attributes,
                host_nodes: d.host_nodes,
                shares: d.shares,
            })
            .collect(),
        unoffered.into_iter().map(|u| UnofferedDevice { id: u.id, reason: u.reason }).collect(),
    ))
}

/// A placement's holds, to store.
pub(crate) fn holds_doc(holds: &[DeviceHold]) -> StoreResult<Value> {
    let docs: Vec<HoldDoc> =
        holds.iter().map(|h| HoldDoc { device: h.device.clone(), mode: h.mode.into() }).collect();
    to_json("placement devices", &docs)
}

/// A placement's stored holds.
pub(crate) fn holds(v: Value) -> StoreResult<Vec<DeviceHold>> {
    let docs: Vec<HoldDoc> = from_json("placement devices", v)?;
    Ok(docs.into_iter().map(|h| DeviceHold { device: h.device, mode: h.mode.into() }).collect())
}
