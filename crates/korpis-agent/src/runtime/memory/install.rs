// SPDX-License-Identifier: AGPL-3.0-or-later

//! The in-memory runtime's installs, as a driver whose volumes are block devices runs them in a
//! service guest (§4.4 of docs/04-runtimes.md): where a test has said it takes block devices,
//! which its declaration then says, each install it is asked for has finished at once, every
//! action counted as run, once its spec names the workload's ids and its volumes' places, as a
//! driver's does. One it is not asked for is unknown, and one forgotten is again.

use super::{InMemory, info};
use crate::runtime::{RuntimeError, RuntimeResult};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::install::v1 as install;
use korpis_proto::korpis::v1 as pb;

impl InMemory {
    /// Declares its volumes block devices, and runs their installs, or stops.
    pub fn block_devices(&self, on: bool) {
        if let Ok(mut s) = self.lock() {
            s.block_devices = on;
        }
    }

    pub(super) fn describe_now(&self) -> RuntimeResult<pb::DriverInfo> {
        let mut described = info(self.name.0);
        if self.lock()?.block_devices {
            let block = pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE;
            let caps = pb::Capabilities { volume_semantics: block.into(), ..Default::default() };
            described.capabilities = caps.into();
        }
        Ok(described)
    }

    pub(super) fn start_install_now(
        &self,
        asked: &driver::StartInstallRequest,
    ) -> RuntimeResult<()> {
        let mut s = self.call("start_install", &asked.job)?;
        if !s.block_devices {
            return Err(RuntimeError::Failed("this runtime takes no block devices".into()));
        }
        // As a driver refuses one: its guest runs as the workload's ids, and its volumes are
        // placed.
        let spec = asked.spec.as_option().cloned().unwrap_or_default();
        if spec.uid_count == 0 || spec.mounts.iter().any(|m| m.source.is_empty()) {
            return Err(RuntimeError::Failed("an install names its ids and its volumes".into()));
        }
        let actions = u32::try_from(asked.actions.len()).unwrap_or(u32::MAX);
        s.installs.entry(asked.job.clone()).or_insert(actions);
        Ok(())
    }

    pub(super) fn install_status_now(
        &self,
        job: &str,
    ) -> RuntimeResult<driver::InstallStatusResponse> {
        let s = self.call("install_status", job)?;
        let Some(&done) = s.installs.get(job) else {
            let unknown = driver::InstallState::INSTALL_STATE_UNKNOWN;
            return Ok(driver::InstallStatusResponse {
                state: unknown.into(),
                ..Default::default()
            });
        };
        let result = install::InstallResult { ok: true, done, ..Default::default() };
        Ok(driver::InstallStatusResponse {
            state: driver::InstallState::INSTALL_STATE_FINISHED.into(),
            result: result.into(),
            progress: done,
            ..Default::default()
        })
    }

    pub(super) fn forget_install_now(&self, job: &str) -> RuntimeResult<()> {
        self.call("forget_install", job)?.installs.remove(job);
        Ok(())
    }
}
