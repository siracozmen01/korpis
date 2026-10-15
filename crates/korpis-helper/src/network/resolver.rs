// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's resolver (§5.1 of docs/07-networking.md): an address of its namespace's
//! loopback, named alone in the `resolv.conf` its container is given, and the sockets on that
//! address's port 53, opened inside the namespace for the supervisor to answer on. A query never
//! leaves the namespace, so no firewall of the host's, Korpis's or another's, stands between a
//! workload and its resolver, and the supervisor knows which workload asked by the socket it
//! arrived on.

use korpis_core::workload::network::RESOLVER_V4;
use rustix::fd::OwnedFd;
use rustix::net::{AddressFamily, SocketFlags, SocketType, sockopt};
use std::io::Write as _;
use std::net::SocketAddrV4;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// The file in a workload's network directory the container is given as `/etc/resolv.conf`.
pub(crate) const CONF: &str = "resolv.conf";

fn socket(kind: SocketType) -> std::io::Result<OwnedFd> {
    let fd = rustix::net::socket_with(AddressFamily::INET, kind, SocketFlags::CLOEXEC, None)?;
    sockopt::set_socket_reuseaddr(&fd, true)?;
    rustix::net::bind(&fd, &SocketAddrV4::new(RESOLVER_V4, 53))?;
    Ok(fd)
}

/// The resolver's sockets, UDP then TCP listening, opened in the calling thread's network
/// namespace, which holds the resolver's address.
pub(crate) fn sockets() -> std::io::Result<Vec<OwnedFd>> {
    let udp = socket(SocketType::DGRAM)?;
    let tcp = socket(SocketType::STREAM)?;
    rustix::net::listen(&tcp, 128)?;
    Ok(vec![udp, tcp])
}

/// Writes the `resolv.conf` naming the resolver alone into the network directory `dir`,
/// readable by every user, since the container's users are not the host's.
pub(crate) fn write(dir: &Path) -> std::io::Result<()> {
    let staged = dir.join(format!(".{CONF}.new"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&staged)?;
    f.write_all(format!("nameserver {RESOLVER_V4}\n").as_bytes())?;
    f.sync_all()?;
    std::fs::rename(staged, dir.join(CONF))
}
