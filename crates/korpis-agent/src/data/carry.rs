// SPDX-License-Identifier: AGPL-3.0-or-later

//! Carrying one session: a stream to the relay of its own, the client's frames to the handler and
//! the handler's bytes to the client, until the handler returns or the node ends the session.
//! What the handler sent before it returned is delivered before the end.
//!
//! The node ends a session itself in three cases, each with its reason: its token expires
//! unrenewed (`expired`), a revocation list arrives naming a grant of its chain (`revoked`), or
//! the relay refuses a renewal (`refused`). A renewal the node accepts moves the deadline and is
//! acknowledged to the client with the new expiry.

use super::Judged;
use super::handler::{Ended, Handler, Io, Opened};
use buffa_types::google::protobuf::Timestamp;
use connectrpc::client::{BidiRecvHalf, BidiSendHalf, ClientTransport, HttpClient};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as Up;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How many frames may wait for the slower side, each way.
const FRAMES: usize = 64;
/// How long a carry waits, after its end, for the relay to finish with it.
const LINGER: Duration = Duration::from_secs(5);

type Recv = BidiRecvHalf<
    <HttpClient as ClientTransport>::ResponseBody,
    agent::__buffa::view::CarryDownView<'static>,
>;
type Send = BidiSendHalf<agent::CarryUp>;

/// What the relay sent that is for the carry rather than the handler.
enum Control {
    Renew(String),
    End(Ended),
}

fn up(frame: Frame) -> agent::CarryUp {
    let frame = pb::SessionFrame { frame: Some(frame), ..Default::default() };
    agent::CarryUp { message: Some(Up::Frame(Box::new(frame))), ..Default::default() }
}

fn end(result: Result<(), Ended>) -> agent::CarryUp {
    let (code, message) = result.map_or_else(|e| (e.code, e.message), |()| Default::default());
    up(Frame::End(Box::new(pb::SessionEnd { code, message, ..Default::default() })))
}

fn ended(code: &str, message: impl Into<String>) -> Ended {
    Ended { code: code.to_owned(), message: message.into() }
}

fn timestamp(at: SystemTime) -> Timestamp {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    Timestamp { seconds, nanos: 0, ..Default::default() }
}

/// The instant `at` falls on, by the monotonic clock.
fn instant(at: SystemTime) -> Instant {
    Instant::now() + at.duration_since(SystemTime::now()).unwrap_or_default()
}

/// Hands the client's data to the handler and the rest to the carry, until the client ends or
/// the stream drops.
async fn read(mut recv: Recv, to_handler: mpsc::Sender<Vec<u8>>, control: mpsc::Sender<Control>) {
    while let Ok(Some(m)) = recv.message::<agent::CarryDown>().await {
        let next = match m.to_owned_message().frame.into_option().and_then(|f| f.frame) {
            Some(Frame::Data(bytes)) => to_handler.send(bytes).await.is_ok(),
            Some(Frame::Renew(token)) => control.send(Control::Renew(token)).await.is_ok(),
            // The client is done: the handler sees its input end.
            Some(Frame::End(e)) if e.code.is_empty() => return,
            // The relay ended it.
            Some(Frame::End(e)) => {
                let _ = control.send(Control::End(ended(&e.code, e.message))).await;
                return;
            }
            _ => true,
        };
        if !next {
            return;
        }
    }
}

/// The session's authority as it stands: who, under which chain, until when.
struct Standing {
    opened: Opened,
    deadline: Instant,
}

impl Standing {
    /// Takes a renewal: the deadline moves, and the client is told; or the session ends.
    async fn renew(&mut self, judged: &Judged, token: &str, send: &mut Send) -> Result<(), Ended> {
        let verified = judged.verify(token, &self.opened).map_err(|e| ended("refused", e))?;
        self.opened.chain = verified.chain;
        self.opened.expires_at = verified.expires_at;
        self.deadline = instant(verified.expires_at);
        let renewed = up(Frame::Renewed(Box::new(timestamp(verified.expires_at))));
        send.send(renewed).await.map_err(|e| ended("unavailable", e.to_string()))
    }
}

/// Carries session `opened` on `client`, served by `handler` and judged by `judged`.
pub(super) async fn carry(
    client: &agent::DataServiceClient<HttpClient>,
    judged: &Arc<Judged>,
    handler: Arc<dyn Handler>,
    opened: Opened,
) -> Result<(), String> {
    let stream = client.carry().await.map_err(|e| e.to_string())?;
    let (mut send, recv) = stream.into_split();
    let first =
        agent::CarryUp { message: Some(Up::Session(opened.session.clone())), ..Default::default() };
    send.send(first).await.map_err(|e| e.to_string())?;
    let (to_handler, incoming) = mpsc::channel(FRAMES);
    let (outgoing, mut from_handler) = mpsc::channel(FRAMES);
    let (control, mut controls) = mpsc::channel(4);
    let reader = tokio::spawn(read(recv, to_handler, control));
    let mut standing = Standing { deadline: instant(opened.expires_at), opened: opened.clone() };
    let mut revoked = judged.revoked.clone();
    let mut serving = handler.serve(opened, Io { incoming, outgoing });
    let result = loop {
        // The node's own reasons first: the reader passes on the relay's end before the
        // handler can see its input close, so a session the relay ended says so.
        tokio::select! {
            biased;
            Some(c) = controls.recv() => match c {
                Control::Renew(token) => {
                    if let Err(e) = standing.renew(judged, &token, &mut send).await {
                        break Err(e);
                    }
                }
                Control::End(e) => break Err(e),
            },
            Ok(()) = revoked.changed() => {
                if let Some(g) = revoked.borrow_and_update().refuses(&standing.opened.chain) {
                    break Err(ended("revoked", format!("grant {g} of its chain is revoked")));
                }
            }
            () = tokio::time::sleep_until(standing.deadline) => {
                break Err(ended("expired", "the session's token expired unrenewed"));
            }
            Some(bytes) = from_handler.recv() => {
                send.send(up(Frame::Data(bytes))).await.map_err(|e| e.to_string())?;
            }
            result = &mut serving => break result,
        }
    };
    let finished = result.is_ok();
    drop(serving);
    // A handler that finished has said all it had to say; one ended by the node has not.
    while finished && let Some(bytes) = from_handler.recv().await {
        send.send(up(Frame::Data(bytes))).await.map_err(|e| e.to_string())?;
    }
    send.send(end(result)).await.map_err(|e| e.to_string())?;
    send.close_send();
    // The relay ends the carry once it has passed the end on; dropping the stream sooner would
    // reset it, and the end would be lost with it.
    if tokio::time::timeout(LINGER, reader).await.is_err() {
        tracing::debug!("the relay did not end the carry in time");
    }
    Ok(())
}
