// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes for tests. Phase 0 enrolls nodes through the agent protocol, which arrives after the
//! scheduler; until then tests put them where enrollment will.

use crate::PgStore;
use korpis_app::ports::{NodeWrite, Store, StoreResult, Tx};
use korpis_core::labels::Labels;
use korpis_core::node::driver::{
    FileAccess, IdMapCaps, NetworkMode, RootfsSource, VolumeSemantics,
};
use korpis_core::node::{
    Capabilities, Connectivity, DriverInfo, Enforcement, Node, NodeAdmin, Reachability,
};
use korpis_core::tenancy::Name;
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_core::workload::spec::{ConsoleMode, IsolationTier};
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

const GIB: u64 = 1 << 30;

/// The `oci` driver at the container tier, enforcing every kind of limit.
#[must_use]
pub fn oci_driver() -> DriverInfo {
    let enforced = |m: &str| Enforcement {
        enforced: true,
        mechanism: m.to_owned(),
        granularity: String::new(),
    };
    let enforcement = ["memory", "cpu", "disk", "io", "pids", "egress"]
        .into_iter()
        .map(|k| (k.to_owned(), enforced("cgroup v2")))
        .collect();
    DriverInfo {
        name: "oci".to_owned(),
        version: "0.0.0".to_owned(),
        protocol: "1".to_owned(),
        healthy: true,
        capabilities: Capabilities {
            tiers: vec![IsolationTier::Container],
            console: vec![ConsoleMode::None, ConsoleMode::Logs, ConsoleMode::Interactive],
            exec: true,
            signals: vec!["SIGTERM".to_owned(), "SIGKILL".to_owned()],
            hot_resize: vec!["memory".to_owned(), "cpu".to_owned()],
            hot_volume_attach: false,
            runtime_snapshot: None,
            live_migrate: false,
            gpu: Vec::new(),
            devices: Vec::new(),
            rootfs_sources: vec![RootfsSource::OciImage],
            network_modes: vec![NetworkMode::Veth],
            enforcement,
            volume_semantics: VolumeSemantics::Filesystem,
            file_access: FileAccess::HostWorker,
            id_mapping: IdMapCaps { idmapped_mounts: true, user_namespaces: true },
            config_schema: "{}".to_owned(),
            disruptive: vec![],
        },
    }
}

/// Eight cores, 16 GiB of memory, 200 GiB of disk, and a gigabit, all allocatable.
#[must_use]
pub fn capacity() -> Bounds {
    BTreeMap::from([
        (Dimension::Cpu, 8_000),
        (Dimension::Memory, 16 * GIB),
        (Dimension::Disk, 200 * GIB),
        (Dimension::Bandwidth, 1_000_000_000),
    ])
}

/// A node fixture's name or labels did not parse.
#[derive(Debug, thiserror::Error)]
#[error("node fixture: {0}")]
pub struct FixtureError(String);

/// A connected, active node owned by `organization`, offering [`oci_driver`], with a public
/// address and `labels`.
pub fn ready_node(
    organization: Uuid,
    name: &str,
    labels: &[(&str, &str)],
) -> Result<Node, FixtureError> {
    let labels = labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned()));
    Ok(Node {
        id: Uuid::now_v7(),
        name: Name::parse(name).map_err(|e| FixtureError(e.to_string()))?,
        organization,
        labels: Labels::new(labels).map_err(|e| FixtureError(e.to_string()))?,
        taints: Vec::new(),
        capacity: capacity(),
        allocatable: capacity(),
        drivers: vec![oci_driver()],
        storage_classes: Vec::new(),
        reachability: Reachability::Public,
        advertised: None,
        probed_at: None,
        admin: NodeAdmin::Active,
        connectivity: Connectivity::Connected,
        agent_version: Some("0.0.0".to_owned()),
        last_seen: Some(SystemTime::now()),
        enrolled_at: SystemTime::now(),
        version: 1,
    })
}

/// Records `node` as enrolled, with an identity key unique to it. Tests never authenticate a
/// node, so the key only has to be distinct.
pub async fn enroll(store: &PgStore, node: &Node) -> StoreResult<()> {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(node.id.as_bytes());
    key[16..].copy_from_slice(Uuid::now_v7().as_bytes());
    let mut tx = store.begin().await?;
    tx.enroll_node(node, &key).await?;
    tx.commit().await
}
