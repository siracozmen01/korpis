// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's probe of every edge group's members through their forwarding paths
//! (§3.1 of docs/07-networking.md, Finding 189). Every [`CLAIM`] this replica claims the groups
//! no other replica probes, holding each for [`HOLD`], and runs one task for each it holds. A
//! group's task sends, every interval of the group's, one datagram to the probe port of each
//! member's own address and of each address it holds, and counts an answer signed by the node
//! the group forwards the probe to, arriving within the interval, as the probe passed. A member
//! whose own address misses the group's count of probes in a row is failing, and one that
//! answers is ready again at once. What changes is recorded at once, and what does not every
//! [`RECORD`], so a group's status says when each member was last probed.

use crate::api::Api;
use korpis_app::edge_group::{ProbePlan, ProbeTarget};
use korpis_core::edge::probe::{ANSWER, Measured, NONCE, answer_message, request};
use korpis_core::node::identity::verify;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// How often this replica claims groups and renews those it probes.
const CLAIM: Duration = Duration::from_secs(2);
/// How long a claim holds a group's probe, so another replica takes it over once this one stops.
const HOLD: Duration = Duration::from_secs(10);
/// How often a standing that has not changed is recorded again.
const RECORD: Duration = Duration::from_secs(10);
/// Why a member or an address fails that misses its probes, the count in place of `{}`.
const MISSED: &str = "it missed {} probes in a row through its forwarding path";
/// The longest a probe waits for its answer, whatever the group's interval.
const ANSWER_WITHIN: Duration = Duration::from_secs(1);

/// What one probed address stands at, and when that was last recorded, with why it failed.
#[derive(Debug, Default)]
struct Standing {
    measured: Measured,
    recorded: Option<Instant>,
    why: Option<&'static str>,
}

