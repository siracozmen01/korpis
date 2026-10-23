// SPDX-License-Identifier: AGPL-3.0-or-later

//! The probes a node makes from no socket of its own (§5 of docs/04-runtimes.md): an `exec`
//! probe, whose command its driver runs inside the workload, and a `heartbeat` probe, checked
//! against when the workload last said on its identity socket that it is alive.

use super::{Outcome, Which};
use crate::health::Tried;
use crate::host::Host;
use crate::reconcile::Reconciler;
use crate::runtime::Runtime;
use korpis_core::workload::health::heard;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most of an exec probe's output read back, for the node's log when it fails.
const EXEC_OUTPUT: u32 = 4096;

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Runs `workload`'s exec probe `which` in a task of its own: `command` inside its runtime
    /// object, through its driver, given the probe's timeout. An exit status of zero passes, any
    /// other or a timeout fails, and a driver that could not run it judges nothing.
    pub(super) fn run_exec(
        &mut self,
        workload: &str,
        which: Which,
        command: Vec<String>,
        now: Instant,
    ) {
        let (tx, runtime) = (self.probed(), Arc::clone(&self.runtime));
        let Some(t) = self.tracked.get_mut(workload) else { return };
        let (Some(life), Some(id)) = (t.probing.life.clone(), t.runtime.clone()) else { return };
        let Some(track) = t.probing.track(which) else { return };
        track.next = now + track.timing.period;
        track.trying = true;
        let (timeout, workload) = (track.timing.timeout, workload.to_owned());
        tokio::spawn(async move {
            let tried = match runtime.exec(&id, &command, timeout, EXEC_OUTPUT).await {
                Ok(e) if e.status == 0 && !e.timed_out => Tried::Passed,
                Ok(e) => {
                    let output = String::from_utf8_lossy(&e.output);
                    tracing::debug!(%workload, status = e.status, %output, "an exec probe failed");
                    Tried::Failed
                }
                Err(e) => {
                    tracing::warn!(%workload, error = %e, "an exec probe's command was not run");
                    Tried::Unjudged
                }
            };
            let _ = tx.send(Outcome { workload, life, which, tried }).await;
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
