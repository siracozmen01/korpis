// SPDX-License-Identifier: AGPL-3.0-or-later

//! A task or scheduled workload's runs on this node (§4.2 and §4.5 of docs/05-scheduling.md).
//! The workload's own object is never started: each run its assignment names gets an object of
//! its own, built as the workload's is and labelled with the run, started once and never
//! brought back. Its timeout is the node's to enforce, so it holds while the control plane is
//! unreachable. A run no longer named is stopped. A run that ended is reported once and its
//! object kept while it is still named, so a node that has not yet been told it ended never runs
//! it twice, and is removed once it is not.

use super::act::{Failure, running};
use super::{Reconciler, Tracked, effect, text};
use crate::host::Host;
use crate::runtime::{Runtime, RuntimeError};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// How long a run is given to stop before it is killed, at its timeout or when it is no longer
/// named.
const GRACE: Duration = Duration::from_secs(30);

/// What the reconciler knows of one run's runtime object.
#[derive(Debug, Clone, Default)]
pub(super) struct RunTracked {
    /// Its runtime object; empty when it could not be made.
    pub(super) runtime: String,
    /// When its timeout runs out, counted from when its process started, once known.
    pub(super) deadline: Option<SystemTime>,
    /// Why the node stopped it, when it did: `timeout` or `cancelled`.
    pub(super) stopped_for: Option<&'static str>,
    /// Whether its end was journaled.
    pub(super) ended: bool,
    /// Whether its process ran when last looked at.
    pub(super) running: bool,
}

/// Whether `a`'s target is a workload that has runs.
pub(super) fn has_runs(a: &agent::Assignment) -> bool {
    use korpis_proto::korpis::v1::Lifecycle;
    let lifecycle = a.target.as_option().and_then(|t| t.body.as_option()).map(|b| b.lifecycle);
    matches!(
        lifecycle.and_then(|l| l.as_known()),
        Some(Lifecycle::LIFECYCLE_TASK | Lifecycle::LIFECYCLE_SCHEDULED)
    )
}

