// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-driver-oci`: the container tier, served to the agent on a local socket.

use clap::Parser;
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::serve::{bind, serve};
use korpis_driver_oci::service::Driver;
use korpis_driver_oci::validate::Policy;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "korpis-driver-oci", version = korpis_core::build::describe("korpis-driver-oci"))]
struct Cli {
    /// The socket the agent connects to.
    #[arg(long, env = "KORPIS_DRIVER_OCI_SOCKET", default_value = "/run/korpis/driver-oci.sock")]
    socket: PathBuf,
    /// containerd's socket.
    #[arg(long, default_value = "/run/containerd/containerd.sock")]
    containerd: PathBuf,
    /// The node's content store, which the agent fills and the driver only reads.
    #[arg(long, default_value = "/var/lib/korpis-agent/content")]
    content: PathBuf,
    /// The directory the helper keeps volumes under.
    #[arg(long, default_value = "/var/lib/korpis/volumes")]
    volume_root: PathBuf,
    /// The directory the agent makes workload identity sockets under; with the volume root,
    /// the only host paths ever mounted.
    #[arg(long, default_value = "/var/lib/korpis-agent/identity")]
    identity_root: PathBuf,
    /// A user allowed to connect, by uid; repeatable. Without one, only the driver's own user.
    #[arg(long = "allow-uid")]
    allow_uids: Vec<u32>,
}

/// Waits for an interrupt or a termination request.
async fn shutdown() {
    use tokio::signal::unix::{SignalKind, signal};
    if let Ok(mut term) = signal(SignalKind::terminate()) {
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// The users allowed to connect: those named, or else the driver's own.
fn allowed(named: Vec<u32>) -> std::io::Result<BTreeSet<u32>> {
    if named.is_empty() {
        return Ok(BTreeSet::from([std::fs::metadata("/proc/self")?.uid()]));
    }
    Ok(named.into_iter().collect())
}

async fn run(cli: Cli) -> Result<(), String> {
    let allowed = allowed(cli.allow_uids).map_err(|e| format!("own uid: {e}"))?;
    let containerd = Containerd::connect(&cli.containerd, &cli.content)
        .await
        .map_err(|e| format!("containerd at {}: {e}", cli.containerd.display()))?;
    let listener = bind(&cli.socket).map_err(|e| format!("{}: {e}", cli.socket.display()))?;
    let (driver, watcher) = Driver::new(
        containerd,
        Policy { volume_root: cli.volume_root, identity_root: cli.identity_root },
    );
    tracing::info!(socket = %cli.socket.display(), ?allowed, "serving");
    serve(listener, allowed, driver, shutdown()).await;
    watcher.abort();
    let _ = std::fs::remove_file(&cli.socket);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the driver stopped");
            ExitCode::FAILURE
        }
    }
}
