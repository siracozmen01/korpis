// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-agent`: join a control plane once, then run.

use clap::{Args, Parser, Subcommand};
use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use korpis_agent::advertise::Advertise;
use korpis_agent::content::{ContentStore, Reach};
use korpis_agent::host::node::NodeHost;
use korpis_agent::host::privileged::HelperClient;
use korpis_agent::host::{Host, InMemoryHost};
use korpis_agent::join::{Join, join};
use korpis_agent::preflight::probe::{Asking, probe};
use korpis_agent::preflight::{Machine, Offer, checks};
use korpis_agent::run::{Agent, Options, Reserve, facts_of};
use korpis_agent::runtime::Runtime;
use korpis_agent::runtime::drivers::Drivers;
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::runtime::process::DriverRuntime;
use korpis_agent::state::StateDir;
use korpis_core::node::advertised::PortRange;
use korpis_core::node::driver::DeviceClass;
use std::net::IpAddr;
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
    /// Report what this machine lacks of what the node is configured to offer, and how to fix
    /// each; change nothing (§2 of docs/18-operations.md). Fails only when the node could offer
    /// nothing at all.
    Preflight(PreflightArgs),
    /// Run the node: hold the control stream, keep the journal, and reconcile.
    Run(Box<RunArgs>),
}

/// What `run` is told.
#[derive(Args)]
struct RunArgs {
    /// The runtime: `oci`, the driver processes serving on `--driver-socket`, the container
    /// tier's `korpis-driver-oci` by default, or `memory`, which runs nothing, for trying the
    /// agent without one.
    #[arg(long, env = "KORPIS_AGENT_DRIVER", default_value = "oci")]
    driver: String,
    /// A driver's socket; repeatable, one for each tier the node offers through its own
    /// driver process (§3 of docs/04-runtimes.md).
    #[arg(long, default_values = ["/run/korpis/driver-oci.sock"])]
    driver_socket: Vec<PathBuf>,
    /// The privileged helper's socket.
    #[arg(long, default_value = "/run/korpis/helper.sock")]
    helper_socket: PathBuf,
    /// A registry spoken to over plain HTTP, as `host:port`; repeatable. For a development
    /// registry only: content is verified by digest either way.
    #[arg(long = "plain-http")]
    plain_http: Vec<String>,
    /// A network an install's `fetch` may reach besides the public internet, as a CIDR, for
    /// a private mirror; repeatable. Loopback, private, and link-local addresses are
    /// otherwise refused (§4 of docs/09-recipes.md).
    #[arg(long = "fetch-allow")]
    fetch_allow: Vec<IpNet>,
    /// Millicores kept back from placement for the machine's own system.
    #[arg(long, default_value_t = Reserve::default().cpu_millis)]
    reserve_cpu_millis: u64,
    /// Bytes of memory kept back from placement for the machine's own system.
    #[arg(long, default_value_t = Reserve::default().memory_bytes)]
    reserve_memory_bytes: u64,
    /// An address clients reach this node at, such as a router's public address forwarding
    /// to it or a LAN address for clients on the LAN; repeatable. Unset: the node's own
    /// address towards the control plane, if it is routable on the internet. Either way the
    /// control plane measures it before anything relies on it.
    #[arg(long = "advertise-address")]
    advertise_address: Vec<IpAddr>,
    /// The ports endpoints on this node are allocated from, as `FIRST-LAST`.
    #[arg(long, default_value = "30000-32767")]
    ports: PortRange,
    /// The IPv4 pool workloads get addresses from, private to this node; change it when it
    /// collides with a network the node reaches (§4.1 of docs/07-networking.md).
    #[arg(long, default_value = korpis_agent::network::DEFAULT_IPV4)]
    workload_ipv4: Ipv4Net,
    /// An IPv6 prefix routed to this node, which gives workloads routable addresses; unset,
    /// they get unique local ones, translated on the way out.
    #[arg(long)]
    workload_ipv6: Option<Ipv6Net>,
    /// Serve the ingress: the helper opens ports 80 and 443 on the advertised addresses for
    /// the node's ingress proxy, and the node says it serves it, so workloads with ingress
    /// endpoints are placed here (§3.2 of docs/07-networking.md).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    ingress: bool,
    /// An interface the node's workloads leave by and the capacity it sells, as
    /// `eth0=1gbit`, keeping five percent for the node unless `,reserve=50mbit` says
    /// otherwise; repeatable. Declared, since a virtual machine's link speed is not what
    /// its provider sold; none, and the node enforces no bandwidth (§7 of
    /// docs/07-networking.md).
    #[arg(long = "uplink", value_parser = korpis_agent::shaping::parse_uplink)]
    uplinks: Vec<korpis_proto::korpis::helper::v1::Uplink>,
    /// A directory of Container Device Interface spec files the node's devices are read
    /// from; repeatable, a later one's device replacing an earlier one's of the same name
    /// (§6.1 of docs/05-scheduling.md).
    #[arg(long = "cdi-dir", default_values = ["/etc/cdi", "/var/run/cdi"])]
    cdi_dirs: Vec<PathBuf>,
    /// The class of a device kind whose name is none Korpis knows, as
    /// `vendor.com/kind=class`; repeatable. A device of a kind neither names is not offered.
    #[arg(long = "device-class", value_parser = korpis_agent::devices::parse_class)]
    device_classes: Vec<(String, DeviceClass)>,
    /// How many workloads may hold one device of a kind at once in `timeslice`, as
    /// `vendor.com/kind=N`; repeatable. Timeslicing isolates nothing between its holders
    /// (§6.2 of docs/05-scheduling.md); a kind not named is given in `passthrough` alone.
    #[arg(long = "device-share", value_parser = korpis_agent::devices::parse_share)]
    device_shares: Vec<(String, u32)>,
    /// The physical cores offered for dedication, by id (the lowest logical CPU of each), as
    /// a kernel CPU list such as `2-11`; unset, every core but the one holding CPU 0 (§5.2
    /// of docs/05-scheduling.md).
    #[arg(long = "dedicated-cores", value_parser = korpis_topology::cpu_list)]
    dedicated_cores: Option<std::collections::BTreeSet<u32>>,
}

