// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration's own consistency.

use crate::support::body;
use korpis_core::workload::spec::endpoints::{Protocol, TlsSpec};
use korpis_core::workload::spec::health::{Probe, ProbeKind};
use korpis_core::workload::spec::{ConsoleMode, Lifecycle};

fn refused(mutate: impl FnOnce(&mut korpis_core::workload::IntentBody)) -> String {
    let mut b = body();
    mutate(&mut b);
    b.validate().expect_err("refused").field
}

#[test]
fn the_example_is_valid_and_each_rule_names_its_field() {
    body().validate().unwrap();
    assert_eq!(refused(|b| b.recipe.digest = "sha256:x".into()), "recipe");
    assert_eq!(refused(|b| b.lifecycle = Lifecycle::Scheduled), "schedule");
    assert_eq!(refused(|b| b.schedule = Some("0 4 * * *".into())), "schedule");
    assert_eq!(refused(|b| b.runtime.driver.clear()), "runtime.driver");
    assert_eq!(refused(|b| b.console = ConsoleMode::Serial), "console");
    assert_eq!(refused(|b| b.resources.memory.reservation = 8 << 30), "resources.memory");
    assert_eq!(refused(|b| b.volumes[0].path = "/data/../etc".into()), "volumes[0].path");
    assert_eq!(refused(|b| b.volumes[0].path = "/.korpis/data".into()), "volumes[0].path");
    assert_eq!(refused(|b| b.volumes.push(b.volumes[0].clone())), "volumes[1].name");
    assert_eq!(refused(|b| b.endpoints[0].container_port = 0), "endpoints[0].container_port");
    assert_eq!(
        refused(|b| b.endpoints[0].domains = vec!["mc.example.org".into()]),
        "endpoints[0].domains"
    );
    assert_eq!(refused(|b| b.endpoints[0].tls = Some(TlsSpec::Acme)), "endpoints[0].tls");
    let probe = |endpoint: &str| Probe {
        kind: ProbeKind::Tcp { endpoint: endpoint.into() },
        period: None,
        timeout: None,
        failures: 0,
        initial_delay: None,
    };
    assert_eq!(refused(|b| b.health.readiness = Some(probe("rcon"))), "health.readiness");
    let mut ok = body();
    ok.health.liveness = Some(probe("game"));
    ok.endpoints[0].protocol = Protocol::Https;
    ok.endpoints[0].domains = vec!["mc.example.org".into(), "*.example.org".into()];
    ok.endpoints[0].tls = Some(TlsSpec::Provided { secret: "cert".into() });
    ok.lifecycle = Lifecycle::Scheduled;
    ok.schedule = Some("0 4 * * *".into());
    ok.validate().unwrap();
}
