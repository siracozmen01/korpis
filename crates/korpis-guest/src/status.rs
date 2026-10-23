// SPDX-License-Identifier: AGPL-3.0-or-later

//! How far a guest's run got, written by `korpis-guest` in the config drive's last block: that it
//! has started, once its serial port is open and what is typed on it is kept, and how the run
//! ended, before it powers off. The driver reads the first before it sends a console input and
//! the second once the microVM has exited, and clears the block before each run: sixteen bytes
//! of fixed layout, so the host parses nothing the guest could shape beyond a number. A drive
//! with no ending is a guest that never got as far as writing one, which the driver reports as
//! such.

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

/// The record's kind for a run that has started and not ended.
const STARTED: u8 = 4;

fn encode(kind: u8, value: i32) -> [u8; 16] {
    let mut record = [0; 16];
    let (magic, rest) = record.split_at_mut(MAGIC.len());
    magic.copy_from_slice(MAGIC);
    if let Some((k, rest)) = rest.split_first_mut() {
        *k = kind;
        rest.iter_mut().zip(value.to_le_bytes()).for_each(|(b, v)| *b = v);
    }
    record
}

/// Writes `record` on `drive` and syncs it, so the host reads it at once.
fn put(drive: &mut std::fs::File, record: &[u8; 16]) -> std::io::Result<()> {
    drive.seek(SeekFrom::Start(OFFSET))?;
    drive.write_all(record)?;
    drive.sync_all()
}

/// The record on `drive`, its kind and value, if there is one.
fn get(drive: &mut (impl Read + Seek)) -> std::io::Result<Option<(u8, i32)>> {
    drive.seek(SeekFrom::Start(OFFSET))?;
    let mut record = [0; 16];
    match drive.read_exact(&mut record) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let (magic, rest) = record.split_at(MAGIC.len());
    let value = rest.get(1..5).and_then(|v| v.try_into().ok()).map(i32::from_le_bytes);
    Ok((magic == MAGIC).then(|| rest.first().copied().zip(value)).flatten())
}

/// Says on `drive` that the run has started.
pub fn started(drive: &mut std::fs::File) -> std::io::Result<()> {
    put(drive, &encode(STARTED, 0))
}

/// Whether the guest on `drive` has started its run, ended or not.
pub fn has_started(drive: &mut (impl Read + Seek)) -> std::io::Result<bool> {
    Ok(get(drive)?.is_some_and(|(kind, _)| (1..=STARTED).contains(&kind)))
}

/// Clears the record on `drive`, before a run.
pub fn clear(drive: &mut std::fs::File) -> std::io::Result<()> {
    put(drive, &[0; 16])
}

impl Ended {
    fn kind(self) -> (u8, i32) {
        match self {
            Self::Exited(code) => (1, code),
            Self::Signalled(signal) => (2, signal),
            Self::Failed => (3, 0),
        }
    }

    fn decode((kind, value): (u8, i32)) -> Option<Self> {
        match kind {
            1 => Some(Self::Exited(value)),
            2 => Some(Self::Signalled(value)),
            3 => Some(Self::Failed),
            _ => None,
        }
    }

    /// Writes the record on `drive` and syncs it.
    pub fn write(self, drive: &mut std::fs::File) -> std::io::Result<()> {
        let (kind, value) = self.kind();
        put(drive, &encode(kind, value))
    }

    /// How the run on `drive` ended, if the guest said.
    pub fn read(drive: &mut (impl Read + Seek)) -> std::io::Result<Option<Self>> {
        Ok(get(drive)?.and_then(Self::decode))
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

    #[test]
    fn a_run_is_started_then_ended_and_cleared_before_the_next() {
        let mut drive = tempfile::tempfile().unwrap();
        drive.set_len(crate::config::SIZE).unwrap();
        assert!(!has_started(&mut drive).unwrap());
        started(&mut drive).unwrap();
        assert!(has_started(&mut drive).unwrap());
        assert_eq!(Ended::read(&mut drive).unwrap(), None, "started is not ended");
        Ended::Exited(0).write(&mut drive).unwrap();
        assert!(has_started(&mut drive).unwrap(), "an ended run had started");
        clear(&mut drive).unwrap();
        assert!(!has_started(&mut drive).unwrap() && Ended::read(&mut drive).unwrap().is_none());
    }
}
