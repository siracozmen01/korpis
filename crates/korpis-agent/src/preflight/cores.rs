// SPDX-License-Identifier: AGPL-3.0-or-later

//! What dedicated cores need of the machine (§2 of docs/18-operations.md, §5.2 of
//! docs/05-scheduling.md): a physical core to offer besides the one holding CPU 0, read from the
//! kernel's topology as the agent reads it to offer them; the cgroup v2 cpuset partitions a
//! holder is made with, read as the drivers read them to declare cores held; and interrupts
//! steered off the offered cores, read from the affinity the kernel gives every new interrupt.
//! None is essential: a node without them offers no dedicated cores, or holds them with less.

use super::{Check, Machine, Offer, Outcome};
use std::collections::BTreeSet;

/// The most hexadecimal digits of an affinity mask read: one for every four CPUs the topology
/// reader believes.
const MOST_MASK_DIGITS: usize = 2048;

/// The checks of the node's dedicated cores.
#[must_use]
pub fn checks(m: &Machine, offer: &Offer) -> Vec<Check> {
    let system = m.sys.join("devices/system");
    let offered = korpis_topology::cores(&system, offer.dedicated_cores.as_ref());
    let held: BTreeSet<u32> = offered.iter().flat_map(|c| c.cpus.iter().copied()).collect();
    vec![cores(m, offer, &offered), partitions(m, &held), interrupts(m, &held)]
}

fn cores(m: &Machine, offer: &Offer, offered: &[korpis_core::node::cores::Core]) -> Check {
    let (name, needed_for) = ("physical cores", "dedicated cores");
    let ids: BTreeSet<u32> = offered.iter().map(|c| c.id).collect();
    let found = if offered.is_empty() {
        "no core offered".to_owned()
    } else {
        format!("{} offered, by id {}", offered.len(), korpis_topology::listed(&ids))
    };
    let missing: Vec<u32> =
        offer.dedicated_cores.iter().flatten().filter(|id| !ids.contains(id)).copied().collect();
    let outcome = if !missing.is_empty() {
        Outcome::Lacking {
            costs: format!("cores {} are not offered", korpis_topology::listed(&missing)),
            fix: "name in --dedicated-cores only cores this machine has, each by its lowest \
                  logical CPU, every one of its CPUs online and none of them CPU 0"
                .into(),
        }
    } else if offered.is_empty() {
        let topology = m.sys("devices/system/cpu/online").is_some();
        Outcome::Lacking {
            costs: "dedicated cores: the node offers none".into(),
            fix: if topology {
                "a machine of one physical core has none to offer besides CPU 0's; bring the \
                 others' CPUs online (/sys/devices/system/cpu/cpuN/online)"
            } else {
                "mount sysfs at /sys, where the kernel's CPU topology is read"
            }
            .into(),
        }
    } else {
        Outcome::Met
    };
    Check { name, needed_for, found, outcome, essential: false }
}

fn partitions(m: &Machine, held: &BTreeSet<u32>) -> Check {
    let (name, needed_for) = ("cpuset partitions", "dedicated cores kept from the host");
    let control = m.sys("fs/cgroup/cgroup.subtree_control").unwrap_or_default();
    let given = control.split_whitespace().any(|c| c == "cpuset");
    let made = std::fs::read_dir(m.sys.join("fs/cgroup")).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let p = e.path();
            p.join("cpuset.cpus.exclusive").is_file() && p.join("cpuset.cpus.partition").is_file()
        })
    });
    let found = match (given, made) {
        (false, _) => "the cpuset controller is not given to the root's children",
        (true, false) => "no cgroup below the root has cpuset.cpus.exclusive",
        (true, true) => "remote partitions made below the root",
    }
    .to_owned();
    let shared = "or give the drivers --dedicated-cores-host shared, which keeps other tenants \
                  off a held core and not the host";
    let outcome = if held.is_empty() {
        Outcome::NotNeeded("no core is offered for dedication".into())
    } else if given && made {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "dedicated cores: the drivers declare them not held, and none is placed".into(),
            fix: if given {
                format!("Linux 6.7 or later, whose cpuset makes remote partitions; {shared}")
            } else {
                format!(
                    "echo +cpuset > /sys/fs/cgroup/cgroup.subtree_control, at every boot; \
                         {shared}"
                )
            },
        }
    };
    Check { name, needed_for, found, outcome, essential: false }
}

fn interrupts(m: &Machine, held: &BTreeSet<u32>) -> Check {
    let (name, needed_for) = ("interrupt affinity", "dedicated cores undisturbed");
    let Some(affinity) = m.proc("irq/default_smp_affinity").and_then(|a| mask(&a)) else {
        let outcome = if held.is_empty() {
            Outcome::NotNeeded("no core is offered for dedication".into())
        } else {
            Outcome::Unchecked("/proc/irq/default_smp_affinity could not be read".into())
        };
        return Check { name, needed_for, found: "unknown".into(), outcome, essential: false };
    };
    let on_held: BTreeSet<u32> = affinity.intersection(held).copied().collect();
    let found = format!("new interrupts go to CPUs {}", korpis_topology::listed(&affinity));
    let outcome = if held.is_empty() {
        Outcome::NotNeeded("no core is offered for dedication".into())
    } else if on_held.is_empty() {
        Outcome::Met
    } else {
        let online = m.sys("devices/system/cpu/online");
        let online = online.and_then(|o| korpis_topology::cpu_list(&o).ok()).unwrap_or_default();
        let rest: BTreeSet<u32> = online.difference(held).copied().collect();
        Outcome::Lacking {
            costs: format!(
                "a holder is interrupted on CPUs {} it holds, by interrupts the kernel sends there",
                korpis_topology::listed(&on_held)
            ),
            fix: format!(
                "boot with irqaffinity={} on the kernel command line, the CPUs no offered core has",
                korpis_topology::listed(&rest)
            ),
        }
    };
    Check { name, needed_for, found, outcome, essential: false }
}

/// The CPUs a kernel affinity mask (`ff`, `00000000,00000f0f`) names; `None` where it is not one
/// or is longer than the topology reader believes.
fn mask(s: &str) -> Option<BTreeSet<u32>> {
    let digits: Vec<char> = s.trim().chars().filter(|c| *c != ',').collect();
    if digits.is_empty() || digits.len() > MOST_MASK_DIGITS {
        return None;
    }
    let mut cpus = BTreeSet::new();
    for (i, d) in digits.iter().rev().enumerate() {
        let value = d.to_digit(16)?;
        let base = u32::try_from(i).ok()?.checked_mul(4)?;
        cpus.extend((0..4).filter(|b| value & (1 << b) != 0).map(|b| base + b));
    }
    Some(cpus)
}

#[cfg(test)]
mod tests {
    use super::mask;

    #[test]
    fn an_affinity_mask_names_its_cpus_across_its_comma_separated_words() {
        assert_eq!(mask("ffffff\n").unwrap(), (0..24).collect());
        assert_eq!(mask("00000001,00000003").unwrap(), [0, 1, 32].into_iter().collect());
        assert!(mask("0").unwrap().is_empty());
        assert!(mask("").is_none());
        assert!(mask("fg").is_none());
        assert!(mask(&"f".repeat(2049)).is_none());
    }
}
