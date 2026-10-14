// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a sealed segment held in memory, as one comes back from the repository it was
//! offloaded to (§4 of docs/14-streams.md). Nothing in it is believed before its footer's digest
//! is checked against both its bytes and the digest the index holds for it, so a segment changed
//! in the store, or put in another's place, is refused rather than read; and every length in it
//! is checked against what is there before anything is allocated for it.

use crate::record::Record;
use crate::segment::{CRC, END, FOOTER, HEADER, MAGIC, VERSION, hex, parse};
use sha2::{Digest as _, Sha256};

/// Why an offloaded segment is not read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refused {
    /// It is not a sealed segment of a version this reads.
    #[error("not a sealed segment")]
    NotASegment,
    /// Its bytes are not the ones the index holds the digest of.
    #[error("its digest is not {0}")]
    Digest(String),
    /// A frame in it is not whole, though its digest held: written so by its node.
    #[error("a frame at byte {0} is not whole")]
    Torn(usize),
}

fn word(bytes: &[u8], at: usize) -> Option<[u8; 8]> {
    bytes.get(at..at + 8)?.try_into().ok()
}

/// The records of `bytes`, the sealed segment whose digest the index holds as `digest`, from
/// offset `from` on, at most `limit` of them.
///
/// # Errors
/// [`Refused`] when the segment is not one, or not the one the index names.
pub fn records(
    bytes: &[u8],
    digest: &str,
    (from, limit): (u64, usize),
) -> Result<Vec<Record>, Refused> {
    let footer = usize::try_from(FOOTER).map_err(|_| Refused::NotASegment)?;
    let header = usize::try_from(HEADER).map_err(|_| Refused::NotASegment)?;
    let body_end = bytes.len().checked_sub(footer).filter(|e| *e >= header);
    let body_end = body_end.ok_or(Refused::NotASegment)?;
    let (body, tail) = bytes.split_at(body_end);
    let sealed_with = tail.get(28..60).ok_or(Refused::NotASegment)?;
    let whole = body.get(..4) == Some(MAGIC.as_slice())
        && body.get(4) == Some(&VERSION)
        && tail.get(..4) == Some(END.as_slice());
    if !whole {
        return Err(Refused::NotASegment);
    }
    let computed = Sha256::digest(body);
    if computed[..] != *sealed_with || format!("sha256:{}", hex(&computed)) != digest {
        return Err(Refused::Digest(digest.to_owned()));
    }
    let mut out = Vec::new();
    let mut at = header;
    while at < body.len() && out.len() < limit {
        let len = word(body, at).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
        let crc = word(body, at).map(|w| u32::from_le_bytes([w[4], w[5], w[6], w[7]]));
        let (Some(len), Some(crc)) = (len, crc) else { return Err(Refused::Torn(at)) };
        let len = usize::try_from(len).map_err(|_| Refused::Torn(at))?;
        let payload = body.get(at + 8..at + 8 + len).ok_or(Refused::Torn(at))?;
        if CRC.checksum(payload) != crc {
            return Err(Refused::Torn(at));
        }
        let record = parse(payload).ok_or(Refused::Torn(at))?;
        if record.offset >= from {
            out.push(record);
        }
        at += 8 + len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Kind, Source};
    use crate::segment::SegmentWriter;

    fn sealed(dir: &std::path::Path) -> (Vec<u8>, String) {
        let path = dir.join("s");
        let mut w = SegmentWriter::create(&path, 5).unwrap();
        for (i, line) in ["one", "two", "three"].iter().enumerate() {
            let offset = 5 + u64::try_from(i).unwrap();
            let r = Record {
                offset,
                time_nanos: 1,
                source: Source::Stdout,
                kind: Kind::Text,
                body: line.as_bytes().to_vec(),
            };
            w.append(&r).unwrap();
        }
        let s = w.seal().unwrap();
        (std::fs::read(&path).unwrap(), s.digest)
    }

    #[test]
    fn a_segment_is_read_from_an_offset_only_when_it_is_the_one_the_index_names() {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, digest) = sealed(dir.path());
        let read = records(&bytes, &digest, (6, 10)).unwrap();
        let bodies: Vec<_> = read.iter().map(|r| r.body.clone()).collect();
        assert_eq!(bodies, [b"two".to_vec(), b"three".to_vec()]);
        assert_eq!(records(&bytes, &digest, (0, 1)).unwrap().len(), 1, "limited");

        let mut changed = bytes.clone();
        changed[30] ^= 1;
        assert!(matches!(records(&changed, &digest, (0, 10)), Err(Refused::Digest(_))));
        let other = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(records(&bytes, &other, (0, 10)), Err(Refused::Digest(_))));
        assert_eq!(records(&bytes[..40], &digest, (0, 10)), Err(Refused::NotASegment));
    }
}
