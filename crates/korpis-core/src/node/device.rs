// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's devices (§6 of docs/05-scheduling.md): those the Container Device Interface's files on
//! it describe, as its agent reports them, and the holds a placement keeps on them. A device
//! carries no record of who holds it; placements do.

use super::driver::DeviceClass;
use std::collections::BTreeMap;

/// How a workload holds a device (§6.2 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceMode {
    /// The device, alone, while the workload is placed.
    Passthrough,
    /// A virtual device a hypervisor partitions it into.
    Mediated,
    /// The same device as up to its shares of other holders.
    Timeslice,
}

/// A device of a node, as its agent found it described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Its CDI name, `vendor.com/kind=name`; unique on its node.
    pub id: String,
    /// Its class: the kind's name where it names one, or the operator's mapping.
    pub class: DeviceClass,
    /// Its CDI annotations, which a request's selector matches.
    pub attributes: BTreeMap<String, String>,
    /// The host device nodes its own edits give.
    pub host_nodes: Vec<String>,
    /// How many workloads may hold it at once in timeslice; 0 offers passthrough alone.
    pub shares: u32,
}

/// A device a node's files describe and the node does not offer, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnofferedDevice {
    /// Its CDI name.
    pub id: String,
    /// Why it is not offered, for its operator.
    pub reason: String,
}

/// A device a placement holds, and how.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceHold {
    /// The device's id on the placement's node.
    pub device: String,
    /// How it is held.
    pub mode: DeviceMode,
}

/// The longest device name kept: a kind's prefix of 253, its name of 63, and a device's name.
const LONGEST: usize = 512;

/// Whether `id` is a CDI device name, `vendor.com/kind=name` (§5 of docs/research/evidence.md):
/// a DNS subdomain, a slash, the kind's name of at most 63 characters, `=`, and the device's name,
/// each name of alphanumerics with dashes, underscores, and dots between. A device's name may also
/// hold a colon between, as NVIDIA's MIG devices' (`nvidia.com/gpu=1:0`) do, which the
/// specification's grammar does not list and its files rely on.
#[must_use]
pub fn is_cdi_name(id: &str) -> bool {
    let Some((kind, name)) = id.split_once('=') else { return false };
    let Some((vendor, class)) = kind.split_once('/') else { return false };
    id.len() <= LONGEST
        && vendor.len() <= 253
        && !vendor.is_empty()
        && vendor.split('.').all(|l| !l.is_empty() && l.len() <= 63 && label(l))
        && class.len() <= 63
        && segment(class, b"")
        && segment(name, b":")
}

/// A DNS label: alphanumerics and dashes, neither first nor last a dash.
fn label(l: &str) -> bool {
    l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !l.starts_with('-')
        && !l.ends_with('-')
}

/// A CDI name segment: alphanumerics first and last; dashes, underscores, dots, and `more`
/// between.
fn segment(s: &str, more: &[u8]) -> bool {
    let edge = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_alphanumeric());
    edge(s.bytes().next())
        && edge(s.bytes().last())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b) || more.contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cdi_name_is_a_vendor_a_kind_and_a_device() {
        for good in ["nvidia.com/gpu=0", "nvidia.com/gpu=1:0", "korpis.test/standin=a.b_c-1"] {
            assert!(is_cdi_name(good), "{good}");
        }
        for bad in ["gpu=0", "nvidia.com/gpu", "nvidia.com/g:pu=0", "a.com/gpu=0:", "a.com/=0"] {
            assert!(!is_cdi_name(bad), "{bad}");
        }
        assert!(!is_cdi_name(&format!("a.com/{}=0", "k".repeat(64))));
    }
}
