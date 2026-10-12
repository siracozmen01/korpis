// SPDX-License-Identifier: AGPL-3.0-or-later

//! The store's own formats for the structured columns, and the conversions between rows and the
//! domain.
//!
//! These documents belong to the store, not to the wire or the domain: a field renamed in the
//! protocol or a struct reshaped in the domain changes a conversion here, never data already
//! written. Every conversion from a row refuses what it cannot read as
//! [`StoreError::Corrupt`] rather than defaulting it.

mod node;
mod plan;
mod scope;
mod time;

pub(crate) use node::{bounds, bounds_doc, drivers, node_docs, storage_classes, taints};
pub(crate) use plan::{ImpactDoc, StepDoc};
pub(crate) use scope::{
    ConditionsDoc, DelegationDoc, ExpressionDoc, ScopeDoc, actions, expression,
};
pub(crate) use time::{from_time, to_time};

use korpis_app::ports::StoreError;
use korpis_core::principal::Principal;

/// A stored principal key.
pub(crate) fn principal(key: &str) -> Result<Principal, StoreError> {
    Principal::from_key(key).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// A stored JSON document.
pub(crate) fn from_json<T: serde::de::DeserializeOwned>(
    what: &str,
    v: serde_json::Value,
) -> Result<T, StoreError> {
    serde_json::from_value(v).map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))
}

/// A document to store.
pub(crate) fn to_json<T: serde::Serialize>(
    what: &str,
    v: &T,
) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(v).map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))
}

/// A stored count, which the schema keeps non-negative.
pub(crate) fn count(what: &str, n: i64) -> Result<u64, StoreError> {
    u64::try_from(n).map_err(|_| StoreError::Corrupt(format!("{what} is negative")))
}

/// A count to store.
pub(crate) fn to_count(what: &str, n: u64) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Corrupt(format!("{what} is out of range")))
}
