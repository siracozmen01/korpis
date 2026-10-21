// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a console is recorded (§8 of docs/17-security.md, Finding 122 of
//! docs/23-walkthroughs.md), wired as `korpis-agent run` wires it: the node journals the read as
//! the act of the session's subject, under the grant its token was minted from, before it sends a
//! record; and the control plane records it as that subject's act because that grant permitted
//! it.

use crate::console_write::running;
use crate::data::{open, opened};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::data::console::ACTION;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Waits until the node watches `w`'s console, so a read finds it.
async fn watched(state: &Path, w: &str) {
    for _ in 0..200 {
        let kept = std::fs::read_to_string(state.join("streams")).unwrap_or_default();
        if kept.lines().any(|l| l.starts_with(&format!("{w} "))) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w}'s console was never watched");
}

/// How many reads are recorded under `grant` as its token's act, searching for `Saved`.
async fn reads(cp: &ControlPlane, grant: &str) -> i64 {
    let leaf = korpis_core::id::Id::parse(korpis_core::id::IdKind::Grant, grant).unwrap().uuid();
    let sql = "SELECT count(*) FROM effect WHERE action = 'workload.console.read' \
               AND grant_id = $1::text::uuid AND actor::text LIKE '%token%' \
               AND after->>'pattern' = 'Saved' AND after ? 'session'";
    for _ in 0..300 {
        let recorded = cp.query_one::<(i64,)>(sql, &leaf.to_string()).await.map_or(0, |(n,)| n);
        if recorded > 0 {
            return recorded;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    0
}

#[tokio::test]
async fn a_console_read_is_an_effect_of_whoever_read_it() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-12").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    runtime.capture_consoles(true);
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let w = cp.declare(&cp.project().await, "mc").await;
    running(&runtime, &w).await;
    watched(&dir.path().join("agent"), &w).await;

    let (grant, secret) = cp.grant_to_new_token("reader", &[ACTION]).await;
    let minted = cp.minted(&secret, &w, &[ACTION], 0).await;
    let mut request = open(&minted.token, ACTION, &w, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
        o.params =
            pb::ConsoleRead { pattern: "Saved".into(), ..Default::default() }.encode_to_vec();
    }
    let _session = opened(&cp.relay(), &request).await;
    assert_eq!(reads(&cp, &grant).await, 1, "the read is the reader's act, under its grant");
    agent.stop().await;
}
