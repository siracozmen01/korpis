// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup, browsed in a live guest (§5.4 of docs/06-storage.md): the image the
//! node staged is served read-only at `/backup` by a service guest of its own, which runs beside
//! the workload's own guest; an image taken from a running guest, whose journal still needs
//! replaying, is mounted all the same; a write is refused; two sessions share one guest, which
//! ends with the last; and an image outside the volume root, one not marked a block device's, and
//! one holding a filesystem its guests do not mount are refused. Needs what the driver's runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use buffa::Message as _;
use korpis_driver_firecracker::machine::{IMAGE_AT, Machine};
use korpis_files::frame;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

mod support;

use support::{Rig, live, no_service_guest, ok, ran, until};

/// A backup's image as the helper stages one for reading, in `name` beside the volumes, holding
/// `world/level.dat`, and flagged as needing its journal replayed, as an image taken from a
/// running guest may be; its path.
fn staged(rig: &Rig, name: &str, base: u32) -> String {
    use korpis_core::workload::volume::{BLOCK_IMAGE, BLOCK_MARK};
    let dir = rig.dir.path().join("volumes").join(name);
    std::fs::create_dir(&dir).unwrap();
    rustix::fs::setxattr(&dir, BLOCK_MARK.0, BLOCK_MARK.1, rustix::fs::XattrFlags::empty())
        .unwrap();
    let files = tempfile::tempdir().unwrap();
    std::fs::create_dir(files.path().join("world")).unwrap();
    std::fs::write(files.path().join("world/level.dat"), b"seed 42").unwrap();
    let image = dir.join(BLOCK_IMAGE);
    ok(Command::new("mkfs.ext4").args(["-q", "-F", "-d"]).arg(files.path()).arg(&image).arg("16M"));
    ok(Command::new("debugfs").args(["-w", "-R", "feature needs_recovery"]).arg(&image));
    std::os::unix::fs::chown(&image, Some(base), Some(base)).unwrap();
    image.to_string_lossy().into_owned()
}

fn image(image: &str, base: u32) -> driver::StagedImage {
    driver::StagedImage {
        image: image.into(),
        workload: "wl_seen".into(),
        uid_base: base,
        uid_count: 65_536,
        filesystem: "ext4".into(),
        ..Default::default()
    }
}

/// What the worker answers `request`, asked on `s` as request `id`.
fn ask(s: &mut UnixStream, id: u64, request: Request) -> Response {
    let asked = pb::FileRequest { id, request: Some(request), ..Default::default() };
    frame::write(s, &asked.encode_to_vec()).unwrap();
    let answer = frame::read(s).unwrap().expect("an answer");
    let answer = pb::FileResponse::decode_from_slice(&answer).unwrap();
    assert_eq!(answer.id, id, "answered in order");
    answer.response.expect("a response")
}

fn names(s: &mut UnixStream, id: u64, path: &str) -> Vec<String> {
    let list = pb::FileList { path: path.into(), limit: 100, ..Default::default() };
    let Response::Entries(listed) = ask(s, id, Request::List(Box::new(list))) else {
        panic!("{path} is listed");
    };
    listed.entries.iter().map(|e| e.name.clone()).collect()
}

/// A session on staged image `i`, as a blocking stream, and the hold on its guest.
async fn session(
    machine: &Machine,
    i: &driver::StagedImage,
) -> (UnixStream, korpis_driver_firecracker::machine::Lease) {
    let session = machine.image_files(i).await.unwrap();
    let stream = session.stream.into_std().unwrap();
    stream.set_nonblocking(false).unwrap();
    (stream, session.lease.expect("a service guest serves a staged image"))
}

/// How many service guests of `machine`'s run.
fn service_guests(machine: &Machine) -> usize {
    let jails = std::fs::read_dir(machine.settings().jails_of_firecracker());
    let jails = jails.into_iter().flatten().flatten();
    jails.filter(|e| e.file_name().to_string_lossy().starts_with("svc-")).count()
}

#[tokio::test]
async fn a_staged_backup_image_is_read_in_a_guest_of_its_own_beside_the_workload_s() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let spec = rig.spec("wl_seen", "echo live > /data/now; while true; do sleep 1; done");
    let base = spec.uid_base;
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;

    let staged = image(&staged(&rig, ".wl_seen.data.read+job1", base), base);
    let (mut s, first) = session(&rig.machine, &staged).await;
    assert_eq!(names(&mut s, 1, "/"), [IMAGE_AT.trim_start_matches('/')], "the backup alone");
    assert!(names(&mut s, 2, IMAGE_AT).contains(&"world".to_owned()), "its journal replayed");
    let level = format!("{IMAGE_AT}/world/level.dat");
    let read = pb::FileRead { path: level.clone(), length: 64, ..Default::default() };
    assert_eq!(ask(&mut s, 3, Request::Read(Box::new(read))), Response::Data(b"seed 42".to_vec()));
    let w = pb::FileWrite { path: level, data: b"x".to_vec(), ..Default::default() };
    let written = ask(&mut s, 4, Request::Write(Box::new(w)));
    assert!(matches!(written, Response::Error(_)), "read-only: {written:?}");

    let (mut again, second) = session(&rig.machine, &staged).await;
    assert_eq!(service_guests(&rig.machine), 1, "one guest serves both sessions");
    assert_eq!(names(&mut again, 1, "/"), ["backup"]);
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/now"]).await;
    assert_eq!(said, "live\n", "the workload's own guest runs on, holding its own volume");
    drop((s, first));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(service_guests(&rig.machine), 1, "the guest stays while a session holds it");
    drop((again, second));
    no_service_guest(&rig.machine).await;
    rig.machine.destroy(&id).await.unwrap();
}

#[tokio::test]
async fn only_a_block_device_s_image_beneath_the_volume_root_is_served() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let base = 7 * 65_536;
    let path = staged(&rig, ".wl_seen.data.read+job2", base);
    let outside = rig.dir.path().join("block.ext4");
    std::fs::copy(&path, &outside).unwrap();
    let unmarked = rig.dir.path().join("volumes/.wl_seen.data.read+job3");
    std::fs::create_dir(&unmarked).unwrap();
    std::fs::copy(&path, unmarked.join("block.ext4")).unwrap();
    for wrong in [outside, unmarked.join("block.ext4")] {
        let asked = image(&wrong.to_string_lossy(), base);
        assert!(rig.machine.image_files(&asked).await.is_err(), "{}", wrong.display());
    }
    let few = driver::StagedImage { uid_count: 1, ..image(&path, base) };
    assert!(rig.machine.image_files(&few).await.is_err(), "a workload's whole range of ids");
    let other = driver::StagedImage { filesystem: "btrfs".into(), ..image(&path, base) };
    let refused = rig.machine.image_files(&other).await.unwrap_err().to_string();
    assert!(refused.contains("restored whole"), "a filesystem its guests do not mount: {refused}");
    assert_eq!(service_guests(&rig.machine), 0, "nothing booted for any of them");
}
