// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup read through the relay against a real control plane (§5.4 of
//! docs/06-storage.md), with the in-memory runtime as a driver whose volumes are block devices and
//! the in-memory helper: the backup says it holds an `ext4` image; a `backup.browse` session has
//! the node ask the control plane for the job's access, which verifies the token and records the
//! read, restore the image for reading beside the volume, and open the driver on it, its files at
//! `/backup`, and ends at its first read of a file's bytes; a `backup.read` session open at the
//! same time reads them from the same copy; the copy's job is forgotten when the last session
//! ends; and a backup the token's workload does not have is refused before anything is staged.

use crate::backup::{backed_up, serve_target, settled, start};
use crate::data::{data, end_code, frame, open, opened};
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::data::backup::{BROWSE, READ};
use korpis_agent::host::{Host as _, InMemoryHost};
use korpis_agent::runtime::memory::InMemory;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::sync::Arc;
use std::time::Duration;

/// The node's facts, its one driver taking block devices, as a microVM's driver declares.
pub(crate) fn blocks() -> korpis_agent::session::Facts {
    let mut f = facts();
    for d in &mut f.drivers {
        let mut caps = d.capabilities.as_option().cloned().unwrap_or_default();
        caps.volume_semantics = pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE.into();
        d.capabilities = caps.into();
    }
    f
}

/// A session of `action` on backup `backup` of `w`, with a token `secret` mints.
async fn session(
    cp: &ControlPlane,
    secret: &str,
    (w, backup): (&str, &str),
    action: &str,
) -> crate::data::Client {
    let minted = cp.minted(secret, w, &[action], 0).await;
    let mut request = open(&minted.token, action, w, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
        o.params = pb::BackupFiles { backup: backup.into(), ..Default::default() }.encode_to_vec();
    }
    opened(&cp.relay(), &request).await
}

fn ask(id: u64, request: File) -> pb::RelayRequest {
    data(&pb::FileRequest { id, request: Some(request), ..Default::default() }.encode_to_vec())
}

async fn answer(s: &mut crate::data::Client) -> Response {
    match frame(s).await {
        Frame::Data(bytes) => {
            pb::FileResponse::decode_from_slice(&bytes).unwrap().response.unwrap()
        }
        other => panic!("an answer, got {other:?}"),
    }
}

fn read(path: &str) -> File {
    File::Read(Box::new(pb::FileRead { path: path.into(), length: 64, ..Default::default() }))
}

/// The reading jobs the helper was asked for, and whether any is still kept.
async fn reading_jobs(host: &InMemoryHost) -> (Vec<String>, bool) {
    let started: Vec<String> =
        host.started_jobs().into_iter().filter(|j| j.starts_with("read-")).collect();
    let kept = host.list_jobs().await.unwrap().iter().any(|j| j.job.starts_with("read-"));
    (started, kept)
}

#[tokio::test]
async fn a_block_device_s_backup_is_browsed_and_read_in_a_guest_through_the_node() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    // Named as the driver the recipe's tier is placed on, which here takes block devices.
    let (runtime, host) = (InMemory::named("oci", "blk"), InMemoryHost::new());
    runtime.block_devices(true);
    let guest = dir.path().join("guest");
    std::fs::create_dir_all(guest.join("backup/world")).unwrap();
    std::fs::write(guest.join("backup/world/level.dat"), b"seed 42").unwrap();
    runtime.guest_files(Some(guest));
    let (agent, root) = start(&cp, dir.path(), (&runtime, &host), Arc::new(blocks)).await;
    serve_target(&cp, &project, &root).await;
    host.offer_content(&format!("sha256:{}", "1".repeat(64)), b"a jar");
    let w = cp.declare(&project, "mc").await;
    let (backup, _) = backed_up(&cp, &project).await;
    let taken = settled(&cp, &backup).await;
    assert_eq!(taken.image_filesystem, "ext4", "a block device's backup is its image: {taken:?}");

    let (_, browser) = cp.grant_to_new_token("browser", &[BROWSE]).await;
    let mut browsing = session(&cp, &browser, (&w, &backup), BROWSE).await;
    let list = pb::FileList { path: "/backup".into(), limit: 10, ..Default::default() };
    let listed = answer_to(&mut browsing, 1, File::List(Box::new(list))).await;
    let Response::Entries(listed) = listed else { panic!("the image's top is listed") };
    assert_eq!(listed.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["world"]);
    let (_, reader) = cp.grant_to_new_token("reader", &[READ]).await;
    let mut reading = session(&cp, &reader, (&w, &backup), READ).await;
    let read_back = answer_to(&mut reading, 1, read("/backup/world/level.dat")).await;
    assert_eq!(read_back, Response::Data(b"seed 42".to_vec()));

    let (started, kept) = reading_jobs(&host).await;
    assert!(started.len() == 1 && kept, "one copy for both sessions: {started:?}");
    let job = &started[0];
    let Some(Work::Restore(work)) = host.started(job).unwrap().work else { panic!("a restore") };
    assert!(work.reading && work.backup == uuid_of(&backup), "{work:?}");
    let image = format!("image_files /memory/.{}.read+{job}/block.ext4", work.volume);
    let through = runtime.calls().iter().filter(|c| **c == image).count();
    assert_eq!(through, 2, "each session through the driver, on the one copy");
    browsing.send(ask(2, read("/backup/world/level.dat"))).await.unwrap();
    assert_eq!(end_code(frame(&mut browsing).await), "refused", "browsing reads no file's bytes");
    drop(browsing);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(reading_jobs(&host).await.1, "the copy stays while a session reads it");
    reading.close_send();
    drop(reading);
    for _ in 0..100 {
        if !reading_jobs(&host).await.1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!reading_jobs(&host).await.1, "forgotten once the last session ends");
    for action in [BROWSE, READ] {
        let sql = "SELECT count(*) FROM effect WHERE action = $1 AND after ? 'node'";
        let (recorded,): (i64,) = cp.query_one(sql, action).await.unwrap();
        assert_eq!(recorded, 1, "{action} recorded by the control plane as it granted it");
    }

    let other = cp.declare(&project, "other").await;
    let (_, stranger) = cp.grant_to_new_token("stranger", &[BROWSE]).await;
    let mut s = session(&cp, &stranger, (&other, &backup), BROWSE).await;
    s.send(ask(1, File::List(Box::default()))).await.unwrap();
    assert_eq!(end_code(frame(&mut s).await), "refused", "not that workload's backup");
    assert_eq!(reading_jobs(&host).await.0.len(), 1, "nothing staged for it");
    agent.stop().await;
}

async fn answer_to(s: &mut crate::data::Client, id: u64, request: File) -> Response {
    s.send(ask(id, request)).await.unwrap();
    answer(s).await
}

/// A backup's identifier as the helper is given it: the uuid of its wire form.
fn uuid_of(backup: &str) -> String {
    korpis_core::id::Id::parse(korpis_core::id::IdKind::Backup, backup).unwrap().uuid().to_string()
}
