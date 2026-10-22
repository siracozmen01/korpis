// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a guest's run ended, written by `korpis-guest` in the config drive's last block before it
//! powers off, and read there by the driver once the microVM has exited: sixteen bytes of fixed
//! layout, so the host parses nothing the guest could shape beyond a number. A drive with no
//! record is a guest that never got as far as writing one, which the driver reports as such.

use std::io::{Read, Seek, SeekFrom, Write};

/// Where the record is on the config drive: its last 4 KiB block.
pub const OFFSET: u64 = crate::config::SIZE - 4096;

/// What the record begins with.
const MAGIC: &[u8; 8] = b"KORPISX1";

/// How the entry ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It exited with this status.
    Exited(i32),
    /// A signal ended it.
    Signalled(i32),
    /// The guest's init failed before or while running it; its console says why.
    Failed,
}

impl Ended {
    fn encode(self) -> [u8; 16] {
        let (kind, value) = match self {
            Self::Exited(code) => (1u8, code),
            Self::Signalled(signal) => (2, signal),
            Self::Failed => (3, 0),
        };
        let mut record = [0; 16];
        let (magic, rest) = record.split_at_mut(MAGIC.len());
        magic.copy_from_slice(MAGIC);
        if let Some((k, rest)) = rest.split_first_mut() {
            *k = kind;
            rest.iter_mut().zip(value.to_le_bytes()).for_each(|(b, v)| *b = v);
        }
        record
    }

    fn decode(record: &[u8; 16]) -> Option<Self> {
        let (magic, rest) = record.split_at(MAGIC.len());
        if magic != MAGIC {
            return None;
        }
        let value = i32::from_le_bytes(rest.get(1..5)?.try_into().ok()?);
        match rest.first()? {
            1 => Some(Self::Exited(value)),
            2 => Some(Self::Signalled(value)),
            3 => Some(Self::Failed),
            _ => None,
        }
    }

    /// Writes the record on `drive` and syncs it.
    pub fn write(self, drive: &mut std::fs::File) -> std::io::Result<()> {
        drive.seek(SeekFrom::Start(OFFSET))?;
        drive.write_all(&self.encode())?;
        drive.sync_all()
    }

    /// The record on `drive`, if the guest wrote one.
    pub fn read(drive: &mut (impl Read + Seek)) -> std::io::Result<Option<Self>> {
        drive.seek(SeekFrom::Start(OFFSET))?;
        let mut record = [0; 16];
        match drive.read_exact(&mut record) {
            Ok(()) => Ok(Self::decode(&record)),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ending_is_read_back_and_a_blank_drive_holds_none() {
        let mut drive = tempfile::tempfile().unwrap();
        drive.set_len(crate::config::SIZE).unwrap();
        assert_eq!(Ended::read(&mut drive).unwrap(), None, "nothing written yet");
        for ended in [Ended::Exited(0), Ended::Exited(-3), Ended::Signalled(9), Ended::Failed] {
            ended.write(&mut drive).unwrap();
            assert_eq!(Ended::read(&mut drive).unwrap(), Some(ended));
        }
        let mut short = std::io::Cursor::new(vec![0; 100]);
        assert_eq!(Ended::read(&mut short).unwrap(), None, "a drive too short holds none");
    }
}
