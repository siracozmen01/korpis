// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the edge tests share: network namespaces of the test's own, the links between them, and
//! whether this kernel makes the links edges need.

#![allow(dead_code, reason = "each test uses what it needs of these")]

use korpis_helper::overlay::Overlay;
use korpis_proto::korpis::helper::v1 as helper;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A network namespace of the test's own, removed when dropped.
pub struct Netns(pub String);

impl Netns {
    pub fn new(name: String) -> Self {
        assert!(ip(&["netns", "add", &name]), "netns {name}");
        assert!(ip(&["-n", &name, "link", "set", "lo", "up"]));
        Self(name)
    }

    pub fn path(&self) -> PathBuf {
        Path::new("/run/netns").join(&self.0)
    }

    /// `args` run inside it; whether they succeeded.
    pub fn run(&self, args: &[&str]) -> bool {
        let mut all = vec!["netns", "exec", &self.0];
        all.extend_from_slice(args);
        ip(&all)
    }

    /// `f` run on a thread inside it.
    pub fn within<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let path = self.path();
        std::thread::spawn(move || {
            let ns = std::fs::File::open(path).unwrap();
            rustix::thread::move_into_link_name_space(
                std::os::fd::AsFd::as_fd(&ns),
                Some(rustix::thread::LinkNameSpaceType::Network),
            )
            .unwrap();
            f()
        })
        .join()
        .unwrap()
    }

    pub fn keep(&self, o: &Overlay, r: helper::KeepOverlayRequest) -> helper::KeepOverlayResponse {
        let o = o.clone();
        self.within(move || o.keep(&r).unwrap())
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = ip(&["netns", "del", &self.0]);
    }
}

pub fn ip(args: &[&str]) -> bool {
    Command::new("ip").args(args).output().is_ok_and(|o| o.status.success())
}

/// A veth from `a` to `b`, each end named and addressed as given, both up.
pub fn wire(a: (&Netns, &str, &str), b: (&Netns, &str, &str)) {
    assert!(ip(&[
        "link", "add", a.1, "netns", &a.0.0, "type", "veth", "peer", "name", b.1, "netns", &b.0.0
    ]));
    for (ns, link, addr) in [a, b] {
        assert!(ns.run(&["ip", "addr", "add", addr, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
}

/// Whether this kernel makes WireGuard and GRE links, as `ns` asks.
pub fn capable(ns: &Netns) -> bool {
    let wg = ns.run(&["ip", "link", "add", "kedg-wg", "type", "wireguard"])
        && ns.run(&["ip", "link", "del", "kedg-wg"]);
    let gre = ns.run(&[
        "ip",
        "link",
        "add",
        "kedg-gre",
        "type",
        "gre",
        "local",
        "198.18.0.1",
        "remote",
        "198.18.0.2",
    ]) && ns.run(&["ip", "link", "del", "kedg-gre"]);
    wg && gre
}
