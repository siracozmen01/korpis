// SPDX-License-Identifier: AGPL-3.0-or-later

//! What became of a stream's segments, as its writer records it for the agent: one JSON line per
//! seal or prune, appended to `changes` in the stream's directory and made durable before the
//! writer goes on. The agent reads it from the line it last journaled (§4 of docs/14-streams.md),
//! so a seal the agent was not running to see is journaled when it starts again.

use crate::segment::Sealed;
use std::fs::OpenOptions;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::path::Path;

/// The file name.
pub const FILE: &str = "changes";

/// One change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A segment was sealed.
    Sealed(Sealed),
    /// The sealed segment starting at this offset was pruned to the retention.
    Pruned(u64),
}

impl Change {
    fn line(&self) -> String {
        match self {
            Self::Sealed(s) => serde_json::json!({
                "event": "sealed",
                "first_offset": s.first_offset,
                "end_offset": s.end_offset,
                "bytes": s.bytes,
                "digest": s.digest,
                "first_at": s.times.0,
                "last_at": s.times.1,
            }),
            Self::Pruned(first) => serde_json::json!({ "event": "pruned", "first_offset": first }),
        }
        .to_string()
    }

    fn parse(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
        let t = |k: &str| v.get(k).and_then(serde_json::Value::as_i64);
        match v.get("event")?.as_str()? {
            "sealed" => Some(Self::Sealed(Sealed {
                first_offset: n("first_offset")?,
                end_offset: n("end_offset")?,
                bytes: n("bytes")?,
                digest: v.get("digest")?.as_str()?.to_owned(),
                times: (t("first_at")?, t("last_at")?),
            })),
            "pruned" => Some(Self::Pruned(n("first_offset")?)),
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

/// The changes in the log in `dir` from line `from` on (counting from 0), and the line to read
/// from next. A line not yet whole, or not understood, ends what is read.
pub fn read_from(dir: &Path, from: usize) -> io::Result<(Vec<Change>, usize)> {
    let file = match std::fs::File::open(dir.join(FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), from)),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    let mut next = from;
    for (i, line) in BufReader::new(file).split(b'\n').enumerate().skip(from) {
        let line = line?;
        let Some(change) = std::str::from_utf8(&line).ok().and_then(Change::parse) else { break };
        out.push(change);
        next = i + 1;
    }
    Ok((out, next))
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
        append(dir.path(), &[Change::Sealed(sealed.clone()), Change::Pruned(0)]).unwrap();
        let (all, next) = read_from(dir.path(), 0).unwrap();
        assert_eq!(all, [Change::Sealed(sealed), Change::Pruned(0)]);
        assert_eq!(read_from(dir.path(), next).unwrap(), (Vec::new(), 2));
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(b"{\"event\":\"pru")
            .unwrap();
        assert_eq!(read_from(dir.path(), 2).unwrap(), (Vec::new(), 2), "half a line waits");
    }
}
