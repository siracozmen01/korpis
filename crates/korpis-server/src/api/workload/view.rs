// SPDX-License-Identifier: AGPL-3.0-or-later

//! The responses of `WorkloadService` that describe a workload: its status and its placement,
//! and the set a declaration names.

use super::super::error;
use super::super::wire::placement::{bound_by_to_pb, explanation_to_pb};
use super::super::wire::plan::plan_to_pb;
use super::super::wire::spec::tier_to_pb;
use super::super::wire::workload::{convergence_to_pb, shown_intent_to_pb, workload_to_pb};
use super::super::wire::{self, Refuse, timestamp, wire_id};
use connectrpc::ConnectError;
use korpis_app::workload::{Classified, PlacementView, SetDeclaration, WorkloadView};
use korpis_core::id::IdKind;
use korpis_core::placement::Placement;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::node::{holds_to_pb, resources_to_pb};
use korpis_wire::observation::observation_to_pb;

/// Everything a client needs to render one workload (§4.3 of docs/02-architecture.md), its
/// placement as the caller may see it. The observation is empty until a node reports one.
pub(super) fn status(v: &WorkloadView) -> pb::WorkloadStatus {
    pb::WorkloadStatus {
        endpoints: endpoints(v),
        certificates: certificates(v),
        names: names(v),
        newest_run: v.newest_run.as_ref().map(korpis_wire::run::run_to_pb).into(),
        held_by_suspension: v.held_by_suspension,
        dependencies: v
            .dependencies
            .iter()
            .map(korpis_wire::spec::dependencies::status_to_pb)
            .collect(),
        named_rules: v
            .named_rules
            .iter()
            .map(korpis_wire::spec::dependencies::named_rule_to_pb)
            .collect(),
        dedicated: v.dedicated.as_ref().map(korpis_wire::address::dedicated_status_to_pb).into(),
        workload: workload_to_pb(&v.workload, &v.power).into(),
        target: v.target.as_ref().map(|t| shown_intent_to_pb(t, &v.withheld.target)).into(),
        head: shown_intent_to_pb(&v.head, &v.withheld.head).into(),
        convergence: convergence_to_pb(v.convergence).into(),
        active_plan: v.active_plan.as_ref().map(plan_to_pb).into(),
        placement: v.placement.as_ref().map(|p| placement_to_pb(v.workload.id, p)).into(),
        unplaced: v
            .unplaced
            .as_ref()
            .map(|u| pb::Unplaced {
                intent: wire_id(IdKind::Intent, u.intent),
                since: timestamp(u.since).into(),
                explanation: explanation_to_pb(&u.explanation).into(),
                ..Default::default()
            })
            .into(),
        observation: v
            .observation
            .as_ref()
            .map(|o| {
                // The node is named only to a caller who may see where the workload is placed.
                let visible = v.placement.as_ref().and_then(|p| p.node) == Some(o.observation.node);
                observation_to_pb(&o.observation, o.current, visible)
            })
            .into(),
        drift: v
            .drift
            .as_ref()
            .map(|d| pb::Drift {
                set: wire_id(IdKind::Set, d.set),
                intent: wire_id(IdKind::Intent, v.head.id),
                declared_by: korpis_wire::principal::to_pb(&v.head.declared_by).into(),
                declared_at: timestamp(v.head.declared_at).into(),
                set_intent: d.set_intent.map(|i| wire_id(IdKind::Intent, i)),
                effect: d.effect.map(|e| wire_id(IdKind::Effect, e)),
                ..Default::default()
            })
            .into(),
        ..Default::default()
    }
}

/// Its endpoints as placed; the node is named only to a caller who may see where the workload is
/// placed, the addresses being what a client connects to.
fn endpoints(v: &WorkloadView) -> Vec<pb::Endpoint> {
    let placed = v.placement.as_ref().and_then(|p| p.node);
    v.endpoints
        .iter()
        .map(|(e, addresses)| {
            let mut shown = korpis_wire::endpoint::endpoint_to_pb(e, addresses);
            if placed != Some(e.node)
                && let Some(a) = shown.address.as_option_mut()
            {
                a.node = String::new();
            }
            shown
        })
        .collect()
}

/// The certificate of each of its ingress endpoints, without its chain.
fn certificates(v: &WorkloadView) -> Vec<pb::EndpointCertificate> {
    v.certificates
        .iter()
        .map(|(endpoint, c)| pb::EndpointCertificate {
            endpoint: endpoint.clone(),
            certificate: pb::Certificate {
                chain: String::new(),
                ..korpis_wire::certificate::certificate_to_pb(c)
            }
            .into(),
            ..Default::default()
        })
        .collect()
}

