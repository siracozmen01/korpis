// SPDX-License-Identifier: AGPL-3.0-or-later

//! The leases a node holds, as the holder counts them (§4.5 of docs/02-architecture.md). A
//! lease runs from the moment its request was sent, on this machine's monotonic clock, for
//! `term × (1 − ρ)`: the control plane counts from the receipt, which is later, so the holder
//! always believes its lease ends first, whatever the network did. A lease is renewed a third
//! of the way through its term, so one lost answer costs nothing. Everything here is a pure
//! function of what was sent, what came back, and the time, so it is tested without a network.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How long a request waits for its answer before another is sent with a fresh nonce.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// The first wait after a refusal; it doubles up to a third of the term.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// What a workload does when its lease ends unrenewed (§4.5 of docs/02-architecture.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExpiry {
    /// Keep running: nothing else can use its disk, and stopping it serves nobody.
    KeepRunning,
    /// Stop: another node could attach its storage.
    Stop,
}

/// Why the control plane refused a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Another node holds a newer epoch.
    Superseded {
        /// The node holding it.
        by: String,
    },
    /// The workload is not placed on this node.
    NotAssigned,
    /// Transient: the old lease is not over yet.
    Unavailable,
}

/// What the node may do for a workload, by its lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// A lease is held: the node may act.
    Held {
        /// Its epoch.
        epoch: u64,
    },
    /// No lease has been granted yet.
    Pending,
    /// The lease ended unrenewed; what to do is the workload's declared `on_expiry`. Nothing
    /// is mutated meanwhile, and a reissue resumes it (fail-static).
    Expired {
        /// The epoch it held.
        epoch: u64,
        /// What to do.
        on_expiry: OnExpiry,
    },
    /// The control plane refused it.
    Refused(Refusal),
}

/// A request to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The workload.
    pub workload: String,
    /// Echoed by the answer.
    pub nonce: Vec<u8>,
    /// The epoch held, zero if none.
    pub held_epoch: u64,
}

/// A grant, as it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The epoch.
    pub epoch: u64,
    /// How long it lasts.
    pub term: Duration,
    /// The bound on clock-rate error, in parts per million.
    pub rho_ppm: u32,
    /// What to do if it ends unrenewed.
    pub on_expiry: OnExpiry,
}

#[derive(Debug, Clone)]
struct Entry {
    epoch: u64,
    valid_until: Option<Instant>,
    term: Option<Duration>,
    on_expiry: OnExpiry,
    outstanding: Option<(Vec<u8>, Instant)>,
    next_request: Instant,
    refused: Option<Refusal>,
    retry: Duration,
}

/// The node's leases.
#[derive(Debug, Default)]
pub struct Book {
    entries: BTreeMap<String, Entry>,
    /// Workloads whose lease was released while their assignment is still held (a retirement
    /// done, its completion not yet ingested): never asked for again until they leave the set.
    released: BTreeSet<String>,
}

impl Entry {
    /// How long its outstanding request waits for an answer.
    fn answer_within(&self) -> Duration {
        self.term.map_or(ANSWER_WITHIN, |t| ANSWER_WITHIN.min(t / 3))
    }

    /// When its outstanding request stops waiting.
    fn answer_by(&self) -> Option<Instant> {
        let (_, sent) = self.outstanding.as_ref()?;
        sent.checked_add(self.answer_within())
    }
}

/// `term × (1 − ρ)`.
fn shortened(term: Duration, rho_ppm: u32) -> Duration {
    let keep = 1_000_000u32.saturating_sub(rho_ppm.min(1_000_000));
    term.checked_mul(keep).map_or(Duration::ZERO, |t| t / 1_000_000)
}

