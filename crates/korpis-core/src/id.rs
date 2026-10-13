// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identifiers on the wire: a type prefix and a UUID, e.g. `wl_0192f3...`.
//!
//! The store keys everything by UUID (version 7, time-ordered). The API shows the same UUID with
//! a prefix naming what it identifies, so an identifier pasted into the wrong field is refused
//! rather than silently looked up as something else. Identifiers are immutable and never reused
//! (§5 of docs/01-model.md).

use std::fmt;
use uuid::Uuid;

/// The kinds of object with wire identifiers, and their prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdKind {
    /// An organization (`org_`).
    Organization,
    /// A project (`prj_`).
    Project,
    /// A workload (`wl_`).
    Workload,
    /// An intent (`int_`).
    Intent,
    /// A plan (`pln_`).
    Plan,
    /// An effect (`eff_`).
    Effect,
    /// A node (`nd_`).
    Node,
    /// A grant (`grt_`).
    Grant,
    /// A volume (`vol_`).
    Volume,
    /// An endpoint (`ep_`).
    Endpoint,
    /// A stream (`str_`).
    Stream,
    /// An API token (`tok_`).
    Token,
    /// A user (`usr_`).
    User,
    /// A long-running operation (`op_`).
    Operation,
    /// A declared set (`set_`).
    Set,
}

impl IdKind {
    /// The prefix the kind is written with.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Organization => "org",
            Self::Project => "prj",
            Self::Workload => "wl",
            Self::Intent => "int",
            Self::Plan => "pln",
            Self::Effect => "eff",
            Self::Node => "nd",
            Self::Grant => "grt",
            Self::Volume => "vol",
            Self::Endpoint => "ep",
            Self::Stream => "str",
            Self::Token => "tok",
            Self::User => "usr",
            Self::Operation => "op",
            Self::Set => "set",
        }
    }
}

/// A wire identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id {
    kind: IdKind,
    uuid: Uuid,
}

/// Why a string is not the identifier that was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseIdError {
    /// Not shaped like an identifier.
    Malformed(String),
    /// An identifier of another kind.
    WrongKind {
        /// The prefix that was expected.
        expected: &'static str,
        /// What was found.
        found: String,
    },
}

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(s) => write!(f, "{s:?} is not a Korpis identifier"),
            Self::WrongKind { expected, found } => {
                write!(f, "expected a {expected}_ identifier, found {found:?}")
            }
        }
    }
}

impl std::error::Error for ParseIdError {}

impl Id {
    /// A fresh, time-ordered identifier.
    #[must_use]
    pub fn generate(kind: IdKind) -> Self {
        Self::from_uuid(kind, Uuid::now_v7())
    }

    /// The identifier of `uuid` as a `kind`.
    #[must_use]
    pub fn from_uuid(kind: IdKind, uuid: Uuid) -> Self {
        Self { kind, uuid }
    }

    /// What it identifies.
    #[must_use]
    pub fn kind(&self) -> IdKind {
        self.kind
    }

    /// The UUID the store keys by.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// Parses `s`, which must carry `kind`'s prefix.
    pub fn parse(kind: IdKind, s: &str) -> Result<Self, ParseIdError> {
        let (prefix, rest) =
            s.split_once('_').ok_or_else(|| ParseIdError::Malformed(s.to_owned()))?;
        if prefix != kind.prefix() {
            return Err(ParseIdError::WrongKind { expected: kind.prefix(), found: s.to_owned() });
        }
        if rest.len() != 32 {
            return Err(ParseIdError::Malformed(s.to_owned()));
        }
        let uuid = Uuid::try_parse(rest).map_err(|_| ParseIdError::Malformed(s.to_owned()))?;
        Ok(Self::from_uuid(kind, uuid))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.kind().prefix(), self.uuid.simple())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_the_wrong_kind() {
        let id = Id::generate(IdKind::Workload);
        let s = id.to_string();
        assert!(s.starts_with("wl_") && s.len() == 35);
        assert_eq!(Id::parse(IdKind::Workload, &s), Ok(id));
        assert!(matches!(Id::parse(IdKind::Project, &s), Err(ParseIdError::WrongKind { .. })));
        assert!(Id::parse(IdKind::Workload, "wl_123").is_err());
        assert!(Id::parse(IdKind::Workload, "nope").is_err());
    }
}