impl RunArgs {
    /// The node these flags describe.
    fn node(self) -> Node {
        Node {
            driver: self.driver,
            driver_socket: self.driver_socket,
            helper_socket: self.helper_socket,
            plain_http: self.plain_http,
            fetch_allow: self.fetch_allow,
            reserve: Reserve {
                cpu_millis: self.reserve_cpu_millis,
                memory_bytes: self.reserve_memory_bytes,
            },
            advertise_address: self.advertise_address,
            ports: self.ports,
            workload_ipv4: self.workload_ipv4,
            workload_ipv6: self.workload_ipv6,
            ingress: self.ingress,
            uplinks: self.uplinks,
            devices: korpis_agent::devices::Config {
                dirs: self.cdi_dirs,
                classes: self.device_classes.into_iter().collect(),
                shares: self.device_shares.into_iter().collect(),
                owner: 0,
            },
            dedicated_cores: self.dedicated_cores,
        }
    }
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

/// Where the node's runtime and host are, and what it keeps back and offers.
struct Node {
    driver: String,
    driver_socket: Vec<PathBuf>,
    helper_socket: PathBuf,
    plain_http: Vec<String>,
    fetch_allow: Vec<IpNet>,
    reserve: Reserve,
    advertise_address: Vec<IpAddr>,
    ports: PortRange,
    workload_ipv4: Ipv4Net,
    workload_ipv6: Option<Ipv6Net>,
    ingress: bool,
    uplinks: Vec<korpis_proto::korpis::helper::v1::Uplink>,
    devices: korpis_agent::devices::Config,
    dedicated_cores: Option<std::collections::BTreeSet<u32>>,
}

/// Runs the agent with `runtime` and `host` until it is asked to stop; workloads are left
/// running.
async fn serve<R: Runtime + Clone, H: Host + Clone>(
    state: &StateDir,
    runtime: R,
    host: H,
    node: &Node,
) -> ExitCode {
    let (server, id) = match state.load() {
        Ok(identity) => (identity.server, identity.node),
        Err(e) => {
            tracing::error!(error = %e, "the node's state could not be read");
            return ExitCode::FAILURE;
        }
    };
    let advertise = Advertise::new(node.advertise_address.clone(), node.ports, &server);
    let ingress = node.ingress.then(|| advertise.ingress());
    let system = std::path::Path::new("/sys/devices/system");
    let cores = korpis_topology::cores(system, node.dedicated_cores.as_ref());
    let cores = cores.iter().map(korpis_wire::node::core_to_pb).collect();
    let offer = (node.reserve, node.uplinks.as_slice(), cores);
    let (devices, watching) = korpis_agent::devices::watch(node.devices.clone()).await;
    let facts = match facts_of(&runtime, &host, offer, (advertise, devices.clone())).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "the driver could not be described");
            return ExitCode::FAILURE;
        }
    };
    let pools = (node.workload_ipv4, node.workload_ipv6);
    let network = korpis_agent::network::offered(&runtime, &host, &id, pools).await;
    let options = Options { network, ingress, devices: Some(devices), ..Options::default() };
    let ran = start(state, (runtime, host), facts, options).await;
    watching.abort();
    ran
}

