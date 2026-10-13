// SPDX-License-Identifier: AGPL-3.0-or-later

use super::read::{DAMAGED, DISCARDED};
use super::*;
use korpis_proto::korpis::v1 as pb;
use std::io::Write as _;

const KIB: u64 = 1024;

fn small() -> Bounds {
    Bounds {
        bytes: 200 * KIB,
        segment: 64 * KIB,
        retention: Duration::from_secs(3600),
        coalesce: Duration::from_secs(10),
    }
}

fn at(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000 + s)
}

/// A record of about a kibibyte that is never coalesced.
fn glass(n: u64) -> Body {
    Body::BreakGlass(Box::new(agent::BreakGlass {
        operator: format!("operator-{n}"),
        reason: "x".repeat(1000),
        session: n.to_string(),
        ..Default::default()
    }))
}

fn effect(action: &str) -> Body {
    Body::Effect(Box::new(pb::Effect { action: action.into(), ..Default::default() }))
}

fn written(a: Appended) -> u64 {
    match a {
        Appended::Written(seq) => seq,
        other => panic!("expected a write, got {other:?}"),
    }
}

fn gap_of(r: &agent::JournalRecord) -> Option<(u64, u64, String)> {
    match &r.body {
        Some(Body::Gap(g)) => Some((g.first_seq, g.last_seq, g.cause.clone())),
        _ => None,
    }
}

#[test]
fn numbers_survive_a_restart_and_a_torn_tail_is_cut_away() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    assert_eq!((j.head(), j.tail()), (0, 0));
    for n in 1..=3 {
        assert_eq!(written(j.append(&glass(n), at(n)).unwrap()), n);
    }
    let active = j.active.as_ref().unwrap().path.clone();
    drop(j);
    let mut file = std::fs::OpenOptions::new().append(true).open(&active).unwrap();
    file.write_all(&[200, 0, 0, 0, 1, 2]).unwrap();
    drop(file);
    let mut j = Journal::open(dir.path(), small()).unwrap();
    assert_eq!((j.head(), j.tail()), (3, 1));
    assert_eq!(written(j.append(&glass(4), at(4)).unwrap()), 4, "never reused");
    let seqs: Vec<u64> = j.read_after(1, u64::MAX).unwrap().iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4]);
}

#[test]
fn acknowledged_segments_are_kept_for_the_retention_window_then_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    for n in 1..=150 {
        written(j.append(&glass(n), at(0)).unwrap());
    }
    assert!(j.sealed.len() >= 2, "{} sealed", j.sealed.len());
    j.acknowledge(150, at(10)).unwrap();
    assert_eq!(j.tail(), 1, "within the window, everything is kept");
    let reopened = Journal::open(dir.path(), small()).unwrap();
    assert_eq!(reopened.meta.acked_at.len(), j.sealed.len(), "the window survives a restart");
    j.maintain(at(10 + 3600)).unwrap();
    assert!(j.sealed.is_empty());
    assert!(j.tail() > 1 && j.head() == 150);
}

#[test]
fn a_cursor_older_than_the_tail_is_answered_with_a_gap_for_what_was_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), Bounds { retention: Duration::ZERO, ..small() }).unwrap();
    for n in 1..=150 {
        written(j.append(&glass(n), at(0)).unwrap());
    }
    j.acknowledge(150, at(1)).unwrap();
    let tail = j.tail();
    assert!(tail > 1);
    // A control plane restored from an older backup: its cursor is 10.
    j.acknowledge(10, at(2)).unwrap();
    let records = j.read_after(10, u64::MAX).unwrap();
    let first = records.first().unwrap();
    assert_eq!(gap_of(first), Some((11, tail - 1, DISCARDED.to_owned())));
    assert_eq!(first.seq, tail - 1);
    assert_eq!(records.last().unwrap().seq, 150);
    assert!(DAMAGED != DISCARDED);
}

#[test]
fn at_the_bound_numbers_are_lost_and_one_gap_record_names_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    let mut dropped = Vec::new();
    for n in 1..=250 {
        if let Appended::Lost(seq) = j.append(&glass(n), at(0)).unwrap() {
            dropped.push(seq);
        }
    }
    let (first, last) = (*dropped.first().unwrap(), *dropped.last().unwrap());
    assert_eq!(dropped.len(), 250 - usize::try_from(first).unwrap() + 1);
    assert!(j.used() <= small().bytes);
    drop(j);
    let mut j = Journal::open(dir.path(), small()).unwrap();
    assert_eq!(j.meta.lost, Some((first, last)), "the loss survives a restart");
    j.acknowledge(first - 1, at(1)).unwrap();
    j.maintain(at(2)).unwrap();
    assert_eq!(j.head(), last, "the gap record stands for the numbers lost");
    let gap = j.read_after(first - 1, u64::MAX).unwrap();
    assert_eq!(gap_of(gap.first().unwrap()), Some((first, last, EXHAUSTED.to_owned())));
    assert_eq!(written(j.append(&glass(0), at(3)).unwrap()), last + 1);
}

#[test]
fn a_cursor_past_the_head_moves_the_numbering_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    written(j.append(&glass(1), at(0)).unwrap());
    j.acknowledge(40, at(1)).unwrap();
    assert_eq!(written(j.append(&glass(2), at(2)).unwrap()), 41);
    drop(j);
    let mut j = Journal::open(dir.path(), small()).unwrap();
    assert_eq!(written(j.append(&glass(3), at(3)).unwrap()), 42);
}

#[test]
fn repeated_effects_are_written_once_with_a_count() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    assert_eq!(written(j.append(&effect("workload.restart"), at(0)).unwrap()), 1);
    for s in 1..=4 {
        assert_eq!(j.append(&effect("workload.restart"), at(s)).unwrap(), Appended::Coalesced);
    }
    j.maintain(at(5)).unwrap();
    assert_eq!(j.head(), 1, "the window is still open");
    j.maintain(at(11)).unwrap();
    let records = j.read_after(0, u64::MAX).unwrap();
    let counts: Vec<(u64, u64)> = records.iter().map(|r| (r.seq, r.count)).collect();
    assert_eq!(counts, vec![(1, 1), (2, 4)]);
}

#[test]
fn reading_resumes_where_it_stopped_and_bounds_its_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(dir.path(), small()).unwrap();
    for n in 1..=100 {
        written(j.append(&glass(n), at(0)).unwrap());
    }
    let mut after = 0;
    let mut batches = 0;
    loop {
        let batch = j.read_after(after, 8 * KIB).unwrap();
        let Some(last) = batch.last() else { break };
        assert_eq!(batch.first().unwrap().seq, after + 1);
        after = last.seq;
        batches += 1;
    }
    assert_eq!(after, 100);
    assert!(batches > 10, "{batches} batches");
}
