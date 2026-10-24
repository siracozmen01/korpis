// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session through the driver (§4.4 of docs/04-runtimes.md): the driver's `Files` stream,
//! opened on the object and bridged to an in-memory stream that carries the session's frames as a
//! host worker's socket does, so the node's file sessions read and write one kind of stream
//! whichever worker serves them. Each frame is bounded as the worker bounds it.

use super::{DriverRuntime, error};
use crate::runtime::RuntimeResult;
use connectrpc::client::{BidiRecvHalf, BidiSendHalf, ClientTransport, SharedHttp2Connection};
use korpis_files::frame::LIMIT;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::driver::v1::__buffa::oneof::files_request::Message;
use korpis_proto::korpis::v1 as pb;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream, ReadHalf, WriteHalf};

type Recv = BidiRecvHalf<
    <SharedHttp2Connection as ClientTransport>::ResponseBody,
    driver::__buffa::view::FilesResponseView<'static>,
>;
type Send = BidiSendHalf<driver::FilesRequest>;

impl DriverRuntime {
    pub(super) async fn serves_files_call(&self) -> RuntimeResult<bool> {
        let info = self.describe_call().await?;
        let access = info.capabilities.as_option().and_then(|c| c.file_access.as_known());
        Ok(access == Some(pb::FileAccess::FILE_ACCESS_GUEST_WORKER))
    }

    pub(super) async fn files_call(&self, id: &str, write: bool) -> RuntimeResult<DuplexStream> {
        let stream = self.client.files().await.map_err(error)?;
        let (mut send, recv) = stream.into_split();
        let open = driver::FilesOpen { runtime_id: id.to_owned(), write, ..Default::default() };
        let first = driver::FilesRequest {
            message: Some(Message::Open(Box::new(open))),
            ..Default::default()
        };
        send.send(first).await.map_err(error)?;
        let (ours, theirs) = tokio::io::duplex(2 * LIMIT);
        let (from_session, to_session) = tokio::io::split(theirs);
        tokio::spawn(up(from_session, send));
        tokio::spawn(down(recv, to_session));
        Ok(ours)
    }
}

/// Sends each frame the session writes on `from` to the driver, and ends the stream once the
/// session ends its side or sends past the limit.
async fn up(mut from: ReadHalf<DuplexStream>, mut send: Send) {
    loop {
        let mut length = [0_u8; 4];
        if from.read_exact(&mut length).await.is_err() {
            break;
        }
        let length = usize::try_from(u32::from_le_bytes(length)).unwrap_or(usize::MAX);
        if length > LIMIT {
            break;
        }
        let mut frame = vec![0; length];
        if from.read_exact(&mut frame).await.is_err() {
            break;
        }
        let request =
            driver::FilesRequest { message: Some(Message::Frame(frame)), ..Default::default() };
        if send.send(request).await.is_err() {
            break;
        }
    }
    send.close_send();
}

/// Writes each frame the driver sends back to the session on `to`, and closes it once the
/// driver's stream ends.
async fn down(mut recv: Recv, mut to: WriteHalf<DuplexStream>) {
    while let Ok(Some(m)) = recv.message::<driver::FilesResponse>().await {
        let frame = m.to_owned_message().frame;
        let Ok(length) = u32::try_from(frame.len()) else { break };
        if frame.len() > LIMIT
            || to.write_all(&length.to_le_bytes()).await.is_err()
            || to.write_all(&frame).await.is_err()
        {
            break;
        }
    }
    let _ = to.shutdown().await;
}
