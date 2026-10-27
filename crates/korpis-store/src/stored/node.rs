// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's taints, drivers, storage classes, and capacities, as stored in the `node` table.

use super::from_json;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::extension::block::BlockProtocol;
use korpis_core::node::driver::{
    DeviceClass, DeviceMode, Fencing, FileAccess, IdMapCaps, NetworkMode, RootfsSource,
    SnapshotCaps, StorageBackend, StorageCaps, StorageFeature, VolumeSemantics,
};
use korpis_core::node::{Capabilities, DriverInfo, Enforcement, StorageClass, Taint};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_core::workload::plan::StepAction;
use korpis_core::workload::spec::{ConsoleMode, IsolationTier};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A store-owned mirror of a domain enum, so a variant renamed in the domain changes this
/// mapping and never the stored name.
macro_rules! mirror {
    ($doc:ident, $domain:ident { $($v:ident),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $doc { $($v),* }
        impl From<$domain> for $doc {
            fn from(d: $domain) -> Self { match d { $($domain::$v => Self::$v),* } }
        }
        impl From<$doc> for $domain {
            fn from(d: $doc) -> Self { match d { $($doc::$v => Self::$v),* } }
        }
    };
}

mirror!(TierDoc, IsolationTier { Process, Container, MicroVm, Vm });
mirror!(ConsoleDoc, ConsoleMode { None, Logs, Interactive, Serial });
mirror!(ModeDoc, DeviceMode { Passthrough, Mediated, Timeslice });
mirror!(DeviceDoc, DeviceClass { Gpu, Usb, Pci, Tpm, Serial });
mirror!(RootfsDoc, RootfsSource { OciImage, Directory, DiskImage, Iso });
mirror!(NetworkDoc, NetworkMode { Veth, Tap, Host, None });
mirror!(VolumeSemanticsDoc, VolumeSemantics { Filesystem, BlockDevice });
mirror!(FileAccessDoc, FileAccess { HostWorker, GuestWorker, None });

fn all<D: Copy + Into<T>, T>(v: &[D]) -> Vec<T> {
    v.iter().map(|d| (*d).into()).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaintDoc {
    key: String,
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnforcementDoc {
    enforced: bool,
    mechanism: String,
    granularity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotDoc {
    cpu_template: String,
    hypervisor_version: String,
    kernel_min: String,
    kernel_max: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "the domain's capabilities, stored as declared")]
struct CapsDoc {
    tiers: Vec<TierDoc>,
    console: Vec<ConsoleDoc>,
    exec: bool,
    /// Absent from a node stored before drivers declared it, whose drivers are asked again
    /// at its next session.
    #[serde(default)]
    identity_dir: bool,
    #[serde(default)]
    guest_agent: bool,
    signals: Vec<String>,
    hot_resize: Vec<String>,
    hot_volume_attach: bool,
    runtime_snapshot: Option<SnapshotDoc>,
    live_migrate: bool,
    /// Read from a driver stored before modes applied to every class, whose `gpu` is ignored
    /// and which declared none, as empty (§4.1 of docs/04-runtimes.md).
    #[serde(default)]
    device_modes: Vec<ModeDoc>,
    devices: Vec<DeviceDoc>,
    rootfs_sources: Vec<RootfsDoc>,
    network_modes: Vec<NetworkDoc>,
    enforcement: BTreeMap<String, EnforcementDoc>,
    volume_semantics: VolumeSemanticsDoc,
    file_access: FileAccessDoc,
    id_mapping: IdMapDoc,
    config_schema: String,
    // Absent from documents a driver stored before it could declare any.
    #[serde(default)]
    disruption: Vec<(TierDoc, Vec<String>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdMapDoc {
    idmapped_mounts: bool,
    user_namespaces: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DriverDoc {
    name: String,
    version: String,
    protocol: String,
    healthy: bool,
    capabilities: CapsDoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StorageClassDoc {
    name: String,
    // Absent from classes stored before the helper's declaration was kept; read as what every
    // such class claimed, which is nothing (§2 of docs/06-storage.md).
    #[serde(default = "directory")]
    backend: BackendDoc,
    #[serde(default)]
    features: Vec<FeatureDoc>,
    #[serde(default = "unfenced")]
    fencing: FencingDoc,
    capacity_bytes: u64,
    free_bytes: u64,
    // A shared class's store (§2.1 of docs/06-storage.md); absent from every class on a node's
    // own disk, and from every class stored before shared ones existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store: Option<String>,
    // A block class's protocol (§2.1 of docs/06-storage.md); absent from every other class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    block: Option<String>,
}

mirror!(
    BackendDoc,
    StorageBackend { Zfs, Btrfs, LvmThin, XfsProject, Ext4Project, Network, Directory }
);
mirror!(FencingDoc, Fencing { None, LeaseOnly, StorageEnforced });
mirror!(
    FeatureDoc,
    StorageFeature {
        Snapshot,
        SnapshotAtomic,
        IncrementalSend,
        QuotaEnforced,
        QuotaCapacity,
        QuotaInodes,
        ThinProvision,
        OnlineGrow,
        OnlineShrink,
        ReflinkCopy,
        Compression,
        EncryptionAtRest,
        SharedAccess,
        MultiNodeAttach,
    }
);

const fn directory() -> BackendDoc {
    BackendDoc::Directory
}

const fn unfenced() -> FencingDoc {
    FencingDoc::None
}

impl CapsDoc {
    fn from_domain(c: &Capabilities) -> Self {
        Self {
            tiers: all(&c.tiers),
            console: all(&c.console),
            exec: c.exec,
            identity_dir: c.identity_dir,
            guest_agent: c.guest_agent,
            signals: c.signals.clone(),
            hot_resize: c.hot_resize.clone(),
            hot_volume_attach: c.hot_volume_attach,
            runtime_snapshot: c.runtime_snapshot.as_ref().map(|s| SnapshotDoc {
                cpu_template: s.cpu_template.clone(),
                hypervisor_version: s.hypervisor_version.clone(),
                kernel_min: s.kernel_min.clone(),
                kernel_max: s.kernel_max.clone(),
            }),
            live_migrate: c.live_migrate,
            device_modes: all(&c.device_modes),
            devices: all(&c.devices),
            rootfs_sources: all(&c.rootfs_sources),
            network_modes: all(&c.network_modes),
            enforcement: c
                .enforcement
                .iter()
                .map(|(k, e)| {
                    let doc = EnforcementDoc {
                        enforced: e.enforced,
                        mechanism: e.mechanism.clone(),
                        granularity: e.granularity.clone(),
                    };
                    (k.clone(), doc)
                })
                .collect(),
            volume_semantics: c.volume_semantics.into(),
            file_access: c.file_access.into(),
            id_mapping: IdMapDoc {
                idmapped_mounts: c.id_mapping.idmapped_mounts,
                user_namespaces: c.id_mapping.user_namespaces,
            },
            config_schema: c.config_schema.clone(),
            disruption: c
                .disruptive
                .iter()
                .map(|(t, a)| ((*t).into(), a.iter().map(|a| a.as_str().to_owned()).collect()))
                .collect(),
        }
    }

    fn into_domain(self) -> StoreResult<Capabilities> {
        let disruptive = disruption(self.disruption)?;
        Ok(Capabilities {
            tiers: all(&self.tiers),
            console: all(&self.console),
            exec: self.exec,
            identity_dir: self.identity_dir,
            guest_agent: self.guest_agent,
            signals: self.signals,
            hot_resize: self.hot_resize,
            hot_volume_attach: self.hot_volume_attach,
            runtime_snapshot: self.runtime_snapshot.map(|s| SnapshotCaps {
                cpu_template: s.cpu_template,
                hypervisor_version: s.hypervisor_version,
                kernel_min: s.kernel_min,
                kernel_max: s.kernel_max,
            }),
            live_migrate: self.live_migrate,
            device_modes: all(&self.device_modes),
            devices: all(&self.devices),
            rootfs_sources: all(&self.rootfs_sources),
            network_modes: all(&self.network_modes),
            enforcement: self
                .enforcement
                .into_iter()
                .map(|(k, e)| {
                    let e = Enforcement {
                        enforced: e.enforced,
                        mechanism: e.mechanism,
                        granularity: e.granularity,
                    };
                    (k, e)
                })
                .collect(),
            volume_semantics: self.volume_semantics.into(),
            file_access: self.file_access.into(),
            id_mapping: IdMapCaps {
                idmapped_mounts: self.id_mapping.idmapped_mounts,
                user_namespaces: self.id_mapping.user_namespaces,
            },
            config_schema: self.config_schema,
            disruptive,
        })
    }
}

fn disruption(
    docs: Vec<(TierDoc, Vec<String>)>,
) -> StoreResult<Vec<(IsolationTier, Vec<StepAction>)>> {
    docs.into_iter()
        .map(|(tier, actions)| {
            let parsed = actions
                .iter()
                .map(|a| {
                    StepAction::parse(a)
                        .ok_or_else(|| StoreError::Corrupt(format!("driver disruption {a:?}")))
                })
                .collect::<StoreResult<_>>()?;
            Ok((tier.into(), parsed))
        })
        .collect()
}

/// Taints, drivers, and storage classes to store.
pub(crate) fn node_docs(
    taints: &[Taint],
    drivers: &[DriverInfo],
    classes: &[StorageClass],
) -> (Vec<TaintDoc>, Vec<DriverDoc>, Vec<StorageClassDoc>) {
    let taints = taints.iter().map(|t| TaintDoc { key: t.key.clone(), value: t.value.clone() });
    let drivers = drivers.iter().map(|d| DriverDoc {
        name: d.name.clone(),
        version: d.version.clone(),
        protocol: d.protocol.clone(),
        healthy: d.healthy,
        capabilities: CapsDoc::from_domain(&d.capabilities),
    });
    let classes = classes.iter().map(|c| StorageClassDoc {
        name: c.name.clone(),
        backend: c.backend.into(),
        features: c.capabilities.features.iter().map(|f| (*f).into()).collect(),
        fencing: c.capabilities.fencing.into(),
        capacity_bytes: c.capacity_bytes,
        free_bytes: c.free_bytes,
        store: c.store.clone(),
        block: c.block.map(|b| b.as_str().to_owned()),
    });
    (taints.collect(), drivers.collect(), classes.collect())
}

/// Stored taints.
pub(crate) fn taints(v: Value) -> StoreResult<Vec<Taint>> {
    let docs: Vec<TaintDoc> = from_json("node taints", v)?;
    Ok(docs.into_iter().map(|t| Taint { key: t.key, value: t.value }).collect())
}

/// Stored drivers.
pub(crate) fn drivers(v: Value) -> StoreResult<Vec<DriverInfo>> {
    let docs: Vec<DriverDoc> = from_json("node drivers", v)?;
    docs.into_iter()
        .map(|d| {
            Ok(DriverInfo {
                name: d.name,
                version: d.version,
                protocol: d.protocol,
                healthy: d.healthy,
                capabilities: d.capabilities.into_domain()?,
            })
        })
        .collect()
}

/// Stored storage classes.
pub(crate) fn storage_classes(v: Value) -> StoreResult<Vec<StorageClass>> {
    let docs: Vec<StorageClassDoc> = from_json("node storage classes", v)?;
    Ok(docs
        .into_iter()
        .map(|c| StorageClass {
            name: c.name,
            backend: c.backend.into(),
            capabilities: StorageCaps {
                features: c.features.into_iter().map(Into::into).collect(),
                fencing: c.fencing.into(),
            },
            capacity_bytes: c.capacity_bytes,
            free_bytes: c.free_bytes,
            store: c.store,
            block: c.block.as_deref().and_then(BlockProtocol::parse),
        })
        .collect())
}

/// Stored amounts per dimension, keyed by the dimension's wire name.
pub(crate) fn bounds(what: &str, v: Value) -> StoreResult<Bounds> {
    let raw: BTreeMap<String, u64> = from_json(what, v)?;
    raw.into_iter()
        .map(|(k, n)| {
            Dimension::parse(&k)
                .map(|d| (d, n))
                .ok_or_else(|| StoreError::Corrupt(format!("{what}: dimension {k:?}")))
        })
        .collect()
}

/// Amounts per dimension to store.
pub(crate) fn bounds_doc(b: &Bounds) -> Value {
    Value::Object(b.iter().map(|(d, n)| (d.as_str().to_owned(), Value::from(*n))).collect())
}
