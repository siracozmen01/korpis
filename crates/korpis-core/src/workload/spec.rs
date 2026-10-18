// SPDX-License-Identifier: AGPL-3.0-or-later

//! The intent body: what a subject declares should be true (§3.3 of docs/01-model.md).
//! Validation here is of the declaration's own consistency; whether its configuration fits its
//! recipe is the recipe's question (`crate::recipe`), and whether the caller may declare it is
//! the application's.

pub mod cron;
pub mod dependencies;
pub mod egress;
pub mod endpoints;
pub mod health;
pub mod placement;
pub mod resources;
pub mod runs;
pub mod streams;
pub mod volumes;

pub use cron::Schedule;
pub use dependencies::Dependency;
pub use egress::EgressPolicy;
pub use endpoints::EndpointSpec;
pub use health::HealthSpec;
pub use placement::PlacementSpec;
pub use resources::ResourceSpec;
pub use runs::{Concurrency, RunPolicy};
pub use streams::StreamSpec;
pub use volumes::VolumeSpec;

use crate::recipe::RecipeRef;
use crate::value::ValueMap;

/// Why a declaration is inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct SpecError {
    /// The offending field, as a path into the body, e.g. `endpoints[1].container_port`.
    pub field: String,
    /// What is wrong.
    pub message: String,
}

impl SpecError {
    /// An error at `field`.
    pub fn new(field: &str, message: impl Into<String>) -> Self {
        Self { field: field.to_owned(), message: message.into() }
    }
}

/// Names of volumes and endpoints: `[a-z0-9]([a-z0-9-]*[a-z0-9])?`, at most 63 bytes.
pub(crate) fn check_name(field: &str, name: &str) -> Result<(), SpecError> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 63
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b.first().is_some_and(u8::is_ascii_alphanumeric)
        && b.last().is_some_and(u8::is_ascii_alphanumeric);
    if ok { Ok(()) } else { Err(SpecError::new(field, "a DNS label: [a-z0-9-], at most 63")) }
}

/// How long the workload is meant to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Runs until stopped.
    Persistent,
    /// Runs to completion once.
    Task,
    /// Runs to completion on a schedule.
    Scheduled,
}

/// What console the workload offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleMode {
    /// None.
    None,
    /// Its output, read-only.
    Logs,
    /// Its terminal, read and write.
    Interactive,
    /// A virtual machine's serial console.
    Serial,
}

/// The isolation tiers of docs/04-runtimes.md, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IsolationTier {
    /// A confined process.
    Process,
    /// A container.
    Container,
    /// A microVM.
    MicroVm,
    /// A full virtual machine.
    Vm,
}

/// Which driver runs it, at which tier.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSpec {
    /// The driver, e.g. `oci`.
    pub driver: String,
    /// The isolation tier.
    pub tier: IsolationTier,
    /// Driver-specific configuration, validated by the driver's own schema.
    pub config: ValueMap,
}

/// What a node does with the workload when it loses its lease (§4.5 of
/// docs/02-architecture.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Continuity {
    /// Derived from the volumes: stop if any is attachable from another node.
    #[default]
    Derived,
    /// Keep running.
    KeepRunning,
    /// Stop.
    Stop,
}

/// The declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentBody {
    /// The recipe, resolved to its digest.
    pub recipe: RecipeRef,
    /// How long it runs.
    pub lifecycle: Lifecycle,
    /// The cron schedule; present exactly when `lifecycle` is scheduled.
    pub schedule: Option<String>,
    /// The IANA zone the schedule is read in; UTC when absent; only with a schedule.
    pub timezone: Option<String>,
    /// How its runs are made and kept; only for task and scheduled workloads, the defaults when
    /// absent (§4 of docs/05-scheduling.md).
    pub runs: Option<RunPolicy>,
    /// Its console.
    pub console: ConsoleMode,
    /// Its runtime.
    pub runtime: RuntimeSpec,
    /// What it reserves and is limited to.
    pub resources: ResourceSpec,
    /// What it mounts.
    pub volumes: Vec<VolumeSpec>,
    /// How it is reached.
    pub endpoints: Vec<EndpointSpec>,
    /// How its health is judged.
    pub health: HealthSpec,
    /// Its configuration, validated against the recipe's schema (Rule K-17).
    pub config: ValueMap,
    /// Where it may run.
    pub placement: PlacementSpec,
    /// What happens when a node loses its lease.
    pub continuity: Continuity,
    /// What it reaches by name.
    pub dependencies: Vec<Dependency>,
    /// The limits of its console stream.
    pub streams: StreamSpec,
    /// Where it may connect out, beyond the egress default.
    pub egress: EgressPolicy,
}

impl IntentBody {
    /// Checks the declaration's own consistency.
    pub fn validate(&self) -> Result<(), SpecError> {
        self.recipe.validate().map_err(|e| SpecError::new("recipe", e.to_string()))?;
        self.validate_timing()?;
        if self.runtime.driver.is_empty() {
            return Err(SpecError::new("runtime.driver", "required"));
        }
        if self.console == ConsoleMode::Serial && self.runtime.tier < IsolationTier::MicroVm {
            return Err(SpecError::new("console", "a serial console needs a virtual machine"));
        }
        self.resources.validate()?;
        volumes::validate(&self.volumes)?;
        endpoints::validate(&self.endpoints)?;
        self.health.validate(&self.endpoints)?;
        dependencies::validate(&self.dependencies)?;
        self.streams.validate()?;
        self.egress.validate()?;
        self.placement.validate()
    }
}

impl IntentBody {
    /// Its run policy, the defaults where it declares none.
    #[must_use]
    pub fn run_policy(&self) -> RunPolicy {
        self.runs.unwrap_or_default()
    }

    /// When it runs: a schedule exactly when scheduled, a zone only with a schedule, and a run
    /// policy only for a workload that has runs.
    fn validate_timing(&self) -> Result<(), SpecError> {
        match (self.lifecycle, &self.schedule) {
            (Lifecycle::Scheduled, None) => Err(SpecError::new("schedule", "required to schedule")),
            (Lifecycle::Scheduled, Some(s)) => cron::validate(s),
            (_, Some(_)) => Err(SpecError::new("schedule", "only for scheduled workloads")),
            (_, None) => Ok(()),
        }?;
        match (&self.timezone, &self.schedule) {
            (Some(_), None) => return Err(SpecError::new("timezone", "only with a schedule")),
            (Some(zone), Some(_)) => runs::validate_zone(zone)?,
            (None, _) => {}
        }
        match (&self.runs, self.lifecycle) {
            (Some(_), Lifecycle::Persistent) => {
                Err(SpecError::new("runs", "only for task and scheduled workloads"))
            }
            (Some(policy), _) => policy.validate(),
            (None, _) => Ok(()),
        }
    }
}

/// A body for the crate's own tests: a container under the OCI driver, with nothing declared.
#[cfg(test)]
pub(crate) fn fixture() -> IntentBody {
    IntentBody {
        recipe: RecipeRef {
            name: "r".into(),
            version: "1".into(),
            digest: String::new(),
            trust: None,
            providers: Vec::new(),
        },
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        timezone: None,
        runs: None,
        console: ConsoleMode::None,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::new(),
        },
        resources: ResourceSpec::default(),
        volumes: Vec::new(),
        endpoints: Vec::new(),
        health: HealthSpec::default(),
        config: ValueMap::new(),
        placement: PlacementSpec::default(),
        continuity: Continuity::Derived,
        dependencies: Vec::new(),
        streams: StreamSpec::default(),
        egress: EgressPolicy::default(),
    }
}
