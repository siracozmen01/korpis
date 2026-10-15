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
use korpis_agent::runtime::process::DriverRuntime;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_core::node::advertised::PortRange;
use korpis_driver_oci::containerd::Containerd;
use korpis_driver_oci::containerd::stream::Streams;
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
    _dir: tempfile::TempDir,
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
}

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
/// from `content`, the agent's content store.
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
    let mut h = korpis_helper::helper::Helper::new(vec![class], ledger)
        .with_jobs(jobs)
        .with_network(network);
    let listener = korpis_helper::serve::bound(socket).unwrap();
    std::thread::spawn(move || korpis_helper::serve::serve(&listener, &[0].into(), &mut h, None));
}

/// The stream writer, built where this test was if it is not already.
fn writer() -> PathBuf {
    built("korpis-stream", "korpis-stream-writer")
}

/// Binary `bin` of `package`, built where this test was if it is not already.
fn built(package: &str, bin: &str) -> PathBuf {
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
/// from the agent's default root, `identity` in its state directory, and writes consoles with
/// the stream writer, keeping its own state in `driver` beside the agent's.
async fn driver(socket: &Path, state: &StateDir, (volumes, netns): (&Path, &Path)) {
    let content = state.root().join("content");
    let streams = Streams { writer: writer(), state: state.root().with_file_name("driver") };
    let containerd =
        Containerd::connect(Path::new(CONTAINERD), &content).await.unwrap().with_streams(streams);
    let listener = korpis_driver_oci::serve::bind(socket).unwrap();
    let identity_root = state.root().join("identity");
    let policy =
        Policy { volume_root: volumes.to_owned(), identity_root, netns_root: netns.to_owned() };
    let (d, _watcher) = korpis_driver_oci::service::Driver::new(containerd, policy);
    let allowed = BTreeSet::from([0]);
    tokio::spawn(korpis_driver_oci::serve::serve(listener, allowed, d, std::future::pending()));
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
        driver(&driver_socket, &state, (&volumes, &netns)).await;
        Self {
            _dir: dir,
            state,
            volumes,
            driver: driver_socket,
            helper: helper_socket,
            guarded: Mutex::new(Vec::new()),
            mounted,
            netns,
            pool: next_pool(),
        }
    }

    /// The agent's runtime: the driver over its socket.
    pub fn runtime(&self) -> DriverRuntime {
        DriverRuntime::connect(&self.driver)
    }

    /// Starts the agent as `korpis-agent run --driver oci` does, and waits until it is welcomed.
    pub async fn agent(&self) -> Agent {
        // As `--fetch-allow 127.0.0.0/8` would: the tests serve what installs fetch on loopback.
        let loopback = korpis_agent::content::Reach::with(vec!["127.0.0.0/8".parse().unwrap()]);
        let content = ContentStore::open(&self.state.root().join("content"), registries())
            .and_then(|c| c.reaching(loopback))
            .unwrap();
        let host = NodeHost::new(HelperClient::new(&self.helper), content);
        let runtime = self.runtime();
        let identity = self.state.load().unwrap();
        let (server, id) = (identity.server, identity.node);
        // As `--advertise-address` with this machine's own address would: the test's control
        // plane is on this machine, and the node forwards its endpoints' ports on every address
        // but loopback (§3 of docs/07-networking.md).
        let advertise = Advertise::new(vec![host_address().into()], PortRange::DEFAULT, &server);
        let facts = facts_of(&runtime, &host, Reserve::default(), advertise).await.unwrap();
        // As `--workload-ipv4` with the node's own pool would.
        let network = korpis_agent::network::offered(&runtime, &host, &id, (self.pool, None)).await;
        let options = Options { period: Duration::from_millis(500), network, ..Options::default() };
        let agent = Agent::start(&self.state, runtime, host, facts, options).unwrap();
        let mut events = agent.events();
        let welcomed =
            async { while !matches!(events.recv().await, Ok(Event::Welcomed { .. })) {} };
        tokio::time::timeout(Duration::from_secs(10), welcomed).await.expect("welcomed");
        agent
    }

    /// Removes `workload`'s containers when the node is dropped, if the test did not.
    pub fn guard(&self, workload: &str) {
        self.guarded.lock().unwrap().push(workload.to_owned());
    }
}

impl Drop for Node {
    fn drop(&mut self) {
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
        if let Some(at) = &self.mounted {
            let _ = Command::new("umount").arg("-l").arg(at).status();
        }
    }
}
