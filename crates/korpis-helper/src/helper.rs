// SPDX-License-Identifier: AGPL-3.0-or-later

//! One request in, one answer out: the envelope decoded, the verb dispatched to the class or
//! the ledger it concerns, and whatever happened encoded as the answer. Nothing here panics on
//! what a peer sends; a request that does not decode, or names a verb this helper does not
//! know, is answered as such.

use crate::error::{HelperError, HelperResult};
use crate::ids::Ledger;
use crate::job::{JobConfig, Jobs, Placed, make_into};
use crate::storage::Class;
use buffa::Message as _;
use korpis_core::workload::assignment::IdRange;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

/// The helper's state: its classes, the first of them the default, and its ledger.
#[derive(Debug)]
pub struct Helper {
    classes: Vec<Class>,
    ledger: Ledger,
    jobs: Option<Jobs>,
}

fn unsupported() -> helper::HelperError {
    helper::HelperError {
        code: helper::HelperErrorCode::HELPER_ERROR_CODE_UNSUPPORTED.into(),
        message: "a verb this helper does not know".into(),
        ..Default::default()
    }
}

impl Helper {
    /// A helper offering `classes`, the first the default, and keeping `ledger`.
    #[must_use]
    pub fn new(classes: Vec<Class>, ledger: Ledger) -> Self {
        Self { classes, ledger, jobs: None }
    }

    /// The same helper, running backup and restore jobs as `config` says.
    #[must_use]
    pub fn with_jobs(mut self, config: JobConfig) -> Self {
        self.jobs = Some(Jobs::new(config));
        self
    }

    /// Whether a job this helper started still runs, so it must not exit while idle.
    pub fn busy(&mut self) -> bool {
        self.jobs.as_mut().is_some_and(Jobs::busy)
    }

    fn jobs(&mut self) -> HelperResult<&mut Jobs> {
        self.jobs.as_mut().ok_or_else(|| HelperError::Refused("this helper runs no jobs".into()))
    }

    /// What job `r` works in, from the identifiers it names.
    fn place(&mut self, r: &helper::StartJobRequest) -> HelperResult<Placed> {
        use korpis_proto::korpis::backup::v1 as pb;
        use korpis_proto::korpis::backup::v1::__buffa::oneof::job_spec::Work as W;
        use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work as R;
        match &r.work {
            Some(R::Backup(b)) => {
                let directory = self.holding(&b.volume)?.backup_source(&b.volume, &b.snapshot)?;
                let work = pb::BackupWork {
                    backup: b.backup.clone(),
                    parent: b.parent.clone(),
                    consistent: !b.snapshot.is_empty(),
                    content: b.content.clone(),
                    ..Default::default()
                };
                Ok(Placed { directory, work: W::Backup(Box::new(work)) })
            }
            Some(R::Restore(w)) => {
                let volume = self.holding(&w.volume)?.path_of(&w.volume);
                let directory =
                    if w.into.is_empty() { volume } else { make_into(&volume, &w.into)? };
                let work = pb::RestoreWork {
                    backup: w.backup.clone(),
                    path: w.path.clone(),
                    ..Default::default()
                };
                Ok(Placed { directory, work: W::Restore(Box::new(work)) })
            }
            None => Err(HelperError::Refused("a job names its work".into())),
        }
    }

    /// The answer to a job verb.
    fn job(&mut self, verb: Verb) -> HelperResult<Answer> {
        self.jobs()?;
        Ok(match verb {
            Verb::StartJob(r) => {
                let placed = self.place(&r)?;
                let started = self.jobs()?.start(&r, placed)?;
                Answer::StartJob(Box::new(helper::StartJobResponse {
                    started,
                    ..Default::default()
                }))
            }
            Verb::JobStatus(r) => Answer::JobStatus(Box::new(self.jobs()?.status(&r.job)?)),
            Verb::CancelJob(r) => {
                let cancelled = self.jobs()?.cancel(&r.job)?;
                Answer::CancelJob(Box::new(helper::CancelJobResponse {
                    cancelled,
                    ..Default::default()
                }))
            }
            Verb::ForgetJob(r) => {
                self.jobs()?.forget(&r.job)?;
                Answer::ForgetJob(Box::default())
            }
            _ => return Err(HelperError::Refused("not a job verb".into())),
        })
    }

