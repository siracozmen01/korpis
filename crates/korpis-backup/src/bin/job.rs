// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-backup-job`: one backup or restore job, started by the node's helper with its spec on
//! standard input (§5.5 of docs/06-storage.md). It confines itself before anything else, while it
//! is one thread, then runs, then writes its result where the spec says, and exits 0 when it
//! wrote one, whatever the result says.

use korpis_backup::job::{confine_to, read_spec, run, write_result};
use korpis_proto::korpis::backup::v1 as pb;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let spec = match read_spec(std::io::stdin().lock()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "no job to run");
            return ExitCode::FAILURE;
        }
    };
    let path = PathBuf::from(&spec.result);
    let result = outcome(&spec);
    match write_result(&path, &result) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, job = %spec.id, "the result could not be written");
            ExitCode::FAILURE
        }
    }
}

/// What the job did, or why it did not.
fn outcome(spec: &pb::JobSpec) -> pb::JobResult {
    let failed = |e: String| pb::JobResult { ok: false, error: e, ..Default::default() };
    let confinement = match confine_to(spec) {
        Ok(c) => c,
        Err(e) => return failed(e.to_string()),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => return failed(format!("the runtime: {e}")),
    };
    let mut result = runtime.block_on(run(spec)).unwrap_or_else(|e| failed(e.to_string()));
    result.confinement = confinement.into();
    tracing::info!(job = %spec.id, ok = result.ok, "job ended");
    result
}
