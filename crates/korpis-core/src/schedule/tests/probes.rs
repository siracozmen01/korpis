// SPDX-License-Identifier: AGPL-3.0-or-later

//! A probe the node cannot run (§5 of docs/04-runtimes.md): an exec probe needs a driver that
//! runs a command inside the workload, and a heartbeat probe one that gives the workload its
//! identity socket; a node whose driver declares neither is refused for it, said so, rather than
//! placed with a probe nothing would ever run, or one nothing could ever answer.

use super::{Case, checks, node};
use crate::schedule::Check;
use crate::workload::spec::health::{Probe, ProbeKind};
use std::time::Duration;
use uuid::Uuid;

fn exec() -> Probe {
    let command = vec!["true".to_owned()];
    Probe {
        kind: ProbeKind::Exec { command },
        period: None,
        timeout: None,
        failures: 0,
        initial_delay: None,
    }
}

#[test]
fn an_exec_probe_needs_a_driver_that_runs_commands_in_the_workload() {
    let mut without = node(2, &[]);
    without.drivers[0].capabilities.exec = false;
    let mut case = Case::new(vec![node(1, &[]), without]);
    case.body.health.liveness = Some(exec());
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Probe]);
    assert!(e.filtered[0].detail.contains("exec probe"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(1)), "the node whose driver runs commands");

    case.body.health.liveness = None;
    case.body.health.readiness =
        Some(Probe { kind: ProbeKind::Tcp { endpoint: "game".into() }, ..exec() });
    assert!(checks(&case.run(), 2).is_empty(), "a probe from outside needs nothing of it");
}

#[test]
fn a_heartbeat_probe_needs_a_driver_that_gives_the_workload_its_identity_socket() {
    let mut without = node(2, &[]);
    without.drivers[0].capabilities.identity_dir = false;
    let mut case = Case::new(vec![node(1, &[]), without]);
    let max_interval = Duration::from_secs(30);
    case.body.health.readiness =
        Some(Probe { kind: ProbeKind::Heartbeat { max_interval }, ..exec() });
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Probe]);
    assert!(e.filtered[0].detail.contains("heartbeat probe"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(1)), "the node whose driver serves the socket");
}

#[test]
fn a_guest_agent_probe_needs_a_driver_that_reaches_the_guest_s_agent() {
    let mut with = node(1, &[]);
    with.drivers[0].capabilities.guest_agent = true;
    let mut case = Case::new(vec![with, node(2, &[])]);
    case.body.health.liveness = Some(Probe { kind: ProbeKind::GuestAgent, ..exec() });
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Probe]);
    assert!(e.filtered[0].detail.contains("guest_agent probe"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(1)));
}
