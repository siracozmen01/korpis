// SPDX-License-Identifier: AGPL-3.0-or-later

//! A health provider's half of a `query` probe (§5 of docs/04-runtimes.md, §5 of
//! docs/16-extensions.md). The node holds the socket and the provider holds the protocol: the node
//! connects from inside the workload's network namespace and runs each round, sending what it is
//! told and reading what comes back, and the provider, given the rounds so far, says what to send
//! next or how the workload is. The provider is never given a way to the workload itself, so a
//! provider an operator installed for its customers' game servers reaches none of them; and the
//! bounds here keep an exchange small enough that it is a probe and not a session.

use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

/// The most rounds one exchange may have: the provider's verdict must come by then.
pub const MAX_ROUNDS: usize = 4;
/// The most a provider may have the node send in one round.
pub const MAX_SEND: usize = 4096;
/// The most the node reads in one round.
pub const MAX_READ: u32 = 64 * 1024;
/// The longest a round may wait for what it reads; the probe's timeout bounds the exchange too.
pub const MAX_WAIT: Duration = Duration::from_secs(10);
/// The most a verdict's detail may hold, in entries, each key and value at most 256 bytes.
pub const MAX_DETAIL: usize = 32;

/// How the endpoint is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A connection, kept for every round of the exchange.
    Tcp,
    /// Datagrams: each round sends one and reads the first that comes back.
    Udp,
}

impl Transport {
    /// Its name, as stored and shown.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One round the node has run: what it sent, what it read, and whether the workload closed the
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Round {
    /// What was sent.
    pub sent: Vec<u8>,
    /// What was read before the round's wait ran out, at most its `read_at_most`.
    pub received: Vec<u8>,
    /// Whether the workload ended the connection; never for UDP.
    pub closed: bool,
}

/// One step of a query probe, as a health provider is asked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthStep {
    /// The query protocol, `<publisher>/<protocol>`, which named the provider.
    pub protocol: String,
    /// The workload probed.
    pub workload: Uuid,
    /// The endpoint the probe names.
    pub endpoint: String,
    /// How it is reached.
    pub transport: Transport,
    /// The rounds run so far; none for the first step.
    pub rounds: Vec<Round>,
}

/// What a health provider answers a step with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthNext {
    /// Another round: send `data`, which may be empty to read on, and read at most
    /// `read_at_most` bytes within `wait`.
    Send {
        /// What to send.
        data: Vec<u8>,
        /// The most to read.
        read_at_most: u32,
        /// How long to wait for it.
        wait: Duration,
    },
    /// How the workload is.
    Verdict {
        /// Whether it answered as a healthy one does.
        healthy: bool,
        /// Why, for the status.
        reason: String,
        /// What it said of itself, a game's players and map say.
        detail: BTreeMap<String, String>,
    },
}

impl HealthNext {
    /// Whether it answers `step` within the bounds: a round only while rounds remain, and
    /// nothing larger than a probe needs.
    #[must_use]
    pub fn answers(&self, step: &HealthStep) -> bool {
        match self {
            Self::Send { data, read_at_most, wait } => {
                step.rounds.len() < MAX_ROUNDS
                    && data.len() <= MAX_SEND
                    && *read_at_most <= MAX_READ
                    && *wait <= MAX_WAIT
            }
            Self::Verdict { reason, detail, .. } => {
                let short = |s: &str| s.len() <= 256;
                short(reason)
                    && detail.len() <= MAX_DETAIL
                    && detail.iter().all(|(k, v)| short(k) && short(v))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(rounds: usize) -> HealthStep {
        HealthStep {
            protocol: "korpis-test/echo".into(),
            workload: Uuid::nil(),
            endpoint: "game".into(),
            transport: Transport::Tcp,
            rounds: vec![Round::default(); rounds],
        }
    }

    fn send(data: usize, read: u32, wait: u64) -> HealthNext {
        HealthNext::Send {
            data: vec![0; data],
            read_at_most: read,
            wait: Duration::from_secs(wait),
        }
    }

    #[test]
    fn an_answer_is_a_probe_and_not_a_session() {
        assert!(send(MAX_SEND, MAX_READ, 10).answers(&step(0)));
        assert!(!send(MAX_SEND + 1, 1, 1).answers(&step(0)), "too much to send");
        assert!(!send(1, MAX_READ + 1, 1).answers(&step(0)), "too much to read");
        assert!(!send(1, 1, 11).answers(&step(0)), "too long a wait");
        assert!(!send(1, 1, 1).answers(&step(MAX_ROUNDS)), "a verdict was due");
        let verdict = |n: usize| HealthNext::Verdict {
            healthy: true,
            reason: String::new(),
            detail: (0..n).map(|i| (i.to_string(), "x".into())).collect(),
        };
        assert!(verdict(MAX_DETAIL).answers(&step(MAX_ROUNDS)));
        assert!(!verdict(MAX_DETAIL + 1).answers(&step(1)));
    }
}
