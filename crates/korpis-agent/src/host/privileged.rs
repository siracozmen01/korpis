// SPDX-License-Identifier: AGPL-3.0-or-later

//! The privileged helper, reached over its socket (`korpis.helper.v1`): one connection per
//! call, so the helper, which serves one connection at a time and exits when idle, is never held
//! open by the supervisor. Each message is framed as the protocol says, a 4-byte big-endian
//! length and then one envelope, and the whole call has a deadline.

use super::HostError;
use buffa::Message as _;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// The largest message either side sends.
const MAX: usize = 1 << 20;
/// How long one call may take, from connecting to the answer.
const DEADLINE: Duration = Duration::from_secs(60);

/// The helper at a socket.
#[derive(Debug, Clone)]
pub struct HelperClient {
    socket: PathBuf,
}

fn failed(why: impl std::fmt::Display) -> HostError {
    HostError(format!("the helper: {why}"))
}

async fn exchange(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut s = UnixStream::connect(socket).await?;
    let len = u32::try_from(request.len()).map_err(std::io::Error::other)?;
    s.write_all(&len.to_be_bytes()).await?;
    s.write_all(request).await?;
    s.flush().await?;
    let len = usize::try_from(s.read_u32().await?).unwrap_or(usize::MAX);
    if len > MAX {
        return Err(std::io::Error::other("an answer above the limit"));
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    Ok(body)
}

impl HelperClient {
    /// The helper serving on `socket`.
    #[must_use]
    pub fn new(socket: &Path) -> Self {
        Self { socket: socket.to_owned() }
    }

    /// Sends `verb`; the helper's answer, or why there is none.
    pub async fn call(&self, verb: Verb) -> Result<Answer, HostError> {
        let request =
            helper::HelperRequest { verb: Some(verb), ..Default::default() }.encode_to_vec();
        let body = tokio::time::timeout(DEADLINE, exchange(&self.socket, &request))
            .await
            .map_err(|_| failed("no answer in time"))?
            .map_err(failed)?;
        let response = helper::HelperResponse::decode_from_slice(&body).map_err(failed)?;
        match response.result {
            Some(Answer::Error(e)) => Err(failed(format!("{:?}: {}", e.code, e.message))),
            Some(answer) => Ok(answer),
            None => Err(failed("an empty answer")),
        }
    }

    /// Creates a volume, or finds it.
    pub async fn ensure_volume(
        &self,
        r: &helper::EnsureVolumeRequest,
    ) -> Result<helper::EnsureVolumeResponse, HostError> {
        match self.call(Verb::EnsureVolume(Box::new(r.clone()))).await? {
            Answer::EnsureVolume(a) => Ok(*a),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Removes a volume; whether there was one.
    pub async fn delete_volume(&self, volume: &str) -> Result<bool, HostError> {
        let r = helper::DeleteVolumeRequest { volume: volume.to_owned(), ..Default::default() };
        match self.call(Verb::DeleteVolume(Box::new(r))).await? {
            Answer::DeleteVolume(a) => Ok(a.deleted),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Takes, rolls back to, or removes a snapshot of a volume.
    pub async fn snapshot(
        &self,
        op: super::SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> Result<(), HostError> {
        let (volume, snapshot) = (volume.to_owned(), snapshot.to_owned());
        let verb = match op {
            super::SnapshotOp::Take => {
                Verb::SnapshotVolume(Box::new(helper::SnapshotVolumeRequest {
                    volume,
                    snapshot,
                    ..Default::default()
                }))
            }
            super::SnapshotOp::Rollback => {
                Verb::RollbackVolume(Box::new(helper::RollbackVolumeRequest {
                    volume,
                    snapshot,
                    ..Default::default()
                }))
            }
            super::SnapshotOp::Delete => {
                Verb::DeleteSnapshot(Box::new(helper::DeleteSnapshotRequest {
                    volume,
                    snapshot,
                    ..Default::default()
                }))
            }
        };
        match self.call(verb).await? {
            Answer::SnapshotVolume(_) | Answer::RollbackVolume(_) | Answer::DeleteSnapshot(_) => {
                Ok(())
            }
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// What the filesystem accounts for a volume.
    pub async fn volume_usage(
        &self,
        volume: &str,
    ) -> Result<helper::VolumeUsageResponse, HostError> {
        let r = helper::VolumeUsageRequest { volume: volume.to_owned(), ..Default::default() };
        match self.call(Verb::VolumeUsage(Box::new(r))).await? {
            Answer::VolumeUsage(a) => Ok(*a),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Records the id range a workload holds.
    pub async fn ensure_id_range(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> Result<(), HostError> {
        let r = helper::EnsureIdRangeRequest {
            workload: workload.to_owned(),
            uid_base: base,
            uid_count: count,
            ..Default::default()
        };
        match self.call(Verb::EnsureIdRange(Box::new(r))).await? {
            Answer::EnsureIdRange(_) => Ok(()),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// The storage classes the node offers.
    pub async fn describe_storage(
        &self,
    ) -> Result<Vec<korpis_proto::korpis::v1::StorageClassInfo>, HostError> {
        match self.call(Verb::DescribeStorage(Box::default())).await? {
            Answer::DescribeStorage(a) => Ok(a.classes),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }
}
