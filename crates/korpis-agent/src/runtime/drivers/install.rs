// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installs, each sent to the driver its workload's spec names: an install runs before the
//! workload has an object, so no object says which driver owns it; and a block device's backup
//! staged for reading, which is no object's either, read through the driver named.

use super::Drivers;
use crate::runtime::{Runtime, RuntimeError, RuntimeResult};
use korpis_proto::korpis::driver::v1 as driver;

impl<R: Runtime> Drivers<R> {
    /// The driver named `name`, or why there is none.
    fn installer(&self, name: &str) -> RuntimeResult<&R> {
        let named = self.named(name).map(|(_, d)| d);
        named.ok_or_else(|| RuntimeError::Failed(format!("this node has no {name:?} driver")))
    }

    pub(super) async fn start_install_on(
        &self,
        asked: &driver::StartInstallRequest,
    ) -> RuntimeResult<()> {
        let name = asked.spec.as_option().map_or("", |s| s.driver.as_str());
        self.installer(name)?.start_install(asked).await
    }

    pub(super) async fn install_status_on(
        &self,
        name: &str,
        job: &str,
    ) -> RuntimeResult<driver::InstallStatusResponse> {
        self.installer(name)?.install_status(name, job).await
    }

    pub(super) async fn forget_install_on(&self, name: &str, job: &str) -> RuntimeResult<()> {
        self.installer(name)?.forget_install(name, job).await
    }

    pub(super) async fn image_files_on(
        &self,
        name: &str,
        image: &driver::StagedImage,
    ) -> RuntimeResult<tokio::io::DuplexStream> {
        self.installer(name)?.image_files(name, image).await
    }
}
