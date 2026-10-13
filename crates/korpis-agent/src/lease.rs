// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases on the agent (§4.5 of docs/02-architecture.md): no lease, no execution. The keeper
//! asks for a lease for every workload in the held set, renews each a third of the way through
//! its term, and counts every lease on this machine's own clock from the moment its request was
//! sent. The reconciler asks it what it may do for a workload: act, wait, keep running without
//! mutating anything (fail-static), or stop because another node holds the workload. A lease is
//! released only once the workload has stopped acting under it, and the release is a journal
//! record, so it reaches the control plane even across a partition (§4.10).

mod book;

pub use book::{Book, Grant, OnExpiry, Refusal, Request, Standing};

use crate::held::HeldSet;
use crate::journal::JournalHandle;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, watch};

/// The longest the keeper sleeps without looking again.
const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// The node's leases, shared by the keeper and the reconciler.
#[derive(Debug, Clone)]
pub struct Leases {
    book: Arc<Mutex<Book>>,
    changed: Arc<Notify>,
    journal: JournalHandle,
}

/// What a workload declared for the end of an unrenewed lease. A resolved assignment never
/// leaves it unspecified; if one did, stopping is the choice that cannot make two writers.
fn on_expiry(v: buffa::EnumValue<pb::OnLeaseExpiry>) -> OnExpiry {
    match v.as_known() {
        Some(pb::OnLeaseExpiry::ON_LEASE_EXPIRY_KEEP_RUNNING) => OnExpiry::KeepRunning,
        _ => OnExpiry::Stop,
    }
}

fn nonce() -> Vec<u8> {
    let mut n = [0u8; 16];
    if getrandom::fill(&mut n).is_err() {
        // A nonce only has to differ from the last one; it guards nothing secret.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        n = now.map(|d| d.as_nanos()).unwrap_or_default().to_le_bytes();
    }
    n.to_vec()
}

fn request(r: Request) -> agent::AgentMessage {
    let request = agent::LeaseRequest {
        workload: r.workload,
        nonce: r.nonce,
        held_epoch: r.held_epoch,
        ..Default::default()
    };
    agent::AgentMessage { message: Some(Up::LeaseRequest(Box::new(request))), ..Default::default() }
}

fn grant(g: &agent::LeaseGrant) -> Option<Grant> {
    let term = korpis_wire::duration_from("term", g.term.as_option()?).ok()?;
    Some(Grant { epoch: g.epoch, term, rho_ppm: g.rho_ppm, on_expiry: on_expiry(g.on_expiry) })
}

fn refusal(r: &agent::LeaseRefusal) -> Refusal {
    match r.reason.as_known() {
        Some(agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_SUPERSEDED) => {
            Refusal::Superseded { by: r.superseded_by.clone() }
        }
        Some(agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_NOT_ASSIGNED) => Refusal::NotAssigned,
        _ => Refusal::Unavailable,
    }
}

impl Leases {
    /// No leases yet; releases are written to `journal`.
    #[must_use]
    pub fn new(journal: JournalHandle) -> Self {
        Self { book: Arc::default(), changed: Arc::new(Notify::new()), journal }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Book) -> T) -> Option<T> {
        self.book.lock().ok().map(|mut b| f(&mut b))
    }

    /// What the node may do for `workload` now.
    #[must_use]
    pub fn standing(&self, workload: &str) -> Standing {
        self.with(|b| b.standing(workload, Instant::now())).unwrap_or(Standing::Pending)
    }

    /// The workload has stopped acting under its lease: forgets it and journals the release.
    pub async fn release(&self, workload: &str) -> std::io::Result<Option<u64>> {
        let Some(epoch) = self.with(|b| b.release(workload)).flatten() else { return Ok(None) };
        let released =
            agent::LeaseReleased { workload: workload.to_owned(), epoch, ..Default::default() };
        self.journal.append(Body::LeaseReleased(Box::new(released))).await?;
        Ok(Some(epoch))
    }

    /// Waits until a standing may have changed.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Takes a grant or a refusal; whether it was one.
    fn answer(&self, m: &Down) -> bool {
        let taken = match m {
            Down::LeaseGrant(g) => {
                let Some(parsed) = grant(g) else {
                    tracing::warn!(workload = %g.workload, "a lease grant without a term");
                    return true;
                };
                self.with(|b| b.granted(&g.workload, &g.nonce, &parsed))
            }
            Down::LeaseRefusal(r) => {
                tracing::warn!(workload = %r.workload, reason = ?refusal(r), "lease refused");
                self.with(|b| b.refused(&r.workload, &r.nonce, refusal(r), Instant::now()))
            }
            _ => return false,
        };
        if taken == Some(true) {
            self.changed.notify_one();
        }
        true
    }
}

/// Asks for, renews, and takes the answers to leases.
#[derive(Debug)]
pub struct Keeper {
    leases: Leases,
    held: watch::Receiver<HeldSet>,
    inbound: mpsc::Receiver<Down>,
    outbound: mpsc::Sender<agent::AgentMessage>,
}

/// Each workload held, with what it declared for an unrenewed lease.
fn wanted(held: &HeldSet) -> BTreeMap<String, OnExpiry> {
    held.assignments.iter().map(|(w, a)| (w.clone(), on_expiry(a.on_lease_expiry))).collect()
}

impl Keeper {
    /// A keeper for the workloads in `held`, reading the control plane's answers from
    /// `inbound` and sending requests on `outbound`.
    #[must_use]
    pub fn new(
        leases: Leases,
        held: watch::Receiver<HeldSet>,
        inbound: mpsc::Receiver<Down>,
        outbound: mpsc::Sender<agent::AgentMessage>,
    ) -> Self {
        Self { leases, held, inbound, outbound }
    }

    /// Takes one control message; whether the channel is still open.
    fn take(&self, m: Option<Down>) -> bool {
        let Some(m) = m else { return false };
        if !self.leases.answer(&m) {
            // Revocations, feasibility queries, and content hints arrive with their consumers.
            tracing::debug!(message = ?m, "control message not yet handled");
        }
        true
    }

    /// Sends the requests now due; whether the channel is still open.
    async fn send_due(&self) -> bool {
        let wanted = wanted(&self.held.borrow());
        let due = self.leases.with(|b| b.due(&wanted, Instant::now(), nonce)).unwrap_or_default();
        for r in due {
            if self.outbound.send(request(r)).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Runs until the session's channels close.
    pub async fn run(mut self) {
        loop {
            if !self.send_due().await {
                return;
            }
            let next = self.leases.with(|b| b.next_change()).flatten();
            let now = Instant::now();
            let wake = next.filter(|t| *t > now).map_or(LOOK_AGAIN, |t| t - now).min(LOOK_AGAIN);
            let open = tokio::select! {
                changed = self.held.changed() => changed.is_ok(),
                m = self.inbound.recv() => self.take(m),
                () = tokio::time::sleep(wake) => {
                    self.leases.changed.notify_one();
                    true
                }
            };
            if !open {
                return;
            }
        }
    }
}
