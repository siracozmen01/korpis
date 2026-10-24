// SPDX-License-Identifier: AGPL-3.0-or-later

//! Several drivers on one node (§3 of docs/04-runtimes.md): each tier a node offers is served by
//! its own driver process, and the agent speaks to all of them as one runtime. A prepare goes to
//! the driver the intent names, a create to the driver that prepared the spec, and every other
//! verb to the driver that made the object. Which driver made it is a table filled by create and
//! recover, never a mark added to the identifier: each driver issues its own, and the meter keys
//! its book by them (§2 of docs/15-observability.md), so an object keeps its identifier however
//! many drivers its node runs. An identifier the table lacks is asked of each driver in turn.

use super::{Executed, Prepared, Runtime, RuntimeError, RuntimeResult, Sender};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::broadcast;

/// The drivers of one node, as one runtime; clones share them.
#[derive(Debug)]
pub struct Drivers<R> {
    /// Each driver, by the name it declares.
    each: Arc<Vec<(String, R)>>,
    /// The driver that made each object it has seen, by its place in `drivers`.
    owners: Arc<Mutex<HashMap<String, usize>>>,
    /// Every driver's changes, merged.
    changes: broadcast::Sender<String>,
}

impl<R> Clone for Drivers<R> {
    fn clone(&self) -> Self {
        Self {
            each: Arc::clone(&self.each),
            owners: Arc::clone(&self.owners),
            changes: self.changes.clone(),
        }
    }
}

