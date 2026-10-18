// SPDX-License-Identifier: AGPL-3.0-or-later

//! A range of a stream shared as a link (§8 of docs/14-streams.md), read back from its offloaded
//! segments through the control plane (MinIO; skipped without it): only the records written
//! within the range are sent, a segment wholly before it is not fetched, and the reading ends at
//! the first record past it. A ranged grant reads the stream and does nothing else.

use crate::offloads::{create, journaled, made, offload};
use crate::snapshots::running;
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope;
use std::time::{Duration, SystemTime};

const SECOND: i64 = 1_000_000_000;
const T0: i64 = 1_760_000_000 * SECOND;

fn at(nanos: i64) -> buffa_types::google::protobuf::Timestamp {
    buffa_types::google::protobuf::Timestamp {
        seconds: nanos / SECOND,
        nanos: i32::try_from(nanos % SECOND).unwrap(),
        ..Default::default()
    }
}

/// A sealed segment from offset `first` of `lines`, one a second from `T0 + first` seconds.
fn sealed(dir: &std::path::Path, first: u64, lines: &[&str]) -> (Vec<u8>, pb::StreamSegment) {
    use korpis_stream::record::{Kind, Record, Source};
    let path = dir.join(format!("{first}.kseg"));
    let mut w = korpis_stream::segment::SegmentWriter::create(&path, first).unwrap();
    let time = |offset: u64| T0 + SECOND * i64::try_from(offset).unwrap();
    for (i, line) in lines.iter().enumerate() {
        let offset = first + u64::try_from(i).unwrap();
        let body = line.as_bytes().to_vec();
        let time_nanos = time(offset);
        w.append(&Record { offset, time_nanos, source: Source::Stdout, kind: Kind::Text, body })
            .unwrap();
    }
    let s = w.seal().unwrap();
    let last = first + u64::try_from(lines.len()).unwrap() - 1;
    let segment = pb::StreamSegment {
        first_offset: s.first_offset,
        end_offset: s.end_offset,
        first_at: Some(at(time(first))).into(),
        last_at: Some(at(time(last))).into(),
        bytes: s.bytes,
        digest: s.digest,
        ..Default::default()
    };
    (std::fs::read(&path).unwrap(), segment)
}

/// A link to read `workload`'s console written from `from` to `to`, `actions` permitted.
async fn share(
    h: &Harness,
    key: &str,
    workload: &str,
    actions: &[&str],
    (from, to): (i64, i64),
) -> Result<String, connectrpc::ConnectError> {
    let hour = SystemTime::now() + Duration::from_secs(3600);
    let conditions = pb::Conditions {
        expires_at: Some(korpis_wire::timestamp(hour)).into(),
        max_uses: Some(5),
        stream_range: Some(pb::TimeRange {
            from: Some(at(from)).into(),
            to: Some(at(to)).into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    };
    let request = pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: pb::Scope {
            kind: Some(scope::Kind::Workload(workload.into())),
            ..Default::default()
        }
        .into(),
        conditions: conditions.into(),
        link_form: true,
        ..Default::default()
    };
    Ok(h.grants(h.root()).issue_grant(request).await?.into_owned().link_token)
}

async fn read_as(h: &Harness, credential: &str, workload: &str) -> Vec<String> {
    let r = pb::ReadOffloadedRequest { workload: workload.into(), ..Default::default() };
    let mut stream = h.streams(h.config(Some(credential))).read_offloaded(r).await.unwrap();
    let mut bodies = Vec::new();
    while let Some(chunk) = stream.message().await.unwrap() {
        let chunk = chunk.to_owned_message();
        bodies.extend(chunk.records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()));
    }
    bodies
}

#[tokio::test]
async fn a_shared_range_of_an_offloaded_stream_is_all_that_is_sent() {
    use agent::SegmentEvent as E;
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = korpis_backup::testing::Minio::start(scratch.path()).await.unwrap() else {
        return;
    };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, _, seq) = running(Vec::new()).await;
    crate::backup_target::serving_with(&h, crate::prunes::real(&minio)).await;
    create(&h, made("nightly", "korpis/", None, None)).await.unwrap();
    let Outcome::Grant(g) = offload(&mut s, "o1", &workload).await else { panic!() };
    let repo = crate::prunes::repo_of(&g);
    let mut records = Vec::new();
    let parts = [
        (0, ["Starting server", "Done"]),
        (2, ["Alex joined", "Alex fell"]),
        (4, ["Saved", "Stopping"]),
    ];
    for (i, (first, lines)) in parts.into_iter().enumerate() {
        let (bytes, mut segment) = sealed(scratch.path(), first, &lines);
        korpis_backup::offload::put_segment(&repo, (None, first, &segment.digest), bytes)
            .await
            .unwrap();
        segment.offloaded = true;
        segment.repository.clone_from(&g.repository);
        let n = seq + 2 * u64::try_from(i).unwrap();
        records.push(journaled(n, &workload, E::SEGMENT_EVENT_SEALED, &segment));
        records.push(journaled(n + 1, &workload, E::SEGMENT_EVENT_OFFLOADED, &segment));
    }
    crate::journal::send(&mut s, records).await;
    crate::journal::until(&mut s, |d| crate::journal::ack(d).filter(|a| *a >= seq + 5)).await;

    let range = (T0 + 2 * SECOND + SECOND / 2, T0 + 4 * SECOND + SECOND / 2);
    let link = share(&h, "range", &workload, &["workload.console.read"], range).await.unwrap();
    assert_eq!(read_as(&h, &link, &workload).await, ["Alex fell", "Saved"]);
    let shown = pb::GetStreamRequest { workload: workload.clone(), ..Default::default() };
    let shown = h.streams(h.config(Some(&link))).get_stream(shown).await.unwrap().into_owned();
    let firsts: Vec<u64> =
        shown.stream.as_option().unwrap().segments.iter().map(|s| s.first_offset).collect();
    assert_eq!(firsts, [2, 4], "its index shows only what holds some of the range");
    let root = h.installed.token().unwrap().1.expose().to_owned();
    assert_eq!(read_as(&h, &root, &workload).await.len(), 6, "an unranged reader");

    let both = ["workload.console.read", "workload.restart"];
    let e = share(&h, "wider", &workload, &both, range).await.unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "a range narrows reading, and nothing else");
}
