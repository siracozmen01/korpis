// SPDX-License-Identifier: AGPL-3.0-or-later

//! One segment file: a header, then record frames, then, once sealed, a footer.
//!
//! ```text
//! header  "KSEG" | version u8 = 1 | 3 bytes zero | first offset u64
//! frame   length u32 | crc32c u32 of the payload | payload
//! payload offset u64 | time i64 | source tag u8 | path length u16 | path | kind tag u8 | body
//! footer  "KEND" | end offset u64 | first time i64 | last time i64 | sha256 of every byte
//!         before the footer
//! ```
//!
//! Integers are little-endian. A frame whose bytes end early or whose checksum fails is where a
//! crash stopped a write: reading stops there, and the writer cuts the file back to it. The
//! footer's digest is the segment's address once sealed (§4 of docs/14-streams.md).

use crate::record::{Kind, Record, Source};
use sha2::{Digest as _, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"KSEG";
const END: &[u8; 4] = b"KEND";
const VERSION: u8 = 1;
/// The header's length.
pub const HEADER: u64 = 16;
/// The footer's length.
const FOOTER: u64 = 60;
const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);

/// What a sealed segment is, as the index records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The offset of its first record.
    pub first_offset: u64,
    /// One past its last.
    pub end_offset: u64,
    /// Its size on disk, footer included.
    pub bytes: u64,
    /// `sha256:` and the hex digest of everything before the footer.
    pub digest: String,
    /// The node-clock times of its first and last records, in nanoseconds since the Unix epoch;
    /// informational, as offsets order.
    pub times: (i64, i64),
}

/// A segment being written.
#[derive(Debug)]
pub struct SegmentWriter {
    file: File,
    hash: Sha256,
    first_offset: u64,
    end_offset: u64,
    bytes: u64,
    times: Option<(i64, i64)>,
}

fn frame(record: &Record) -> Vec<u8> {
    let (source, path) = record.source.tag();
    let path_len = u16::try_from(path.len()).unwrap_or(u16::MAX);
    let path = path.get(..usize::from(path_len)).unwrap_or_default();
    let mut payload = Vec::with_capacity(20 + path.len() + record.body.len());
    payload.extend_from_slice(&record.offset.to_le_bytes());
    payload.extend_from_slice(&record.time_nanos.to_le_bytes());
    payload.push(source);
    payload.extend_from_slice(&path_len.to_le_bytes());
    payload.extend_from_slice(path);
    payload.push(record.kind.tag());
    payload.extend_from_slice(&record.body);
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&CRC.checksum(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

impl SegmentWriter {
    /// A new segment at `path`, starting at `first_offset`; refuses to replace a file.
    pub fn create(path: &Path, first_offset: u64) -> io::Result<Self> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&[VERSION, 0, 0, 0]);
        header.extend_from_slice(&first_offset.to_le_bytes());
        file.write_all(&header)?;
        let mut hash = Sha256::new();
        hash.update(&header);
        Ok(Self { file, hash, first_offset, end_offset: first_offset, bytes: HEADER, times: None })
    }

    /// The unsealed segment at `path`, cut back to its last whole record and opened to append.
    pub fn recover(path: &Path) -> io::Result<Self> {
        let scan = scan(path)?;
        if scan.sealed.is_some() {
            return Err(io::Error::other("the segment is sealed"));
        }
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.set_len(scan.good)?;
        let mut hash = Sha256::new();
        let mut kept = (&file).take(scan.good);
        io::copy(&mut kept, &mut HashWriter(&mut hash))?;
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            file,
            hash,
            first_offset: scan.first_offset,
            end_offset: scan.end_offset,
            bytes: scan.good,
            times: scan.times,
        })
    }

    /// Appends `record`, which must carry the next offset.
    pub fn append(&mut self, record: &Record) -> io::Result<()> {
        if record.offset != self.end_offset {
            return Err(io::Error::other("records are appended in offset order"));
        }
        let bytes = frame(record);
        self.file.write_all(&bytes)?;
        self.hash.update(&bytes);
        self.bytes += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.end_offset += 1;
        self.times = Some(
            self.times
                .map_or((record.time_nanos, record.time_nanos), |(f, _)| (f, record.time_nanos)),
        );
        Ok(())
    }

    /// Makes what was appended durable.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// One past the last offset appended.
    #[must_use]
    pub const fn end_offset(&self) -> u64 {
        self.end_offset
    }

    /// The offset it starts at.
    #[must_use]
    pub const fn first_offset(&self) -> u64 {
        self.first_offset
    }

    /// Its size so far.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The times of its first and last records; none before the first.
    #[must_use]
    pub const fn times(&self) -> Option<(i64, i64)> {
        self.times
    }

    /// Writes the footer and makes the segment durable; what it is.
    pub fn seal(mut self) -> io::Result<Sealed> {
        let digest = self.hash.finalize();
        let times = self.times.unwrap_or_default();
        let mut footer = Vec::with_capacity(60);
        footer.extend_from_slice(END);
        footer.extend_from_slice(&self.end_offset.to_le_bytes());
        footer.extend_from_slice(&times.0.to_le_bytes());
        footer.extend_from_slice(&times.1.to_le_bytes());
        footer.extend_from_slice(&digest);
        self.file.write_all(&footer)?;
        self.file.sync_all()?;
        Ok(Sealed {
            first_offset: self.first_offset,
            end_offset: self.end_offset,
            bytes: self.bytes + FOOTER,
            digest: format!("sha256:{}", hex(&digest)),
            times,
        })
    }
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
}

