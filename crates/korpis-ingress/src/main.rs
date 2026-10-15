// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-ingress --control <socket>`: connects to the agent, takes the sockets it hands over,
//! confines itself, and serves until the agent closes the connection, when it exits for the
//! agent to start it again.

use clap::Parser;
use korpis_ingress::{Table, confine, control, serve};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, Parser)]
#[command(name = "korpis-ingress", version = korpis_proto_version())]
struct Args {
    /// The agent's socket.
    #[arg(long)]
    control: PathBuf,
}

const fn korpis_proto_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the proxy stopped");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let (stream, listeners) = control::connect(&args.control)?;
    let held = confine::confine()?;
    tracing::info!(
        landlock = held.landlock,
        http = listeners.http.len(),
        https = listeners.https.len(),
        "confined, serving"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        stream.set_nonblocking(true)?;
        let stream = tokio::net::UnixStream::from_std(stream)?;
        let (publish, table) = watch::channel(Arc::new(Table::default()));
        tokio::select! {
            fed = control::configs(stream, &publish) => fed,
            served = serve(listeners, table) => served,
        }
    })?;
    Ok(())
}
