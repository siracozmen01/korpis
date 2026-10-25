// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies: `korpis.v1.Dependency` (§5 of docs/07-networking.md).

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{Result, id, invalid, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::dependency::{DependencyStatus, Reach};
use korpis_core::workload::spec::Dependency;
use korpis_core::workload::spec::dependencies::{DependencyTarget, Injection};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;

pub(crate) fn injection_to_pb(i: Injection) -> pb::DependencyInjection {
    match i {
        Injection::Env => pb::DependencyInjection::DEPENDENCY_INJECTION_ENV,
        Injection::File => pb::DependencyInjection::DEPENDENCY_INJECTION_FILE,
        Injection::Both => pb::DependencyInjection::DEPENDENCY_INJECTION_BOTH,
    }
}

fn injection_from_pb(
    field: &str,
    i: buffa::EnumValue<pb::DependencyInjection>,
) -> Result<Injection> {
    match i.as_known() {
        Some(pb::DependencyInjection::DEPENDENCY_INJECTION_ENV) => Ok(Injection::Env),
        Some(pb::DependencyInjection::DEPENDENCY_INJECTION_FILE) => Ok(Injection::File),
        Some(
            pb::DependencyInjection::DEPENDENCY_INJECTION_BOTH
            | pb::DependencyInjection::DEPENDENCY_INJECTION_UNSPECIFIED,
        ) => Ok(Injection::Both),
        None => Err(invalid(field, "env, file, or both")),
    }
}

/// The wire form of a dependency.
#[must_use]
pub fn dependency_to_pb(d: &Dependency) -> pb::Dependency {
    let target = match &d.target {
        DependencyTarget::Workload(w) => Target::Workload(wire_id(IdKind::Workload, *w)),
        DependencyTarget::Endpoint(e) => Target::Endpoint(wire_id(IdKind::Endpoint, *e)),
        DependencyTarget::Selector(s) => Target::Selector(Box::new(selector_to_pb(s))),
    };
    pb::Dependency {
        name: d.name.clone(),
        target: Some(target),
        inject: injection_to_pb(d.inject).into(),
        required: d.required,
        ..Default::default()
    }
}

/// A dependency from the field `field`.
pub fn dependency_from_pb(field: &str, d: &pb::Dependency) -> Result<Dependency> {
    let target = match d.target.as_ref() {
        Some(Target::Workload(w)) => {
            DependencyTarget::Workload(id(IdKind::Workload, &format!("{field}.workload"), w)?)
        }
        Some(Target::Endpoint(e)) => {
            DependencyTarget::Endpoint(id(IdKind::Endpoint, &format!("{field}.endpoint"), e)?)
        }
        Some(Target::Selector(s)) => {
            DependencyTarget::Selector(selector_from_pb(&format!("{field}.selector"), Some(s))?)
        }
        None => return Err(invalid(field, "a workload, an endpoint, or a selector")),
    };
    Ok(Dependency {
        name: d.name.clone(),
        target,
        inject: injection_from_pb(&format!("{field}.inject"), d.inject)?,
        required: d.required,
    })
}

/// Every dependency of the repeated field `field`.
pub fn dependencies_from_pb(field: &str, ds: &[pb::Dependency]) -> Result<Vec<Dependency>> {
    ds.iter().enumerate().map(|(i, d)| dependency_from_pb(&format!("{field}[{i}]"), d)).collect()
}

/// The wire form of a dependency as its dependent's status shows it.
#[must_use]
pub fn status_to_pb(d: &DependencyStatus) -> pb::DependencyStatus {
    let reach = |r: Reach| match r {
        Reach::SameNode => pb::DependencyReach::DEPENDENCY_REACH_SAME_NODE,
        Reach::Mesh => pb::DependencyReach::DEPENDENCY_REACH_MESH,
        Reach::Unreached => pb::DependencyReach::DEPENDENCY_REACH_UNREACHED,
        Reach::TargetUnplaced => pb::DependencyReach::DEPENDENCY_REACH_TARGET_UNPLACED,
        Reach::DependentUnplaced => pb::DependencyReach::DEPENDENCY_REACH_DEPENDENT_UNPLACED,
    };
    pb::DependencyStatus {
        name: d.name.clone(),
        required: d.required,
        targets: d
            .targets
            .iter()
            .map(|t| pb::DependencyTargetStatus {
                workload: wire_id(IdKind::Workload, t.workload),
                endpoint: t.endpoint.clone(),
                reach: reach(t.reach).into(),
                ..Default::default()
            })
            .collect(),
        left_out: d
            .left_out
            .iter()
            .map(|(w, why)| pb::DependencyLeftOut {
                workload: wire_id(IdKind::Workload, *w),
                reason: why.as_str().to_owned(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}
