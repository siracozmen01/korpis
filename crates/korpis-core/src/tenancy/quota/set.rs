// SPDX-License-Identifier: AGPL-3.0-or-later

//! A quota set as its owner's parent sets it (§5.1 of docs/05-scheduling.md, §3.1 of
//! docs/01-model.md): its entries, and the overcommit it permits its own children's limits. What
//! a set may say is checked against the set above it and the sets below it: the reservations its
//! children are given, entry by entry of the same shape, sum within its own, never
//! oversubscribed; their limits sum within its own times the ratio it was permitted; and a
//! project's entries, a sub-limit within its organization's, each fit the organization's.

use super::{Bounds, Dimension, QuotaEntry};
use crate::labels::LabelSelector;
use crate::node::driver::DeviceClass;
use std::collections::BTreeMap;

/// The dimensions that are exclusive by nature and never overcommitted (§5.3 of
/// docs/05-scheduling.md).
pub const EXCLUSIVE: [Dimension; 7] = [
    Dimension::DedicatedIpv4,
    Dimension::NodePorts,
    Dimension::Devices(DeviceClass::Gpu),
    Dimension::Devices(DeviceClass::Usb),
    Dimension::Devices(DeviceClass::Pci),
    Dimension::Devices(DeviceClass::Tpm),
    Dimension::Devices(DeviceClass::Serial),
];

/// An owner's quota set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaSet {
    /// Its entries, at most one of each shape.
    pub entries: Vec<QuotaEntry>,
    /// Per dimension, the percent of its own limits its children's limits may sum to; a
    /// dimension absent permits none beyond them.
    pub child_overcommit: BTreeMap<Dimension, u32>,
}

/// What an entry counts: the workloads its selector matches, from their declaration or on the
/// nodes it names. Entries of one shape, at two levels of the tree, are the same promise made at
/// each.
pub type Shape<'a> = (Option<&'a LabelSelector>, Option<&'a LabelSelector>);

/// The shape of `e`.
#[must_use]
pub fn shape(e: &QuotaEntry) -> Shape<'_> {
    (e.selector.as_ref(), e.nodes.as_ref())
}

/// Why a set may not be said.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    /// Two entries of one shape.
    #[error("entry {0} has the shape of an earlier one: merge them")]
    Repeated(usize),
    /// A reservation above the limit beside it.
    #[error("entry {entry} reserves more {} than its limit", dimension.as_str())]
    ReservedAboveLimit {
        /// The entry's index.
        entry: usize,
        /// The dimension.
        dimension: Dimension,
    },
    /// Overcommit on a dimension that is never overcommitted, or below one hundred percent.
    #[error("{} overcommit of {percent}% is not possible: {why}", dimension.as_str())]
    Overcommit {
        /// The dimension.
        dimension: Dimension,
        /// The percent asked.
        percent: u32,
        /// Why not.
        why: &'static str,
    },
    /// More given below than there is above.
    #[error(
        "{} {side} of {given} given below exceed the {bound} above, for entries of one shape",
        dimension.as_str()
    )]
    Overallocated {
        /// The dimension.
        dimension: Dimension,
        /// `reservations` or `limits`.
        side: &'static str,
        /// What the sets below give, summed.
        given: u64,
        /// What the set above allows them.
        bound: u64,
    },
    /// What an organization runs itself, beside what it promised the organizations beneath it,
    /// more than it holds (Finding 139 of docs/23-walkthroughs.md).
    #[error(
        "{} reservations the organization above holds itself, beside what it guaranteed beneath \
         it, would be {would}, above its {bound}",
        dimension.as_str()
    )]
    Unkept {
        /// The dimension.
        dimension: Dimension,
        /// What it would hold and guarantee together.
        would: u64,
        /// Its bound.
        bound: u64,
    },
}

