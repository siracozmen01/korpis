// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's cores offered for dedication, as stored in the `node` table (§5.2 of
//! docs/05-scheduling.md).

use super::{from_json, to_json};
use korpis_app::ports::StoreResult;
use korpis_core::node::cores::Core;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CoreDoc {
    id: u32,
    cpus: Vec<u32>,
    numa: u32,
}

/// A node's cores, to store.
pub(crate) fn cores_doc(cores: &[Core]) -> StoreResult<Value> {
    let docs: Vec<CoreDoc> =
        cores.iter().map(|c| CoreDoc { id: c.id, cpus: c.cpus.clone(), numa: c.numa }).collect();
    to_json("node cores", &docs)
}

/// A node's stored cores.
pub(crate) fn cores(v: Value) -> StoreResult<Vec<Core>> {
    let docs: Vec<CoreDoc> = from_json("node cores", v)?;
    Ok(docs.into_iter().map(|c| Core { id: c.id, cpus: c.cpus, numa: c.numa }).collect())
}
