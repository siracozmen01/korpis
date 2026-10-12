// SPDX-License-Identifier: AGPL-3.0-or-later

//! Health probes and placement.

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{Result, duration, duration_from, fits, invalid};
use korpis_core::workload::spec::health::{HealthSpec, Probe, ProbeKind};
use korpis_core::workload::spec::placement::{AffinityRule, PlacementSpec, Toleration};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::probe::Kind as K;

fn kind_to_pb(k: &ProbeKind) -> K {
    match k {
        ProbeKind::Tcp { endpoint } => {
            K::Tcp(Box::new(pb::TcpProbe { endpoint: endpoint.clone(), ..Default::default() }))
        }
        ProbeKind::Http { endpoint, path, expect_status } => K::Http(Box::new(pb::HttpProbe {
            endpoint: endpoint.clone(),
            path: path.clone(),
            expect_status: u32::from(*expect_status),
            ..Default::default()
        })),
        ProbeKind::Query { protocol, endpoint } => K::Query(Box::new(pb::QueryProbe {
            protocol: protocol.clone(),
            endpoint: endpoint.clone(),
            ..Default::default()
        })),
        ProbeKind::Exec { command } => {
            K::Exec(Box::new(pb::ExecProbe { command: command.clone(), ..Default::default() }))
        }
        ProbeKind::GuestAgent => K::GuestAgent(Box::default()),
        ProbeKind::Exit { success_codes } => K::Exit(Box::new(pb::ExitProbe {
            success_codes: success_codes.clone(),
            ..Default::default()
        })),
        ProbeKind::Heartbeat { max_interval } => K::Heartbeat(Box::new(pb::HeartbeatProbe {
            max_interval: duration(*max_interval).into(),
            ..Default::default()
        })),
    }
}

fn kind_from_pb(field: &str, k: &K) -> Result<ProbeKind> {
    Ok(match k {
        K::Tcp(t) => ProbeKind::Tcp { endpoint: t.endpoint.clone() },
        K::Http(h) => ProbeKind::Http {
            endpoint: h.endpoint.clone(),
            path: h.path.clone(),
            expect_status: fits(
                &format!("{field}.http.expect_status"),
                u64::from(h.expect_status),
            )?,
        },
        K::Query(q) => {
            ProbeKind::Query { protocol: q.protocol.clone(), endpoint: q.endpoint.clone() }
        }
        K::Exec(e) => ProbeKind::Exec { command: e.command.clone() },
        K::GuestAgent(_) => ProbeKind::GuestAgent,
        K::Exit(e) => ProbeKind::Exit { success_codes: e.success_codes.clone() },
        K::Heartbeat(h) => {
            let f = format!("{field}.heartbeat.max_interval");
            let d = h.max_interval.as_option().ok_or_else(|| invalid(&f, "required"))?;
            ProbeKind::Heartbeat { max_interval: duration_from(&f, d)? }
        }
    })
}

fn probe_to_pb(p: &Probe) -> pb::Probe {
    pb::Probe {
        kind: Some(kind_to_pb(&p.kind)),
        period: p.period.map(duration).into(),
        timeout: p.timeout.map(duration).into(),
        failures: p.failures,
        initial_delay: p.initial_delay.map(duration).into(),
        ..Default::default()
    }
}

fn probe_from_pb(field: &str, p: &pb::Probe) -> Result<Probe> {
    let d = |name: &str, v: Option<&buffa_types::google::protobuf::Duration>| {
        v.map(|v| duration_from(&format!("{field}.{name}"), v)).transpose()
    };
    Ok(Probe {
        kind: kind_from_pb(field, p.kind.as_ref().ok_or_else(|| invalid(field, "a probe kind"))?)?,
        period: d("period", p.period.as_option())?,
        timeout: d("timeout", p.timeout.as_option())?,
        failures: p.failures,
        initial_delay: d("initial_delay", p.initial_delay.as_option())?,
    })
}

/// The wire form of a health declaration.
#[must_use]
pub fn health_to_pb(h: &HealthSpec) -> pb::HealthSpec {
    pb::HealthSpec {
        readiness: h.readiness.as_ref().map(probe_to_pb).into(),
        liveness: h.liveness.as_ref().map(probe_to_pb).into(),
        ..Default::default()
    }
}

/// A health declaration from the field `field`.
pub fn health_from_pb(field: &str, h: Option<&pb::HealthSpec>) -> Result<HealthSpec> {
    let Some(h) = h else { return Ok(HealthSpec::default()) };
    let probe = |name: &str, p: Option<&pb::Probe>| {
        p.map(|p| probe_from_pb(&format!("{field}.{name}"), p)).transpose()
    };
    Ok(HealthSpec {
        readiness: probe("readiness", h.readiness.as_option())?,
        liveness: probe("liveness", h.liveness.as_option())?,
    })
}

/// The wire form of a placement declaration.
#[must_use]
pub fn placement_to_pb(p: &PlacementSpec) -> pb::PlacementSpec {
    pb::PlacementSpec {
        node_selector: selector_to_pb(&p.node_selector).into(),
        tolerations: p
            .tolerations
            .iter()
            .map(|t| pb::Toleration {
                key: t.key.clone(),
                value: t.value.clone(),
                ..Default::default()
            })
            .collect(),
        affinity: p
            .affinity
            .iter()
            .map(|a| pb::AffinityRule {
                anti: a.anti,
                required: a.required,
                workloads: selector_to_pb(&a.workloads).into(),
                topology_key: a.topology_key.clone(),
                ..Default::default()
            })
            .collect(),
        preferred_region: p.preferred_region.clone(),
        ..Default::default()
    }
}

/// A placement declaration from the field `field`.
pub fn placement_from_pb(field: &str, p: Option<&pb::PlacementSpec>) -> Result<PlacementSpec> {
    let Some(p) = p else { return Ok(PlacementSpec::default()) };
    let affinity = p
        .affinity
        .iter()
        .enumerate()
        .map(|(i, a)| {
            Ok(AffinityRule {
                anti: a.anti,
                required: a.required,
                workloads: selector_from_pb(
                    &format!("{field}.affinity[{i}].workloads"),
                    a.workloads.as_option(),
                )?,
                topology_key: a.topology_key.clone(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(PlacementSpec {
        node_selector: selector_from_pb(
            &format!("{field}.node_selector"),
            p.node_selector.as_option(),
        )?,
        tolerations: p
            .tolerations
            .iter()
            .map(|t| Toleration { key: t.key.clone(), value: t.value.clone() })
            .collect(),
        affinity,
        preferred_region: p.preferred_region.clone(),
    })
}
