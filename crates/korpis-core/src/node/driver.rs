// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node's drivers and storage declare they can do (§4.1 of docs/04-runtimes.md). Declared,
//! never inferred: the scheduler asks these and never assumes from a version (Rule K-9).

use crate::workload::plan::StepAction;
use crate::workload::spec::{ConsoleMode, IsolationTier};
use std::collections::BTreeMap;

pub use super::device::DeviceMode;

/// A class of device a driver can attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceClass {
    /// A GPU.
    Gpu,
    /// A USB device.
    Usb,
    /// A PCI device.
    Pci,
    /// A TPM.
    Tpm,
    /// A serial port.
    Serial,
}

/// Where a root filesystem can come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RootfsSource {
    /// An OCI image.
    OciImage,
    /// A directory on the host.
    Directory,
    /// A disk image.
    DiskImage,
    /// An installation ISO.
    Iso,
}

/// How a driver connects a workload to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NetworkMode {
    /// A veth pair.
    Veth,
    /// A tap device.
    Tap,
    /// The host's network.
    Host,
    /// No network.
    None,
}

/// What a volume is to the workload (§3 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeSemantics {
    /// A directory on a filesystem the host controls.
    Filesystem,
    /// A block device the guest owns.
    BlockDevice,
}

/// How a workload's files are reached (§4.4 of docs/04-runtimes.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    /// A worker on the host.
    HostWorker,
    /// A worker inside the guest.
    GuestWorker,
    /// Not at all.
    None,
}

/// Whether, and how, a driver holds one kind of limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enforcement {
    /// The kernel holds it; when false, the limit is neither accepted nor displayed (Rule K-3).
    pub enforced: bool,
    /// For example `cgroup v2 memory.max`.
    pub mechanism: String,
    /// What the mechanism actually bounds, where it differs.
    pub granularity: String,
}

/// Where an execution-state snapshot can be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCaps {
    /// The CPU model or template.
    pub cpu_template: String,
    /// The hypervisor version.
    pub hypervisor_version: String,
    /// The oldest host kernel.
    pub kernel_min: String,
    /// The newest host kernel.
    pub kernel_max: String,
}

/// How workload uids reach the disk (§4.5 of docs/04-runtimes.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdMapCaps {
    /// Id-mapped mounts.
    pub idmapped_mounts: bool,
    /// User namespaces.
    pub user_namespaces: bool,
}

/// A driver's declared capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a capability the driver declares on its own (§4.1 of docs/04-runtimes.md)"
)]
pub struct Capabilities {
    /// The isolation tiers it implements.
    pub tiers: Vec<IsolationTier>,
    /// The console modes it offers.
    pub console: Vec<ConsoleMode>,
    /// Whether it can run a command inside a running workload.
    pub exec: bool,
    /// Driver-defined signal names.
    pub signals: Vec<String>,
    /// The resources that change without a restart.
    pub hot_resize: Vec<String>,
    /// Whether a volume attaches to a running workload.
    pub hot_volume_attach: bool,
    /// Where execution-state snapshots restore; `None` when they are not supported.
    pub runtime_snapshot: Option<SnapshotCaps>,
    /// Whether it migrates a running workload.
    pub live_migrate: bool,
    /// The modes it gives devices of the classes in `devices` in; none gives none (§6.2 of
    /// docs/05-scheduling.md).
    pub device_modes: Vec<DeviceMode>,
    /// The device classes it attaches.
    pub devices: Vec<DeviceClass>,
    /// Where root filesystems come from.
    pub rootfs_sources: Vec<RootfsSource>,
    /// How it networks.
    pub network_modes: Vec<NetworkMode>,
    /// Per resource kind (`memory`, `cpu`, `disk`, `io`, `pids`, `egress`): whether it holds
    /// a limit of that kind. A kind absent is not enforced.
    pub enforcement: BTreeMap<String, Enforcement>,
    /// What its volumes are.
    pub volume_semantics: VolumeSemantics,
    /// How files are reached.
    pub file_access: FileAccess,
    /// How uids reach the disk.
    pub id_mapping: IdMapCaps,
    /// Its configuration's JSON Schema document.
    pub config_schema: String,
    /// Per tier, the step actions it makes disruptive there: the driver's layer of the
    /// classification chain (§3.3 of docs/01-model.md), which only ever adds to the floor.
    pub disruptive: Vec<(IsolationTier, Vec<StepAction>)>,
    /// Whether it gives each workload the directory its node serves it, at `/run/korpis`: the
    /// identity socket, the API relay, and the managed and dependency files (§6 of
    /// docs/02-architecture.md).
    pub identity_dir: bool,
    /// Whether it can ask a guest's own agent whether it answers (§5 of docs/04-runtimes.md).
    pub guest_agent: bool,
}

