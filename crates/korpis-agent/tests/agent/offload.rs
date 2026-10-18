// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offloading a console stream (§4 and question 1 of docs/14-streams.md), against a real object
//! store (MinIO, `scripts/test-tools.sh`; skipped without it): nothing is offloaded while no
//! repository takes the stream, and nothing is lost meanwhile; once one does, every segment the
//! node still holds goes, sealed under the workload's key, and is journaled as offloaded; an
//! agent that restarts goes on from where it was; and a stream whose workload left is followed
//! until it is offloaded through.

use korpis_agent::jobs::JobAccesses;
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::offload::Offloader;
use korpis_backup::Repo;
use korpis_backup::keys::Keys;
use korpis_backup::offload::segment_id;
use korpis_backup::s3::S3Store;
use korpis_backup::store::ObjectStore as _;
use korpis_backup::testing::{Minio, SECRET, USER};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::changes::{self, Change};
use korpis_stream::dir::StreamDir;
use korpis_stream::redact::Redactor;
use korpis_stream::segment::Sealed;
use korpis_stream::writer::{Limits, StreamWriter};
use korpis_stream::{Kind, Source};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const T0: i64 = 1_760_000_000_000_000_000;
const KEY: [u8; 32] = [7; 32];
const REPOSITORY: &str = "rep_0192a5b4000070008000000000000002";

/// Two sealed segments of `lines` each, recorded in the changes log as the writer records them.
fn written(dir: &Path, lines: [&[&str]; 2]) -> Vec<Sealed> {
    let mut w = StreamWriter::open(StreamDir::new(dir), Limits::default(), Redactor::default(), T0)
        .unwrap();
    let mut sealed = Vec::new();
    for (n, segment) in lines.iter().enumerate() {
        for line in *segment {
            w.write(Source::Stdout, Kind::Text, line.as_bytes(), T0 + i64::try_from(n).unwrap())
                .unwrap();
        }
        w.seal().unwrap();
        let made = w.changes().sealed;
        let recorded: Vec<Change> = made.iter().cloned().map(Change::Sealed).collect();
        changes::append(dir, &recorded).unwrap();
        sealed.extend(made);
    }
    sealed
}

/// Answers the next request sent up with `outcome`, checking it asks to offload `workload`.
async fn answer(
    rx: &mut mpsc::Receiver<agent::AgentMessage>,
    jobs: &JobAccesses,
    workload: &str,
    outcome: Outcome,
) {
    let Some(Up::JobAccessRequest(r)) = rx.recv().await.unwrap().message else { panic!() };
    assert_eq!((r.offload.as_str(), r.plan.as_str()), (workload, ""));
    jobs.answer(agent::JobAccess {
        request: r.request,
        outcome: Some(outcome),
        ..Default::default()
    });
}