impl QuotaSet {
    /// Checks the set's own form: one entry of each shape, no reservation above the limit
    /// beside it, and overcommit only where it is possible.
    pub fn validate(&self) -> Result<(), SetError> {
        for (i, e) in self.entries.iter().enumerate() {
            if self.entries.iter().take(i).any(|o| shape(o) == shape(e)) {
                return Err(SetError::Repeated(i));
            }
            let above =
                e.reservations.iter().find(|(d, n)| e.limits.get(d).is_some_and(|l| *n > l));
            if let Some((d, _)) = above {
                return Err(SetError::ReservedAboveLimit { entry: i, dimension: *d });
            }
        }
        for (d, percent) in &self.child_overcommit {
            let why = if *percent < 100 {
                Some("a ratio below one hundred percent permits less than the limits themselves")
            } else if EXCLUSIVE.contains(d) && *percent > 100 {
                Some("it is exclusive by nature")
            } else {
                None
            };
            if let Some(why) = why {
                return Err(SetError::Overcommit { dimension: *d, percent: *percent, why });
            }
        }
        Ok(())
    }

    /// The entry of `shape`, if the set has one.
    #[must_use]
    pub fn of_shape(&self, s: Shape<'_>) -> Option<&QuotaEntry> {
        self.entries.iter().find(|e| shape(e) == s)
    }

    /// Checks that what `children` are given fits within this set, entry by entry of one
    /// shape: their reservations sum within its reservations, and their limits within its limits
    /// times the overcommit it permits them. A shape this set has no entry of, or a dimension
    /// its entry leaves unbounded, holds them to nothing here.
    pub fn fits(&self, children: &[&Self]) -> Result<(), SetError> {
        for mine in &self.entries {
            let theirs: Vec<&QuotaEntry> =
                children.iter().filter_map(|c| c.of_shape(shape(mine))).collect();
            self.fits_entry(mine, &theirs)?;
        }
        Ok(())
    }

    /// Checks `theirs`, entries of the shape of `mine`, against it.
    fn fits_entry(&self, mine: &QuotaEntry, theirs: &[&QuotaEntry]) -> Result<(), SetError> {
        let reserved = |d: &Dimension| summed(theirs, |e| &e.reservations, *d);
        let limited = |d: &Dimension| summed(theirs, |e| &e.limits, *d);
        if let Some((d, bound)) = mine.reservations.iter().find(|(d, b)| reserved(d) > **b) {
            return Err(over(*d, "reservations", reserved(d), *bound));
        }
        let permitted = |d: &Dimension, limit: u64| {
            let percent = u64::from(self.child_overcommit.get(d).copied().unwrap_or(100));
            limit.saturating_mul(percent) / 100
        };
        let exceeded = mine.limits.iter().find(|(d, l)| limited(d) > permitted(d, **l));
        if let Some((d, limit)) = exceeded {
            return Err(over(*d, "limits", limited(d), permitted(d, *limit)));
        }
        Ok(())
    }

    /// Checks that each entry of `project`, a sub-limit within this organization's quota, fits
    /// within this set's entry of its shape. A dimension the project leaves unbounded is bounded
    /// by the organization's entry at admission, which counts everything below it.
    pub fn bounds_project(&self, project: &Self) -> Result<(), SetError> {
        for e in &project.entries {
            let Some(mine) = self.of_shape(shape(e)) else { continue };
            within(&e.reservations, &mine.reservations, "reservations")?;
            within(&e.limits, &mine.limits, "limits")?;
        }
        Ok(())
    }
}

/// What `entries` give in `d`, summed, of the bounds `pick` takes.
fn summed(entries: &[&QuotaEntry], pick: fn(&QuotaEntry) -> &Bounds, d: Dimension) -> u64 {
    entries.iter().fold(0u64, |a, e| a.saturating_add(pick(e).get(&d).copied().unwrap_or(0)))
}

/// Checks that every dimension `theirs` bounds is within `ours` where `ours` bounds it.
fn within(theirs: &Bounds, ours: &Bounds, side: &'static str) -> Result<(), SetError> {
    let beyond =
        theirs.iter().find_map(|(d, g)| ours.get(d).filter(|b| g > *b).map(|b| (*d, *g, *b)));
    match beyond {
        Some((d, given, bound)) => Err(over(d, side, given, bound)),
        None => Ok(()),
    }
}

fn over(dimension: Dimension, side: &'static str, given: u64, bound: u64) -> SetError {
    SetError::Overallocated { dimension, side, given, bound }
}
