// SPDX-License-Identifier: AGPL-3.0-or-later

//! The probes a node makes from no socket of its own (§5 of docs/04-runtimes.md): an `exec`
//! probe, whose command its driver runs inside the workload; a `guest_agent` probe, which its
//! driver asks of the agent inside the workload's guest; and a `heartbeat` probe, checked against
//! when the workload last said on its identity socket that it is alive.

use super::{Outcome, Which};
use crate::health::Tried;
use crate::host::Host;
use crate::reconcile::Reconciler;
use crate::runtime::Runtime;
use korpis_core::workload::health::heard;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most of an exec probe's output read back, for the node's log when it fails.
const EXEC_OUTPUT: u32 = 4096;

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Makes `workload`'s probe `which` in a task of its own, through its driver: `ask` is given
    /// the runtime, the object's identifier, and the probe's timeout, and says what it found.
    fn through_driver<F, Fut>(&mut self, workload: &str, which: Which, now: Instant, ask: F)
    where
        F: FnOnce(Arc<R>, String, Duration) -> Fut + Send + 'static,
        Fut: Future<Output = Tried> + Send + 'static,
    {
        let (tx, runtime) = (self.probed(), Arc::clone(&self.runtime));
        let Some(t) = self.tracked.get_mut(workload) else { return };
        let (Some(life), Some(id)) = (t.probing.life.clone(), t.runtime.clone()) else { return };
        let Some(track) = t.probing.track(which) else { return };
        track.next = now + track.timing.period;
        track.trying = true;
        let (timeout, workload) = (track.timing.timeout, workload.to_owned());
        tokio::spawn(async move {
            let tried = ask(runtime, id, timeout).await;
            let _ = tx.send(Outcome { workload, life, which, tried }).await;
        });
    }

    /// Runs `workload`'s exec probe `which`: `command` inside its runtime object, given the
    /// probe's timeout. An exit status of zero passes, any other or a timeout fails, and a driver
    /// that could not run it judges nothing.
    pub(super) fn run_exec(
        &mut self,
        workload: &str,
        which: Which,
        command: Vec<String>,
        now: Instant,
    ) {
        let named = workload.to_owned();
        self.through_driver(workload, which, now, move |runtime, id, timeout| async move {
            match runtime.exec(&id, &command, timeout, EXEC_OUTPUT).await {
                Ok(e) if e.status == 0 && !e.timed_out => Tried::Passed,
                Ok(e) => {
                    let output = String::from_utf8_lossy(&e.output);
                    tracing::debug!(workload = %named, status = e.status, %output, "an exec probe failed");
                    Tried::Failed
                }
                Err(e) => {
                    tracing::warn!(workload = %named, error = %e, "an exec probe's command was not run");
                    Tried::Unjudged
                }
            }
        });
    }

    /// Asks whether the agent inside `workload`'s guest answers, for its probe `which`, within
    /// the probe's timeout: an answer passes, silence fails, since a guest whose agent does not
    /// answer is one whose kernel or agent has stopped while its hypervisor runs, and a driver
    /// that could not ask judges nothing.
    pub(super) fn ping_guest(&mut self, workload: &str, which: Which, now: Instant) {
        let named = workload.to_owned();
        self.through_driver(workload, which, now, move |runtime, id, timeout| async move {
            match runtime.ping_guest(&id, timeout).await {
                Ok(true) => Tried::Passed,
                Ok(false) => Tried::Failed,
                Err(e) => {
                    tracing::warn!(workload = %named, error = %e, "a guest's agent could not be asked");
                    Tried::Unjudged
                }
            }
        });
    }

    /// Checks `workload`'s heartbeat probe `which` at `now` against when its identity socket last
    /// heard from it in this life; whether the verdict changed.
    pub(super) fn listened(
        &mut self,
        workload: &str,
        which: Which,
        max_interval: Duration,
        now: Instant,
    ) -> bool {
        let last = self.wiring.identity.as_ref().and_then(|i| i.last_beat(workload));
        let Some(t) = self.tracked.get_mut(workload) else { return false };
        let Some((since, _)) = t.probing.life.clone() else { return false };
        let Some(track) = t.probing.track(which) else { return false };
        track.next = now + track.timing.period;
        let last = last.filter(|beat| *beat >= since);
        let from = since + track.timing.initial_delay;
        heard(last, from, max_interval, now).is_some_and(|passed| track.judge.record(passed))
    }
}
