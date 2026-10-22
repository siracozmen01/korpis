// SPDX-License-Identifier: AGPL-3.0-or-later

mod installing;
mod migrating;
mod restoring;
mod serving;
mod sftp_flags;

use clap::{Parser, Subcommand};
use korpis_server::data::DataDir;
use korpis_server::install::{RecoverConfig, invitation_report, recover_root};
use serving::{ServeArgs, serve};
use std::fmt::Write as _;
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
    /// Install into an empty database, once preflight finds nothing essential lacking: migrate,
    /// create keys and TLS, the root organization and the account holding its root grant, the
    /// control plane's own login, its backup declaration, its users and systemd units; print
    /// that account's invitation once, to redeem by registering a passkey within the hour, and
    /// the first node's join command.
    Install(Box<installing::InstallArgs>),
    /// Move the schema (§4 of docs/18-operations.md): apply the migrations this build carries,
    /// or, with --to, undo those after a version, so the binary that ran before can run again.
    Migrate(Box<migrating::MigrateArgs>),
    /// Back up what the control plane holds outside its database, every key and the epoch
    /// watermark, to the target installation declared, sealed with the key it printed; and say
    /// whether PostgreSQL archives the database (§6 of docs/18-operations.md).
    Backup {
        /// The installation's data directory.
        #[arg(long, env = "KORPIS_DATA_DIR", default_value = "/var/lib/korpis-server")]
        data_dir: PathBuf,
        /// The installation's database.
        #[arg(long, env = "KORPIS_DATABASE_URL")]
        database_url: String,
        /// How many backups to keep in the target; older ones are removed.
        #[arg(long, default_value_t = 30)]
        keep: usize,
    },
    /// Restore the control plane from its backup (§6 of docs/18-operations.md), while it is
    /// stopped and after PostgreSQL has restored the database: put the backup's keys back, fence
    /// the epochs, rotate the token key, hold the relays, and record the gap; the next start
    /// opens the window in which the agents present what they hold.
    Restore(Box<restoring::RestoreArgs>),
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
        /// An origin the panel is served from, whose calls are answered cross-origin so a
        /// browser can open sessions here; repeatable.
        #[arg(long = "web-origin", env = "KORPIS_WEB_ORIGIN", value_delimiter = ',')]
        web_origin: Vec<String>,
        #[command(flatten)]
        sftp: sftp_flags::SftpArgs,
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
        Command::Install(args) => installing::run(*args).await,
        Command::Migrate(args) => migrating::run(*args).await,
        Command::Backup { data_dir, database_url, keep } => {
            backed_up(&DataDir::new(data_dir), &database_url, keep).await
        }
        Command::Restore(args) => restoring::run(*args, &operator()).await,
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
        Command::Relay {
            command: RelayCommand::Run { dir, listen, proxy_protocol_from, web_origin, sftp },
        } => {
            let edge = korpis_server::standalone::Edge {
                proxies: proxy_protocol_from,
                web_origins: web_origin,
            };
            relay_run(dir, listen, edge, &sftp).await
        }
    }
}

/// Backs up the control plane and says what it did; fails when the database is not backed up by
/// archiving, after saying what was backed up, so a timer that runs it reports the gap.
async fn backed_up(
    data: &DataDir,
    database_url: &str,
    keep: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let done = korpis_server::control_backup::backup(data, database_url, keep).await?;
    let mut out = format!(
        "Backed up {} files and the epoch watermark ({}, at most {} a second) to {}.\n",
        done.files,
        done.epoch.max_issued,
        done.epoch.issue_rate_per_sec,
        done.path.display()
    );
    for p in &done.pruned {
        let _ = writeln!(out, "Removed {}.", p.display());
    }
    match done.archiving {
        Ok(said) => {
            say(&format!("{out}The database: {said}.\n"));
            Ok(())
        }
        Err(why) => {
            say(&out);
            Err(why.into())
        }
    }
}

/// Runs a joined relay until the process is interrupted, the control plane refuses it, or
/// serving fails.
async fn relay_run(
    dir: PathBuf,
    listen: Option<SocketAddr>,
    edge: korpis_server::standalone::Edge,
    sftp: &sftp_flags::SftpArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let sftp = sftp.bound(&dir).await?;
    let dir = korpis_server::standalone::RelayDir::new(dir);
    let listen = listen.map_or_else(|| default_listen(&dir), Ok)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let running = korpis_server::standalone::run(dir, listener, edge).await?;
    let sftp = match sftp {
        Some((listener, key)) => Some(running.serve_sftp(listener, key)?),
        None => None,
    };
    tokio::select! {
        r = running.task => r??,
        why = sftp_flags::ended(sftp) => return Err(why.into()),
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
