// SPDX-License-Identifier: AGPL-3.0-or-later

//! Task events from containerd's events service, as the driver protocol's `Events`: for every
//! task of the driver's containers that starts, exits, is deleted, pauses, resumes, or runs out
//! of memory, the status it leaves the container in. The status is read back with `inspect`
//! rather than built from the event, so an event can only make the agent look sooner, never
//! tell it something `inspect` would not; an event lost in a reconnection costs that, and the
//! reconciler's period still reads every container.

use super::{Containerd, NAMESPACE};
use crate::error::{DriverError, DriverResult};
use buffa::Message as _;
use korpis_containerd::containerd::events as task;
use korpis_containerd::containerd::services::events::v1 as events;
use korpis_containerd::containerd::types::Envelope;
use korpis_proto::korpis::driver::v1 as driver;
use std::time::Duration;
use tokio::sync::broadcast;

/// How long to wait before subscribing again after the stream ends.
const RESUBSCRIBE: Duration = Duration::from_secs(1);

/// The container an envelope is about, and what the event adds to its status.
fn subject(envelope: &Envelope) -> Option<(String, &'static str)> {
    let any = envelope.event.as_option()?;
    let bytes: &[u8] = &any.value;
    let (id, reason) = match envelope.topic.as_str() {
        "/tasks/start" => (task::TaskStart::decode_from_slice(bytes).ok()?.container_id, ""),
        "/tasks/exit" => (task::TaskExit::decode_from_slice(bytes).ok()?.container_id, ""),
        "/tasks/delete" => (task::TaskDelete::decode_from_slice(bytes).ok()?.container_id, ""),
        "/tasks/paused" => (task::TaskPaused::decode_from_slice(bytes).ok()?.container_id, ""),
        "/tasks/resumed" => (task::TaskResumed::decode_from_slice(bytes).ok()?.container_id, ""),
        "/tasks/oom" => {
            (task::TaskOOM::decode_from_slice(bytes).ok()?.container_id, "out of memory")
        }
        _ => return None,
    };
    id.starts_with("korpis-").then_some((id, reason))
}

impl Containerd {
    /// Sends every task event in the namespace on `out` for as long as it runs, subscribing
    /// again whenever containerd ends the stream; the driver stops it by dropping it.
    pub async fn watch(&self, out: broadcast::Sender<driver::EventsResponse>) {
        loop {
            match self.subscribe(&out).await {
                Ok(()) => tracing::debug!("containerd ended the event stream"),
                Err(e) => tracing::warn!(error = %e, "containerd's event stream failed"),
            }
            tokio::time::sleep(RESUBSCRIBE).await;
        }
    }

    /// One subscription, read to its end.
    async fn subscribe(&self, out: &broadcast::Sender<driver::EventsResponse>) -> DriverResult<()> {
        let filter = format!("namespace=={NAMESPACE},topic~=\"^/tasks/\"");
        let request = events::SubscribeRequest { filters: vec![filter], ..Default::default() };
        let client = events::EventsClient::new(self.conn.clone(), self.config.clone());
        let mut stream = client.subscribe(request).await?;
        while let Some(m) = stream.message::<Envelope>().await? {
            let envelope = m.to_owned_message();
            let Some((id, reason)) = subject(&envelope) else { continue };
            let mut status = match self.inspect(&id).await {
                Ok(s) => s,
                Err(DriverError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            reason.clone_into(&mut status.reason);
            let event = driver::EventsResponse {
                runtime_id: id,
                status: status.into(),
                at: envelope.timestamp.clone(),
                ..Default::default()
            };
            // Nobody listening is not an error: the agent reads `inspect` anyway.
            let _ = out.send(event);
        }
        Ok(())
    }
}
