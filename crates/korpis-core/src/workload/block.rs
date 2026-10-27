// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices (§2.1 of docs/06-storage.md): one per volume, made by the class's
//! provider and attached to one node for one lease epoch. What a device is asked for next is
//! decided here from what it is and what is wanted of it: made, if it is not; attached, if the
//! node and epoch its workload holds its lease for are not the ones it was attached for, which
//! the provider does by fencing whichever node held it before; detached, if no lease is held for
//! the node and epoch it was attached for; and deleted, once its volume is, or once its workload
//! is and no volume was ever made of it, after it is detached.

use crate::extension::block::BlockProtocol;
use std::time::Duration;
use uuid::Uuid;

/// A device wanted for a volume of a workload placed on a node offering its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedDevice {
    /// The identifier its volume takes.
    pub id: Uuid,
    /// The workload's organization.
    pub organization: Uuid,
    /// The workload.
    pub workload: Uuid,
    /// The class's store.
    pub store: String,
    /// The helper's identifier its volume has there.
    pub helper_id: String,
    /// Its volume's name in its workload's declaration.
    pub name: String,
    /// The class, and so its provider's name.
    pub storage_class: String,
    /// Its size.
    pub size_bytes: u64,
}

/// A device due a call, as the control plane that took it sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDevice {
    /// Its identifier, its volume's.
    pub id: Uuid,
    /// Its organization, whose provider is asked.
    pub organization: Uuid,
    /// The class, and so its provider's name.
    pub storage_class: String,
    /// Its size.
    pub size_bytes: u64,
    /// The provider's name for it, once made.
    pub device: Option<String>,
    /// The node its workload holds its lease on, the epoch of that lease, and how that node
    /// attaches the class's devices; `None` while no lease is held on the node it is placed on.
    pub wanted: Option<(Uuid, u64, BlockProtocol)>,
    /// The node and epoch it was last attached for.
    pub attached: Option<(Uuid, u64)>,
    /// Whether it is to be deleted: its volume was, or its workload was and no volume was made.
    pub gone: bool,
    /// How many calls failed since one answered.
    pub attempts: u32,
}

/// What a device is asked for next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCall {
    /// Made, for its volume.
    Create,
    /// Attached to a node for an epoch, by a protocol.
    Attach {
        /// The provider's name for it.
        device: String,
        /// The node.
        node: Uuid,
        /// The epoch.
        epoch: u64,
        /// How the node attaches it.
        protocol: BlockProtocol,
    },
    /// Detached from the node and epoch it was attached for.
    Detach {
        /// The provider's name for it.
        device: String,
        /// The node.
        node: Uuid,
        /// The epoch.
        epoch: u64,
    },
    /// Deleted, with every byte of it.
    Delete {
        /// The provider's name for it.
        device: String,
    },
    /// Forgotten: it is to be deleted and was never made, so no provider is asked.
    Forget,
}

impl DueDevice {
    /// What it is asked for next; `None` when it is as it is wanted.
    #[must_use]
    pub fn next(&self) -> Option<DeviceCall> {
        let Some(device) = self.device.clone() else {
            return Some(if self.gone { DeviceCall::Forget } else { DeviceCall::Create });
        };
        match (self.wanted.filter(|_| !self.gone), self.attached) {
            (Some((node, epoch, protocol)), at) if at != Some((node, epoch)) => {
                Some(DeviceCall::Attach { device, node, epoch, protocol })
            }
            (None, Some((node, epoch))) => Some(DeviceCall::Detach { device, node, epoch }),
            (None, None) if self.gone => Some(DeviceCall::Delete { device }),
            _ => None,
        }
    }
}

/// A device as its workload's status shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    /// Its volume's name in the workload's declaration.
    pub name: String,
    /// The class, and so the name of the provider asked for it.
    pub storage_class: String,
    /// Whether the provider has made it.
    pub made: bool,
    /// Whether it is attached for the lease the workload holds.
    pub attached: bool,
    /// Why the provider's last answer failed, while it is asked again.
    pub reason: Option<String>,
}

/// How long after `attempts` failures a device is asked again: ten seconds doubling to ten
/// minutes, as an address's router is.
#[must_use]
pub fn device_backoff(attempts: u32) -> Duration {
    let doubled = 10u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(6));
    Duration::from_secs(doubled.min(600))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(device: Option<&str>, wanted: Option<(Uuid, u64)>) -> DueDevice {
        DueDevice {
            id: Uuid::now_v7(),
            organization: Uuid::now_v7(),
            storage_class: "fast".into(),
            size_bytes: 1 << 30,
            device: device.map(str::to_owned),
            wanted: wanted.map(|(n, e)| (n, e, BlockProtocol::Loop)),
            attached: None,
            gone: false,
            attempts: 0,
        }
    }

    #[test]
    fn a_device_is_made_then_attached_at_the_epoch_of_its_workload_s_lease() {
        let (node, other) = (Uuid::now_v7(), Uuid::now_v7());
        let mut d = due(None, Some((node, 1)));
        assert_eq!(d.next(), Some(DeviceCall::Create), "made first");
        d.device = Some("img".into());
        let attach = |node, epoch| DeviceCall::Attach {
            device: "img".into(),
            node,
            epoch,
            protocol: BlockProtocol::Loop,
        };
        assert_eq!(d.next(), Some(attach(node, 1)));
        d.attached = Some((node, 1));
        assert_eq!(d.next(), None, "as it is wanted");
        d.wanted = Some((other, 2, BlockProtocol::Loop));
        assert_eq!(d.next(), Some(attach(other, 2)), "failed over, at the new epoch");
        d.attached = Some((other, 2));
        d.wanted = None;
        let detach = DeviceCall::Detach { device: "img".into(), node: other, epoch: 2 };
        assert_eq!(d.next(), Some(detach), "no lease held for it: detached");
        d.attached = None;
        assert_eq!(d.next(), None, "detached, and no lease held: nothing asked");
        assert_eq!(device_backoff(1), Duration::from_secs(10));
        assert_eq!(device_backoff(20), Duration::from_secs(600));
    }

    #[test]
    fn a_device_whose_volume_is_gone_is_detached_then_deleted_and_one_never_made_forgotten() {
        let node = Uuid::now_v7();
        let mut d = DueDevice {
            attached: Some((node, 3)),
            gone: true,
            ..due(Some("img"), Some((node, 3)))
        };
        let detach = DeviceCall::Detach { device: "img".into(), node, epoch: 3 };
        assert_eq!(d.next(), Some(detach), "detached first, whatever lease is held");
        d.attached = None;
        assert_eq!(d.next(), Some(DeviceCall::Delete { device: "img".into() }));
        d.device = None;
        assert_eq!(d.next(), Some(DeviceCall::Forget), "never made: no provider is asked");
    }
}
