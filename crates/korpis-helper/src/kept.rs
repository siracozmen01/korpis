// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a job leaves on a volume until it is forgotten: the snapshot a backup read, or the
//! staging place of a replacing restore. It is recorded in the job's directory when the job
//! starts, so a helper restarted since still knows what to release.

use crate::error::{HelperError, HelperResult};
use crate::staging::Staging;

/// What a job keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// A snapshot taken for it, by volume and name.
    Snapshot {
        /// The volume.
        volume: String,
        /// The snapshot.
        name: String,
    },
    /// A staging place made for it beside a volume.
    Staging {
        /// The volume.
        volume: String,
        /// What the staging place is: its subvolume or inode.
        identity: u64,
        /// The volume's limits when it was made.
        limits: (u64, u64),
    },
}

impl Kept {
    /// The record, one line.
    #[must_use]
    pub fn to_line(&self) -> String {
        match self {
            Self::Snapshot { volume, name } => format!("snapshot {volume} {name}"),
            Self::Staging { volume, identity, limits: (bytes, inodes) } => {
                format!("staging {volume} {identity} {bytes} {inodes}")
            }
        }
    }

    /// The record read back.
    ///
    /// # Errors
    /// A line this module did not write.
    pub fn from_line(line: &str) -> HelperResult<Self> {
        let bad = || HelperError::Failed(format!("a job's record of what it keeps: {line:?}"));
        let number = |s: Option<&str>| s.and_then(|s| s.parse::<u64>().ok()).ok_or_else(bad);
        let mut words = line.split(' ');
        match (words.next(), words.next()) {
            (Some("snapshot"), Some(volume)) => {
                let name = words.next().ok_or_else(bad)?;
                Ok(Self::Snapshot { volume: volume.to_owned(), name: name.to_owned() })
            }
            (Some("staging"), Some(volume)) => {
                let identity = number(words.next())?;
                let limits = (number(words.next())?, number(words.next())?);
                Ok(Self::Staging { volume: volume.to_owned(), identity, limits })
            }
            _ => Err(bad()),
        }
    }

    /// The volume it is on.
    #[must_use]
    pub fn volume(&self) -> &str {
        match self {
            Self::Snapshot { volume, .. } | Self::Staging { volume, .. } => volume,
        }
    }

    /// The staging place it is, if it is one.
    #[must_use]
    pub fn staging(&self) -> Option<Staging<'_>> {
        match self {
            Self::Staging { volume, identity, limits } => {
                Some(Staging { volume, identity: *identity, limits: *limits })
            }
            Self::Snapshot { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_back_as_it_was_written_and_nothing_else_reads() {
        for kept in [
            Kept::Snapshot { volume: "wl_1.data".into(), name: "job-bkp-1".into() },
            Kept::Staging { volume: "wl_1.data".into(), identity: 257, limits: (1 << 30, 0) },
        ] {
            assert_eq!(Kept::from_line(&kept.to_line()).unwrap(), kept);
        }
        for bad in ["", "staging wl_1.data x 1 2", "snapshot wl_1.data", "other a b"] {
            assert!(Kept::from_line(bad).is_err(), "{bad:?}");
        }
    }
}
