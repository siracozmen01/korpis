// SPDX-License-Identifier: AGPL-3.0-or-later

//! The connection the agent feeds the proxy over (ingress.proto): the proxy connects to the Unix
//! socket the agent names, receives its listening sockets attached to the first message, and then
//! reads one whole configuration after another. A configuration that does not parse, or names a
//! certificate whose key is not its own, is refused and the one before it kept, so a mistake of
//! the agent's never takes down what the proxy serves.

use crate::serve::Listeners;
use crate::table::Table;
use buffa::Message as _;
use korpis_proto::korpis::ingress::v1 as pb;
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::sync::watch;

/// The largest message the agent sends: every route's certificate chain and key.
pub const MAX: usize = 64 << 20;

/// The most sockets the agent hands over: ports 80 and 443 on as many addresses as a node has.
pub const MAX_LISTENERS: usize = 64;

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

fn length(head: [u8; 4]) -> io::Result<usize> {
    let len = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
    if len > MAX {
        return Err(invalid("a message above the limit"));
    }
    Ok(len)
}

/// Connects to the agent at `path` and receives the sockets to serve. Blocking: it runs before
/// the proxy confines itself and starts its runtime.
pub fn connect(path: &Path) -> io::Result<(UnixStream, Listeners)> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
    let mut s = UnixStream::connect(path)?;
    let mut head = [0u8; 4];
    let mut space =
        [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_LISTENERS))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let got =
        recvmsg(&s, &mut [io::IoSliceMut::new(&mut head)], &mut control, RecvFlags::CMSG_CLOEXEC)?;
    let mut attached: Vec<OwnedFd> = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = message {
            attached.extend(fds);
        }
    }
    if got.bytes == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the agent closed"));
    }
    s.read_exact(head.get_mut(got.bytes..).unwrap_or_default())?;
    let mut body = vec![0u8; length(head)?];
    s.read_exact(&mut body)?;
    let said = pb::Listeners::decode_from_slice(&body).map_err(|e| invalid(e.to_string()))?;
    let listeners = split(&said, attached)?;
    Ok((s, listeners))
}

/// The sockets attached, as the message says: `http` plain ones, then `https` TLS ones.
fn split(said: &pb::Listeners, attached: Vec<OwnedFd>) -> io::Result<Listeners> {
    let http = usize::try_from(said.http).unwrap_or(usize::MAX);
    let https = usize::try_from(said.https).unwrap_or(usize::MAX);
    if http.checked_add(https) != Some(attached.len()) {
        return Err(invalid(format!("{} sockets attached, {http} + {https} said", attached.len())));
    }
    let mut all = attached.into_iter().map(std::net::TcpListener::from);
    let http = all.by_ref().take(http).collect();
    Ok(Listeners { http, https: all.collect() })
}

/// Reads configurations from `r` until the agent closes it, publishing each one that holds.
pub async fn configs(
    mut r: impl AsyncRead + Unpin,
    publish: &watch::Sender<Arc<Table>>,
) -> io::Result<()> {
    loop {
        let mut head = [0u8; 4];
        match r.read_exact(&mut head).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        let mut body = vec![0u8; length(head)?];
        r.read_exact(&mut body).await?;
        let table = pb::IngressConfig::decode_from_slice(&body)
            .map_err(|e| e.to_string())
            .and_then(|c| Table::from_pb(&c).map_err(|e| e.to_string()));
        match table {
            Ok(t) => {
                publish.send_replace(Arc::new(t));
            }
            Err(e) => tracing::error!(error = %e, "a configuration refused; serving the last"),
        }
    }
}
