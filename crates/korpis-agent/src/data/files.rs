// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's files over the data path: the handler of `workload.file.read` and
//! `workload.file.write`, which implies it (§3.2 of docs/06-storage.md). The node names the
//! workload's volumes from the assignment it holds, each at the path the workload mounts it at and
//! read-only where the workload mounts it so, has the helper start a worker confined to them, or,
//! where the workload's driver serves file sessions inside its object, as a microVM's does (§4.4 of
//! docs/04-runtimes.md), opens the session through the driver, and carries the session's frames to
//! the worker and back unchanged: every frame up is one
//! `FileRequest`, every frame down the `FileResponse` answering it, in order. A client may send
//! requests ahead of the answers; the node holds at most [`AHEAD`] unanswered.
//!
//! The session is an effect at both ends, under the grant its token was minted from: its opening
//! is journaled before the worker is asked for, so a node that dies during a session leaves its
//! opening on record, and its end is journaled with what it changed ([`tally`]), failed when it
//! did not end cleanly. Reads are not recorded one by one.

mod tally;

use super::console_write::Runtimes;
use super::handler::{Ended, Handler, Io, Opened, Serving};
use crate::held::HeldSet;
use crate::host::Host;
use crate::journal::JournalHandle;
use crate::runtime::Runtime;
use buffa::Message as _;
use korpis_core::principal::Principal;
use korpis_files::frame::LIMIT;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use tally::{Pending, Tally};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, watch};

/// The action that reads.
pub const READ: &str = "workload.file.read";
/// The action that writes, and reads.
pub const WRITE: &str = "workload.file.write";

/// The most requests a client has sent that the worker has not answered.
const AHEAD: usize = 16;

/// A worker's end of a session, whichever serves it.
trait Worker: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Worker for T {}

/// Serves file sessions through the node's helper, or the workload's driver.
#[derive(Debug, Clone)]
pub struct FileSessions<H, R> {
    host: H,
    runtime: R,
    runtimes: Runtimes,
    held: watch::Receiver<HeldSet>,
    journal: JournalHandle,
}

impl<H: Host + Clone + Send + Sync + 'static, R: Runtime + Clone> FileSessions<H, R> {
    /// Opens sessions on the workloads `held` names through `host`, or through `runtime` on the
    /// object `runtimes` names where its driver serves them, journaling each.
    #[must_use]
    pub fn new(
        (host, runtime): (H, R),
        runtimes: Runtimes,
        held: watch::Receiver<HeldSet>,
        journal: JournalHandle,
    ) -> Self {
        Self { host, runtime, runtimes, held, journal }
    }
}

impl<H: Host + Clone + Send + Sync + 'static, R: Runtime + Clone> Handler for FileSessions<H, R> {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(serve(self.clone(), opened, io))
    }
}

fn ended(code: &str, message: impl Into<String>) -> Ended {
    Ended { code: code.to_owned(), message: message.into() }
}

fn text(s: &str) -> pb::Value {
    pb::Value { kind: Some(pb::value::Kind::StringValue(s.to_owned())), ..Default::default() }
}

/// What the helper is asked to open for `opened`, from the assignment held for its workload, and
/// the workload's id range.
fn request(held: &HeldSet, opened: &Opened) -> Result<(helper::OpenFilesRequest, u32), Ended> {
    let a = held
        .assignments
        .get(&opened.workload)
        .ok_or_else(|| ended("unavailable", "the workload is not held here"))?;
    let body = a.target.as_option().and_then(|t| t.body.as_option());
    let volumes: Vec<helper::FileVolume> = body
        .map(|b| b.volumes.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|v| helper::FileVolume {
            path: v.path.clone(),
            volume: crate::reconcile::volume_id(a, &v.name),
            read_only: v.mode.as_known() == Some(pb::VolumeMode::VOLUME_MODE_RO),
            ..Default::default()
        })
        .collect();
    if volumes.is_empty() {
        return Err(ended("no_volumes", "the workload has no volumes"));
    }
    let r = helper::OpenFilesRequest {
        workload: a.workload.clone(),
        id_base: a.uid_base,
        volumes,
        write: opened.action == WRITE,
        ..Default::default()
    };
    Ok((r, a.uid_count))
}

impl<H: Host, R: Runtime> FileSessions<H, R> {
    /// Journals the session's `event`, with `fields`, as `opened`'s subject's act.
    async fn record(
        &self,
        opened: &Opened,
        actor: &Principal,
        (event, fields, error): (&str, BTreeMap<String, pb::Value>, Option<&Ended>),
    ) -> Result<(), Ended> {
        let mut after = fields;
        after.insert("session".to_owned(), text(&opened.session));
        after.insert("event".to_owned(), text(event));
        let outcome = if error.is_some() {
            pb::EffectOutcome::EFFECT_OUTCOME_FAILED
        } else {
            pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED
        };
        let error = error.map(|e| pb::Error {
            reason: format!("file.{}", e.code),
            message: e.message.clone(),
            ..Default::default()
        });
        let e = pb::Effect {
            workload: Some(opened.workload.clone()),
            action: opened.action.clone(),
            outcome: outcome.into(),
            actor: korpis_wire::principal::to_pb(actor).into(),
            grant: opened
                .chain
                .first()
                .map(|g| korpis_wire::wire_id(korpis_core::id::IdKind::Grant, *g)),
            error: error.into(),
            after,
            ..Default::default()
        };
        self.journal
            .append(Body::Effect(Box::new(e)))
            .await
            .map(|_| ())
            .map_err(|e| ended("unavailable", format!("the session could not be recorded: {e}")))
    }
}

