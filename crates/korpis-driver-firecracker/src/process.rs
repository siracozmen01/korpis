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

/// Whether process `pid` runs as `uid`, by its real user id.
#[must_use]
pub fn runs_as(pid: i32, uid: u32) -> bool {
    let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else { return false };
    let real = status.lines().find_map(|l| l.strip_prefix("Uid:"));
    real.and_then(|ids| ids.split_whitespace().next()).and_then(|r| r.parse().ok()) == Some(uid)
}

/// Whether process `pid` is in the cgroup at `cgroup`, which only root can move a process into.
#[must_use]
pub fn in_cgroup(pid: i32, cgroup: &std::path::Path) -> bool {
    std::fs::read_to_string(cgroup.join("cgroup.procs"))
        .is_ok_and(|procs| procs.lines().any(|p| p.trim().parse() == Ok(pid)))
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

    #[test]
    fn a_process_is_named_by_its_user_and_its_cgroup() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let uid = rustix::process::getuid().as_raw();
        assert!(runs_as(pid, uid) && !runs_as(pid, uid.wrapping_add(1)));
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let path = own.trim().strip_prefix("0::").unwrap().trim_start_matches('/');
        let cgroup = std::path::Path::new("/sys/fs/cgroup").join(path);
        assert!(in_cgroup(pid, &cgroup));
        assert!(!in_cgroup(1, &cgroup) || cgroup == std::path::Path::new("/sys/fs/cgroup"));
    }
}
