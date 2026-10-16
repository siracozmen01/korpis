// SPDX-License-Identifier: AGPL-3.0-or-later

//! A call whose answer carries descriptors (`OpenResolver`): the helper sends them as
//! `SCM_RIGHTS` with the answer's first bytes, which a plain read would drop, so the length is
//! received with `recvmsg` and the rest read as usual. Blocking, on a thread of its own.

use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// The most descriptors an answer carries.
const MAX_ATTACHED: usize = 32;

/// Sends `request` to the helper at `socket` and returns its answer and the descriptors attached
/// to it, within `deadline` for each read and write.
pub(super) fn exchange(
    socket: &Path,
    request: &[u8],
    (deadline, max): (Duration, usize),
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(deadline))?;
    s.set_write_timeout(Some(deadline))?;
    let len = u32::try_from(request.len()).map_err(io::Error::other)?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(request)?;
    s.flush()?;

    let mut head = [0u8; 4];
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_ATTACHED))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let got =
        recvmsg(&s, &mut [io::IoSliceMut::new(&mut head)], &mut control, RecvFlags::CMSG_CLOEXEC)?;
    let mut attached = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = message {
            attached.extend(fds);
        }
    }
    if got.bytes == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the helper closed"));
    }
    s.read_exact(head.get_mut(got.bytes..).unwrap_or_default())?;
    let len = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
    if len > max {
        return Err(io::Error::other("an answer above the limit"));
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    Ok((body, attached))
}
