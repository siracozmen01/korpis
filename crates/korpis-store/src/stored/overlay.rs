// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's side of the mesh, as stored in the `node` table (§9.1 of docs/07-networking.md):
//! keys as hexadecimal, pools as CIDR text, and each handshake as seconds since the epoch.

use super::{from_json, to_json};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::node::mesh::{Key, OverlayFacts};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverlayDoc {
    key: String,
    port: u16,
    pools: Vec<String>,
    handshakes: BTreeMap<String, u64>,
}

fn hex(key: &Key) -> String {
    key.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(text: &str) -> StoreResult<Key> {
    let corrupt = || StoreError::Corrupt(format!("{text:?} is no overlay key"));
    let mut key: Key = [0; 32];
    if text.len() != 64 {
        return Err(corrupt());
    }
    for (i, b) in key.iter_mut().enumerate() {
        let pair = text.get(i * 2..i * 2 + 2).ok_or_else(corrupt)?;
        *b = u8::from_str_radix(pair, 16).map_err(|_| corrupt())?;
    }
    Ok(key)
}

/// A node's side of the mesh, to store.
pub(crate) fn overlay_doc(o: &OverlayFacts) -> StoreResult<Value> {
    let handshakes = o
        .handshakes
        .iter()
        .filter_map(|(k, at)| Some((hex(k), at.duration_since(UNIX_EPOCH).ok()?.as_secs())));
    let doc = OverlayDoc {
        key: hex(&o.key),
        port: o.port,
        pools: o.pools.iter().map(ToString::to_string).collect(),
        handshakes: handshakes.collect(),
    };
    to_json("node overlay", &doc)
}

/// A node's stored side of the mesh.
pub(crate) fn overlay(v: Value) -> StoreResult<OverlayFacts> {
    let doc: OverlayDoc = from_json("node overlay", v)?;
    let pools = doc
        .pools
        .iter()
        .map(|p| p.parse().map_err(|_| StoreError::Corrupt(format!("{p:?} is no overlay pool"))));
    let handshakes = doc
        .handshakes
        .iter()
        .map(|(k, secs)| Ok((unhex(k)?, UNIX_EPOCH + Duration::from_secs(*secs))));
    Ok(OverlayFacts {
        key: unhex(&doc.key)?,
        port: doc.port,
        pools: pools.collect::<StoreResult<_>>()?,
        handshakes: handshakes.collect::<StoreResult<_>>()?,
    })
}
