// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-driver-oci`: the container tier, served to the agent on a local socket.

use clap::Parser;
use korpis_driver_kit::serve::{allowed, bind, serve, shutdown};
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::containerd::stream::Streams;
use korpis_driver_oci::service::Driver;
use korpis_driver_oci::validate::Policy;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
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
    /// The directory the agent makes workload identity sockets under; with the volume root and
    /// the `resolv.conf` of a workload's network directory, the only host paths ever mounted.
    #[arg(long, default_value = "/var/lib/korpis-agent/identity")]
    identity_root: PathBuf,
    /// The directory the helper binds workloads' network namespaces under.
    #[arg(long, default_value = "/run/korpis/netns")]
    netns_root: PathBuf,
    /// A user allowed to connect, by uid; repeatable. Without one, only the driver's own user.
    #[arg(long = "allow-uid")]
    allow_uids: Vec<u32>,
    /// The stream writer containerd starts beside each task (§4 of docs/14-streams.md); by
    /// default the `korpis-stream-writer` installed beside this binary.
    #[arg(long, env = "KORPIS_STREAM_WRITER")]
    stream_writer: Option<PathBuf>,
    /// The driver's own state: the files it hands each task's stream writer.
    #[arg(long, default_value = "/var/lib/korpis-driver-oci")]
    state_dir: PathBuf,
}

/// The stream writer: the one named, or the one beside this binary; it must be there, since a
/// task whose logging binary is missing never starts, and one with none loses its console.
fn stream_writer(named: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = match named {
        Some(p) => p,
        None => std::env::current_exe()
            .map_err(|e| format!("own path: {e}"))?
            .with_file_name("korpis-stream-writer"),
    };
    let executable = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0);
    if !executable {
        return Err(format!("no stream writer at {}: pass --stream-writer", path.display()));
    }
    Ok(path)
}

async fn run(cli: Cli) -> Result<(), String> {
    let allowed = allowed(cli.allow_uids).map_err(|e| format!("own uid: {e}"))?;
    let streams = Streams { writer: stream_writer(cli.stream_writer)?, state: cli.state_dir };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&streams.state)
        .map_err(|e| format!("{}: {e}", streams.state.display()))?;
    let containerd = Containerd::connect(&cli.containerd, &cli.content)
        .await
        .map_err(|e| format!("containerd at {}: {e}", cli.containerd.display()))?
        .with_streams(streams);
    let listener = bind(&cli.socket).map_err(|e| format!("{}: {e}", cli.socket.display()))?;
    let (driver, watcher) = Driver::new(
        containerd,
        Policy {
            volume_root: cli.volume_root,
            identity_root: cli.identity_root,
            netns_root: cli.netns_root,
        },
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
