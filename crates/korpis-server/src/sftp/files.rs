// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SFTP half: version 3 packets read from the channel, each answered by the session's verbs
//! (§3.2 of docs/06-storage.md). Reads and writes of an open file go to the node as they arrive,
//! up to as many as the node holds unanswered, and are answered as the node answers them, so a
//! client that keeps several in flight, as every client does, is not held to one round trip each;
//! any other request waits for those before it, so it sees what they did. A packet that cannot be
//! read ends the connection, which is all a stream that has lost its framing can do.

use super::Authorizer;
use super::answers::{refused, status};
use super::node::{Answered, FileSession, Lost};
use crate::relay::Verifier;
use bytes::Bytes;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use russh_sftp::protocol::{Data, Packet, StatusCode};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

/// The action that writes, and reads.
pub(super) const WRITE: &str = "workload.file.write";
/// How many reads and writes may wait on the node: as many as it holds unanswered.
const WINDOW: usize = 16;
/// The longest packet a client may send: a write of 256 KiB, and its header.
const PACKET: u32 = 257 * 1024;
/// The most a read returns, as OpenSSH's own server bounds it.
const READ_MAX: u32 = 255 * 1024;

/// A handle the client holds.
pub(super) enum Open {
    /// A file, opened to write when `write`, every write at its end when `append`.
    File { path: String, write: bool, append: bool },
    /// A directory being listed: the name the last page ended after, and whether it was the last.
    Dir { path: String, after: String, done: bool },
}

/// A read or a write the node has not answered yet, by the client's request number.
enum Waiting {
    Read(u32),
    Write(u32),
}

/// One SFTP connection's state.
pub(super) struct Files<V> {
    pub(super) session: FileSession<V>,
    /// Whether the token allows writing.
    pub(super) write: bool,
    pub(super) handles: HashMap<String, Open>,
    pub(super) next_handle: u64,
    waiting: VecDeque<Waiting>,
}

type Packets = mpsc::Receiver<Result<Packet, String>>;

/// Reads packets from `from` into `to` until it ends or one cannot be read.
async fn read_packets<R: AsyncRead + Unpin>(mut from: R, to: mpsc::Sender<Result<Packet, String>>) {
    loop {
        let Ok(length) = from.read_u32().await else { return };
        if length > PACKET {
            let _ = to.send(Err(format!("a packet of {length} bytes"))).await;
            return;
        }
        let mut body = vec![0u8; usize::try_from(length).unwrap_or(0)];
        if from.read_exact(&mut body).await.is_err() {
            return;
        }
        let packet = Packet::try_from(&mut Bytes::from(body)).map_err(|e| e.to_string());
        let unreadable = packet.is_err();
        if to.send(packet).await.is_err() || unreadable {
            return;
        }
    }
}

/// Writes `packet` to `to`.
async fn send<W: AsyncWrite + Unpin>(to: &mut W, packet: Packet) -> Result<(), Lost> {
    let bytes = Bytes::try_from(packet).map_err(|e| e.to_string())?;
    to.write_all(&bytes).await.map_err(|e| e.to_string())?;
    to.flush().await.map_err(|e| e.to_string())
}

impl<V: Verifier> Files<V> {
    /// A connection over `session`, which writes when `write`.
    pub(super) fn new(session: FileSession<V>, write: bool) -> Self {
        let (handles, waiting) = (HashMap::new(), VecDeque::new());
        Self { session, write, handles, next_handle: 0, waiting }
    }

    /// Serves `stream` until the client is done or the session is lost, then ends the session.
    pub(super) async fn serve<S, A>(mut self, stream: S, authorizer: Arc<A>)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
        A: Authorizer,
    {
        let (from, mut to) = tokio::io::split(stream);
        let (sender, mut packets) = mpsc::channel(WINDOW);
        let reading = tokio::spawn(read_packets(from, sender));
        if let Err(e) = self.run(&mut packets, &mut to, &*authorizer).await {
            tracing::debug!(error = %e, "an sftp session ended");
        }
        reading.abort();
        self.session.end().await;
        let _ = to.shutdown().await;
    }

