// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices on this node (§2.1 of docs/06-storage.md). A volume in a block class
//! is made by its step, or mounted for its object, only with the device its assignment carries
//! for it, which its class's provider attached to this node for the workload's lease; until the
//! assignment carries one the step waits, and the status of the step says why. A device is let go
//! of once no assignment held carries it and nothing of its workload runs here, since a running
//! object holds its filesystem: a workload retired, moved, or stopped by its lease's end.

use super::{Reconciler, Tracked};
use crate::host::Host;
use crate::reconcile::HeldSet;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1::RuntimeState;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;

const STOPPED: RuntimeState = RuntimeState::RUNTIME_STATE_STOPPED;

/// The device `a` carries for the volume the helper knows as `volume`, if it carries one.
pub(super) fn device_of(a: &agent::Assignment, volume: &str) -> Option<helper::BlockDevice> {
    a.block_attachments.iter().find(|d| d.helper_id == volume).map(|d| helper::BlockDevice {
        protocol: d.protocol,
        target: d.target.clone(),
        credential: d.credential.clone(),
        guest: d.guest,
        blank: d.blank,
        ..Default::default()
    })
}

/// The names of `classes` that are block classes: those naming how their devices are attached.
/// Every other class's protocol is the unspecified one, which a wire enumeration still knows.
pub(crate) fn block_classes(classes: &[pb::StorageClassInfo]) -> BTreeSet<String> {
    const NONE: pb::BlockProtocol = pb::BlockProtocol::BLOCK_PROTOCOL_UNSPECIFIED;
    let attached =
        |c: &&pb::StorageClassInfo| c.block_protocol.as_known().is_some_and(|p| p != NONE);
    classes.iter().filter(attached).map(|c| c.name.clone()).collect()
}

/// Whether anything of `t`'s workload may hold a filesystem mounted here: an object not known to
/// be stopped, or a run.
fn holding(t: &Tracked) -> bool {
    let stopped = t.status.as_ref().is_some_and(|s| s.state.as_known() == Some(STOPPED));
    (t.runtime.is_some() && !stopped) || !t.runs.is_empty()
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Gives `request` the device `a` carries for its volume when its class is a block class;
    /// whether it may be made now, which it may not while the assignment carries none.
    pub(super) fn with_device(
        &self,
        a: &agent::Assignment,
        request: &mut helper::EnsureVolumeRequest,
    ) -> bool {
        if !self.wiring.offered.block_classes.contains(&request.storage_class) {
            return true;
        }
        match device_of(a, &request.volume) {
            Some(device) => {
                request.device = device.into();
                true
            }
            None => false,
        }
    }

    /// Lets go of every device no assignment held carries whose workload holds nothing here; one
    /// that cannot be let go of now is tried again at the next pass.
    pub(super) async fn release_devices(&mut self, held: &HeldSet) {
        if self.wiring.offered.block_classes.is_empty() {
            return;
        }
        let attached = match self.host.devices().await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "the devices attached here could not be listed");
                return;
            }
        };
        for d in attached {
            let carried = held
                .assignments
                .get(&d.workload)
                .is_some_and(|a| a.block_attachments.iter().any(|b| b.helper_id == d.volume));
            if carried || self.tracked.get(&d.workload).is_some_and(holding) {
                continue;
            }
            match self.host.release_device(&d.volume).await {
                Ok(_) => tracing::info!(volume = %d.volume, "device released"),
                Err(e) => tracing::warn!(volume = %d.volume, error = %e, "device not released yet"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::driver::v1 as driver;

    #[test]
    fn a_device_is_found_by_its_volume_and_held_while_its_object_may_hold_it() {
        let attached = |volume: &str| agent::BlockVolumeAttachment {
            helper_id: volume.into(),
            protocol: pb::BlockProtocol::BLOCK_PROTOCOL_LOOP.into(),
            target: format!("{volume}.img"),
            ..Default::default()
        };
        let a = agent::Assignment {
            block_attachments: vec![attached("w.data"), attached("w.logs")],
            ..Default::default()
        };
        assert_eq!(device_of(&a, "w.logs").map(|d| d.target), Some("w.logs.img".to_owned()));
        assert!(device_of(&a, "w.other").is_none());

        let mut t = Tracked { runtime: Some("ctr".into()), ..Tracked::default() };
        assert!(holding(&t), "an object whose state is not known yet");
        let state = |s: driver::RuntimeState| driver::RuntimeStatus {
            state: s.into(),
            ..Default::default()
        };
        t.status = Some(state(driver::RuntimeState::RUNTIME_STATE_RUNNING));
        assert!(holding(&t));
        t.status = Some(state(STOPPED));
        assert!(!holding(&t), "stopped: its filesystem is free");
        t.runtime = None;
        t.status = None;
        assert!(!holding(&Tracked::default()), "nothing here");
    }

    #[test]
    fn a_block_class_is_one_naming_how_its_devices_are_attached_and_no_other() {
        let class = |name: &str, p: pb::BlockProtocol| pb::StorageClassInfo {
            name: name.into(),
            block_protocol: p.into(),
            ..Default::default()
        };
        let classes = [
            class("local", pb::BlockProtocol::BLOCK_PROTOCOL_UNSPECIFIED),
            class("shared", pb::BlockProtocol::BLOCK_PROTOCOL_UNSPECIFIED),
            class("fast", pb::BlockProtocol::BLOCK_PROTOCOL_LOOP),
        ];
        assert_eq!(block_classes(&classes), BTreeSet::from(["fast".to_owned()]));
    }
}
