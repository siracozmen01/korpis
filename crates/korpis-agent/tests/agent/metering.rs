// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's meter, wired as `korpis-agent run` wires it (§2 of docs/15-observability.md): at
//! the end of every metering interval the control plane declares, the node journals a sample for
//! each workload it holds, and the control plane records what each interval used as the
//! difference from the sample before, across an agent restart, and writes no effect for it.

use crate::console_write::running;
use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_server::ServeConfig;
use std::sync::Arc;
use std::time::Duration;

/// The CPU time recorded of `workload`, once it reaches `at_least` microseconds.
async fn recorded(cp: &ControlPlane, workload: &str, at_least: i64) -> i64 {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    let sql = "SELECT coalesce(sum(cpu_usec), 0)::bigint FROM metering_record \
               WHERE workload_id = $1::text::uuid AND kind = 'measured'";
    let mut seen = 0;
    for _ in 0..300 {
        seen = cp.query_one::<(i64,)>(sql, &id.uuid().to_string()).await.map_or(0, |(n,)| n);
        if seen >= at_least {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    seen
}

fn cpu(usec: u64) -> driver::ObjectCounters {
    driver::ObjectCounters { cpu_usec: Some(usec), ..Default::default() }
}

#[tokio::test]
async fn what_a_workload_used_is_recorded_interval_by_interval_across_an_agent_restart() {
    let two = Duration::from_secs(2);
    let cp = ControlPlane::start_as(true, |c| ServeConfig { metering_interval: two, ..c }).await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-13").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    // The agent's own interval is an hour: the control plane's two seconds are what it samples.
    let options = || Options { period: Duration::from_millis(200), ..Options::default() };
    let start = || Agent::start(&state, runtime.clone(), host.clone(), Arc::new(facts), options());
    let agent = start().unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let w = cp.declare(&cp.project().await, "mc").await;
    let object = running(&runtime, &w).await;
    runtime.set_counters(&object, cpu(5_000));
    assert_eq!(recorded(&cp, &w, 5_000).await, 5_000, "the first interval counts from zero");

    // Restarted, the agent takes up its accounts: what was counted is not counted again.
    agent.stop().await;
    let agent = start().unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    runtime.set_counters(&object, cpu(7_000));
    assert_eq!(recorded(&cp, &w, 7_000).await, 7_000, "each interval the difference, once");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(recorded(&cp, &w, 0).await, 7_000, "and nothing more while nothing ran");

    let effects = "SELECT count(*) FROM effect WHERE action LIKE 'journal.%' AND $1 <> ''";
    let journal = cp.query_one::<(i64,)>(effects, "x").await.map_or(-1, |(n,)| n);
    assert_eq!(journal, 0, "a sample believed is a record, not an effect");
    assert!(dir.path().join("agent").join("meter").exists(), "the meter keeps its accounts");
    agent.stop().await;
}
