// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's files through the relay (§3.2 of docs/06-storage.md), wired as `korpis-agent run`
//! wires it, with the in-memory host serving the worker's verbs: the session asks for the
//! workload's volumes as its assignment declares them, its frames are carried both ways in order
//! with several requests sent ahead, what it changed is journaled at its end, and the control plane
//! records its opening and its end as the act of the session's subject under that subject's grant;
//! and where the workload's driver serves its files inside its object, as a microVM's does, the
//! session goes through the driver and the helper is never asked.

use crate::data::{data, end_code, frame, open, opened};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::data::files::{READ, WRITE};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::sync::Arc;
use std::time::Duration;

fn ask(id: u64, request: File) -> pb::RelayRequest {
    data(&pb::FileRequest { id, request: Some(request), ..Default::default() }.encode_to_vec())
}

async fn answer(session: &mut crate::data::Client) -> pb::FileResponse {
    match frame(session).await {
        Frame::Data(bytes) => pb::FileResponse::decode_from_slice(&bytes).unwrap(),
        other => panic!("an answer, got {other:?}"),
    }
}

/// A session of `action` on `w`, opened at the relay with a token `secret` mints.
async fn session(cp: &ControlPlane, secret: &str, w: &str, action: &str) -> crate::data::Client {
    let minted = cp.minted(secret, w, &[action], 0).await;
    let mut request = open(&minted.token, action, w, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
    }
    opened(&cp.relay(), &request).await
}

/// The session's effects under `grant`, by event, once both are recorded; and what was rejected.
async fn session_effects(cp: &ControlPlane, grant: &str) -> (Vec<(String, String)>, String) {
    let leaf = korpis_core::id::Id::parse(korpis_core::id::IdKind::Grant, grant).unwrap().uuid();
    let sql = "SELECT coalesce(string_agg(concat_ws(':', after->>'event', after->>'files', \
               after->>'bytes', after->>'removed_count'), ',' ORDER BY origin_seq), '') \
               FROM effect WHERE action = 'workload.file.write' \
               AND grant_id = $1::text::uuid AND actor::text LIKE '%token%'";
    let mut found = String::new();
    for _ in 0..300 {
        found = cp
            .query_one::<(String,)>(sql, &leaf.to_string())
            .await
            .map(|(s,)| s)
            .unwrap_or_default();
        if found.contains("ended") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rejected = cp
        .query_one::<(String,)>(
            "SELECT coalesce(string_agg(error::text, '; '), '') FROM effect \
             WHERE action = 'journal.rejected' AND $1 <> ''",
            "any",
        )
        .await
        .map(|(r,)| r)
        .unwrap_or_default();
    let events = found
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (event, rest) = s.split_once(':').unwrap_or((s, ""));
            (event.to_owned(), rest.to_owned())
        })
        .collect();
    (events, rejected)
}

#[tokio::test]
async fn a_file_session_reaches_the_workload_s_volumes_and_is_recorded_as_its_holder_s_act() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-12").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    host.serve_files_from(&dir.path().join("files"));
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime, host.clone(), Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let volume = pb::VolumeSpec { name: "data".into(), path: "/data".into(), ..Default::default() };
    let w = cp.declare_with(&cp.project().await, "mc", vec![volume]).await;

    let (grant, secret) = cp.grant_to_new_token("editor", &[WRITE]).await;
    let mut s = session(&cp, &secret, &w, WRITE).await;

    // Three requests sent ahead of their answers, answered in order.
    let write = |path: &str, data: &[u8]| {
        File::Write(Box::new(pb::FileWrite {
            path: path.into(),
            data: data.to_vec(),
            create: true,
            ..Default::default()
        }))
    };
    s.send(ask(1, write("/data/hello.txt", b"hello"))).await.unwrap();
    s.send(ask(2, write("/data/old.txt", b"x"))).await.unwrap();
    let read = pb::FileRead { path: "/data/hello.txt".into(), length: 64, ..Default::default() };
    s.send(ask(3, File::Read(Box::new(read)))).await.unwrap();
    assert_eq!(answer(&mut s).await.response, Some(Response::Written(5)));
    assert_eq!(answer(&mut s).await.response, Some(Response::Written(1)));
    // Answered, so the worker was asked for, as the assignment declares the volumes.
    let opened_with = host.opened_files();
    let asked = opened_with.last().expect("the helper is asked for a worker");
    assert!(asked.write && asked.volumes.iter().any(|v| v.path == "/data"), "{asked:?}");
    let third = answer(&mut s).await;
    assert_eq!((third.id, third.response), (3, Some(Response::Data(b"hello".to_vec()))));
    let remove = pb::FileRemove { path: "/data/old.txt".into(), ..Default::default() };
    s.send(ask(4, File::Remove(Box::new(remove)))).await.unwrap();
    assert_eq!(answer(&mut s).await.response, Some(Response::Done(true)));
    s.close_send();
    drop(s);

    let (events, rejected) = session_effects(&cp, &grant).await;
    let ended = events.iter().find(|(e, _)| e == "ended").map(|(_, rest)| rest.as_str());
    assert!(events.iter().any(|(e, _)| e == "opened"), "{events:?}: {rejected}");
    assert_eq!(ended, Some("2:6:1"), "two files, six bytes, one removed: {rejected}");

    // A session that only reads is refused a write, and a frame that is no request ends it.
    let (_, reader) = cp.grant_to_new_token("viewer", &[READ]).await;
    let mut r = session(&cp, &reader, &w, READ).await;
    r.send(ask(1, write("/data/hello.txt", b"no"))).await.unwrap();
    let refused = answer(&mut r).await;
    assert!(matches!(refused.response, Some(Response::Error(_))), "{refused:?}");
    r.send(data(b"not a FileRequest at all, not even close")).await.unwrap();
    assert_eq!(end_code(frame(&mut r).await), "invalid");
    agent.stop().await;
}

#[tokio::test]
async fn a_session_whose_workload_s_driver_serves_files_inside_it_goes_through_the_driver() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-13").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    host.serve_files_from(&dir.path().join("files"));
    // As a microVM's driver: the object's own worker serves its volume.
    let inside = dir.path().join("guest");
    std::fs::create_dir_all(inside.join("data")).unwrap();
    runtime.guest_files(Some(inside.clone()));
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime.clone(), host.clone(), Arc::new(facts), options);
    let agent = agent.unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let volume = pb::VolumeSpec { name: "data".into(), path: "/data".into(), ..Default::default() };
    let w = cp.declare_with(&cp.project().await, "guest", vec![volume]).await;
    let id = loop {
        let running = runtime.objects().into_iter().find(|(_, (o, run))| o == &w && *run);
        if let Some((id, _)) = running {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // The reconciler names the object once it has started it.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let (_, secret) = cp.grant_to_new_token("editor", &[WRITE]).await;
    let mut s = session(&cp, &secret, &w, WRITE).await;
    let write = pb::FileWrite {
        path: "/data/hello.txt".into(),
        data: b"merhaba".to_vec(),
        create: true,
        ..Default::default()
    };
    s.send(ask(1, File::Write(Box::new(write)))).await.unwrap();
    assert_eq!(answer(&mut s).await.response, Some(Response::Written(7)));
    s.close_send();
    drop(s);
    assert_eq!(std::fs::read(inside.join("data/hello.txt")).unwrap(), b"merhaba");
    assert!(runtime.calls().contains(&format!("files {id}")), "through the driver");
    assert!(host.opened_files().is_empty(), "the helper is never asked for a worker");
    agent.stop().await;
}
