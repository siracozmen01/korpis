// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis backup …` (§5 of docs/06-storage.md): taking a backup of a volume into a repository,
//! reading a workload's backups, and restoring one.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{self, short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis backup …`.
#[derive(Debug, Clone, Subcommand)]
pub enum BackupAction {
    /// Backs a volume up into a repository; the plan applies at once.
    Create {
        /// The volume, `vol_…`, as `korpis get volumes` lists it.
        volume: String,
        /// The repository, `rep_…`.
        #[arg(long)]
        repository: String,
    },
    /// A workload's backups, newest first.
    List {
        /// The workload, by name or identifier.
        workload: String,
        /// Pruned ones too.
        #[arg(long)]
        deleted: bool,
    },
    /// One backup.
    Get {
        /// The backup, `bkp_…`.
        backup: String,
    },
    /// Restores a backup: into a new directory of a volume, which applies at once, or over the
    /// whole volume, which loses what it holds and waits for approval.
    #[command(group(clap::ArgGroup::new("target").required(true).args(["into", "replace"])))]
    Restore {
        /// The backup, `bkp_…`.
        backup: String,
        /// A new directory at the volume's root to restore into.
        #[arg(long)]
        into: Option<String>,
        /// Replace the whole volume with the whole backup.
        #[arg(long)]
        replace: bool,
        /// The volume restored into, `vol_…`; the backup's own when not given.
        #[arg(long)]
        volume: Option<String>,
        /// One path in the backup, with `--into`; all of it when not given.
        #[arg(long, requires = "into")]
        path: Option<String>,
    },
}

fn rows(list: &[pb::Backup]) -> String {
    let mut all = vec![
        ["ID", "VOLUME", "STATUS", "CONSISTENT", "BYTES", "STORED", "TAKEN"]
            .map(str::to_owned)
            .to_vec(),
    ];
    all.extend(list.iter().map(|b| {
        vec![
            b.id.clone(),
            b.volume.clone(),
            short(b.status, "BACKUP_STATUS_").to_lowercase(),
            if b.consistent { "snapshot".into() } else { "live".into() },
            show::bytes(b.bytes),
            show::bytes(b.stored_bytes),
            time(b.created.as_option()),
        ]
    }));
    table(&all)
}

/// Answers `korpis backup …`.
pub async fn backup(c: &Connection, action: &BackupAction) -> Result<Answer, CliError> {
    let client = pb::BackupServiceClient::new(c.http(), c.config());
    Ok(match action {
        BackupAction::Create { volume, repository } => {
            let r = pb::CreateBackupRequest {
                mutation: mutation(None).into(),
                volume: volume.clone(),
                repository: repository.clone(),
                ..Default::default()
            };
            let made = client.create_backup(r).await?.into_owned();
            let plan = made.plan.into_option().unwrap_or_default();
            let id = made.backup.into_option().unwrap_or_default().id;
            Answer {
                human: format!("backup {id}\n{}", show::plan(&plan, &plan.workload)),
                json: json(&plan),
            }
        }
        BackupAction::List { workload, deleted } => {
            let r = pb::ListBackupsRequest {
                workload: resolve::workload(c, workload).await?,
                include_deleted: *deleted,
                ..Default::default()
            };
            let list = client.list_backups(r).await?.into_owned().backups;
            Answer { human: rows(&list), json: json(&list) }
        }
        BackupAction::Restore { backup, into, replace, volume, path } => {
            use pb::__buffa::oneof::restore_backup_request::Target;
            let target = match into {
                Some(dir) => Target::Into(dir.clone()),
                None => Target::Replace(*replace),
            };
            let r = pb::RestoreBackupRequest {
                mutation: mutation(None).into(),
                backup: backup.clone(),
                volume: volume.clone().unwrap_or_default(),
                path: path.clone().unwrap_or_default(),
                target: Some(target),
                ..Default::default()
            };
            let made = client.restore_backup(r).await?.into_owned();
            let plan = made.plan.into_option().unwrap_or_default();
            Answer { human: show::plan(&plan, &plan.workload), json: json(&plan) }
        }
        BackupAction::Get { backup } => {
            let r = pb::GetBackupRequest { id: backup.clone(), ..Default::default() };
            let got =
                client.get_backup(r).await?.into_owned().backup.into_option().unwrap_or_default();
            let mut lines = vec![rows(std::slice::from_ref(&got))];
            if !got.error.is_empty() {
                lines.push(format!("error: {}", got.error));
            }
            lines.extend(got.skipped.iter().map(|s| format!("skipped: {s}")));
            let human = lines.join("\n");
            Answer { human, json: json(&got) }
        }
    })
}
