// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver conformance suite (§8 of docs/04-runtimes.md) against both of Korpis's drivers,
//! each started as its own process serving its socket, as a node starts it, over a content store,
//! a volume root, and state of the test's own: every case's verdict is printed, and no case may
//! fail. The `oci` driver needs root and the machine's containerd; the `firecracker` driver needs
//! what the microVM tests need. A driver this machine cannot run is not run.

use crate::guests::{able, end_guests, musl};
use crate::node::{built, live, writer};
use korpis_driver_conformance::Fixture;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

/// Each object's ids, as a node gives a workload.
const IDS: u32 = 65_536;
/// The most objects one run makes.
const OBJECTS: u32 = 64;

/// A driver process, killed when the test ends.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every guest beneath a cgroup ended, and the volumes its jails bound unbound, when the test
/// ends, whether or not it failed.
struct Guests(PathBuf, PathBuf);

impl Drop for Guests {
    fn drop(&mut self) {
        end_guests(&self.0, &self.1);
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The fixture of a driver serving on `socket` over `dir`, its objects' ids from `base`.
fn fixture(dir: &Path, socket: PathBuf, base: u32) -> Fixture {
    let volume_root = dir.join("volumes");
    std::fs::create_dir_all(&volume_root).unwrap();
    Fixture {
        socket,
        content: dir.join("content"),
        volume_root: volume_root.canonicalize().unwrap(),
        probe: musl("korpis-conformance-probe", "korpis-conformance-probe"),
        uid_base: base,
        uid_count: IDS,
        objects: OBJECTS,
    }
}

/// Starts `command`, and waits for it to serve on `socket`.
async fn serving(mut command: Command, socket: &Path) -> Process {
    let process = Process(command.spawn().unwrap());
    for _ in 0..300 {
        if socket.exists() {
            return process;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the driver never served on {}", socket.display());
}

/// The suite's report of the driver `fixture` names.
async fn report(fixture: Fixture) -> korpis_driver_conformance::Report {
    korpis_driver_conformance::run(fixture).await.unwrap()
}

#[tokio::test]
async fn the_oci_driver_conforms() {
    if !live() {
        return;
    }
    let containerd = Path::new("/run/containerd/containerd.sock");
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let fixture = fixture(d, d.join("oci.sock"), 26_000 * IDS);
    let mut command = Command::new(built("korpis-driver-oci", "korpis-driver-oci"));
    command
        .arg("--socket")
        .arg(&fixture.socket)
        .arg("--containerd")
        .arg(containerd)
        .arg("--content")
        .arg(&fixture.content)
        .arg("--volume-root")
        .arg(&fixture.volume_root)
        .arg("--identity-root")
        .arg(d.join("identity"))
        .arg("--netns-root")
        .arg(d.join("netns"))
        .arg("--stream-writer")
        .arg(writer())
        .arg("--state-dir")
        .arg(d.join("state"));
    let _driver = serving(command, &fixture.socket).await;
    let report = report(fixture).await;
    eprintln!("{report}");
    assert!(report.conforms(), "{report}");
}

#[tokio::test]
async fn the_firecracker_driver_conforms() {
    if !live() || !able() {
        eprintln!("this machine cannot run a guest: not run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let fixture = fixture(d, d.join("firecracker.sock"), 27_000 * IDS);
    let tools = repo().join(".tools");
    let cgroup = format!("korpis-conformance-{}-firecracker", std::process::id());
    let mut command = Command::new(built("korpis-driver-firecracker", "korpis-driver-firecracker"));
    command
        .arg("--socket")
        .arg(&fixture.socket)
        .arg("--firecracker")
        .arg(tools.join("bin/firecracker"))
        .arg("--jailer")
        .arg(tools.join("bin/jailer"))
        .arg("--kernel")
        .arg(tools.join("guest/vmlinux"))
        .arg("--guest")
        .arg(musl("korpis-guest", "korpis-guest"))
        .arg("--fsworker")
        .arg(musl("korpis-files", "korpis-fsworker"))
        .arg("--install-job")
        .arg(musl("korpis-install", "korpis-install-job"))
        .arg("--image-build")
        .arg(built("korpis-image", "korpis-image-build"))
        .arg("--stream-writer")
        .arg(writer())
        .arg("--content")
        .arg(&fixture.content)
        .arg("--volume-root")
        .arg(&fixture.volume_root)
        .arg("--netns-root")
        .arg(d.join("netns"))
        .arg("--identity-root")
        .arg(d.join("identity"))
        .arg("--state-dir")
        .arg(d.join("state"))
        .arg("--cgroup")
        .arg(&cgroup);
    let _guests = Guests(Path::new("/sys/fs/cgroup").join(&cgroup), d.join("state"));
    let _driver = serving(command, &fixture.socket).await;
    let report = report(fixture).await;
    eprintln!("{report}");
    assert!(report.conforms(), "{report}");
}
