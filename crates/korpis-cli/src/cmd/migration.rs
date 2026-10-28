// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis workload move` and `korpis migration …` (§8.1 of docs/05-scheduling.md, §2 of
//! docs/13-surface-cli.md): proposing that a workload move to another node, reading its
//! migrations, and ending one.

use super::{Answer, json, mutation, seconds};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{self, short, table, time};
use buffa_types::google::protobuf::Duration;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis migration …`.
#[derive(Debug, Clone, Subcommand)]
pub enum MigrationAction {
    /// A workload's migrations, newest first, or one of them with its volumes.
    Get {
        /// The workload.
        workload: String,
        /// One migration of it, by its identifier (`mig_…`).
        migration: Option<String>,
    },
    /// Ends a migration before its cutover: the workload stays where it is.
    Abandon {
        /// The migration (`mig_…`).
        migration: String,
        /// Why, for the record.
        #[arg(long)]
        reason: String,
    },
    /// Moves the workload back after its cutover, to what it held when it stopped there; what
    /// it wrote since is kept as divergent copies of its volumes.
    Rollback {
        /// The migration (`mig_…`).
        migration: String,
        /// Why, for the record.
        #[arg(long)]
        reason: String,
        /// Rolling back stops the workload again; say so.
        #[arg(long)]
        yes: bool,
    },
    /// Ends the hold now: the source's copies are deleted, and nothing can roll it back.
    Release {
        /// The migration (`mig_…`).
        migration: String,
        /// Releasing deletes the source's copies; say so.
        #[arg(long)]
        yes: bool,
    },
}

/// Answers `korpis workload move`: the plan, which a person approves.
pub async fn moved(
    c: &Connection,
    workload: String,
    to: Option<&str>,
    hold: Option<&str>,
) -> Result<Answer, CliError> {
    let node = match to {
        Some(n) => resolve::node(c, n).await?,
        None => String::new(),
    };
    let hold =
        hold.map(seconds).transpose()?.map(|s| Duration { seconds: s, ..Default::default() });
    let request = pb::MigrateWorkloadRequest {
        mutation: mutation(None).into(),
        workload,
        node,
        hold: hold.into(),
        ..Default::default()
    };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let p = client.migrate_workload(request).await?.into_owned().plan.into_option();
    let p = p.unwrap_or_default();
    Ok(Answer { human: show::plan(&p, &p.workload), json: json(&p) })
}

/// One migration, with each of its volumes.
fn one(m: &pb::Migration) -> String {
    let mut out = format!(
        "migration {} of {}: {} since {}\nfrom {} to {}, held {}s after cutover{}\n",
        m.id,
        m.workload,
        short(m.phase, "MIGRATION_PHASE_"),
        time(m.phase_since.as_option()),
        m.source,
        m.destination,
        m.hold.as_option().map_or(0, |d| d.seconds),
        match m.hold_until.as_option() {
            Some(t) => format!(", until {}", time(Some(t))),
            None => String::new(),
        }
    );
    if m.outcome.as_known() != Some(pb::MigrationOutcome::MIGRATION_OUTCOME_UNSPECIFIED) {
        let how = short(m.outcome, "MIGRATION_OUTCOME_");
        out.push_str(&how);
        out.push_str(": ");
        out.push_str(&m.reason);
        out.push('\n');
    }
    let mut rows = vec![
        vec!["VOLUME", "CROSSES", "SIZE", "PASSES", "LAST SENT", "VERIFIED"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    ];
    for v in &m.volumes {
        let verified = match (v.verified, v.first_difference.is_empty()) {
            (true, _) => "yes".to_owned(),
            (false, true) => "-".to_owned(),
            (false, false) => format!("no: {}", v.first_difference),
        };
        rows.push(vec![
            v.name.clone(),
            short(v.crossing, "MIGRATION_CROSSING_"),
            v.size_bytes.to_string(),
            v.passes.to_string(),
            v.sent_bytes.to_string(),
            verified,
        ]);
    }
    out.push_str(&table(&rows));
    out
}

/// The migrations of a workload, one line each.
fn rows(list: &[pb::Migration]) -> String {
    let mut rows = vec![
        vec!["MIGRATION", "PHASE", "SINCE", "FROM", "TO", "OUTCOME"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    ];
    for m in list {
        rows.push(vec![
            m.id.clone(),
            short(m.phase, "MIGRATION_PHASE_"),
            time(m.phase_since.as_option()),
            m.source.clone(),
            m.destination.clone(),
            short(m.outcome, "MIGRATION_OUTCOME_").replace("unspecified", "-"),
        ]);
    }
    table(&rows)
}

/// Answers `korpis migration get|abandon|rollback|release`.
pub async fn migration(c: &Connection, action: &MigrationAction) -> Result<Answer, CliError> {
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let ended = match action {
        MigrationAction::Get { workload, migration } => {
            let workload = resolve::workload(c, workload).await?;
            let r = pb::ListMigrationsRequest { workload, ..Default::default() };
            let list = client.list_migrations(r).await?.into_owned().migrations;
            return Ok(match migration {
                Some(id) => {
                    let m = list.into_iter().find(|m| &m.id == id).ok_or_else(|| {
                        CliError::Local(format!("{id} is not a migration of the workload"))
                    })?;
                    Answer { human: one(&m), json: json(&m) }
                }
                None => Answer { human: rows(&list), json: json(&list) },
            });
        }
        MigrationAction::Abandon { migration, reason } => {
            let r = pb::AbandonMigrationRequest {
                mutation: mutation(None).into(),
                migration: migration.clone(),
                reason: reason.clone(),
                ..Default::default()
            };
            client.abandon_migration(r).await?.into_owned().migration
        }
        MigrationAction::Rollback { migration, reason, yes } => {
            if !yes {
                let why = "rolling back stops the workload again; pass --yes to roll it back";
                return Err(CliError::Local(why.into()));
            }
            let r = pb::RollbackMigrationRequest {
                mutation: mutation(None).into(),
                migration: migration.clone(),
                reason: reason.clone(),
                ..Default::default()
            };
            client.rollback_migration(r).await?.into_owned().migration
        }
        MigrationAction::Release { migration, yes } => {
            if !yes {
                let why = "releasing deletes the source's copies; pass --yes to release them";
                return Err(CliError::Local(why.into()));
            }
            let r = pb::ReleaseMigrationRequest {
                mutation: mutation(None).into(),
                migration: migration.clone(),
                ..Default::default()
            };
            client.release_migration(r).await?.into_owned().migration
        }
    };
    let m = ended.into_option().unwrap_or_default();
    Ok(Answer { human: one(&m), json: json(&m) })
}
