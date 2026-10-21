// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload uses now, wired as `korpis-agent run` wires it (§2 of
//! docs/15-observability.md): a client that opens a `metering.read` session through the relay is
//! sent the meter's readings of the workload as they are taken, each rate over the window from
//! the reading before.

use crate::console_write::running;
use crate::data::{frame, open, opened};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::data::usage::ACTION;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use korpis_server::ServeConfig;
use std::sync::Arc;
use std::time::Duration;

async fn reading(client: &mut crate::data::Client) -> pb::UsageNow {
    match frame(client).await {
        Frame::Data(bytes) => pb::UsageNow::decode_from_slice(&bytes).unwrap(),
        other => panic!("a reading, got {other:?}"),
    }
}

#[tokio::test]
async fn a_metering_session_is_sent_each_reading_of_the_workload_with_its_rates() {
    // Two-second intervals, so the meter reads every two seconds rather than every ten.
    let two = Duration::from_secs(2);
    let cp = ControlPlane::start_as(true, |c| ServeConfig { metering_interval: two, ..c }).await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-14").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let w = cp.declare(&cp.project().await, "mc").await;
    let object = running(&runtime, &w).await;
    let counters = |cpu| driver::ObjectCounters {
        cpu_usec: Some(cpu),
        memory_bytes: Some(64 << 20),
        ..Default::default()
    };
    runtime.set_counters(&object, counters(0));

    // A token holding metering.read alone, as a billing system's would.
    let (_, secret) = cp.grant_to_new_token("billing", &[ACTION]).await;
    let minted = cp.minted(&secret, &w, &[ACTION], 0).await;
    let mut request = open(&minted.token, ACTION, &w, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
    }
    let mut client = opened(&cp.relay(), &request).await;
    let first = reading(&mut client).await;
    assert!(first.since.as_option().is_none() && first.cpu_millicores.is_none(), "{first:?}");
    // The reading the session began at may be one taken before the counters were set.
    let mut read = first;
    for _ in 0..5 {
        if read.memory_bytes.is_some() {
            break;
        }
        read = reading(&mut client).await;
    }
    assert_eq!(read.memory_bytes, Some(64 << 20), "memory in use at the reading");

    // CPU used from now is a rate over the window of the reading that first counts it.
    runtime.set_counters(&object, counters(4_000_000));
    let mut next = reading(&mut client).await;
    for _ in 0..5 {
        if next.cpu_millicores.is_some_and(|m| m > 0) {
            break;
        }
        next = reading(&mut client).await;
    }
    let nanos = |t: Option<&buffa_types::google::protobuf::Timestamp>| {
        t.map(|t| i128::from(t.seconds) * 1_000_000_000 + i128::from(t.nanos))
    };
    let (since, at) = (nanos(next.since.as_option()), nanos(next.at.as_option()));
    assert!(since.is_some_and(|s| at.is_some_and(|a| s < a)), "a window: {next:?}");
    assert!(next.cpu_millicores.is_some_and(|m| m > 0), "a rate over it: {next:?}");
    assert_eq!(next.net_rx_bytes_per_second, None, "no interface was read");
    agent.stop().await;
}
