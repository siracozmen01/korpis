// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Describe` (§4.1 of docs/04-runtimes.md): what this driver can do, declared from what its code
//! does rather than from what containers in general can do. Every entry here is a promise the
//! scheduler places against, so each one names the mechanism that keeps it, and what is not kept
//! is declared as not enforced, which is what keeps it from being displayed (Rule K-3).

use crate::prepare::NAME;
use korpis_proto::korpis::v1 as pb;

/// The driver protocol it speaks.
pub const PROTOCOL: &str = "korpis.driver.v1";

/// The signal names it accepts, as `signal_number` reads them.
pub const SIGNALS: [&str; 8] =
    ["SIGTERM", "SIGKILL", "SIGINT", "SIGHUP", "SIGQUIT", "SIGUSR1", "SIGUSR2", "SIGWINCH"];

fn enforced(mechanism: &str, granularity: &str) -> pb::EnforcementDecl {
    pb::EnforcementDecl {
        enforced: true,
        mechanism: mechanism.into(),
        granularity: granularity.into(),
        ..Default::default()
    }
}

fn not_enforced(granularity: &str) -> pb::EnforcementDecl {
    pb::EnforcementDecl {
        enforced: false,
        mechanism: "none".into(),
        granularity: granularity.into(),
        ..Default::default()
    }
}

fn enforcement() -> Vec<(String, pb::EnforcementDecl)> {
    let cpu = enforced(
        "cgroup v2 cpu.max",
        "the limit is a quota per 100 ms period; the reservation is a cpu.weight share, \
         which divides contended time and guarantees none",
    );
    let memory = enforced(
        "cgroup v2 memory.max",
        "page cache counts against the limit and swap is off; the reservation is memory.low",
    );
    let pids = enforced("cgroup v2 pids.max", "processes and threads together");
    let disk = not_enforced("a volume's size is the helper's to enforce, per volume");
    let io = not_enforced("no io.max is set");
    let egress = not_enforced("the container's network holds only loopback");
    [("cpu", cpu), ("memory", memory), ("pids", pids), ("disk", disk), ("io", io)]
        .into_iter()
        .chain([("egress", egress)])
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

/// What the driver declares; `healthy` is whether containerd answered.
#[must_use]
pub fn info(healthy: bool) -> pb::DriverInfo {
    let capabilities = pb::Capabilities {
        tiers: vec![pb::IsolationTier::ISOLATION_TIER_CONTAINER.into()],
        console: vec![
            pb::ConsoleMode::CONSOLE_MODE_NONE.into(),
            pb::ConsoleMode::CONSOLE_MODE_LOGS.into(),
            pb::ConsoleMode::CONSOLE_MODE_INTERACTIVE.into(),
        ],
        signals: SIGNALS.iter().map(|s| (*s).to_owned()).collect(),
        gpu: vec![pb::GpuMode::GPU_MODE_NONE.into()],
        rootfs_sources: vec![pb::RootfsSource::ROOTFS_SOURCE_OCI_IMAGE.into()],
        network_modes: vec![
            pb::NetworkMode::NETWORK_MODE_VETH.into(),
            pb::NetworkMode::NETWORK_MODE_NONE.into(),
        ],
        enforcement: enforcement().into_iter().collect(),
        volume_semantics: pb::VolumeSemantics::VOLUME_SEMANTICS_FILESYSTEM.into(),
        file_access: pb::FileAccess::FILE_ACCESS_HOST_WORKER.into(),
        id_mapping: pb::IdMapCaps {
            idmapped_mounts: true,
            user_namespaces: true,
            ..Default::default()
        }
        .into(),
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
