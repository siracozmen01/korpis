// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node as it runs in production, in one test process: the privileged helper on its own
//! thread, the `oci` driver serving its socket, and the agent reaching both through their
//! sockets and the control plane through its stream. Only the processes are folded together;
//! every boundary between them is the socket it is in production.

use crate::support::ControlPlane;
use korpis_agent::advertise::Advertise;
use korpis_agent::content::ContentStore;
use korpis_agent::host::node::NodeHost;
use korpis_agent::host::privileged::HelperClient;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options, Reserve, facts_of};
use korpis_agent::runtime::Runtime;
use korpis_agent::runtime::drivers::Drivers;
use korpis_agent::runtime::process::DriverRuntime;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_core::node::advertised::PortRange;
use korpis_core::node::driver::DeviceClass;
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::containerd::stream::Streams;
use korpis_driver_oci::devices::Specs;
use korpis_driver_oci::validate::Policy;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

/// containerd's socket on this machine.
pub const CONTAINERD: &str = "/run/containerd/containerd.sock";

/// A joined node.
pub struct Node {
    dir: tempfile::TempDir,
    pub state: StateDir,
    pub volumes: PathBuf,
    driver: PathBuf,
    helper: PathBuf,
    /// The workloads whose containers are removed when the test ends, however it ends.
    guarded: Mutex<Vec<String>>,
    /// A filesystem mounted for the volumes, unmounted when the node is dropped.
    mounted: Option<PathBuf>,
    /// Where the helper binds the workloads' network namespaces; every one is released when
    /// the node is dropped.
    pub netns: PathBuf,
    /// The node's own IPv4 pool, so two nodes in one test never hand out one address.
    pub pool: ipnet::Ipv4Net,
    /// The node's own ports, so two nodes on this one machine never forward one port.
    pub ports: PortRange,
    /// The `firecracker` driver's socket and the cgroup its guests run beneath, on a node that
    /// runs it beside the `oci` driver.
    microvms: Option<(PathBuf, PathBuf)>,
    /// The node's own Container Device Interface directory, which its agent and its driver read
    /// as their `--cdi-dir`; empty unless a test writes a spec file into it.
    pub cdi: PathBuf,
}

/// The device kind the tests' spec files name, which the agent maps to `serial` as
/// `--device-class korpis.test/standin=serial` would.
pub const STANDIN: &str = "korpis.test/standin";

/// An address of this machine other than loopback: the one its traffic towards the internet
/// leaves from, or loopback where it has none.
pub fn host_address() -> std::net::Ipv4Addr {
    let route = Command::new("ip").args(["-4", "-o", "route", "get", "1.1.1.1"]).output();
    let text = route.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut words = text.split_whitespace();
    let src = words.find(|w| *w == "src").and_then(|_| words.next());
    src.and_then(|a| a.parse().ok()).unwrap_or(std::net::Ipv4Addr::LOCALHOST)
}

/// A pool no other node of this test process uses: `10.N.0.0/16`, N from 200 upward.
fn next_pool() -> ipnet::Ipv4Net {
    static NEXT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 50;
    ipnet::Ipv4Net::new(std::net::Ipv4Addr::new(10, 200 + n, 0, 0), 16).unwrap()
}

/// Ports no other node of this test process uses: fifty of the default range, from 30000.
fn next_ports() -> PortRange {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let first = 30_000 + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 50 * 50;
    PortRange::new(first, first + 49).unwrap()
}

/// Whether this machine can run the test: containerd is there and the test is root.
pub fn live() -> bool {
    let root =
        std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0);
    root && std::os::unix::net::UnixStream::connect(CONTAINERD).is_ok()
}

fn registries() -> Vec<String> {
    vec![std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())]
}

