// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices (§2.1 of docs/06-storage.md): wanted for a placed workload's volumes,
//! each made and attached by its class's provider, and what the node it is attached to needs.

use super::{SealedKey, StoreResult, VaultError};
use crate::page::Page;
use korpis_core::extension::block::BlockProtocol;
use korpis_core::workload::block::{DeviceStatus, DueDevice, WantedDevice};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// What a node needs to attach a device, its credential sealed to the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedAttachment {
    /// How to attach it.
    pub protocol: BlockProtocol,
    /// Where it is.
    pub target: String,
    /// What the node presents for it, sealed; `None` where none is needed.
    pub credential: Option<SealedKey>,
}

/// A device attached to a node, as that node's assignment carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldDevice {
    /// The device, its volume's identifier to be.
    pub id: Uuid,
    /// The helper's identifier of its volume.
    pub helper_id: String,
    /// What the node needs.
    pub attachment: SealedAttachment,
    /// Whether it is a guest's disk.
    pub guest: bool,
    /// Whether no volume was made of it yet.
    pub blank: bool,
}

/// Reading devices.
pub trait BlockRead {
    /// A page of the workloads, with the node each is placed on, whose node offers a block
    /// class, in workload order: those whose devices are tended.
    fn placed_on_block(
        &self,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<(Uuid, Uuid)>>> + Send;

    /// The devices of `workload` attached to the node and epoch of `at`.
    fn held_devices(
        &self,
        workload: Uuid,
        at: (Uuid, u64),
    ) -> impl Future<Output = StoreResult<Vec<HeldDevice>>> + Send;

    /// The live devices of `workload`, by name, as its status shows them at `now`.
    fn device_status(
        &self,
        workload: Uuid,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<DeviceStatus>>> + Send;
}

/// Writing devices.
pub trait BlockWrite {
    /// Records `d` as wanted, unless a device is kept for its store and helper identifier
    /// already, which then follows `d`'s workload; whether it was new.
    fn want_device(&mut self, d: &WantedDevice) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Takes the first device due a call at `now`, holding it until `until`, so that one
    /// control plane asks.
    fn take_due_device(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<DueDevice>>> + Send;

    /// Records that the provider made `id` as `device`.
    fn device_created(
        &mut self,
        id: Uuid,
        device: &str,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that the provider attached `id` to the node and epoch of `at`, answering
    /// `attachment`.
    fn device_attached(
        &mut self,
        id: Uuid,
        at: (Uuid, u64),
        attachment: &SealedAttachment,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that the provider detached `id` from the node it was attached to.
    fn device_detached(&mut self, id: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `id` is deleted at `now`: the provider deleted it, or it was never made.
    fn device_deleted(
        &mut self,
        id: Uuid,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records why the last call for `id` failed and how many failed since one answered, as
    /// `failure`, and when to ask again, `due`.
    fn device_failed(
        &mut self,
        id: Uuid,
        failure: (&str, u32),
        due: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}

/// What seals a device's credential to the device alone: the control plane's device key, in its
/// data directory beside its other keys.
pub trait DeviceVault: Send + Sync + std::fmt::Debug {
    /// `credential`, sealed to `device`.
    fn seal(&self, device: Uuid, credential: &str) -> Result<SealedKey, VaultError>;

    /// The credential of `device`, opened from `sealed`; wiped when dropped.
    fn open(
        &self,
        device: Uuid,
        sealed: &SealedKey,
    ) -> Result<zeroize::Zeroizing<String>, VaultError>;
}
