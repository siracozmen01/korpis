// SPDX-License-Identifier: AGPL-3.0-or-later

//! How an install in a service guest goes (§4.4 of docs/04-runtimes.md), as its agent answers a
//! [`super::Kind::InstallStatus`]: how many actions have run, whether the install job has ended,
//! and the result it wrote, an `InstallResult` the install job encodes and the host decodes with
//! the same memory-safe decoder it reads a host install's with, bounded by the frame.

/// The fixed part of an answer: the progress and the ended flag.
const FIXED: usize = 5;

/// What the agent says of its install.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Installing {
    /// How many actions have run, as the install job last recorded.
    pub progress: u32,
    /// Whether the install job has ended, a result written or not.
    pub ended: bool,
    /// The result it wrote, empty while it has written none.
    pub result: Vec<u8>,
}

impl Installing {
    /// The payload: the progress, the ended flag, then the result, which must leave the frame
    /// within [`super::MOST`].
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIXED + self.result.len());
        out.extend_from_slice(&self.progress.to_le_bytes());
        out.push(u8::from(self.ended));
        out.extend_from_slice(&self.result);
        out
    }

    /// The answer `payload` holds, if its flag is a flag.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (progress, rest) = payload.split_first_chunk::<4>()?;
        let (&ended, result) = rest.split_first()?;
        let ended = match ended {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self { progress: u32::from_le_bytes(*progress), ended, result: result.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn an_answer_reads_back_and_a_bad_flag_or_a_short_one_is_refused() {
        let answer = Installing { progress: 3, ended: true, result: b"\x08\x01".to_vec() };
        assert_eq!(Installing::decode(&answer.encode()), Some(answer.clone()));
        let mut bad = answer.encode();
        *bad.get_mut(4).unwrap() = 2;
        assert_eq!(Installing::decode(&bad), None);
        assert_eq!(Installing::decode(&[1, 0, 0]), None);
    }
}
