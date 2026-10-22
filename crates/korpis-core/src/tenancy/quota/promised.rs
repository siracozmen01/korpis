// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an organization's entry has promised below it (§5.1 of docs/05-scheduling.md, Finding
//! 139 of docs/23-walkthroughs.md). A child organization given a set is guaranteed its
//! reservations: whatever its parent runs itself, the child's workloads may reserve up to them.
//! So the parent's entry counts each child's entry of its shape as the larger of what the child
//! was given and what it holds, beside what the parent's own workloads hold, and a workload
//! within its child's guarantee is never refused by the parent's entry. Limits are sold, not
//! guaranteed, and are counted as they are held.

use super::{
    Bounds, Dimension, Exceeded, Held, QuotaEntry, QuotaSet, SetError, Side, Totals, shape,
};

/// A child organization's entry of the shape of the entry admitting: the reservations it was
/// given, what the workloads it counts hold less the workload being admitted, and whether it
/// counts that workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promise {
    /// The reservations it was given.
    pub given: Bounds,
    /// What the workloads it counts hold, less what it counted of the workload being admitted.
    pub held: Totals,
    /// Whether it counts the workload being admitted.
    pub counts: bool,
}

impl Promise {
    fn given(&self, d: Dimension) -> u64 {
        self.given.get(&d).copied().unwrap_or(0)
    }
}

/// What `entry`'s workloads would reserve in `d` once a workload reserving `mine` is admitted
/// beside `rest`, its children's promises counted as the larger of what each was given and what
/// it holds; and whether the workload stays within the guarantee of the child counting it.
fn reserved_with(rest: &Totals, promises: &[Promise], d: Dimension, mine: u64) -> (u64, bool) {
    let below: u64 = promises.iter().map(|p| p.held.reserved(d)).sum();
    let own = rest.reserved(d).saturating_sub(below);
    let mut guaranteed = false;
    let mut owed = 0u64;
    for p in promises {
        let held = p.held.reserved(d).saturating_add(if p.counts { mine } else { 0 });
        guaranteed |= p.counts && p.given.contains_key(&d) && held <= p.given(d);
        owed = owed.saturating_add(held.max(p.given(d)));
    }
    let direct = if promises.iter().any(|p| p.counts) { 0 } else { mine };
    (own.saturating_add(direct).saturating_add(owed), guaranteed)
}

impl QuotaEntry {
    /// Admits a workload that held `old` (nothing, if new) and would hold `new`, beside `rest`,
    /// what everything else the entry counts holds, and `promises`, what it guaranteed the
    /// children whose entries have its shape. Only growth is refused, as by [`Self::admit`]; and
    /// a workload within its child's guarantee is not refused by this entry in that dimension.
    ///
    /// # Errors
    /// The workload would take more than the entry's reservations, counting its promises, or
    /// than its limits.
    pub fn admit_promised(
        &self,
        rest: &Totals,
        promises: &[Promise],
        old: Option<&Held>,
        new: &Held,
    ) -> Result<(), Exceeded> {
        if promises.is_empty() {
            return self.admit(rest, old, new);
        }
        for (d, bound) in &self.reservations {
            let mine = new.reservations.get(d).copied().unwrap_or(0);
            let before = old.and_then(|o| o.reservations.get(d).copied()).unwrap_or(0);
            let (would, guaranteed) = reserved_with(rest, promises, *d, mine);
            if mine > before && !guaranteed && would > *bound {
                return Err(Exceeded {
                    entry: self.id,
                    dimension: *d,
                    side: Side::Reservation,
                    bound: *bound,
                    would,
                });
            }
        }
        let limits_only = Self { reservations: Bounds::new(), ..self.clone() };
        limits_only.admit(rest, old, new)
    }

    /// What this entry's reservations are held to in `d` when `promises` are its children's:
    /// what its own workloads hold, beside each child's given reservations or holdings, whichever
    /// is larger. Setting a set checks this against the bound, so a parent never promises more
    /// than it has left beside what it runs itself.
    #[must_use]
    pub fn promised_reserved(&self, d: Dimension, promises: &[Promise]) -> u64 {
        let rest = self.held.clone().unwrap_or_default();
        reserved_with(&rest, promises, d, 0).0
    }
}

