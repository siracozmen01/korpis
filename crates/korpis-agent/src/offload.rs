// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offloading console streams (§4 and question 1 of docs/14-streams.md): every segment the writer
//! seals is sent, sealed again under the workload's data key, to the repository that takes the
//! workload's streams, with write access to its stream prefix alone, and journaled as offloaded,
//! from which the control plane's index says it outlives the node. A task of its own, so the
//! reconciler's loop never waits on a store. It follows each stream's changes log from its own
//! durable place, so a segment sealed while no repository took streams, or while the control
//! plane was away, is offloaded once one does, if the node still holds it; one pruned first is
//! what the index reports held only on the node, and lost with it.

mod positions;

use crate::jobs::{JobAccesses, NoAccess};
use crate::journal::JournalHandle;
use crate::reconcile::streams::Located;
use crate::reconcile::tail::segment_record;
use korpis_backup::Repo;
use korpis_backup::keys::{KEY_LEN, Keys};
use korpis_backup::offload::put_segment;
use korpis_backup::s3::{S3Config, S3Store};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_stream::changes::{self, Change};
use korpis_stream::dir::StreamDir;
use korpis_stream::segment::Sealed;
use positions::{Position, Positions};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long before its access ends a grant is no longer used: an upload must finish in it.
const MARGIN: Duration = Duration::from_secs(300);

/// How long after a refusal it asks again: no repository takes the workload's streams, or the
/// workload is not this node's.
const AFTER_REFUSAL: Duration = Duration::from_secs(300);

/// How long after the target was not reachable it asks again.
const AFTER_FAILURE: Duration = Duration::from_secs(60);

/// What one grant lets the offloader write, where, and until when.
struct Granted {
    repo: Repo<S3Store>,
    repository: String,
    until: SystemTime,
}

/// The offloader.
pub struct Offloader {
    positions: Positions,
    located: Located,
    journal: JournalHandle,
    jobs: Arc<JobAccesses>,
    period: Duration,
    granted: HashMap<String, Granted>,
    resting: HashMap<String, Instant>,
}

impl std::fmt::Debug for Offloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offloader").field("period", &self.period).finish_non_exhaustive()
    }
}

fn failed(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The repository `g` reaches, and when its access ends.
fn repo_of(g: &agent::JobGrant) -> Result<(Repo<S3Store>, SystemTime), String> {
    let s = g.store.as_option().ok_or("a grant without a store")?;
    let key: [u8; KEY_LEN] = g.data_key.as_slice().try_into().map_err(|_| "a key of 32 bytes")?;
    let config = S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: s.key_prefix.clone(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
    };
    let store = S3Store::new(config).map_err(failed)?;
    let until = g.expires_at.as_option().map_or(UNIX_EPOCH, |t| {
        UNIX_EPOCH + Duration::from_secs(u64::try_from(t.seconds).unwrap_or(0))
    });
    Ok((Repo::new(Arc::new(store), Arc::new(Keys::derive(&key))), until))
}

impl Offloader {
    /// An offloader keeping its places at `path`, following the streams `located` names, asking
    /// `jobs` for access, journaling to `journal`, every `period`.
    pub fn new(
        path: &Path,
        located: Located,
        (journal, jobs): (JournalHandle, Arc<JobAccesses>),
        period: Duration,
    ) -> std::io::Result<Self> {
        let positions = Positions::open(path)?;
        Ok(Self {
            positions,
            located,
            journal,
            jobs,
            period,
            granted: HashMap::new(),
            resting: HashMap::new(),
        })
    }

    /// Offloads, pass after pass, as long as the agent runs.
    pub async fn run(mut self) {
        loop {
            self.pass().await;
            tokio::time::sleep(self.period).await;
        }
    }

    /// One pass over every stream followed: a stream no longer located and offloaded through is
    /// no longer followed.
    pub async fn pass(&mut self) {
        let located = self.located.borrow().clone();
        for (workload, dir) in &located {
            if let Err(e) = self.positions.follow(workload, dir) {
                tracing::error!(%workload, error = %e, "a stream could not be followed");
            }
        }
        for (workload, position) in self.positions.all() {
            match self.offload(&workload, &position).await {
                Ok(true) if !located.contains_key(&workload) => self.unfollow(&workload),
                Ok(_) => {}
                Err(e) => tracing::warn!(%workload, error = %e, "a stream was not offloaded"),
            }
        }
    }

    fn unfollow(&mut self, workload: &str) {
        if let Err(e) = self.positions.drop_stream(workload) {
            tracing::error!(%workload, error = %e, "the places could not be kept");
        }
    }

    /// Offloads what `workload`'s stream sealed since `position`; whether it is offloaded through.
    async fn offload(&mut self, workload: &str, position: &Position) -> Result<bool, String> {
        let read = changes::read_from(&position.dir, position.next);
        let read = match read {
            Ok(read) => read,
            // The stream's volume was deleted: there is nothing left to offload.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(failed(e)),
        };
        let dir = StreamDir::new(&position.dir);
        for (change, next) in read {
            let done = match &change {
                Change::Sealed(s) => self.send(workload, &dir, s).await?,
                Change::Pruned(_) => true,
            };
            if !done {
                return Ok(false);
            }
            self.positions.reached(workload, next).map_err(failed)?;
        }
        Ok(true)
    }

    /// Sends segment `s` of `workload`'s stream in `dir` and journals it as offloaded; whether
    /// it is done with, which a segment pruned before it could go also is. False while no access
    /// can be had.
    async fn send(&mut self, workload: &str, dir: &StreamDir, s: &Sealed) -> Result<bool, String> {
        let bytes = match tokio::fs::read(dir.sealed_path(s.first_offset)).await {
            Ok(bytes) => bytes,
            // Pruned before it could go: the index says it was held only here.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(failed(e)),
        };
        let Some((repo, repository)) = self.repo_for(workload) else { return Ok(false) };
        if let Err(e) = put_segment(&repo, (s.first_offset, &s.digest), bytes).await {
            self.granted.remove(workload);
            return Err(failed(e));
        }
        let offloaded = agent::SegmentEvent::SEGMENT_EVENT_OFFLOADED;
        let record = segment_record(workload, offloaded, s, &repository);
        self.journal.append(record).await.map_err(failed)?;
        Ok(true)
    }

    /// The repository `workload`'s stream is offloaded to, and its identifier, while a grant for
    /// it lasts; `None` while one is asked for, or after a refusal until it is time to ask again.
    fn repo_for(&mut self, workload: &str) -> Option<(Repo<S3Store>, String)> {
        let now = SystemTime::now();
        if let Some(g) = self.granted.get(workload).filter(|g| g.until > now + MARGIN) {
            return Some((g.repo.clone(), g.repository.clone()));
        }
        if self.resting.get(workload).is_some_and(|until| *until > Instant::now()) {
            return None;
        }
        let rest = match self.jobs.poll_offload(workload)? {
            Ok(grant) => match repo_of(&grant) {
                Ok((repo, until)) => {
                    let repository = grant.repository.clone();
                    let kept =
                        Granted { repo: repo.clone(), repository: repository.clone(), until };
                    self.granted.insert(workload.to_owned(), kept);
                    return Some((repo, repository));
                }
                Err(e) => {
                    tracing::warn!(%workload, error = %e, "an offload grant could not be used");
                    AFTER_FAILURE
                }
            },
            Err(NoAccess::Refused(_)) => AFTER_REFUSAL,
            Err(NoAccess::Later(_)) => AFTER_FAILURE,
        };
        self.resting.insert(workload.to_owned(), Instant::now() + rest);
        None
    }
}
