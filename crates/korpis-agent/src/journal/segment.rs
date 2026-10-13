// SPDX-License-Identifier: AGPL-3.0-or-later

//! One file of the journal. A segment is named by the first sequence number it may hold, begins
//! with an eight-byte magic naming the format, and holds frames: the record's length and its
//! CRC-32C, four bytes each, little-endian, then the record as protobuf. A frame is written with
//! one call and synced before the append returns; a frame cut short by a crash, or one whose
//! checksum does not match, ends what the segment is believed to hold.

use crate::durable::sync_dir;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1::JournalRecord;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

/// The format, version 1.
const MAGIC: &[u8; 8] = b"KJOURNL1";
/// The magic's length.
pub(super) const HEADER: u64 = 8;
/// A frame's length and checksum.
pub(super) const FRAME: u64 = 8;
/// The largest record a frame holds: anything larger is refused before it is written.
pub(super) const MAX_RECORD: usize = 1 << 20;
const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);
const SUFFIX: &str = "journal";

fn invalid(why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_owned())
}

fn size(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The file name of the segment beginning at `first`.
pub(super) fn file_name(first: u64) -> String {
    format!("{first:020}.{SUFFIX}")
}

/// The first sequence number of the segment at `path`, if it is one.
pub(super) fn first_of(path: &Path) -> Option<u64> {
    if path.extension()? != SUFFIX {
        return None;
    }
    path.file_stem()?.to_str()?.parse().ok()
}

/// `record` as a frame.
pub(super) fn frame(record: &JournalRecord) -> std::io::Result<Vec<u8>> {
    let payload = record.encode_to_vec();
    if payload.len() > MAX_RECORD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("a journal record of {} bytes is larger than {MAX_RECORD}", payload.len()),
        ));
    }
    let len = u32::try_from(payload.len()).map_err(|_| invalid("record length"))?;
    let mut out = Vec::with_capacity(payload.len().saturating_add(8));
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&CRC.checksum(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Reads until `buf` is full or the file ends; how much was read.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while let Some(rest) = buf.get_mut(read..) {
        if rest.is_empty() {
            break;
        }
        match r.read(rest) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

/// What follows in a segment.
enum Next {
    /// A record, and the length of its frame.
    Record(Box<JournalRecord>, u64),
    /// The segment ends cleanly.
    End,
    /// A frame cut short, or one that does not check.
    Damaged,
}

fn next(r: &mut impl Read) -> std::io::Result<Next> {
    let mut head = [0u8; 8];
    match fill(r, &mut head)? {
        0 => return Ok(Next::End),
        8 => {}
        _ => return Ok(Next::Damaged),
    }
    let word = |b: &[u8]| <[u8; 4]>::try_from(b).ok().map(u32::from_le_bytes);
    let (len, sum) = head.split_at(4);
    let (Some(len), Some(sum)) = (word(len), word(sum)) else { return Ok(Next::Damaged) };
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len > MAX_RECORD {
        return Ok(Next::Damaged);
    }
    let mut payload = vec![0u8; len];
    if fill(r, &mut payload)? != len || CRC.checksum(&payload) != sum {
        return Ok(Next::Damaged);
    }
    Ok(match JournalRecord::decode_from_slice(&payload) {
        Ok(record) => Next::Record(Box::new(record), FRAME.saturating_add(size(len))),
        Err(_) => Next::Damaged,
    })
}

/// How a segment ended when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ending {
    /// At its end.
    Clean,
    /// At a frame that does not check: what follows it is not believed.
    Damaged,
    /// Its magic is missing or wrong: nothing in it is believed.
    Unformatted,
}

/// Reads the frames of the segment at `path` from `offset` (the first frame if it is below the
/// header), handing each record and the offset just past it to `visit` until `visit` says stop;
/// how it ended, and the offset where it stopped.
pub(super) fn read(
    path: &Path,
    offset: u64,
    mut visit: impl FnMut(JournalRecord, u64) -> bool,
) -> std::io::Result<(Ending, u64)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    if fill(&mut r, &mut magic)? != magic.len() || &magic != MAGIC {
        return Ok((Ending::Unformatted, 0));
    }
    let mut at = offset.max(HEADER);
    r.seek(SeekFrom::Start(at))?;
    loop {
        match next(&mut r)? {
            Next::End => return Ok((Ending::Clean, at)),
            Next::Damaged => return Ok((Ending::Damaged, at)),
            Next::Record(record, len) => {
                at = at.saturating_add(len);
                if !visit(*record, at) {
                    return Ok((Ending::Clean, at));
                }
            }
        }
    }
}

/// The segment being appended to.
#[derive(Debug)]
pub(super) struct Active {
    /// Where it is.
    pub(super) path: PathBuf,
    /// The first sequence number it may hold.
    pub(super) first: u64,
    /// Its length: everything up to here is synced and believed.
    pub(super) len: u64,
    /// Whether it holds a record yet.
    pub(super) holds: bool,
    file: File,
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

impl Active {
    /// A new, empty segment in `dir` beginning at `first`.
    pub(super) fn create(dir: &Path, first: u64) -> std::io::Result<Self> {
        let path = dir.join(file_name(first));
        let mut file = options().create(true).truncate(true).open(&path)?;
        file.write_all(MAGIC)?;
        file.sync_all()?;
        sync_dir(dir)?;
        Ok(Self { path, first, len: HEADER, holds: false, file })
    }

    /// The segment at `path`, cut back to the `len` bytes that were read as whole frames; a
    /// segment that holds nothing believable is formatted again.
    pub(super) fn reopen(path: &Path, first: u64, len: u64, holds: bool) -> std::io::Result<Self> {
        if len < HEADER {
            let dir = path.parent().unwrap_or_else(|| Path::new("."));
            return Self::create(dir, first);
        }
        let mut file = options().open(path)?;
        if file.metadata()?.len() != len {
            file.set_len(len)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::Start(len))?;
        Ok(Self { path: path.to_owned(), first, len, holds, file })
    }

    /// Appends `frame` and syncs it; a failed append is cut back, so the segment never holds
    /// half a frame it did not report.
    pub(super) fn append(&mut self, frame: &[u8]) -> std::io::Result<()> {
        let written = self.file.write_all(frame).and_then(|()| self.file.sync_data());
        if let Err(e) = written {
            let _ = self.file.set_len(self.len);
            let _ = self.file.seek(SeekFrom::Start(self.len));
            return Err(e);
        }
        self.len = self.len.saturating_add(size(frame.len()));
        self.holds = true;
        Ok(())
    }
}
