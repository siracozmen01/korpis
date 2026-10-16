// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress (§3.2 of docs/07-networking.md): the helper opens ports 80 and 443 for the
//! proxy, which needs a privilege neither the supervisor nor the proxy has, and starts the proxy
//! as the supervisor's own user, in a cgroup of its own. Its new connections may then leave only
//! through a workload's link, which the host table enforces by the cgroup, since Landlock, the
//! proxy's own confinement, names ports and not addresses. The helper does not read a byte the
//! proxy serves.

use crate::error::{HelperError, HelperResult};
use crate::network::nft::TABLE;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use rustix::fd::OwnedFd;
use rustix::net::{AddressFamily, SocketAddrAny, SocketType, bind, listen, socket};
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The ports the proxy serves: plain HTTP, then TLS.
pub const PORTS: [u16; 2] = [80, 443];

/// The most addresses the proxy listens on.
pub const MAX_ADDRESSES: usize = 16;

/// The chain confining the proxy's new connections.
const CHAIN: &str = "ingress_output";

/// Where the proxy is, and the cgroup its own is made in.
#[derive(Debug, Clone)]
pub struct IngressConfig {
    /// The `korpis-ingress` binary.
    pub binary: PathBuf,
    /// The cgroup its own is made in, as jobs' are; none to leave it in the helper's.
    pub cgroups: Option<PathBuf>,
}

/// The proxy the helper started, if any.
#[derive(Debug)]
pub struct Ingress {
    config: IngressConfig,
    child: Option<Child>,
}

/// A listening TCP socket on `at`, with the address reusable as a restarted proxy needs.
fn listening(at: SocketAddr) -> std::io::Result<OwnedFd> {
    let family = if at.is_ipv4() { AddressFamily::INET } else { AddressFamily::INET6 };
    let fd = socket(family, SocketType::STREAM, None)?;
    rustix::net::sockopt::set_socket_reuseaddr(&fd, true)?;
    if at.is_ipv6() {
        rustix::net::sockopt::set_ipv6_v6only(&fd, true)?;
    }
    bind(&fd, &SocketAddrAny::from(at))?;
    listen(&fd, 1024)?;
    Ok(fd)
}

/// The addresses asked for, each one a proxy could serve the internet on.
fn addresses(asked: &[String]) -> HelperResult<Vec<IpAddr>> {
    if asked.is_empty() || asked.len() > MAX_ADDRESSES {
        return Err(HelperError::Refused(format!("1 to {MAX_ADDRESSES} addresses")));
    }
    asked
        .iter()
        .map(|a| {
            let ip: IpAddr =
                a.parse().map_err(|_| HelperError::Refused(format!("{a:?} is not an address")))?;
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                return Err(HelperError::Refused(format!("{ip} serves no one outside")));
            }
            Ok(ip)
        })
        .collect()
}

/// Ports 80 and 443 of each of `asked`: every port 80, then every 443.
pub fn open(asked: &[String]) -> HelperResult<Vec<OwnedFd>> {
    let ips = addresses(asked)?;
    let mut out = Vec::with_capacity(ips.len() * PORTS.len());
    for port in PORTS {
        for ip in &ips {
            let at = SocketAddr::new(*ip, port);
            out.push(listening(at).map_err(|e| HelperError::Failed(format!("{at}: {e}")))?);
        }
    }
    Ok(out)
}

/// The rule that lets the proxy in `cgroup`, `level` components below the root, open new
/// connections only through a workload's link, in `nft`'s own syntax: its JSON input does not
/// take a cgroup match in the versions nodes run (nftables 1.0.9 refuses the key it prints).
/// `None` when the path holds a character that has no place in a cgroup the helper names.
fn confining(cgroup: &str, level: usize) -> Option<String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/');
    if cgroup.is_empty() || !cgroup.chars().all(plain) {
        return None;
    }
    Some(format!(
        "add table inet {TABLE}\n\
         add chain inet {TABLE} {CHAIN} {{ type filter hook output priority 0; policy accept; }}\n\
         flush chain inet {TABLE} {CHAIN}\n\
         add rule inet {TABLE} {CHAIN} socket cgroupv2 level {level} \"{cgroup}\" \
         ct state new oifname != \"kw*\" drop\n"
    ))
}

/// Applies `script` with `nft -f -`, in one transaction.
fn apply_text(script: &str) -> std::io::Result<()> {
    let mut nft = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = nft.stdin.take() {
        stdin.write_all(script.as_bytes())?;
    }
    let out = nft.wait_with_output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_owned()))
}

impl Ingress {
    /// No proxy yet, to be started as `config` says.
    #[must_use]
    pub fn new(config: IngressConfig) -> Self {
        Self { config, child: None }
    }

