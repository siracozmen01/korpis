// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session's frames between the node and a guest's worker (§4.4 of docs/04-runtimes.md):
//! each request's frame written to the worker as the worker reads one, four bytes of
//! little-endian length then the bytes, and each frame the worker writes passed back as a
//! response, its length bounded before anything is read for it. Neither is read beyond that: the
//! node decodes them, as it does a host worker's.

use connectrpc::{ConnectError, ErrorCode, InboundStream};
use futures_util::StreamExt as _;
use korpis_files::frame::LIMIT;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::driver::v1::__buffa::oneof::files_request::Message;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

fn invalid(why: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, why)
}

/// The session's opening: its first request.
///
/// # Errors
/// The stream ended first, failed, or began otherwise.
pub async fn opened(
    requests: &mut InboundStream<driver::FilesRequest>,
) -> Result<driver::FilesOpen, ConnectError> {
    let first = requests.next().await.ok_or_else(|| invalid("a file session opens first"))??;
    match first.to_owned_message().message {
        Some(Message::Open(open)) => Ok(*open),
        _ => Err(invalid("a file session's first request opens it")),
    }
}

/// Writes each frame of `requests` to the worker at `to`, and ends the worker's input once they
/// end, so it answers what it holds and closes.
pub async fn up(mut requests: InboundStream<driver::FilesRequest>, mut to: OwnedWriteHalf) {
    while let Some(Ok(request)) = requests.next().await {
        let Some(Message::Frame(bytes)) = request.to_owned_message().message else { break };
        let Ok(length) = u32::try_from(bytes.len()) else { break };
        if bytes.len() > LIMIT
            || to.write_all(&length.to_le_bytes()).await.is_err()
            || to.write_all(&bytes).await.is_err()
        {
            break;
        }
    }
    let _ = to.shutdown().await;
}

/// Each frame the worker at `from` writes, as a response, until it closes between two frames;
/// `held` is kept until then, or until the node lets the session go.
pub fn down<H: Send + 'static>(
    from: OwnedReadHalf,
    held: H,
) -> impl futures_util::Stream<Item = Result<driver::FilesResponse, ConnectError>> + Send + 'static
{
    futures_util::stream::unfold(Some((from, held)), |state| async move {
        let (mut from, held) = state?;
        let mut length = [0_u8; 4];
        if from.read_exact(&mut length).await.is_err() {
            return None;
        }
        let length = usize::try_from(u32::from_le_bytes(length)).unwrap_or(usize::MAX);
        if length > LIMIT {
            let lost = ConnectError::new(ErrorCode::DataLoss, "the worker sent past the limit");
            return Some((Err(lost), None));
        }
        let mut frame = vec![0; length];
        if let Err(e) = from.read_exact(&mut frame).await {
            let lost = ConnectError::new(ErrorCode::Unavailable, format!("the worker: {e}"));
            return Some((Err(lost), None));
        }
        Some((Ok(driver::FilesResponse { frame, ..Default::default() }), Some((from, held))))
    })
}
