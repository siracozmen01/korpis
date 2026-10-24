// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Inspect` and `Stats`: an object's state read from its record, its Firecracker's process, and
//! the sixteen bytes its guest wrote on its config drive before it ended (§6 of
//! docs/04-runtimes.md); its counters from its Firecracker's cgroup, which holds the guest's
//! vCPUs and memory, so what the guest uses is what the kernel counts there.

use super::{Machine, MachineResult, failed};
use crate::object::{Object, Vm};
use crate::process;
use buffa_types::google::protobuf::Timestamp;
use korpis_guest::status::Ended;
use korpis_proto::korpis::driver::v1 as driver;

/// Why a guest ended, where its record does not say by a number.
fn ended(record: Option<Ended>) -> (Option<i32>, &'static str) {
    match record {
        Some(Ended::Exited(code)) => (Some(code), ""),
        Some(Ended::Signalled(signal)) => (Some(128 + signal), ""),
        Some(Ended::OutOfMemory) => (Some(137), "oom_killed"),
        Some(Ended::Failed) => (None, "the guest's first process failed; its console says why"),
        None => (None, "the guest ended without saying how"),
    }
}

fn at(ms: u64) -> Timestamp {
    let seconds = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
    let nanos = i32::try_from(ms % 1000 * 1_000_000).unwrap_or(0);
    Timestamp { seconds, nanos, ..Default::default() }
}

fn running(vm: &Vm) -> driver::RuntimeStatus {
    driver::RuntimeStatus {
        state: driver::RuntimeState::RUNTIME_STATE_RUNNING.into(),
        started_at: at(vm.started_at_ms).into(),
        pid: u32::try_from(vm.pid).unwrap_or(0),
        ..Default::default()
    }
}

impl Machine {
    /// What object `id` is doing now.
    pub fn inspect(&self, id: &str) -> MachineResult<driver::RuntimeStatus> {
        let dir = self.dir(id)?;
        let object = Object::load(&dir).map_err(failed)?;
        let Some(vm) = object.vm else {
            let created = driver::RuntimeState::RUNTIME_STATE_CREATED;
            return Ok(driver::RuntimeStatus { state: created.into(), ..Default::default() });
        };
        if process::alive(&vm) {
            return Ok(running(&vm));
        }
        let mut drive = std::fs::File::open(Object::config(&dir)).map_err(failed)?;
        let (exit_code, reason) = ended(Ended::read(&mut drive).map_err(failed)?);
        Ok(driver::RuntimeStatus {
            state: driver::RuntimeState::RUNTIME_STATE_STOPPED.into(),
            exit_code,
            started_at: at(vm.started_at_ms).into(),
            finished_at: vm.ended_at_ms.map(at).into(),
            reason: reason.to_owned(),
            ..Default::default()
        })
    }

    /// Object `id`'s counters since its Firecracker started, from its cgroup; each absent where
    /// the kernel keeps none.
    pub fn stats(&self, id: &str) -> MachineResult<driver::StatsResponse> {
        self.dir(id)?;
        let cgroup = self.settings.cgroup_of(id);
        let counters =
            korpis_driver_kit::counters::counters(|f| std::fs::read_to_string(cgroup.join(f)).ok());
        let usage = korpis_proto::korpis::v1::ResourceUsage {
            memory_bytes: counters.memory_bytes.unwrap_or(0),
            ..Default::default()
        };
        Ok(driver::StatsResponse {
            usage: usage.into(),
            counters: counters.into(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ending_is_a_status_or_a_reason() {
        assert_eq!(ended(Some(Ended::Exited(3))), (Some(3), ""));
        assert_eq!(ended(Some(Ended::Signalled(9))), (Some(137), ""), "as a shell reports it");
        assert_eq!(ended(Some(Ended::OutOfMemory)), (Some(137), "oom_killed"), "and why");
        assert!(ended(Some(Ended::Failed)).0.is_none() && ended(None).0.is_none());
    }
}
