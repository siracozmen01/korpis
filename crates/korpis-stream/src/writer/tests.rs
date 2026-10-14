// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::read::read;
use crate::redact::Secret;

const SECOND: i64 = 1_000_000_000;
const T0: i64 = 1_760_000_000 * SECOND;

fn writer(root: &std::path::Path, limits: Limits) -> StreamWriter {
    let secret = Secret { name: "rcon_password".into(), value: b"hunter22".to_vec() };
    let (redactor, _) = Redactor::new(&[secret]).unwrap();
    StreamWriter::open(StreamDir::new(root), limits, redactor, T0).unwrap()
}

fn bodies(root: &std::path::Path) -> Vec<String> {
    let batch = read(&StreamDir::new(root), 0, 1000).unwrap();
    batch.records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect()
}

#[test]
fn a_burst_beyond_the_rate_is_dropped_and_said_to_be() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits { rate: 10, burst: 10, ..Limits::default() };
    let mut w = writer(dir.path(), limits);
    w.write(Source::Stdout, Kind::Text, b"saved", T0).unwrap();
    for _ in 0..3 {
        w.write(Source::Stdout, Kind::Text, b"spam spam", T0).unwrap();
    }
    w.write(Source::Stdout, Kind::Text, b"left", T0 + 2 * SECOND).unwrap();
    let got = bodies(dir.path());
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0], "saved");
    let gap: serde_json::Value = serde_json::from_str(&got[1]).unwrap();
    assert_eq!((gap["records"].as_u64(), gap["bytes"].as_u64()), (Some(3), Some(27)));
    assert_eq!(got[2], "left", "the gap is written before the next record admitted");
}

#[test]
fn secrets_never_reach_the_file_and_overlong_records_are_cut() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits { max_record: 64, ..Limits::default() };
    let mut w = writer(dir.path(), limits);
    w.write(Source::Stdout, Kind::Text, b"RCON password hunter22", T0).unwrap();
    w.write(Source::Stderr, Kind::Text, &[b'x'; 72], T0).unwrap();
    w.tick(T0).unwrap();
    let raw: Vec<u8> = std::fs::read_dir(dir.path())
        .unwrap()
        .flat_map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert!(!raw.windows(8).any(|w| w == b"hunter22"), "redacted before persistence");
    let got = bodies(dir.path());
    assert!(got[0].ends_with("(redacted: rcon_password)"), "{got:?}");
    assert_eq!(got[1].len(), 64);
    let gap: serde_json::Value = serde_json::from_str(&got[2]).unwrap();
    assert_eq!(gap["bytes"].as_u64(), Some(8));
}

#[test]
fn segments_seal_by_size_and_age_are_pruned_and_offsets_continue_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits { segment_bytes: 200, retention_bytes: 400, ..Limits::default() };
    let mut w = writer(dir.path(), limits);
    for i in 0..30 {
        w.write(Source::Stdout, Kind::Text, format!("line {i:02} of thirty").as_bytes(), T0)
            .unwrap();
    }
    let changes = w.changes();
    assert!(changes.sealed.len() >= 3, "{changes:?}");
    assert!(!changes.pruned.is_empty(), "the oldest pruned to the retention");
    let first_held = StreamDir::new(dir.path()).segments().unwrap()[0].first_offset;
    let batch = read(&StreamDir::new(dir.path()), 0, 1000).unwrap();
    assert_eq!(batch.missing, Some((0, first_held)), "what was pruned is said to be missing");
    drop(w);

    let mut w = writer(dir.path(), limits);
    assert_eq!(w.next_offset(), 30, "a restart continues the offsets");
    w.write(Source::Stdout, Kind::Text, b"after", T0).unwrap();
    w.tick(T0 + 301 * SECOND).unwrap();
    assert_eq!(w.changes().sealed.last().map(|s| s.end_offset), Some(31), "sealed by age");
    let batch = read(&StreamDir::new(dir.path()), 30, 10).unwrap();
    assert_eq!(batch.records.len(), 1);
    assert_eq!(batch.next, 31);
}
