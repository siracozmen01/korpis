// SPDX-License-Identifier: AGPL-3.0-or-later

//! A query probe's provider calls on the wire (§5 of docs/04-runtimes.md):
//! `korpis.v1.HealthStep`, the rounds a node ran, and `korpis.v1.HealthNext`, what the provider
//! answers. A `HealthNext` naming neither a round nor a verdict, or a round without its wait, is no
//! answer at all.

use crate::{IdKind, duration, wire_id};
use korpis_core::extension::health::{HealthNext, HealthStep, Round, Transport};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::health_next::Next;

fn transport_to_pb(t: Transport) -> pb::HealthTransport {
    match t {
        Transport::Tcp => pb::HealthTransport::HEALTH_TRANSPORT_TCP,
        Transport::Udp => pb::HealthTransport::HEALTH_TRANSPORT_UDP,
    }
}

/// A round as the wire carries it.
#[must_use]
pub fn round_to_pb(r: &Round) -> pb::HealthRound {
    pb::HealthRound {
        sent: r.sent.clone(),
        received: r.received.clone(),
        closed: r.closed,
        ..Default::default()
    }
}

/// A round as a node reported it.
#[must_use]
pub fn round_from_pb(r: &pb::HealthRound) -> Round {
    Round { sent: r.sent.clone(), received: r.received.clone(), closed: r.closed }
}

/// A step as it goes down a provider's stream.
#[must_use]
pub fn step_to_pb(s: &HealthStep) -> pb::HealthStep {
    pb::HealthStep {
        protocol: s.protocol.clone(),
        workload: wire_id(IdKind::Workload, s.workload),
        endpoint: s.endpoint.clone(),
        transport: transport_to_pb(s.transport).into(),
        rounds: s.rounds.iter().map(round_to_pb).collect(),
        ..Default::default()
    }
}

/// What a provider answered, if it answered anything a node can act on.
#[must_use]
pub fn next_from_pb(n: &pb::HealthNext) -> Option<HealthNext> {
    match n.next.as_ref()? {
        Next::Send(s) => Some(HealthNext::Send {
            data: s.data.clone(),
            read_at_most: s.read_at_most,
            wait: crate::duration_from("wait", s.wait.as_option()?).ok()?,
        }),
        Next::Verdict(v) => Some(HealthNext::Verdict {
            healthy: v.healthy,
            reason: v.reason.clone(),
            detail: v.detail.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }),
    }
}

/// What a node is told to do next.
#[must_use]
pub fn next_to_pb(n: &HealthNext) -> pb::HealthNext {
    let next = match n {
        HealthNext::Send { data, read_at_most, wait } => Next::Send(Box::new(pb::HealthSend {
            data: data.clone(),
            read_at_most: *read_at_most,
            wait: duration(*wait).into(),
            ..Default::default()
        })),
        HealthNext::Verdict { healthy, reason, detail } => {
            Next::Verdict(Box::new(pb::HealthVerdict {
                healthy: *healthy,
                reason: reason.clone(),
                detail: detail.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                ..Default::default()
            }))
        }
    };
    pb::HealthNext { next: Some(next), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_round_and_a_verdict_survive_the_wire_and_an_empty_answer_is_none() {
        let send = HealthNext::Send {
            data: b"ping".to_vec(),
            read_at_most: 16,
            wait: Duration::from_secs(2),
        };
        assert_eq!(next_from_pb(&next_to_pb(&send)), Some(send));
        let verdict = HealthNext::Verdict {
            healthy: false,
            reason: "said nothing".into(),
            detail: [("players".to_owned(), "3".to_owned())].into(),
        };
        assert_eq!(next_from_pb(&next_to_pb(&verdict)), Some(verdict));
        assert_eq!(next_from_pb(&pb::HealthNext::default()), None);
        let round = Round { sent: b"a".to_vec(), received: b"b".to_vec(), closed: true };
        assert_eq!(round_from_pb(&round_to_pb(&round)), round);
    }
}
