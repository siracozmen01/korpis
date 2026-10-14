// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offloaded segments past their retention (§4 of docs/14-streams.md), against a real object
//! store (MinIO, `scripts/test-tools.sh`; skipped without it): a segment kept longer than its
//! stream declares, from when it was offloaded, is deleted from the repository and forgotten by
//! the index, and one inside it is kept; the operator's floor keeps both; and the stream says how
//! long offloaded segments are kept, and what the floor is.

use crate::offloads::{bodies, create, journaled, made, offload, read, sealed};
use crate::prunes::{real, repo_of, sql};
use crate::snapshots::running;
use korpis_app::ports::StreamRead as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn an_offloaded_segment_past_its_retention_is_deleted_and_one_inside_it_kept() {
    use agent::SegmentEvent as E;
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = korpis_backup::testing::Minio::start(scratch.path()).await.unwrap() else {
        return;
    };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, _, seq) = running(Vec::new()).await;
    crate::backup_target::serving_with(&h, real(&minio)).await;
    create(&h, made("nightly", "korpis/", None, None)).await.unwrap();
    let Outcome::Grant(g) = offload(&mut s, "o1", &workload).await else { panic!() };
    let repo = repo_of(&g);
    let mut records = Vec::new();
    let mut ids = Vec::new();
    for (n, (first, lines)) in
        [(0, ["old", "older"]), (2, ["new", "newer"])].into_iter().enumerate()
    {
        let (bytes, mut segment) = sealed(scratch.path(), first, &lines);
        let id = korpis_backup::offload::segment_id(first, &segment.digest).unwrap();
        korpis_backup::offload::put_segment(&repo, (first, &segment.digest), bytes).await.unwrap();
        let at = seq + 2 * u64::try_from(n).unwrap();
        records.push(journaled(at, &workload, E::SEGMENT_EVENT_SEALED, &segment));
        segment.offloaded = true;
        segment.repository.clone_from(&g.repository);
        records.push(journaled(at + 1, &workload, E::SEGMENT_EVENT_OFFLOADED, &segment));
        ids.push(id);
    }
    crate::journal::send(&mut s, records).await;
    crate::journal::until(&mut s, |d| crate::journal::ack(d).filter(|a| *a >= seq + 3)).await;
    sql(
        &h,
        "UPDATE stream_segment SET offloaded_at = now() - interval '31 days'
              WHERE first_offset = 0 AND repository_id IS NOT NULL",
    )
    .await;

    // The operator's floor of forty days keeps it; without one, the declared thirty do not.
    let store = h.api.app().store();
    let floor = Duration::from_secs(40 * 86_400);
    assert!(store.expired_offloaded(SystemTime::now(), floor, 10).await.unwrap().is_empty());
    let pass = h.api.app().expire_offloaded().await.unwrap();
    assert!(pass.failed.is_empty(), "{:?}", pass.failed);
    assert_eq!(pass.deleted, 1);
    assert!(repo.segment(&ids[0]).await.is_err(), "deleted from the repository");
    assert!(repo.segment(&ids[1]).await.is_ok(), "the one inside its retention is kept");
    assert_eq!(bodies(&read(&h, &workload, (0, 0)).await.unwrap()), ["new", "newer"]);
    assert_eq!(h.api.app().expire_offloaded().await.unwrap().deleted, 0, "once");

    let ask = pb::GetStreamRequest { workload: workload.clone(), ..Default::default() };
    let got = h.streams(h.root()).get_stream(ask).await.unwrap().into_owned();
    let stream = got.stream.into_option().unwrap();
    let kept = stream.offload_retention.as_option().map(|d| d.seconds);
    assert_eq!(kept, Some(30 * 86_400), "the default, with no floor above it");
    assert_eq!(stream.retention_floor.as_option().map(|d| d.seconds), Some(0));
    let offloaded: Vec<bool> = stream.segments.iter().map(|s| s.offloaded).collect();
    assert_eq!(offloaded, [false, true]);
}

#[test]
fn the_control_plane_names_a_segment_object_as_the_node_writes_it() {
    let good = format!("sha256:{}", "0a".repeat(32));
    for (first, digest) in [(0, good.as_str()), (u64::MAX, good.as_str()), (7, "sha256:../x")] {
        let node = korpis_backup::offload::segment_id(first, digest);
        let control_plane = korpis_core::workload::stream::segment_object(first, digest);
        assert_eq!(control_plane, node.map(|id| format!("segments/{id}")), "{first} {digest}");
    }
    assert_eq!(korpis_backup::seal::Kind::Segment.prefix(), "segments/");
}

#[tokio::test]
async fn deleting_a_stream_deletes_its_offloaded_segments_at_the_next_pass() {
    use crate::volumes::{delete, ensured, volume_record};
    use agent::SegmentEvent as E;
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = korpis_backup::testing::Minio::start(scratch.path()).await.unwrap() else {
        return;
    };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, _, seq) = running(Vec::new()).await;
    crate::backup_target::serving_with(&h, real(&minio)).await;
    create(&h, made("nightly", "korpis/", None, None)).await.unwrap();
    let Outcome::Grant(g) = offload(&mut s, "o1", &workload).await else { panic!() };
    let repo = repo_of(&g);
    let (bytes, mut segment) = sealed(scratch.path(), 0, &["last words"]);
    let id = korpis_backup::offload::segment_id(0, &segment.digest).unwrap();
    korpis_backup::offload::put_segment(&repo, (0, &segment.digest), bytes).await.unwrap();
    segment.offloaded = true;
    segment.repository.clone_from(&g.repository);
    let records = vec![
        volume_record(seq, ensured(&workload, "stream_console", true, true)),
        journaled(seq + 1, &workload, E::SEGMENT_EVENT_OFFLOADED, &segment),
    ];
    crate::journal::send(&mut s, records).await;
    crate::journal::until(&mut s, |d| crate::journal::ack(d).filter(|a| *a > seq)).await;
    assert_eq!(h.api.app().expire_offloaded().await.unwrap().deleted, 0, "inside its retention");

    // Retiring the workload with its stream, approved: the stream's offloaded copies go too.
    let workloads = h.workloads(h.root());
    let deleted = workloads.delete_workload(delete(&workload, &["stream_console"])).await;
    let plan = deleted.unwrap().into_owned().plan.into_option().unwrap().id;
    let approve = crate::lifecycle::approve("retire", &plan);
    h.plans(h.root()).approve_plan(approve).await.unwrap();
    let retiring =
        crate::journal::until(&mut s, crate::journal::assignment(|a| a.retire.is_set())).await;
    crate::journal::complete(&mut s, &workload, retiring.plan.as_option().unwrap(), seq + 2).await;
    let pass = h.api.app().expire_offloaded().await.unwrap();
    assert_eq!((pass.deleted, pass.failed.len()), (1, 0));
    assert!(repo.segment(&id).await.is_err(), "deleted with the stream");
}
