// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver conformance suite (§8 of docs/04-runtimes.md) against both of Korpis's drivers,
//! each started by the suite as its own process serving its socket, as a node starts it, and
//! killed and started again by it, over a content store, a volume root, and state of the test's
//! own; a restart of the host is what this machine can do without restarting containerd, which
//! other services share. The suite is run by its runner, as a third party runs it, from a
//! fixture file; every case's verdict is printed, and no case may fail. The `oci` driver
//! needs root and the machine's containerd; the `firecracker` driver needs what the microVM tests
//! need. A driver this machine cannot run is not run.

use crate::guests::{able, end_guests, musl};
use crate::node::{built, live, writer};
use korpis_driver_conformance::{Fixture, Launch};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Each object's ids, as a node gives a workload.
const IDS: u32 = 65_536;
/// The most objects one run makes.
const OBJECTS: u32 = 64;

/// What a restart of the host does to a container: its process and its shim, which a reboot
/// ends, are killed, and containerd, which other services on this machine share, is left as it
/// is. The shim is the parent of the container's first process.
const CONTAINERS_RESTART: &str = "for p; do s=$(ps -o ppid= -p \"$p\"); kill -9 \"$p\"; [ \"${s:-1}\" -gt 1 ] && kill -9 $s; done; true";

/// What a restart of the host does to a guest: its Firecracker, which a reboot ends, is killed.
const GUESTS_RESTART: &str = "for p; do kill -9 \"$p\"; done; true";

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

/// Every container beneath a cgroup removed when the test ends, whether or not it failed.
struct Containers(PathBuf);

impl Drop for Containers {
    fn drop(&mut self) {
        crate::containers::remove_under(&self.0);
    }
}

/// `script` run by the shell, with what it is given as its arguments.
fn shell(script: &str) -> Launch {
    let args = ["-c", script, "restart"].map(OsString::from).to_vec();
    Launch { program: "/bin/sh".into(), args }
}

/// The fixture of `driver`, serving on `socket` in `dir` and restarting the host with `restart`,
/// its objects' ids from `base`.
fn fixture(dir: &Path, (driver, socket): (Launch, PathBuf), restart: Launch, base: u32) -> Fixture {
    Fixture {
        driver,
        socket,
        host_restart: Some(restart),
        content: dir.join("content"),
        volume_root: dir.join("volumes").canonicalize().unwrap(),
        probe: musl("korpis-conformance-probe", "korpis-conformance-probe"),
        uid_base: base,
        uid_count: IDS,
        objects: OBJECTS,
    }
}

/// `pairs` of an option and its value, as arguments.
fn args(pairs: &[(&str, &Path)]) -> Vec<OsString> {
    pairs.iter().flat_map(|(o, v)| [OsString::from(o), v.as_os_str().to_owned()]).collect()
}

/// Runs the suite's runner as a third party does, from a fixture file in `dir`, against the
/// driver named `name`; and finds it conforming to every case, isolation named as not tested.
fn conforms(dir: &Path, fixture: &Fixture, name: &str) {
    let (file, json) = (dir.join("fixture.json"), dir.join("report.json"));
    std::fs::write(&file, fixture.to_json().unwrap()).unwrap();
    let status = Command::new(built("korpis-driver-conformance", "korpis-driver-conformance"))
        .arg("--fixture")
        .arg(&file)
        .arg("--json")
        .arg(&json)
        .status()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert!(status.success(), "{name} does not conform: {report:#}");
    assert_eq!((report["driver"].as_str(), report["conforms"].as_bool()), (Some(name), Some(true)));
    let outcomes = report["outcomes"].as_array().unwrap();
    assert!(outcomes.len() >= 26 && outcomes.iter().all(|o| o["verdict"] != "fail"), "{report:#}");
    assert!(report["untested"][0]["requirement"].as_str().unwrap().starts_with("isolation"));
}

#[test]
fn the_oci_driver_conforms() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().canonicalize().unwrap();
    std::fs::create_dir(d.join("volumes")).unwrap();
    let socket = d.join("oci.sock");
    let writer = writer();
    let mut driver = Launch {
        program: built("korpis-driver-oci", "korpis-driver-oci"),
        args: args(&[
            ("--socket", &socket),
            ("--containerd", Path::new("/run/containerd/containerd.sock")),
            ("--content", &d.join("content")),
            ("--volume-root", &d.join("volumes")),
            ("--identity-root", &d.join("identity")),
            ("--netns-root", &d.join("netns")),
            ("--stream-writer", &writer),
            ("--state-dir", &d.join("state")),
        ]),
    };
    // Its own cgroup parent, the host sharing any dedicated cores: what it leaves is removed.
    let own = crate::containers::own("conformance");
    let host = ["--cgroup-parent", &own.parent, "--dedicated-cores-host", "shared"];
    driver.args.extend(host.map(OsString::from));
    let _containers = Containers(own.root.join(own.parent.trim_start_matches('/')));
    let fixture = fixture(&d, (driver, socket), shell(CONTAINERS_RESTART), 26_000 * IDS);
    conforms(&d, &fixture, "oci");
}

#[test]
fn the_firecracker_driver_conforms() {
    if !live() || !able() {
        eprintln!("this machine cannot run a guest: not run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().canonicalize().unwrap();
    std::fs::create_dir(d.join("volumes")).unwrap();
    let socket = d.join("firecracker.sock");
    let tools = repo().join(".tools");
    let cgroup = format!("korpis-conformance-{}-firecracker", std::process::id());
    let built_by = |package, bin| musl(package, bin);
    let (guest, fsworker) =
        (built_by("korpis-guest", "korpis-guest"), built_by("korpis-files", "korpis-fsworker"));
    let install = built_by("korpis-install", "korpis-install-job");
    let (image_build, writer) = (built("korpis-image", "korpis-image-build"), writer());
    let mut flags = args(&[
        ("--socket", &socket),
        ("--firecracker", &tools.join("bin/firecracker")),
        ("--jailer", &tools.join("bin/jailer")),
        ("--kernel", &tools.join("guest/vmlinux")),
        ("--guest", &guest),
        ("--fsworker", &fsworker),
        ("--install-job", &install),
        ("--image-build", &image_build),
        ("--stream-writer", &writer),
        ("--content", &d.join("content")),
        ("--volume-root", &d.join("volumes")),
        ("--netns-root", &d.join("netns")),
        ("--identity-root", &d.join("identity")),
        ("--state-dir", &d.join("state")),
    ]);
    flags.extend(["--cgroup".into(), OsString::from(&cgroup)]);
    flags.extend(["--dedicated-cores-host", "shared"].map(OsString::from));
    let driver = Launch {
        program: built("korpis-driver-firecracker", "korpis-driver-firecracker"),
        args: flags,
    };
    let _guests = Guests(Path::new("/sys/fs/cgroup").join(&cgroup), d.join("state"));
    let fixture = fixture(&d, (driver, socket), shell(GUESTS_RESTART), 27_000 * IDS);
    conforms(&d, &fixture, "firecracker");
}
