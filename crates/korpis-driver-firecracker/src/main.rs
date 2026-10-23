// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-driver-firecracker`: the microvm tier, served to the agent on a local socket.

use clap::Parser;
use korpis_driver_firecracker::machine::Machine;
use korpis_driver_firecracker::service::Driver;
use korpis_driver_firecracker::settings::Settings;
use korpis_driver_kit::serve::{allowed, bind, serve, shutdown};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "korpis-driver-firecracker",
    version = korpis_core::build::describe("korpis-driver-firecracker")
)]
struct Cli {
    /// The socket the agent connects to.
    #[arg(long, default_value = "/run/korpis/driver-firecracker.sock")]
    socket: PathBuf,
    /// Firecracker's binary, of the release its jailer is.
    #[arg(long, default_value = "/usr/lib/korpis/firecracker")]
    firecracker: PathBuf,
    /// Firecracker's jailer.
    #[arg(long, default_value = "/usr/lib/korpis/jailer")]
    jailer: PathBuf,
    /// The guest kernel every guest boots (scripts/guest-kernel.sh).
    #[arg(long, default_value = "/usr/lib/korpis/vmlinux")]
    kernel: PathBuf,
    /// `korpis-guest`, built static.
    #[arg(long, default_value = "/usr/lib/korpis/korpis-guest")]
    guest: PathBuf,
    /// `korpis-image-build`; by default the one installed beside this binary.
    #[arg(long)]
    image_build: Option<PathBuf>,
    /// `korpis-stream-writer`, which writes a guest's console to its stream (§4 of
    /// docs/14-streams.md); by default the one installed beside this binary.
    #[arg(long)]
    stream_writer: Option<PathBuf>,
    /// The node's content store, which the agent fills and the driver only reads.
    #[arg(long, default_value = "/var/lib/korpis-agent/content")]
    content: PathBuf,
    /// The directory the helper keeps volumes under.
    #[arg(long, default_value = "/var/lib/korpis/volumes")]
    volume_root: PathBuf,
    /// The driver's own state: its objects, the drives its guests share, and their jails.
    #[arg(long, default_value = "/var/lib/korpis-driver-firecracker")]
    state_dir: PathBuf,
    /// The cgroup, from the unified hierarchy's root, beneath which each guest's is made.
    #[arg(long, default_value = "korpis-firecracker")]
    cgroup: String,
    /// A user allowed to connect, by uid; repeatable. Without one, only the driver's own user.
    #[arg(long = "allow-uid")]
    allow_uids: Vec<u32>,
}

async fn run(cli: Cli) -> Result<(), String> {
    let allowed = allowed(cli.allow_uids).map_err(|e| format!("own uid: {e}"))?;
    let beside = |name: &str| {
        std::env::current_exe()
            .map(|p| p.with_file_name(name))
            .map_err(|e| format!("own path: {e}"))
    };
    let image_build = cli.image_build.map_or_else(|| beside("korpis-image-build"), Ok)?;
    let writer = cli.stream_writer.map_or_else(|| beside("korpis-stream-writer"), Ok)?;
    let settings = Settings {
        firecracker: cli.firecracker,
        jailer: cli.jailer,
        kernel: cli.kernel,
        guest: cli.guest,
        image_build,
        writer,
        content: cli.content,
        volume_root: cli.volume_root,
        state: cli.state_dir,
        cgroup: cli.cgroup,
    };
    let machine = Machine::new(settings).map_err(|e| format!("the driver's state: {e}"))?;
    // Every guest still running is watched again before the agent asks.
    let found = machine.recover();
    tracing::info!(objects = found.len(), "recovered");
    let listener = bind(&cli.socket).map_err(|e| format!("{}: {e}", cli.socket.display()))?;
    tracing::info!(socket = %cli.socket.display(), ?allowed, "serving");
    serve(listener, allowed, Driver::new(machine), shutdown()).await;
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
