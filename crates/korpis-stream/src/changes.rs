// SPDX-License-Identifier: AGPL-3.0-or-later

//! What became of a stream's segments, as its writer records it for the agent: one JSON line per
//! seal or prune, appended to `changes` in the stream's directory and made durable before the
//! writer goes on. The agent reads it from the byte it last journaled (§4 of docs/14-streams.md),
//! so a seal the agent was not running to see is journaled when it starts again.

use crate::segment::Sealed;
use std::fs::OpenOptions;
use std::io::{self, BufRead as _, BufReader, Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// The file name.
pub const FILE: &str = "changes";

/// One change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A segment was sealed.
    Sealed(Sealed),
    /// A sealed segment was pruned to the retention.
    Pruned(Sealed),
}

impl Change {
    fn line(&self) -> String {
        let (event, s) = match self {
            Self::Sealed(s) => ("sealed", s),
            Self::Pruned(s) => ("pruned", s),
        };
        serde_json::json!({
            "event": event,
            "first_offset": s.first_offset,
            "end_offset": s.end_offset,
            "bytes": s.bytes,
            "digest": s.digest,
            "first_at": s.times.0,
            "last_at": s.times.1,
        })
        .to_string()
    }

    fn parse(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
        let t = |k: &str| v.get(k).and_then(serde_json::Value::as_i64);
        let segment = Sealed {
            first_offset: n("first_offset")?,
            end_offset: n("end_offset")?,
            bytes: n("bytes")?,
            digest: v.get("digest")?.as_str()?.to_owned(),
            times: (t("first_at")?, t("last_at")?),
        };
        match v.get("event")?.as_str()? {
            "sealed" => Some(Self::Sealed(segment)),
            "pruned" => Some(Self::Pruned(segment)),
            _ => None,
        }
    }
}

/// Appends `changes` to the log in `dir` and makes them durable.
pub fn append(dir: &Path, changes: &[Change]) -> io::Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut file = OpenOptions::new().create(true).append(true).open(dir.join(FILE))?;
    let mut text = String::new();
    for c in changes {
        text.push_str(&c.line());
        text.push('\n');
    }
    file.write_all(text.as_bytes())?;
    file.sync_data()
}

/// The changes in the log in `dir` from byte `from` on, each with the byte past its line, where
/// a reader that has taken it reads from next. A line not yet whole, or not understood, ends what
/// is read.
pub fn read_from(dir: &Path, from: u64) -> io::Result<Vec<(Change, u64)>> {
    let mut file = match std::fs::File::open(dir.join(FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    file.seek(SeekFrom::Start(from))?;
    let mut reader = BufReader::new(file);
    let mut out = Vec::new();
    let mut next = from;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            break;
        }
        let text = line.get(..line.len() - 1).and_then(|l| std::str::from_utf8(l).ok());
        let Some(change) = text.and_then(Change::parse) else { break };
        next += u64::try_from(n).map_err(io::Error::other)?;
        out.push((change, next));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_read_back_from_where_the_reader_left_off() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = Sealed {
            first_offset: 0,
            end_offset: 10,
            bytes: 400,
            digest: "sha256:ab".into(),
            times: (1, 2),
        };
        append(dir.path(), &[Change::Sealed(sealed.clone()), Change::Pruned(sealed.clone())])
            .unwrap();
        let all = read_from(dir.path(), 0).unwrap();
        let read: Vec<_> = all.iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(read, [Change::Sealed(sealed.clone()), Change::Pruned(sealed)]);
        let next = all[1].1;
        assert_eq!(read_from(dir.path(), all[0].1).unwrap(), all[1..], "from the first line's end");
        assert_eq!(read_from(dir.path(), next).unwrap(), []);
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(b"{\"event\":\"pru")
            .unwrap();
        assert_eq!(read_from(dir.path(), next).unwrap(), [], "half a line waits");
    }
}
