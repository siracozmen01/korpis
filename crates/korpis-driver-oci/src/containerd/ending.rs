// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stopping a container, and ending its task once it stopped. containerd's runc shim holds the
//! write ends of a task's output pipes, and signals the binary log driver that writes the
//! workload's stream to exit, only as the task is deleted (containerd's
//! `cmd/containerd-shim-runc-v2/process/io.go`, `binaryIO.Close`); until then the stream's writer
//! never sees its input end, and its last segment stays open. So a stop or a kill the agent asks
//! for deletes the task once it stopped, and the writer seals: the stream of a stopped workload
//! is complete, which a migration's `quiesce` waits for, and the shim is released. containerd
//! forgets the exit with the task, so the driver first records it in the container's label
//! `korpis.ended`, as the exit status, the instant it exited, and whether the kernel had killed a
//! process of it for want of memory, and `Inspect` reads a container with no task through it.
//! A task that exits on its own is kept until the agent stops it or starts it again, so a crash
//! is seen as one.

use super::Containerd;
use super::lifecycle::KILL_WAIT;
use super::times::label_of_timestamp;
use crate::error::{DriverError, DriverResult};
use buffa_types::google::protobuf::{FieldMask, Timestamp};
use korpis_containerd::containerd::services::containers::v1 as containers;
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use korpis_containerd::containerd::v1::types::Status;
use std::time::{Duration, Instant};

/// The label a container's last ended task is kept in.
pub(crate) const LABEL_ENDED: &str = "korpis.ended";

/// How a task ended, as the label keeps it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ended {
    /// The exit status of its first process.
    pub code: u32,
    /// When it exited.
    pub at: Option<Timestamp>,
    /// Whether the kernel killed a process of it for want of memory.
    pub oom: bool,
}

impl Ended {
    /// As the label keeps it: `<code> <exited> <oom>`, the instant as `times` writes one.
    fn label(&self) -> String {
        let at = self.at.as_ref().map_or_else(|| "-".to_owned(), label_of_timestamp);
        format!("{} {at} {}", self.code, u8::from(self.oom))
    }

    /// What the label `value` keeps, if it is one.
    pub(crate) fn read(value: &str) -> Option<Self> {
        let mut parts = value.split(' ');
        let code = parts.next()?.parse().ok()?;
        let at = match parts.next()? {
            "-" => None,
            at => Some(super::times::started_at(at)?),
        };
        let oom = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        parts.next().is_none().then_some(Self { code, at, oom })
    }
}

impl Containerd {
    /// Sends `signal` to every process of `id`.
    pub async fn signal(&self, id: &str, signal: u32) -> DriverResult<()> {
        self.held(id).await?;
        self.signal_task(id, signal).await
    }

    /// Sends the task on `id`, if there is one, `signal`.
    async fn signal_task(&self, id: &str, signal: u32) -> DriverResult<()> {
        let request = tasks::KillRequest {
            container_id: id.to_owned(),
            signal,
            all: true,
            ..Default::default()
        };
        match self.tasks().kill(request).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Waits up to `wait` for the task on `id` to stop; whether it did.
    async fn stopped_within(&self, id: &str, wait: Duration) -> DriverResult<bool> {
        let until = Instant::now() + wait;
        loop {
            match self.task(id).await? {
                None | Some((Status::STOPPED, _, _)) => return Ok(true),
                Some(_) if Instant::now() >= until => return Ok(false),
                Some(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Stops `id` with `signal`, killing it if it outlives `grace`, and ends its task.
    pub async fn stop(&self, id: &str, signal: u32, grace: Duration) -> DriverResult<()> {
        self.held(id).await?;
        if matches!(self.task(id).await?, Some((Status::RUNNING | Status::PAUSED, _, _))) {
            self.signal_task(id, signal).await?;
            if !self.stopped_within(id, grace).await? {
                self.killed(id).await?;
            }
        }
        self.end(id).await
    }

    /// Kills `id` at once, and ends its task.
    pub async fn kill(&self, id: &str) -> DriverResult<()> {
        self.held(id).await?;
        self.killed(id).await?;
        self.end(id).await
    }

    /// Kills the task on `id` and waits for it to be reaped.
    pub(crate) async fn killed(&self, id: &str) -> DriverResult<()> {
        self.signal_task(id, 9).await?;
        if self.stopped_within(id, KILL_WAIT).await? {
            Ok(())
        } else {
            Err(DriverError::Failed(format!("{id} outlived SIGKILL for {KILL_WAIT:?}")))
        }
    }

    /// Records how the stopped task on `id` ended, then deletes it; nothing without a task.
    async fn end(&self, id: &str) -> DriverResult<()> {
        let Some(process) = self.process(id).await? else { return Ok(()) };
        let ended = Ended {
            code: process.exit_status,
            at: process.exited_at.as_option().cloned(),
            oom: self.oom_killed(id),
        };
        let container = containers::Container {
            id: id.to_owned(),
            labels: [(LABEL_ENDED.to_owned(), ended.label())].into_iter().collect(),
            ..Default::default()
        };
        let mask = FieldMask { paths: vec![format!("labels.{LABEL_ENDED}")], ..Default::default() };
        let request = containers::UpdateContainerRequest {
            container: container.into(),
            update_mask: mask.into(),
            ..Default::default()
        };
        self.containers_client().update(request).await?;
        self.delete_task(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ending_reads_back_as_it_was_kept() {
        let at = Timestamp { seconds: 1_792_000_000, nanos: 7, ..Default::default() };
        let ended = Ended { code: 137, at: Some(at), oom: true };
        assert_eq!(Ended::read(&ended.label()), Some(ended));
        let unknown = Ended { code: 0, at: None, oom: false };
        assert_eq!(Ended::read(&unknown.label()), Some(unknown));
        for bad in ["", "0", "0 -", "x - 0", "0 - 2", "0 - 0 extra", "0 12.x 0"] {
            assert_eq!(Ended::read(bad), None, "{bad:?}");
        }
    }
}
