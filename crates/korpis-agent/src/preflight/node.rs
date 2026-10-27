// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node's configured offer needs of the machine (§2 of docs/18-operations.md): its
//! driver's runtime, the storage the helper can enforce, I/O shares, and KVM and the tun driver,
//! which only the virtual-machine tiers need, as its drivers declare them, and nested
//! virtualization only when the node is itself a virtual machine (finding S67).

use super::{Check, Machine, Offer, Outcome, Probes};
use korpis_proto::korpis::v1 as pb;

/// The checks of the node's own offer.
#[must_use]
pub fn checks(m: &Machine, p: &Probes, offer: &Offer) -> Vec<Check> {
    vec![kvm(m, p), tun(m, p), io_cost(m, p), runtime(p, offer), storage(p)]
}

/// Whether a driver of the node offers the microvm tier, or, where the drivers could not be
/// asked, why not.
fn microvms(p: &Probes) -> Result<bool, String> {
    match &p.tiers {
        None => Ok(false),
        Some(Ok(tiers)) => Ok(tiers.contains(&pb::IsolationTier::ISOLATION_TIER_MICROVM)),
        Some(Err(e)) => Err(e.clone()),
    }
}

/// Whether this machine is itself a virtual machine: its processors say a hypervisor runs them.
fn virtual_machine(m: &Machine) -> bool {
    m.proc("cpuinfo").is_some_and(|c| {
        c.lines().any(|l| l.starts_with("flags") && l.split_whitespace().any(|f| f == "hypervisor"))
    })
}

/// Whether the KVM module lets guests run guests, where this machine says.
fn nested(m: &Machine) -> Option<bool> {
    ["module/kvm_intel/parameters/nested", "module/kvm_amd/parameters/nested"]
        .into_iter()
        .find_map(|p| m.sys(p))
        .map(|v| v == "Y" || v == "1")
}

fn kvm(m: &Machine, p: &Probes) -> Check {
    let present = m.dev.join("kvm").exists();
    let in_vm = virtual_machine(m);
    let mut found = if present { "/dev/kvm present".to_owned() } else { "no /dev/kvm".to_owned() };
    if in_vm {
        let nesting = match nested(m) {
            Some(true) => "nested virtualization on",
            Some(false) => "nested virtualization off",
            None => "nested virtualization unknown",
        };
        found = format!("{found}; this machine is a virtual machine, {nesting}");
    }
    let fix = if in_vm {
        "load kvm_intel or kvm_amd, and turn on nested virtualization in the hypervisor that runs \
         this machine"
    } else {
        "turn on virtualization in the firmware, and load kvm_intel or kvm_amd"
    };
    let outcome = match microvms(p) {
        Ok(true) if present => Outcome::Met,
        Ok(true) => Outcome::Lacking {
            costs: "the microvm tier: its driver runs no guest".into(),
            fix: fix.into(),
        },
        Ok(false) => Outcome::NotNeeded(
            "no driver this node runs offers the microvm or vm tiers; without KVM it offers \
             process and container only (§4.1 of docs/04-runtimes.md)"
                .into(),
        ),
        Err(e) => Outcome::Unchecked(e),
    };
    Check { name: "KVM", needed_for: "microvm, vm", found, outcome, essential: false }
}

/// The tun driver, through which the helper makes a guest's tap (§4.1 of
/// docs/07-networking.md).
fn tun(m: &Machine, p: &Probes) -> Check {
    let present = m.dev.join("net/tun").exists();
    let found = if present { "/dev/net/tun present" } else { "no /dev/net/tun" }.to_owned();
    let outcome = match microvms(p) {
        Ok(true) if present => Outcome::Met,
        Ok(true) => Outcome::Lacking {
            costs: "a microVM's network: its workload does not start where networks are given"
                .into(),
            fix: "modprobe tun, and name it in /etc/modules-load.d/ so every boot loads it".into(),
        },
        Ok(false) => Outcome::NotNeeded("no driver this node runs offers the microvm tier".into()),
        Err(e) => Outcome::Unchecked(e),
    };
    Check { name: "tun", needed_for: "microvm networks", found, outcome, essential: false }
}

