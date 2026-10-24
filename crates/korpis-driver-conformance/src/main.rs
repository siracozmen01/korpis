// SPDX-License-Identifier: Apache-2.0

//! `korpis-driver-conformance`, the runner a third party starts on its own (§8 of
//! docs/04-runtimes.md). Given a fixture file, which says how its driver is started, the socket
//! it serves on, and the node's paths it is started with, it runs every case against the driver,
//! prints each verdict, and writes the report as JSON where it is asked to. It exits 0 when the
//! driver conforms, 1 when a case failed, and 2 when the suite could not run or its report could
//! not be written. It runs as root, since it gives volumes to ids and marks block devices.

use clap::Parser;
use korpis_driver_conformance::{Fixture, Report};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Runs the driver conformance suite against the driver a fixture file names.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// The fixture, in JSON: how the driver is started, its socket, the node's content store and
    /// volume root, the probe, and the ids its objects run as.
    #[arg(long)]
    fixture: PathBuf,
    /// Where the report is written, in JSON.
    #[arg(long)]
    json: Option<PathBuf>,
}

/// Says that the suite could not run, and why.
fn unrun(why: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "korpis-driver-conformance: {why}");
    ExitCode::from(2)
}

/// The fixture the file at `path` holds.
fn fixture(path: &Path) -> Result<Fixture, String> {
    let file = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Fixture::from_json(&file)
}

/// Writes `report` as JSON to `path`.
fn written(report: &Report, path: &Path) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(&report.to_json()).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("the report, {}: {e}", path.display()))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    if !rustix::process::geteuid().is_root() {
        return unrun("the suite runs as root: it gives volumes to ids and marks block devices");
    }
    let fixture = match fixture(&args.fixture) {
        Ok(f) => f,
        Err(e) => return unrun(&e),
    };
    let report = match korpis_driver_conformance::run(fixture).await {
        Ok(r) => r,
        Err(e) => return unrun(&e),
    };
    let _ = writeln!(std::io::stdout().lock(), "{report}");
    if let Some(path) = args.json
        && let Err(e) = written(&report, &path)
    {
        return unrun(&e);
    }
    if report.conforms() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
