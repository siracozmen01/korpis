// SPDX-License-Identifier: AGPL-3.0-or-later

//! A runtime that runs nothing: runtime objects are records in memory, started and stopped by
//! changing a field. It exists for tests, which share one between the agent before a restart
//! and after it, as a node's containerd outlives its agent, and for trying the agent on a
//! machine without a driver. Every call is logged, so a test can say what was done, and any
//! verb can be made to fail.

use super::{Prepared, Runtime, RuntimeError, RuntimeResult, Sender};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::broadcast;

mod knobs;

/// The driver name it declares.
pub const NAME: &str = "memory";

#[derive(Debug, Clone)]
struct Object {
    spec: driver::PreparedSpec,
    status: driver::RuntimeStatus,
    counters: driver::ObjectCounters,
}

#[derive(Debug, Default)]
struct State {
    objects: BTreeMap<String, Object>,
    next: u64,
    calls: Vec<String>,
    failing: BTreeSet<String>,
    /// Verbs answered as a driver that cannot be reached, or did not answer in time.
    unreachable: BTreeSet<String>,
    rejecting: Option<driver::Rejection>,
    holding: BTreeSet<String>,
    capturing: bool,
    /// Whether the terminating signal is ignored, so only a kill stops an object.
    deaf: bool,
    /// Whether what a stop hook writes is ignored, rather than heeded by exiting.
    unheeding: bool,
}

/// The in-memory runtime; clones share it.
#[derive(Debug, Clone)]
pub struct InMemory {
    state: Arc<Mutex<State>>,
    changes: broadcast::Sender<String>,
    released: Arc<tokio::sync::Notify>,
}

impl Default for InMemory {
    fn default() -> Self {
        Self { state: Arc::default(), changes: broadcast::channel(64).0, released: Arc::default() }
    }
}

fn state(s: driver::RuntimeState, exit_code: Option<i32>) -> driver::RuntimeStatus {
    driver::RuntimeStatus { state: s.into(), exit_code, ..Default::default() }
}

impl InMemory {
    /// An empty runtime.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> RuntimeResult<MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| RuntimeError::Failed("the runtime's lock is poisoned".into()))
    }

    /// Records the call `verb id`, failing it if `verb` was made to fail.
    fn call(&self, verb: &str, id: &str) -> RuntimeResult<MutexGuard<'_, State>> {
        let mut s = self.lock()?;
        s.calls.push(format!("{verb} {id}"));
        if s.failing.contains(verb) {
            return Err(RuntimeError::Failed(format!("{verb} was made to fail")));
        }
        if s.unreachable.contains(verb) {
            return Err(RuntimeError::Unavailable(format!("{verb} was not answered in time")));
        }
        Ok(s)
    }

    fn set(&self, verb: &str, id: &str, to: driver::RuntimeStatus) -> RuntimeResult<()> {
        let mut s = self.call(verb, id)?;
        let object = s.objects.get_mut(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        object.status = to;
        Ok(())
    }
}

fn info() -> pb::DriverInfo {
    pb::DriverInfo {
        name: NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: "korpis.driver.v1".to_owned(),
        healthy: true,
        ..Default::default()
    }
}

/// The verbs, answered at once.
impl InMemory {
    fn prepare_now(&self, workload: &str, intent: &pb::Intent) -> RuntimeResult<Prepared> {
        let s = self.call("prepare", workload)?;
        if let Some(r) = s.rejecting.clone() {
            return Ok(Prepared::Rejected(r));
        }
        let body = intent.body.as_option();
        let console = body.and_then(|b| b.console.as_known());
        let captured = s.capturing && console != Some(pb::ConsoleMode::CONSOLE_MODE_NONE);
        Ok(Prepared::Spec(Box::new(driver::PreparedSpec {
            workload: workload.to_owned(),
            intent: intent.id.clone(),
            rootfs: body.map(|b| b.recipe.digest.clone()).unwrap_or_default(),
            resources: body.and_then(|b| b.resources.as_option().cloned()).into(),
            stream: captured.then(driver::StreamCapture::default).into(),
            ..Default::default()
        })))
    }

    fn create_now(&self, spec: &driver::PreparedSpec) -> RuntimeResult<String> {
        let mut s = self.call("create", &spec.workload)?;
        let run = |s: &driver::PreparedSpec| s.labels.as_option().map(|l| l.run.clone());
        let same = |o: &Object| {
            o.spec.workload == spec.workload
                && o.spec.intent == spec.intent
                && run(&o.spec) == run(spec)
        };
        if let Some((id, _)) = s.objects.iter().find(|(_, o)| same(o)) {
            return Ok(id.clone());
        }
        s.next += 1;
        let id = format!("mem-{}", s.next);
        let created = state(driver::RuntimeState::RUNTIME_STATE_CREATED, None);
        let counters = driver::ObjectCounters::default();
        s.objects.insert(id.clone(), Object { spec: spec.clone(), status: created, counters });
        Ok(id)
    }

    fn start_now(&self, id: &str) -> RuntimeResult<()> {
        self.set("start", id, state(driver::RuntimeState::RUNTIME_STATE_RUNNING, None))
    }

