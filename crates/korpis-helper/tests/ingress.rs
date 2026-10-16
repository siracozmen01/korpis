// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress, as the helper keeps it (§3.2 of docs/07-networking.md): ports 80 and 443
//! opened on the addresses asked, and the proxy started as its asker in a cgroup of its own whose
//! new connections leave only through a workload's link. Needs root, cgroup v2, and nftables;
//! skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_helper::ingress::{Ingress, IngressConfig, open};
use std::net::{IpAddr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;

fn root() -> bool {
    let root = rustix::process::geteuid().is_root();
    if !root {
        eprintln!("the ingress needs root: not run");
    }
    root
}

fn host_address() -> Option<IpAddr> {
    let out = Command::new("ip").args(["-4", "-o", "route", "get", "1.1.1.1"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut words = text.split_whitespace();
    words.find(|w| *w == "src")?;
    words.next()?.parse().ok()
}

#[test]
fn ports_80_and_443_are_opened_listening_on_the_address_asked() {
    let Some(address) = host_address().filter(|_| root()) else { return };
    let opened = open(&[address.to_string()]).unwrap();
    let ports: Vec<_> =
        opened.into_iter().map(|fd| TcpListener::from(fd).local_addr().unwrap()).collect();
    assert_eq!(ports.iter().map(std::net::SocketAddr::port).collect::<Vec<_>>(), [80, 443]);
    assert!(ports.iter().all(|p| p.ip() == address));
    assert!(open(&["127.0.0.1".into()]).is_err(), "loopback serves no one outside");
}

#[test]
fn the_proxy_runs_as_its_asker_in_a_cgroup_of_its_own_under_the_outbound_rule() {
    let cgroups = PathBuf::from(format!("/sys/fs/cgroup/korpis-test-{}", std::process::id()));
    if !root() || std::fs::create_dir_all(&cgroups).is_err() {
        eprintln!("no cgroup v2 to make one in: not run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("korpis-ingress");
    std::fs::write(&binary, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut ingress = Ingress::new(IngressConfig { binary, cgroups: Some(cgroups.clone()) });
    let started = ingress.start("/run/korpis-agent/ingress.sock", (65534, 65534)).unwrap();
    assert!(started.cgroup, "in its own cgroup");
    assert!(started.outbound, "its new connections confined");
    let procs = std::fs::read_to_string(cgroups.join("ingress/cgroup.procs")).unwrap();
    assert!(procs.lines().any(|p| p == started.pid.to_string()), "{procs}");
    let status = std::fs::read_to_string(format!("/proc/{}/status", started.pid)).unwrap();
    assert!(status.lines().any(|l| l.starts_with("Uid:") && l.contains("65534")), "{status}");
    let chain = ["list", "chain", "inet", "korpis", "ingress_output"];
    let rules = Command::new("nft").args(chain).output().unwrap();
    let listed = String::from_utf8_lossy(&rules.stdout).into_owned();
    assert!(listed.contains("socket cgroupv2") && listed.contains("kw*"), "{listed}");
    assert!(ingress.running());
    assert!(ingress.stop(), "it ran");
    assert!(!ingress.running());
    let _ =
        Command::new("nft").args(["delete", "chain", "inet", "korpis", "ingress_output"]).status();
    let _ = std::fs::remove_dir(cgroups.join("ingress"));
    let _ = std::fs::remove_dir(&cgroups);
}

#[test]
fn a_control_socket_that_is_not_an_absolute_path_is_refused() {
    let config = IngressConfig { binary: "/bin/true".into(), cgroups: None };
    assert!(Ingress::new(config).start("relative.sock", (65534, 65534)).is_err());
}
