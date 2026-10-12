// SPDX-License-Identifier: AGPL-3.0-or-later

//! Translation between the wire (`korpis.v1`) and the domain. Everything a client sends passes
//! through here before it reaches a use case, so every function refuses malformed input rather
//! than defaulting it. Nothing here decides anything.

pub mod audit;
pub mod authz;
pub mod principal;
pub mod scope;
pub mod tenancy;

use super::error::invalid;
use buffa_types::google::protobuf::Timestamp;
use connectrpc::ConnectError;
use korpis_core::authz::Kind;
use korpis_core::id::{Id, IdKind};
use korpis_core::tenancy::Name;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Parses a wire identifier of `kind` from the request field `field`.
pub fn id(kind: IdKind, field: &str, value: &str) -> Result<Uuid, ConnectError> {
    Id::parse(kind, value).map(|id| id.uuid()).map_err(|e| invalid(field, e.to_string()))
}

/// The wire form of an identifier.
#[must_use]
pub fn wire_id(kind: IdKind, uuid: Uuid) -> String {
    Id::from_uuid(kind, uuid).to_string()
}

/// The identifier kind of an object kind.
#[must_use]
pub fn id_kind(kind: Kind) -> IdKind {
    match kind {
        Kind::Organization => IdKind::Organization,
        Kind::Project => IdKind::Project,
        Kind::Workload => IdKind::Workload,
        Kind::Volume => IdKind::Volume,
        Kind::Endpoint => IdKind::Endpoint,
        Kind::Stream => IdKind::Stream,
        Kind::Node => IdKind::Node,
    }
}

/// A name from the request field `field`.
pub fn name(field: &str, value: &str) -> Result<Name, ConnectError> {
    Name::parse(value).map_err(|e| invalid(field, e.to_string()))
}

/// A wire timestamp.
#[must_use]
pub fn timestamp(t: SystemTime) -> Timestamp {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// A time from the request field `field`. Times before 1970 are refused rather than clamped.
pub fn time(field: &str, t: &Timestamp) -> Result<SystemTime, ConnectError> {
    let secs = u64::try_from(t.seconds).map_err(|_| invalid(field, "before 1970"))?;
    let nanos = u32::try_from(t.nanos).map_err(|_| invalid(field, "negative nanoseconds"))?;
    if nanos >= 1_000_000_000 {
        return Err(invalid(field, "nanoseconds out of range"));
    }
    Ok(UNIX_EPOCH + Duration::new(secs, nanos))
}
