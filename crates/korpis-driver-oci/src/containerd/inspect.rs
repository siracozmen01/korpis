// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading containers back: every one the driver created, with the labels adoption matches it by
//! (§4.4 of docs/02-architecture.md), what each is doing, and what it uses. Usage is read from
//! the container's own cgroup v2 files, which are what the kernel enforces; CPU is a rate, so it
//! is measured between two readings.

use super::Containerd;
use super::lifecycle::{LABEL_INTENT, LABEL_RUN, LABEL_WORKLOAD};
use crate::error::DriverResult;
use korpis_containerd::containerd::v1::types::Status;
use korpis_proto::korpis::driver::v1 as driver;
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

fn status_of(task: Option<(Status, u32, u32)>) -> driver::RuntimeStatus {
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
    driver::RuntimeStatus { state: state.into(), exit_code, pid, ..Default::default() }
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
        if !self.container_exists(id).await? {
            return Err(crate::error::DriverError::NotFound(id.to_owned()));
        }
        Ok(status_of(self.task(id).await?))
    }

    /// Every container the driver created, with its labels and status.
    pub async fn recover(&self) -> DriverResult<Vec<driver::RuntimeObject>> {
        let mut out = Vec::new();
        for c in self.list().await? {
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
                status: status_of(self.task(&c.id).await?).into(),
                ..Default::default()
            });
        }
        Ok(out)
    }

    /// What `id` uses now.
    pub fn usage(&self, id: &str) -> DriverResult<Usage> {
        let dir =
            std::path::Path::new("/sys/fs/cgroup").join(self.cgroup(id).trim_start_matches('/'));
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
        let dir =
            std::path::Path::new("/sys/fs/cgroup").join(self.cgroup(id).trim_start_matches('/'));
        super::counters::counters(|f| std::fs::read_to_string(dir.join(f)).ok())
    }

    /// Forgets the CPU reading of a container that is gone.
    pub(crate) fn forget_sample(&self, id: &str) {
        if let Ok(mut s) = self.samples.lock() {
            s.remove(id);
        }
    }
}