    /// The proxy's cgroup, made beneath the configured one, if it can be.
    fn cgroup(&self) -> Option<PathBuf> {
        let base = self.config.cgroups.as_ref()?;
        let dir = base.join("ingress");
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                tracing::warn!(error = %e, "no cgroup for the ingress proxy");
                None
            }
        }
    }

    /// Starts the proxy as `peer`'s user and group, connecting to `control`.
    pub fn start(
        &mut self,
        control: &str,
        (uid, gid): (u32, u32),
    ) -> HelperResult<helper::StartIngressResponse> {
        let path = Path::new(control);
        if !path.is_absolute() || control.contains('\0') {
            return Err(HelperError::Refused("the control socket is an absolute path".into()));
        }
        self.stop();
        let child = Command::new(&self.config.binary)
            .arg("--control")
            .arg(path)
            .env_clear()
            .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .uid(uid)
            .gid(gid)
            .spawn()
            .map_err(|e| HelperError::Failed(format!("starting the proxy: {e}")))?;
        let pid = child.id();
        self.child = Some(child);
        let (cgroup, outbound) = self.confine(pid);
        Ok(helper::StartIngressResponse { pid, cgroup, outbound, ..Default::default() })
    }

    /// Moves `pid` into the proxy's cgroup and confines what leaves it; what held.
    fn confine(&self, pid: u32) -> (bool, bool) {
        let Some(dir) = self.cgroup() else { return (false, false) };
        if let Err(e) = std::fs::write(dir.join("cgroup.procs"), format!("{pid}\n")) {
            tracing::warn!(error = %e, "the ingress proxy stays in the helper's cgroup");
            return (false, false);
        }
        let Ok(relative) = dir.strip_prefix("/sys/fs/cgroup") else { return (true, false) };
        let level = relative.components().count();
        let Some(rule) = confining(&relative.to_string_lossy(), level) else {
            tracing::warn!(cgroup = %relative.display(), "a cgroup path nft cannot be told");
            return (true, false);
        };
        match apply_text(&rule) {
            Ok(()) => (true, true),
            Err(e) => {
                tracing::warn!(error = %e, "the ingress proxy's connections are not confined");
                (true, false)
            }
        }
    }

    /// Whether the proxy it started still runs.
    pub fn running(&mut self) -> bool {
        self.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Stops the proxy if it runs; whether it did.
    pub fn stop(&mut self) -> bool {
        let Some(mut child) = self.child.take() else { return false };
        let running = matches!(child.try_wait(), Ok(None));
        if running {
            let _ = child.kill();
        }
        let _ = child.wait();
        running
    }
}

/// The answer to an ingress verb from `peer`, with the descriptors it carries; `ingress` is the
/// proxy the helper keeps, none where it starts none.
pub(crate) fn answer(
    ingress: Option<&mut Ingress>,
    peer: (u32, u32),
    verb: Verb,
) -> HelperResult<(Answer, Vec<OwnedFd>)> {
    let refused = || HelperError::Refused("this helper starts no ingress proxy".into());
    Ok(match verb {
        Verb::OpenIngress(r) => {
            let attached = open(&r.addresses)?;
            let count = u32::try_from(attached.len()).unwrap_or(u32::MAX);
            let opened = helper::OpenIngressResponse { attached: count, ..Default::default() };
            (Answer::OpenIngress(Box::new(opened)), attached)
        }
        Verb::StartIngress(r) => {
            let started = ingress.ok_or_else(refused)?.start(&r.control, peer)?;
            (Answer::StartIngress(Box::new(started)), Vec::new())
        }
        Verb::StopIngress(_) => {
            let stopped = ingress.is_some_and(Ingress::stop);
            let answer = helper::StopIngressResponse { stopped, ..Default::default() };
            (Answer::StopIngress(Box::new(answer)), Vec::new())
        }
        _ => return Err(HelperError::Refused("not an ingress verb".into())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_the_internet_could_reach_are_opened() {
        let refused = |a: &[&str]| {
            let a: Vec<String> = a.iter().map(|s| (*s).to_owned()).collect();
            addresses(&a).is_err()
        };
        assert!(refused(&[]));
        assert!(refused(&["127.0.0.1"]) && refused(&["::1"]) && refused(&["0.0.0.0"]));
        assert!(refused(&["224.0.0.1"]) && refused(&["not-an-address"]));
        assert!(!refused(&["203.0.113.10", "2001:db8::10"]));
    }

    #[test]
    fn the_proxys_new_connections_leave_only_through_a_workloads_link() {
        let rule = confining("korpis/ingress", 2).unwrap();
        let wanted =
            r#"socket cgroupv2 level 2 "korpis/ingress" ct state new oifname != "kw*" drop"#;
        assert!(rule.contains(wanted), "{rule}");
        assert!(confining("korpis/x\"; flush ruleset", 2).is_none(), "nothing but a path");
    }
}
