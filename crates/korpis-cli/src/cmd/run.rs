// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis run …` (§4 of docs/05-scheduling.md): reading a task or scheduled workload's runs and
//! their consoles, and cancelling one. A run is requested with `korpis workload restart`, since a higher power cycle
//! is what requests one.

use super::logs::{Out, Reading};
use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis run …`.
#[derive(Debug, Clone, Subcommand)]
pub enum RunAction {
    /// A workload's runs, newest first.
    List {
        /// The workload, by name or identifier.
        workload: String,
    },
    /// One run.
    Get {
        /// The run, `run_…`.
        run: String,
    },
    /// Cancels an active run; its node stops it.
    Cancel {
        /// The run, `run_…`.
        run: String,
    },
    /// A run's console, which each run writes its own of: read from its node, or, with
    /// `--offloaded`, from the repository that takes its workload's stream.
    Logs {
        /// The run, `run_…`.
        run: String,
        /// How to read it.
        #[command(flatten)]
        reading: Reading,
    },
}

/// Why a run ended, or its exit status beside it.
fn ended(r: &pb::Run) -> String {
    let cause = short(r.cause, "RUN_CAUSE_").to_lowercase();
    match (r.exit_status, cause.as_str()) {
        (Some(code), _) => format!("{cause} {code}"),
        (None, "unspecified") => String::new(),
        (None, _) => cause,
    }
}

fn rows(list: &[pb::Run]) -> String {
    let mut all = vec![
        ["ID", "TRIGGER", "STATUS", "ENDED", "FOR", "STARTED", "ATTEMPT"]
            .map(str::to_owned)
            .to_vec(),
    ];
    all.extend(list.iter().map(|r| {
        let mut due = time(r.scheduled_for.as_option());
        if r.occurrences > 1 {
            due = format!("{due} (+{} more)", r.occurrences - 1);
        }
        vec![
            r.id.clone(),
            short(r.trigger, "RUN_TRIGGER_").to_lowercase(),
            short(r.status, "RUN_STATUS_").to_lowercase(),
            ended(r),
            due,
            time(r.started_at.as_option()),
            r.attempt.to_string(),
        ]
    }));
    table(&all)
}

/// Runs `action`.
///
/// # Errors
/// The control plane refused, or could not be reached.
pub async fn run(c: &Connection, action: &RunAction) -> Result<Answer, CliError> {
    let client = pb::RunServiceClient::new(c.http(), c.config());
    let one = |r: pb::Run| Answer { human: rows(std::slice::from_ref(&r)), json: json(&r) };
    Ok(match action {
        RunAction::List { workload } => {
            let r = pb::ListRunsRequest {
                workload: resolve::workload(c, workload).await?,
                ..Default::default()
            };
            let list = client.list_runs(r).await?.into_owned().runs;
            Answer { human: rows(&list), json: json(&list) }
        }
        RunAction::Get { run } => {
            let r = pb::GetRunRequest { id: run.clone(), ..Default::default() };
            one(client.get_run(r).await?.into_owned().run.into_option().unwrap_or_default())
        }
        RunAction::Logs { .. } => {
            return Err(CliError::Local("a run's console is printed as it is read".into()));
        }
        RunAction::Cancel { run } => {
            let r = pb::CancelRunRequest {
                mutation: mutation(None).into(),
                run: run.clone(),
                ..Default::default()
            };
            one(client.cancel_run(r).await?.into_owned().run.into_option().unwrap_or_default())
        }
    })
}

/// Prints `run`'s console as `reading` asks, handing each line to `out`: the run is looked up
/// for its workload, through which its console is read.
///
/// # Errors
/// The control plane refused, no node or repository could be reached, or the session was lost.
pub async fn logs(
    c: &Connection,
    run: &str,
    reading: &Reading,
    out: Out<'_>,
) -> Result<(), CliError> {
    let client = pb::RunServiceClient::new(c.http(), c.config());
    let r = pb::GetRunRequest { id: run.to_owned(), ..Default::default() };
    let found = client.get_run(r).await?.into_owned().run.into_option().unwrap_or_default();
    super::logs::read(c, (&found.workload, Some(&found.id)), reading, out).await
}
