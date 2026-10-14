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

/// The helper, serving on `socket` from its own thread for the rest of the process.
fn helper(socket: &Path, volumes: &Path, state: &Path) {
    let class = korpis_helper::storage::Class::open("local", volumes).unwrap();
    let ledger = korpis_helper::ids::Ledger::open(state).unwrap();
    let mut h = korpis_helper::helper::Helper::new(vec![class], ledger);
    let listener = korpis_helper::serve::bound(socket).unwrap();
    std::thread::spawn(move || korpis_helper::serve::serve(&listener, &[0].into(), &mut h, None));
}

/// The stream writer, built where this test was if it is not already.
fn writer() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let target = profile_dir.parent().unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "korpis-stream", "--bin", "korpis-stream-writer"])
        .args(["--profile", profile, "--target-dir"])
        .arg(target)
        .status()
        .unwrap();
    assert!(built.success(), "korpis-stream-writer did not build");
    profile_dir.join("korpis-stream-writer")
}

/// The oci driver, serving on `socket` for the rest of the test; it mounts identity sockets
/// from the agent's default root, `identity` in its state directory, and writes consoles with
/// the stream writer, keeping its own state in `driver` beside the agent's.
async fn driver(socket: &Path, state: &StateDir, volumes: &Path) {
    let content = state.root().join("content");
    let streams = Streams { writer: writer(), state: state.root().with_file_name("driver") };
    let containerd =
        Containerd::connect(Path::new(CONTAINERD), &content).await.unwrap().with_streams(streams);
    let listener = korpis_driver_oci::serve::bind(socket).unwrap();
    let identity_root = state.root().join("identity");
    let policy = Policy { volume_root: volumes.to_owned(), identity_root };
    let (d, _watcher) = korpis_driver_oci::service::Driver::new(containerd, policy);
    let allowed = BTreeSet::from([0]);
    tokio::spawn(korpis_driver_oci::serve::serve(listener, allowed, d, std::future::pending()));
}

impl Node {
    /// A node named `name`, joined to `cp`, its helper and driver serving.
    pub async fn join(cp: &ControlPlane, name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path().join("agent"));
        let (token, ca) = cp.token(name).await;
        let request =
            Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: name.into() };
        join(&state, &request).await.unwrap();
        let volumes = dir.path().join("volumes");
        let (driver_socket, helper_socket) =
            (dir.path().join("driver.sock"), dir.path().join("helper.sock"));
        helper(&helper_socket, &volumes, &dir.path().join("helper"));
        driver(&driver_socket, &state, &volumes).await;
        Self {
            _dir: dir,
            state,
            volumes,
            driver: driver_socket,
            helper: helper_socket,
            guarded: Mutex::new(Vec::new()),
        }
    }

    /// The agent's runtime: the driver over its socket.
    pub fn runtime(&self) -> DriverRuntime {
        DriverRuntime::connect(&self.driver)
    }

    /// Starts the agent as `korpis-agent run --driver oci` does, and waits until it is welcomed.
    pub async fn agent(&self) -> Agent {
        let content = ContentStore::open(&self.state.root().join("content"), registries()).unwrap();
        let host = NodeHost::new(HelperClient::new(&self.helper), content);
        let runtime = self.runtime();
        let server = self.state.load().unwrap().server;
        // As `--advertise-address 127.0.0.1` would: the test's control plane is on this machine.
        let loopback = vec![std::net::Ipv4Addr::LOCALHOST.into()];
        let advertise = Advertise::new(loopback, PortRange::DEFAULT, &server);
        let facts = facts_of(&runtime, &host, Reserve::default(), advertise).await.unwrap();
        let options = Options { period: Duration::from_millis(500), ..Options::default() };
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
    }
}
