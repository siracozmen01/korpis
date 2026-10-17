// SPDX-License-Identifier: AGPL-3.0-or-later

mod serving;

use clap::{Parser, Subcommand};
use korpis_app::install::RootHolder;
use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::{
    InstallConfig, RecoverConfig, install, installed_report, invitation_report, recover_root,
};
use serving::{ServeArgs, serve};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// organization and the account holding its root grant, and print that account's
    /// invitation once, to redeem by registering a passkey within the hour.
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
        /// Issue the root grant to an API token instead, printed once: a bearer secret with no
        /// second factor, for an unattended install or a test (§9.1 of docs/08-identity.md).
        #[arg(long)]
        root_token: bool,
    },
    /// Administration done at the machine, by its root user.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Serve the API.
    Serve(Box<ServeArgs>),
    /// A relay of the data path running as its own process (§3.1 of docs/02-architecture.md).
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
}

/// What `serve` is told.
#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Issue a new root grant to a new account and print its invitation, to redeem by
    /// registering a passkey within the hour, when every holder of root is locked out (§9.2 of
    /// docs/08-identity.md). Recorded as the bootstrap actor and the user who ran it.
    RecoverRoot {
        /// The installation's data directory.
        #[arg(long, env = "KORPIS_DATA_DIR")]
        data_dir: PathBuf,
        /// The installation's database.
        #[arg(long, env = "KORPIS_DATABASE_URL")]
        database_url: String,
    },
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    /// Join the control plane once, with an enrollment token from
    /// `korpis call korpis.v1.NodeService/CreateRelayEnrollmentToken`.
    Join {
        /// Where the relay keeps its keys, certificate, and identity.
        #[arg(long, env = "KORPIS_RELAY_DIR", default_value = "/var/lib/korpis-relay")]
        dir: PathBuf,
        /// The control plane, e.g. `https://korpis.example:7443`.
        #[arg(long)]
        server: String,
        /// The enrollment token.
        #[arg(long, env = "KORPIS_JOIN_TOKEN")]
        token: String,
        /// The fingerprint of the installation's authority, as the token's creation printed it.
        #[arg(long)]
        ca_fingerprint: String,
    },
    /// Serve as the relay that joined, verifying by what the control plane feeds it.
    Run {
        /// Where the relay keeps its keys, certificate, and identity.
        #[arg(long, env = "KORPIS_RELAY_DIR", default_value = "/var/lib/korpis-relay")]
        dir: PathBuf,
        /// Where to listen; by default every IPv4 address, on the port of the relay's URL.
        #[arg(long, env = "KORPIS_LISTEN")]
        listen: Option<SocketAddr>,
        /// A load balancer whose connections open with a PROXY header (version 1 or 2) naming
        /// the client, as a CIDR; repeatable. Connections from anywhere else are their own client.
        #[arg(
            long = "proxy-protocol-from",
            env = "KORPIS_PROXY_PROTOCOL_FROM",
            value_delimiter = ','
        )]
        proxy_protocol_from: Vec<ipnet::IpNet>,
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
        Command::Install { data_dir, database_owner_url, organization, tls_names, root_token } => {
            let config = InstallConfig {
                data: DataDir::new(data_dir),
                owner_url: database_owner_url,
                organization: Name::parse(organization)?,
                tls_names,
                root: if root_token { RootHolder::Token } else { RootHolder::Passkey },
            };
            let done = install(&config).await?;
            say(&installed_report(&config, &done));
            Ok(())
        }
        Command::Admin { command: AdminCommand::RecoverRoot { data_dir, database_url } } => {
            let config = RecoverConfig { data: DataDir::new(data_dir), database_url };
            let invitation = recover_root(&config, &operator()).await?;
            say(&invitation_report(&invitation));
            Ok(())
        }
        Command::Serve(args) => serve(*args).await,
        Command::Relay { command: RelayCommand::Join { dir, server, token, ca_fingerprint } } => {
            let dir = korpis_server::standalone::RelayDir::new(dir);
            let id =
                korpis_server::standalone::join(&dir, &server, &token, &ca_fingerprint).await?;
            tracing::info!(relay = %id, "joined");
            Ok(())
        }
        Command::Relay { command: RelayCommand::Run { dir, listen, proxy_protocol_from } } => {
            relay_run(dir, listen, proxy_protocol_from).await
        }
    }
}

/// Runs a joined relay until the process is interrupted, the control plane refuses it, or
/// serving fails.
async fn relay_run(
    dir: PathBuf,
    listen: Option<SocketAddr>,
    proxies: Vec<ipnet::IpNet>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = korpis_server::standalone::RelayDir::new(dir);
    let listen = listen.map_or_else(|| default_listen(&dir), Ok)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let running = korpis_server::standalone::run(dir, listener, proxies).await?;
    tokio::select! {
        r = running.task => r??,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}

/// Every IPv4 address, on the port of the relay's URL.
fn default_listen(
    dir: &korpis_server::standalone::RelayDir,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let url = korpis_server::standalone::url(dir)?;
    let port = korpis_core::relay::RelayUrl::parse(&url)?.port();
    Ok(SocketAddr::from(([0, 0, 0, 0], port)))
}

#[expect(clippy::print_stdout, reason = "a command's answer is its output")]
fn say(text: &str) {
    print!("{text}");
}

/// The operating-system user running this, as the effect of a recovery names them: the user
/// `sudo` was run by, if it was, and the user it runs as.
fn operator() -> String {
    use std::os::unix::fs::MetadataExt as _;
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid().to_string());
    let uid = uid.unwrap_or_else(|_| "unknown".to_owned());
    let user = std::env::var("USER").unwrap_or_default();
    match std::env::var("SUDO_USER") {
        Ok(by) => format!("uid {uid} ({user}), by sudo from {by}"),
        Err(_) => format!("uid {uid} ({user})"),
    }
}
