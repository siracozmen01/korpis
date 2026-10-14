// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration as sent: an `IntentBody` whose unset parts the recipe completes (§3.1 of
//! docs/09-recipes.md). An unspecified enum, a zero port or size, an empty path or driver, and an
//! unset probe each mean "the recipe's", never a value of their own.

use super::endpoints::{exposure_from_pb, policy_from_pb, protocol_from_pb, tls_from_pb};
use super::health::{health_from_pb, placement_from_pb};
use super::resources::{mode, resources_from_pb};
use super::{console_from_pb, continuity_from_pb, lifecycle_from_pb, tier_from_pb};
use crate::value::map_from_pb;
use crate::{Result, fits, id, invalid};
use korpis_core::id::IdKind;
use korpis_core::workload::declare::{
    Declaration, DeclaredEndpoint, DeclaredRuntime, DeclaredVolume,
};
use korpis_core::workload::spec::endpoints::ShapingSpec;
use korpis_proto::korpis::v1 as pb;

/// The recipe a declaration names: a repository and a tag or digest to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRequest {
    /// The repository.
    pub name: String,
    /// A tag, or a digest when the declaration pins one.
    pub version: String,
}

fn unless<E: buffa::Enumeration + Copy + PartialEq, T>(
    v: buffa::EnumValue<E>,
    unspecified: E,
    read: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    if v.as_known() == Some(unspecified) { Ok(None) } else { read().map(Some) }
}

fn runtime(field: &str, r: Option<&pb::RuntimeSpec>) -> Result<DeclaredRuntime> {
    let Some(r) = r else { return Ok(DeclaredRuntime::default()) };
    Ok(DeclaredRuntime {
        driver: (!r.driver.is_empty()).then(|| r.driver.clone()),
        tier: unless(r.tier, pb::IsolationTier::ISOLATION_TIER_UNSPECIFIED, || {
            tier_from_pb(&format!("{field}.tier"), r.tier)
        })?,
        config: map_from_pb(&format!("{field}.config"), &r.config)?,
    })
}

fn volume(field: &str, v: &pb::VolumeSpec) -> Result<DeclaredVolume> {
    Ok(DeclaredVolume {
        name: v.name.clone(),
        path: (!v.path.is_empty()).then(|| v.path.clone()),
        size_bytes: (v.size_bytes > 0).then_some(v.size_bytes),
        storage_class: v.storage_class.clone(),
        mode: mode(&format!("{field}.mode"), v.mode)?,
        existing: v
            .existing
            .as_deref()
            .map(|e| id(IdKind::Volume, &format!("{field}.existing"), e))
            .transpose()?,
    })
}

fn endpoint(field: &str, e: &pb::EndpointSpec) -> Result<DeclaredEndpoint> {
    let f = |name: &str| format!("{field}.{name}");
    Ok(DeclaredEndpoint {
        name: e.name.clone(),
        protocol: unless(e.protocol, pb::EndpointProtocol::ENDPOINT_PROTOCOL_UNSPECIFIED, || {
            protocol_from_pb(&f("protocol"), e.protocol)
        })?,
        container_port: match e.container_port {
            0 => None,
            p => Some(fits(&f("container_port"), u64::from(p))?),
        },
        exposure: unless(e.exposure, pb::Exposure::EXPOSURE_UNSPECIFIED, || {
            exposure_from_pb(&f("exposure"), e.exposure)
        })?,
        domains: e.domains.clone(),
        tls: e.tls.as_option().map(|t| tls_from_pb(&f("tls"), t)).transpose()?,
        policy: policy_from_pb(&f("policy"), e.policy.as_option())?,
        accounting: e.accounting,
        shaping: e.shaping.as_option().map_or_else(ShapingSpec::default, |s| ShapingSpec {
            egress_bits_per_sec: s.egress_bits_per_sec,
            ingress_bits_per_sec: s.ingress_bits_per_sec,
        }),
    })
}

/// A declaration and the recipe it names, from the field `field`.
pub fn declaration_from_pb(
    field: &str,
    b: &pb::IntentBody,
) -> Result<(RecipeRequest, Declaration)> {
    let f = |name: &str| format!("{field}.{name}");
    let r = b.recipe.as_option().ok_or_else(|| invalid(&f("recipe"), "required"))?;
    let version = if r.digest.is_empty() { r.version.clone() } else { r.digest.clone() };
    if r.name.is_empty() || version.is_empty() {
        return Err(invalid(&f("recipe"), "a name and a version or digest"));
    }
    let declaration = Declaration {
        lifecycle: unless(b.lifecycle, pb::Lifecycle::LIFECYCLE_UNSPECIFIED, || {
            lifecycle_from_pb(&f("lifecycle"), b.lifecycle)
        })?,
        schedule: (!b.schedule.is_empty()).then(|| b.schedule.clone()),
        console: unless(b.console, pb::ConsoleMode::CONSOLE_MODE_UNSPECIFIED, || {
            console_from_pb(&f("console"), b.console)
        })?,
        runtime: runtime(&f("runtime"), b.runtime.as_option())?,
        resources: resources_from_pb(&f("resources"), b.resources.as_option())?,
        volumes: b
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| volume(&format!("{field}.volumes[{i}]"), v))
            .collect::<Result<_>>()?,
        endpoints: b
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| endpoint(&format!("{field}.endpoints[{i}]"), e))
            .collect::<Result<_>>()?,
        health: health_from_pb(&f("health"), b.health.as_option())?,
        config: map_from_pb(&f("config"), &b.config)?,
        placement: placement_from_pb(&f("placement"), b.placement.as_option())?,
        continuity: continuity_from_pb(&f("continuity"), b.continuity.as_option())?,
        dependencies: super::dependencies::dependencies_from_pb(
            &f("dependencies"),
            &b.dependencies,
        )?,
        streams: crate::stream::spec_from_pb(&f("streams"), b.streams.as_option())?,
    };
    Ok((RecipeRequest { name: r.name.clone(), version }, declaration))
}
