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
    /// Declares a new workload like the one a backup is of, its backed-up volume created holding
    /// what the backup held: how a restore is tested.
    Clone {
        /// The backup, `bkp_…`.
        backup: String,
        /// The new workload's name.
        #[arg(long)]
        name: String,
        /// Its project, `prj_…`; the source's when not given.
        #[arg(long)]
        project: Option<String>,
    },
    /// Lists one directory of a backup: names, kinds, and sizes, and no byte of any file.
    Ls {
        /// The backup, `bkp_…`.
        backup: String,
        /// A directory in it; the volume's root when not given.
        #[arg(default_value = "")]
        path: String,
    },
    /// Writes one file of a backup to standard output, or to `--output`.
    Cat {
        /// The backup, `bkp_…`.
        backup: String,
        /// The file in it.
        path: String,
        /// Where to write it instead of standard output; it must not exist.
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
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
    /// Proposes pruning a workload's backups in a repository by its retention policy: the plan
    /// names each backup it forgets and why every other one is kept, and waits for approval.
    Prune {
        /// The workload, by name or identifier.
        workload: String,
        /// The repository, `rep_…`.
        #[arg(long)]
        repository: String,
    },
}

/// A backup's identifier as it is shown, from the one a step's detail holds.
fn backup_id(raw: &str) -> String {
    uuid::Uuid::parse_str(raw).map_or_else(
        |_| raw.to_owned(),
        |u| korpis_wire::wire_id(korpis_core::id::IdKind::Backup, u),
    )
}

/// What a prune plan forgets and keeps, from its step's detail.
fn pruned(plan: &pb::Plan) -> String {
    use pb::__buffa::oneof::value::Kind;
    let Some(step) = plan.steps.first() else { return String::new() };
    let text = |v: &pb::Value| match &v.kind {
        Some(Kind::StringValue(s)) => s.clone(),
        _ => String::new(),
    };
    let list = |v: Option<&pb::Value>| match v.and_then(|v| v.kind.as_ref()) {
        Some(Kind::ListValue(l)) => l.values.iter().map(text).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut lines: Vec<String> = list(step.detail.get("forget"))
        .iter()
        .map(|b| format!("forget {}", backup_id(b)))
        .collect();
    if let Some(Kind::MapValue(kept)) = step.detail.get("kept").and_then(|v| v.kind.as_ref()) {
        lines.extend(
            kept.fields.iter().map(|(b, why)| {
                format!("keep   {} ({})", backup_id(b), list(Some(why)).join(", "))
            }),
        );
    }
    lines.join("\n")
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

fn listing(entries: &[pb::BackupEntry]) -> String {
    let mut all = vec![["KIND", "SIZE", "MODIFIED", "NAME"].map(str::to_owned).to_vec()];
    all.extend(entries.iter().map(|e| {
        let mut name = String::from_utf8_lossy(&e.name).into_owned();
        if !e.link_target.is_empty() {
            name = format!("{name} -> {}", String::from_utf8_lossy(&e.link_target));
        }
        vec![
            short(e.kind, "BACKUP_ENTRY_KIND_").to_lowercase(),
            show::bytes(e.size),
            time(e.modified.as_option()),
            name,
        ]
    }));
    table(&all)
}

/// Writes file `path` of `backup` to `out`, as the control plane streams it; its length. A
/// stream that ends in an error leaves what was written and returns the error.
///
/// # Errors
/// The call failed, the stream ended in an error, or `out` could not be written.
pub async fn cat(
    c: &Connection,
    (backup, path): (&str, &str),
    out: &mut dyn std::io::Write,
) -> Result<u64, CliError> {
    let client = pb::BackupServiceClient::new(c.http(), c.config());
    let r = pb::ReadBackupFileRequest {
        backup: backup.to_owned(),
        path: path.to_owned(),
        ..Default::default()
    };
    let mut stream = client.read_backup_file(r).await?;
    let mut written = 0u64;
    while let Some(piece) = stream.message().await? {
        let piece = piece.to_owned_message();
        out.write_all(&piece.data).map_err(|e| CliError::Local(e.to_string()))?;
        written += u64::try_from(piece.data.len()).unwrap_or(u64::MAX);
    }
    out.flush().map_err(|e| CliError::Local(e.to_string()))?;
    Ok(written)
}

/// The request `korpis backup restore` makes.
fn restore_request(action: &BackupAction) -> pb::RestoreBackupRequest {
    use pb::__buffa::oneof::restore_backup_request::Target;
    let BackupAction::Restore { backup, into, replace, volume, path } = action else {
        return pb::RestoreBackupRequest::default();
    };
    let target = match into {
        Some(dir) => Target::Into(dir.clone()),
        None => Target::Replace(*replace),
    };
    pb::RestoreBackupRequest {
        mutation: mutation(None).into(),
        backup: backup.clone(),
        volume: volume.clone().unwrap_or_default(),
        path: path.clone().unwrap_or_default(),
        target: Some(target),
        ..Default::default()
    }
}

/// Answers `korpis backup prune`: the plan, and what it forgets and keeps.
async fn prune(c: &Connection, workload: &str, repository: &str) -> Result<Answer, CliError> {
    let client = pb::BackupServiceClient::new(c.http(), c.config());
    let r = pb::PruneBackupsRequest {
        mutation: mutation(None).into(),
        workload: resolve::workload(c, workload).await?,
        repository: repository.to_owned(),
        ..Default::default()
    };
    let made = client.prune_backups(r).await?.into_owned();
    let plan = made.plan.into_option().unwrap_or_default();
    let human = format!("{}\n{}", show::plan(&plan, &plan.workload), pruned(&plan));
    Ok(Answer { human, json: json(&plan) })
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
        BackupAction::Restore { .. } => {
            let made = client.restore_backup(restore_request(action)).await?.into_owned();
            let plan = made.plan.into_option().unwrap_or_default();
            Answer { human: show::plan(&plan, &plan.workload), json: json(&plan) }
        }
        BackupAction::Prune { workload, repository } => prune(c, workload, repository).await?,
        BackupAction::Ls { backup, path } => {
            let r = pb::BrowseBackupRequest {
                backup: backup.clone(),
                path: path.clone(),
                ..Default::default()
            };
            let entries = client.browse_backup(r).await?.into_owned().entries;
            Answer { human: listing(&entries), json: json(&entries) }
        }
        BackupAction::Clone { .. } | BackupAction::Cat { .. } => {
            return Err(CliError::Local("answered by its own command".into()));
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
