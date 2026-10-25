// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's dedicated cores (§4.2 of docs/04-runtimes.md, §5.2 of docs/05-scheduling.md). Its
//! Firecracker runs in a cgroup of its own under the driver's `dedicated` cgroup, the jailer
//! writing the cores' `cpuset` and, unless the host shares them, making it a partition before
//! Firecracker runs; the guest is given one vCPU for each of their logical CPUs, no CPU quota, and
//! each vCPU's thread pinned to a CPU of its own. The cores are checked against the topology the
//! driver reads itself, and the partition against what the kernel says it made.

use crate::guest::MAX_VCPUS;
use crate::jail::Shape;
use korpis_driver_kit::cores::{Cores, Cpuset, Host};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a guest's vCPU threads are waited for, to be pinned.
const PIN_BOUND: Duration = Duration::from_secs(5);

/// The logical CPUs `cpuset` names, in order.
fn cpus_of(cpuset: &Cpuset) -> Vec<u32> {
    korpis_topology::cpu_list(&cpuset.cpus).map(|c| c.into_iter().collect()).unwrap_or_default()
}

/// The `cpuset` of the cores `spec` holds, checked against the topology as `cores` reads it;
/// none when it holds none.
///
/// # Errors
/// A core the machine does not have, CPUs not that core's, CPU 0's, or more CPUs than a guest
/// has vCPUs.
pub fn given(
    cores: &Cores,
    held: &[korpis_proto::korpis::v1::Core],
) -> Result<Option<Cpuset>, String> {
    let cpuset = cores.given(held)?;
    if let Some(set) = &cpuset {
        let count = u64::try_from(cpus_of(set).len()).unwrap_or(u64::MAX);
        if count > MAX_VCPUS {
            return Err(format!("{count} CPUs held, and a guest has at most {MAX_VCPUS} vCPUs"));
        }
    }
    Ok(cpuset)
}

/// `shape` holding `cpuset`: one vCPU for each CPU, no quota, the cores' `cpuset`, and the
/// partition the jailer makes before Firecracker runs unless `host` shares them.
pub fn hold(shape: &mut Shape, cpuset: &Cpuset, host: Host) {
    let cpus = cpus_of(cpuset);
    shape.vcpus = u32::try_from(cpus.len()).unwrap_or(1).max(1);
    shape.cgroup.retain(|s| !s.starts_with("cpu.max=") && !s.starts_with("pids.max="));
    shape.cgroup.push(format!("pids.max={}", 64 + shape.vcpus));
    shape.cgroup.push(format!("cpuset.cpus={}", cpuset.cpus));
    shape.cgroup.push(format!("cpuset.mems={}", cpuset.mems));
    if host == Host::Kept {
        shape.cgroup.push(format!("cpuset.cpus.exclusive={}", cpuset.cpus));
        shape.cgroup.push("cpuset.cpus.partition=root".to_owned());
    }
    shape.dedicated = true;
}

/// The shape `spec` asks for, holding its dedicated cores' `cpuset` where it holds any, checked
/// against the topology as `cores` reads it; and that `cpuset`.
///
/// # Errors
/// As [`given`].
pub fn shaped(
    cores: &Cores,
    spec: &korpis_proto::korpis::driver::v1::PreparedSpec,
) -> Result<(Shape, Option<Cpuset>), String> {
    let mut shape = crate::guest::shape(spec);
    let cpuset = given(cores, &spec.cores)?;
    if let Some(set) = &cpuset {
        hold(&mut shape, set, cores.host);
    }
    Ok((shape, cpuset))
}

/// The vCPU threads of Firecracker process `pid`, by index, as it names them (`fc_vcpu 0`).
fn vcpu_threads(pid: i32) -> Vec<(usize, i32)> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return Vec::new() };
    tasks
        .filter_map(Result::ok)
        .filter_map(|t| {
            let tid = t.file_name().to_str()?.parse::<i32>().ok()?;
            let comm = std::fs::read_to_string(t.path().join("comm")).ok()?;
            let index = comm.trim().strip_prefix("fc_vcpu ")?.parse::<usize>().ok()?;
            Some((index, tid))
        })
        .collect()
}

/// Settles the guest of Firecracker process `pid`, holding `cpuset` in the cgroup at `dir`: the
/// partition checked as the kernel made it, and each vCPU's thread pinned to its own CPU once
/// Firecracker has made them.
///
/// # Errors
/// The kernel made no partition, or a vCPU's thread could not be pinned or was not made in time.
pub async fn settle(cores: &Cores, dir: &Path, pid: i32, cpuset: &Cpuset) -> Result<(), String> {
    cores.settled(dir)?;
    let cpus = cpus_of(cpuset);
    let until = Instant::now() + PIN_BOUND;
    let threads = loop {
        let threads = vcpu_threads(pid);
        if threads.len() >= cpus.len() || Instant::now() >= until {
            break threads;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if threads.len() < cpus.len() {
        return Err(format!("{} of {} vCPU threads made in time", threads.len(), cpus.len()));
    }
    for (index, tid) in threads {
        let cpu = cpus.get(index).ok_or_else(|| format!("vCPU {index} has no CPU held"))?;
        let mut set = rustix::thread::CpuSet::new();
        set.set(usize::try_from(*cpu).map_err(|e| e.to_string())?);
        let thread = rustix::thread::Pid::from_raw(tid).ok_or("no thread")?;
        rustix::thread::sched_setaffinity(Some(thread), &set)
            .map_err(|e| format!("vCPU {index} pinned to CPU {cpu}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        let cgroup =
            vec!["memory.max=1".into(), "pids.max=66".into(), "cpu.max=200000 100000".into()];
        Shape { vcpus: 2, memory_mib: 256, cgroup, netns: None, dedicated: false }
    }

    #[test]
    fn a_guest_holding_cores_has_a_vcpu_for_each_cpu_its_cpuset_and_no_quota() {
        let set = Cpuset { cpus: "2-3,6-7".into(), mems: "0".into() };
        let mut held = shape();
        hold(&mut held, &set, Host::Kept);
        assert_eq!(held.vcpus, 4);
        assert!(held.dedicated);
        assert_eq!(
            held.cgroup,
            [
                "memory.max=1",
                "pids.max=68",
                "cpuset.cpus=2-3,6-7",
                "cpuset.mems=0",
                "cpuset.cpus.exclusive=2-3,6-7",
                "cpuset.cpus.partition=root",
            ],
            "the partition written last, after its CPUs, as the jailer writes them in order"
        );
        let mut shared = shape();
        hold(&mut shared, &set, Host::Shared);
        assert!(!shared.cgroup.iter().any(|s| s.starts_with("cpuset.cpus.partition")));
    }
}