/// The helper, serving on `socket` from its own thread for the rest of the process, running
/// backup and restore jobs with the job binary as a node's helper does, which read recipe content
/// from `content`, the agent's content store, and file sessions with the file worker.
fn helper(socket: &Path, volumes: &Path, (state, content, netns): (&Path, &Path, &Path)) {
    let class = korpis_helper::storage::Class::open("local", volumes).unwrap();
    let ledger = korpis_helper::ids::Ledger::open(state).unwrap();
    let jobs = korpis_helper::job::JobConfig {
        binary: built("korpis-backup", "korpis-backup-job"),
        install_binary: Some(built("korpis-install", "korpis-install-job")),
        state: state.to_owned(),
        cgroups: None,
        content: Some(content.to_owned()),
    };
    let network = korpis_helper::network::Network::new(netns.to_owned());
    // As `--ingress-binary` beside the helper would, the proxy's cgroup made beneath one of the
    // test's own, which the node removes when it is dropped.
    let ingress =
        korpis_helper::ingress::IngressConfig { binary: proxy(), cgroups: Some(cgroups(state)) };
    let mut h = korpis_helper::helper::Helper::new(vec![class], ledger)
        .with_jobs(jobs)
        .with_network(network)
        .with_ingress(korpis_helper::ingress::Ingress::new(ingress))
        .with_files(korpis_helper::files::Files::new(korpis_helper::files::FilesConfig {
            binary: built("korpis-files", "korpis-fsworker"),
            state: state.to_owned(),
            cgroups: None,
        }));
    let listener = korpis_helper::serve::bound(socket).unwrap();
    std::thread::spawn(move || korpis_helper::serve::serve(&listener, &[0].into(), &mut h, None));
}

/// The ingress proxy, built where this test was, once per test process.
fn proxy() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| built("korpis-ingress", "korpis-ingress")).clone()
}

/// The cgroup the helper whose state is at `state` makes its ingress proxy's beneath: one of
/// this test process's own, named for that helper, so no two nodes share it.
fn cgroups(state: &Path) -> PathBuf {
    let name = state.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy();
    PathBuf::from(format!("/sys/fs/cgroup/korpis-e2e-{}-{name}", std::process::id()))
}

/// The stream writer, built where this test was if it is not already.
pub fn writer() -> PathBuf {
    built("korpis-stream", "korpis-stream-writer")
}

/// Binary `bin` of `package`, built where this test was if it is not already.
pub fn built(package: &str, bin: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let target = profile_dir.parent().unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", package, "--bin", bin])
        .args(["--profile", profile, "--target-dir"])
        .arg(target)
        .status()
        .unwrap();
    assert!(built.success(), "{bin} did not build");
    profile_dir.join(bin)
}

/// The oci driver, serving on `socket` for the rest of the test; it mounts identity sockets
/// from the agent's default root, `identity` in its state directory, writes consoles with the
/// stream writer, keeping its own state in `driver` beside the agent's, and reads devices from
/// the spec files in `cdi`.
async fn driver(socket: &Path, state: &StateDir, (volumes, netns, cdi): (&Path, &Path, &Path)) {
    let content = state.root().join("content");
    let streams = Streams { writer: writer(), state: state.root().with_file_name("driver") };
    let containerd =
        Containerd::connect(Path::new(CONTAINERD), &content).await.unwrap().with_streams(streams);
    let listener = korpis_driver_kit::serve::bind(socket).unwrap();
    let identity_root = state.root().join("identity");
    let policy = Policy {
        volume_root: volumes.to_owned(),
        identity_root,
        netns_root: netns.to_owned(),
        devices: Specs { dirs: vec![cdi.to_owned()], owner: 0 },
    };
    let (d, _watcher) = korpis_driver_oci::service::Driver::new(containerd, policy);
    let allowed = BTreeSet::from([0]);
    tokio::spawn(korpis_driver_kit::serve::serve(listener, allowed, d, std::future::pending()));
}

impl Node {
    /// A node named `name`, joined to `cp`, its helper and driver serving.
    pub async fn join(cp: &ControlPlane, name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let volumes = dir.path().join("volumes");
        Self::join_with(cp, name, dir, (volumes, None)).await
    }

