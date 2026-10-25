// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container's dedicated cores in its runtime specification (§4.2 of docs/04-runtimes.md):
//! `linux.resources.cpu.cpus` and `mems`, which runc writes as its cgroup's `cpuset`. The cores
//! are its CPU's bound, so it is given no quota and no weight: a quota of the millicores declared
//! would throttle it below the threads it holds, and a weight divides only CPUs it shares.

use super::RuntimeSpec;

impl RuntimeSpec {
    /// The same specification, its container held to `cpuset`, its dedicated cores' CPUs and
    /// memory, with no CPU quota or weight; unchanged for none.
    #[must_use]
    pub fn with_cpuset(mut self, cpuset: Option<&korpis_driver_kit::cores::Cpuset>) -> Self {
        if let Some(set) = cpuset {
            let cpu = &mut self.linux.resources.cpu;
            cpu.cpus = Some(set.cpus.clone());
            cpu.mems = Some(set.mems.clone());
            (cpu.quota, cpu.period, cpu.shares) = (None, None, None);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::super::runtime_spec;
    use super::super::tests::{image, spec};
    use crate::devices::Given;
    use korpis_driver_kit::cores::Cpuset;

    #[test]
    fn a_container_holding_cores_is_bound_by_its_cpuset_and_no_quota() {
        let built = runtime_spec(&spec(&[]), &image(), "/k", &Given::default()).unwrap();
        let set = Cpuset { cpus: "2,6".into(), mems: "0".into() };
        let held = serde_json::to_value(built.clone().with_cpuset(Some(&set))).unwrap();
        let cpu = &held["linux"]["resources"]["cpu"];
        assert_eq!((cpu["cpus"].as_str(), cpu["mems"].as_str()), (Some("2,6"), Some("0")));
        assert!(cpu.get("quota").is_none() && cpu.get("shares").is_none(), "{cpu}");
        assert_eq!(built.clone().with_cpuset(None), built, "none, unchanged");
    }
}
