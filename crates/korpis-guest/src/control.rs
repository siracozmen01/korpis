// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control port: what the driver asks a running guest's `korpis-guest`, over the microVM's
//! vsock device (§6 of docs/04-runtimes.md). Every message is a frame: a kind, a length, and that
//! many bytes, the length bounded before anything is read. The host is root and the guest is the
//! tenant's kernel, so what a guest answers is read the way the run's record is: numbers of fixed
//! layout, and bytes the host passes on without interpreting, never a structure it parses. A
//! guest whose agent does not answer is one the host cannot reach, which is all the `guest_agent`
//! probe asks; nothing the agent says is trusted beyond that, since a workload that is root of
//! its own guest can stop the agent or answer in its place.

mod answers;
mod files;

pub use answers::*;
pub use files::{MOST_FILE, MOST_FILES, PART, Part, Place, keep, kept, plain};
use std::io::{self, Read, Write};

/// The vsock port the guest's agent listens on.
pub const PORT: u32 = 1024;
/// The guest's address on its vsock device: the first a guest may have.
pub const GUEST_CID: u32 = 3;
/// The host's address on a guest's vsock device.
pub const HOST_CID: u32 = 2;
/// The host port a guest's connections to its identity socket are relayed to (§9.5 of
/// docs/08-identity.md): the guest opens them, and the driver relays each to the workload's own
/// socket on the host.
pub const IDENTITY_PORT: u32 = 1025;
/// The host port a guest's connections to the API relay are relayed to, where its node relays
/// the API to the workload.
pub const API_PORT: u32 = 1026;
/// The version of this protocol a guest speaks, which it says in every answer to a ping.
pub const VERSION: u32 = 1;
/// The longest a frame's payload may be.
pub const MOST: usize = 64 * 1024;

/// What a frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The host asks whether the agent answers; no payload.
    Ping,
    /// The agent answers a ping: [`Pong`], fixed layout.
    Pong,
    /// The host asks the agent to send the entry's process group a signal, named in the payload
    /// as the driver declares it (`SIGHUP`), at most [`LONGEST_SIGNAL`] bytes.
    Signal,
    /// The agent answers a signal: one byte, a [`Signalled`].
    Signalled,
    /// The host asks the agent to set the size of an interactive entry's terminal: four bytes,
    /// [`size`].
    Resize,
    /// The agent answers a resize: one byte, a [`Resized`].
    Resized,
    /// The host asks the agent to run a command as the entry runs: an [`Exec`], as JSON, which
    /// the guest reads and the host writes.
    Exec,
    /// What the command wrote, stdout and stderr together: bytes the host passes on.
    Output,
    /// How the command ended, after its output: an [`Exited`], fixed layout.
    Exited,
    /// The host says which relays the workload has beside its identity socket: one byte, a
    /// [`Relays`]. The agent serves `/run/korpis/api.sock` while the API relay is set.
    Relays,
    /// The agent answers a relays frame with what it now serves: one byte, a [`Relays`].
    Relayed,
    /// The host sends a part of one of the workload's files: a [`Part`].
    Put,
    /// The agent answers a part: one byte, 0 kept and 1 refused.
    Stored,
    /// The host says which files one of the workload's directories keeps, the rest removed:
    /// [`keep`]. The last of a sync is the `deps` directory's.
    Keep,
    /// The agent answers what a directory keeps: one byte, 0 done and 1 refused.
    Kept,
}

impl Kind {
    const fn byte(self) -> u8 {
        match self {
            Self::Ping => 1,
            Self::Signal => 2,
            Self::Resize => 3,
            Self::Exec => 4,
            Self::Relays => 5,
            Self::Put => 6,
            Self::Keep => 7,
            Self::Pong => 0x81,
            Self::Signalled => 0x82,
            Self::Resized => 0x83,
            Self::Output => 0x84,
            Self::Exited => 0x85,
            Self::Relayed => 0x86,
            Self::Stored => 0x87,
            Self::Kept => 0x88,
        }
    }

    const fn of(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Ping),
            2 => Some(Self::Signal),
            3 => Some(Self::Resize),
            4 => Some(Self::Exec),
            5 => Some(Self::Relays),
            6 => Some(Self::Put),
            7 => Some(Self::Keep),
            0x81 => Some(Self::Pong),
            0x82 => Some(Self::Signalled),
            0x83 => Some(Self::Resized),
            0x84 => Some(Self::Output),
            0x85 => Some(Self::Exited),
            0x86 => Some(Self::Relayed),
            0x87 => Some(Self::Stored),
            0x88 => Some(Self::Kept),
            _ => None,
        }
    }
}

/// A frame's header: its kind and its payload's length, five bytes.
pub const HEADER: usize = 5;

/// `payload` as a frame of `kind`.
///
/// # Errors
/// The payload is longer than [`MOST`].
pub fn frame(kind: Kind, payload: &[u8]) -> io::Result<Vec<u8>> {
    let length = u32::try_from(payload.len()).ok().filter(|_| payload.len() <= MOST);
    let length = length.ok_or_else(|| io::Error::other("a frame longer than the most"))?;
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.push(kind.byte());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// The kind and payload length a header says, if it is one this side reads.
///
/// # Errors
/// An unknown kind, or a length past [`MOST`]: nothing more is read of such a frame.
pub fn header(bytes: [u8; HEADER]) -> io::Result<(Kind, usize)> {
    let [kind, length @ ..] = bytes;
    let kind = Kind::of(kind).ok_or_else(|| io::Error::other("a frame of an unknown kind"))?;
    let length = usize::try_from(u32::from_le_bytes(length)).unwrap_or(usize::MAX);
    if length > MOST {
        return Err(io::Error::other("a frame longer than the most"));
    }
    Ok((kind, length))
}

/// Reads one frame from `r`, blocking.
///
/// # Errors
/// What [`header`] refuses, or the stream ends or fails.
pub fn read(r: &mut impl Read) -> io::Result<(Kind, Vec<u8>)> {
    let mut head = [0; HEADER];
    r.read_exact(&mut head)?;
    let (kind, length) = header(head)?;
    let mut payload = vec![0; length];
    r.read_exact(&mut payload)?;
    Ok((kind, payload))
}

/// Writes one frame to `w`, blocking.
///
/// # Errors
/// What [`frame`] refuses, or the stream fails.
pub fn write(w: &mut impl Write, kind: Kind, payload: &[u8]) -> io::Result<()> {
    w.write_all(&frame(kind, payload)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_frame_reads_back_and_a_long_or_unknown_one_is_refused_before_its_payload() {
        let pong = Pong { version: VERSION, uptime_ms: 1234 };
        let bytes = frame(Kind::Pong, &pong.encode()).unwrap();
        let (kind, payload) = read(&mut bytes.as_slice()).unwrap();
        assert_eq!((kind, Pong::decode(&payload)), (Kind::Pong, Some(pong)));
        assert!(Pong::decode(&payload[..11]).is_none(), "fixed layout or nothing");
        let mut long = [0x81, 0, 0, 0, 0];
        long[1..].copy_from_slice(&(u32::try_from(MOST).unwrap() + 1).to_le_bytes());
        assert!(header(long).is_err(), "past the most");
        assert!(header([9, 0, 0, 0, 0]).is_err(), "an unknown kind");
        assert!(frame(Kind::Ping, &vec![0; MOST + 1]).is_err());
    }
}
