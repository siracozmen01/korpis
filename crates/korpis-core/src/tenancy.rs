// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and the projects inside them (§3.1 of docs/01-model.md).

pub mod quota;

use crate::labels::Labels;
use std::time::SystemTime;
use uuid::Uuid;

/// A name for humans: changeable, never a reference (§5 of docs/01-model.md).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(String);

/// Why a name is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a name is 1 to {max} printable characters on one line", max = Name::MAX)]
pub struct NameError;

impl Name {
    /// The longest name, in characters.
    pub const MAX: usize = 128;

    /// Validates a name.
    pub fn parse(s: impl Into<String>) -> Result<Self, NameError> {
        let s = s.into();
        let len = s.chars().count();
        if len == 0 || len > Self::MAX || s.chars().any(char::is_control) {
            return Err(NameError);
        }
        Ok(Self(s))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether an organization may act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizationStatus {
    /// In good standing.
    Active,
    /// Suspended by an ancestor (§3.1 of docs/01-model.md); its workloads are stopped.
    Suspended,
}

/// An organization: the unit of tenancy, delegation, and quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organization {
    /// Immutable identity.
    pub id: Uuid,
    /// For humans.
    pub name: Name,
    /// `None` only for the root, which installation creates (Rule K-14).
    pub parent: Option<Uuid>,
    /// Whether it may act.
    pub status: OrganizationStatus,
    /// Bumped on every change, for compare-and-swap.
    pub version: u64,
    /// When it was created.
    pub created: SystemTime,
}

/// A project: a group of workloads within one organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Immutable identity.
    pub id: Uuid,
    /// The organization it belongs to; never changes.
    pub organization: Uuid,
    /// For humans, unique within the organization.
    pub name: Name,
    /// For selectors.
    pub labels: Labels,
    /// Bumped on every change, for compare-and-swap.
    pub version: u64,
    /// When it was created.
    pub created: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_bounded_printable_single_lines() {
        assert!(Name::parse("customer-a").is_ok());
        assert!(Name::parse("Ünïcode ok").is_ok());
        assert_eq!(Name::parse(""), Err(NameError));
        assert_eq!(Name::parse("two\nlines"), Err(NameError));
        assert_eq!(Name::parse("x".repeat(129)), Err(NameError));
    }
}
