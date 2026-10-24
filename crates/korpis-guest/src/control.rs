// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control port: what the driver asks a running guest's `korpis-guest`, over the microVM's
//! vsock device (§6 of docs/04-runtimes.md). Every message is a frame: a kind, a length, and that
//! many bytes, the length bounded before anything is read. The host is root and the guest is the
//! tenant's kernel, so what a guest answers is read the way the run's record is: numbers of fixed
//! layout, and bytes the host passes on without interpreting, never a structure it parses. A
//! guest whose agent does not answer is one the host cannot reach, which is all the `guest_agent`
//! probe asks; nothing the agent says is trusted beyond that, since a workload that is root of
//! its own guest can stop the agent or answer in its place.

use std::io::{self, Read, Write};

/// The vsock port the guest's agent listens on.
pub const PORT: u32 = 1024;
/// The guest's address on its vsock device: the first a guest may have.
pub const GUEST_CID: u32 = 3;
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
}

impl Kind {
    const fn byte(self) -> u8 {
        match self {
            Self::Ping => 1,
            Self::Signal => 2,
            Self::Resize => 3,
            Self::Exec => 4,
            Self::Pong => 0x81,
            Self::Signalled => 0x82,
            Self::Resized => 0x83,
            Self::Output => 0x84,
            Self::Exited => 0x85,
        }
    }

    const fn of(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Ping),
            2 => Some(Self::Signal),
            3 => Some(Self::Resize),
            4 => Some(Self::Exec),
            0x81 => Some(Self::Pong),
            0x82 => Some(Self::Signalled),
            0x83 => Some(Self::Resized),
            0x84 => Some(Self::Output),
            0x85 => Some(Self::Exited),
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

/// The longest signal name a [`Kind::Signal`] frame carries.
pub const LONGEST_SIGNAL: usize = 16;

/// The signals a guest's entry can be sent by name: the ones the `oci` driver declares, so a
/// recipe asks the same of either tier.
pub const SIGNALS: [&str; 8] =
    ["SIGTERM", "SIGKILL", "SIGINT", "SIGHUP", "SIGQUIT", "SIGUSR1", "SIGUSR2", "SIGWINCH"];

/// What became of a signal the host asked the agent to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signalled {
    /// Sent to the entry's process group.
    Sent,
    /// No entry runs to send it to: it has ended, or not yet started.
    NoEntry,
    /// Not a signal this agent sends.
    Unknown,
}

impl Signalled {
    /// Its payload, one byte.
    #[must_use]
    pub const fn encode(self) -> [u8; 1] {
        match self {
            Self::Sent => [0],
            Self::NoEntry => [1],
            Self::Unknown => [2],
        }
    }