/// When `r`'s timeout runs out: counted from when its process started, as the driver says, or
/// from now when it does not, which a run adopted after an agent restart can only overestimate.
fn deadline(
    r: Option<&agent::RunRef>,
    status: Option<&driver::RuntimeStatus>,
) -> Option<SystemTime> {
    let timeout = r?.timeout.as_option()?;
    let timeout = Duration::from_secs(u64::try_from(timeout.seconds).ok()?);
    let started = status
        .and_then(|s| s.started_at.as_option())
        .and_then(|t| korpis_wire::time("started_at", t).ok())
        .unwrap_or_else(SystemTime::now);
    started.checked_add(timeout)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// The runs `a` names, each started once when `start` allows it, and those it no longer
    /// names stopped. A node whose lease expired starts none, and still stops each at its
    /// timeout.
    pub(super) async fn runs(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        start: bool,
    ) -> Result<(), Failure> {
        let named: BTreeMap<&str, &agent::RunRef> =
            a.runs.iter().map(|r| (r.run.as_str(), r)).collect();
        for run in t.runs.keys().cloned().collect::<Vec<_>>() {
            let still = named.get(run.as_str()).copied();
            self.tend_run(&a.workload, t, &run, still).await?;
        }
        for (run, r) in named.into_iter().filter(|_| start) {
            if !t.runs.contains_key(run) {
                self.start_run(a, t, r).await;
            }
        }
        Ok(())
    }

    /// Stops every run held: the workload is stopped or retired.
    pub(super) async fn stop_runs(
        &mut self,
        workload: &str,
        t: &mut Tracked,
    ) -> Result<(), Failure> {
        for run in t.runs.keys().cloned().collect::<Vec<_>>() {
            self.tend_run(workload, t, &run, None).await?;
        }
        Ok(())
    }

    /// One held run: stopped at its timeout or once it is not named, its end journaled once,
    /// and its object removed once it has ended and is not named.
    async fn tend_run(
        &mut self,
        workload: &str,
        t: &mut Tracked,
        run: &str,
        named: Option<&agent::RunRef>,
    ) -> Result<(), Failure> {
        let Some(mut r) = t.runs.remove(run) else { return Ok(()) };
        let mut status = self.status_of(&r.runtime).await;
        if r.deadline.is_none() {
            r.deadline = deadline(named, status.as_ref());
        }
        let named = named.is_some();
        let why = match (status.as_ref().is_some_and(running), named) {
            (false, _) => None,
            (true, true) => r.deadline.filter(|d| SystemTime::now() >= *d).map(|_| "timeout"),
            (true, false) => Some("cancelled"),
        };
        if let Some(why) = why {
            r.stopped_for.get_or_insert(why);
            if let Err(e) = self.runtime.stop(&r.runtime, "", GRACE).await {
                // Kept, so the next pass stops it again: a failed stop never forgets a run.
                t.runs.insert(run.to_owned(), r);
                return Err(e.into());
            }
            status = self.status_of(&r.runtime).await;
        }
        r.running = status.as_ref().is_some_and(running);
        if r.running {
            t.runs.insert(run.to_owned(), r);
            return Ok(());
        }
        if !r.ended {
            r.ended = self.journal_end(workload, run, &r, status.as_ref()).await;
        }
        if named {
            t.runs.insert(run.to_owned(), r);
            return Ok(());
        }
        if !r.runtime.is_empty() {
            match self.runtime.destroy(&r.runtime).await {
                Ok(()) | Err(RuntimeError::NotFound(_)) => {}
                Err(e) => {
                    t.runs.insert(run.to_owned(), r);
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    /// What `runtime` is doing; `None` when it is gone or was never made.
    async fn status_of(&self, runtime: &str) -> Option<driver::RuntimeStatus> {
        if runtime.is_empty() {
            return None;
        }
        self.runtime.inspect(runtime).await.ok()
    }

    /// Builds and starts the object of `r`, and journals that it started; one that could not be
    /// made or started is journaled as ended, failed, and never tried again while it is named.
    async fn start_run(&mut self, a: &agent::Assignment, t: &mut Tracked, r: &agent::RunRef) {
        let mut scratch = Tracked::default();
        // An object made but not started is kept with the failure, so it is removed with the
        // run's and never left behind untracked.
        let made = match self.build_for(a, &mut scratch, &r.run).await {
            Ok(id) => match self.runtime.start(&id).await {
                Ok(()) => Ok(id),
                Err(e) => Err((Some(id), Failure::from(e))),
            },
            Err(e) => Err((None, e)),
        };
        match made {
            Ok(runtime) => {
                let mut e = effect(Some(&a.workload), "run.started", true);
                e.after.insert("run".into(), text(&r.run));
                e.after.insert("runtime_id".into(), text(&runtime));
                self.record(e).await;
                let status = self.status_of(&runtime).await;
                let deadline = deadline(Some(r), status.as_ref());
                let running = status.as_ref().is_some_and(running);
                let held = RunTracked { runtime, deadline, running, ..RunTracked::default() };
                t.runs.insert(r.run.clone(), held);
            }
            Err((runtime, Failure(why))) => {
                tracing::warn!(workload = %a.workload, run = %r.run, error = %why, "a run did not start");
                let mut e = effect(Some(&a.workload), "run.ended", false);
                e.after.insert("run".into(), text(&r.run));
                e.after.insert("cause".into(), text("exit"));
                e.error =
                    korpis_proto::korpis::v1::Error { message: why, ..Default::default() }.into();
                let ended = self.record(e).await;
                let runtime = runtime.unwrap_or_default();
                t.runs
                    .insert(r.run.clone(), RunTracked { runtime, ended, ..RunTracked::default() });
            }
        }
    }

    /// Journals that `run` ended, with its exit status and why; whether it is on disk.
    async fn journal_end(
        &self,
        workload: &str,
        run: &str,
        r: &RunTracked,
        status: Option<&driver::RuntimeStatus>,
    ) -> bool {
        let exit = status.and_then(|s| s.exit_code);
        let mut e = effect(Some(workload), "run.ended", true);
        e.after.insert("run".into(), text(run));
        e.after.insert("cause".into(), text(r.stopped_for.unwrap_or("exit")));
        if let Some(code) = exit {
            e.after.insert("exit_status".into(), text(&code.to_string()));
        }
        self.record(e).await
    }
}
