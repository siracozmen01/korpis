// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the frames of the control port carry: each a fixed layout the host decodes or refuses,
//! except a command, which the host encodes and the guest decodes.

/// The longest signal name a [`super::Kind::Signal`] frame carries.
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

/// A terminal's size as a [`super::Kind::Resize`] frame carries it: columns, then rows, each two bytes,
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
    /// The protocol version, [`super::VERSION`] for this build.
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

/// The bit of a relays frame that is the API relay.
const API_RELAY: u8 = 1;

/// The relays beside a workload's identity socket in its guest, as a [`super::Kind::Relays`]
/// frame asks for them and a [`super::Kind::Relayed`] one says what is served: one byte, bit 0
/// the API relay, every other bit clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relays {
    /// Whether `/run/korpis/api.sock` is served.
    pub api: bool,
}

impl Relays {
    /// The byte.
    #[must_use]
    pub fn encode(self) -> [u8; 1] {
        [if self.api { API_RELAY } else { 0 }]
    }

    /// The relays `payload` holds, if it is one byte of known bits.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        match payload {
            [b] if b & !API_RELAY == 0 => Some(Self { api: b & API_RELAY != 0 }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

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
        for api in [true, false] {
            assert_eq!(Relays::decode(&Relays { api }.encode()), Some(Relays { api }));
        }
        assert_eq!(Relays::decode(&[2]), None, "an unknown relay is refused");
    }
}
