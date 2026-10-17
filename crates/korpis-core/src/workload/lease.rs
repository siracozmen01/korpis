// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases (§4.5 of docs/02-architecture.md): every running workload needs one, and at most one
//! is valid at a time. Exclusivity rests on the clocks, not on delivery: the holder's lease runs
//! from when it asked and the grantor's from when it heard, so the holder always believes its
//! lease ends first, and a lease for another node is issued only once the old one is provably
//! over.

use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A workload's lease as the control plane holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// The workload.
    pub workload: Uuid,
    /// The node holding it.
    pub node: Uuid,
    /// Monotonic, never reused.
    pub epoch: u64,
    /// How long it lasts from the holder's request.
    pub term: Duration,
    /// When the grantor last heard the holder ask: its end is this plus `term`.
    pub received_at: SystemTime,
    /// When the holder said it stopped acting under it.
    pub released_at: Option<SystemTime>,
}

impl Lease {
    /// The grantor's end of the lease.
    #[must_use]
    pub fn grantor_expiry(&self) -> SystemTime {
        self.received_at + self.term
    }

    /// Whether nobody can be acting under it any more: released, or past the grantor's end.
    #[must_use]
    pub fn over(&self, now: SystemTime) -> bool {
        self.released_at.is_some() || now >= self.grantor_expiry()
    }
}

/// Why a lease is refused; the control plane always knows which case it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another node holds it under an epoch issued to it: stop, keep the data, report the
    /// volume (§4.5).
    Superseded {
        /// The holder.
        by: Uuid,
    },
    /// The workload is not assigned to the node that asked.
    NotAssigned,
    /// It is assigned to the node that asked, but the previous holder's lease is not over yet:
    /// retry. A lost `LeaseRevoke` makes the handoff wait one term, never unsafe.
    Unavailable,
}

/// What to do with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Extend the asking node's current lease from now, under the epoch it already has.
    Renew {
        /// The epoch.
        epoch: u64,
    },
    /// Issue a new epoch to the asking node.
    Issue,
    /// Refuse.
    Refuse(Refusal),
}

/// Decides a request from `node` for a workload, which is placed on `placed_on` (if anywhere),
/// whose current lease is `current`, at `now`.
///
/// The asking node's own unreleased lease is renewed under its epoch whatever epoch the node
/// says it holds: a node whose agent restarted, or whose control plane was restored, is given
/// back the epoch that is still its own (the fail-static row of §4.5). A released one is not
/// renewed: the holder said it stopped, so acting again is a new epoch.
#[must_use]
pub fn decide(
    node: Uuid,
    placed_on: Option<Uuid>,
    current: Option<&Lease>,
    now: SystemTime,
) -> Decision {
    if let Some(l) = current
        && l.node == node
        && l.released_at.is_none()
        && placed_on == Some(node)
    {
        return Decision::Renew { epoch: l.epoch };
    }
    let held_elsewhere = current.filter(|l| l.node != node && !l.over(now));
    match (placed_on == Some(node), held_elsewhere) {
        (true, None) => Decision::Issue,
        (true, Some(_)) => Decision::Refuse(Refusal::Unavailable),
        (false, Some(l)) => Decision::Refuse(Refusal::Superseded { by: l.node }),
        (false, None) => Decision::Refuse(Refusal::NotAssigned),
    }
}

/// A restore's first fence (§8 of docs/03-state.md): the highest epoch that can have been issued
/// since a backup holding `max_issued`, `backup_age` ago, by a control plane issuing at most
/// `rate_per_sec`. Issuance spends from a bucket holding a second's worth, so in `t` seconds at
/// most `rate_per_sec × (t + 1)` epochs are issued; the age is rounded up to the second.
#[must_use]
pub fn fence_floor(max_issued: u64, rate_per_sec: u64, backup_age: std::time::Duration) -> u64 {
    let seconds = backup_age.as_secs().saturating_add(u64::from(backup_age.subsec_nanos() > 0));
    max_issued.saturating_add(rate_per_sec.saturating_mul(seconds.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fence_is_past_every_epoch_the_rate_allowed_since_the_backup() {
        let age = std::time::Duration::from_millis(2_500);
        assert_eq!(fence_floor(40, 1000, age), 40 + 1000 * 4, "3 s rounded up, plus the bucket");
        assert_eq!(fence_floor(40, 1000, std::time::Duration::ZERO), 1040);
        assert_eq!(fence_floor(u64::MAX - 1, 1000, age), u64::MAX, "saturates, never wraps");
    }

    fn lease(node: u128, epoch: u64, received: SystemTime) -> Lease {
        Lease {
            workload: Uuid::from_u128(9),
            node: Uuid::from_u128(node),
            epoch,
            term: Duration::from_secs(60),
            received_at: received,
            released_at: None,
        }
    }

    #[test]
    fn the_holder_renews_and_a_new_node_waits_for_the_old_lease_to_end() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let now = SystemTime::now();
        assert_eq!(decide(a, Some(a), None, now), Decision::Issue);
        let held = lease(1, 7, now);
        assert_eq!(decide(a, Some(a), Some(&held), now), Decision::Renew { epoch: 7 });
        assert_eq!(decide(b, Some(b), Some(&held), now), Decision::Refuse(Refusal::Unavailable));
        let later = now + Duration::from_secs(61);
        assert_eq!(decide(b, Some(b), Some(&held), later), Decision::Issue, "provably over");
        let released = Lease { released_at: Some(now), ..held };
        assert_eq!(decide(b, Some(b), Some(&released), now), Decision::Issue);
        assert_eq!(decide(a, Some(a), Some(&released), now), Decision::Issue, "a new epoch");
    }

    #[test]
    fn a_node_the_workload_left_is_told_who_holds_it_or_that_it_is_not_assigned() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let now = SystemTime::now();
        let held_by_b = lease(2, 8, now);
        let refusal = decide(a, Some(b), Some(&held_by_b), now);
        assert_eq!(refusal, Decision::Refuse(Refusal::Superseded { by: b }));
        assert_eq!(decide(a, None, None, now), Decision::Refuse(Refusal::NotAssigned));
        let mine_but_moved = lease(1, 3, now);
        let refusal = decide(a, Some(b), Some(&mine_but_moved), now);
        assert_eq!(refusal, Decision::Refuse(Refusal::NotAssigned));
    }
}