    /// The outcome `payload` holds, if it is one this side knows.
    #[must_use]
    pub const fn decode(payload: &[u8]) -> Option<Self> {
        match payload {
            [0] => Some(Self::Sent),
            [1] => Some(Self::NoEntry),
            [2] => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// A terminal's size as a [`Kind::Resize`] frame carries it: columns, then rows, each two bytes,
/// little-endian.
#[must_use]
pub fn size(cols: u16, rows: u16) -> [u8; 4] {
    let mut out = [0; 4];
    let (c, r) = out.split_at_mut(2);
    c.copy_from_slice(&cols.to_le_bytes());
    r.copy_from_slice(&rows.to_le_bytes());
    out
}

/// The columns and rows `payload` holds, if it is four bytes.
#[must_use]
pub fn size_of(payload: &[u8]) -> Option<(u16, u16)> {
    let [c0, c1, r0, r1] = <[u8; 4]>::try_from(payload).ok()?;
    Some((u16::from_le_bytes([c0, c1]), u16::from_le_bytes([r0, r1])))
}

/// What became of a resize the host asked the agent for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resized {
    /// The terminal has the size; the kernel tells its foreground group with `SIGWINCH`.
    Set,
    /// The entry has no terminal: its console is one of logs.
    NoTerminal,
    /// The terminal would not take it.
    Failed,
}

impl Resized {
    /// Its payload, one byte.
    #[must_use]
    pub const fn encode(self) -> [u8; 1] {
        match self {
            Self::Set => [0],
            Self::NoTerminal => [1],
            Self::Failed => [2],
        }
    }

    /// The outcome `payload` holds, if it is one this side knows.
    #[must_use]
    pub const fn decode(payload: &[u8]) -> Option<Self> {
        match payload {
            [0] => Some(Self::Set),
            [1] => Some(Self::NoTerminal),
            [2] => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A command the host asks the agent to run inside the guest, as the entry runs: its user, its
/// environment, its working directory, in the workload's root (§4.2 of docs/04-runtimes.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Exec {
    /// The command and its arguments.
    pub argv: Vec<String>,
    /// How long it may run before it is killed, in milliseconds.
    pub timeout_ms: u64,
    /// The most of its output sent.
    pub output_limit: u32,
}

/// How a command the agent ran ended: six bytes, its status and two flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exited {
    /// Its exit status; 128 and the signal's number for one a signal ended; 127 for one that
    /// could not be started, whose output says why.
    pub status: u32,
    /// Killed for outliving its timeout.
    pub timed_out: bool,
    /// Output past its limit was dropped.
    pub truncated: bool,
}

impl Exited {
    /// Its payload.
    #[must_use]
    pub fn encode(self) -> [u8; 6] {
        let mut out = [0; 6];
        let (status, flags) = out.split_at_mut(4);
        status.copy_from_slice(&self.status.to_le_bytes());
        flags.copy_from_slice(&[u8::from(self.timed_out), u8::from(self.truncated)]);
        out
    }

    /// The ending `payload` holds, if it is six bytes whose flags are flags.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let [s0, s1, s2, s3, timed_out, truncated] = <[u8; 6]>::try_from(payload).ok()?;
        let flag = |b: u8| match b {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        Some(Self {
            status: u32::from_le_bytes([s0, s1, s2, s3]),
            timed_out: flag(timed_out)?,
            truncated: flag(truncated)?,
        })
    }
}

/// The agent's answer to a ping: the protocol it speaks, and how long its guest has been up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    /// The protocol version, [`VERSION`] for this build.
    pub version: u32,
    /// Milliseconds since the agent started.
    pub uptime_ms: u64,
}

impl Pong {
    /// Its payload: twelve bytes, little-endian.
    #[must_use]
    pub fn encode(self) -> [u8; 12] {
        let mut out = [0; 12];
        let (version, uptime) = out.split_at_mut(4);
        version.copy_from_slice(&self.version.to_le_bytes());
        uptime.copy_from_slice(&self.uptime_ms.to_le_bytes());
        out
    }

    /// The pong `payload` holds, if it is twelve bytes.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (version, uptime) = payload.split_first_chunk::<4>()?;
        let uptime: [u8; 8] = uptime.try_into().ok()?;
        Some(Self { version: u32::from_le_bytes(*version), uptime_ms: u64::from_le_bytes(uptime) })
    }
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
        assert!(header([7, 0, 0, 0, 0]).is_err(), "an unknown kind");
        assert!(frame(Kind::Ping, &vec![0; MOST + 1]).is_err());
    }

    #[test]
    fn a_signal_s_outcome_is_one_byte_and_nothing_else() {
        for s in [Signalled::Sent, Signalled::NoEntry, Signalled::Unknown] {
            assert_eq!(Signalled::decode(&s.encode()), Some(s));
        }
        assert_eq!(Signalled::decode(&[3]), None);
        assert_eq!(Signalled::decode(&[0, 0]), None, "one byte or nothing");
        assert_eq!(Resized::decode(&Resized::NoTerminal.encode()), Some(Resized::NoTerminal));
        assert_eq!(size_of(&size(132, 43)), Some((132, 43)));
        assert_eq!(size_of(&[1, 0, 2]), None, "four bytes or nothing");
        let exited = Exited { status: 137, timed_out: true, truncated: false };
        assert_eq!(Exited::decode(&exited.encode()), Some(exited));
        assert_eq!(Exited::decode(&[0, 0, 0, 0, 2, 0]), None, "a flag is a flag");
    }
}
