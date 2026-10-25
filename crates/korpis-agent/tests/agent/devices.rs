// SPDX-License-Identifier: AGPL-3.0-or-later

//! The devices a node's spec files describe reach the node's record (§6.1 of
//! docs/05-scheduling.md): the agent reports what it read in its Hello, and what it reads again
//! in its next heartbeat, a device offered with its class and a device it does not offer with
//! the reason.

use crate::session::until;
use crate::support::{self, ControlPlane};
use korpis_agent::devices::{Config, read};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::{Event, Facts};
use korpis_agent::state::StateDir;
use korpis_core::node::driver::DeviceClass;
use korpis_proto::korpis::v1 as pb;
use korpis_server::ServeConfig;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// A spec file of kind `kind` naming one device, `name`, which gives `/dev/zero`.
fn spec(dir: &Path, kind: &str, name: &str) {
    let text = format!(
        r#"{{"cdiVersion":"1.0.0","kind":"{kind}","devices":[{{"name":"{name}",
            "containerEdits":{{"deviceNodes":[{{"path":"/dev/zero"}}]}}}}]}}"#
    );
    std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
}

/// The node named `name` once `pick` holds of it.
async fn node(cp: &ControlPlane, name: &str, pick: impl Fn(&pb::Node) -> bool) -> pb::Node {
    let listed = pb::ListNodesRequest { organization: cp.root_org.clone(), ..Default::default() };
    for _ in 0..300 {
        let nodes = cp.nodes().list_nodes(listed.clone()).await.unwrap().into_owned().nodes;
        if let Some(n) = nodes.into_iter().find(|n| n.name == name && pick(n)) {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the node {name} never reported what was awaited");
}

#[tokio::test]
async fn the_devices_a_nodes_files_describe_reach_its_record_and_so_does_a_change() {
    // A heartbeat every second, so the change is reported within the test's patience.
    let beat = Duration::from_secs(1);
    let cp = ControlPlane::start_as(true, |c| ServeConfig { heartbeat_interval: beat, ..c }).await;
    let dir = tempfile::tempdir().unwrap();
    let specs = dir.path().join("cdi");
    std::fs::create_dir(&specs).unwrap();
    spec(&specs, "korpis.test/standin", "a");
    let config = Config {
        dirs: vec![specs.clone()],
        classes: [("korpis.test/standin".to_owned(), DeviceClass::Serial)].into(),
        owner: std::fs::metadata(&specs).unwrap().uid(),
        ..Config::default()
    };
    let (token, ca) = cp.token("devices-1").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (described, devices) = tokio::sync::watch::channel(Arc::new(read(&config)));
    // As `facts_of` does: the devices as they are read when the facts are asked.
    let facts = move || {
        let held = Arc::clone(&devices.borrow());
        Facts {
            devices: held.devices.clone(),
            unoffered_devices: held.unoffered.clone(),
            ..support::facts()
        }
    };
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime, host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| {
        eprintln!("EVENT {e:?}");
        matches!(e, Event::Welcomed { .. })
    })
    .await;

    let reported = node(&cp, "devices-1", |n| !n.devices.is_empty()).await;
    let device = &reported.devices[0];
    assert_eq!(device.id, "korpis.test/standin=a");
    assert_eq!(device.class, pb::DeviceClass::DEVICE_CLASS_SERIAL);
    assert_eq!(device.host_nodes, ["/dev/zero"]);

    // A kind the operator has not mapped arrives as the files are read again.
    spec(&specs, "korpis.test/other", "b");
    described.send(Arc::new(read(&config))).unwrap();
    let changed = node(&cp, "devices-1", |n| !n.unoffered_devices.is_empty()).await;
    let unoffered = &changed.unoffered_devices[0];
    assert_eq!(unoffered.id, "korpis.test/other=b");
    assert!(unoffered.reason.contains("unclassified"), "{unoffered:?}");
    assert_eq!(changed.devices.len(), 1, "the mapped device is still offered");
    agent.stop().await;
}
