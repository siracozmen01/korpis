// SPDX-License-Identifier: AGPL-3.0-or-later

//! Carrying one session: a stream to the relay of its own, the client's frames to the handler and
//! the handler's bytes to the client, until the handler returns. What the handler sent before it
//! returned is delivered before the end.

use super::handler::{Ended, Handler, Io, Opened};
use connectrpc::client::{BidiRecvHalf, ClientTransport, HttpClient};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as Up;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::sync::Arc;
use tokio::sync::mpsc;

/// How many frames may wait for the slower side, each way.
const FRAMES: usize = 64;
/// How long a carry waits, after its end, for the relay to finish with it.
const LINGER: std::time::Duration = std::time::Duration::from_secs(5);

type Recv = BidiRecvHalf<
    <HttpClient as ClientTransport>::ResponseBody,
    agent::__buffa::view::CarryDownView<'static>,
>;

fn up(frame: Frame) -> agent::CarryUp {
    let frame = pb::SessionFrame { frame: Some(frame), ..Default::default() };
    agent::CarryUp { message: Some(Up::Frame(Box::new(frame))), ..Default::default() }
}

fn end(result: Result<(), Ended>) -> agent::CarryUp {
    let (code, message) = result.map_or_else(|e| (e.code, e.message), |()| Default::default());
    up(Frame::End(Box::new(pb::SessionEnd { code, message, ..Default::default() })))
}

/// Hands the client's data to the handler until the client ends or the stream drops.
async fn read(mut recv: Recv, to_handler: mpsc::Sender<Vec<u8>>) {
    while let Ok(Some(m)) = recv.message::<agent::CarryDown>().await {
        match m.to_owned_message().frame.into_option().and_then(|f| f.frame) {
            Some(Frame::Data(bytes)) => {
                if to_handler.send(bytes).await.is_err() {
                    return;
                }
            }
            Some(Frame::End(_)) => return,
            // Renewal is checked before frames reach here; anything else is not for a handler.
            _ => {}
        }
    }
}

/// Carries session `opened` on `client`, served by `handler`.
pub(super) async fn carry(
    client: &agent::DataServiceClient<HttpClient>,
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
    let reader = tokio::spawn(read(recv, to_handler));
    let mut serving = handler.serve(opened, Io { incoming, outgoing });
    let result = loop {
        tokio::select! {
            result = &mut serving => break result,
            Some(bytes) = from_handler.recv() => {
                send.send(up(Frame::Data(bytes))).await.map_err(|e| e.to_string())?;
            }
        }
    };
    while let Some(bytes) = from_handler.recv().await {
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
