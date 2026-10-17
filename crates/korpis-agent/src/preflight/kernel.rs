// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every tier needs of the kernel (§2 of docs/18-operations.md): a 6.1 floor, cgroup v2
//! with the controllers limits are enforced through, user namespaces, seccomp, and Landlock for
//! the tenant file API.

use super::{Check, Machine, Outcome, Probes};

/// The kernel floor: 6.1, the LTS with every interface the node uses.
const FLOOR: (u32, u32) = (6, 1);

/// The cgroup controllers the limits of §10 of docs/17-security.md are enforced through.
const CONTROLLERS: [&str; 4] = ["cpu", "memory", "pids", "io"];

/// The checks of the kernel.
#[must_use]
pub fn checks(m: &Machine, p: &Probes) -> Vec<Check> {
    vec![release(m), cgroups(m), user_namespaces(m), seccomp(m), landlock(m, p)]
}

/// `major.minor` of a release string such as `6.8.0-142-generic`.
fn version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(['.', '-']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn release(m: &Machine) -> Check {
    let found = m.proc("sys/kernel/osrelease");
    let outcome = match found.as_deref().and_then(version) {
        Some(v) if v >= FLOOR => Outcome::Met,
        Some(_) => Outcome::Lacking {
            costs: "everything: the interfaces the node confines workloads with".into(),
            fix: "install the distribution's 6.1-or-later LTS kernel and reboot into it".into(),
        },
        None => Outcome::Unchecked("/proc/sys/kernel/osrelease could not be read".into()),
    };
    let found = found.unwrap_or_else(|| "unknown".into());
    Check { name: "Linux 6.1+", needed_for: "everything", found, outcome, essential: true }
}

fn cgroups(m: &Machine) -> Check {
    let Some(available) = m.sys("fs/cgroup/cgroup.controllers") else {
        return Check {
            name: "cgroup v2",
            needed_for: "every tier",
            found: "no unified hierarchy at /sys/fs/cgroup".into(),
            outcome: Outcome::Lacking {
                costs: "everything: CPU, memory, PID, and I/O limits are enforced through it"
                    .into(),
                fix: "add systemd.unified_cgroup_hierarchy=1 to the kernel command line and \
                      reboot"
                    .into(),
            },
            essential: true,
        };
    };
    let have: Vec<&str> = available.split_whitespace().collect();
    let missing: Vec<&str> = CONTROLLERS.into_iter().filter(|c| !have.contains(c)).collect();
    let outcome = if missing.is_empty() {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: format!("the {} limits cannot be enforced", missing.join(", ")),
            fix: format!(
                "a kernel built with the {} controllers (CONFIG_CGROUP_*), and nothing on the \
                 kernel command line disabling them (cgroup_disable=)",
                missing.join(", ")
            ),
        }
    };
    let found = format!("unified, controllers: {}", have.join(" "));
    Check { name: "cgroup v2", needed_for: "every tier", found, outcome, essential: true }
}

fn user_namespaces(m: &Machine) -> Check {
    let most = m.proc("sys/user/max_user_namespaces").and_then(|v| v.parse::<u64>().ok());
    let outcome = match most {
        Some(0) => Outcome::Lacking {
            costs: "everything: root inside a workload would be root on the host".into(),
            fix: "sysctl -w user.max_user_namespaces=63000, and the same line in \
                  /etc/sysctl.d/90-korpis.conf"
                .into(),
        },
        Some(_) => Outcome::Met,
        None => Outcome::Lacking {
            costs: "everything: root inside a workload would be root on the host".into(),
            fix: "a kernel built with user namespaces (CONFIG_USER_NS)".into(),
        },
    };
    let found = most.map_or_else(|| "absent".into(), |n| format!("at most {n}"));
    Check { name: "user namespaces", needed_for: "every tier", found, outcome, essential: true }
}

fn seccomp(m: &Machine) -> Check {
    let actions = m.proc("sys/kernel/seccomp/actions_avail");
    let outcome = if actions.is_some() {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "everything: workloads' system calls are filtered by it".into(),
            fix: "a kernel built with seccomp filters (CONFIG_SECCOMP_FILTER)".into(),
        }
    };
    let found = actions.map_or_else(|| "absent".into(), |a| format!("actions: {a}"));
    Check { name: "seccomp", needed_for: "every tier", found, outcome, essential: true }
}

fn landlock(m: &Machine, p: &Probes) -> Check {
    let (name, needed_for) = ("Landlock", "tenant file access");
    if let Some(abi) = p.landlock_abi {
        let found = format!("the file access rights of ABI {abi}");
        return Check { name, needed_for, found, outcome: Outcome::Met, essential: false };
    }
    let lsm = m.sys("kernel/security/lsm").unwrap_or_default();
    let fix = if lsm.split(',').any(|l| l == "landlock") || lsm.is_empty() {
        "a kernel built with Landlock (CONFIG_SECURITY_LANDLOCK)".to_owned()
    } else {
        format!("add landlock to the lsm= kernel parameter, e.g. lsm=landlock,{lsm}, and reboot")
    };
    let costs = "no tenant file API: the file worker is confined by it".to_owned();
    let outcome = Outcome::Lacking { costs, fix };
    let found = "absent".to_owned();
    Check { name, needed_for, found, outcome, essential: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_is_read_by_its_major_and_minor() {
        assert_eq!(version("6.8.0-142-generic"), Some((6, 8)));
        assert_eq!(version("5.15.0"), Some((5, 15)));
        assert_eq!(version("6.1-rc2"), Some((6, 1)));
        assert_eq!(version("garbage"), None);
    }
}