/// Starts the agent, and runs it until it is asked to stop.
async fn start<R: Runtime + Clone, H: Host + Clone>(
    state: &StateDir,
    (runtime, host): (R, H),
    facts: std::sync::Arc<dyn Fn() -> korpis_agent::session::Facts + Send + Sync>,
    options: Options,
) -> ExitCode {
    match Agent::start(state, runtime, host, facts, options) {
        Ok(agent) => {
            tracing::info!("agent running");
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

/// Runs the agent with the driver `node` names.
async fn run(state: &StateDir, node: Node) -> ExitCode {
    match node.driver.as_str() {
        "memory" => serve(state, InMemory::new(), InMemoryHost::new(), &node).await,
        "oci" => {
            let plain_http = node.plain_http.clone();
            let reach = Reach::with(node.fetch_allow.clone());
            let opened = ContentStore::open(&state.root().join("content"), plain_http)
                .and_then(|c| c.reaching(reach));
            let content = match opened {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "the content store could not be opened");
                    return ExitCode::FAILURE;
                }
            };
            let host = NodeHost::new(HelperClient::new(&node.helper_socket), content);
            let sockets = node.driver_socket.iter().map(|s| DriverRuntime::connect(s)).collect();
            match Drivers::new(sockets).await {
                Ok(drivers) => serve(state, drivers, host, &node).await,
                Err(e) => {
                    tracing::error!(error = %e, "the drivers could not be described");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            tracing::error!(driver = %other, "no such driver in this build");
            ExitCode::FAILURE
        }
    }
}

/// Enrolls this machine as `how` says.
async fn enroll(state: &StateDir, how: &Join) -> ExitCode {
    match join(state, how).await {
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

/// Prints what preflight found; fails only when the node could offer nothing.
async fn preflight(asking: &Asking, json: bool) -> ExitCode {
    let probes = probe(asking).await;
    let report = checks(&Machine::this(), &probes, &asking.offer);
    let printed = if json { format!("{}\n", report.json()) } else { report.text("This node") };
    let mut out = std::io::stdout().lock();
    if std::io::Write::write_all(&mut out, printed.as_bytes()).is_err() {
        return ExitCode::FAILURE;
    }
    if report.offers_anything() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// What `preflight` is told.
#[derive(clap::Args)]
struct PreflightArgs {
    /// The runtime driver the node will run: `oci` or `memory`.
    #[arg(long, env = "KORPIS_AGENT_DRIVER", default_value = "oci")]
    driver: String,
    /// containerd's socket, which the oci driver runs workloads through.
    #[arg(long, default_value = "/run/containerd/containerd.sock")]
    containerd_socket: PathBuf,
    /// A driver's socket, as `run` is given them, each asked which tiers it offers.
    #[arg(long, default_values = ["/run/korpis/driver-oci.sock"])]
    driver_socket: Vec<PathBuf>,
    /// The privileged helper's socket.
    #[arg(long, default_value = "/run/korpis/helper.sock")]
    helper_socket: PathBuf,
    /// The physical cores `run` is given for dedication, as a kernel CPU list of their ids;
    /// unset, every core but the one holding CPU 0.
    #[arg(long = "dedicated-cores", value_parser = korpis_topology::cpu_list)]
    dedicated_cores: Option<std::collections::BTreeSet<u32>>,
    /// The helper's volume root.
    #[arg(long, default_value = "/var/lib/korpis/volumes")]
    volume_root: PathBuf,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

impl PreflightArgs {
    /// Where preflight asks.
    fn asking(self) -> Asking {
        Asking {
            offer: Offer { driver: self.driver, dedicated_cores: self.dedicated_cores },
            containerd: self.containerd_socket,
            drivers: self.driver_socket,
            helper: self.helper_socket,
            volume_root: self.volume_root,
            sys: "/sys".into(),
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
            enroll(&state, &Join { server, token, ca_fingerprint, hostname }).await
        }
        Command::Preflight(args) => {
            let json = args.json;
            preflight(&args.asking(), json).await
        }
        Command::Run(args) => run(&state, args.node()).await,
    }
}
