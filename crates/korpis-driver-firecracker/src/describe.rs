// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Describe` (§4.1 of docs/04-runtimes.md): what this driver does, declared from its code. Every
//! limit it holds names what holds it, the hypervisor or the kernel's cgroup around the guest's
//! Firecracker, and what it cannot hold is declared not enforced, which keeps it from being shown
//! (Rule K-3): a guest's processes are its own kernel's to count, and its volumes are block
//! devices whose size is their bound (§3.1 of docs/06-storage.md).

use crate::prepare::NAME;
use korpis_proto::korpis::v1 as pb;

/// The driver protocol it speaks.
pub const PROTOCOL: &str = "korpis.driver.v1";

fn declared(enforced: bool, mechanism: &str, granularity: &str) -> pb::EnforcementDecl {
    pb::EnforcementDecl {
        enforced,
        mechanism: mechanism.into(),
        granularity: granularity.into(),
        ..Default::default()
    }
}

fn enforcement() -> Vec<(String, pb::EnforcementDecl)> {
    let memory = declared(
        true,
        "the guest's memory size, and cgroup v2 memory.max on its Firecracker",
        "the guest sees exactly its limit, its page cache inside it; its Firecracker may use \
         64 MiB beyond it",
    );
    let cpu = declared(
        true,
        "the guest's vCPUs, and cgroup v2 cpu.max on its Firecracker",
        "whole vCPUs as the guest sees them, the limit rounded up; the limit is a quota of its \
         Firecracker's time per 100 ms, and the reservation no share",
    );
    let disk = declared(true, "a volume's block device size", "the guest's ENOSPC at its size");
    let pids = declared(false, "none", "a guest's processes are its own kernel's to count");
    let io = declared(false, "none", "no io.max is set");
    // The node's network shapes a guest's link as it does a container's, and the agent declares
    // so on the driver's behalf where the node does (§7 of docs/07-networking.md); the driver
    // itself shapes nothing.
    let egress = declared(false, "none", "no shaper is applied by the driver");
    let ingress = declared(false, "none", "no policer is applied by the driver");
    [("memory", memory), ("cpu", cpu), ("disk", disk), ("pids", pids), ("io", io)]
        .into_iter()
        .chain([("egress", egress), ("ingress", ingress)])
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

/// What the driver declares; `healthy` is whether this machine can run a guest now.
#[must_use]
pub fn info(healthy: bool) -> pb::DriverInfo {
    let capabilities = pb::Capabilities {
        tiers: vec![pb::IsolationTier::ISOLATION_TIER_MICROVM.into()],
        console: [
            pb::ConsoleMode::CONSOLE_MODE_NONE,
            pb::ConsoleMode::CONSOLE_MODE_LOGS,
            pb::ConsoleMode::CONSOLE_MODE_INTERACTIVE,
        ]
        .map(Into::into)
        .into(),
        gpu: vec![pb::GpuMode::GPU_MODE_NONE.into()],
        rootfs_sources: vec![pb::RootfsSource::ROOTFS_SOURCE_OCI_IMAGE.into()],
        network_modes: vec![
            pb::NetworkMode::NETWORK_MODE_TAP.into(),
            pb::NetworkMode::NETWORK_MODE_NONE.into(),
        ],
        enforcement: enforcement().into_iter().collect(),
        volume_semantics: pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE.into(),
        file_access: pb::FileAccess::FILE_ACCESS_NONE.into(),
        guest_agent: true,
        config_schema: r#"{"type":"object","additionalProperties":false}"#.into(),
        ..Default::default()
    };
    pb::DriverInfo {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL.into(),
        capabilities: capabilities.into(),
        healthy,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_declares_the_microvm_tier_and_no_limit_it_cannot_hold_as_held() {
        let i = info(true);
        let caps = i.capabilities.as_option().unwrap();
        let tiers: Vec<_> = caps.tiers.iter().map(buffa::EnumValue::as_known).collect();
        assert_eq!(tiers, [Some(pb::IsolationTier::ISOLATION_TIER_MICROVM)]);
        assert!(caps.signals.is_empty(), "a guest is sent no named signal (Finding 144)");
        assert!(!caps.enforcement["pids"].enforced && caps.enforcement["memory"].enforced);
        let modes: Vec<_> = caps.network_modes.iter().map(buffa::EnumValue::as_known).collect();
        assert_eq!(modes[0], Some(pb::NetworkMode::NETWORK_MODE_TAP), "a guest holds a tap");
        assert_eq!(
            caps.volume_semantics.as_known(),
            Some(pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE)
        );
    }
}
