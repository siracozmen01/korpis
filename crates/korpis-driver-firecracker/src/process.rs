// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's Firecracker as a process: named by its process id and the time the kernel says it
//! started, so a process id used again by another process is never taken for it; watched and
//! signalled through a process descriptor opened only once that name has been checked.

use crate::object::Vm;
use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use std::os::fd::OwnedFd;

/// When process `pid` started, in clock ticks since boot, if it is running.
#[must_use]
pub fn start_ticks(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name, in parentheses, may hold spaces; the fields after it are plain. The start time
    // is the twenty-second field, the twentieth after the name.
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Whether `vm` is still running.
#[must_use]
pub fn alive(vm: &Vm) -> bool {
    start_ticks(vm.pid) == Some(vm.start_ticks)
}

/// A descriptor of `vm`'s process, if it is still the one recorded.
#[must_use]
pub fn descriptor(vm: &Vm) -> Option<OwnedFd> {
    let fd = pidfd_open(Pid::from_raw(vm.pid)?, PidfdFlags::empty()).ok()?;
    // Checked after opening: a descriptor names one process for good, so if the name matches
    // now, the descriptor is the recorded process's.
    alive(vm).then_some(fd)
}

/// Waits until the process `fd` names has ended.
pub async fn ended(fd: OwnedFd) {
    if let Ok(watched) = tokio::io::unix::AsyncFd::new(fd) {
        let _ = watched.readable().await;
    }
}

/// Kills `vm`'s process, if it is still the one recorded.
pub fn kill(vm: &Vm) {
    if let Some(fd) = descriptor(vm) {
        let _ = pidfd_send_signal(&fd, Signal::KILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_under_its_own_name_and_not_under_another() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let ticks = start_ticks(pid).unwrap();
        let me = Vm { pid, start_ticks: ticks, started_at_ms: 0, ended_at_ms: None };
        assert!(alive(&me) && descriptor(&me).is_some());
        let reused = Vm { start_ticks: ticks + 1, ..me };
        assert!(!alive(&reused) && descriptor(&reused).is_none(), "a pid used again");
    }
}
