// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading containers back: every one the driver created, with the labels adoption matches it by
//! (§4.4 of docs/02-architecture.md), what each is doing and since when, and what it uses. Usage
//! is read from the container's own cgroup v2 files, which are what the kernel enforces; CPU is a
//! rate, so it is measured between two readings.

use super::Containerd;
use super::lifecycle::{LABEL_INTENT, LABEL_RUN, LABEL_WORKLOAD};
use super::times::{LABEL_STARTED, started_at};
use crate::error::DriverResult;
use korpis_containerd::containerd::v1::types::{Process, Status};
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::BTreeMap;
use std::time::Instant;

/// One reading of a container's CPU time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sample {
    at: Instant,
    usage_usec: u64,
}

/// What a container uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Millicores, over the interval since the previous reading; zero at the first.
    pub cpu_millis: u64,
    /// Bytes of memory charged to it.
    pub memory_bytes: u64,
    /// Processes and threads.
    pub pids: u64,
}

/// The status of a container whose labels are `labels` and whose task's process is `process`:
/// when it last started while it has a task, and when it ended once it has; and, when `oom` says
/// its cgroup's kernel killed a process of it for want of memory and its entry ended by
/// `SIGKILL`, that it was killed for that.
fn status_of(
    process: Option<&Process>,
    labels: &BTreeMap<String, String>,
    oom: bool,
) -> driver::RuntimeStatus {
    let task =
        process.map(|p| (p.status.as_known().unwrap_or(Status::UNKNOWN), p.exit_status, p.pid));
    let (state, exit_code, pid) = match task {
        None => (driver::RuntimeState::RUNTIME_STATE_CREATED, None, 0),
        Some((Status::RUNNING, _, pid)) => (driver::RuntimeState::RUNTIME_STATE_RUNNING, None, pid),
        Some((Status::CREATED, _, pid)) => (driver::RuntimeState::RUNTIME_STATE_CREATED, None, pid),
        Some((Status::PAUSED | Status::PAUSING, _, pid)) => {
            (driver::RuntimeState::RUNTIME_STATE_PAUSED, None, pid)
        }
        Some((Status::STOPPED, code, _)) => {
            (driver::RuntimeState::RUNTIME_STATE_STOPPED, i32::try_from(code).ok(), 0)
        }
        Some((_, _, pid)) => (driver::RuntimeState::RUNTIME_STATE_UNKNOWN, None, pid),
    };
    let started = process.and(labels.get(LABEL_STARTED)).and_then(|v| started_at(v));
    let stopped = state == driver::RuntimeState::RUNTIME_STATE_STOPPED;
    let finished = process.filter(|_| stopped).and_then(|p| p.exited_at.as_option().cloned());
    let killed_for_memory = stopped && oom && exit_code == Some(137);
    driver::RuntimeStatus {
        state: state.into(),
        exit_code,
        pid,
        started_at: started.into(),
        finished_at: finished.into(),
        reason: if killed_for_memory { "oom_killed".into() } else { String::new() },
        ..Default::default()
    }
}

/// A number from one line of a cgroup file.
fn number(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// `usage_usec` from a `cpu.stat`.
fn usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat.lines().find_map(|l| l.strip_prefix("usage_usec ")).and_then(number)
}

impl Containerd {
    /// What `id` is doing.
    pub async fn inspect(&self, id: &str) -> DriverResult<driver::RuntimeStatus> {
        let Some(labels) = self.labels_of(id).await? else {
            return Err(crate::error::DriverError::NotFound(id.to_owned()));
        };
        Ok(status_of(self.process(id).await?.as_ref(), &labels, self.oom_killed(id)))
    }

    /// Every container the driver created, with its labels and status.
    pub async fn recover(&self) -> DriverResult<Vec<driver::RuntimeObject>> {
        let mut out = Vec::new();
        for c in self.list().await? {
            let kept: BTreeMap<String, String> =
                c.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let label = |k: &str| c.labels.get(k).cloned().unwrap_or_default();
            let number = |k: &str| c.labels.get(k).and_then(|v| v.parse().ok()).unwrap_or(0);
            let labels = driver::RuntimeLabels {
                workload: label(LABEL_WORKLOAD),
                intent: label(LABEL_INTENT),
                assignment_generation: number("korpis.assignment_generation"),
                power_cycle: number("korpis.power_cycle"),
                lease_epoch: number("korpis.lease_epoch"),
                run: label(LABEL_RUN),
                ..Default::default()
            };
            out.push(driver::RuntimeObject {
                runtime_id: c.id.clone(),
                labels: labels.into(),
                status: status_of(
                    self.process(&c.id).await?.as_ref(),
                    &kept,
                    self.oom_killed(&c.id),
                )
                .into(),
                cpus: korpis_driver_kit::cores::cpus_of(&label(super::lifecycle::LABEL_CPUS)),
                ..Default::default()
            });
        }
        Ok(out)
    }

    /// Whether the kernel has killed a process of `id` for want of memory, as its cgroup counts
    /// it: the cgroup stays until the task is deleted, which a start of a new one does, so the
    /// count is the task's.
    fn oom_killed(&self, id: &str) -> bool {
        let dir = self.cores.dir(id);
        let events = std::fs::read_to_string(dir.join("memory.events")).unwrap_or_default();
        let kills = events.lines().find_map(|l| l.strip_prefix("oom_kill ")).and_then(number);
        kills.is_some_and(|n| n > 0)
    }

    /// What `id` uses now.
    pub fn usage(&self, id: &str) -> DriverResult<Usage> {
        let dir = self.cores.dir(id);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        let now =
            Sample { at: Instant::now(), usage_usec: usage_usec(&read("cpu.stat")).unwrap_or(0) };
        let mut samples = self
            .samples
            .lock()
            .map_err(|_| crate::error::DriverError::Failed("poisoned".into()))?;
        let cpu_millis = samples.insert(id.to_owned(), now).map_or(0, |before| {
            let used = now.usage_usec.saturating_sub(before.usage_usec);
            let elapsed =
                u64::try_from(now.at.duration_since(before.at).as_micros()).unwrap_or(u64::MAX);
            used.saturating_mul(1000).checked_div(elapsed).unwrap_or(0)
        });
        Ok(Usage {
            cpu_millis,
            memory_bytes: number(&read("memory.current")).unwrap_or(0),
            pids: number(&read("pids.current")).unwrap_or(0),
        })
    }

    /// `id`'s counters since its cgroup was made, each absent where its file is.
    pub fn counters(&self, id: &str) -> driver::ObjectCounters {
        let dir = self.cores.dir(id);
        korpis_driver_kit::counters::counters(|f| std::fs::read_to_string(dir.join(f)).ok())
    }

    /// Forgets the CPU reading of a container that is gone.
    pub(crate) fn forget_sample(&self, id: &str) {
        if let Ok(mut s) = self.samples.lock() {
            s.remove(id);
        }
    }
}
