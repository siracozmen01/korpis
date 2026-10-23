// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's network, made by the helper on this machine (§4.1 of docs/07-networking.md): the
//! tap beside the namespace's link, owned by the workload's first id, the workload's address
//! routed to it rather than held, the gateway reached through the host side's own hardware
//! address, and the namespace made again when the kind of interface asked for changes. A guest
//! reaching and reached through it is the firecracker driver's test. Needs root and nftables;
//! skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_helper::network::{Network, host_link, host_mac};
use korpis_proto::korpis::helper::v1 as helper;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;

/// A workload's network, released however the test ends.
struct Held(Network, String);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.release(&self.1);
    }
}

/// What `args` print inside the namespace bound at `path`.
fn inside(path: &Path, args: &[&str]) -> String {
    let net = format!("--net={}", path.display());
    let out = Command::new("nsenter").arg(net).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_microvm_s_namespace_routes_its_address_to_a_tap_its_first_id_owns() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    if !rustix::process::geteuid().is_root() || !network.describe().offered {
        eprintln!("networks need root and a node that offers them: not run");
        return;
    }
    let workload = format!("tap-test-{}", std::process::id());
    let ip = Ipv4Addr::new(10, 245, u8::try_from(std::process::id() % 250).unwrap(), 2);
    let mut request = helper::EnsureNetworkRequest {
        workload: workload.clone(),
        ipv4: ip.to_string(),
        ipv4_pool: "10.245.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        tap: true,
        ..Default::default()
    };
    let made = network.ensure(&request).unwrap();
    let _held = Held(network.clone(), workload.clone());
    let net = Path::new(&made.namespaces).join("net");

    let tap = inside(&net, &["ip", "-d", "-o", "link", "show", "tap0"]);
    assert!(tap.contains("tap") && tap.contains("persist on"), "a persistent tap: {tap}");
    let owner = format!("user {FIRST_WORKLOAD_ID}");
    assert!(tap.contains(&owner), "the jailed VMM's id owns the tap: {tap}");
    let routes = inside(&net, &["ip", "-4", "route"]);
    assert!(routes.contains(&format!("{ip} dev tap0")), "routed to the tap: {routes}");
    let held = inside(&net, &["ip", "-4", "-o", "addr", "show", "dev", "eth0"]);
    assert!(held.trim().is_empty(), "the namespace holds no address of its own: {held}");
    let mac: Vec<String> = host_mac(&workload).iter().map(|b| format!("{b:02x}")).collect();
    let neighbours = inside(&net, &["ip", "neigh", "show", "169.254.1.1"]);
    assert!(
        neighbours.contains(&mac.join(":")) && neighbours.contains("PERMANENT"),
        "the gateway is the host side's own address: {neighbours}"
    );
    let host = Command::new("cat")
        .arg(format!("/sys/class/net/{}/address", host_link(&workload)))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&host.stdout).trim(), mac.join(":"), "and stays it");

    // Asked again, the tap a running guest would hold is left as it is.
    let index = |t: &str| t.split(':').next().unwrap_or_default().to_owned();
    assert_eq!(network.ensure(&request).unwrap(), made);
    let again = inside(&net, &["ip", "-d", "-o", "link", "show", "tap0"]);
    assert_eq!(index(&again), index(&tap));

    // Asked for a veth's end, the namespace is made again, without the tap.
    request.tap = false;
    network.ensure(&request).unwrap();
    assert!(inside(&net, &["ip", "-o", "link", "show", "tap0"]).is_empty(), "no tap is left");
    let held = inside(&net, &["ip", "-4", "-o", "addr", "show", "dev", "eth0"]);
    assert!(held.contains(&format!("{ip}/32")), "the container's end holds it: {held}");
    assert!(network.release(&workload).unwrap());
}