impl Capabilities {
    /// Whether a limit of `kind` is held by the kernel.
    #[must_use]
    pub fn enforces(&self, kind: &str) -> bool {
        self.enforcement.get(kind).is_some_and(|e| e.enforced)
    }

    /// Whether it gives a device of `class` in `mode`.
    #[must_use]
    pub fn gives(&self, class: DeviceClass, mode: DeviceMode) -> bool {
        self.devices.contains(&class) && self.device_modes.contains(&mode)
    }
}

/// A runtime driver as a node offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverInfo {
    /// For example `oci`.
    pub name: String,
    /// Its version, informational only.
    pub version: String,
    /// The driver protocol version it speaks.
    pub protocol: String,
    /// What it declares it can do.
    pub capabilities: Capabilities,
    /// Whether it last reported itself healthy.
    pub healthy: bool,
}

/// A storage class as a node offers it (§2 of docs/06-storage.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageClass {
    /// Its name.
    pub name: String,
    /// What implements it.
    pub backend: StorageBackend,
    /// What it can do, declared by the helper and never inferred from the backend.
    pub capabilities: StorageCaps,
    /// Total bytes.
    pub capacity_bytes: u64,
    /// Free bytes, as last reported.
    pub free_bytes: u64,
}

/// What implements a storage class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageBackend {
    /// ZFS datasets.
    Zfs,
    /// Btrfs subvolumes.
    Btrfs,
    /// LVM thin volumes.
    LvmThin,
    /// XFS with a project quota per volume.
    XfsProject,
    /// ext4 with a project quota per volume.
    Ext4Project,
    /// Storage reached over the network.
    Network,
    /// A directory on a filesystem with no project quota: nothing is enforced.
    Directory,
}

/// One thing a storage class can do (§2 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageFeature {
    /// Point-in-time snapshots.
    Snapshot,
    /// Snapshots atomic across the volume.
    SnapshotAtomic,
    /// Sending only what changed since a snapshot.
    IncrementalSend,
    /// The kernel fails the write at the quota, not a monitor after it.
    QuotaEnforced,
    /// A byte quota.
    QuotaCapacity,
    /// An inode quota.
    QuotaInodes,
    /// Space taken as written, not at creation.
    ThinProvision,
    /// Grows while mounted.
    OnlineGrow,
    /// Shrinks while mounted.
    OnlineShrink,
    /// Copies by reference.
    ReflinkCopy,
    /// Compresses.
    Compression,
    /// Encrypts at rest.
    EncryptionAtRest,
    /// More than one workload may mount it.
    SharedAccess,
    /// More than one node may attach it.
    MultiNodeAttach,
}

/// How a storage class stops a writer that has lost its lease (§2 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fencing {
    /// It does not.
    #[default]
    None,
    /// Only by the lease: a writer that keeps writing is not stopped by the storage.
    LeaseOnly,
    /// The storage refuses the old writer.
    StorageEnforced,
}

/// What a storage class declares it can do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageCaps {
    /// The features it has; one absent is a feature it lacks.
    pub features: std::collections::BTreeSet<StorageFeature>,
    /// How it fences.
    pub fencing: Fencing,
}

impl StorageCaps {
    /// Whether it has `feature`.
    #[must_use]
    pub fn has(&self, feature: StorageFeature) -> bool {
        self.features.contains(&feature)
    }
}
