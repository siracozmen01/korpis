// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `firecracker` driver on a test's node, beside the `oci` driver (§3 of
//! docs/04-runtimes.md): whether this machine can run a guest, the driver serving in the node's
//! directory over the agent's content store and the helper's volumes and namespaces, and every
//! guest ended as the node is dropped.

use crate::node::{built, writer};
use korpis_agent::state::StateDir;
use korpis_driver_firecracker::machine::Machine;
use korpis_driver_firecracker::settings::Settings;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether this machine can run a guest: KVM, and Firecracker, its jailer, and the guest kernel
/// where the repository's tools put them.
pub fn able() -> bool {
    let tools = repo().join(".tools");
    let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
    kvm && ["bin/firecracker", "bin/jailer", "guest/vmlinux"].iter().all(|p| tools.join(p).exists())
}

/// `package`'s `bin`, built static for the guest where this test was.
pub(crate) fn musl(package: &str, bin: &str) -> PathBuf {
    const TRIPLE: &str = "x86_64-unknown-linux-musl";
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let target = profile_dir.parent().unwrap();
    let name = profile_dir.file_name().unwrap().to_str().unwrap();
    let profile = if name == "debug" { "dev" } else { name };
    let made = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", package, "--bin", bin, "--target", TRIPLE])
        .args(["--profile", profile, "--target-dir"])
        .arg(target)
        .status()
        .unwrap();
    assert!(made.success(), "{bin} did not build");
    target.join(TRIPLE).join(name).join(bin)
}

/// The `firecracker` driver, serving in `dir` for the rest of the test as a node's would, reading
/// the agent's content store and the helper's volumes and namespaces: its socket, and the cgroup
/// its guests run beneath.
pub fn serve(dir: &Path, state: &StateDir, (volumes, netns): (&Path, &Path)) -> (PathBuf, PathBuf) {
    let tools = repo().join(".tools");
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let settings = Settings {
        firecracker: tools.join("bin/firecracker"),
        jailer: tools.join("bin/jailer"),
        kernel: tools.join("guest/vmlinux"),
        guest: musl("korpis-guest", "korpis-guest"),
        fsworker: musl("korpis-files", "korpis-fsworker"),
        install_job: musl("korpis-install", "korpis-install-job"),
        image_build: built("korpis-image", "korpis-image-build"),
        writer: writer(),
        content: state.root().join("content"),
        volume_root: volumes.to_owned(),
        netns_root: netns.to_owned(),
        identity_root: state.root().join("identity"),
        state: dir.join("firecracker"),
        cgroup: format!("korpis-e2e-{}-{name}-firecracker", std::process::id()),
        cores_host: korpis_driver_kit::cores::Host::Shared,
    };
    let cgroup = settings.cgroup_parent();
    let machine = Machine::new(settings).unwrap();
    let socket = dir.join("firecracker.sock");
    let listener = korpis_driver_kit::serve::bind(&socket).unwrap();
    let driver = korpis_driver_firecracker::service::Driver::new(machine);
    let allowed = BTreeSet::from([0]);
    let pending = std::future::pending();
    tokio::spawn(korpis_driver_kit::serve::serve(listener, allowed, driver, pending));
    (socket, cgroup)
}

/// Ends every guest beneath `cgroup`, removes the cgroups, and unbinds every volume a guest's
/// jail beneath the driver's `state` still holds, since a guest the test did not destroy keeps
/// them bound, and its directory could not be removed.
pub fn end_guests(cgroup: &Path, state: &Path) {
    korpis_driver_kit::cores::remove_under(cgroup);
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut beneath: Vec<&str> = mounts
        .lines()
        .filter_map(|l| l.split(' ').nth(4))
        .filter(|at| Path::new(at).starts_with(state))
        .collect();
    beneath.sort_by_key(|at| std::cmp::Reverse(at.len()));
    for at in beneath {
        let _ = Command::new("umount").arg("-l").arg(at).status();
    }
}