/// What reading a segment through found.
#[derive(Debug)]
pub struct Scan {
    /// The offset in its header.
    pub first_offset: u64,
    /// One past the last whole record.
    pub end_offset: u64,
    /// How many bytes, from the start, hold whole frames.
    pub good: u64,
    /// The footer's end offset, if it is sealed.
    pub sealed: Option<u64>,
    /// The times of its first and last whole records; none if it holds none.
    pub times: Option<(i64, i64)>,
}

/// Reads the header of `r`; its first offset.
fn header(r: &mut impl Read) -> io::Result<u64> {
    let mut h = [0u8; 16];
    r.read_exact(&mut h)?;
    let (magic, rest) = h.split_at(4);
    let (version, first) = rest.split_at(4);
    if magic != MAGIC || version.first() != Some(&VERSION) {
        return Err(io::Error::other("not a segment of version 1"));
    }
    let first: [u8; 8] = first.try_into().map_err(|_| io::Error::other("short header"))?;
    Ok(u64::from_le_bytes(first))
}

/// What one step of reading found.
enum Step {
    Record(Record, u64),
    Sealed(u64),
    End,
}

/// The next frame of `r`: a record and its frame's length, the footer, or the end of what is
/// whole.
fn step(r: &mut impl Read) -> io::Result<Step> {
    let mut head = [0u8; 8];
    if read_all(r, &mut head)? < 8 {
        return Ok(Step::End);
    }
    let (len, crc) = head.split_at(4);
    if len == END {
        let mut end = [0u8; 8];
        return Ok(if read_all(r, &mut end)? == 8 {
            Step::Sealed(u64::from_le_bytes(end))
        } else {
            Step::End
        });
    }
    let len = u32::from_le_bytes(len.try_into().map_err(|_| io::Error::other("frame"))?);
    let crc = u32::from_le_bytes(crc.try_into().map_err(|_| io::Error::other("frame"))?);
    let mut payload = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
    if read_all(r, &mut payload)? < payload.len() || CRC.checksum(&payload) != crc {
        return Ok(Step::End);
    }
    Ok(parse(&payload).map_or(Step::End, |record| Step::Record(record, 8 + u64::from(len))))
}

fn read_all(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(buf.get_mut(filled..).unwrap_or_default())? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn parse(payload: &[u8]) -> Option<Record> {
    let offset = u64::from_le_bytes(payload.get(0..8)?.try_into().ok()?);
    let time_nanos = i64::from_le_bytes(payload.get(8..16)?.try_into().ok()?);
    let source = *payload.get(16)?;
    let path_len = usize::from(u16::from_le_bytes(payload.get(17..19)?.try_into().ok()?));
    let path = payload.get(19..19 + path_len)?;
    let kind = Kind::from_tag(*payload.get(19 + path_len)?)?;
    let body = payload.get(20 + path_len..)?.to_vec();
    Some(Record { offset, time_nanos, source: Source::from_tag(source, path)?, kind, body })
}

/// What the sealed segment at `path` is, read from its footer and its size without reading its
/// records; none if it is not sealed.
pub fn sealed_info(path: &Path) -> io::Result<Option<Sealed>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < HEADER + FOOTER {
        return Ok(None);
    }
    let mut first = [0u8; 16];
    file.read_exact(&mut first)?;
    let first_offset = u64::from_le_bytes(
        first
            .get(8..16)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| io::Error::other("header"))?,
    );
    let back = i64::try_from(FOOTER).map_err(io::Error::other)?;
    file.seek(SeekFrom::End(-back))?;
    let mut f = [0u8; 60];
    file.read_exact(&mut f)?;
    let word = |at: usize| -> io::Result<[u8; 8]> {
        f.get(at..at + 8).and_then(|b| b.try_into().ok()).ok_or_else(|| io::Error::other("footer"))
    };
    if f.get(..4) != Some(END.as_slice()) {
        return Ok(None);
    }
    Ok(Some(Sealed {
        first_offset,
        end_offset: u64::from_le_bytes(word(4)?),
        bytes: len,
        digest: format!("sha256:{}", hex(f.get(28..60).unwrap_or_default())),
        times: (i64::from_le_bytes(word(12)?), i64::from_le_bytes(word(20)?)),
    }))
}

/// Reads the segment at `path` through.
pub fn scan(path: &Path) -> io::Result<Scan> {
    let mut r = BufReader::new(File::open(path)?);
    let first_offset = header(&mut r)?;
    let mut scan =
        Scan { first_offset, end_offset: first_offset, good: HEADER, sealed: None, times: None };
    loop {
        match step(&mut r)? {
            Step::Record(record, len) => {
                scan.end_offset = record.offset + 1;
                scan.good += len;
                let t = record.time_nanos;
                scan.times = Some(scan.times.map_or((t, t), |(f, _)| (f, t)));
            }
            Step::Sealed(end) => {
                scan.sealed = Some(end);
                return Ok(scan);
            }
            Step::End => return Ok(scan),
        }
    }
}

/// The records of the segment at `path` from `offset` on, at most `limit` of them, and whether
/// the segment is sealed (so nothing will follow them in it).
pub fn records_from(path: &Path, offset: u64, limit: usize) -> io::Result<(Vec<Record>, bool)> {
    let mut r = BufReader::new(File::open(path)?);
    header(&mut r)?;
    let mut out = Vec::new();
    while out.len() < limit {
        match step(&mut r)? {
            Step::Record(record, _) if record.offset >= offset => out.push(record),
            Step::Record(..) => {}
            Step::Sealed(_) => return Ok((out, true)),
            Step::End => return Ok((out, false)),
        }
    }
    Ok((out, false))
}

#[cfg(test)]
mod tests;
