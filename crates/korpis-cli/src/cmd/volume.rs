// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis volume …` (§4 and §7 of docs/06-storage.md): what is done to a volume apart from its
//! workload. Deleting a retained volume, rolling one back, and deleting a snapshot each propose a
//! plan of one irreversible step that always waits for a person; taking a snapshot applies at
//! once where nothing in it needs one. A volume a workload holds is deleted with that workload
//! (`korpis workload delete --delete-volumes`).

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show::{self, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis volume …`.
#[derive(Debug, Clone, Subcommand)]
pub enum VolumeAction {
    /// Proposes deleting a retained volume, data and snapshots and all; the plan waits for a
    /// person.
    Delete {
        /// The volume, `vol_…`, as `korpis get volumes` lists it.
        volume: String,
    },
    /// Takes a snapshot of a volume, in a class that can.
    Snapshot {
        /// The volume, `vol_…`.
        volume: String,
        /// The snapshot's name, unique among the volume's.
        name: String,
    },
    /// A volume's snapshots.
    Snapshots {
        /// The volume, `vol_…`.
        volume: String,
        /// Deleted ones too.
        #[arg(long)]
        deleted: bool,
    },
    /// Proposes making a volume what a snapshot holds; what was written since is lost, and the
    /// plan waits for a person.
    Rollback {
        /// The snapshot, `snp_…`.
        snapshot: String,
    },
    /// Proposes deleting a snapshot; the plan waits for a person.
    DeleteSnapshot {
        /// The snapshot, `snp_…`.
        snapshot: String,
    },
}

fn planned(plan: Option<pb::Plan>) -> Answer {
    let plan = plan.unwrap_or_default();
    Answer { human: show::plan(&plan, &plan.workload), json: json(&plan) }
}

/// Answers `korpis volume …`.
pub async fn volume(c: &Connection, action: &VolumeAction) -> Result<Answer, CliError> {
    let client = pb::VolumeServiceClient::new(c.http(), c.config());
    let m = || mutation(None).into();
    Ok(match action {
        VolumeAction::Delete { volume } => {
            let r =
                pb::DeleteVolumeRequest { mutation: m(), id: volume.clone(), ..Default::default() };
            planned(client.delete_volume(r).await?.into_owned().plan.into_option())
        }
        VolumeAction::Snapshot { volume, name } => {
            let r = pb::CreateSnapshotRequest {
                mutation: m(),
                volume: volume.clone(),
                name: name.clone(),
                ..Default::default()
            };
            let made = client.create_snapshot(r).await?.into_owned();
            let mut answer = planned(made.plan.into_option());
            answer.human = format!("snapshot {}\n{}", made.snapshot, answer.human);
            answer
        }
        VolumeAction::Snapshots { volume, deleted } => {
            let r = pb::ListSnapshotsRequest {
                volume: volume.clone(),
                include_deleted: *deleted,
                ..Default::default()
            };
            let list = client.list_snapshots(r).await?.into_owned().snapshots;
            let mut rows = vec![vec!["ID".into(), "NAME".into(), "TAKEN".into(), "DELETED".into()]];
            rows.extend(list.iter().map(|s| {
                let deleted = s.deleted_at.as_option().map_or("-".into(), |t| time(Some(t)));
                vec![s.id.clone(), s.name.clone(), time(s.created.as_option()), deleted]
            }));
            Answer { human: table(&rows), json: json(&list) }
        }
        VolumeAction::Rollback { snapshot } => {
            let r = pb::RollbackVolumeRequest {
                mutation: m(),
                snapshot: snapshot.clone(),
                ..Default::default()
            };
            planned(client.rollback_volume(r).await?.into_owned().plan.into_option())
        }
        VolumeAction::DeleteSnapshot { snapshot } => {
            let r = pb::DeleteSnapshotRequest {
                mutation: m(),
                id: snapshot.clone(),
                ..Default::default()
            };
            planned(client.delete_snapshot(r).await?.into_owned().plan.into_option())
        }
    })
}
