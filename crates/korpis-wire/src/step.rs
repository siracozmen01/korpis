// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe steps' provider calls on the wire (§4 of docs/09-recipes.md): `korpis.v1.RunStep` and
//! `korpis.v1.StepResult`.

use crate::value::{from_pb, map_to_pb};
use crate::{IdKind, wire_id};
use korpis_core::extension::{Platform, StepArtifact, StepResult};
use korpis_core::value::ValueMap;
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

/// A step request as it goes down a provider's stream.
#[must_use]
pub fn run_step_to_pb(
    step: &str,
    arguments: &ValueMap,
    workload: Uuid,
    platform: &Platform,
) -> pb::RunStep {
    pb::RunStep {
        step: step.to_owned(),
        arguments: map_to_pb(arguments),
        workload: wire_id(IdKind::Workload, workload),
        platform: pb::Platform {
            os: platform.os.clone(),
            arch: platform.arch.clone(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// A step result as a provider answered it; whether the node can act on it is the domain's to
/// judge ([`StepResult::problem`]).
#[must_use]
pub fn step_result_from_pb(r: &pb::StepResult) -> StepResult {
    StepResult {
        artifacts: r
            .artifacts
            .iter()
            .map(|a| StepArtifact {
                path: a.path.clone(),
                url: a.url.clone(),
                sha256: a.sha256.clone(),
                size: a.size,
                mode: a.mode,
                extract: a.extract,
            })
            .collect(),
        pinned: r.pinned,
        resolved: r.resolved.clone(),
    }
}

/// Arguments as a recipe step gives them; a malformed value names its field.
pub fn arguments_from_pb<'a>(
    field: &str,
    m: impl IntoIterator<Item = (&'a String, &'a pb::Value)>,
) -> crate::Result<ValueMap> {
    m.into_iter().map(|(k, v)| Ok((k.clone(), from_pb(&format!("{field}.{k}"), v)?))).collect()
}