    fn stop_now(&self, id: &str) -> RuntimeResult<()> {
        self.set("stop", id, state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0)))
    }

    fn kill_now(&self, id: &str) -> RuntimeResult<()> {
        self.set("kill", id, state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(137)))
    }

    fn destroy_now(&self, id: &str) -> RuntimeResult<()> {
        let mut s = self.call("destroy", id)?;
        s.objects.remove(id).map(|_| ()).ok_or_else(|| RuntimeError::NotFound(id.into()))
    }

    fn recover_now(&self) -> RuntimeResult<Vec<driver::RuntimeObject>> {
        let s = self.call("recover", "")?;
        Ok(s.objects
            .iter()
            .map(|(id, o)| driver::RuntimeObject {
                runtime_id: id.clone(),
                labels: o.spec.labels.clone(),
                status: o.status.clone().into(),
                ..Default::default()
            })
            .collect())
    }

    fn inspect_now(&self, id: &str) -> RuntimeResult<driver::RuntimeStatus> {
        let s = self.lock()?;
        s.objects.get(id).map(|o| o.status.clone()).ok_or_else(|| RuntimeError::NotFound(id.into()))
    }

    fn stats_now(&self, id: &str) -> RuntimeResult<driver::StatsResponse> {
        let s = self.lock()?;
        let o = s.objects.get(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        Ok(driver::StatsResponse { counters: o.counters.clone().into(), ..Default::default() })
    }

    /// Logs `verb id what`, for an object that exists.
    fn console_now(&self, verb: &str, id: &str, what: &str) -> RuntimeResult<()> {
        let s = self.call(verb, &format!("{id} {what}"))?;
        s.objects.get(id).map(|_| ()).ok_or_else(|| RuntimeError::NotFound(id.into()))
    }

    /// What a stop hook writes (the session `pre_stop`) stops a running object, as the software
    /// a recipe's hook is written for exits on it, unless the runtime was made to ignore it.
    fn heed(&self, id: &str, session: &str) -> RuntimeResult<()> {
        let mut s = self.lock()?;
        let unheeding = s.unheeding;
        let Some(o) = s.objects.get_mut(id) else { return Ok(()) };
        let running =
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
        if session != "pre_stop" || unheeding || !running {
            return Ok(());
        }
        o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0));
        drop(s);
        let _ = self.changes.send(id.to_owned());
        Ok(())
    }

    /// A terminating signal stops a running object, as a process that handles it by exiting
    /// does, unless the runtime was made to ignore it; any other is only recorded.
    fn signal_now(&self, id: &str, signal: &str) -> RuntimeResult<()> {
        let mut s = self.call("signal", id)?;
        let deaf = s.deaf;
        let o = s.objects.get_mut(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        let running =
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
        if !matches!(signal, "" | "SIGTERM" | "SIGINT") || deaf || !running {
            return Ok(());
        }
        o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0));
        drop(s);
        let _ = self.changes.send(id.to_owned());
        Ok(())
    }
}

impl Runtime for InMemory {
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send {
        ready(Ok(info()))
    }

    fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        _manifest: &[u8],
    ) -> impl Future<Output = RuntimeResult<Prepared>> + Send {
        ready(self.prepare_now(workload, intent))
    }

    fn create(
        &self,
        spec: &driver::PreparedSpec,
    ) -> impl Future<Output = RuntimeResult<String>> + Send {
        let (this, spec) = (self.clone(), spec.clone());
        async move {
            this.held("create").await;
            this.create_now(&spec)
        }
    }

    fn start(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.start_now(id))
    }

    /// As a driver does: an object that ignores the terminating signal is killed once `grace`
    /// is over, and the call returns only then.
    fn stop(
        &self,
        id: &str,
        _signal: &str,
        grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        let deaf = self.lock().is_ok_and(|s| s.deaf);
        let (this, id) = (self.clone(), id.to_owned());
        async move {
            if !deaf {
                return this.stop_now(&id);
            }
            tokio::time::sleep(grace).await;
            this.kill_now(&id)
        }
    }

    fn kill(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.kill_now(id))
    }

    fn destroy(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.destroy_now(id))
    }

    fn recover(&self) -> impl Future<Output = RuntimeResult<Vec<driver::RuntimeObject>>> + Send {
        ready(self.recover_now())
    }

    fn inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = RuntimeResult<driver::RuntimeStatus>> + Send {
        ready(self.inspect_now(id))
    }

    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<driver::StatsResponse>> + Send {
        ready(self.stats_now(id))
    }

    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.signal_now(id, signal))
    }

    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        let text = String::from_utf8_lossy(data);
        let logged =
            self.console_now("console_input", id, &format!("{} {} {text}", from.by, from.session));
        ready(logged.and_then(|()| self.heed(id, from.session)))
    }

    fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        (cols, rows): (u16, u16),
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.console_now("resize_console", id, &format!("{} {cols}x{rows}", from.by)))
    }

    fn changes(&self) -> broadcast::Receiver<String> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests;