impl Book {
    /// Tracks exactly `wanted` (each workload with its declared `on_expiry`), forgetting the
    /// rest, and returns the requests due at `now`; `nonce` makes each request's nonce.
    pub fn due(
        &mut self,
        wanted: &BTreeMap<String, OnExpiry>,
        now: Instant,
        mut nonce: impl FnMut() -> Vec<u8>,
    ) -> Vec<Request> {
        self.released.retain(|w| wanted.contains_key(w));
        self.entries.retain(|w, _| wanted.contains_key(w) && !self.released.contains(w));
        for (workload, on_expiry) in wanted.iter().filter(|(w, _)| !self.released.contains(*w)) {
            let entry = self.entries.entry(workload.clone()).or_insert_with(|| Entry {
                epoch: 0,
                valid_until: None,
                term: None,
                on_expiry: *on_expiry,
                outstanding: None,
                next_request: now,
                refused: None,
                retry: FIRST_RETRY,
            });
            entry.on_expiry = *on_expiry;
        }
        let mut out = Vec::new();
        for (workload, e) in &mut self.entries {
            if e.answer_by().is_some_and(|by| now < by) || now < e.next_request {
                continue;
            }
            let n = nonce();
            e.outstanding = Some((n.clone(), now));
            out.push(Request { workload: workload.clone(), nonce: n, held_epoch: e.epoch });
        }
        out
    }

    /// The entry an answer with `nonce` belongs to, and when its request was sent.
    fn answered(&mut self, workload: &str, nonce: &[u8]) -> Option<(&mut Entry, Instant)> {
        let e = self.entries.get_mut(workload)?;
        let sent = e.outstanding.as_ref().filter(|(n, _)| n == nonce).map(|(_, s)| *s)?;
        e.outstanding = None;
        Some((e, sent))
    }

    /// A grant arrived for `workload` answering `nonce`; whether it was the one awaited. A
    /// grant with another epoch for this node is a reissue, and is taken.
    pub fn granted(&mut self, workload: &str, nonce: &[u8], g: &Grant) -> bool {
        let Some((e, sent)) = self.answered(workload, nonce) else { return false };
        e.epoch = g.epoch;
        e.term = Some(g.term);
        e.on_expiry = g.on_expiry;
        e.valid_until = sent.checked_add(shortened(g.term, g.rho_ppm));
        e.next_request = sent.checked_add(g.term / 3).unwrap_or(sent);
        e.refused = None;
        e.retry = FIRST_RETRY;
        true
    }

    /// A refusal arrived for `workload` answering `nonce` at `now`; whether it was awaited. A
    /// lease already held stays valid to the end the holder counted, since validity is a
    /// property of the clocks; only a supersession ends it at once, because the control plane
    /// issues another node an epoch only once this one's lease is provably over.
    pub fn refused(&mut self, workload: &str, nonce: &[u8], r: Refusal, now: Instant) -> bool {
        let Some((e, _)) = self.answered(workload, nonce) else { return false };
        if matches!(r, Refusal::Superseded { .. }) {
            e.valid_until = None;
        }
        let ceiling = e.term.map_or(Duration::from_secs(20), |t| t / 3).max(FIRST_RETRY);
        e.next_request = now.checked_add(e.retry).unwrap_or(now);
        e.retry = (e.retry * 2).min(ceiling);
        e.refused = Some(r);
        true
    }

    /// What the node may do for `workload` at `now`.
    #[must_use]
    pub fn standing(&self, workload: &str, now: Instant) -> Standing {
        let Some(e) = self.entries.get(workload) else { return Standing::Pending };
        if let Some(until) = e.valid_until
            && now < until
        {
            return Standing::Held { epoch: e.epoch };
        }
        match &e.refused {
            Some(r @ Refusal::Superseded { .. }) => return Standing::Refused(r.clone()),
            Some(Refusal::NotAssigned) if e.epoch == 0 => {
                return Standing::Refused(Refusal::NotAssigned);
            }
            _ if e.epoch == 0 => return Standing::Pending,
            _ => {}
        }
        Standing::Expired { epoch: e.epoch, on_expiry: e.on_expiry }
    }

    /// Forgets `workload`, returning the epoch it held: it has stopped acting under it, and is
    /// not asked for again while its assignment is still held.
    pub fn release(&mut self, workload: &str) -> Option<u64> {
        self.released.insert(workload.to_owned());
        self.entries.remove(workload).map(|e| e.epoch).filter(|epoch| *epoch > 0)
    }

