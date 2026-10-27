// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration as a subject writes it, and how the recipe completes it into an intent body
//! (§3.1 of docs/09-recipes.md).
//!
//! A subject declares what it cares about and the recipe supplies the rest: the shape its
//! preset proposes, its probes, the tier it runs at. The control plane completes the declaration
//! once, at declaration, against the recipe resolved to a digest, and the intent stores the
//! completed body. So what an intent means never depends on reading its recipe again, and
//! `korpis plan` shows the body that will actually run.

mod parts;

pub use parts::{DeclaredEndpoint, DeclaredRuntime, DeclaredVolume, default_driver};

use super::spec::devices::DeviceRequest;
use super::spec::{
    ConsoleMode, Continuity, Dependency, EgressPolicy, HealthSpec, IntentBody, Lifecycle,
    PlacementSpec, ReplicaFailover, ResourceSpec, RunPolicy, SpecError, StreamSpec,
};
use crate::recipe::{Recipe, RecipeRef};
use crate::value::ValueMap;

/// A declaration: an intent body whose parts may be left for the recipe to supply.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Declaration {
    /// How long it runs; the preset's when absent.
    pub lifecycle: Option<Lifecycle>,
    /// The cron schedule, for scheduled workloads.
    pub schedule: Option<String>,
    /// The zone the schedule is read in.
    pub timezone: Option<String>,
    /// How its runs are made and kept, for task and scheduled workloads.
    pub runs: Option<RunPolicy>,
    /// Its console; the preset's when absent.
    pub console: Option<ConsoleMode>,
    /// Its runtime; the recipe's first target when absent.
    pub runtime: DeclaredRuntime,
    /// What it reserves and is limited to; the preset's recommendation when nothing is declared.
    pub resources: ResourceSpec,
    /// The devices it holds while placed; a recipe proposes none.
    pub devices: Vec<DeviceRequest>,
    /// Volumes, merged by name into the preset's.
    pub volumes: Vec<DeclaredVolume>,
    /// Endpoints, merged by name into the preset's.
    pub endpoints: Vec<DeclaredEndpoint>,
    /// Probes; each one absent is the recipe's.
    pub health: HealthSpec,
    /// Configuration as declared; resolved against the schema afterwards (Rule K-17).
    pub config: ValueMap,
    /// Where it may run.
    pub placement: PlacementSpec,
    /// What happens when a node loses its lease.
    pub continuity: Continuity,
    /// Whether failover may start it on a replica.
    pub replica_failover: ReplicaFailover,
    /// What it reaches by name.
    pub dependencies: Vec<Dependency>,
    /// The limits of its console stream.
    pub streams: StreamSpec,
    /// Where it may connect out, beyond the egress default.
    pub egress: EgressPolicy,
}

impl Declaration {
    /// The intent body this declaration means, given its recipe. The body is validated: a
    /// declaration the recipe cannot complete, or whose completion is inconsistent, is refused,
    /// as is one whose volumes leave somewhere its recipe installs into outside every volume
    /// (§4 of docs/09-recipes.md).
    pub fn complete(self, recipe_ref: RecipeRef, recipe: &Recipe) -> Result<IntentBody, SpecError> {
        let preset = &recipe.preset;
        let lifecycle = self.lifecycle.unwrap_or(preset.lifecycle);
        // The policy and the zone are stored as they apply, the defaults written out, so that a
        // later default cannot change what a stored intent means.
        let runs = match lifecycle {
            Lifecycle::Persistent => self.runs,
            Lifecycle::Task | Lifecycle::Scheduled => Some(self.runs.unwrap_or_default()),
        };
        let timezone = match &self.schedule {
            Some(_) => Some(self.timezone.unwrap_or_else(|| "UTC".to_owned())),
            None => self.timezone,
        };
        let body = IntentBody {
            recipe: recipe_ref,
            lifecycle,
            schedule: self.schedule,
            timezone,
            runs,
            console: self.console.unwrap_or(preset.console),
            runtime: self.runtime.complete(&recipe.runtime)?,
            resources: parts::resources(self.resources, preset.resources)?,
            devices: self.devices,
            volumes: parts::volumes(self.volumes, &preset.volumes)?,
            endpoints: parts::endpoints(self.endpoints, &preset.endpoints)?,
            health: HealthSpec {
                readiness: self.health.readiness.or_else(|| recipe.probes.readiness.clone()),
                liveness: self.health.liveness.or_else(|| recipe.probes.liveness.clone()),
            },
            config: self.config,
            placement: self.placement,
            continuity: self.continuity,
            replica_failover: self.replica_failover,
            dependencies: self.dependencies,
            streams: self.streams,
            egress: self.egress,
        };
        body.validate()?;
        recipe
            .check_targets(&body.volumes)
            .map_err(|e| SpecError::new("volumes", format!("{}: {}", e.field, e.message)))?;
        Ok(body)
    }
}