/// Forwards `from`'s changes to `to` for as long as both are open; a change missed while behind
/// costs only latency, as the reconciler reads every object on its period anyway.
async fn forward(mut from: broadcast::Receiver<String>, to: broadcast::Sender<String>) {
    loop {
        match from.recv().await {
            Ok(id) => drop(to.send(id)),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

impl<R: Runtime> Drivers<R> {
    /// The drivers `drivers` reach, each asked its name; refused if one cannot be asked, or two
    /// declare one name, since an intent names its driver.
    pub async fn new(drivers: Vec<R>) -> RuntimeResult<Self> {
        let (changes, _) = broadcast::channel(256);
        let mut named: Vec<(String, R)> = Vec::new();
        for d in drivers {
            let name = d.describe().await?.name;
            if named.iter().any(|(n, _)| *n == name) {
                return Err(RuntimeError::Failed(format!("two drivers declare the name {name:?}")));
            }
            tokio::spawn(forward(d.changes(), changes.clone()));
            named.push((name, d));
        }
        if named.is_empty() {
            return Err(RuntimeError::Failed("no driver".into()));
        }
        Ok(Self { each: Arc::new(named), owners: Arc::default(), changes })
    }

    fn owners(&self) -> RuntimeResult<MutexGuard<'_, HashMap<String, usize>>> {
        self.owners
            .lock()
            .map_err(|_| RuntimeError::Failed("the drivers' table is poisoned".into()))
    }

    /// The driver named `name`, or the only one when no name is given.
    fn named(&self, name: &str) -> Option<(usize, &R)> {
        if name.is_empty() && self.each.len() == 1 {
            return self.each.first().map(|(_, d)| (0, d));
        }
        self.each.iter().enumerate().find(|(_, (n, _))| n == name).map(|(i, (_, d))| (i, d))
    }

    /// The driver that made object `id`: the table's, or the first that knows it.
    async fn owner(&self, id: &str) -> RuntimeResult<&R> {
        let known = self.owners()?.get(id).copied();
        if let Some((_, d)) = known.and_then(|i| self.each.get(i)) {
            return Ok(d);
        }
        let mut failed = None;
        for (i, (_, d)) in self.each.iter().enumerate() {
            match d.inspect(id).await {
                Ok(_) => {
                    self.owners()?.insert(id.to_owned(), i);
                    return Ok(d);
                }
                Err(RuntimeError::NotFound(_)) => {}
                Err(e) => failed = Some(e),
            }
        }
        Err(failed.unwrap_or_else(|| RuntimeError::NotFound(id.to_owned())))
    }

    /// Every driver's objects, each recorded as its maker's. An identifier two drivers both
    /// report is the first's, and the second's object is left out and said, since no verb could
    /// tell the two apart.
    async fn recover_all(&self) -> RuntimeResult<Vec<driver::RuntimeObject>> {
        let mut reported = Vec::new();
        for (i, (_, d)) in self.each.iter().enumerate() {
            reported.extend(d.recover().await?.into_iter().map(|o| (i, o)));
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut all = Vec::new();
        for (i, o) in reported {
            if let Some(first) = seen.get(&o.runtime_id) {
                let name = |at: usize| self.each.get(at).map_or("", |(n, _)| n.as_str());
                tracing::warn!(id = %o.runtime_id, driver = %name(i), first = %name(*first),
                    "two drivers report one runtime object; left to the first");
                continue;
            }
            seen.insert(o.runtime_id.clone(), i);
            all.push(o);
        }
        self.owners()?.extend(seen);
        Ok(all)
    }

    async fn create_on(&self, spec: &driver::PreparedSpec) -> RuntimeResult<String> {
        let Some((i, d)) = self.named(&spec.driver) else {
            return Err(RuntimeError::Failed(format!("no driver {:?} on this node", spec.driver)));
        };
        let id = d.create(spec).await?;
        self.owners()?.insert(id.clone(), i);
        Ok(id)
    }

    async fn prepare_on(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> RuntimeResult<Prepared> {
        let runtime = intent.body.as_option().and_then(|b| b.runtime.as_option());
        let name = runtime.map_or("", |r| r.driver.as_str());
        match self.named(name) {
            Some((_, d)) => d.prepare(workload, intent, manifest).await,
            None => Ok(Prepared::Rejected(driver::Rejection {
                reason: pb::UnsatisfiableReason::UNSATISFIABLE_REASON_MISSING_DRIVER.into(),
                detail: format!("this node has no {name:?} driver"),
                ..Default::default()
            })),
        }
    }

    async fn destroy_on(&self, id: &str) -> RuntimeResult<()> {
        self.owner(id).await?.destroy(id).await?;
        self.owners()?.remove(id);
        Ok(())
    }

    /// The first driver's declaration; [`Runtime::drivers`] gives every one.
    async fn describe_first(&self) -> RuntimeResult<pb::DriverInfo> {
        let (_, first) = self.each.first().ok_or(RuntimeError::NotFound("driver".into()))?;
        first.describe().await
    }

    async fn describe_all(&self) -> RuntimeResult<Vec<pb::DriverInfo>> {
        let mut all = Vec::with_capacity(self.each.len());
        for (_, d) in self.each.iter() {
            all.push(d.describe().await?);
        }
        Ok(all)
    }

    async fn start_on(&self, id: &str) -> RuntimeResult<()> {
        self.owner(id).await?.start(id).await
    }

    async fn stop_on(&self, id: &str, signal: &str, grace: Duration) -> RuntimeResult<()> {
        self.owner(id).await?.stop(id, signal, grace).await
    }

    async fn kill_on(&self, id: &str) -> RuntimeResult<()> {
        self.owner(id).await?.kill(id).await
    }

    async fn inspect_on(&self, id: &str) -> RuntimeResult<driver::RuntimeStatus> {
        self.owner(id).await?.inspect(id).await
    }

    async fn stats_on(&self, id: &str) -> RuntimeResult<driver::StatsResponse> {
        self.owner(id).await?.stats(id).await
    }

    async fn signal_on(&self, id: &str, signal: &str) -> RuntimeResult<()> {
        self.owner(id).await?.signal(id, signal).await
    }

    async fn exec_on(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
        output_limit: u32,
    ) -> RuntimeResult<Executed> {
        self.owner(id).await?.exec(id, command, timeout, output_limit).await
    }

    async fn ping_on(&self, id: &str, within: Duration) -> RuntimeResult<bool> {
        self.owner(id).await?.ping_guest(id, within).await
    }

    async fn input_on(&self, id: &str, from: Sender<'_>, data: &[u8]) -> RuntimeResult<()> {
        self.owner(id).await?.console_input(id, from, data).await
    }

    async fn resize_on(&self, id: &str, from: Sender<'_>, size: (u16, u16)) -> RuntimeResult<()> {
        self.owner(id).await?.resize_console(id, from, size).await
    }

    async fn serves_on(&self, id: &str) -> RuntimeResult<bool> {
        self.owner(id).await?.serves_files(id).await
    }

    async fn files_on(&self, id: &str, write: bool) -> RuntimeResult<tokio::io::DuplexStream> {
        self.owner(id).await?.files(id, write).await
    }
}

impl<R: Runtime> Runtime for Drivers<R> {
    /// The first driver's declaration; [`Runtime::drivers`] gives every one.
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send {
        self.describe_first()
    }

    fn drivers(&self) -> impl Future<Output = RuntimeResult<Vec<pb::DriverInfo>>> + Send {
        self.describe_all()
    }

    fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> impl Future<Output = RuntimeResult<Prepared>> + Send {
        self.prepare_on(workload, intent, manifest)
    }

    fn create(
        &self,
        spec: &driver::PreparedSpec,
    ) -> impl Future<Output = RuntimeResult<String>> + Send {
        self.create_on(spec)
    }

    fn start(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.start_on(id)
    }

    fn stop(
        &self,
        id: &str,
        signal: &str,
        grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.stop_on(id, signal, grace)
    }

    fn kill(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.kill_on(id)
    }

    fn destroy(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.destroy_on(id)
    }

    fn recover(&self) -> impl Future<Output = RuntimeResult<Vec<driver::RuntimeObject>>> + Send {
        self.recover_all()
    }

    fn inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = RuntimeResult<driver::RuntimeStatus>> + Send {
        self.inspect_on(id)
    }

    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<driver::StatsResponse>> + Send {
        self.stats_on(id)
    }

    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.signal_on(id, signal)
    }

    fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
        output_limit: u32,
    ) -> impl Future<Output = RuntimeResult<Executed>> + Send {
        self.exec_on(id, command, timeout, output_limit)
    }

    fn ping_guest(
        &self,
        id: &str,
        within: Duration,
    ) -> impl Future<Output = RuntimeResult<bool>> + Send {
        self.ping_on(id, within)
    }

    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.input_on(id, from, data)
    }

    fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        size: (u16, u16),
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.resize_on(id, from, size)
    }

    fn serves_files(&self, id: &str) -> impl Future<Output = RuntimeResult<bool>> + Send {
        self.serves_on(id)
    }

    fn files(
        &self,
        id: &str,
        write: bool,
    ) -> impl Future<Output = RuntimeResult<tokio::io::DuplexStream>> + Send {
        self.files_on(id, write)
    }

    fn changes(&self) -> broadcast::Receiver<String> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests;
