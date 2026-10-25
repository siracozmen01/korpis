// SPDX-License-Identifier: AGPL-3.0-or-later

//! A driver as the scheduler's tests declare it.

use crate::node::driver::{FileAccess, IdMapCaps, VolumeSemantics};
use crate::node::{Capabilities, DriverInfo, Enforcement};
use crate::workload::spec::{ConsoleMode, IsolationTier};

/// An `oci` driver of the container tier, enforcing `enforced`.
pub(super) fn driver(enforced: &[&str]) -> DriverInfo {
    let enforcement = enforced
        .iter()
        .map(|k| {
            let e = Enforcement {
                enforced: true,
                mechanism: "cgroup v2".into(),
                granularity: String::new(),
            };
            ((*k).to_owned(), e)
        })
        .collect();
    DriverInfo {
        name: "oci".into(),
        version: "1".into(),
        protocol: "1".into(),
        healthy: true,
        capabilities: Capabilities {
            tiers: vec![IsolationTier::Container],
            console: vec![ConsoleMode::Logs, ConsoleMode::Interactive],
            exec: true,
            signals: Vec::new(),
            hot_resize: Vec::new(),
            hot_volume_attach: false,
            runtime_snapshot: None,
            live_migrate: false,
            device_modes: Vec::new(),
            devices: Vec::new(),
            rootfs_sources: Vec::new(),
            network_modes: Vec::new(),
            enforcement,
            volume_semantics: VolumeSemantics::Filesystem,
            file_access: FileAccess::HostWorker,
            id_mapping: IdMapCaps::default(),
            config_schema: String::new(),
            disruptive: vec![],
            identity_dir: true,
            guest_agent: false,
        },
    }
}
