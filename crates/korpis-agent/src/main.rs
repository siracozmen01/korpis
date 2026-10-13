// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-agent`: join a control plane once, then run.

use clap::{Parser, Subcommand};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options, Reserve, facts_of};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::state::StateDir;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "korpis-agent", version = korpis_core::build::describe("korpis-agent"))]
struct Cli {
    /// Where the agent keeps its identity, journal, and assignments.
    #[arg(long, env = "KORPIS_AGENT_STATE", default_value = "/var/lib/korpis-agent")]
    state_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enroll this machine with a one-time token (§3 of docs/18-operations.md).
    Join {
        /// The control plane, e.g. `https://korpis.example:7443`.
        #[arg(long)]
        server: String,
        /// The enrollment token.
        #[arg(long, env = "KORPIS_JOIN_TOKEN")]
        token: String,
        /// sha256: of the installation CA, as the token's creation printed it.
        #[arg(long)]
        ca_fingerprint: String,
        /// What this machine calls itself; informational.
        #[arg(long, default_value = "")]
        hostname: String,
    },
    /// Run the node: hold the control stream, keep the journal, and reconcile.
    Run {
        /// The runtime driver: `memory` runs nothing, for trying the agent on a machine
        /// without one.
        #[arg(long, env = "KORPIS_AGENT_DRIVER", default_value = "memory")]
        driver: String,
        /// Millicores kept back from placement for the machine's own system.
        #[arg(long, default_value_t = Reserve::default().cpu_millis)]
        reserve_cpu_millis: u64,
        /// Bytes of memory kept back from placement for the machine's own system.
        #[arg(long, default_value_t = Reserve::default().memory_bytes)]
        reserve_memory_bytes: u64,
    },
}

/// Waits for an interrupt or a termination request.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs the agent until it is asked to stop; workloads are left running.
async fn run(state: &StateDir, driver: &str, reserve: Reserve) -> ExitCode {
    if driver != "memory" {
        tracing::error!(%driver, "no such driver in this build");
        return ExitCode::FAILURE;
    }
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let facts = match facts_of(&runtime, &host, reserve).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "the driver could not be described");
            return ExitCode::FAILURE;
        }
    };
    match Agent::start(state, runtime, host, facts, Options::default()) {
        Ok(agent) => {
            tracing::info!(%driver, "agent running");
            shutdown().await;
            agent.stop().await;
            tracing::info!("agent stopped; workloads left as they are");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "the agent could not start");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let state = StateDir::new(cli.state_dir);
    match cli.command {
        Command::Join { server, token, ca_fingerprint, hostname } => {
            match join(&state, &Join { server, token, ca_fingerprint, hostname }).await {
                Ok(id) => {
                    tracing::info!(node = %id.node, "enrolled");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    tracing::error!(error = %e, "join failed");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Run { driver, reserve_cpu_millis, reserve_memory_bytes } => {
            let reserve =
                Reserve { cpu_millis: reserve_cpu_millis, memory_bytes: reserve_memory_bytes };
            run(&state, &driver, reserve).await
        }
    }
}
