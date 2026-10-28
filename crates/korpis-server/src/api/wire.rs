// SPDX-License-Identifier: AGPL-3.0-or-later

//! The wire mapping (`korpis-wire`), and what only the API needs beside it: turning a refused
//! translation into the structured error of §8 of docs/10-api.md, and the effect log's page
//! cursor.

pub use korpis_wire::{
    audit, authz, duration, duration_from, event, extension, id, migration, name, node, placement,
    plan, principal, provider, recipe, relay, scope, spec, template, tenancy, time, timestamp,
    value, wire_id, workload,
};

use super::error::invalid;
use connectrpc::ConnectError;
use korpis_wire::WireError;

/// A refused translation, as the `request.invalid` error naming the field.
#[must_use]
pub fn refuse(e: WireError) -> ConnectError {
    invalid(&e.field, e.message)
}

/// Translation results in the API's error type.
pub trait Refuse<T> {
    /// The value, or the refusal as a `request.invalid` error.
    fn refuse(self) -> Result<T, ConnectError>;
}

impl<T> Refuse<T> for Result<T, WireError> {
    fn refuse(self) -> Result<T, ConnectError> {
        self.map_err(refuse)
    }
}
