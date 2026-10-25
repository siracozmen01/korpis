// SPDX-License-Identifier: AGPL-3.0-or-later

//! One request in, one answer out: the envelope decoded, the verb dispatched to the class or
//! the ledger it concerns, and whatever happened encoded as the answer. Nothing here panics on
//! what a peer sends; a request that does not decode, or names a verb this helper does not
//! know, is answered as such.

use crate::error::{HelperError, HelperResult};
use crate::ids::Ledger;
use crate::ingress::Ingress;
use crate::job::{JobConfig, Jobs};
use crate::kept::Kept;
use crate::network::Network;
use crate::storage::Class;
use buffa::Message as _;
use korpis_core::workload::assignment::IdRange;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

mod held;
mod placing;
mod sessions;
mod snapshots;

/// The helper's state: its classes, the first of them the default, and its ledger.
#[derive(Debug)]
pub struct Helper {
    classes: Vec<Class>,
    ledger: Ledger,
    jobs: Option<Jobs>,
    network: Option<Network>,
    ingress: Option<Ingress>,
    files: Option<crate::files::Files>,
    cores: Option<crate::cores::Cores>,
    /// The user and group of the connection being answered, as the kernel recorded them.
    peer: (u32, u32),
    /// Descriptors the last answer carries, sent with its frame and then closed here.
    attached: Vec<rustix::fd::OwnedFd>,
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
        Self {
            classes,
            ledger,
            jobs: None,
            network: None,
            ingress: None,
            files: None,
            cores: None,
            peer: (u32::MAX, u32::MAX),
            attached: Vec::new(),
        }
    }

    /// The same helper, giving workloads networks as `network` keeps them.
    #[must_use]
    pub fn with_network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }

    /// The same helper, starting the ingress proxy as `ingress` says.
    #[must_use]
    pub fn with_ingress(mut self, ingress: Ingress) -> Self {
        self.ingress = Some(ingress);
        self
    }

    /// Answers the connection of `uid` and `gid` from now on.
    pub fn answering(&mut self, uid: u32, gid: u32) {
        self.peer = (uid, gid);
    }

    /// The answer to an ingress verb, keeping the descriptors it carries for the transport.
    fn ingress_verb(&mut self, verb: Verb) -> HelperResult<Answer> {
        let (answer, attached) = crate::ingress::answer(self.ingress.as_mut(), self.peer, verb)?;
        self.attached = attached;
        Ok(answer)
    }

    fn network(&self) -> HelperResult<&Network> {
        self.network
            .as_ref()
            .ok_or_else(|| HelperError::Refused("this helper makes no networks".into()))
    }

    /// The descriptors the last answer carries, which the transport sends with its frame.
    pub fn take_attached(&mut self) -> Vec<rustix::fd::OwnedFd> {
        std::mem::take(&mut self.attached)
    }

    /// The answer to a network verb.
    fn networking(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::EnsureNetwork(r) => {
                // The namespaces map the workload's own range, recorded by EnsureIdRange, and
                // no other: a range at the host's own ids would make them the workload's.
                let range = IdRange { base: r.uid_base, count: r.uid_count };
                if range.slot().is_none() || self.ledger.holder(r.uid_base) != Some(&r.workload) {
                    return Err(HelperError::Refused(
                        "not the workload's recorded id range".into(),
                    ));
                }
                Answer::EnsureNetwork(Box::new(self.network()?.ensure(&r)?))
            }
            Verb::ReleaseNetwork(r) => {
                let released = self.network()?.release(&r.workload)?;
                Answer::ReleaseNetwork(Box::new(helper::ReleaseNetworkResponse {
                    released,
                    ..Default::default()
                }))
            }
            Verb::OpenResolver(r) => {
                self.attached = self.network()?.open_resolver(&r.workload)?;
                let attached = u32::try_from(self.attached.len()).unwrap_or(u32::MAX);
                Answer::OpenResolver(Box::new(helper::OpenResolverResponse {
                    attached,
                    ..Default::default()
                }))
            }
            Verb::OpenProbe(r) => {
                let udp = r.transport.as_known() == Some(helper::Transport::TRANSPORT_UDP);
                self.attached = self.network()?.open_probe(&r.workload, udp)?;
                let attached = u32::try_from(self.attached.len()).unwrap_or(u32::MAX);
                Answer::OpenProbe(Box::new(helper::OpenProbeResponse {
                    attached,
                    ..Default::default()
                }))
            }
            Verb::DescribeNetwork(_) => Answer::DescribeNetwork(Box::new(match &self.network {
                Some(n) => n.describe(),
                None => helper::DescribeNetworkResponse {
                    why_not: "the helper was started with no network root".into(),
                    ..Default::default()
                },
            })),
            Verb::EnsureUplinks(r) => {
                Answer::EnsureUplinks(Box::new(self.network()?.ensure_uplinks(&r)?))
            }
            Verb::ReadCounters(_) => {
                Answer::ReadCounters(Box::new(self.network()?.read_counters()?))
            }
            _ => return Err(HelperError::Refused("not a network verb".into())),
        })
    }

    /// The same helper, running backup and restore jobs as `config` says.
    #[must_use]
    pub fn with_jobs(mut self, config: JobConfig) -> Self {
        self.jobs = Some(Jobs::new(config));
        self
    }

    /// Whether a job or the ingress proxy this helper started still runs, so it must not exit
    /// while idle.
    pub fn busy(&mut self) -> bool {
        self.jobs.as_mut().is_some_and(Jobs::busy)
            || self.ingress.as_mut().is_some_and(Ingress::running)
            || self.files.as_mut().is_some_and(crate::files::Files::running)
    }

    fn jobs(&mut self) -> HelperResult<&mut Jobs> {
        self.jobs.as_mut().ok_or_else(|| HelperError::Refused("this helper runs no jobs".into()))
    }

    /// The answer to a job verb.
    fn job(&mut self, verb: Verb) -> HelperResult<Answer> {
        self.jobs()?;
        Ok(match verb {
            Verb::StartJob(r) => {
                // A job already started is not placed again: placing a replacing restore
                // empties its staging place.
                let started = !self.jobs()?.known(&r.job)? && {
                    let placed = self.place(&r)?;
                    self.jobs()?.start(&r, placed)?
                };
                Answer::StartJob(Box::new(helper::StartJobResponse {
                    started,
                    ..Default::default()
                }))
            }
            Verb::JobStatus(r) => Answer::JobStatus(Box::new(self.job_status(&r.job)?)),
            Verb::CancelJob(r) => {
                let cancelled = self.jobs()?.cancel(&r.job)?;
                Answer::CancelJob(Box::new(helper::CancelJobResponse {
                    cancelled,
                    ..Default::default()
                }))
            }
            Verb::ForgetJob(r) => {
                // The snapshot a running job reads is not released under it.
                let state = self.jobs()?.status(&r.job)?.state;
                if state.as_known() == Some(helper::JobState::JOB_STATE_RUNNING) {
                    return Err(HelperError::Refused(format!("job {} is running", r.job)));
                }
                self.release(&r.job, false)?;
                self.jobs()?.forget(&r.job)?;
                Answer::ForgetJob(Box::default())
            }
            Verb::CommitJob(r) => {
                let status = self.jobs()?.status(&r.job)?;
                let result = status.result.as_option();
                let whole = result
                    .is_some_and(|r| r.ok && r.restored.as_option().is_some_and(|x| x.verified));
                if status.state.as_known() != Some(helper::JobState::JOB_STATE_FINISHED) || !whole {
                    return Err(HelperError::Refused(format!("job {} is no whole restore", r.job)));
                }
                if !self.release(&r.job, true)? {
                    return Err(HelperError::Refused(format!("job {} replaces nothing", r.job)));
                }
                Answer::CommitJob(Box::default())
            }
            Verb::ListJobs(_) => {
                let jobs = self.jobs()?.list()?;
                Answer::ListJobs(Box::new(helper::ListJobsResponse { jobs, ..Default::default() }))
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

    /// Releases what `job` keeps: its snapshot deleted, or its staging place committed when
    /// `commit` and discarded otherwise, or its reading place discarded unless `commit`; whether
    /// it kept a staging place to exchange. A volume gone took all of them with it.
    fn release(&mut self, job: &str, commit: bool) -> HelperResult<bool> {
        let Some(kept) = self.jobs()?.kept_of(job)? else { return Ok(false) };
        let Ok(class) = self.holding(kept.volume()) else { return Ok(false) };
        if let Some(s) = kept.staging() {
            class.settle(&s, commit)?;
            return Ok(true);
        }
        match &kept {
            Kept::Snapshot { volume, name } if !commit => {
                class.delete_snapshot(volume, name)?;
            }
            // A reading place is never made the volume, and a commit leaves it for ForgetJob.
            Kept::Reading { volume, job } if !commit => class.discard_reading(volume, job)?,
            _ => {}
        }
        Ok(false)
    }

    /// How job `job` stands, and for a reading restore the image it restores into.
    fn job_status(&mut self, job: &str) -> HelperResult<helper::JobStatusResponse> {
        let mut status = self.jobs()?.status(job)?;
        if let Some(Kept::Reading { volume, job }) = self.jobs()?.kept_of(job)? {
            let image = self.holding(&volume)?.reading_image(&volume, &job);
            status.image = image.to_string_lossy().into_owned();
        }
        Ok(status)
    }

    /// The class that holds `volume`, if any does.
    fn holding(&mut self, volume: &str) -> HelperResult<&mut Class> {
        crate::error::identifier("volume", volume)?;
        self.classes
            .iter_mut()
            .find(|c| c.path_of(volume).symlink_metadata().is_ok())
            .ok_or_else(|| HelperError::NotFound(format!("volume {volume}")))
    }

    fn storage(&self) -> helper::DescribeStorageResponse {
        let classes = self.classes.iter().map(Class::describe).collect();
        helper::DescribeStorageResponse { classes, ..Default::default() }
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
            v @ (Verb::SnapshotVolume(_) | Verb::RollbackVolume(_) | Verb::DeleteSnapshot(_)) => {
                self.snapshot_verb(v)?
            }
            v @ (Verb::StartJob(_)
            | Verb::JobStatus(_)
            | Verb::CancelJob(_)
            | Verb::ForgetJob(_)
            | Verb::CommitJob(_)
            | Verb::ListJobs(_)) => self.job(v)?,
            v @ (Verb::EnsureNetwork(_)
            | Verb::ReleaseNetwork(_)
            | Verb::DescribeNetwork(_)
            | Verb::EnsureUplinks(_)
            | Verb::ReadCounters(_)
            | Verb::OpenResolver(_)
            | Verb::OpenProbe(_)) => self.networking(v)?,
            v @ (Verb::OpenIngress(_) | Verb::StartIngress(_) | Verb::StopIngress(_)) => {
                self.ingress_verb(v)?
            }
            Verb::DescribeStorage(_) => Answer::DescribeStorage(Box::new(self.storage())),
            Verb::OpenFiles(r) => self.open_files(&r)?,
            Verb::HoldCores(r) => self.hold_cores(&r)?,
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
