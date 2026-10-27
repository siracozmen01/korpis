// SPDX-License-Identifier: AGPL-3.0-or-later

//! Storage classes as the helper declares them (§2 of docs/06-storage.md): what implements each
//! and what it can do. Read strictly: an unknown backend or fencing mode is refused rather than
//! read as one the scheduler or the backup planner would then trust.

use crate::{Result, invalid};
use korpis_core::node::StorageClass;
use korpis_core::node::driver::{Fencing, StorageBackend, StorageCaps, StorageFeature};
use korpis_proto::korpis::v1 as pb;

/// Each feature with the wire field that carries it.
fn flags(c: &pb::StorageCapabilities) -> [(StorageFeature, bool); 14] {
    use StorageFeature as F;
    [
        (F::Snapshot, c.snapshot),
        (F::SnapshotAtomic, c.snapshot_atomic),
        (F::IncrementalSend, c.incremental_send),
        (F::QuotaEnforced, c.quota_enforced),
        (F::QuotaCapacity, c.quota_capacity),
        (F::QuotaInodes, c.quota_inodes),
        (F::ThinProvision, c.thin_provision),
        (F::OnlineGrow, c.online_grow),
        (F::OnlineShrink, c.online_shrink),
        (F::ReflinkCopy, c.reflink_copy),
        (F::Compression, c.compression),
        (F::EncryptionAtRest, c.encryption_at_rest),
        (F::SharedAccess, c.shared_access),
        (F::MultiNodeAttach, c.multi_node_attach),
    ]
}

fn backend_from_pb(field: &str, b: buffa::EnumValue<pb::StorageBackend>) -> Result<StorageBackend> {
    use pb::StorageBackend as P;
    Ok(match b.as_known() {
        Some(P::STORAGE_BACKEND_ZFS) => StorageBackend::Zfs,
        Some(P::STORAGE_BACKEND_BTRFS) => StorageBackend::Btrfs,
        Some(P::STORAGE_BACKEND_LVM_THIN) => StorageBackend::LvmThin,
        Some(P::STORAGE_BACKEND_XFS_PROJECT) => StorageBackend::XfsProject,
        Some(P::STORAGE_BACKEND_EXT4_PROJECT) => StorageBackend::Ext4Project,
        Some(P::STORAGE_BACKEND_NETWORK) => StorageBackend::Network,
        Some(P::STORAGE_BACKEND_DIRECTORY) => StorageBackend::Directory,
        Some(P::STORAGE_BACKEND_UNSPECIFIED) | None => return Err(invalid(field, "required")),
    })
}

const fn backend_to_pb(b: StorageBackend) -> pb::StorageBackend {
    use pb::StorageBackend as P;
    match b {
        StorageBackend::Zfs => P::STORAGE_BACKEND_ZFS,
        StorageBackend::Btrfs => P::STORAGE_BACKEND_BTRFS,
        StorageBackend::LvmThin => P::STORAGE_BACKEND_LVM_THIN,
        StorageBackend::XfsProject => P::STORAGE_BACKEND_XFS_PROJECT,
        StorageBackend::Ext4Project => P::STORAGE_BACKEND_EXT4_PROJECT,
        StorageBackend::Network => P::STORAGE_BACKEND_NETWORK,
        StorageBackend::Directory => P::STORAGE_BACKEND_DIRECTORY,
    }
}

pub(crate) fn fencing_from_pb(field: &str, f: buffa::EnumValue<pb::Fencing>) -> Result<Fencing> {
    Ok(match f.as_known() {
        Some(pb::Fencing::FENCING_NONE) => Fencing::None,
        Some(pb::Fencing::FENCING_LEASE_ONLY) => Fencing::LeaseOnly,
        Some(pb::Fencing::FENCING_STORAGE_ENFORCED) => Fencing::StorageEnforced,
        Some(pb::Fencing::FENCING_UNSPECIFIED) | None => return Err(invalid(field, "required")),
    })
}

pub(crate) const fn fencing_to_pb(f: Fencing) -> pb::Fencing {
    match f {
        Fencing::None => pb::Fencing::FENCING_NONE,
        Fencing::LeaseOnly => pb::Fencing::FENCING_LEASE_ONLY,
        Fencing::StorageEnforced => pb::Fencing::FENCING_STORAGE_ENFORCED,
    }
}

/// A shared class's store: a UUID, the form the helper writes it in, and only on a class reached
/// over the network, since a class on one node's disk is no storage another node shares. A block
/// class is reached over the network too, and names no store: its store is its organization's,
/// which the control plane sets (§2.1 of docs/06-storage.md); and it declares
/// `multi_node_attach`, which makes `stop` the derived answer to a lease's end for a workload
/// holding one of its volumes, so that it can fail over (§2 of docs/06-storage.md).
fn store_from_pb(field: &str, c: &pb::StorageClassInfo) -> Result<Option<String>> {
    let network = c.backend.as_known() == Some(pb::StorageBackend::STORAGE_BACKEND_NETWORK);
    if crate::block::protocol_from_pb(c.block_protocol).is_some() {
        if !network {
            return Err(invalid(field, "a block class is reached over the network"));
        }
        if !c.store.is_empty() {
            return Err(invalid(field, "a block class's store is its organization's"));
        }
        if !c.capabilities.as_option().is_some_and(|caps| caps.multi_node_attach) {
            return Err(invalid(field, "a block class is attachable from another node"));
        }
        return Ok(None);
    }
    if c.store.is_empty() {
        return Ok(None);
    }
    if !network {
        return Err(invalid(field, "only a class reached over the network has a store"));
    }
    let store = uuid::Uuid::parse_str(&c.store).map_err(|_| invalid(field, "not a UUID"))?;
    Ok(Some(store.hyphenated().to_string()))
}

