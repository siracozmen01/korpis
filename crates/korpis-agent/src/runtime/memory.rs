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

/// The driver name it declares.
pub const NAME: &str = "memory";

#[derive(Debug, Clone)]
struct Object {
    spec: driver::PreparedSpec,
    status: driver::RuntimeStatus,
}

#[derive(Debug, Default)]
struct State {
    objects: BTreeMap<String, Object>,
    next: u64,
    calls: Vec<String>,
    failing: BTreeSet<String>,
    rejecting: Option<driver::Rejection>,
    holding: BTreeSet<String>,
    capturing: bool,
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
        Ok(s)
    }

    fn set(&self, verb: &str, id: &str, to: driver::RuntimeStatus) -> RuntimeResult<()> {
        let mut s = self.call(verb, id)?;
        let object = s.objects.get_mut(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        object.status = to;
        Ok(())
    }

    /// Every call so far, as `verb id`.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.lock().map(|s| s.calls.clone()).unwrap_or_default()
    }

    /// Makes every later call of `verb` fail, or succeed again.
    pub fn fail(&self, verb: &str, failing: bool) {
        if let Ok(mut s) = self.lock() {
            if failing {
                s.failing.insert(verb.to_owned());
            } else {
                s.failing.remove(verb);
            }
        }
    }

    /// Makes every later call of `verb` wait until it is released, as a slow step would; a
    /// held call is recorded as `held verb` when it begins waiting.
    pub fn hold(&self, verb: &str, holding: bool) {
        if let Ok(mut s) = self.lock() {
            if holding {
                s.holding.insert(verb.to_owned());
            } else {
                s.holding.remove(verb);
            }
        }
        self.released.notify_waiters();
    }

    /// Whether `verb` is held, noting the call as held the first time it is.
    fn holds(&self, verb: &str, noted: &mut bool) -> bool {
        let Ok(mut s) = self.lock() else { return false };
        if !s.holding.contains(verb) {
            return false;
        }
        if !*noted {
            s.calls.push(format!("held {verb}"));
            *noted = true;
        }
        true
    }

    /// Waits while `verb` is held.
    async fn held(&self, verb: &str) {
        let mut noted = false;
        loop {
            let released = self.released.notified();
            if !self.holds(verb, &mut noted) {
                return;
            }
            released.await;
        }
    }

    /// Whether a workload whose console mode is not `none` has its console captured into a
    /// stream, as a real driver's prepare asks for: off by default, so a test that does not look
    /// at streams gets no stream volumes.
    pub fn capture_consoles(&self, capturing: bool) {
        if let Ok(mut s) = self.lock() {
            s.capturing = capturing;
        }
    }

    /// Makes every later `prepare` reject with `rejection`, or accept again.
    pub fn reject(&self, rejection: Option<driver::Rejection>) {
        if let Ok(mut s) = self.lock() {
            s.rejecting = rejection;
        }
    }

    /// The object `id` exits by itself with `code`, as a crash would, and says so.
    pub fn exit(&self, id: &str, code: i32) {
        if let Ok(mut s) = self.lock()
            && let Some(o) = s.objects.get_mut(id)
        {
            o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(code));
        }
        let _ = self.changes.send(id.to_owned());
    }

    /// The objects, by identifier: their workload, and whether they are running.
    #[must_use]
    pub fn objects(&self) -> BTreeMap<String, (String, bool)> {
        let Ok(s) = self.lock() else { return BTreeMap::new() };
        let running = |o: &Object| {
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING)
        };
        s.objects
            .iter()
            .map(|(id, o)| (id.clone(), (o.spec.workload.clone(), running(o))))
            .collect()
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
        s.objects.insert(id.clone(), Object { spec: spec.clone(), status: created });
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

    fn stats_now(&self, id: &str) -> RuntimeResult<pb::ResourceUsage> {
        let s = self.lock()?;
        s.objects
            .get(id)
            .map(|_| pb::ResourceUsage::default())
            .ok_or_else(|| RuntimeError::NotFound(id.into()))
    }

    /// Logs `verb id what`, for an object that exists.
    fn console_now(&self, verb: &str, id: &str, what: &str) -> RuntimeResult<()> {
        let s = self.call(verb, &format!("{id} {what}"))?;
        s.objects.get(id).map(|_| ()).ok_or_else(|| RuntimeError::NotFound(id.into()))
    }

    fn signal_now(&self, id: &str) -> RuntimeResult<()> {
        let s = self.call("signal", id)?;
        s.objects.get(id).map(|_| ()).ok_or_else(|| RuntimeError::NotFound(id.into()))
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

    fn stop(
        &self,
        id: &str,
        _signal: &str,
        _grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.stop_now(id))
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

    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<pb::ResourceUsage>> + Send {
        ready(self.stats_now(id))
    }

    fn signal(&self, id: &str, _signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        ready(self.signal_now(id))
    }

    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        let text = String::from_utf8_lossy(data);
        ready(self.console_now(
            "console_input",
            id,
            &format!("{} {} {text}", from.by, from.session),
        ))
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn objects_are_created_once_per_intent_and_outlive_whoever_holds_a_clone() {
        let runtime = InMemory::new();
        let intent = pb::Intent { id: "in_1".into(), ..Default::default() };
        let Prepared::Spec(spec) = runtime.prepare("wl_1", &intent, b"").await.unwrap() else {
            panic!("the memory runtime accepts everything unless told otherwise");
        };
        let id = runtime.create(&spec).await.unwrap();
        assert_eq!(runtime.create(&spec).await.unwrap(), id, "idempotent by workload and intent");
        runtime.start(&id).await.unwrap();
        let other = runtime.clone();
        assert_eq!(other.recover().await.unwrap().len(), 1);
        other.exit(&id, 1);
        let status = runtime.inspect(&id).await.unwrap();
        assert_eq!(status.exit_code, Some(1));
        runtime.fail("start", true);
        assert!(runtime.start(&id).await.is_err());
        runtime.destroy(&id).await.unwrap();
        assert!(runtime.objects().is_empty());
    }
}
