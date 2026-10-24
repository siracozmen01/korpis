// SPDX-License-Identifier: AGPL-3.0-or-later

//! Installs into a workload's block devices (§4.4 of docs/04-runtimes.md, §4 of
//! docs/09-recipes.md): run in a service guest by the install job its boot drive carries, as the
//! node's helper runs one beside a container, and asked after as the helper's jobs are. The
//! driver writes the job's spec, naming the guest's own paths, and what it reads from the node's
//! content store onto a content drive the guest mounts read-only in the image's place; the guest
//! runs the job once its root is ready and says how it goes when asked. The driver asks until the
//! job has ended, keeps what it wrote, and stops the guest, its volumes synced, unless a file
//! session still holds it. What it keeps is in memory: a driver that restarts knows no install,
//! and the node asks again from where the last progress it kept says, as for a lost worker.

use super::service::{Wanted, content_of, service_id};
use super::{Machine, MachineError, MachineResult, failed};
use crate::{agent, jail, process};
use buffa::Message as _;
use korpis_guest::config::Role;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::install::v1 as pb;
use korpis_proto::korpis::v1 as common;
use std::collections::BTreeMap;
use std::time::Duration;

/// How often a running install is asked after, and how long its agent is given to answer.
const POLL: Duration = Duration::from_millis(500);
const ASK: Duration = Duration::from_secs(5);

/// How an install stands, as the driver keeps it.
#[derive(Debug, Clone)]
pub(super) enum Outcome {
    /// Its guest runs it; how many actions it has run.
    Running { workload: String, progress: u32 },
    /// It wrote its result.
    Finished { result: pb::InstallResult, progress: u32 },
    /// It ended without one.
    Lost { progress: u32 },
}

/// An install the node asks for.
#[derive(Debug, Clone)]
pub struct Install {
    /// The job, as the node names it.
    pub job: String,
    /// The workload's spec as `Prepare` made it.
    pub spec: driver::PreparedSpec,
    /// The actions, in order.
    pub actions: Vec<common::Value>,
    /// The first action to run.
    pub from: u32,
    /// The recipe's files, by path, and each one's digest.
    pub files: BTreeMap<String, String>,
    /// What its fetches downloaded, by digest.
    pub fetched: Vec<String>,
}

/// What the driver says of an install.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallStatus {
    /// How it stands.
    pub state: driver::InstallState,
    /// What it wrote, once it finished.
    pub result: Option<pb::InstallResult>,
    /// How many of its actions have run.
    pub progress: u32,
}

/// The job's spec for `asked`, naming the guest's own paths: each volume the workload writes
/// where the guest mounts it, the content drive, and the guest's ids from 0.
fn spec_of(asked: &Install) -> pb::InstallSpec {
    let volumes = asked
        .spec
        .mounts
        .iter()
        .filter(|m| !m.volume.is_empty() && !m.read_only)
        .map(|m| pb::InstallVolume {
            path: m.destination.clone(),
            volume: m.volume.clone(),
            directory: m.destination.clone(),
            ..Default::default()
        })
        .collect();
    pb::InstallSpec {
        id: asked.job.clone(),
        actions: asked.actions.clone(),
        from: asked.from,
        volumes,
        files: asked.files.clone(),
        content: korpis_guest::drive::INSTALL_CONTENT.into(),
        out: korpis_guest::drive::INSTALL_OUT.into(),
        root: korpis_guest::drive::INSTALL_ROOT.into(),
        id_base: 0,
        ..Default::default()
    }
}

impl Machine {
    /// Starts `asked` in a service guest of its workload's, ending one that serves files, whose
    /// sessions then end; a job already known is left as it is.
    ///
    /// # Errors
    /// The spec is not one this driver builds, an object of the workload runs, another install
    /// runs, or the content or the guest could not be made.
    pub async fn start_install(&self, asked: Install) -> MachineResult<()> {
        super::create::validate(&asked.spec, &self.settings)?;
        let workload = asked.spec.workload.clone();
        let mut running = self.services.0.lock().await;
        if running.installs.contains_key(&asked.job) {
            return Ok(());
        }
        if running.held.get(&workload).is_some_and(|h| h.install.is_some()) {
            let why = format!("another install is writing {workload}'s volumes");
            return Err(MachineError::Refused(why));
        }
        if let Some(held) = running.held.remove(&workload) {
            self.halt(&workload, &held.vm).await;
        }
        let content = self.content(&asked).await?;
        let vm = self.boot(&Wanted::own(&asked.spec, Role::Install, Some(content))).await?;
        let number = running.hold(&workload, vm, 0, Some(asked.job.clone()));
        let started = Outcome::Running { workload: workload.clone(), progress: asked.from };
        running.installs.insert(asked.job.clone(), started);
        tokio::spawn(self.clone().watch_install(workload, asked.job, number));
        Ok(())
    }

