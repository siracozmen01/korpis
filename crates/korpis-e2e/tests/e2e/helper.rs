// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's privileged helper, serving from a thread of the test process as `korpis-helper`
//! serves from its own.

use crate::node::built as built_binary;
use korpis_helper::cores::{Cores, CoresConfig};
use std::path::{Path, PathBuf};

/// The helper, serving on `socket` from its own thread for the rest of the process, running
/// backup and restore jobs with the job binary as a node's helper does, which read recipe content
/// from `content`, the agent's content store, and file sessions with the file worker; and, where
/// `parent` names the node's driver's cgroup parent, keeping the cpusets beneath it alone, as
/// `--cores-cgroup` naming it would.
pub(crate) fn helper(
    socket: &Path,
    (volumes, shared): (&Path, Option<&Path>),
    dirs: (&Path, &Path, &Path),
    parent: Option<&str>,
) {
    serve_on(socket, built((volumes, shared), dirs, parent), None);
}

/// As [`helper`], serving from a thread inside the network namespace at `node`, as a node's own
/// helper runs in its host's, and keeping the mesh's interface there, its key with the rest of
/// its state (§9.1 of docs/07-networking.md). Nothing it makes is in this machine's own
/// namespace.
#[allow(dead_code, reason = "the browser test, a binary of its own, runs no mesh")]
pub(crate) fn helper_in(socket: &Path, volumes: &Path, dirs: (&Path, &Path, &Path), node: &Path) {
    let overlay = korpis_helper::overlay::Overlay::new(dirs.0.join("overlay.key"));
    let h = built((volumes, None), dirs, None).with_overlay(overlay);
    serve_on(socket, h, Some(node.to_owned()));
}

/// A btrfs made for a test in `dir` and mounted there, qgroups on, so its class can snapshot
/// and send; where it is mounted, or `None` where this machine cannot make one.
pub(crate) fn btrfs_in(dir: &Path) -> Option<std::path::PathBuf> {
    use std::process::Command;
    let image = dir.join("btrfs.img");
    std::fs::File::create(&image).unwrap().set_len(512 << 20).unwrap();
    let at = dir.join("fs");
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
    Some(at)
}

/// The helper with its storage, jobs, network, ingress, and file sessions, and the cpusets
/// beneath `parent` where one is named; and a shared class `shared` on the directory `shared`
/// names, where it names one, which another node of the test shares as a network filesystem
/// would be shared (§2.1 of docs/06-storage.md).
fn built(
    (volumes, shared): (&Path, Option<&Path>),
    (state, content, netns): (&Path, &Path, &Path),
    parent: Option<&str>,
) -> korpis_helper::helper::Helper {
    use korpis_helper::storage::{Class, Network};
    let mut classes = vec![Class::open("local", volumes).unwrap()];
    classes.extend(shared.map(|s| Class::shared("shared", s, Network::Assumed).unwrap()));
    let ledger = korpis_helper::ids::Ledger::open(state).unwrap();
    let jobs = korpis_helper::job::JobConfig {
        binary: built_binary("korpis-backup", "korpis-backup-job"),
        install_binary: Some(built_binary("korpis-install", "korpis-install-job")),
        state: state.to_owned(),
        cgroups: None,
        content: Some(content.to_owned()),
    };
    let network = korpis_helper::network::Network::new(netns.to_owned());
    // As `--ingress-binary` beside the helper would, the proxy's cgroup made beneath one of the
    // test's own, which the node removes when it is dropped.
    let ingress =
        korpis_helper::ingress::IngressConfig { binary: proxy(), cgroups: Some(cgroups(state)) };
    let mut h = korpis_helper::helper::Helper::new(classes, ledger)
        .with_jobs(jobs)
        .with_network(network)
        .with_ingress(korpis_helper::ingress::Ingress::new(ingress))
        .with_files(korpis_helper::files::Files::new(korpis_helper::files::FilesConfig {
            binary: built_binary("korpis-files", "korpis-fsworker"),
            state: state.to_owned(),
            cgroups: None,
        }));
    if let Some(parent) = parent {
        let config = CoresConfig {
            root: "/sys/fs/cgroup".into(),
            online: "/sys/devices/system/cpu/online".into(),
            parents: vec![parent.into()],
        };
        h = h.with_cores(Cores::new(config).unwrap());
    }
    h
}

/// Serves `h` on `socket` from its own thread for the rest of the process, entering the network
/// namespace at `netns` first where one is named.
fn serve_on(socket: &Path, mut h: korpis_helper::helper::Helper, netns: Option<PathBuf>) {
    let listener = korpis_helper::serve::bound(socket).unwrap();
    std::thread::spawn(move || {
        if let Some(ns) = netns {
            let ns = std::fs::File::open(ns).unwrap();
            let kind = Some(rustix::thread::LinkNameSpaceType::Network);
            rustix::thread::move_into_link_name_space(std::os::fd::AsFd::as_fd(&ns), kind).unwrap();
        }
        korpis_helper::serve::serve(&listener, &[0].into(), &mut h, None)
    });
}

/// The ingress proxy, built where this test was, once per test process.
fn proxy() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| built_binary("korpis-ingress", "korpis-ingress")).clone()
}

/// The cgroup the helper whose state is at `state` makes its ingress proxy's beneath: one of
/// this test process's own, named for that helper, so no two nodes share it.
pub(crate) fn cgroups(state: &Path) -> PathBuf {
    let name = state.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy();
    PathBuf::from(format!("/sys/fs/cgroup/korpis-e2e-{}-{name}", std::process::id()))
}