    /// As [`Self::join`], with its volumes on a btrfs made for it, qgroups on, so its class can
    /// snapshot; `None` where this machine cannot make one.
    pub async fn join_on_btrfs(cp: &ControlPlane, name: &str) -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("btrfs.img");
        std::fs::File::create(&image).unwrap().set_len(512 << 20).unwrap();
        let at = dir.path().join("fs");
        std::fs::create_dir(&at).unwrap();
        let ok = |c: &mut Command| c.status().is_ok_and(|s| s.success());
        let made = ok(Command::new("mkfs.btrfs").arg("-q").arg(&image))
            && ok(Command::new("mount").args(["-o", "loop"]).arg(&image).arg(&at));
        if !made {
            return None;
        }
        let quota = ok(Command::new("btrfs").args(["quota", "enable"]).arg(&at))
            && ok(Command::new("btrfs").args(["quota", "rescan", "-w"]).arg(&at));
        assert!(quota, "qgroups on the test's own filesystem");
        let volumes = at.join("volumes");
        Some(Self::join_with(cp, name, dir, (volumes, Some(at))).await)
    }

    async fn join_with(
        cp: &ControlPlane,
        name: &str,
        dir: tempfile::TempDir,
        (volumes, mounted): (PathBuf, Option<PathBuf>),
    ) -> Self {
        let state = StateDir::new(dir.path().join("agent"));
        let (token, ca) = cp.token(name).await;
        let request =
            Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: name.into() };
        join(&state, &request).await.unwrap();
        let (driver_socket, helper_socket) =
            (dir.path().join("driver.sock"), dir.path().join("helper.sock"));
        let content = state.root().join("content");
        let netns = dir.path().join("netns");
        helper(&helper_socket, &volumes, (&dir.path().join("helper"), &content, &netns));
        let cdi = dir.path().join("cdi");
        std::fs::create_dir(&cdi).unwrap();
        driver(&driver_socket, &state, (&volumes, &netns, &cdi)).await;
        Self {
            dir,
            state,
            volumes,
            driver: driver_socket,
            helper: helper_socket,
            guarded: Mutex::new(Vec::new()),
            mounted,
            netns,
            pool: next_pool(),
            ports: next_ports(),
            microvms: None,
            cdi,
        }
    }

    /// As [`Self::join`], the node running the `firecracker` driver beside the `oci` driver, as
    /// `korpis-agent run --driver oci=... --driver firecracker=...` does; `None` where this
    /// machine cannot run a guest.
    pub async fn join_with_microvms(cp: &ControlPlane, name: &str) -> Option<Self> {
        if !crate::guests::able() {
            return None;
        }
        let mut node = Self::join(cp, name).await;
        let dir = node.dir.path().to_owned();
        let served = crate::guests::serve(&dir, &node.state, (&node.volumes, &node.netns));
        node.microvms = Some(served);
        Some(node)
    }

    /// The agent's runtime: the driver over its socket.
    pub fn runtime(&self) -> DriverRuntime {
        DriverRuntime::connect(&self.driver)
    }

    /// Starts the agent as `korpis-agent run --driver oci --ingress false` does, and waits until
    /// it is welcomed: only a test of the ingress has its node open ports 80 and 443 on this
    /// machine's address, which two nodes cannot both hold.
    pub async fn agent(&self) -> Agent {
        self.agent_with(false).await
    }

    /// As [`Self::agent`], the node serving the ingress, as `korpis-agent run` does by default.
    pub async fn serving_ingress(&self) -> Agent {
        self.agent_with(true).await
    }

    async fn agent_with(&self, ingress: bool) -> Agent {
        match &self.microvms {
            None => self.agent_over(self.runtime(), ingress).await,
            Some((socket, _)) => {
                let both = vec![self.runtime(), DriverRuntime::connect(socket)];
                self.agent_over(Drivers::new(both).await.unwrap(), ingress).await
            }
        }
    }

    async fn agent_over<R: Runtime + Clone>(&self, runtime: R, ingress: bool) -> Agent {
        // As `--fetch-allow 127.0.0.0/8` would: the tests serve what installs fetch on loopback.
        let loopback = korpis_agent::content::Reach::with(vec!["127.0.0.0/8".parse().unwrap()]);
        let content = ContentStore::open(&self.state.root().join("content"), registries())
            .and_then(|c| c.reaching(loopback))
            .unwrap();
        let host = NodeHost::new(HelperClient::new(&self.helper), content);
        let identity = self.state.load().unwrap();
        let (server, id) = (identity.server, identity.node);
        // As `--advertise-address` with this machine's own address would: the test's control
        // plane is on this machine, and the node forwards its endpoints' ports on every address
        // but loopback (§3 of docs/07-networking.md).
        let advertise = Advertise::new(vec![host_address().into()], self.ports, &server);
        let ingress = ingress.then(|| advertise.ingress());
        let offer = (Reserve::default(), [].as_slice(), Vec::new());
        // As `--cdi-dir` with the node's own directory and `--device-class` for the stand-in.
        let classes = [(STANDIN.to_owned(), DeviceClass::Serial)].into();
        let config = korpis_agent::devices::Config {
            dirs: vec![self.cdi.clone()],
            classes,
            ..Default::default()
        };
        let (devices, _) = korpis_agent::devices::watch(config).await;
        let facts = facts_of(&runtime, &host, offer, (advertise, devices.clone())).await.unwrap();
        // As `--workload-ipv4` with the node's own pool would.
        let network = korpis_agent::network::offered(&runtime, &host, &id, (self.pool, None)).await;
        let period = Duration::from_millis(500);
        let options =
            Options { period, network, ingress, devices: Some(devices), ..Options::default() };
        let agent = Agent::start(&self.state, runtime, host, facts, options).unwrap();
        let mut events = agent.events();
        let welcomed =
            async { while !matches!(events.recv().await, Ok(Event::Welcomed { .. })) {} };
        tokio::time::timeout(Duration::from_secs(10), welcomed).await.expect("welcomed");
        agent
    }

    /// The process ids of the Firecracker processes running the node's guests, each beneath the
    /// cgroup of its object.
    pub fn firecrackers(&self) -> Vec<u32> {
        let Some((_, cgroup)) = &self.microvms else { return Vec::new() };
        let objects: Vec<PathBuf> = std::fs::read_dir(cgroup)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        let comm = |p: &u32| std::fs::read_to_string(format!("/proc/{p}/comm"));
        objects
            .iter()
            .flat_map(|o| {
                let procs = std::fs::read_to_string(o.join("cgroup.procs")).unwrap_or_default();
                procs.lines().filter_map(|l| l.parse().ok()).collect::<Vec<u32>>()
            })
            .filter(|p| comm(p).is_ok_and(|c| c.trim() == "firecracker"))
            .collect()
    }

    /// Removes `workload`'s containers when the node is dropped, if the test did not.
    pub fn guard(&self, workload: &str) {
        self.guarded.lock().unwrap().push(workload.to_owned());
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some((_, cgroup)) = &self.microvms {
            crate::guests::end_guests(cgroup, &self.dir.path().join("firecracker"));
        }
        let ctr = |args: &[&str]| Command::new("ctr").args(["-n", "korpis"]).args(args).output();
        for w in self.guarded.lock().map(|g| g.clone()).unwrap_or_default() {
            let filter = format!("labels.\"korpis.workload\"=={w}");
            let Ok(out) = ctr(&["containers", "ls", "-q", &filter]) else { continue };
            for id in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                let _ = ctr(&["tasks", "kill", "-s", "SIGKILL", id]);
                std::thread::sleep(Duration::from_millis(300));
                let _ = ctr(&["tasks", "rm", "-f", id]);
                let _ = ctr(&["containers", "rm", id]);
                let _ = ctr(&["snapshots", "rm", id]);
            }
        }
        let networks = korpis_helper::network::Network::new(self.netns.clone());
        for entry in std::fs::read_dir(&self.netns).into_iter().flatten().flatten() {
            let _ = networks.release(&entry.file_name().to_string_lossy());
        }
        // The proxy left when its agent did; its cgroup goes, with the rule naming it.
        let cgroups = cgroups(&self.dir.path().join("helper"));
        let _ = std::fs::write(cgroups.join("ingress/cgroup.kill"), "1");
        if cgroups.exists() {
            let chain = ["delete", "chain", "inet", "korpis", "ingress_output"];
            let _ = Command::new("nft").args(chain).status();
        }
        for _ in 0..20 {
            let _ = std::fs::remove_dir(cgroups.join("ingress"));
            if std::fs::remove_dir(&cgroups).is_ok() || !cgroups.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(at) = &self.mounted {
            let _ = Command::new("umount").arg("-l").arg(at).status();
        }
    }
}
