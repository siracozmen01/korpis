// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the journal keeps beside its segments, in a small text file replaced atomically: the
//! cursor last acknowledged, the highest sequence number known to be used, the numbers lost
//! because the journal reached its bound and not yet covered by a gap record, and when each
//! sealed segment became wholly acknowledged, which starts its retention window.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The journal's side state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Meta {
    /// The control plane's cursor, as last acknowledged.
    pub(super) acked: u64,
    /// The highest sequence number known to be used: written, lost, or acknowledged. No record
    /// is ever numbered at or below it again.
    pub(super) floor: u64,
    /// Numbers lost to the bound, first and last, awaiting their gap record.
    pub(super) lost: Option<(u64, u64)>,
    /// For each sealed segment wholly acknowledged, by its first number: since when.
    pub(super) acked_at: BTreeMap<u64, SystemTime>,
}

fn invalid(line: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("journal state: {line:?}"))
}

fn number(word: Option<&str>, line: &str) -> std::io::Result<u64> {
    word.and_then(|w| w.parse().ok()).ok_or_else(|| invalid(line))
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl Meta {
    /// The state kept at `path`; none kept is the default.
    pub(super) fn load(path: &Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let mut meta = Self::default();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("acked") => meta.acked = number(words.next(), line)?,
                Some("floor") => meta.floor = number(words.next(), line)?,
                Some("lost") => {
                    let first = number(words.next(), line)?;
                    meta.lost = Some((first, number(words.next(), line)?));
                }
                Some("acked_at") => {
                    let first = number(words.next(), line)?;
                    let at = UNIX_EPOCH + Duration::from_secs(number(words.next(), line)?);
                    meta.acked_at.insert(first, at);
                }
                _ => return Err(invalid(line)),
            }
        }
        Ok(meta)
    }

    /// Keeps the state at `path`.
    pub(super) fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut lines = vec![format!("acked {}", self.acked), format!("floor {}", self.floor)];
        lines.extend(self.lost.map(|(first, last)| format!("lost {first} {last}")));
        lines.extend(self.acked_at.iter().map(|(f, at)| format!("acked_at {f} {}", secs(*at))));
        let text = lines.join("\n") + "\n";
        crate::durable::replace(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_survives_a_round_trip_and_a_stranger_line_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        assert_eq!(Meta::load(&path).unwrap(), Meta::default());
        let mut meta = Meta { acked: 7, floor: 12, lost: Some((10, 12)), ..Meta::default() };
        meta.acked_at.insert(1, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        meta.save(&path).unwrap();
        assert_eq!(Meta::load(&path).unwrap(), meta);
        std::fs::write(&path, "acked 7\nwhatever 3\n").unwrap();
        assert!(Meta::load(&path).is_err());
    }
}
