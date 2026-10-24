// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's install step (§4 of docs/09-recipes.md). The step's detail carries every action the
//! install runs; the node fetches what they read into its content store (each `fetch` by its hash,
//! and the recipe's own files), then has the helper run them in a worker that sees nothing but the
//! workload's volumes and that content. An install outlasts many passes of the loop, as a backup
//! does: each pass asks the helper where its job is, and waits while it runs. A workload whose
//! driver takes its volumes as block devices has its installs run by that driver instead, in a
//! service guest (§4.4 of docs/04-runtimes.md), asked after the same way.
//!
//! How far an install has come is kept per install, not per plan: a digest of the workload and its
//! actions names it, and its job. A worker that fails, or is lost, has its progress kept and its job
//! forgotten, so the next attempt, in this plan for a lost worker or in a later one after a
//! failure, starts a job of the same name at the action that did not finish. A worker lost three
//! times in a row fails the step rather than looping.

mod resolve;

use super::act::{Failure, volume_id};
use super::progress::Install;
use super::step::After;
use super::{Reconciler, Tracked, text};
use crate::host::Host;
use crate::runtime::Runtime;
use buffa::Message as _;
use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::{InstallAction, detail};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::install::v1 as install;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
use sha2::{Digest as _, Sha256};

/// How many times in a row a worker may be lost before the install fails.
const LOST_AT_MOST: u32 = 3;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

/// The actions step `s` carries, as the wire carries them.
fn actions(s: &pb::Step) -> Result<&[pb::Value], Failure> {
    match s.detail.get("actions").and_then(|v| v.kind.as_ref()) {
        Some(Kind::ListValue(list)) => Ok(&list.values),
        _ => Err(failed("an install step lists its actions")),
    }
}