/// What `mine`, an entry of a parent's set, promised the children whose sets are `sets`: each
/// one's entry of its shape, with what that entry's workloads hold.
fn promises_of(mine: &QuotaEntry, sets: &[&QuotaSet]) -> Vec<Promise> {
    sets.iter()
        .filter_map(|s| s.of_shape(shape(mine)))
        .map(|c| Promise {
            given: c.reservations.clone(),
            held: c.held.clone().unwrap_or_default(),
            counts: false,
        })
        .collect()
}

/// Checks that `parent` keeps what it promised its children, whose sets are `after`, beside
/// what it runs itself: in every reservation each entry of `parent` bounds, its own holdings and
/// each child's given reservations or holdings, whichever is more, within the bound; unless that
/// is no more than it was with `before`, the children's sets until now, since only growth is
/// refused, as at admission.
///
/// # Errors
/// [`SetError::Unkept`], naming the first dimension that would exceed its bound.
pub fn keeps_promises(
    parent: &QuotaSet,
    after: &[&QuotaSet],
    before: &[&QuotaSet],
) -> Result<(), SetError> {
    for mine in &parent.entries {
        let (now, was) = (promises_of(mine, after), promises_of(mine, before));
        for (d, bound) in &mine.reservations {
            let would = mine.promised_reserved(*d, &now);
            if would > *bound && would > mine.promised_reserved(*d, &was) {
                return Err(SetError::Unkept { dimension: *d, would, bound: *bound });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::super::QuotaOwner;
    use super::*;
    use uuid::Uuid;

    const MIB: u64 = 1 << 20;

    fn memory(n: u64) -> Bounds {
        Bounds::from([(Dimension::Memory, n * MIB)])
    }

    fn holding(n: u64) -> Held {
        Held { reservations: memory(n), ..Held::default() }
    }

    fn reseller() -> QuotaEntry {
        QuotaEntry {
            id: Uuid::nil(),
            owner: QuotaOwner::Organization(Uuid::nil()),
            selector: None,
            nodes: None,
            reservations: memory(512),
            limits: Bounds::new(),
            version: 0,
            egress: None,
            held: None,
        }
    }

    fn promise(given: u64, held: u64, counts: bool) -> Promise {
        Promise { given: memory(given), held: Totals::sum([&holding(held)]), counts }
    }

    #[test]
    fn a_parent_runs_only_what_it_has_left_beside_what_it_promised() {
        let e = reseller();
        let sub = promise(384, 64, false);
        let own = Totals::sum([&holding(64), &holding(64)]);
        // 64 of its own and 384 promised: 64 more of its own fits, 128 more does not.
        e.admit_promised(&own, std::slice::from_ref(&sub), None, &holding(64)).unwrap();
        let refused = e.admit_promised(&own, &[sub], None, &holding(128)).unwrap_err();
        assert_eq!(refused.would, 576 * MIB, "its own 192 beside the 384 it promised");
    }

    #[test]
    fn a_child_within_its_guarantee_is_never_refused_by_its_parent() {
        let e = reseller();
        // The host lowered the reseller after it ran 300 of its own: its promise still holds.
        let rest = Totals::sum([&holding(300), &holding(64)]);
        let sub = promise(384, 64, true);
        e.admit_promised(&rest, std::slice::from_ref(&sub), None, &holding(320)).unwrap();
        let beyond = e.admit_promised(&rest, &[sub], None, &holding(330));
        assert!(beyond.is_err(), "beyond its guarantee, the parent's bound counts it");
    }

    #[test]
    fn what_a_parent_has_promised_is_what_it_gave_or_what_is_held_whichever_is_more() {
        let e = QuotaEntry { held: Some(Totals::sum([&holding(100), &holding(50)])), ..reseller() };
        let promises = [promise(200, 50, false), promise(10, 0, false)];
        assert_eq!(e.promised_reserved(Dimension::Memory, &promises), 310 * MIB);
    }
}
