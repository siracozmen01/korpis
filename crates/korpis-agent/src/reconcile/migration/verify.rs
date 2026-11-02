// SPDX-License-Identifier: AGPL-3.0-or-later

//! `verify`, on either side: each volume sent has its manifest computed by a job of the helper,
//! over the pass both sides hold for a volume crossing by increments, and over the source's
//! volume, stopped, or the destination's copy for one crossing by its tree; the destination
//! prepares the target again; and the phase is reported once every manifest is in, with each, or
//! failed where a job failed or the driver refuses. A job lost with the helper is started again.

use super::super::act::volume_id;
use super::super::{Reconciler, Tracked};
use super::{by_increments, crosses};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;

/// The job computing the manifest of `volume` for `migration` on this node.
fn job_of(migration: &str, volume: &str) -> String {
    format!("manifest-{migration}-{volume}")
}

/// What a volume's manifest is computed over on this side of `role`.
fn subject(
    a: &agent::Assignment,
    role: &agent::MigrationRole,
    v: &agent::MigrationVolumeRole,
) -> helper::ManifestVolume {
    let here = role.role.as_known() == Some(agent::MigrationSide::MIGRATION_SIDE_DESTINATION);
    let (volume, pass) = if by_increments(v) {
        (v.source_helper_id.clone(), v.held_pass.clone())
    } else if here {
        (volume_id(a, &v.name), String::new())
    } else {
        (v.source_helper_id.clone(), String::new())
    };
    let storage_class = crate::migrate::class_of(a, &v.name);
    helper::ManifestVolume { volume, storage_class, pass, ..Default::default() }
}

/// Where one volume's manifest stands.
enum Manifested {
    /// Computed: its part of the report.
    Done(agent::MigrationVolumeProgress),
    /// Not yet.
    Waiting,
    /// It could not be computed, for this reason.
    Failed(String),
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// At `verify`: this side's manifests, and on the destination the target prepared again,
    /// reported once.
    pub(super) async fn verified(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        role: &agent::MigrationRole,
    ) {
        if self.reported(t, role) {
            return;
        }
        let mut volumes = Vec::new();
        for v in role.volumes.iter().filter(|v| crosses(v)) {
            match self.manifested(a, role, v).await {
                Manifested::Done(part) => volumes.push(part),
                Manifested::Waiting => return,
                Manifested::Failed(why) => {
                    return self.report(t, role, (Vec::new(), Vec::new()), Some(why)).await;
                }
            }
        }
        let destination =
            role.role.as_known() == Some(agent::MigrationSide::MIGRATION_SIDE_DESTINATION);
        let refused = if destination { self.prepare(a, t).await.err().map(|e| e.0) } else { None };
        self.report(t, role, (volumes, Vec::new()), refused).await;
        for v in role.volumes.iter().filter(|v| crosses(v)) {
            if let Err(e) = self.host.forget_job(&job_of(&role.migration, &v.name)).await {
                tracing::warn!(volume = %v.name, error = %e.0, "a manifest job not forgotten");
            }
        }
    }

    /// The manifest of `v` on this side of `role`, its job started if it was not.
    async fn manifested(
        &self,
        a: &agent::Assignment,
        role: &agent::MigrationRole,
        v: &agent::MigrationVolumeRole,
    ) -> Manifested {
        use helper::JobState as S;
        let job = job_of(&role.migration, &v.name);
        let request = helper::StartJobRequest {
            job: job.clone(),
            work: Some(Work::Manifest(Box::new(subject(a, role, v)))),
            ..Default::default()
        };
        if let Err(e) = self.host.start_job(&request).await {
            return Manifested::Failed(format!("volume {}'s manifest: {}", v.name, e.0));
        }
        let status = match self.host.job_status(&job).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(volume = %v.name, error = %e.0, "a manifest job not read");
                return Manifested::Waiting;
            }
        };
        match status.state.as_known() {
            Some(S::JOB_STATE_FINISHED) => {
                let result = status.result.into_option().unwrap_or_default();
                if !result.ok {
                    return Manifested::Failed(format!("volume {}'s manifest: {}", v.name, result.error));
                }
                let m = result.manifest.into_option().unwrap_or_default();
                let chunks = m.runs.into_iter().map(|r| agent::ManifestChunk {
                    first_path: r.first_path,
                    digest: r.digest,
                    ..Default::default()
                });
                Manifested::Done(agent::MigrationVolumeProgress {
                    name: v.name.clone(),
                    manifest: m.digest,
                    chunks: chunks.collect(),
                    ..Default::default()
                })
            }
            Some(S::JOB_STATE_LOST) => {
                // Started again at the next pass.
                let _ = self.host.forget_job(&job).await;
                Manifested::Waiting
            }
            _ => Manifested::Waiting,
        }
    }
}
