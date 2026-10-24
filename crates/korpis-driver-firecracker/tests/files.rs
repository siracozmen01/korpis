// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session in a live guest (§4.4 of docs/04-runtimes.md): the driver opens it on the
//! guest's files port, the guest runs the file worker its boot drive carries, and the session
//! lists the workload's volumes and nothing else, writes a file the workload then reads, reads it
//! back, and is refused a write to a volume the workload mounts read-only, beyond its volumes, or
//! in a session opened to read. A stopped guest's volumes are served by a service guest, shared
//! by its sessions, ended with the last, and ended by the object's start. Needs what the driver's
//! runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use buffa::Message as _;
use korpis_driver_firecracker::machine::{Lease, Machine};
use korpis_files::frame;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::os::unix::net::UnixStream;
use std::time::Duration;

mod support;

use support::{Rig, live, no_service_guest, ran, service_guests, until};

/// A session on `id`'s volumes, writing if `write`, as a blocking stream, and the hold on the
/// guest serving it if `id` does not run.
async fn session(machine: &Machine, id: &str, write: bool) -> (UnixStream, Option<Lease>) {
    let session = machine.files(id, write).await.unwrap();
    let stream = session.stream.into_std().unwrap();
    stream.set_nonblocking(false).unwrap();
    (stream, session.lease)
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

fn write(path: &str, data: &[u8]) -> Request {
    let w = pb::FileWrite {
        path: path.into(),
        data: data.to_vec(),
        create: true,
        truncate: true,
        ..Default::default()
    };
    Request::Write(Box::new(w))
}

fn read(path: &str) -> Request {
    Request::Read(Box::new(pb::FileRead { path: path.into(), length: 1024, ..Default::default() }))
}

fn refused(r: &Response) -> bool {
    matches!(r, Response::Error(_))
}

#[tokio::test]
async fn a_session_in_a_guest_reads_and_writes_its_volumes_and_nothing_else() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let mut spec = rig.spec("wl_files", "while true; do sleep 1; done");
    let ro = rig.volume("wl_files_ro", spec.uid_base);
    spec.mounts.push(driver::Mount {
        source: ro.to_string_lossy().into_owned(),
        destination: "/ro".into(),
        volume: "ro".into(),
        read_only: true,
        ..Default::default()
    });
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;

    let (mut s, lease) = session(&rig.machine, &id, true).await;
    assert!(lease.is_none(), "its own guest serves it");
    let list = pb::FileList { path: "/".into(), limit: 100, ..Default::default() };
    let Response::Entries(top) = ask(&mut s, 1, Request::List(Box::new(list))) else {
        panic!("the top is listed");
    };
    let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["data", "ro"], "its volumes, and nothing of the guest's image");
    assert_eq!(ask(&mut s, 2, write("/data/hello.txt", b"merhaba")), Response::Written(7));
    assert_eq!(ask(&mut s, 3, read("/data/hello.txt")), Response::Data(b"merhaba".to_vec()));
    assert!(refused(&ask(&mut s, 4, write("/ro/x", b"x"))), "a read-only volume");
    assert!(refused(&ask(&mut s, 5, write("/etc/passwd", b"x"))), "beyond its volumes");
    drop(s);
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/hello.txt"]).await;
    assert_eq!(said, "merhaba", "the workload reads what the session wrote");

    let (mut s, _) = session(&rig.machine, &id, false).await;
    assert!(refused(&ask(&mut s, 1, write("/data/other", b"x"))), "a session opened to read");
    assert_eq!(ask(&mut s, 2, read("/data/hello.txt")), Response::Data(b"merhaba".to_vec()));
    drop(s);
    rig.machine.destroy(&id).await.unwrap();
}

#[tokio::test]
async fn a_stopped_guest_s_volumes_are_served_until_the_last_session_ends_or_it_starts() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let id = rig.machine.create(rig.spec("wl_kept", "while true; do sleep 1; done")).await.unwrap();
    let (mut s, lease) = session(&rig.machine, &id, true).await;
    assert!(lease.is_some(), "a guest of its own serves a stopped object's files");
    let list = pb::FileList { path: "/".into(), limit: 100, ..Default::default() };
    let Response::Entries(top) = ask(&mut s, 1, Request::List(Box::new(list))) else {
        panic!("the top is listed");
    };
    let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["data"], "its volume and nothing else");
    assert_eq!(ask(&mut s, 2, write("/data/kept.txt", b"kept")), Response::Written(4));
    let (mut other, held) = session(&rig.machine, &id, false).await;
    assert_eq!(ask(&mut other, 1, read("/data/kept.txt")), Response::Data(b"kept".to_vec()));
    drop((s, lease, other, held));
    no_service_guest(&rig.machine).await;

    let (mut s, lease) = session(&rig.machine, &id, false).await;
    assert_eq!(ask(&mut s, 1, read("/data/kept.txt")), Response::Data(b"kept".to_vec()));
    rig.machine.start(&id).await.unwrap();
    assert!(!service_guests(&rig.machine), "the start ended it");
    let asked =
        pb::FileRequest { id: 2, request: Some(read("/data/kept.txt")), ..Default::default() };
    let _ = frame::write(&mut s, &asked.encode_to_vec());
    assert!(!matches!(frame::read(&mut s), Ok(Some(_))), "its session ended with it");
    drop((s, lease));
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/kept.txt"]).await;
    assert_eq!(said, "kept", "what the session wrote was synced to the volume");
    rig.machine.destroy(&id).await.unwrap();
}
