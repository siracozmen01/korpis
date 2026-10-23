// SPDX-License-Identifier: AGPL-3.0-or-later

//! The verbs that are one request and one answer, and `Exec`, whose answer is streamed and read
//! to its end.

use super::{CREATE, DriverRuntime, QUICK, error, within};
use crate::runtime::{Executed, RuntimeError, RuntimeResult, Sender};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::driver::v1::__buffa::oneof::exec_response::Event;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

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

    pub(super) async fn stats_call(&self, id: &str) -> RuntimeResult<driver::StatsResponse> {
        let request = driver::StatsRequest { runtime_id: id.to_owned(), ..Default::default() };
        let r = self.client.stats_with_options(request, within(QUICK)).await.map_err(error)?;
        Ok(r.into_owned())
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

    /// Runs `command` in `id` and reads what it wrote, up to `output_limit`, and how it ended;
    /// the call is given the command's timeout and the quick deadline beyond it.
    pub(super) async fn exec_call(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
        output_limit: u32,
    ) -> RuntimeResult<Executed> {
        let request = driver::ExecRequest {
            runtime_id: id.to_owned(),
            command: command.to_vec(),
            timeout: korpis_wire::duration(timeout).into(),
            output_limit,
            ..Default::default()
        };
        let options = within(timeout + QUICK);
        let mut stream = self.client.exec_with_options(request, options).await.map_err(error)?;
        let mut executed = Executed::default();
        let limit = usize::try_from(output_limit).unwrap_or(0);
        while let Some(m) = stream.message::<driver::ExecResponse>().await.map_err(error)? {
            match m.to_owned_message().event {
                Some(Event::Output(data)) => {
                    let room = limit.saturating_sub(executed.output.len());
                    executed.output.extend(data.into_iter().take(room));
                }
                Some(Event::Exit(exit)) => {
                    executed.status = exit.status;
                    executed.timed_out = exit.timed_out;
                    return Ok(executed);
                }
                None => {}
            }
        }
        Err(RuntimeError::Failed("the driver ended a command's stream without its exit".into()))
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
