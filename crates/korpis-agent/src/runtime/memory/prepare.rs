// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the in-memory runtime prepares from an intent.

use super::InMemory;
use crate::runtime::{Prepared, RuntimeResult};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;

impl InMemory {
    /// Prepares as a driver does: the intent's volumes are mounted where it says, their sources
    /// left for the helper to name.
    pub(super) fn prepare_now(
        &self,
        workload: &str,
        intent: &pb::Intent,
    ) -> RuntimeResult<Prepared> {
        let s = self.call("prepare", workload)?;
        if let Some(r) = s.rejecting.clone() {
            return Ok(Prepared::Rejected(r));
        }
        let body = intent.body.as_option();
        let console = body.and_then(|b| b.console.as_known());
        let captured = s.capturing && console != Some(pb::ConsoleMode::CONSOLE_MODE_NONE);
        Ok(Prepared::Spec(Box::new(driver::PreparedSpec {
            workload: workload.to_owned(),
            intent: intent.id.clone(),
            rootfs: body.map(|b| b.recipe.digest.clone()).unwrap_or_default(),
            resources: body.and_then(|b| b.resources.as_option().cloned()).into(),
            stream: captured.then(driver::StreamCapture::default).into(),
            driver: self.name.0.to_owned(),
            mounts: body.map(mounts).unwrap_or_default(),
            ..Default::default()
        })))
    }
}

/// The mounts of the intent's volumes, as the driver kit prepares them.
fn mounts(body: &pb::IntentBody) -> Vec<driver::Mount> {
    body.volumes
        .iter()
        .map(|v| driver::Mount {
            destination: v.path.clone(),
            read_only: v.mode.as_known() == Some(pb::VolumeMode::VOLUME_MODE_RO),
            volume: v.name.clone(),
            ..Default::default()
        })
        .collect()
}
