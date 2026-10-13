// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the reconciler does to a runtime object: build it from the approved target, start it,
//! stop it, restart it, and destroy it. An object built from an older target is rebuilt before it
//! is started again, so a reconfiguration takes effect at the next start, never silently later.
//! Every object is labelled at creation with what adoption matches it by (§4.4 of
//! docs/02-architecture.md).

use super::{Reconciler, Tracked};
use crate::host::Host;
use crate::runtime::{Prepared, Runtime, RuntimeError};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use std::time::{Duration, Instant};

/// How long a workload is given to stop before it is killed.
const GRACE: Duration = Duration::from_secs(30);

/// Why an action did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failure(pub(super) String);

impl From<RuntimeError> for Failure {
    fn from(e: RuntimeError) -> Self {
        Self(e.to_string())
    }
}

impl From<crate::host::HostError> for Failure {
    fn from(e: crate::host::HostError) -> Self {
        Self(e.0)
    }
}

/// Whether `s` is running.
pub(super) fn running(s: &driver::RuntimeStatus) -> bool {
    s.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING)
}

/// The volume identifier the helper knows a workload's named volume by: an existing volume's
/// own, or one derived from the workload and the name for a volume created with it.
pub(super) fn volume_id(workload: &str, name: &str, existing: Option<&str>) -> String {
    existing.map_or_else(|| format!("{workload}.{name}"), str::to_owned)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// The approved target, prepared: what Create would build, or why the driver refuses.
    pub(super) async fn prepare(
        &self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<driver::PreparedSpec, Failure> {
        let target = a.target.as_option().ok_or_else(|| Failure("no target".into()))?;
        match self.runtime.prepare(&a.workload, target, &a.recipe_manifest).await? {
            Prepared::Spec(spec) => {
                t.rejected = None;
                Ok(*spec)
            }
            Prepared::Rejected(r) => {
                let why = format!("the driver refused the target: {}", r.detail);
                t.rejected = Some(r);
                Err(Failure(why))
            }
        }
    }

    /// Ensures each volume the spec mounts, filling in where the helper put it.
    async fn mount(
        &self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        let volumes = a.target.as_option().and_then(|t| t.body.as_option()).map(|b| &b.volumes);
        for m in spec.mounts.iter_mut().filter(|m| !m.volume.is_empty()) {
            let v = volumes.and_then(|vs| vs.iter().find(|v| v.name == m.volume));
            let request = helper::EnsureVolumeRequest {
                volume: volume_id(&a.workload, &m.volume, v.and_then(|v| v.existing.as_deref())),
                workload: a.workload.clone(),
                storage_class: v.map(|v| v.storage_class.clone()).unwrap_or_default(),
                size_bytes: v.map_or(0, |v| v.size_bytes),
                ..Default::default()
            };
            m.source = self.host.ensure_volume(&request).await?.host_path;
        }
        Ok(())
    }

    /// Builds the runtime object for the approved target, labelled.
    async fn build(&mut self, a: &agent::Assignment, t: &mut Tracked) -> Result<String, Failure> {
        let mut spec = self.prepare(a, t).await?;
        self.mount(a, &mut spec).await?;
        if let Some(identity) = &self.wiring.identity {
            let dir = identity.ensure(&a.workload).map_err(|e| {
                Failure(format!("the workload's identity socket could not be served: {e}"))
            })?;
            spec.identity_dir = dir.to_string_lossy().into_owned();
        }
        let power = a.power.as_option();
        spec.labels = driver::RuntimeLabels {
            workload: a.workload.clone(),
            intent: spec.intent.clone(),
            assignment_generation: a.generation,
            power_cycle: power.map_or(0, |p| p.cycle),
            lease_epoch: a.lease_epoch,
            ..Default::default()
        }
        .into();
        spec.uid_base = a.uid_base;
        spec.uid_count = a.uid_count;
        if a.uid_count > 0 {
            self.host.ensure_ids(&a.workload, a.uid_base, a.uid_count).await?;
        }
        let id = self.runtime.create(&spec).await?;
        t.runtime = Some(id.clone());
        t.labels = spec.labels.as_option().cloned();
        t.status = self.runtime.inspect(&id).await.ok();
        t.stopped_by_us = true;
        if let Err(e) = self.progress.cycled(&a.workload, power.map_or(0, |p| p.cycle)) {
            tracing::error!(error = %e, "progress could not be kept");
        }
        Ok(id)
    }

    /// The runtime object for the approved target: the one held if it was built from it,
    /// otherwise a new one, the old one destroyed first.
    pub(super) async fn current(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<String, Failure> {
        let target = a.target.as_option().map(|t| t.id.as_str()).unwrap_or_default();
        let built_from = t.labels.as_ref().map(|l| l.intent.as_str());
        if let Some(id) = t.runtime.clone() {
            if built_from == Some(target) {
                return Ok(id);
            }
            self.destroy(t).await?;
        }
        self.build(a, t).await
    }

    /// Starts the runtime object for the approved target.
    pub(super) async fn start(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<String, Failure> {
        let id = self.current(a, t).await?;
        if !t.status.as_ref().is_some_and(running) {
            self.runtime.start(&id).await?;
            t.running_since = Some(Instant::now());
        }
        t.stopped_by_us = false;
        t.status = self.runtime.inspect(&id).await.ok();
        Ok(id)
    }

    /// Stops the runtime object, if it is running.
    pub(super) async fn stop(&mut self, t: &mut Tracked) -> Result<(), Failure> {
        let Some(id) = t.runtime.clone() else { return Ok(()) };
        t.stopped_by_us = true;
        if t.status.as_ref().is_some_and(running) {
            self.runtime.stop(&id, "", GRACE).await?;
        }
        t.running_since = None;
        t.status = self.runtime.inspect(&id).await.ok();
        Ok(())
    }

    /// Stops and destroys the runtime object; volumes are not touched.
    pub(super) async fn destroy(&mut self, t: &mut Tracked) -> Result<(), Failure> {
        let Some(id) = t.runtime.clone() else { return Ok(()) };
        self.stop(t).await?;
        match self.runtime.destroy(&id).await {
            Ok(()) | Err(RuntimeError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
        t.runtime = None;
        t.labels = None;
        t.status = None;
        Ok(())
    }

    /// Restarts it once, rebuilt if its target changed, and keeps that the cycle was done.
    pub(super) async fn restart(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<String, Failure> {
        self.stop(t).await?;
        let id = self.start(a, t).await?;
        let cycle = a.power.as_option().map_or(0, |p| p.cycle);
        if let Err(e) = self.progress.cycled(&a.workload, cycle) {
            tracing::error!(error = %e, "progress could not be kept");
        }
        Ok(id)
    }
}
