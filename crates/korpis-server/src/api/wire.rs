// SPDX-License-Identifier: AGPL-3.0-or-later

//! The wire mapping (`korpis-wire`), and what only the API needs beside it: turning a refused
//! translation into the structured error of §8 of docs/10-api.md, and the effect log's page
//! cursor.

pub use korpis_wire::{
    audit, authz, duration, duration_from, event, extension, id, name, node, placement, plan,
    principal, provider, recipe, relay, scope, spec, template, tenancy, time, timestamp, value,
    wire_id, workload,
};

use super::error::invalid;
use connectrpc::ConnectError;
use korpis_app::ports::EffectCursor;
use korpis_wire::WireError;
use std::time::{Duration, UNIX_EPOCH};
use uuid::Uuid;

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

/// The log is ordered by (ingest time, id); its page token carries both, as microseconds since
/// the epoch and the identifier.
#[must_use]
pub fn encode_cursor(c: EffectCursor) -> String {
    let micros = c.at.duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
    format!("{micros}.{}", c.id.simple())
}

/// A page token from the request.
pub fn decode_cursor(s: &str) -> Result<EffectCursor, ConnectError> {
    let bad = || invalid("page.page_token", "malformed");
    let (at, id) = s.split_once('.').ok_or_else(bad)?;
    let micros: u64 = at.parse().map_err(|_| bad())?;
    Ok(EffectCursor {
        at: UNIX_EPOCH + Duration::from_micros(micros),
        id: Uuid::try_parse(id).map_err(|_| bad())?,
    })
}
