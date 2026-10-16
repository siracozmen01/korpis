// SPDX-License-Identifier: AGPL-3.0-or-later

//! The proxy's supervision (§3.2 of docs/07-networking.md, §6 of docs/02-architecture.md). The
//! agent holds the listening sockets the helper opened, listens on a socket of its own for the
//! proxy, and has the helper start the proxy, which connects. The agent sends it the sockets and
//! then every configuration as it changes; when the proxy goes away, the agent has it started
//! again, waiting longer each time it fails quickly. The node says it serves the ingress while
//! the agent holds the sockets, which is what placement relies on.

use crate::held::HeldSet;
use crate::host::Host;
use buffa::Message as _;
use korpis_proto::korpis::ingress::v1 as ingress;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, watch};

/// How long the proxy has to connect once started.
const CONNECT: Duration = Duration::from_secs(10);
/// The longest wait before starting it again.
const MOST: Duration = Duration::from_secs(60);

/// Runs the node's ingress proxy for as long as the agent runs.
#[derive(Debug)]
pub struct Supervisor<H> {
    host: H,
    addresses: Vec<String>,
    control: PathBuf,
    config: watch::Receiver<ingress::IngressConfig>,
    held: watch::Receiver<HeldSet>,
    serving: Arc<AtomicBool>,
    offered: Arc<Notify>,
}

fn framed(body: &[u8]) -> io::Result<Vec<u8>> {
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut out = len.to_be_bytes().to_vec();
    out.extend_from_slice(body);
    Ok(out)
}

/// Sends the first message, `Listeners`, with `sockets` attached: `per_port` plain ones, then as
/// many TLS ones.
async fn hand_over(
    stream: &mut UnixStream,
    sockets: &[OwnedFd],
    per_port: usize,
) -> io::Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let count = u32::try_from(per_port).map_err(io::Error::other)?;
    let first = framed(
        &ingress::Listeners { http: count, https: count, ..Default::default() }.encode_to_vec(),
    )?;
    let fds: Vec<_> = sockets.iter().map(AsFd::as_fd).collect();
    let mut space =
        vec![std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(fds.len()))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("the sockets do not fit their buffer"));
    }
    stream.writable().await?;
    let sent = sendmsg(&*stream, &[io::IoSlice::new(&first)], &mut control, SendFlags::empty())?;
    stream.write_all(first.get(sent..).unwrap_or_default()).await
}

impl<H: Host> Supervisor<H> {
    /// The proxy of a node advertising `addresses`, served on what `config` holds and the
    /// challenges of the set `held`, its control socket at `control`; `serving` says whether the
    /// node holds the ingress's sockets, and `offered` is told when that changes.
    pub fn new(
        host: H,
        addresses: Vec<String>,
        control: PathBuf,
        (config, held): (watch::Receiver<ingress::IngressConfig>, watch::Receiver<HeldSet>),
        (serving, offered): (Arc<AtomicBool>, Arc<Notify>),
    ) -> Self {
        Self { host, addresses, control, config, held, serving, offered }
    }

    /// What the proxy is sent now: the routes and certificates published, and the challenges
    /// of the set held.
    fn current(&mut self) -> ingress::IngressConfig {
        let mut config = self.config.borrow_and_update().clone();
        config.challenges = super::challenges(&self.held.borrow_and_update());
        config
    }

    /// Opens the sockets, then keeps a proxy serving them; never returns.
    pub async fn run(mut self) {
        let mut wait = Duration::from_secs(1);
        let sockets = loop {
            match self.host.open_ingress(&self.addresses).await {
                Ok(s) => break s,
                Err(e) => {
                    tracing::warn!(error = %e, "the ingress's ports could not be opened");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(MOST);
                }
            }
        };
        self.serving.store(true, Ordering::Relaxed);
        self.offered.notify_one();
        let mut wait = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            if let Err(e) = self.serve_once(&sockets).await {
                tracing::warn!(error = %e, "the ingress proxy stopped");
            }
            wait = if started.elapsed() > MOST {
                Duration::from_secs(1)
            } else {
                (wait * 2).min(MOST)
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Starts the proxy, hands it the sockets, and feeds it until it goes away.
    async fn serve_once(&mut self, sockets: &[OwnedFd]) -> io::Result<()> {
        let _ = std::fs::remove_file(&self.control);
        let listener = UnixListener::bind(&self.control)?;
        let confined = self.host.start_ingress(&self.control).await.map_err(io::Error::other)?;
        if !confined.cgroup || !confined.outbound {
            tracing::warn!(
                cgroup = confined.cgroup,
                outbound = confined.outbound,
                "the ingress proxy runs without all of its confinement"
            );
        }
        let (mut stream, _) = tokio::time::timeout(CONNECT, listener.accept())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the proxy never connected"))??;
        drop(listener);
        hand_over(&mut stream, sockets, self.addresses.len()).await?;
        tracing::info!(pid = confined.pid, "the ingress proxy serves");
        loop {
            let config = self.current().encode_to_vec();
            stream.write_all(&framed(&config)?).await?;
            let mut byte = [0u8; 1];
            tokio::select! {
                changed = self.config.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                changed = self.held.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                // The proxy writes nothing: whatever ends a read is the proxy going away.
                _ = stream.read(&mut byte) => {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the proxy closed"));
                }
            }
        }
    }
}