/// The storage classes a helper declares.
///
/// # Errors
/// The field of a class with no backend or no fencing mode.
pub fn storage_classes_from_pb(
    field: &str,
    classes: &[pb::StorageClassInfo],
) -> Result<Vec<StorageClass>> {
    classes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let at = |name: &str| format!("{field}[{i}].{name}");
            let caps = c
                .capabilities
                .as_option()
                .ok_or_else(|| invalid(&at("capabilities"), "required"))?;
            let features = flags(caps).into_iter().filter(|(_, on)| *on).map(|(f, _)| f).collect();
            Ok(StorageClass {
                name: c.name.clone(),
                backend: backend_from_pb(&at("backend"), c.backend)?,
                capabilities: StorageCaps {
                    features,
                    fencing: fencing_from_pb(&at("capabilities.fencing"), caps.fencing)?,
                },
                capacity_bytes: c.capacity_bytes,
                free_bytes: c.free_bytes,
                store: store_from_pb(&at("store"), c)?,
                block: crate::block::protocol_from_pb(c.block_protocol),
            })
        })
        .collect()
}

/// The wire form of a storage class.
#[must_use]
pub fn storage_class_to_pb(c: &StorageClass) -> pb::StorageClassInfo {
    use StorageFeature as F;
    let has = |f| c.capabilities.has(f);
    pb::StorageClassInfo {
        name: c.name.clone(),
        backend: backend_to_pb(c.backend).into(),
        capabilities: pb::StorageCapabilities {
            snapshot: has(F::Snapshot),
            snapshot_atomic: has(F::SnapshotAtomic),
            incremental_send: has(F::IncrementalSend),
            quota_enforced: has(F::QuotaEnforced),
            quota_capacity: has(F::QuotaCapacity),
            quota_inodes: has(F::QuotaInodes),
            thin_provision: has(F::ThinProvision),
            online_grow: has(F::OnlineGrow),
            online_shrink: has(F::OnlineShrink),
            reflink_copy: has(F::ReflinkCopy),
            compression: has(F::Compression),
            encryption_at_rest: has(F::EncryptionAtRest),
            shared_access: has(F::SharedAccess),
            multi_node_attach: has(F::MultiNodeAttach),
            fencing: fencing_to_pb(c.capabilities.fencing).into(),
            ..Default::default()
        }
        .into(),
        capacity_bytes: c.capacity_bytes,
        free_bytes: c.free_bytes,
        store: c.store.clone().unwrap_or_default(),
        block_protocol: c.block.map(crate::block::protocol_to_pb).unwrap_or_default().into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]
    use super::*;
    use korpis_core::extension::block::BlockProtocol;

    fn class(backend: pb::StorageBackend, store: &str) -> pb::StorageClassInfo {
        pb::StorageClassInfo {
            name: "shared".into(),
            backend: backend.into(),
            capabilities: pb::StorageCapabilities {
                shared_access: true,
                multi_node_attach: true,
                fencing: pb::Fencing::FENCING_LEASE_ONLY.into(),
                ..Default::default()
            }
            .into(),
            store: store.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_store_is_a_uuid_on_a_class_reached_over_the_network() {
        let network = pb::StorageBackend::STORAGE_BACKEND_NETWORK;
        let store = "0B9C1F40-3D2E-4C55-9F0A-7E6D5C4B3A21";
        let read = storage_classes_from_pb("c", &[class(network, store)]).unwrap();
        let wanted = "0b9c1f40-3d2e-4c55-9f0a-7e6d5c4b3a21";
        assert_eq!(read[0].store.as_deref(), Some(wanted), "in one form, as every node reads it");
        assert_eq!(storage_class_to_pb(&read[0]).store, wanted, "and back");
        let local = class(pb::StorageBackend::STORAGE_BACKEND_BTRFS, store);
        assert!(storage_classes_from_pb("c", &[local]).is_err(), "a node's own disk is no store");
        assert!(storage_classes_from_pb("c", &[class(network, "nfs-1")]).is_err());
        let none = storage_classes_from_pb("c", &[class(network, "")]).unwrap();
        assert_eq!(none[0].store, None);
    }

    #[test]
    fn a_block_class_is_reached_over_the_network_and_names_no_store() {
        let block = |backend, store| pb::StorageClassInfo {
            block_protocol: pb::BlockProtocol::BLOCK_PROTOCOL_ISCSI.into(),
            ..class(backend, store)
        };
        let network = pb::StorageBackend::STORAGE_BACKEND_NETWORK;
        let read = storage_classes_from_pb("c", &[block(network, "")]).unwrap();
        assert_eq!((read[0].block, read[0].store.as_deref()), (Some(BlockProtocol::Iscsi), None));
        let store = "0b9c1f40-3d2e-4c55-9f0a-7e6d5c4b3a21";
        assert!(storage_classes_from_pb("c", &[block(network, store)]).is_err(), "its own store");
        let local = block(pb::StorageBackend::STORAGE_BACKEND_BTRFS, "");
        assert!(storage_classes_from_pb("c", &[local]).is_err(), "on a node's own disk");
        let mut one = block(network, "");
        one.capabilities.as_option_mut().unwrap().multi_node_attach = false;
        assert!(storage_classes_from_pb("c", &[one]).is_err(), "attachable from one node alone");
    }
}
