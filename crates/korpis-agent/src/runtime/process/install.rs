// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installs a driver runs in a service guest (§4.4 of docs/04-runtimes.md): started, asked after,
//! and forgotten, as the helper's jobs are.

use super::{CREATE, DriverRuntime, QUICK, error, within};
use crate::runtime::RuntimeResult;
use korpis_proto::korpis::driver::v1 as driver;

impl DriverRuntime {
    /// Given as long as a create, since the driver builds the install's content drive and boots
    /// its guest before it answers.
    pub(super) async fn start_install_call(
        &self,
        asked: &driver::StartInstallRequest,
    ) -> RuntimeResult<()> {
        let options = within(CREATE);
        self.client.start_install_with_options(asked.clone(), options).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn install_status_call(
        &self,
        job: &str,
    ) -> RuntimeResult<driver::InstallStatusResponse> {
        let request = driver::InstallStatusRequest { job: job.to_owned(), ..Default::default() };
        let options = within(QUICK);
        let r = self.client.install_status_with_options(request, options).await.map_err(error)?;
        Ok(r.into_owned())
    }

    pub(super) async fn forget_install_call(&self, job: &str) -> RuntimeResult<()> {
        let request = driver::ForgetInstallRequest { job: job.to_owned(), ..Default::default() };
        let options = within(CREATE);
        self.client.forget_install_with_options(request, options).await.map_err(error)?;
        Ok(())
    }
}
