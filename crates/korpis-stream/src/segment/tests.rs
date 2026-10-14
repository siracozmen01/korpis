// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn text(offset: u64, body: &str) -> Record {
    Record {
        offset,
        time_nanos: 1_760_000_000_000_000_000,
        source: Source::Stdout,
        kind: Kind::Text,
        body: body.as_bytes().to_vec(),
    }
}

#[test]
fn records_survive_a_seal_and_a_torn_write_is_cut_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seg");
    let mut w = SegmentWriter::create(&path, 10).unwrap();
    w.append(&text(10, "one")).unwrap();
    w.append(&Record { source: Source::File("/data/log".into()), ..text(11, "two") }).unwrap();
    assert!(w.append(&text(13, "skip")).is_err(), "offsets are consecutive");
    drop(w);
    let mut torn = OpenOptions::new().append(true).open(&path).unwrap();
    torn.write_all(&frame(&text(12, "three"))[..9]).unwrap();
    let mut w = SegmentWriter::recover(&path).unwrap();
    assert_eq!(w.end_offset(), 12, "the torn frame is not a record");
    w.append(&text(12, "three")).unwrap();
    let sealed = w.seal().unwrap();
    assert_eq!((sealed.first_offset, sealed.end_offset), (10, 13));
    assert_eq!(sealed.bytes, std::fs::metadata(&path).unwrap().len());
    let (records, closed) = records_from(&path, 11, 10).unwrap();
    assert!(closed);
    let bodies: Vec<_> = records.iter().map(|r| r.body.clone()).collect();
    assert_eq!(bodies, [b"two".to_vec(), b"three".to_vec()]);
    assert_eq!(records[0].source, Source::File("/data/log".into()));
    let digest = Sha256::digest(
        &std::fs::read(&path).unwrap()[..usize::try_from(sealed.bytes - FOOTER).unwrap()],
    );
    assert_eq!(
        sealed.digest,
        format!("sha256:{}", hex(&digest)),
        "the digest addresses the content"
    );
    assert!(SegmentWriter::recover(&path).is_err(), "a sealed segment is not reopened");
    assert_eq!(sealed_info(&path).unwrap(), Some(sealed.clone()), "read from the end alone");
}
