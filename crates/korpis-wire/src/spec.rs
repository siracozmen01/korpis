// SPDX-License-Identifier: AGPL-3.0-or-later

//! The intent body: `korpis.v1.IntentBody` and its parts.

pub mod declare;
pub mod endpoints;
pub mod health;
pub mod resources;

use crate::recipe::{ref_from_pb, ref_to_pb};
use crate::value::{map_from_pb, map_to_pb};
use crate::{Result, invalid};
use korpis_core::workload::spec::{
    ConsoleMode, Continuity, IntentBody, IsolationTier, Lifecycle, RuntimeSpec,
};
use korpis_proto::korpis::v1 as pb;

enum_map!(lifecycle_to_pb, lifecycle_from_pb, Lifecycle, pb::Lifecycle, "a lifecycle", {
    Lifecycle::Persistent => pb::Lifecycle::LIFECYCLE_PERSISTENT,
    Lifecycle::Task => pb::Lifecycle::LIFECYCLE_TASK,
    Lifecycle::Scheduled => pb::Lifecycle::LIFECYCLE_SCHEDULED,
});

enum_map!(console_to_pb, console_from_pb, ConsoleMode, pb::ConsoleMode, "a console mode", {
    ConsoleMode::None => pb::ConsoleMode::CONSOLE_MODE_NONE,
    ConsoleMode::Logs => pb::ConsoleMode::CONSOLE_MODE_LOGS,
    ConsoleMode::Interactive => pb::ConsoleMode::CONSOLE_MODE_INTERACTIVE,
    ConsoleMode::Serial => pb::ConsoleMode::CONSOLE_MODE_SERIAL,
});

enum_map!(tier_to_pb, tier_from_pb, IsolationTier, pb::IsolationTier, "an isolation tier", {
    IsolationTier::Process => pb::IsolationTier::ISOLATION_TIER_PROCESS,
    IsolationTier::Container => pb::IsolationTier::ISOLATION_TIER_CONTAINER,
    IsolationTier::MicroVm => pb::IsolationTier::ISOLATION_TIER_MICROVM,
    IsolationTier::Vm => pb::IsolationTier::ISOLATION_TIER_VM,
});

/// The wire form of a continuity; `Derived` is the unspecified value.
#[must_use]
pub fn continuity_to_pb(c: Continuity) -> pb::OnLeaseExpiry {
    match c {
        Continuity::Derived => pb::OnLeaseExpiry::ON_LEASE_EXPIRY_UNSPECIFIED,
        Continuity::KeepRunning => pb::OnLeaseExpiry::ON_LEASE_EXPIRY_KEEP_RUNNING,
        Continuity::Stop => pb::OnLeaseExpiry::ON_LEASE_EXPIRY_STOP,
    }
}

fn continuity_from_pb(field: &str, c: Option<&pb::ContinuitySpec>) -> Result<Continuity> {
    let Some(c) = c else { return Ok(Continuity::Derived) };
    match c.on_lease_expiry.as_known() {
        Some(pb::OnLeaseExpiry::ON_LEASE_EXPIRY_UNSPECIFIED) => Ok(Continuity::Derived),
        Some(pb::OnLeaseExpiry::ON_LEASE_EXPIRY_KEEP_RUNNING) => Ok(Continuity::KeepRunning),
        Some(pb::OnLeaseExpiry::ON_LEASE_EXPIRY_STOP) => Ok(Continuity::Stop),
        None => Err(invalid(&format!("{field}.on_lease_expiry"), "unknown")),
    }
}

/// The wire form of an intent body.
#[must_use]
pub fn body_to_pb(b: &IntentBody) -> pb::IntentBody {
    pb::IntentBody {
        recipe: ref_to_pb(&b.recipe).into(),
        lifecycle: lifecycle_to_pb(b.lifecycle).into(),
        schedule: b.schedule.clone().unwrap_or_default(),
        console: console_to_pb(b.console).into(),
        runtime: pb::RuntimeSpec {
            driver: b.runtime.driver.clone(),
            tier: tier_to_pb(b.runtime.tier).into(),
            config: map_to_pb(&b.runtime.config),
            ..Default::default()
        }
        .into(),
        resources: resources::resources_to_pb(&b.resources).into(),
        volumes: b.volumes.iter().map(resources::volume_to_pb).collect(),
        endpoints: b.endpoints.iter().map(endpoints::endpoint_to_pb).collect(),
        health: health::health_to_pb(&b.health).into(),
        config: map_to_pb(&b.config),
        placement: health::placement_to_pb(&b.placement).into(),
        continuity: pb::ContinuitySpec {
            on_lease_expiry: continuity_to_pb(b.continuity).into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn runtime_from_pb(field: &str, r: Option<&pb::RuntimeSpec>) -> Result<RuntimeSpec> {
    let r = r.ok_or_else(|| invalid(field, "required"))?;
    Ok(RuntimeSpec {
        driver: r.driver.clone(),
        tier: tier_from_pb(&format!("{field}.tier"), r.tier)?,
        config: map_from_pb(&format!("{field}.config"), &r.config)?,
    })
}

/// An intent body from the field `field`. Only the translation is checked here; the body's own
/// consistency is `IntentBody::validate`, and its fit to the recipe is the recipe's.
pub fn body_from_pb(field: &str, b: &pb::IntentBody) -> Result<IntentBody> {
    let f = |name: &str| format!("{field}.{name}");
    let recipe = b.recipe.as_option().ok_or_else(|| invalid(&f("recipe"), "required"))?;
    Ok(IntentBody {
        recipe: ref_from_pb(recipe),
        lifecycle: lifecycle_from_pb(&f("lifecycle"), b.lifecycle)?,
        schedule: (!b.schedule.is_empty()).then(|| b.schedule.clone()),
        console: console_from_pb(&f("console"), b.console)?,
        runtime: runtime_from_pb(&f("runtime"), b.runtime.as_option())?,
        resources: resources::resources_from_pb(&f("resources"), b.resources.as_option())?,
        volumes: b
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| resources::volume_from_pb(&format!("{field}.volumes[{i}]"), v))
            .collect::<Result<_>>()?,
        endpoints: b
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| endpoints::endpoint_from_pb(&format!("{field}.endpoints[{i}]"), e))
            .collect::<Result<_>>()?,
        health: health::health_from_pb(&f("health"), b.health.as_option())?,
        config: map_from_pb(&f("config"), &b.config)?,
        placement: health::placement_from_pb(&f("placement"), b.placement.as_option())?,
        continuity: continuity_from_pb(&f("continuity"), b.continuity.as_option())?,
    })
}