fn io_cost(m: &Machine, p: &Probes) -> Check {
    let name = "io.cost";
    let needed_for = "I/O shares";
    let Some((volumes, disk)) = &p.volume_disk else {
        let outcome = Outcome::Unchecked(
            "the volume root's device could not be found: create it, or name it with \
             --volume-root"
                .into(),
        );
        return Check { name, needed_for, found: "unknown".into(), outcome, essential: false };
    };
    let qos = m.sys("fs/cgroup/io.cost.qos").unwrap_or_default();
    let enabled =
        qos.lines().any(|l| l.split_whitespace().next() == Some(disk) && l.contains("enable=1"));
    let outcome = if enabled {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "I/O ceilings only, no proportional shares (§3 of docs/06-storage.md)".into(),
            fix: format!("echo \"{disk} enable=1\" > /sys/fs/cgroup/io.cost.qos, at every boot"),
        }
    };
    let found = format!("volumes on {volumes}, disk {disk}, io.cost {}", on(enabled));
    Check { name, needed_for, found, outcome, essential: false }
}

fn on(yes: bool) -> &'static str {
    if yes { "enabled" } else { "not enabled" }
}

fn runtime(p: &Probes, offer: &Offer) -> Check {
    let name = "containerd";
    let needed_for = "the container tier (the oci driver)";
    let Some(answered) = &p.containerd else {
        let outcome = Outcome::NotNeeded(format!("the {} driver is configured", offer.driver));
        return Check { name, needed_for, found: "not asked".into(), outcome, essential: false };
    };
    let (found, outcome) = match answered {
        Ok(()) => ("answers".to_owned(), Outcome::Met),
        Err(e) => (
            format!("does not answer: {e}"),
            Outcome::Lacking {
                costs: "every workload: the oci driver runs them through it".into(),
                fix: "install containerd 1.7 or later from the distribution and \
                      `systemctl enable --now containerd`"
                    .into(),
            },
        ),
    };
    Check { name, needed_for, found, outcome, essential: true }
}

fn storage(p: &Probes) -> Check {
    let name = "enforced storage";
    let needed_for = "volume sizes, snapshots";
    let classes = match &p.storage {
        None | Some(Err(_)) => {
            let why = match &p.storage {
                Some(Err(e)) => e.clone(),
                _ => "not asked".to_owned(),
            };
            let outcome = Outcome::Unchecked(format!(
                "the helper could not be asked ({why}): `systemctl start korpis-helper.socket`, \
                 or name its socket with --helper-socket"
            ));
            return Check { name, needed_for, found: "unknown".into(), outcome, essential: false };
        }
        Some(Ok(classes)) => classes,
    };
    let unenforced: Vec<&str> = classes
        .iter()
        .filter(|c| c.backend.as_known() == Some(pb::StorageBackend::STORAGE_BACKEND_DIRECTORY))
        .map(|c| c.name.as_str())
        .collect();
    let found = classes
        .iter()
        .map(|c| format!("{} on {}", c.name, backend(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let outcome = if classes.is_empty() {
        Outcome::Lacking {
            costs: "no volumes".into(),
            fix: "create the helper's volume root, /var/lib/korpis/volumes by default".into(),
        }
    } else if unenforced.is_empty() {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: format!(
                "{} enforce no size and take no snapshots, and say so (Rule K-3)",
                unenforced.join(", ")
            ),
            fix: "put the volume root on ext4 or XFS mounted with prjquota (ext4: `tune2fs -O \
                  project,quota` while unmounted), or on btrfs after `btrfs quota enable`, and \
                  restart the helper"
                .into(),
        }
    };
    Check { name, needed_for, found, outcome, essential: false }
}

fn backend(c: &pb::StorageClassInfo) -> &'static str {
    match c.backend.as_known() {
        Some(pb::StorageBackend::STORAGE_BACKEND_BTRFS) => "btrfs, quota and snapshots",
        Some(pb::StorageBackend::STORAGE_BACKEND_XFS_PROJECT) => "XFS, project quota",
        Some(pb::StorageBackend::STORAGE_BACKEND_EXT4_PROJECT) => "ext4, project quota",
        Some(pb::StorageBackend::STORAGE_BACKEND_ZFS) => "ZFS",
        Some(pb::StorageBackend::STORAGE_BACKEND_DIRECTORY) => "a plain directory, no quota",
        Some(pb::StorageBackend::STORAGE_BACKEND_NETWORK)
            if c.block_protocol.as_known()
                != Some(pb::BlockProtocol::BLOCK_PROTOCOL_UNSPECIFIED) =>
        {
            "a block storage's devices, one per volume"
        }
        Some(pb::StorageBackend::STORAGE_BACKEND_NETWORK) => "a shared network filesystem",
        _ => "an unknown backend",
    }
}
