// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the reconciler does to a runtime object: build it from the approved target, start it,
//! stop it, restart it, and destroy it. An object built from an older target is rebuilt before it
//! is started again, so a reconfiguration takes effect at the next start, never silently later.
//! Every object is labelled at creation with what adoption matches it by (§4.4 of
//! docs/02-architecture.md).

use super::volume::Why;
use super::{Reconciler, Tracked};
use crate::host::Host;
use crate::runtime::{Prepared, Runtime, RuntimeError};
use korpis_core::workload::stream::StreamKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use std::path::Path;
use std::time::Instant;

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

/// The volume identifier the helper knows the volume `a`'s workload mounts as `name` by: an
/// existing volume's, as the control plane resolved it from what its node reported, or one
/// derived from the workload and the name for a volume created with it.
pub(crate) fn volume_id(a: &agent::Assignment, name: &str) -> String {
    a.volume_ids.get(name).cloned().unwrap_or_else(|| format!("{}.{name}", a.workload))
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
    pub(super) async fn mount(
        &self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        let volumes = a.target.as_option().and_then(|t| t.body.as_option()).map(|b| &b.volumes);
        let block_device = self.wiring.driver_kinds.block_device(&spec.driver);
        for m in spec.mounts.iter_mut().filter(|m| !m.volume.is_empty()) {
            let v = volumes.and_then(|vs| vs.iter().find(|v| v.name == m.volume));
            let request = helper::EnsureVolumeRequest {
                volume: volume_id(a, &m.volume),
                workload: a.workload.clone(),
                storage_class: v.map(|v| v.storage_class.clone()).unwrap_or_default(),
                size_bytes: v.map_or(0, |v| v.size_bytes),
                block_device,
                ..Default::default()
            };
            m.source = self.ensure_volume(&m.volume, Why::Declared, &request).await?.host_path;
        }
        Ok(())
    }

    /// Ensures the console stream's volume, sized to what its limits let it hold, and watches
    /// its changes from then on; nothing if the driver captures no stream. A run's console is a
    /// stream of its own, in a volume of its own, so runs never share a writer (§4 of
    /// docs/14-streams.md).
    async fn stream(
        &mut self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
        run: &str,
    ) -> Result<(), Failure> {
        let Some(capture) = spec.stream.as_option_mut() else { return Ok(()) };
        let body = a.target.as_option().and_then(|t| t.body.as_option());
        let streams = body.and_then(|b| b.streams.as_option());
        let limits = korpis_wire::stream::spec_from_pb("streams", streams)
            .map_err(|e| Failure(format!("the stream's limits: {e}")))?;
        let volume = match run {
            "" => StreamKind::Console.volume(&a.workload),
            run => StreamKind::Console.run_volume(&a.workload, run),
        };
        let request = helper::EnsureVolumeRequest {
            volume,
            workload: a.workload.clone(),
            size_bytes: limits.disk_bound(),
            ..Default::default()
        };
        // Each run's is reported under its run, so the control plane tells them apart.
        let name = match run {
            "" => StreamKind::Console.volume_name(),
            run => StreamKind::Console.run_volume_name(run),
        };
        capture.dir = self.ensure_volume(&name, Why::Stream, &request).await?.host_path;
        self.streams
            .watch(&super::streams::stream_name(&a.workload, run), Path::new(&capture.dir))
            .map_err(|e| Failure(format!("the stream could not be watched: {e}")))
    }

    /// Builds the runtime object for the approved target, labelled.
    async fn build(&mut self, a: &agent::Assignment, t: &mut Tracked) -> Result<String, Failure> {
        self.build_for(a, t, "").await
    }

    /// Builds the runtime object for the approved target, executing `run` when it names one,
    /// labelled.
    pub(super) async fn build_for(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        run: &str,
    ) -> Result<String, Failure> {
        let mut spec = self.prepare(a, t).await?;
        self.mount(a, &mut spec).await?;
        self.stream(a, &mut spec, run).await?;
        let mut dir = None;
        if let Some(identity) = &self.wiring.identity {
            let served = identity.ensure(&a.workload).map_err(|e| {
                Failure(format!("the workload's identity socket could not be served: {e}"))
            })?;
            identity.relay(&a.workload, a.reaches_api).map_err(|e| {
                Failure(format!("the control plane's relay could not be served: {e}"))
            })?;
            spec.identity_dir = served.to_string_lossy().into_owned();
            dir = Some(served);
        }
        self.managed(a, dir.as_deref()).await?;
        let power = a.power.as_option();
        spec.labels = driver::RuntimeLabels {
            workload: a.workload.clone(),
            intent: spec.intent.clone(),
            assignment_generation: a.generation,
            power_cycle: power.map_or(0, |p| p.cycle),
            lease_epoch: a.lease_epoch,
            run: run.to_owned(),
            ..Default::default()
        }
        .into();
        spec.uid_base = a.uid_base;
        spec.uid_count = a.uid_count;
        if a.uid_count > 0 {
            self.host.ensure_ids(&a.workload, a.uid_base, a.uid_count).await?;
            self.network(a, &mut spec).await?;
        }
        self.inject(a, &mut spec);
        if let Some(dir) = &dir {
            self.write_dependencies(a, dir).map_err(|e| {
                Failure(format!("the workload's dependencies could not be written: {e}"))
            })?;
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
    /// otherwise a new one, the old one destroyed first; `None` while the old one is still
    /// stopping, which a later pass carries on.
    pub(super) async fn current(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<Option<String>, Failure> {
        let target = a.target.as_option().map(|t| t.id.as_str()).unwrap_or_default();
        let built_from = t.labels.as_ref().map(|l| l.intent.as_str());
        if let Some(id) = t.runtime.clone() {
            if built_from == Some(target) {
                return Ok(Some(id));
            }
            if !self.destroy(t).await? {
                return Ok(None);
            }
        }
        self.build(a, t).await.map(Some)
    }

    /// Starts the runtime object for the approved target; `None` while an older one is still
    /// stopping.
    pub(super) async fn start(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<Option<String>, Failure> {
        let Some(id) = self.current(a, t).await? else { return Ok(None) };
        if !t.status.as_ref().is_some_and(running) {
            self.runtime.start(&id).await?;
            t.running_since = Some(Instant::now());
        }
        t.stopped_by_us = false;
        t.status = self.runtime.inspect(&id).await.ok();
        Ok(Some(id))
    }

    /// Moves the stop of the runtime object on (§6 of docs/02-architecture.md); whether it has
    /// stopped, as it has when there is none.
    pub(super) async fn stop(&mut self, t: &mut Tracked) -> Result<bool, Failure> {
        let Some(id) = t.runtime.clone() else { return Ok(true) };
        t.stopped_by_us = true;
        let workload = t.labels.as_ref().map(|l| l.workload.clone()).unwrap_or_default();
        let hook = t.pre_stop.as_ref().map(|h| (workload.as_str(), h));
        let stopped = self.halt(&id, &mut t.halting, hook).await?;
        if stopped {
            t.running_since = None;
        }
        t.status = self.runtime.inspect(&id).await.ok();
        Ok(stopped)
    }

    /// Stops and destroys the runtime object; volumes, and the stream's, are not touched.
    /// Whether it is gone: not while it is still stopping.
    pub(super) async fn destroy(&mut self, t: &mut Tracked) -> Result<bool, Failure> {
        let Some(id) = t.runtime.clone() else { return Ok(true) };
        if !self.stop(t).await? {
            return Ok(false);
        }
        match self.runtime.destroy(&id).await {
            Ok(()) | Err(RuntimeError::NotFound(_)) => {}
            // Not answered in time, or not reached: nothing says the object is not gone or going,
            // and destroying it again is harmless, so a later pass asks again rather than fail
            // the retirement for a moment of the driver's (§6 of docs/02-architecture.md).
            Err(RuntimeError::Unavailable(why)) => {
                tracing::warn!(%why, "the object was not destroyed yet; a later pass asks again");
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        t.runtime = None;
        t.labels = None;
        t.status = None;
        Ok(true)
    }

    /// Restarts it once, rebuilt if its target changed, and keeps that the cycle was done;
    /// `None` while it is still stopping.
    pub(super) async fn restart(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<Option<String>, Failure> {
        if !self.stop(t).await? {
            return Ok(None);
        }
        let Some(id) = self.start(a, t).await? else { return Ok(None) };
        let cycle = a.power.as_option().map_or(0, |p| p.cycle);
        if let Err(e) = self.progress.cycled(&a.workload, cycle) {
            tracing::error!(error = %e, "progress could not be kept");
        }
        Ok(Some(id))
    }
}
