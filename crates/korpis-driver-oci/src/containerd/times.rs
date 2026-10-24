// SPDX-License-Identifier: AGPL-3.0-or-later

//! When a container's task started and ended, as `Inspect` says it (§4.2 of
//! docs/04-runtimes.md): what a run's timeout is counted from, which an agent adopting the run
//! after a restart can only learn from the driver. containerd keeps when a task exited but not
//! when it started, so the driver records the start in the container's label `korpis.started_at`
//! as its task is about to start, as seconds and nanoseconds since the epoch, and reads both back.
//! The label is written first, so it never names an earlier start than the task's; a start that
//! then fails leaves a label no running task is inspected with, and the next one rewrites it.

use super::Containerd;
use crate::error::DriverResult;
use buffa_types::google::protobuf::{FieldMask, Timestamp};
use korpis_containerd::containerd::services::containers::v1 as containers;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// The label a container's last start is kept in.
pub(crate) const LABEL_STARTED: &str = "korpis.started_at";

/// `at` as the label keeps it.
fn label_of(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:09}", since.as_secs(), since.subsec_nanos())
}

/// The instant the label `value` keeps, if it is one.
pub(crate) fn started_at(value: &str) -> Option<Timestamp> {
    let (seconds, nanos) = value.split_once('.')?;
    let nanos: i32 = nanos.parse().ok().filter(|n| (0..1_000_000_000).contains(n))?;
    Some(Timestamp { seconds: seconds.parse().ok()?, nanos, ..Default::default() })
}

impl Containerd {
    /// The labels of the container `id`, or nothing if the driver holds no such container.
    pub(crate) async fn labels_of(
        &self,
        id: &str,
    ) -> DriverResult<Option<BTreeMap<String, String>>> {
        let request = containers::GetContainerRequest { id: id.to_owned(), ..Default::default() };
        match self.containers_client().get(request).await {
            Ok(r) => {
                let container = r.into_owned().container.into_option().unwrap_or_default();
                Ok(Some(container.labels.into_iter().collect()))
            }
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Records that the task on the container `id` starts now.
    pub(crate) async fn mark_started(&self, id: &str) -> DriverResult<()> {
        let container = containers::Container {
            id: id.to_owned(),
            labels: [(LABEL_STARTED.to_owned(), label_of(SystemTime::now()))].into_iter().collect(),
            ..Default::default()
        };
        let mask =
            FieldMask { paths: vec![format!("labels.{LABEL_STARTED}")], ..Default::default() };
        let request = containers::UpdateContainerRequest {
            container: container.into(),
            update_mask: mask.into(),
            ..Default::default()
        };
        self.containers_client().update(request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_start_reads_back_as_the_instant_it_was() {
        let at = UNIX_EPOCH + Duration::new(1_792_000_000, 7);
        let read = started_at(&label_of(at)).unwrap();
        assert_eq!((read.seconds, read.nanos), (1_792_000_000, 7));
        assert_eq!(started_at("12"), None);
        assert_eq!(started_at("12.x"), None);
        assert_eq!(started_at("12.1000000000"), None);
    }
}