fn grant(minio: &Minio, prefix: &str) -> Outcome {
    let store = pb::S3Access {
        endpoint: minio.endpoint.clone(),
        region: "us-east-1".into(),
        bucket: "backups".into(),
        key_prefix: prefix.into(),
        access_key_id: USER.into(),
        secret_access_key: SECRET.into(),
        path_style: true,
        ..Default::default()
    };
    let expires = std::time::SystemTime::now() + Duration::from_secs(3600);
    let seconds = expires.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    Outcome::Grant(Box::new(agent::JobGrant {
        data_key: KEY.to_vec(),
        store: store.into(),
        repository: REPOSITORY.into(),
        expires_at: buffa_types::google::protobuf::Timestamp {
            seconds: i64::try_from(seconds).unwrap(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }))
}

fn none() -> Outcome {
    Outcome::Refused(Box::new(pb::Error {
        reason: "offload.none".into(),
        retryable: true,
        ..Default::default()
    }))
}

/// The segments journaled as offloaded, by first offset, each naming the repository it went to.
async fn offloaded(journal: &JournalHandle) -> Vec<u64> {
    let records = journal.read_after(0, 1 << 20).await.unwrap();
    records
        .into_iter()
        .filter_map(|r| match r.body {
            Some(Body::StreamSegment(e))
                if e.event.as_known() == Some(agent::SegmentEvent::SEGMENT_EVENT_OFFLOADED) =>
            {
                let s = e.segment.as_option()?;
                assert!(s.offloaded && s.repository == REPOSITORY, "{s:?}");
                Some(s.first_offset)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_stream_is_offloaded_once_a_repository_takes_it_and_from_where_it_was() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let (state, w) = (scratch.path().join("state"), "0192a5b4-0000-7000-8000-000000000001");
    std::fs::create_dir_all(&state).unwrap();
    let dir = scratch.path().join("console");
    let sealed = written(&dir, [&["Starting server", "Done"], &["Player Alex joined"]]);
    let (located, watching) = watch::channel(BTreeMap::from([(w.to_owned(), dir.clone())]));
    let journal = JournalHandle::open(&state.join("journal"), Bounds::default()).unwrap();
    let positions = state.join("offloaded");
    let started = |jobs: &Arc<JobAccesses>| {
        let wiring = (journal.clone(), Arc::clone(jobs));
        Offloader::new(&positions, watching.clone(), wiring, Duration::from_millis(10)).unwrap()
    };

    // No repository takes the stream: nothing goes, and nothing is passed over.
    let (tx, mut rx) = mpsc::channel(8);
    let jobs = JobAccesses::new(tx);
    let mut first = started(&jobs);
    first.pass().await;
    answer(&mut rx, &jobs, w, none()).await;
    first.pass().await;
    assert!(offloaded(&journal).await.is_empty());

    // Restarted once one does: everything still held goes, from the beginning.
    let (tx, mut rx) = mpsc::channel(8);
    let jobs = JobAccesses::new(tx);
    let mut second = started(&jobs);
    second.pass().await;
    answer(&mut rx, &jobs, w, grant(&minio, &format!("korpis/streams/{w}/"))).await;
    second.pass().await;
    assert_eq!(offloaded(&journal).await, [0, 2]);
    let store = S3Store::new(minio.access("backups", &format!("korpis/streams/{w}/"))).unwrap();
    let repo = Repo::new(Arc::new(store), Arc::new(Keys::derive(&KEY)));
    for s in &sealed {
        let id = segment_id(None, s.first_offset, &s.digest).unwrap();
        let held = std::fs::read(StreamDir::new(&dir).sealed_path(s.first_offset)).unwrap();
        assert_eq!(repo.segment(&id).await.unwrap(), held, "the segment, whole");
    }
    let other = S3Store::new(minio.access("backups", "korpis/")).unwrap();
    assert_eq!(other.list("streams/").await.unwrap().len(), 2, "beside the backups, not in them");

    // Restarted again: nothing goes twice, nothing is asked for.
    let (tx, mut rx) = mpsc::channel(8);
    let mut third = started(&JobAccesses::new(tx));
    third.pass().await;
    assert!(rx.try_recv().is_err());
    assert_eq!(offloaded(&journal).await, [0, 2]);

    // The workload left: its stream, offloaded through, is no longer followed.
    located.send_replace(BTreeMap::new());
    third.pass().await;
    assert_eq!(std::fs::read_to_string(&positions).unwrap(), "");
}

#[tokio::test]
async fn a_run_s_stream_is_offloaded_beneath_its_run_with_its_workload_s_access() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (w, run) = ("wl_0192a5b4000070008000000000000001", "run_0192a5b4000070008000000000000009");
    let dir = scratch.path().join("run-console");
    let sealed = written(&dir, [&["backing up"], &["done"]]);
    let name = korpis_agent::reconcile::streams::stream_name(w, run);
    let (_located, watching) = watch::channel(BTreeMap::from([(name, dir.clone())]));
    let journal = JournalHandle::open(&state.join("journal"), Bounds::default()).unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let jobs = JobAccesses::new(tx);
    let wiring = (journal.clone(), Arc::clone(&jobs));
    let positions = state.join("offloaded");
    let mut offloader =
        Offloader::new(&positions, watching, wiring, Duration::from_millis(10)).unwrap();
    offloader.pass().await;
    // The access asked for is the workload's: a run's segments go beneath its stream prefix.
    answer(&mut rx, &jobs, w, grant(&minio, &format!("korpis/streams/{w}/"))).await;
    offloader.pass().await;
    assert_eq!(offloaded(&journal).await, [0, 1]);
    let records = journal.read_after(0, 1 << 20).await.unwrap();
    let runs: Vec<String> = records
        .into_iter()
        .filter_map(|r| match r.body {
            Some(Body::StreamSegment(e)) => Some(e.run),
            _ => None,
        })
        .collect();
    assert_eq!(runs, [run, run], "journaled as the run's");
    let store = S3Store::new(minio.access("backups", &format!("korpis/streams/{w}/"))).unwrap();
    let repo = Repo::new(Arc::new(store), Arc::new(Keys::derive(&KEY)));
    for s in &sealed {
        let id = segment_id(Some(run), s.first_offset, &s.digest).unwrap();
        assert!(id.starts_with(&format!("runs/{run}/")), "{id}");
        let held = std::fs::read(StreamDir::new(&dir).sealed_path(s.first_offset)).unwrap();
        assert_eq!(repo.segment(&id).await.unwrap(), held, "beneath its run, whole");
        let elsewhere = segment_id(None, s.first_offset, &s.digest).unwrap();
        assert!(repo.segment(&elsewhere).await.is_err(), "and nowhere a workload's would be");
    }
}
