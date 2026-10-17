// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a session's messages cross the worker's socket: four bytes of little-endian length, then
//! the message (`OpenFilesResponse` in `helper.proto`). A length above [`LIMIT`] is refused
//! before anything is read for it, so a peer cannot make the worker hold more than a frame.

use std::io::{ErrorKind, Read, Write};

/// The largest frame either side sends: a chunk of data and room for the rest of its message.
pub const LIMIT: usize = crate::fs::CHUNK + (64 << 10);

/// The next frame from `from`; none when it ended cleanly between two frames.
///
/// # Errors
/// It ended inside a frame, the length is above [`LIMIT`], or the read failed.
pub fn read(from: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match from.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
    if len > LIMIT {
        return Err(std::io::Error::new(ErrorKind::InvalidData, "a frame above the limit"));
    }
    let mut bytes = vec![0u8; len];
    from.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Writes `bytes` to `to` as one frame.
///
/// # Errors
/// It is above [`LIMIT`], or the write failed.
pub fn write(to: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|_| bytes.len() <= LIMIT)
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "a frame above the limit"))?;
    to.write_all(&len.to_le_bytes())?;
    to.write_all(bytes)?;
    to.flush()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::{LIMIT, read, write};

    #[test]
    fn frames_round_trip_and_a_length_above_the_limit_is_refused_unread() {
        let mut wire = Vec::new();
        write(&mut wire, b"one").unwrap();
        write(&mut wire, b"").unwrap();
        let mut from = wire.as_slice();
        assert_eq!(read(&mut from).unwrap().unwrap(), b"one");
        assert_eq!(read(&mut from).unwrap().unwrap(), b"");
        assert!(read(&mut from).unwrap().is_none());
        let huge = u32::try_from(LIMIT + 1).unwrap().to_le_bytes();
        assert!(read(&mut huge.as_slice()).is_err());
        // Ended inside a frame: an error, not a clean end.
        assert!(read(&mut [9u8, 0, 0, 0, 1].as_slice()).is_err());
        assert!(write(&mut Vec::new(), &vec![0; LIMIT + 1]).is_err());
    }
}
