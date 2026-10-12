// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{Parser, Subcommand};
use korpis_app::install::Installation;
use korpis_core::id::{Id, IdKind};
use korpis_core::schedule::PlacementPolicy;
use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::{InstallConfig, install};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// The Korpis control plane.
#[derive(Debug, Parser)]
#[command(name = "korpis-server", version = korpis_core::build::version_line())]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install into an empty database: migrate, create keys and TLS, create the root
    /// organization, and print the root API token once.
    Install {
        /// The data directory for keys and TLS material. Back it up outside Korpis.
        #[arg(long, env = "KORPIS_DATA_DIR")]
        data_dir: PathBuf,
        /// A database URL with schema-owner rights, for migrations.
        #[arg(long, env = "KORPIS_DATABASE_OWNER_URL")]
        database_owner_url: String,
        /// The root organization's name.
        #[arg(long)]
        organization: String,
        /// Names and addresses the server certificate is valid for.
        #[arg(long = "tls-name", default_values = ["localhost", "127.0.0.1"])]
        tls_names: Vec<String>,
    },
    /// Serve the API.
    Serve {
        #[arg(long, env = "KORPIS_DATA_DIR")]
        data_dir: PathBuf,
        #[arg(long, env = "KORPIS_DATABASE_URL")]
        database_url: String,
        #[arg(long, env = "KORPIS_LISTEN", default_value = "127.0.0.1:7443")]
        listen: SocketAddr,
        #[arg(long, default_value_t = 16)]
        max_connections: u32,
        /// A registry to speak plain HTTP to, as host:port; for a development registry on
        /// loopback. Repeatable.
        #[arg(long = "plain-http-registry")]
        plain_http_registries: Vec<String>,
        /// The installation's placement policy: a `korpis.v1.PlacementPolicy` in JSON. Without
        /// it, `balanced` with every weight 1 (§2.2 of docs/05-scheduling.md).
        #[arg(long, env = "KORPIS_PLACEMENT_POLICY")]
        placement_policy: Option<PathBuf>,
        /// Seconds between passes that retry workloads no node could take.
        #[arg(long, default_value_t = 10)]
        placement_retry_secs: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // One TLS provider for the whole process, installed before anything can need one: several
    // dependencies compile rustls with more than one provider, and rustls then refuses to guess.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            fail(&*e);
            ExitCode::FAILURE
        }
    }
}

#[expect(clippy::print_stderr, reason = "a failed command says why on stderr")]
fn fail(e: &dyn std::error::Error) {
    eprintln!("korpis-server: {e}");
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Install { data_dir, database_owner_url, organization, tls_names } => {
            let config = InstallConfig {
                data: DataDir::new(data_dir),
                owner_url: database_owner_url,
                organization: Name::parse(organization)?,
                tls_names,
            };
            let done = install(&config).await?;
            report(&config, &done);
            Ok(())
        }
        Command::Serve {
            data_dir,
            database_url,
            listen,
            max_connections,
            plain_http_registries,
            placement_policy,
            placement_retry_secs,
        } => {
            let policy = match placement_policy {
                Some(path) => korpis_server::placement_policy(&std::fs::read_to_string(&path)?)?,
                None => PlacementPolicy::default(),
            };
            let running = korpis_server::serve(korpis_server::ServeConfig {
                data: DataDir::new(data_dir),
                database_url,
                listen,
                max_connections,
                plain_http_registries,
                placement_policy: policy,
                placement_retry: Duration::from_secs(placement_retry_secs.max(1)),
            })
            .await?;
            tokio::select! {
                r = running.task => r??,
                _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
            }
            Ok(())
        }
    }
}

#[expect(clippy::print_stdout, reason = "the installation report is this command's output")]
fn report(config: &InstallConfig, done: &Installation) {
    let org = Id::from_uuid(IdKind::Organization, done.organization.id);
    println!("Korpis is installed.");
    println!();
    println!("  root organization  {org}");
    println!("  CA certificate     {}", config.data.tls().join("ca.pem").display());
    println!();
    println!("The root API token is shown once. Store it somewhere safe:");
    println!();
    println!("  {}", done.secret.expose());
    println!();
    println!("It holds the root grant. Every other grant descends from it.");
}