    async fn run<W: AsyncWrite + Unpin, A: Authorizer>(
        &mut self,
        packets: &mut Packets,
        to: &mut W,
        authorizer: &A,
    ) -> Result<(), Lost> {
        loop {
            let renew_in = self.session.renew_in();
            let (room, waiting) = (self.waiting.len() < WINDOW, !self.waiting.is_empty());
            tokio::select! {
                packet = packets.recv(), if room => match packet {
                    Some(packet) => self.take(packet?, to, authorizer).await?,
                    None => return self.drain(to, authorizer).await,
                },
                heard = self.session.heard(), if waiting => {
                    if let Some(answer) = self.session.answer(heard)? {
                        send(to, self.answered(answer)?).await?;
                    }
                }
                () = tokio::time::sleep(renew_in) => self.session.renew(authorizer).await?,
            }
        }
    }

    /// Takes in `packet`: a read or a write sent on, anything else answered once what is ahead
    /// of it is.
    async fn take<W: AsyncWrite + Unpin, A: Authorizer>(
        &mut self,
        packet: Packet,
        to: &mut W,
        authorizer: &A,
    ) -> Result<(), Lost> {
        let sent = match &packet {
            Packet::Read(r) => match self.handles.get(&r.handle) {
                Some(Open::File { path, .. }) => {
                    let length = r.len.min(READ_MAX);
                    let read = pb::FileRead {
                        path: path.clone(),
                        offset: r.offset,
                        length,
                        ..Default::default()
                    };
                    Some((Request::Read(Box::new(read)), Waiting::Read(r.id)))
                }
                _ => None,
            },
            Packet::Write(w) => match self.handles.get(&w.handle) {
                Some(Open::File { path, write: true, append: false }) => {
                    let write = pb::FileWrite {
                        path: path.clone(),
                        offset: w.offset,
                        data: w.data.clone(),
                        ..Default::default()
                    };
                    Some((Request::Write(Box::new(write)), Waiting::Write(w.id)))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some((request, waiting)) = sent {
            self.session.send(request).await?;
            self.waiting.push_back(waiting);
            return Ok(());
        }
        self.drain(to, authorizer).await?;
        let reply = self.serially(packet, authorizer).await?;
        send(to, reply).await
    }

    /// Answers every read and write the node has not answered yet.
    async fn drain<W: AsyncWrite + Unpin, A: Authorizer>(
        &mut self,
        to: &mut W,
        authorizer: &A,
    ) -> Result<(), Lost> {
        while !self.waiting.is_empty() {
            let renew_in = self.session.renew_in();
            tokio::select! {
                heard = self.session.heard() => {
                    if let Some(answer) = self.session.answer(heard)? {
                        send(to, self.answered(answer)?).await?;
                    }
                }
                () = tokio::time::sleep(renew_in) => self.session.renew(authorizer).await?,
            }
        }
        Ok(())
    }

    /// The reply to the oldest read or write waiting, which `answer` answers.
    fn answered(&mut self, answer: Answered) -> Result<Packet, Lost> {
        let waiting = self.waiting.pop_front().ok_or("an answer to nothing")?;
        let id = match waiting {
            Waiting::Read(id) | Waiting::Write(id) => id,
        };
        Ok(match (waiting, answer) {
            (Waiting::Read(_), Ok(Response::Data(data))) if data.is_empty() => {
                Packet::Status(status(id, StatusCode::Eof, ""))
            }
            (Waiting::Read(_), Ok(Response::Data(data))) => Packet::Data(Data { id, data }),
            (Waiting::Write(_), Ok(Response::Written(_))) => {
                Packet::Status(status(id, StatusCode::Ok, ""))
            }
            (_, Err(e)) => Packet::Status(refused(id, &e)),
            (_, Ok(_)) => return Err("an answer of another kind than its request".to_owned()),
        })
    }
}
