// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes through the privileged helper, each one reported (§7 of docs/06-storage.md): the node
//! journals every volume it ensures or deletes, and the control plane keeps the volume object
//! from those records. The record follows the helper's answer, so a volume the journal names was
//! there; one ensured whose record did not reach the journal fails the step or the build that
//! asked for it, and the retry, ensuring it again, reports it.

use super::Reconciler;
use super::act::Failure;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;

/// Why a volume exists, as its report says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    /// The workload declared it.
    Declared,
    /// It holds the workload's console stream.
    Stream,
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Ensures the volume `request` names, known to its workload as `name`, and reports it.
    pub(super) async fn ensure_volume(
        &self,
        name: &str,
        why: Why,
        request: &helper::EnsureVolumeRequest,
    ) -> Result<helper::EnsureVolumeResponse, Failure> {
        let made = self.host.ensure_volume(request).await?;
        let storage_class = if made.storage_class.is_empty() {
            request.storage_class.clone()
        } else {
            made.storage_class.clone()
        };
        self.report_volume(agent::VolumeEvent {
            workload: request.workload.clone(),
            name: name.to_owned(),
            helper_id: request.volume.clone(),
            storage_class,
            size_bytes: request.size_bytes,
            quota_enforced: made.quota_enforced,
            event: agent::VolumeEventKind::VOLUME_EVENT_KIND_ENSURED.into(),
            stream: why == Why::Stream,
            ..Default::default()
        })
        .await?;
        Ok(made)
    }

    /// Deletes the volume the helper knows as `helper_id`, made for `workload` as `name`, and
    /// reports it.
    pub(super) async fn delete_volume(
        &self,
        workload: &str,
        name: &str,
        helper_id: &str,
    ) -> Result<(), Failure> {
        self.host.delete_volume(helper_id).await?;
        self.report_volume(agent::VolumeEvent {
            workload: workload.to_owned(),
            name: name.to_owned(),
            helper_id: helper_id.to_owned(),
            event: agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into(),
            ..Default::default()
        })
        .await
    }

    async fn report_volume(&self, e: agent::VolumeEvent) -> Result<(), Failure> {
        let volume = e.helper_id.clone();
        self.wiring
            .journal
            .append(Body::Volume(Box::new(e)))
            .await
            .map(|_| ())
            .map_err(|e| Failure(format!("volume {volume} could not be reported: {e}")))
    }
}
