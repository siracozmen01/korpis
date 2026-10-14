// SPDX-License-Identifier: AGPL-3.0-or-later

//! The verbs that are one request and one answer.

use super::{CREATE, DriverRuntime, QUICK, error, within};
use crate::runtime::{RuntimeError, RuntimeResult, Sender};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;

impl DriverRuntime {
    pub(super) async fn describe_call(&self) -> RuntimeResult<pb::DriverInfo> {
        let request = driver::DescribeRequest::default();
        let r = self.client.describe_with_options(request, within(QUICK)).await.map_err(error)?;
        r.into_owned()
            .info
            .into_option()
            .ok_or_else(|| RuntimeError::Failed("the driver described nothing".into()))
    }

    pub(super) async fn create_call(&self, spec: &driver::PreparedSpec) -> RuntimeResult<String> {
        let request = driver::CreateRequest { spec: spec.clone().into(), ..Default::default() };
        let r = self.client.create_with_options(request, within(CREATE)).await.map_err(error)?;
        Ok(r.into_owned().runtime_id)
    }

    pub(super) async fn start_call(&self, id: &str) -> RuntimeResult<()> {
        let request = driver::StartRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.start_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn kill_call(&self, id: &str) -> RuntimeResult<()> {
        let request = driver::KillRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.kill_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn destroy_call(&self, id: &str) -> RuntimeResult<()> {
        let request = driver::DestroyRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.destroy_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn recover_call(&self) -> RuntimeResult<Vec<driver::RuntimeObject>> {
        let request = driver::RecoverRequest::default();
        let r = self.client.recover_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(r.into_owned().objects)
    }

    pub(super) async fn inspect_call(&self, id: &str) -> RuntimeResult<driver::RuntimeStatus> {
        let request = driver::InspectRequest { runtime_id: id.to_owned(), ..Default::default() };
        let r = self.client.inspect_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(r.into_owned().status.into_option().unwrap_or_default())
    }

    pub(super) async fn stats_call(&self, id: &str) -> RuntimeResult<pb::ResourceUsage> {
        let request = driver::StatsRequest { runtime_id: id.to_owned(), ..Default::default() };
        let r = self.client.stats_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(r.into_owned().usage.into_option().unwrap_or_default())
    }

    pub(super) async fn input_call(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> RuntimeResult<()> {
        let request = driver::ConsoleInputRequest {
            runtime_id: id.to_owned(),
            by: from.by.to_owned(),
            session: from.session.to_owned(),
            data: data.to_vec(),
            ..Default::default()
        };
        self.client.console_input_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn resize_call(
        &self,
        id: &str,
        from: Sender<'_>,
        (cols, rows): (u16, u16),
    ) -> RuntimeResult<()> {
        let request = driver::ResizeConsoleRequest {
            runtime_id: id.to_owned(),
            by: from.by.to_owned(),
            session: from.session.to_owned(),
            cols: u32::from(cols),
            rows: u32::from(rows),
            ..Default::default()
        };
        self.client.resize_console_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }

    pub(super) async fn signal_call(&self, id: &str, signal: &str) -> RuntimeResult<()> {
        let request = driver::SignalRequest {
            runtime_id: id.to_owned(),
            signal: signal.to_owned(),
            ..Default::default()
        };
        self.client.signal_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(())
    }
}