/// Each name its ingress endpoints route, in DNS.
fn names(v: &WorkloadView) -> Vec<pb::EndpointName> {
    use korpis_core::dns::published::RecordState as S;
    use pb::EndpointNameState as P;
    v.names
        .iter()
        .map(|(endpoint, r)| pb::EndpointName {
            endpoint: endpoint.clone(),
            name: r.name.clone(),
            addresses: r.wanted.iter().flatten().map(ToString::to_string).collect(),
            state: match r.state {
                S::Pending => P::ENDPOINT_NAME_STATE_PENDING,
                S::Published => P::ENDPOINT_NAME_STATE_PUBLISHED,
                S::Unmanaged => P::ENDPOINT_NAME_STATE_UNMANAGED,
                S::Failed => P::ENDPOINT_NAME_STATE_FAILED,
            }
            .into(),
            provider: r.provider.clone().unwrap_or_default(),
            reason: r.reason.clone().unwrap_or_default(),
            ..Default::default()
        })
        .collect()
}

/// The set a request names, if it names one.
pub(super) fn set_name(
    field: &str,
    name: &str,
) -> Result<Option<korpis_core::tenancy::Name>, ConnectError> {
    (!name.is_empty()).then(|| wire::name(field, name)).transpose().refuse()
}

/// The set a declaration goes through; `adopt` and `reconcile` mean nothing without one.
pub(super) fn through_set(
    req: &pb::DeclareIntentRequest,
) -> Result<Option<SetDeclaration>, ConnectError> {
    let Some(name) = set_name("set", &req.set)? else {
        if req.adopt || req.reconcile {
            return Err(error::invalid("set", "adopt and reconcile go through a set"));
        }
        return Ok(None);
    };
    Ok(Some(SetDeclaration { name, adopt: req.adopt, reconcile: req.reconcile }))
}

/// A placement made by hand, as its maker sees it: the node they named, when, and why. The
/// derivation is read with the workload, filtered for whoever reads it.
pub(super) fn placed(p: &Placement) -> pb::PlaceWorkloadResponse {
    pb::PlaceWorkloadResponse { placement: placement_pb(p).into(), ..Default::default() }
}

/// The answer to confirming a held placement.
pub(super) fn confirmed(p: &Placement) -> pb::ConfirmPlacementResponse {
    pb::ConfirmPlacementResponse { placement: placement_pb(p).into(), ..Default::default() }
}

fn placement_pb(p: &Placement) -> pb::Placement {
    pb::Placement {
        workload: wire_id(IdKind::Workload, p.workload),
        node: wire_id(IdKind::Node, p.node),
        bound_at: timestamp(p.bound_at).into(),
        bound_by: bound_by_to_pb(p.bound_by).into(),
        reason: p.reason.clone(),
        authoritative: p.authoritative,
        intent: wire_id(IdKind::Intent, p.intent),
        reserved: resources_to_pb(&p.reserved).into(),
        held_at: p.held_at.map(timestamp).into(),
        devices: holds_to_pb(&p.devices),
        cores: p.cores.clone(),
        ..Default::default()
    }
}

pub(super) fn placement_to_pb(workload: uuid::Uuid, p: &PlacementView) -> pb::Placement {
    pb::Placement {
        workload: wire_id(IdKind::Workload, workload),
        node: p.node.map(|n| wire_id(IdKind::Node, n)).unwrap_or_default(),
        bound_at: timestamp(p.bound_at).into(),
        bound_by: bound_by_to_pb(p.bound_by).into(),
        reason: p.reason.clone(),
        explanation: explanation_to_pb(&p.explanation).into(),
        authoritative: p.authoritative,
        region: p.region.clone().unwrap_or_default(),
        tier: tier_to_pb(p.tier).into(),
        intent: wire_id(IdKind::Intent, p.intent),
        reserved: resources_to_pb(&p.reserved).into(),
        held_at: p.held_at.map(timestamp).into(),
        devices: holds_to_pb(&p.devices),
        cores: p.cores.clone(),
        ..Default::default()
    }
}

/// The answer to setting a workload's classification overrides.
pub(super) fn classified(c: &Classified) -> pb::SetClassificationResponse {
    pb::SetClassificationResponse {
        workload: workload_to_pb(&c.workload, &c.power).into(),
        ..Default::default()
    }
}