    /// When something next changes: a request falling due, or a lease running out.
    #[must_use]
    pub fn next_change(&self) -> Option<Instant> {
        self.entries
            .values()
            .flat_map(|e| [Some(e.next_request), e.valid_until, e.answer_by()])
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(epoch: u64) -> Grant {
        Grant {
            epoch,
            term: Duration::from_secs(60),
            rho_ppm: 1000,
            on_expiry: OnExpiry::KeepRunning,
        }
    }

    fn one(on_expiry: OnExpiry) -> BTreeMap<String, OnExpiry> {
        BTreeMap::from([("wl".to_owned(), on_expiry)])
    }

    #[test]
    fn a_lease_runs_from_the_send_shortened_by_rho_and_is_renewed_at_a_third() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut book = Book::default();
        let wanted = one(OnExpiry::KeepRunning);
        let mut n = 0u8;
        let mut nonce = || {
            n += 1;
            vec![n]
        };
        let first = book.due(&wanted, t0, &mut nonce);
        assert_eq!(first.len(), 1);
        assert_eq!(book.standing("wl", t0), Standing::Pending);
        assert!(book.due(&wanted, at(1), &mut nonce).is_empty(), "the request is outstanding");
        assert!(!book.granted("wl", b"stranger", &grant(7)), "an unawaited answer is ignored");
        assert!(book.granted("wl", &first[0].nonce, &grant(7)));
        assert_eq!(book.standing("wl", at(59)), Standing::Held { epoch: 7 });
        let end = t0 + Duration::from_millis(59_940);
        assert_eq!(
            book.standing("wl", end),
            Standing::Expired { epoch: 7, on_expiry: OnExpiry::KeepRunning }
        );
        assert!(book.due(&wanted, at(19), &mut nonce).is_empty());
        let renewal = book.due(&wanted, at(20), &mut nonce);
        assert_eq!(renewal[0].held_epoch, 7, "renewed a third of the way through");
    }

    #[test]
    fn a_supersession_ends_the_lease_and_a_reissue_resumes_it() {
        let t0 = Instant::now();
        let mut book = Book::default();
        let wanted = one(OnExpiry::Stop);
        let r = book.due(&wanted, t0, || vec![1]);
        book.granted("wl", &r[0].nonce, &grant(3));
        let later = t0 + Duration::from_secs(30);
        let r = book.due(&wanted, later, || vec![2]);
        let by = Refusal::Superseded { by: "nd_x".into() };
        assert!(book.refused("wl", &r[0].nonce, by.clone(), later));
        assert_eq!(book.standing("wl", later), Standing::Refused(by));

        let mut book = Book::default();
        let r = book.due(&wanted, t0, || vec![1]);
        book.granted("wl", &r[0].nonce, &grant(3));
        let gone = t0 + Duration::from_secs(120);
        assert!(matches!(book.standing("wl", gone), Standing::Expired { epoch: 3, .. }));
        let r = book.due(&wanted, gone, || vec![9]);
        assert!(book.granted("wl", &r[0].nonce, &grant(11)), "a reissue for this node");
        assert_eq!(book.standing("wl", gone), Standing::Held { epoch: 11 });
        assert_eq!(book.release("wl"), Some(11));
        assert!(book.due(&wanted, gone, || vec![10]).is_empty(), "released, not asked again");
        assert!(book.due(&BTreeMap::new(), gone, || vec![11]).is_empty());
        assert_eq!(book.due(&wanted, gone, || vec![12]).len(), 1, "held again after leaving");
    }

    #[test]
    fn an_unanswered_request_is_sent_again_and_a_refusal_backs_off() {
        let t0 = Instant::now();
        let mut book = Book::default();
        let wanted = one(OnExpiry::KeepRunning);
        let r = book.due(&wanted, t0, || vec![1]);
        let again = book.due(&wanted, t0 + Duration::from_secs(5), || vec![2]);
        assert_eq!(again.len(), 1, "five seconds without an answer");
        assert!(!book.granted("wl", &r[0].nonce, &grant(1)), "the old nonce is no longer awaited");
        let now = t0 + Duration::from_secs(6);
        assert!(book.refused("wl", &again[0].nonce, Refusal::Unavailable, now));
        assert_eq!(book.standing("wl", now), Standing::Pending);
        assert!(book.due(&wanted, now, || vec![3]).is_empty());
        assert_eq!(book.due(&wanted, now + Duration::from_secs(1), || vec![4]).len(), 1);
        assert!(book.next_change().is_some());
        assert!(book.due(&BTreeMap::new(), now, || vec![5]).is_empty(), "forgotten");
    }
}
