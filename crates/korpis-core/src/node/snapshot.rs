// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a driver's execution-state snapshot restores, and whether another node's driver is such a
//! place: the same CPU template and hypervisor version, and a host kernel within the range the
//! taking driver declares. The control plane reads both nodes' declarations as they last said
//! `Hello`; the driver that resumes a state checks again what the state records of where it was
//! taken, since a node's declaration may have changed since.

use super::driver::{DriverInfo, SnapshotCaps};
use super::Node;

/// A kernel version, `major.minor[.patch...]`, as numbers; none for anything else.
fn version(s: &str) -> Option<Vec<u64>> {
    s.split('.').map(|p| p.parse().ok()).collect::<Option<Vec<u64>>>().filter(|v| !v.is_empty())
}

impl SnapshotCaps {
    /// Whether a snapshot taken where `self` is declared restores where `there` is: the same CPU
    /// template and hypervisor version, and both ends of `there`'s kernel range within `self`'s.
    /// A version that is not one restores nowhere.
    #[must_use]
    pub fn restores_on(&self, there: &Self) -> bool {
        let (Some(low), Some(high)) = (version(&self.kernel_min), version(&self.kernel_max)) else {
            return false;
        };
        let within = |k: &str| version(k).is_some_and(|k| low <= k && k <= high);
        !self.hypervisor_version.is_empty()
            && self.cpu_template == there.cpu_template
            && self.hypervisor_version == there.hypervisor_version
            && within(&there.kernel_min)
            && within(&there.kernel_max)
    }
}

/// Where `node`'s healthy driver named `driver` declares its snapshots restore, if it takes any.
fn snapshots<'n>(node: &'n Node, driver: &str) -> Option<&'n SnapshotCaps> {
    let d: &DriverInfo = node.drivers.iter().find(|d| d.name == driver && d.healthy)?;
    d.capabilities.runtime_snapshot.as_ref()
}

/// Whether a workload run by `driver` moves from `source` to `destination` with its execution
/// state: the source's driver takes snapshots, and the destination's restores them.
#[must_use]
pub fn restores(source: &Node, destination: &Node, driver: &str) -> bool {
    let (Some(from), Some(to)) = (snapshots(source, driver), snapshots(destination, driver)) else {
        return false;
    };
    from.restores_on(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(hypervisor: &str, kernels: (&str, &str)) -> SnapshotCaps {
        SnapshotCaps {
            cpu_template: String::new(),
            hypervisor_version: hypervisor.into(),
            kernel_min: kernels.0.into(),
            kernel_max: kernels.1.into(),
        }
    }

    #[test]
    fn a_snapshot_restores_only_by_the_same_hypervisor_on_a_kernel_in_its_range() {
        let taken = caps("1.17.0", ("6.8", "6.8"));
        assert!(taken.restores_on(&caps("1.17.0", ("6.8", "6.8"))));
        assert!(!taken.restores_on(&caps("1.16.0", ("6.8", "6.8"))), "another hypervisor");
        assert!(!taken.restores_on(&caps("1.17.0", ("6.10", "6.10"))), "a newer kernel");
        assert!(!taken.restores_on(&caps("1.17.0", ("6.1", "6.1"))), "an older one");
        let wide = caps("1.17.0", ("5.10", "6.10"));
        assert!(wide.restores_on(&caps("1.17.0", ("6.8", "6.8"))), "6.8 is within, not below 6.10");
        let mut template = caps("1.17.0", ("6.8", "6.8"));
        template.cpu_template = "T2".into();
        assert!(!taken.restores_on(&template), "another CPU template");
        assert!(!caps("", ("6.8", "6.8")).restores_on(&caps("", ("6.8", "6.8"))), "no version");
        assert!(!caps("1.17.0", ("six", "6.8")).restores_on(&taken), "no kernel version");
    }
}