    /// The class `name` names; the default when it names none.
    fn class(&mut self, name: &str) -> HelperResult<&mut Class> {
        let found = if name.is_empty() {
            self.classes.first_mut()
        } else {
            self.classes.iter_mut().find(|c| c.name() == name)
        };
        found.ok_or_else(|| HelperError::NotFound(format!("storage class {name:?}")))
    }

    /// The class that holds `volume`, if any does.
    fn holding(&mut self, volume: &str) -> HelperResult<&mut Class> {
        crate::error::identifier("volume", volume)?;
        self.classes
            .iter_mut()
            .find(|c| c.path_of(volume).symlink_metadata().is_ok())
            .ok_or_else(|| HelperError::NotFound(format!("volume {volume}")))
    }

    fn dispatch(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::EnsureVolume(r) => {
                Answer::EnsureVolume(Box::new(self.class(&r.storage_class)?.ensure(&r)?))
            }
            Verb::DeleteVolume(r) => {
                let deleted = match self.holding(&r.volume) {
                    Ok(c) => c.delete(&r.volume)?,
                    Err(HelperError::NotFound(_)) => false,
                    Err(e) => return Err(e),
                };
                Answer::DeleteVolume(Box::new(helper::DeleteVolumeResponse {
                    deleted,
                    ..Default::default()
                }))
            }
            Verb::VolumeUsage(r) => {
                Answer::VolumeUsage(Box::new(self.holding(&r.volume)?.usage(&r.volume)?))
            }
            Verb::EnsureIdRange(r) => {
                self.ledger
                    .ensure(&r.workload, IdRange { base: r.uid_base, count: r.uid_count })?;
                Answer::EnsureIdRange(Box::default())
            }
            Verb::SnapshotVolume(r) => {
                let created = self.holding(&r.volume)?.snapshot(&r.volume, &r.snapshot)?;
                Answer::SnapshotVolume(Box::new(helper::SnapshotVolumeResponse {
                    created,
                    ..Default::default()
                }))
            }
            Verb::RollbackVolume(r) => {
                self.holding(&r.volume)?.rollback(&r.volume, &r.snapshot)?;
                Answer::RollbackVolume(Box::default())
            }
            Verb::DeleteSnapshot(r) => {
                let deleted = match self.holding(&r.volume) {
                    Ok(c) => c.delete_snapshot(&r.volume, &r.snapshot)?,
                    Err(HelperError::NotFound(_)) => false,
                    Err(e) => return Err(e),
                };
                Answer::DeleteSnapshot(Box::new(helper::DeleteSnapshotResponse {
                    deleted,
                    ..Default::default()
                }))
            }
            v @ (Verb::StartJob(_)
            | Verb::JobStatus(_)
            | Verb::CancelJob(_)
            | Verb::ForgetJob(_)) => self.job(v)?,
            Verb::DescribeStorage(_) => {
                let classes = self.classes.iter().map(Class::describe).collect();
                Answer::DescribeStorage(Box::new(helper::DescribeStorageResponse {
                    classes,
                    ..Default::default()
                }))
            }
        })
    }

    /// The encoded answer to the encoded request `bytes`.
    pub fn handle(&mut self, bytes: &[u8]) -> Vec<u8> {
        let answer = match helper::HelperRequest::decode_from_slice(bytes) {
            Ok(r) => match r.verb {
                Some(verb) => {
                    self.dispatch(verb).unwrap_or_else(|e| Answer::Error(Box::new(e.to_pb())))
                }
                None => Answer::Error(Box::new(unsupported())),
            },
            Err(e) => {
                tracing::warn!(error = %e, "a request that does not decode was refused");
                Answer::Error(Box::new(HelperError::Refused("undecodable request".into()).to_pb()))
            }
        };
        helper::HelperResponse { result: Some(answer), ..Default::default() }.encode_to_vec()
    }
}
