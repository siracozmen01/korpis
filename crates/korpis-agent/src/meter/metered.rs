// SPDX-License-Identifier: AGPL-3.0-or-later

//! A runtime that tells the meter what it runs: every object it creates or recovers, with the
//! workload it is, and each object's last counters just before it is destroyed, since a cgroup's
//! counters are readable until then and never after. Everything else passes through.

use super::Meter;
use crate::runtime::{Executed, Prepared, Runtime, RuntimeResult, Sender};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// `R`, metered by a [`Meter`].
#[derive(Debug, Clone)]
pub struct Metered<R> {
    inner: R,
    meter: Arc<Meter>,
}

impl<R: Runtime> Metered<R> {
    /// `inner`, telling `meter` what it runs.
    pub const fn new(inner: R, meter: Arc<Meter>) -> Self {
        Self { inner, meter }
    }
}

impl<R: Runtime> Runtime for Metered<R> {
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send {
        self.inner.describe()
    }

    fn drivers(&self) -> impl Future<Output = RuntimeResult<Vec<pb::DriverInfo>>> + Send {
        self.inner.drivers()
    }

    fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> impl Future<Output = RuntimeResult<Prepared>> + Send {
        self.inner.prepare(workload, intent, manifest)
    }

    async fn create(&self, spec: &driver::PreparedSpec) -> RuntimeResult<String> {
        let id = self.inner.create(spec).await?;
        self.meter.track(&id, &spec.workload);
        Ok(id)
    }

    fn start(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.start(id)
    }

    fn stop(
        &self,
        id: &str,
        signal: &str,
        grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.stop(id, signal, grace)
    }

    fn kill(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.kill(id)
    }

    async fn destroy(&self, id: &str) -> RuntimeResult<()> {
        if self.meter.workload_of(id).is_some() {
            let last = self.inner.stats(id).await.ok();
            let counters = last.as_ref().and_then(|s| s.counters.as_option());
            self.meter.ended(id, counters);
        }
        self.inner.destroy(id).await
    }

    async fn recover(&self) -> RuntimeResult<Vec<driver::RuntimeObject>> {
        let objects = self.inner.recover().await?;
        for o in &objects {
            if let Some(labels) = o.labels.as_option()
                && !labels.workload.is_empty()
            {
                self.meter.track(&o.runtime_id, &labels.workload);
            }
        }
        Ok(objects)
    }

    fn inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = RuntimeResult<driver::RuntimeStatus>> + Send {
        self.inner.inspect(id)
    }

    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<driver::StatsResponse>> + Send {
        self.inner.stats(id)
    }

    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.signal(id, signal)
    }

    fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
        output_limit: u32,
    ) -> impl Future<Output = RuntimeResult<Executed>> + Send {
        self.inner.exec(id, command, timeout, output_limit)
    }

    fn ping_guest(
        &self,
        id: &str,
        within: Duration,
    ) -> impl Future<Output = RuntimeResult<bool>> + Send {
        self.inner.ping_guest(id, within)
    }

    fn serves_files(&self, id: &str) -> impl Future<Output = RuntimeResult<bool>> + Send {
        self.inner.serves_files(id)
    }

    fn files(
        &self,
        id: &str,
        write: bool,
    ) -> impl Future<Output = RuntimeResult<tokio::io::DuplexStream>> + Send {
        self.inner.files(id, write)
    }

    fn start_install(
        &self,
        asked: &driver::StartInstallRequest,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.start_install(asked)
    }

    fn install_status(
        &self,
        driver: &str,
        job: &str,
    ) -> impl Future<Output = RuntimeResult<driver::InstallStatusResponse>> + Send {
        self.inner.install_status(driver, job)
    }

    fn forget_install(
        &self,
        driver: &str,
        job: &str,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.forget_install(driver, job)
    }

    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.console_input(id, from, data)
    }

    fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        size: (u16, u16),
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.inner.resize_console(id, from, size)
    }

    fn changes(&self) -> broadcast::Receiver<String> {
        self.inner.changes()
    }
}
