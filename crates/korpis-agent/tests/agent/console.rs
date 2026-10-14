// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a console through the relay (§2 and §8 of docs/14-streams.md): the node serves
//! `workload.console.read` from the stream files it holds, from an offset, searched, limited,
//! and followed as records are written.

use crate::data::{Client, end_code, frame, node_with, open, opened};
use crate::support::ControlPlane;
use buffa::Message as _;
use korpis_agent::data::console::{ACTION, ConsoleRead};
use korpis_agent::data::handler::Handlers;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use korpis_stream::dir::StreamDir;
use korpis_stream::redact::Redactor;
use korpis_stream::writer::{Limits, StreamWriter};
use korpis_stream::{Kind, Source};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const T0: i64 = 1_760_000_000_000_000_000;

fn writer(dir: &Path) -> StreamWriter {
    StreamWriter::open(StreamDir::new(dir), Limits::default(), Redactor::default(), T0).unwrap()
}

/// The next chunk the client is sent.
async fn chunk(client: &mut Client) -> pb::ConsoleChunk {
    match frame(client).await {
        Frame::Data(bytes) => pb::ConsoleChunk::decode_from_slice(&bytes).unwrap(),
        other => panic!("a chunk, got {other:?}"),
    }
}

fn bodies(c: &pb::ConsoleChunk) -> Vec<String> {
    c.records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect()
}

fn asking(read: &pb::ConsoleRead) -> Vec<u8> {
    read.encode_to_vec()
}

#[tokio::test]
async fn a_console_is_read_searched_limited_and_followed_through_the_relay() {
    let cp = ControlPlane::start().await;
    let streams = tempfile::tempdir().unwrap();
    let (located, watching) = tokio::sync::watch::channel(BTreeMap::new());
    let handlers = Handlers::default().with(ACTION, Arc::new(ConsoleRead::new(watching)));
    let n = node_with(&cp, "fra-9", handlers).await;
    let dir = streams.path().join("console");
    let mut w = writer(&dir);
    for (i, line) in ["Starting server", "Player Alex joined", "Saved the game"].iter().enumerate()
    {
        let at = T0 + 1_000_000_000 * i64::try_from(i).unwrap();
        w.write(Source::Stdout, Kind::Text, line.as_bytes(), at).unwrap();
    }
    w.tick(T0 + 3_000_000_000).unwrap();
    located.send_replace(BTreeMap::from([(n.workload.clone(), dir.clone())]));
    let token = cp.mint(&n.workload, &[ACTION]).await;
    let session = |read: pb::ConsoleRead| {
        let mut request = open(&token, ACTION, &n.workload, &n.id);
        if let Some(pb::__buffa::oneof::relay_request::Message::Open(o)) = request.message.as_mut()
        {
            o.params = asking(&read);
        }
        request
    };

    let mut all = opened(&cp.relay(), &session(pb::ConsoleRead::default())).await;
    let first = chunk(&mut all).await;
    assert_eq!(bodies(&first), ["Starting server", "Player Alex joined", "Saved the game"]);
    assert!(first.caught_up && first.next_offset == 3, "{first:?}");
    assert_eq!(first.records[1].source, "stdout");
    assert_eq!(end_code(frame(&mut all).await), "", "a read that does not follow ends");

    let alex = pb::ConsoleRead { pattern: r"Alex \w+".into(), regex: true, ..Default::default() };
    let mut found = opened(&cp.relay(), &session(alex)).await;
    assert_eq!(bodies(&chunk(&mut found).await), ["Player Alex joined"], "searched on the node");

    let one = pb::ConsoleRead { from_offset: 1, limit: 1, ..Default::default() };
    let mut limited = opened(&cp.relay(), &session(one)).await;
    let c = chunk(&mut limited).await;
    assert_eq!((bodies(&c), c.next_offset), (vec!["Player Alex joined".to_owned()], 2));

    let tail = pb::ConsoleRead { last: 2, ..Default::default() };
    let mut scrollback = opened(&cp.relay(), &session(tail)).await;
    let c = chunk(&mut scrollback).await;
    assert_eq!(bodies(&c), ["Player Alex joined", "Saved the game"], "the last two held");

    let follow = pb::ConsoleRead { from_offset: 3, follow: true, ..Default::default() };
    let mut live = opened(&cp.relay(), &session(follow)).await;
    let caught = chunk(&mut live).await;
    assert!(caught.caught_up && caught.records.is_empty(), "{caught:?}");
    w.write(Source::Stderr, Kind::Text, b"Player Alex left", T0 + 4_000_000_000).unwrap();
    w.tick(T0 + 4_000_000_000).unwrap();
    let later = chunk(&mut live).await;
    assert_eq!(bodies(&later), ["Player Alex left"], "written since, sent as it is");
    assert!(!later.caught_up && later.next_offset == 4);

    let bad = pb::ConsoleRead { pattern: "(unclosed".into(), regex: true, ..Default::default() };
    let mut refused = opened(&cp.relay(), &session(bad)).await;
    assert_eq!(end_code(frame(&mut refused).await), "invalid");
}