async fn serve<H: Host, R: Runtime>(
    files: FileSessions<H, R>,
    opened: Opened,
    io: Io,
) -> Result<(), Ended> {
    let actor = Principal::from_key(&opened.subject)
        .map_err(|e| ended("refused", format!("the session's subject: {e}")))?;
    let (r, count) = request(&files.held.borrow(), &opened)?;
    files.record(&opened, &actor, ("opened", BTreeMap::new(), None)).await?;
    let mut tally = Tally::default();
    let carried = carry(&files, (&r, count), io, &mut tally).await;
    files.record(&opened, &actor, ("ended", tally.fields(), carried.as_ref().err())).await?;
    carried
}

/// The worker serving `r`: inside the workload's object where its driver serves file sessions
/// there, and otherwise the helper's, on the host.
async fn open<H: Host, R: Runtime>(
    files: &FileSessions<H, R>,
    (r, count): (&helper::OpenFilesRequest, u32),
) -> Result<Box<dyn Worker>, Ended> {
    let unavailable = |e: &dyn std::fmt::Display| ended("unavailable", e.to_string());
    let object = files.runtimes.borrow().get(&r.workload).cloned();
    if let Some(id) = object
        && files.runtime.serves_files(&id).await.map_err(|e| unavailable(&e))?
    {
        let stream = files.runtime.files(&id, r.write).await.map_err(|e| unavailable(&e))?;
        return Ok(Box::new(stream));
    }
    files.host.ensure_ids(&r.workload, r.id_base, count).await.map_err(|e| unavailable(&e))?;
    let socket = files.host.open_files(r).await.map_err(|e| unavailable(&e))?;
    let socket = std::os::unix::net::UnixStream::from(socket);
    let worker = socket
        .set_nonblocking(true)
        .and_then(|()| tokio::net::UnixStream::from_std(socket))
        .map_err(|e| ended("unavailable", format!("the worker's socket: {e}")))?;
    Ok(Box::new(worker))
}

/// Opens the worker for `r` and carries the session to it until either side ends.
async fn carry<H: Host, R: Runtime>(
    files: &FileSessions<H, R>,
    (r, count): (&helper::OpenFilesRequest, u32),
    io: Io,
    tally: &mut Tally,
) -> Result<(), Ended> {
    let (from_worker, to_worker) = tokio::io::split(open(files, (r, count)).await?);
    let (ahead, answered) = mpsc::channel(AHEAD);
    let forward = forward(io.incoming, to_worker, ahead);
    let back = back(from_worker, io.outgoing, answered, tally);
    tokio::pin!(forward, back);
    let mut forwarding = true;
    loop {
        tokio::select! {
            done = &mut forward, if forwarding => {
                done?;
                forwarding = false;
            }
            done = &mut back => return done,
        }
    }
}

/// Sends each request the client sends to the worker, noting what it may change; once the client
/// is done, closes the worker's input, so it answers what it holds and ends.
async fn forward(
    mut incoming: mpsc::Receiver<Vec<u8>>,
    mut to: impl AsyncWrite + Unpin,
    ahead: mpsc::Sender<Pending>,
) -> Result<(), Ended> {
    let lost = |e: std::io::Error| ended("unavailable", format!("the worker: {e}"));
    while let Some(bytes) = incoming.recv().await {
        let request = pb::FileRequest::decode_from_slice(&bytes)
            .ok()
            .filter(|_| bytes.len() <= LIMIT)
            .ok_or_else(|| ended("invalid", "a frame is not a FileRequest"))?;
        if ahead.send(Pending::of(&request)).await.is_err() {
            return Ok(());
        }
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        to.write_all(&len.to_le_bytes()).await.map_err(lost)?;
        to.write_all(&bytes).await.map_err(lost)?;
    }
    to.shutdown().await.map_err(lost)
}

/// The worker's next frame; none once it ended between two.
async fn frame(from: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match from.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
    if len > LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a frame above the limit",
        ));
    }
    let mut bytes = vec![0u8; len];
    from.read_exact(&mut bytes).await?;
    Ok(Some(bytes))
}

/// Sends each answer the worker gives to the client, counting what it changed.
async fn back(
    mut from: impl AsyncRead + Unpin,
    outgoing: mpsc::Sender<Vec<u8>>,
    mut answered: mpsc::Receiver<Pending>,
    tally: &mut Tally,
) -> Result<(), Ended> {
    let lost = |e: std::io::Error| ended("unavailable", format!("the worker: {e}"));
    while let Some(bytes) = frame(&mut from).await.map_err(lost)? {
        let response = pb::FileResponse::decode_from_slice(&bytes)
            .map_err(|e| ended("unavailable", format!("the worker answered nonsense: {e}")))?;
        let pending = answered
            .recv()
            .await
            .ok_or_else(|| ended("unavailable", "the worker answered what was not asked"))?;
        tally.record(pending, &response);
        if outgoing.send(bytes).await.is_err() {
            return Ok(());
        }
    }
    // The worker ended: cleanly only once the client was done, which closed the worker's input
    // and the channel of what was asked, and every request was answered.
    match answered.try_recv() {
        Err(mpsc::error::TryRecvError::Disconnected) => Ok(()),
        _ => Err(ended("unavailable", "the worker ended before the session did")),
    }
}
