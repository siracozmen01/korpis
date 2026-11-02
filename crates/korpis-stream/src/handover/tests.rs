// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::read::{end_offset, read};
use crate::redact::Redactor;
use crate::writer::{Limits, StreamWriter};

const T0: i64 = 1_760_000_000_000_000_000;

fn writer(dir: &Path) -> StreamWriter {
    let (redactor, _) = Redactor::new(&[]).unwrap();
    StreamWriter::open(StreamDir::new(dir), Limits::default(), redactor, T0).unwrap()
}

#[test]
fn a_stream_short_of_what_was_written_continues_after_a_gap_at_the_offset_handed_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = writer(dir.path());
    for line in [b"one".as_slice(), b"two", b"three"] {
        w.write(Source::Stdout, Kind::Text, line, T0).unwrap();
    }
    w.seal().unwrap();
    drop(w);

    assert!(continue_at(dir.path(), 7, T0).unwrap(), "four records were not carried");
    assert!(!continue_at(dir.path(), 7, T0).unwrap(), "once");
    assert_eq!(end_offset(&StreamDir::new(dir.path())).unwrap(), 7);
    let batch = read(&StreamDir::new(dir.path()), 0, 100).unwrap();
    let offsets: Vec<u64> = batch.records.iter().map(|r| r.offset).collect();
    assert_eq!(offsets, [0, 1, 2, 6]);
    let gap: serde_json::Value = serde_json::from_slice(&batch.records[3].body).unwrap();
    assert_eq!(gap["records"].as_u64(), Some(4));
    let logged = changes::read_from(dir.path(), 0).unwrap();
    assert!(logged.iter().any(|(c, _)| matches!(c, Change::Sealed(s) if s.first_offset == 6)));

    let mut w = writer(dir.path());
    w.write(Source::Stdout, Kind::Text, b"after", T0).unwrap();
    w.seal().unwrap();
    assert_eq!(end_offset(&StreamDir::new(dir.path())).unwrap(), 8, "the next record takes 7");
}

#[test]
fn a_stream_that_arrived_whole_or_never_began_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = writer(dir.path());
    w.write(Source::Stdout, Kind::Text, b"only", T0).unwrap();
    w.seal().unwrap();
    assert!(!continue_at(dir.path(), 1, T0).unwrap());
    let fresh = tempfile::tempdir().unwrap();
    assert!(!continue_at(&fresh.path().join("stream"), 0, T0).unwrap(), "nothing was written");
}
