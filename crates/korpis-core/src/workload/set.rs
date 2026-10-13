// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared sets: a named group of workloads one declarative source owns, in one project (§3.3
//! of docs/01-model.md), and the rule for declaring through one (§5 and §6 of
//! docs/13-surface-cli.md). Drift is derived from what is already stored, never stored itself.

use super::{Intent, Workload};
use crate::principal::Principal;
use crate::tenancy::Name;
use std::time::SystemTime;
use uuid::Uuid;

/// What a set does with a member whose head was declared outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriftMode {
    /// The out-of-band change stands; declaring over it through the set is refused unless the
    /// declaration asks to reconcile.
    #[default]
    Advisory,
    /// Declaring through the set reconciles it back, and records the change it discarded.
    Strict,
}

impl DriftMode {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Strict => "strict",
        }
    }

    /// The mode a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "advisory" => Some(Self::Advisory),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// A declared set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSet {
    /// Immutable identity.
    pub id: Uuid,
    /// The project it and its members live in.
    pub project: Uuid,
    /// Unique within the project.
    pub name: Name,
    /// What it does with drift.
    pub mode: DriftMode,
    /// Incremented on every mode change.
    pub version: u64,
    /// Who declared through it first.
    pub created_by: Principal,
    /// When.
    pub created_at: SystemTime,
}

/// Whether `w`, whose head is `head`, has drifted from its set: it belongs to one and its head
/// was declared outside it.
#[must_use]
pub fn drifted(w: &Workload, head: &Intent) -> bool {
    w.set.is_some() && head.managed_by != w.set
}

/// What declaring through `set` does to an existing workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Through {
    /// An ordinary declaration by the set that owns it.
    Member,
    /// The workload belonged to no set and is taken into this one.
    Adopted,
    /// The head was declared outside the set and says what the declaration says: the set
    /// accepts the out-of-band change as its own.
    DriftAdopted,
    /// The head was declared outside the set and is discarded; the discarded intent is
    /// recorded with its author.
    DriftDiscarded,
}

/// Why declaring through a set is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetRefusal {
    /// The workload belongs to another set; two sets never touch one workload.
    #[error("the workload belongs to another set")]
    OtherSet,
    /// The workload belongs to no set, and the declaration did not ask to adopt it.
    #[error("the workload belongs to no set; declare with adopt to take it in")]
    NotAMember,
    /// The workload drifted in an advisory set, and the declaration differs from the change
    /// that drifted it and did not ask to reconcile.
    #[error("the workload drifted: its head was declared outside the set")]
    Drifted,
}

/// How a declaration through `set` treats workload `w`, whose head is `head`, when the
/// declaration's completed body equals the head's (`same`).
///
/// # Errors
///
/// [`SetRefusal`] when the declaration may not proceed.
pub fn through(
    set: &DeclaredSet,
    w: &Workload,
    head: &Intent,
    same: bool,
    adopt: bool,
    reconcile: bool,
) -> Result<Through, SetRefusal> {
    match w.set {
        Some(s) if s != set.id => Err(SetRefusal::OtherSet),
        None if adopt => Ok(Through::Adopted),
        None => Err(SetRefusal::NotAMember),
        Some(_) if !drifted(w, head) => Ok(Through::Member),
        Some(_) if same => Ok(Through::DriftAdopted),
        Some(_) if set.mode == DriftMode::Strict || reconcile => Ok(Through::DriftDiscarded),
        Some(_) => Err(SetRefusal::Drifted),
    }
}

#[cfg(test)]
mod tests;
