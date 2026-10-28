// SPDX-License-Identifier: AGPL-3.0-or-later

//! Translation between the domain (`korpis-core`) and the wire schema (`korpis.v1`).
//!
//! Everything a client sends passes through here before it reaches a use case, so every function
//! that reads the wire refuses malformed input rather than defaulting it, and names the field it
//! refused. Nothing here decides anything. The control plane's API, its store (for the intent
//! body, which §3.1 of docs/03-state.md keeps in its wire encoding), and later the CLI and the
//! agent share this one mapping, so they cannot disagree about what a message means.

/// Maps a domain enum to a wire enum and back. Reading refuses the unspecified value and any
/// value this build does not know, naming the field.
macro_rules! enum_map {
    ($to:ident, $from:ident, $dom:ty, $pb:ty, $what:literal, { $($d:path => $p:path),+ $(,)? }) => {
        #[doc = concat!("The wire form of ", $what, ".")]
        #[must_use]
        pub fn $to(v: $dom) -> $pb {
            match v {
                $($d => $p,)+
            }
        }

        #[doc = concat!($what, " from the field `field`.")]
        pub fn $from(field: &str, v: buffa::EnumValue<$pb>) -> crate::Result<$dom> {
            match v.as_known() {
                $(Some($p) => Ok($d),)+
                _ => Err(crate::invalid(field, concat!("expected ", $what))),
            }
        }
    };
}

pub mod address;
pub mod assignment;
pub mod audit;
pub mod authz;
pub mod backup;
pub mod binding;
pub mod block;
pub mod body;
pub mod certificate;
pub mod dns;
pub mod domain;
pub mod edge;
pub mod endpoint;
pub mod event;
pub mod extension;
pub mod health;
pub mod identity;
pub mod journal;
pub mod keys;
pub mod metering;
pub mod migration;
pub mod node;
pub mod observation;
pub mod placement;
pub mod plan;
pub mod portal;
pub mod principal;
pub mod provider;
pub mod quota;
pub mod recipe;
pub mod recipe_schema;
pub mod record;
pub mod relay;
pub mod replica;
pub mod repository;
pub mod revocation;
pub mod run;
pub mod scope;
pub mod spec;
pub mod step;
pub mod stream;
pub mod template;
pub mod tenancy;
pub mod value;
pub mod volume;
pub mod workload;

use buffa_types::google::protobuf::{Duration as PbDuration, Timestamp};
use korpis_core::authz::Kind;
use korpis_core::id::{Id, IdKind};
use korpis_core::tenancy::Name;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Why a wire message does not translate: the field, as a path into the request, and what is
/// wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct WireError {
    /// The offending field.
    pub field: String,
    /// What is wrong.
    pub message: String,
}

/// A translation's result.
pub type Result<T> = std::result::Result<T, WireError>;

/// An error at `field`.
pub fn invalid(field: &str, message: impl Into<String>) -> WireError {
    WireError { field: field.to_owned(), message: message.into() }
}

/// Parses a wire identifier of `kind` from the field `field`.
pub fn id(kind: IdKind, field: &str, value: &str) -> Result<Uuid> {
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
        Kind::Repository => IdKind::Repository,
        Kind::Domain => IdKind::Domain,
        Kind::Certificate => IdKind::Certificate,
        Kind::Portal => IdKind::Portal,
    }
}

/// A name from the field `field`.
pub fn name(field: &str, value: &str) -> Result<Name> {
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

/// The last second a wire time may name, 9999-12-31T23:59:59Z: the end of the range the
/// `google.protobuf.Timestamp` it is read from defines, and far inside what every store holds.
const LAST_SECOND: u64 = 253_402_300_799;

/// A time from the field `field`. Times before 1970 are refused rather than clamped, and times
/// after 9999 as outside what a timestamp is.
pub fn time(field: &str, t: &Timestamp) -> Result<SystemTime> {
    let secs = u64::try_from(t.seconds).map_err(|_| invalid(field, "before 1970"))?;
    if secs > LAST_SECOND {
        return Err(invalid(field, "after the year 9999"));
    }
    let nanos = u32::try_from(t.nanos).map_err(|_| invalid(field, "negative nanoseconds"))?;
    if nanos >= 1_000_000_000 {
        return Err(invalid(field, "nanoseconds out of range"));
    }
    Ok(UNIX_EPOCH + Duration::new(secs, nanos))
}

/// A wire duration.
#[must_use]
pub fn duration(d: Duration) -> PbDuration {
    PbDuration {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// A duration from the field `field`. Negative durations are refused.
pub fn duration_from(field: &str, d: &PbDuration) -> Result<Duration> {
    let secs = u64::try_from(d.seconds).map_err(|_| invalid(field, "negative"))?;
    let nanos = u32::try_from(d.nanos).map_err(|_| invalid(field, "negative"))?;
    if nanos >= 1_000_000_000 {
        return Err(invalid(field, "nanoseconds out of range"));
    }
    Ok(Duration::new(secs, nanos))
}

/// A wire count that must fit `T`, from the field `field`.
pub fn fits<T: TryFrom<u64>>(field: &str, n: u64) -> Result<T> {
    T::try_from(n).map_err(|_| invalid(field, format!("{n} is out of range")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_refused_before_1970_and_after_9999() {
        let at = |seconds: i64| Timestamp { seconds, ..Default::default() };
        assert!(time("t", &at(1_700_000_000)).is_ok());
        assert!(time("t", &at(253_402_300_799)).is_ok(), "the last second of 9999");
        assert!(time("t", &at(253_402_300_800)).is_err());
        assert!(time("t", &at(i64::MAX)).is_err(), "a stored time could not hold it");
        assert!(time("t", &at(-1)).is_err());
    }
}
