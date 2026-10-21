// SPDX-License-Identifier: AGPL-3.0-or-later

//! Typing into a console through the relay (§6 of docs/14-streams.md), wired as `korpis-agent
//! run` wires it: the node journals each input as the act of the session's subject, under the
//! grant its token was minted from, before the driver is given it; and the control plane
//! records it as that subject's act because that grant permitted it.

use crate::data::{data, end_code, frame, open, opened};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::data::console_write::ACTION;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::console_input::Input;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use std::sync::Arc;
use std::time::Duration;

fn input(i: Input) -> pb::RelayRequest {
    data(&pb::ConsoleInput { input: Some(i), ..Default::default() }.encode_to_vec())
}

/// The runtime object `w` runs as, once it runs.
pub async fn running(runtime: &InMemory, w: &str) -> String {
    for _ in 0..200 {
        let found = runtime.objects().into_iter().find(|(_, (on, up))| on == w && *up);
        if let Some((id, _)) = found {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} never ran");
}

/// How many inputs are recorded under `grant` as its token's act, once one is; and what was
/// rejected, to say why when none is.
async fn typed_effects(cp: &ControlPlane, grant: &str) -> (i64, String) {
    let leaf = korpis_core::id::Id::parse(korpis_core::id::IdKind::Grant, grant).unwrap().uuid();
    let sql = "SELECT count(*) FROM effect WHERE action = 'workload.console.write' \
               AND grant_id = $1::text::uuid AND actor::text LIKE '%token%' \
               AND (after->>'bytes')::int = 5";
    let mut recorded = 0;
    for _ in 0..300 {
        recorded = cp.query_one::<(i64,)>(sql, &leaf.to_string()).await.map_or(0, |(n,)| n);
        if recorded > 0 {
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
    (recorded, rejected)
}

#[tokio::test]
async fn input_reaches_the_workload_as_an_effect_of_whoever_typed_it() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-11").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let w = cp.declare(&cp.project().await, "mc").await;
    let id = running(&runtime, &w).await;

    let (grant, secret) = cp.grant_to_new_token("typist", &[ACTION]).await;
    let minted = cp.minted(&secret, &w, &[ACTION], 0).await;
    let mut request = open(&minted.token, ACTION, &w, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
    }
    let mut session = opened(&cp.relay(), &request).await;
    session.send(input(Input::Data(b"stop\n".to_vec()))).await.unwrap();
    let size = pb::TerminalSize { cols: 120, rows: 40, ..Default::default() };
    session.send(input(Input::Resize(Box::new(size)))).await.unwrap();
    let typed =
        |c: &String| c.starts_with(&format!("console_input {id} token:")) && c.ends_with(" stop\n");
    for _ in 0..100 {
        if runtime.calls().iter().any(typed) && runtime.calls().iter().any(|c| c.contains("120x40"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(runtime.calls().iter().any(typed), "{:?}", runtime.calls());
    assert!(
        runtime.calls().iter().any(|c| c.starts_with("resize_console") && c.ends_with("120x40"))
    );

    let (recorded, rejected) = typed_effects(&cp, &grant).await;
    assert_eq!(recorded, 1, "the input is the typist's act, under the typist's grant: {rejected}");

    session.send(data(b"not a ConsoleInput")).await.unwrap();
    assert_eq!(end_code(frame(&mut session).await), "invalid");
    agent.stop().await;
}
