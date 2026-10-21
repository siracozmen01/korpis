// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server serve`: its flags, and the control plane they configure.

use korpis_core::account::session::SessionLimits;
use korpis_core::rate::{Rate, Rates};
use korpis_core::schedule::PlacementPolicy;
use korpis_server::data::DataDir;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
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
    /// The metering interval every node samples at, `<n>m` or `1h`: a whole number of minutes
    /// that divides an hour (§2 of docs/15-observability.md).
    #[arg(long, default_value = "1h", value_parser = korpis_server::metering_interval)]
    metering_interval: Duration,
    /// How long after a day ends a late sample may still revise it, in whole hours, before the
    /// day closes and a later one is an adjustment (§2 of docs/15-observability.md).
    #[arg(long, default_value = "1d", value_parser = korpis_server::metering_lateness)]
    metering_lateness: Duration,
    /// Whether this process serves the relay role, carrying console and file sessions; turn it
    /// off where relays run as their own processes (§3.1 of docs/02-architecture.md).
    #[arg(long, env = "KORPIS_RELAY_ROLE", default_value_t = true, action = clap::ArgAction::Set)]
    relay_role: bool,
    /// A load balancer whose connections open with a PROXY header (version 1 or 2) naming the
    /// client, as a CIDR; repeatable. Connections from anywhere else are their own client.
    #[arg(long = "proxy-protocol-from", env = "KORPIS_PROXY_PROTOCOL_FROM", value_delimiter = ',')]
    proxy_protocol_from: Vec<ipnet::IpNet>,
    /// The DNS server, `address:port`, that domains are verified through by their TXT records;
    /// the system's resolvers when unset. Name a public resolver where the system's would answer
    /// for an internal view of a zone (§3.2 of docs/07-networking.md).
    #[arg(long, env = "KORPIS_DNS_RESOLVER")]
    dns_resolver: Option<std::net::SocketAddr>,
    /// The ACME directory `acme` certificates are ordered from (§3.2 of
    /// docs/07-networking.md).
    #[arg(long, env = "KORPIS_ACME_DIRECTORY", default_value = korpis_server::acme::LETS_ENCRYPT)]
    acme_directory: String,
    /// A certificate, in PEM, the ACME directory's own is trusted by instead of the system's
    /// roots: for a test directory with an authority of its own.
    #[arg(long, env = "KORPIS_ACME_CA")]
    acme_ca: Option<PathBuf>,
    /// A contact for the installation's account at the ACME directory, `mailto:…`; repeatable.
    #[arg(long = "acme-contact", env = "KORPIS_ACME_CONTACT", value_delimiter = ',')]
    acme_contact: Vec<String>,
    /// Agree to the ACME directory's terms of service; without it no account is made there and
    /// no `acme` certificate is issued, each saying why.
    #[arg(long, env = "KORPIS_ACME_AGREE_TERMS")]
    acme_agree_terms: bool,
    /// Each subject's bucket, `<per second>/<burst>` (§10 of docs/10-api.md).
    #[arg(long, default_value = "20/200", value_parser = Rate::parse)]
    rate_individual: Rate,
    /// Each organization's bucket, shared by every subject holding a grant in it.
    #[arg(long, default_value = "100/1000", value_parser = Rate::parse)]
    rate_organization: Rate,
    /// Each subject's bucket for requests that compute a plan, back up, or install.
    #[arg(long, default_value = "2/20", value_parser = Rate::parse)]
    rate_expensive_individual: Rate,
    /// Each organization's bucket for requests that compute a plan, back up, or install.
    #[arg(long, default_value = "10/100", value_parser = Rate::parse)]
    rate_expensive_organization: Rate,
    /// Each source's bucket for failed attempts to authenticate, `<per minute>/<burst>`; the
    /// source is the address, or the /64 of an IPv6 one.
    #[arg(long, default_value = "30/30", value_parser = Rate::parse_per_minute)]
    rate_failures: Rate,
    /// The installation's bucket for failed attempts recorded apart, `<per minute>/<burst>`;
    /// past it, every source's failures are counted together.
    #[arg(long, default_value = "600/600", value_parser = Rate::parse_per_minute)]
    rate_failures_recorded: Rate,
    /// The address the installation is reached at, such as `https://korpis.example.com`: its
    /// origin is the panel's and its host the site passkeys are bound to (§9.6 of
    /// docs/08-identity.md). Without it, no passkey can be registered or used.
    #[arg(long, env = "KORPIS_PUBLIC_URL")]
    public_url: Option<String>,
    /// The site passkeys are bound to, when it is a registrable suffix of the public URL's host
    /// rather than the host itself, so a panel at a sibling name can share it.
    #[arg(long, env = "KORPIS_RP_ID")]
    rp_id: Option<String>,
    /// Another origin a panel is served from, on the same site (§1 of docs/11-surface-web.md).
    #[arg(long = "web-origin", env = "KORPIS_WEB_ORIGIN", value_delimiter = ',')]
    web_origin: Vec<String>,
    /// How long a user session lasts without being used, as `<n>m`, `<n>h`, or `<n>d` (§9.4 of
    /// docs/08-identity.md).
    #[arg(long, default_value = "12h", value_parser = korpis_server::span)]
    session_idle: Duration,
    /// How long a user session lasts in all, as `--session-idle` is written.
    #[arg(long, default_value = "7d", value_parser = korpis_server::span)]
    session_max: Duration,
    /// The panel's built bundle, served on this address beside the API (§1 of
    /// docs/11-surface-web.md); without it, the address serves the API alone.
    #[arg(long, env = "KORPIS_WEB_ROOT")]
    web_root: Option<PathBuf>,
}

/// Serves the API until the process is interrupted or the server fails.
pub async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
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
        metering_interval: args.metering_interval,
        metering_lateness: args.metering_lateness,
        denial_window: Duration::from_secs(args.denial_window_secs.max(1)),
        event_retention: Duration::from_secs(args.event_retention_days.max(1) * 86_400),
        stream_retention_floor: Duration::from_secs(
            args.stream_retention_floor_days.saturating_mul(86_400),
        ),
        relay_role: args.relay_role,
        proxy_protocol_from: args.proxy_protocol_from,
        dns_resolver: args.dns_resolver,
        acme: korpis_server::acme::AcmeConfig {
            directory: args.acme_directory,
            ca: args.acme_ca,
            contact: args.acme_contact,
            agreed: args.acme_agree_terms,
        },
        rates: Rates {
            individual: args.rate_individual,
            organization: args.rate_organization,
            expensive_individual: args.rate_expensive_individual,
            expensive_organization: args.rate_expensive_organization,
            failures: args.rate_failures,
            recorded: args.rate_failures_recorded,
            guesses: korpis_core::rate::GUESSES,
            device_guesses: korpis_core::rate::DEVICE_GUESSES,
            account_guesses: korpis_core::rate::GUESSES,
        },
        relying_party: korpis_server::relying_party::relying_party(
            args.public_url.as_deref(),
            args.rp_id.as_deref(),
            &args.web_origin,
        )?,
        session_limits: SessionLimits { idle: args.session_idle, max: args.session_max },
        web_root: args.web_root,
    })
    .await?;
    tokio::select! {
        r = running.task => r??,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