/// Which install `actions` for `workload` is: the hex of their digest.
fn digest_of(workload: &str, actions: &[pb::Value]) -> String {
    let mut h = Sha256::new();
    h.update(workload.as_bytes());
    for a in actions {
        let bytes = a.encode_to_vec();
        h.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        h.update(&bytes);
    }
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The job install step `s` of `workload` works through, if it is an install step.
pub(super) fn install_job(workload: &str, s: &pb::Step) -> Option<String> {
    let actions = actions(s).ok()?;
    Some(format!("ins-{}", digest_of(workload, actions).get(..32)?))
}

/// Who runs an install.
enum Installer {
    /// The helper, beside a container.
    Helper,
    /// The driver named, in a service guest.
    Driver(String),
}

/// A driver's answer, as the helper's would be.
fn as_job(s: driver::InstallStatusResponse) -> helper::JobStatusResponse {
    use driver::InstallState as I;
    use helper::JobState as J;
    let state = match s.state.as_known() {
        Some(I::INSTALL_STATE_RUNNING) => J::JOB_STATE_RUNNING,
        Some(I::INSTALL_STATE_FINISHED) => J::JOB_STATE_FINISHED,
        Some(I::INSTALL_STATE_LOST) => J::JOB_STATE_LOST,
        _ => J::JOB_STATE_UNKNOWN,
    };
    helper::JobStatusResponse {
        state: state.into(),
        install: s.result,
        progress: s.progress,
        ..Default::default()
    }
}

/// The driver's request for install `job` of `actions` from `from` into the volumes of the
/// workload `spec` describes, its volumes placed: what the guest runs and nothing else, so the
/// spec's console, network, and identity directory are left out.
pub(super) fn for_driver(
    job: &str,
    mut spec: driver::PreparedSpec,
    (actions, from): (Vec<pb::Value>, u32),
    files: std::collections::BTreeMap<String, String>,
    fetched: Vec<String>,
) -> driver::StartInstallRequest {
    spec.stream = None.into();
    spec.namespaces.clear();
    spec.addresses.clear();
    spec.identity_dir.clear();
    driver::StartInstallRequest {
        job: job.to_owned(),
        spec: spec.into(),
        actions,
        from,
        files,
        fetched,
        ..Default::default()
    }
}

/// The actions decoded, to find what they read.
fn decoded(actions: &[pb::Value]) -> Result<Vec<InstallAction>, Failure> {
    actions
        .iter()
        .map(|v| {
            let value = korpis_wire::value::from_pb("actions", v).map_err(failed)?;
            detail::decode(&value).map_err(failed)
        })
        .collect()
}

/// Whether an action reads one of the recipe's own files.
fn reads_files(a: &InstallAction) -> bool {
    match &a.step {
        InstallStep::Copy { .. } | InstallStep::Template { .. } => true,
        InstallStep::Extract { from, .. } => !from.starts_with('/'),
        _ => false,
    }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves install step `s` on; its result once the install ended, `None` while it has not.
    pub(super) async fn install_step(
        &mut self,
        a: &agent::Assignment,
        plan: &str,
        s: &pb::Step,
        t: &mut Tracked,
    ) -> Result<Option<After>, Failure> {
        let actions = actions(s)?;
        let digest = digest_of(&a.workload, actions);
        let job = install_job(&a.workload, s).ok_or_else(|| failed("no install job"))?;
        let kept = self.progress.of(&a.workload).install.filter(|i| i.digest == digest);
        let mut at = kept.unwrap_or(Install { digest, done: 0, lost: 0 });
        let by = self.installer(a, t).await?;
        let status = self.job_status(&by, &job).await?;
        match status.state.as_known() {
            Some(helper::JobState::JOB_STATE_UNKNOWN) => {
                let decoded = decoded(actions)?;
                let Some(answers) = self.answers((plan, s), &at.digest, &decoded)? else {
                    return Ok(None);
                };
                let expanded = resolve::expand(actions, &decoded, &answers)?;
                self.keep_install(&a.workload, at.clone())?;
                self.start_install(a, t, (&by, &job, at.done), expanded, &decoded).await?;
                Ok(None)
            }
            Some(helper::JobState::JOB_STATE_RUNNING) => Ok(None),
            Some(helper::JobState::JOB_STATE_FINISHED) => {
                let result = status.install.into_option().unwrap_or_default();
                at.done = result.done.max(status.progress);
                at.lost = 0;
                self.keep_install(&a.workload, at.clone())?;
                if result.ok {
                    self.progress
                        .drop_answers(&at.digest)
                        .map_err(|e| failed(format!("answers: {e}")))?;
                    let mut after = After::new();
                    after.insert("actions".into(), text(&at.done.to_string()));
                    return Ok(Some(after));
                }
                self.forget_job(&by, &job).await?;
                Err(failed(format!("install action {}: {}", at.done, result.error)))
            }
            _ => {
                at.done = at.done.max(status.progress);
                at.lost = at.lost.saturating_add(1);
                self.keep_install(&a.workload, at.clone())?;
                self.forget_job(&by, &job).await?;
                if at.lost >= LOST_AT_MOST {
                    return Err(failed(format!(
                        "the install's worker was lost {LOST_AT_MOST} times"
                    )));
                }
                tracing::warn!(%job, done = at.done, "an install's worker was lost; it resumes");
                Ok(None)
            }
        }
    }

    /// Who runs the installs of `a`'s workload: its driver, in a service guest, when it takes
    /// its volumes as block devices, whose files only a guest's kernel may parse (§4.4 of
    /// docs/04-runtimes.md); the helper otherwise.
    async fn installer(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<Installer, Failure> {
        let spec = self.prepare(a, t).await?;
        Ok(if self.wiring.driver_kinds.block_device(&spec.driver) {
            Installer::Driver(spec.driver)
        } else {
            Installer::Helper
        })
    }

    /// How job `job` stands, as `by` says, in the helper's terms.
    async fn job_status(
        &self,
        by: &Installer,
        job: &str,
    ) -> Result<helper::JobStatusResponse, Failure> {
        match by {
            Installer::Helper => Ok(self.host.job_status(job).await?),
            Installer::Driver(name) => Ok(as_job(self.runtime.install_status(name, job).await?)),
        }
    }

    /// Has `by` forget job `job`.
    async fn forget_job(&self, by: &Installer, job: &str) -> Result<(), Failure> {
        match by {
            Installer::Helper => Ok(self.host.forget_job(job).await?),
            Installer::Driver(name) => Ok(self.runtime.forget_install(name, job).await?),
        }
    }

    fn keep_install(&mut self, workload: &str, at: Install) -> Result<(), Failure> {
        self.progress.installed(workload, at).map_err(|e| failed(format!("install progress: {e}")))
    }

    /// Fetches what the actions read, then has `by` start the job at action `from` of
    /// `expanded`, the step's actions, `decoded`, with its provider steps resolved.
    async fn start_install(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        (by, job, from): (&Installer, &str, u32),
        expanded: resolve::Expanded,
        decoded: &[InstallAction],
    ) -> Result<(), Failure> {
        for (url, digest) in &expanded.fetches {
            self.host.fetch_url(url, digest).await?;
        }
        let wire = expanded.actions;
        let files = if decoded.iter().any(reads_files) {
            let body = a.target.as_option().and_then(|i| i.body.as_option());
            let recipe = body.and_then(|b| b.recipe.as_option());
            let recipe = recipe.ok_or_else(|| failed("the target names no recipe"))?;
            self.host.fetch_files(&format!("{}@{}", recipe.name, recipe.digest)).await?
        } else {
            std::collections::BTreeMap::new()
        };
        let mut spec = self.prepare(a, t).await?;
        if let Installer::Driver(_) = by {
            self.mount(a, &mut spec).await?;
            // The workload's ids, which its guest runs as and its volumes are given to.
            (spec.uid_base, spec.uid_count) = (a.uid_base, a.uid_count);
            let fetched = expanded.fetches.iter().map(|(_, digest)| digest.clone()).collect();
            let asked = for_driver(job, spec, (wire, from), files, fetched);
            self.runtime.start_install(&asked).await?;
            tracing::info!(%job, from, "install started in a service guest");
            return Ok(());
        }
        let volumes = spec
            .mounts
            .iter()
            .filter(|m| !m.volume.is_empty() && !m.read_only)
            .map(|m| install::InstallVolume {
                path: m.destination.clone(),
                volume: volume_id(a, &m.volume),
                ..Default::default()
            })
            .collect();
        let work = helper::InstallWork {
            actions: wire,
            from,
            volumes,
            files,
            workload: a.workload.clone(),
            id_base: a.uid_base,
            ..Default::default()
        };
        self.host.ensure_ids(&a.workload, a.uid_base, a.uid_count).await?;
        let request = helper::StartJobRequest {
            job: job.to_owned(),
            work: Some(Work::Install(Box::new(work))),
            ..Default::default()
        };
        self.host.start_job(&request).await?;
        tracing::info!(%job, from, "install started");
        Ok(())
    }
}
