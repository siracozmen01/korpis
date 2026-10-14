// SPDX-License-Identifier: AGPL-3.0-or-later

//! An interactive console's input and resizing (§6 of docs/14-streams.md). Input goes to the
//! container's stream writer, which records it with who sent it and then feeds the workload's
//! stdin; a resize is recorded the same way and applied to the task's terminal by containerd.

use super::Containerd;
use super::stream::{frames, resize_frame, takes_input};
use crate::error::{DriverError, DriverResult};
use korpis_containerd::containerd::services::containers::v1 as containers;
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use std::collections::BTreeMap;

/// Who sent an input, and on which session.
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    /// The session token's subject.
    pub by: &'a str,
    /// The data-path session.
    pub session: &'a str,
}

impl Containerd {
    /// The streams, and container `id`'s labels, if its console takes input.
    async fn interactive(&self, id: &str) -> DriverResult<&super::stream::Streams> {
        let streams = self
            .streams
            .as_ref()
            .ok_or_else(|| DriverError::Rejected("this driver writes no consoles".into()))?;
        let request = containers::GetContainerRequest { id: id.to_owned(), ..Default::default() };
        let container = match self.containers_client().get(request).await {
            Ok(c) => c.into_owned().container,
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => {
                return Err(DriverError::NotFound(id.to_owned()));
            }
            Err(e) => return Err(e.into()),
        };
        let labels: BTreeMap<String, String> = container
            .as_option()
            .map(|c| c.labels.clone())
            .unwrap_or_default()
            .into_iter()
            .collect();
        if !takes_input(&labels) {
            return Err(DriverError::Rejected("this console takes no input".into()));
        }
        Ok(streams)
    }

    /// Sends `data` from `from` to container `id`'s console.
    pub async fn console_input(&self, id: &str, from: Sender<'_>, data: &[u8]) -> DriverResult<()> {
        let streams = self.interactive(id).await?;
        for frame in frames(from.by, from.session, data)? {
            streams.send(id, &frame)?;
        }
        Ok(())
    }

    /// Resizes container `id`'s terminal to `cols` by `rows`, recording that `from` did.
    pub async fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        (cols, rows): (u16, u16),
    ) -> DriverResult<()> {
        let streams = self.interactive(id).await?;
        streams.send(id, &resize_frame(from.by, from.session, cols, rows))?;
        let request = tasks::ResizePtyRequest {
            container_id: id.to_owned(),
            width: u32::from(cols),
            height: u32::from(rows),
            ..Default::default()
        };
        self.tasks().resize_pty(request).await?;
        Ok(())
    }
}
