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
    Serve(ServeArgs),
    /// A relay of the data path running as its own process (§3.1 of docs/02-architecture.md).
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
}

/// What `serve` is told.
#[derive(Debug, clap::Args)]
struct ServeArgs {
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
    /// A network a registry a recipe names may be on besides the public internet, as a CIDR, for
    /// a private mirror; repeatable. Loopback, private, and link-local addresses are otherwise
    /// refused (§3 of docs/17-security.md).
    #[arg(long = "fetch-allow")]
    fetch_allow: Vec<ipnet::IpNet>,
    /// The installation's placement policy: a `korpis.v1.PlacementPolicy` in JSON. Without
    /// it, `balanced` with every weight 1 (§2.2 of docs/05-scheduling.md).
    #[arg(long, env = "KORPIS_PLACEMENT_POLICY")]
    placement_policy: Option<PathBuf>,
    /// Seconds between passes that retry workloads no node could take.
    #[arg(long, default_value_t = 10)]
    placement_retry_secs: u64,
    /// Seconds between re-reads of a watched workload when nothing was announced.
    #[arg(long, default_value_t = 15)]
    watch_resync_secs: u64,
    /// Seconds between agent heartbeats; a node missing three is unreachable (question 6
    /// of §10 of docs/02-architecture.md).
    #[arg(long, default_value_t = 30)]
    agent_heartbeat_secs: u64,
    /// Seconds a lease lasts from the holder's request; a handoff from an unreachable node
    /// waits this long (§4.5 of docs/02-architecture.md).
    #[arg(long, default_value_t = 60)]
    lease_term_secs: u64,
    /// Seconds over which repeated identical denials are written as one effect with a
    /// count; the first of each run is written at once (§10 of docs/10-api.md).
    #[arg(long, default_value_t = 60)]
    denial_window_secs: u64,
    /// Days the event log keeps an event; a subscriber further behind is told of the gap
    /// (§5 of docs/15-observability.md). Effects are kept regardless.
    #[arg(long, default_value_t = 30)]
    event_retention_days: u64,
    /// Days below which no workload's offloaded console segments are deleted, whatever it
    /// declares; every tenant is shown it with the stream (§8 of docs/17-security.md). Zero is
    /// none.
    #[arg(long, default_value_t = 0)]
    stream_retention_floor_days: u64,
    /// Whether this process serves the relay role, carrying console and file sessions; turn it
    /// off where relays run as their own processes (§3.1 of docs/02-architecture.md).
    #[arg(long, env = "KORPIS_RELAY_ROLE", default_value_t = true, action = clap::ArgAction::Set)]
    relay_role: bool,
    /// A load balancer whose connections open with a PROXY header (version 1 or 2) naming the
    /// client, as a CIDR; repeatable. Connections from anywhere else are their own client.
    #[arg(long = "proxy-protocol-from", env = "KORPIS_PROXY_PROTOCOL_FROM", value_delimiter = ',')]
    proxy_protocol_from: Vec<ipnet::IpNet>,
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
        Command::Serve(args) => serve(args).await,
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

/// Serves the API until the process is interrupted or the server fails.
async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let policy = match args.placement_policy {
        Some(path) => korpis_server::placement_policy(&std::fs::read_to_string(&path)?)?,
        None => PlacementPolicy::default(),
    };
    let running = korpis_server::serve(korpis_server::ServeConfig {
        data: DataDir::new(args.data_dir),
        database_url: args.database_url,
        listen: args.listen,
        max_connections: args.max_connections,
        plain_http_registries: args.plain_http_registries,
        fetch_allow: args.fetch_allow,
        placement_policy: policy,
        placement_retry: Duration::from_secs(args.placement_retry_secs.max(1)),
        watch_resync: Duration::from_secs(args.watch_resync_secs.max(1)),
        heartbeat_interval: Duration::from_secs(args.agent_heartbeat_secs.max(1)),
        lease_term: Duration::from_secs(args.lease_term_secs.max(1)),
        denial_window: Duration::from_secs(args.denial_window_secs.max(1)),
        event_retention: Duration::from_secs(args.event_retention_days.max(1) * 86_400),
        stream_retention_floor: Duration::from_secs(
            args.stream_retention_floor_days.saturating_mul(86_400),
        ),
        relay_role: args.relay_role,
        proxy_protocol_from: args.proxy_protocol_from,
    })
    .await?;
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