    /// Makes the content drive of `asked`, given to its guest's own id, readable by it alone.
    async fn content(&self, asked: &Install) -> MachineResult<std::path::PathBuf> {
        let at = content_of(&self.settings, &service_id(&asked.spec.workload));
        let wanted = korpis_image::content::Wanted {
            blobs: asked.files.values().cloned().collect(),
            fetched: asked.fetched.clone(),
        };
        let (store, spec, out) =
            (self.settings.content.clone(), spec_of(asked).encode_to_vec(), at.clone());
        let made = tokio::task::spawn_blocking(move || {
            korpis_image::content::drive(&store, &wanted, &spec, &out)
        });
        made.await.map_err(failed)?.map_err(|e| MachineError::Refused(e.to_string()))?;
        crate::volume::give(&at, asked.spec.uid_base).map_err(failed)?;
        Ok(at)
    }

    /// Asks after install `job` in `workload`'s service guest `number` until it has ended, keeps
    /// how it ended, and stops the guest unless a file session holds it.
    async fn watch_install(self, workload: String, job: String, number: u64) {
        let socket = jail::vsock_socket(&self.settings, &service_id(&workload));
        let mut progress = 0;
        let ended = loop {
            tokio::time::sleep(POLL).await;
            let vm = {
                let running = self.services.0.lock().await;
                running.held.get(&workload).filter(|h| h.number == number).map(|h| h.vm)
            };
            if !vm.is_some_and(|vm| process::alive(&vm)) {
                break Outcome::Lost { progress };
            }
            let Ok(said) = agent::install_status(&socket, ASK).await else { continue };
            progress = said.progress;
            if said.ended {
                break match pb::InstallResult::decode_from_slice(&said.result) {
                    Ok(result) if !said.result.is_empty() => Outcome::Finished { result, progress },
                    _ => Outcome::Lost { progress },
                };
            }
            self.progressed(&job, progress).await;
        };
        self.finished(&workload, (&job, number), ended).await;
    }

    /// Keeps `progress` for running install `job`.
    async fn progressed(&self, job: &str, progress: u32) {
        let mut running = self.services.0.lock().await;
        if let Some(Outcome::Running { progress: kept, .. }) = running.installs.get_mut(job) {
            *kept = progress;
        }
    }

    /// Keeps how install `job` ended, if it is still known, and stops service guest `number` of
    /// `workload` unless a file session holds it.
    async fn finished(&self, workload: &str, (job, number): (&str, u64), ended: Outcome) {
        let mut running = self.services.0.lock().await;
        if let Some(kept) = running.installs.get_mut(job) {
            *kept = ended;
        }
        let Some(held) = running.held.get_mut(workload).filter(|h| h.number == number) else {
            return;
        };
        held.install = None;
        if held.sessions == 0
            && let Some(held) = running.held.remove(workload)
        {
            self.halt(workload, &held.vm).await;
        }
    }

    /// How install `job` stands.
    pub async fn install_status(&self, job: &str) -> InstallStatus {
        use driver::InstallState as S;
        let running = self.services.0.lock().await;
        let (state, result, progress) = match running.installs.get(job) {
            None => (S::INSTALL_STATE_UNKNOWN, None, 0),
            Some(Outcome::Running { progress, .. }) => (S::INSTALL_STATE_RUNNING, None, *progress),
            Some(Outcome::Finished { result, progress }) => {
                (S::INSTALL_STATE_FINISHED, Some(result.clone()), *progress)
            }
            Some(Outcome::Lost { progress }) => (S::INSTALL_STATE_LOST, None, *progress),
        };
        InstallStatus { state, result, progress }
    }

    /// Forgets install `job`, ending its guest first if it still runs it.
    pub async fn forget_install(&self, job: &str) {
        let mut running = self.services.0.lock().await;
        let Some(Outcome::Running { workload, .. }) = running.installs.remove(job) else { return };
        let runs = running.held.get(&workload).is_some_and(|h| h.install.as_deref() == Some(job));
        if runs && let Some(held) = running.held.remove(&workload) {
            self.halt(&workload, &held.vm).await;
        }
    }
}
