// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis volume delete` (§7 of docs/06-storage.md): proposes deleting a retained volume no
//! workload holds. The plan has one irreversible step and always waits for a person; a volume a
//! workload holds is deleted with that workload (`korpis workload delete --delete-volumes`).

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis volume …`.
#[derive(Debug, Clone, Subcommand)]
pub enum VolumeAction {
    /// Proposes deleting a retained volume, data and all; the plan waits for a person.
    Delete {
        /// The volume, `vol_…`, as `korpis get volumes` lists it.
        volume: String,
    },
}

/// Answers `korpis volume …`.
pub async fn volume(c: &Connection, action: &VolumeAction) -> Result<Answer, CliError> {
    let VolumeAction::Delete { volume } = action;
    let request = pb::DeleteVolumeRequest {
        mutation: mutation(None).into(),
        id: volume.clone(),
        ..Default::default()
    };
    let client = pb::VolumeServiceClient::new(c.http(), c.config());
    let plan =
        client.delete_volume(request).await?.into_owned().plan.into_option().unwrap_or_default();
    Ok(Answer { human: show::plan(&plan, &plan.workload), json: json(&plan) })
}
