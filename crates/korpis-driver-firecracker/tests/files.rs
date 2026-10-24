// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session in a live guest (§4.4 of docs/04-runtimes.md): the driver opens it on the
//! guest's files port, the guest runs the file worker its boot drive carries, and the session
//! lists the workload's volumes and nothing else, writes a file the workload then reads, reads it
//! back, and is refused a write to a volume the workload mounts read-only, beyond its volumes, or
//! in a session opened to read. Needs what the driver's runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use buffa::Message as _;
use korpis_driver_firecracker::machine::Machine;
use korpis_files::frame;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::os::unix::net::UnixStream;
use std::time::Duration;

mod support;

use support::{Rig, live, ran, until};

/// A session on `id`'s volumes, writing if `write`, as a blocking stream.
async fn session(machine: &Machine, id: &str, write: bool) -> UnixStream {
    let stream = machine.files(id, write).await.unwrap().into_std().unwrap();
    stream.set_nonblocking(false).unwrap();
    stream
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

    let mut s = session(&rig.machine, &id, true).await;
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

    let mut s = session(&rig.machine, &id, false).await;
    assert!(refused(&ask(&mut s, 1, write("/data/other", b"x"))), "a session opened to read");
    assert_eq!(ask(&mut s, 2, read("/data/hello.txt")), Response::Data(b"merhaba".to_vec()));
    drop(s);
    rig.machine.destroy(&id).await.unwrap();
}