impl Standing {
    /// Takes one outcome, failing for `why`; whether it is to be recorded now.
    fn take(&mut self, (answered, why): (bool, &'static str), allowed: u32) -> bool {
        let mut changed = self.measured.take(answered, allowed);
        let why = (self.measured.passes == Some(false)).then_some(why);
        changed |= why != self.why;
        self.why = why;
        let stale = self.recorded.is_none_or(|t| t.elapsed() >= RECORD);
        let due = self.measured.passes.is_some() && (changed || stale);
        if due {
            self.recorded = Some(Instant::now());
        }
        due
    }
}

/// Whether `address`, probed of `group` at `at`, answers within `wait` as `answerer` signs.
async fn answers(
    (at, group, address): (SocketAddr, Uuid, IpAddr),
    answerer: &(String, Vec<u8>),
    wait: Duration,
) -> bool {
    let mut nonce = [0_u8; NONCE];
    if getrandom::fill(&mut nonce).is_err() {
        return false;
    }
    let any: SocketAddr = if at.is_ipv4() { ([0_u8; 4], 0).into() } else { ([0_u16; 8], 0).into() };
    let Ok(socket) = UdpSocket::bind(any).await else { return false };
    if socket.send_to(&request(&nonce, group, address), at).await.is_err() {
        return false;
    }
    let mut answer = [0_u8; ANSWER + 1];
    let deadline = Instant::now() + wait;
    while let Ok(Ok((n, from))) =
        tokio::time::timeout_at(deadline, socket.recv_from(&mut answer)).await
    {
        let signed = answer_message(group, address, &answerer.0, &nonce);
        if n == ANSWER
            && from == at
            && answer.get(..ANSWER).is_some_and(|a| verify(&answerer.1, &signed, a))
        {
            return true;
        }
    }
    false
}

/// One member's round: whether its own address answered, none where it cannot be probed and
/// why; and each address it holds with whether it answered.
async fn round(
    t: ProbeTarget,
    (group, port, wait): (Uuid, u16, Duration),
) -> (Uuid, Result<bool, &'static str>, Vec<(IpAddr, bool)>) {
    let Some(answerer) = t.answerer else {
        return (t.member, Err("no node answers its probe yet"), Vec::new());
    };
    let probe = |address: IpAddr| {
        answers((SocketAddr::new(address, port), group, address), &answerer, wait)
    };
    let own = match t.own {
        Some(address) => Ok(probe(address).await),
        None => Err("it advertises no address to be probed at"),
    };
    let mut held = Vec::with_capacity(t.held.len());
    for address in t.held {
        held.push((address, probe(address).await));
    }
    (t.member, own, held)
}

/// Probes every member of `plan` at once and records what changed.
async fn probe_once(
    api: &Api,
    plan: ProbePlan,
    (members, addresses): (&mut BTreeMap<Uuid, Standing>, &mut BTreeMap<IpAddr, Standing>),
) {
    let wait = plan.interval.min(ANSWER_WITHIN);
    let mut rounds = tokio::task::JoinSet::new();
    for t in plan.targets {
        rounds.spawn(round(t, (plan.group, plan.port, wait)));
    }
    while let Some(Ok((member, own, held))) = rounds.join_next().await {
        let standing = members.entry(member).or_default();
        let due = match own {
            Ok(answered) => standing.take((answered, MISSED), plan.misses),
            // It cannot pass, and nothing is waited for to say so.
            Err(why) => standing.take((false, why), 1),
        };
        if due {
            let passes = standing.measured.passes == Some(true);
            let why = standing.why.map(|w| w.replace("{}", &plan.misses.to_string()));
            let said = (passes, why.as_deref());
            if let Err(e) = api.app().record_member_probe((plan.group, member), said).await {
                tracing::warn!(%member, error = %e, "a member's probe was not recorded");
            }
        }
        recorded(api, held, (addresses, plan.misses)).await;
    }
}

/// Takes each address's outcome of `held` into `addresses`, `misses` in a row failing it, and
/// records what is due.
async fn recorded(
    api: &Api,
    held: Vec<(IpAddr, bool)>,
    (addresses, misses): (&mut BTreeMap<IpAddr, Standing>, u32),
) {
    for (address, answered) in held {
        let standing = addresses.entry(address).or_default();
        if !standing.take((answered, MISSED), misses) {
            continue;
        }
        let answering = standing.measured.passes == Some(true);
        if let Err(e) = api.app().record_address_probe(address, answering).await {
            tracing::warn!(%address, error = %e, "an address's probe was not recorded");
        }
    }
}

/// Probes `group` every interval of its own until it is gone; the task is aborted once this
/// replica no longer holds the group's probe.
async fn probe_group(api: Arc<Api>, group: Uuid) {
    let (mut members, mut addresses) = (BTreeMap::new(), BTreeMap::new());
    let mut tables = api.edges().subscribe();
    loop {
        let started = Instant::now();
        let told = tables.borrow_and_update().clone();
        let interval = match told {
            None => ANSWER_WITHIN,
            Some(t) => match api.app().edge_probe_plan(group, &t).await {
                Ok(Some(plan)) => {
                    let every = plan.interval;
                    probe_once(&api, plan, (&mut members, &mut addresses)).await;
                    every
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(%group, error = %e, "a group's probe could not be planned");
                    ANSWER_WITHIN
                }
            },
        };
        tokio::time::sleep_until(started + interval).await;
    }
}

/// Claims and probes groups every [`CLAIM`]; the task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let replica = api.providers().replica();
    let mut probing: BTreeMap<Uuid, JoinHandle<()>> = BTreeMap::new();
    let mut tick = tokio::time::interval(CLAIM);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let claimed = match api.app().claim_edge_probes(replica, HOLD).await {
            Ok(groups) => groups,
            Err(e) => {
                // A claim not renewed lapses, and another replica may take the group over:
                // this one stops rather than probe beside it.
                tracing::warn!(error = %e, "edge groups' probes were not claimed");
                Vec::new()
            }
        };
        probing.retain(|g, task| {
            let kept = claimed.contains(g) && !task.is_finished();
            if !kept {
                task.abort();
            }
            kept
        });
        for g in claimed {
            probing.entry(g).or_insert_with(|| tokio::spawn(probe_group(Arc::clone(&api), g)));
        }
    }
}
