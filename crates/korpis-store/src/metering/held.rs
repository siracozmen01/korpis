// SPDX-License-Identifier: AGPL-3.0-or-later

//! The seconds each class of device a workload's placement held, as a reading or a record stores
//! them (§2 of docs/15-observability.md): each class once, by its stored name.

use crate::stored::node::DeviceDoc;
use crate::stored::{from_json, to_json};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::metering::DeviceSeconds;

/// One class's seconds, as stored.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct HeldDoc {
    class: DeviceDoc,
    seconds: u64,
}

/// Each class's seconds, to store.
pub(super) fn device_seconds_doc(held: &DeviceSeconds) -> StoreResult<serde_json::Value> {
    let docs: Vec<HeldDoc> =
        held.iter().map(|(c, s)| HeldDoc { class: (*c).into(), seconds: *s }).collect();
    to_json("metering device seconds", &docs)
}

/// Each class's seconds, as stored; a class stored twice is corrupt.
pub(super) fn device_seconds(doc: serde_json::Value) -> StoreResult<DeviceSeconds> {
    let docs: Vec<HeldDoc> = from_json("metering device seconds", doc)?;
    let mut held = DeviceSeconds::new();
    for d in docs {
        if held.insert(d.class.into(), d.seconds).is_some() {
            return Err(StoreError::Corrupt("a device class's seconds stored twice".into()));
        }
    }
    Ok(held)
}
