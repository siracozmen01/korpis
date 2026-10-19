// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the workloads a quota entry counts hold together (§5.1 and question 7 of §10 of
//! docs/05-scheduling.md): kept beside the entry and moved in the transaction of every change to
//! what one of them holds, so that admission reads one row rather than every workload the entry
//! counts. Each workload's own contribution is kept with it, so taking it back out is exact.

use super::{Dimension, Held};
use std::collections::BTreeMap;

/// The sums, per dimension, of what the workloads an entry counts hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// Reserved.
    pub reservations: BTreeMap<Dimension, u64>,
    /// Limits, over the workloads whose limit in that dimension is bounded.
    pub limits: BTreeMap<Dimension, u64>,
    /// Per dimension, how many hold an unbounded limit, which no finite bound can hold.
    pub unbounded: BTreeMap<Dimension, u64>,
}

fn add(map: &mut BTreeMap<Dimension, u64>, d: Dimension, n: u64) {
    if n > 0 {
        let slot = map.entry(d).or_insert(0);
        *slot = slot.saturating_add(n);
    }
}

fn sub(map: &mut BTreeMap<Dimension, u64>, d: Dimension, n: u64) {
    if let Some(slot) = map.get_mut(&d) {
        *slot = slot.saturating_sub(n);
        if *slot == 0 {
            map.remove(&d);
        }
    }
}

impl Totals {
    /// The totals of `held`, each one workload's holding.
    pub fn sum<'a>(held: impl IntoIterator<Item = &'a Held>) -> Self {
        let mut out = Self::default();
        for h in held {
            out.add(h);
        }
        out
    }

    /// Counts what `h` holds.
    pub fn add(&mut self, h: &Held) {
        for (d, n) in &h.reservations {
            add(&mut self.reservations, *d, *n);
        }
        for (d, n) in &h.limits {
            match n {
                Some(n) => add(&mut self.limits, *d, *n),
                None => add(&mut self.unbounded, *d, 1),
            }
        }
    }

    /// Takes back what `h` holds, which was counted before.
    pub fn sub(&mut self, h: &Held) {
        for (d, n) in &h.reservations {
            sub(&mut self.reservations, *d, *n);
        }
        for (d, n) in &h.limits {
            match n {
                Some(n) => sub(&mut self.limits, *d, *n),
                None => sub(&mut self.unbounded, *d, 1),
            }
        }
    }

    /// Reserved in `d`.
    #[must_use]
    pub fn reserved(&self, d: Dimension) -> u64 {
        self.reservations.get(&d).copied().unwrap_or(0)
    }

    /// The sum of the limits in `d`; `None` while any of them is unbounded.
    #[must_use]
    pub fn limited(&self, d: Dimension) -> Option<u64> {
        let unbounded = self.unbounded.get(&d).is_some_and(|n| *n > 0);
        (!unbounded).then(|| self.limits.get(&d).copied().unwrap_or(0))
    }
}
