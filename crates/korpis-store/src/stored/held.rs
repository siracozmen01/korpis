// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload holds against quota, and what an entry's workloads hold together, as they are
//! kept (question 7 of §10 of docs/05-scheduling.md): amounts per dimension keyed by the
//! dimension's wire name, an unbounded limit as `null` in a holding and as a count in totals.

use super::from_json;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::tenancy::quota::{Dimension, Held, Totals};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
struct HeldDoc {
    reservations: BTreeMap<String, u64>,
    limits: BTreeMap<String, Option<u64>>,
}

#[derive(Serialize, Deserialize)]
struct TotalsDoc {
    reservations: BTreeMap<String, u64>,
    limits: BTreeMap<String, u64>,
    unbounded: BTreeMap<String, u64>,
}

fn named<T: Copy>(map: &BTreeMap<Dimension, T>) -> BTreeMap<String, T> {
    map.iter().map(|(d, n)| (d.as_str().to_owned(), *n)).collect()
}

fn parsed<T>(what: &str, map: BTreeMap<String, T>) -> StoreResult<BTreeMap<Dimension, T>> {
    map.into_iter()
        .map(|(k, n)| {
            Dimension::parse(&k)
                .map(|d| (d, n))
                .ok_or_else(|| StoreError::Corrupt(format!("{what}: dimension {k:?}")))
        })
        .collect()
}

/// A holding to store.
pub(crate) fn held_doc(h: &Held) -> StoreResult<Value> {
    let doc = HeldDoc { reservations: named(&h.reservations), limits: named(&h.limits) };
    super::to_json("held", &doc)
}

/// A stored holding.
pub(crate) fn held(v: Value) -> StoreResult<Held> {
    let doc: HeldDoc = from_json("held", v)?;
    Ok(Held {
        reservations: parsed("held", doc.reservations)?,
        limits: parsed("held", doc.limits)?,
    })
}

/// Totals to store.
pub(crate) fn totals_doc(t: &Totals) -> StoreResult<Value> {
    let doc = TotalsDoc {
        reservations: named(&t.reservations),
        limits: named(&t.limits),
        unbounded: named(&t.unbounded),
    };
    super::to_json("quota totals", &doc)
}

/// Stored totals.
pub(crate) fn totals(v: Value) -> StoreResult<Totals> {
    let doc: TotalsDoc = from_json("quota totals", v)?;
    Ok(Totals {
        reservations: parsed("quota totals", doc.reservations)?,
        limits: parsed("quota totals", doc.limits)?,
        unbounded: parsed("quota totals", doc.unbounded)?,
    })
}
