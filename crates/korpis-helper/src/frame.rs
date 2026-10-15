// SPDX-License-Identifier: AGPL-3.0-or-later

//! The transport's framing (helper.proto): a 4-byte big-endian length, then that many bytes of
//! one message, at most [`MAX`]. A length above it ends the connection before anything is read
//! into memory, so a peer cannot make the helper allocate what it names.

use std::io::{self, Read, Write};

/// The largest message either side sends.
pub const MAX: usize = 1 << 20;

/// The next message on `r`, or `None` at a clean end between messages.
pub fn read(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
    if len > MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame above the limit"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Writes `body` as one message on `w`.
pub fn write(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|l| usize::try_from(*l).is_ok_and(|l| l <= MAX))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a frame above the limit"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// The most descriptors one frame carries.
pub const MAX_ATTACHED: usize = 4;

/// Writes `body` as one message on `stream`, with `attached` sent as `SCM_RIGHTS` alongside its
/// first bytes, as the peer's `recvmsg` of the length receives them.
pub fn write_attaching(
    stream: &std::os::unix::net::UnixStream,
    body: &[u8],
    attached: &[std::os::fd::OwnedFd],
) -> io::Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    use std::os::fd::AsFd;
    if attached.len() > MAX_ATTACHED {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many descriptors"));
    }
    let mut framed = Vec::with_capacity(4 + body.len());
    write(&mut framed, body)?;
    let fds: Vec<_> = attached.iter().map(AsFd::as_fd).collect();
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_ATTACHED))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("the descriptors do not fit their buffer"));
    }
    let sent = sendmsg(stream, &[io::IoSlice::new(&framed)], &mut control, SendFlags::empty())?;
    let mut w = stream;
    w.write_all(framed.get(sent..).unwrap_or_default())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_and_an_oversized_one_is_never_read() {
        let mut wire = Vec::new();
        write(&mut wire, b"hello").unwrap();
        write(&mut wire, b"").unwrap();
        let mut r = wire.as_slice();
        assert_eq!(read(&mut r).unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(read(&mut r).unwrap().as_deref(), Some(&b""[..]));
        assert_eq!(read(&mut r).unwrap(), None, "a clean end");
        let huge = u32::try_from(MAX + 1).unwrap().to_be_bytes();
        assert!(read(&mut huge.as_slice()).is_err());
        assert!(read(&mut [0u8, 0, 0, 9, 1].as_slice()).is_err(), "a truncated frame");
    }
}
