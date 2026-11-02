// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload moved between two real nodes, each with its data plane in a network namespace of the
//! test's own, joined as two machines by a network, its volumes in a directory class that crosses
//! by its tree. A sleeper holding a dedicated address of a cluster pool, routed by the test
//! extension's router, runs on the first with a volume holding a symbolic link, an extended
//! attribute, a sparse file and two names of one file. Moved to the second through every phase on
//! the nodes' own reports, it runs there over a volume holding each of them as it was, its address
//! routed and translated there and no longer on the first, its console's next record at the offset
//! after the first's last, and its meter's records of the two nodes neither overlapping nor
//! leaving its stay on either uncounted. Moved back and rolled back after the cutover, it runs on
//! the second again with what it held. Needs root, containerd, rsync and the kernel's WireGuard;
//! skipped without them.

use crate::extension::{install, pinged_as};
use crate::image::{TEST_EXTENSION, push_image};
use crate::mesh::{Netns, machines, node_in, until_available};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push, push_sleeper};
use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::net::Ipv4Addr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The test extension as an address router named `test`.
fn router(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: test-router, version: "{VERSION}", description: tests, license: Apache-2.0}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}, --router]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu: {{min: 50m, recommended: 100m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.address_router]
  provides:
    - {{type: address_router, name: test}}
"#,
        entry = TEST_EXTENSION.entry(),
    )
}

/// A cluster pool of the root organization routed by the router named `test`.
async fn pool(cp: &ControlPlane) {
    let pool = pb::AddressPool {
        organization: cp.root_org.clone(),
        cidr: "198.18.9.0/29".into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_CLUSTER.into(),
        router: "test".into(),
        routing: pb::AddressRouting::ADDRESS_ROUTING_BGP.into(),
        reserved: vec!["198.18.9.1".into()],
        ..Default::default()
    };
    let r = pb::CreateAddressPoolRequest {
        mutation: mutation("pool").into(),
        pool: pool.into(),
        ..Default::default()
    };
    let client = pb::AddressPoolServiceClient::new(cp.http(), cp.config());
    client.create_address_pool(r).await.unwrap();
}

/// Whether the node in `ns` translates connections to `address`.
fn translates(ns: &Netns, address: &str) -> bool {
    let out = std::process::Command::new("ip")
        .args(["netns", "exec", ns.name(), "nft", "list", "ruleset"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(address)
}

/// The directory of `node`'s volume of `w` named `name`, once it has one.
async fn volume_of(node: &Node, w: &str, name: &str) -> PathBuf {
    let suffix = format!(".{name}");
    for _ in 0..600 {
        let found = std::fs::read_dir(&node.volumes).into_iter().flatten().flatten();
        let mut found = found.map(|e| e.path()).filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            n.starts_with(w) && n.ends_with(&suffix)
        });
        if let Some(dir) = found.next() {
            return dir;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} never held {w}'s {name}", node.volumes.display());
}

/// Gives the volume at `dir` a link, an attribute, a sparse file and a file of two names.
fn written(dir: &Path) {
    std::fs::write(dir.join("target"), "pointed at").unwrap();
    std::os::unix::fs::symlink("target", dir.join("link")).unwrap();
    std::fs::write(dir.join("attributed"), "x").unwrap();
    xattr_set(&dir.join("attributed"), "user.korpis", b"kept");
    let sparse = std::fs::File::create(dir.join("sparse")).unwrap();
    sparse.set_len(256 << 20).unwrap();
    std::os::unix::fs::FileExt::write_at(&sparse, b"middle", 128 << 20).unwrap();
    std::fs::write(dir.join("one"), "both").unwrap();
    std::fs::hard_link(dir.join("one"), dir.join("two")).unwrap();
}

fn xattr_set(path: &Path, name: &str, value: &[u8]) {
    rustix::fs::setxattr(path, name, value, rustix::fs::XattrFlags::empty()).unwrap();
}

/// That the volume at `dir` holds what [`written`] gave it, as it was.
fn holds(dir: &Path) {
    let link = std::fs::read_link(dir.join("link")).expect("the link, as a link");
    assert_eq!(link, Path::new("target"));
    assert_eq!(std::fs::read_to_string(dir.join("link")).unwrap(), "pointed at");
    let mut value = [0u8; 16];
    let n = rustix::fs::getxattr(dir.join("attributed"), "user.korpis", &mut value).unwrap();
    assert_eq!(&value[..n], b"kept", "the extended attribute");
    let sparse = std::fs::metadata(dir.join("sparse")).unwrap();
    assert_eq!(sparse.len(), 256 << 20);
    assert!(sparse.blocks() * 512 < 1 << 20, "still sparse: {} blocks", sparse.blocks());
    let (one, two) =
        (std::fs::metadata(dir.join("one")).unwrap(), std::fs::metadata(dir.join("two")).unwrap());
    assert_eq!((one.ino(), one.nlink()), (two.ino(), 2), "two names of one file");
}

/// The offsets of the records in `node`'s console stream of `w`.
fn console(node: &Node, w: &str) -> Vec<u64> {
    let dir = node.volumes.join(format!("{w}.stream_console"));
    read(&StreamDir::new(&dir), 0, 10_000).map(|r| r.records.iter().map(|r| r.offset).collect())
        .unwrap_or_default()
}

/// Moves `w` to `node`, keeping the source's copies for `hold`; the migration, once approved.
async fn move_to(cp: &ControlPlane, w: &str, node: &str, hold: Duration) -> String {
    let client = cp.workloads();
    let request = pb::MigrateWorkloadRequest {
        mutation: mutation(&format!("move-{node}")).into(),
        workload: w.to_owned(),
        node: node.to_owned(),
        hold: korpis_wire::duration(hold).into(),
        ..Default::default()
    };
    let proposed = client.migrate_workload(request).await.unwrap().into_owned();
    cp.approve(&proposed.plan.into_option().unwrap().id).await;
    newest(cp, w).await.id
}

/// The newest migration of `w`.
async fn newest(cp: &ControlPlane, w: &str) -> pb::Migration {
    let list = pb::ListMigrationsRequest { workload: w.to_owned(), ..Default::default() };
    let got = cp.workloads().list_migrations(list).await.unwrap().into_owned();
    got.migrations.into_iter().next().unwrap()
}

/// `w`'s dedicated address, once its router routes it.
async fn routed(cp: &ControlPlane, w: &str) -> String {
    for _ in 0..600 {
        let d = cp.status(w).await.dedicated.into_option().unwrap_or_default();
        if !d.waiting_on_router && !d.address.is_empty() {
            return d.address;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w}'s address was never routed");
}

/// Waits until the newest migration of `w` completed: its source deleted what it kept.
async fn ended(cp: &ControlPlane, w: &str) {
    for _ in 0..900 {
        let outcome = newest(cp, w).await.outcome.as_known();
        if outcome == Some(pb::MigrationOutcome::MIGRATION_OUTCOME_COMPLETED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w}'s migration never completed: {:?}", newest(cp, w).await);
}

/// Waits until `w` runs on `node`, placed there.
async fn runs_on(cp: &ControlPlane, w: &str, node: &str) {
    for _ in 0..900 {
        let s = cp.status(w).await;
        let placed = s.placement.as_option().is_some_and(|p| p.node == node);
        let state = s.observation.as_option().and_then(|o| o.state.as_known());
        if placed && state == Some(pb::ObservedState::OBSERVED_STATE_RUNNING) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} never ran on {node}: {:?}", cp.status(w).await);
}

/// The usage records of `w` from each node, once `node` has one.
async fn metered(cp: &ControlPlane, w: &str, node: &str) -> Vec<pb::UsageRecord> {
    let client = pb::MeteringServiceClient::new(cp.http(), cp.config());
    for _ in 0..900 {
        let ask = pb::ListUsageRequest { workload: Some(w.to_owned()), ..Default::default() };
        let got = client.list_usage(ask).await.unwrap().into_owned().records;
        if got.iter().any(|r| r.node.as_deref() == Some(node)) {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{node} never metered {w}");
}

#[tokio::test]
async fn a_workload_moves_with_its_data_address_console_and_meter_and_a_rollback_returns_it() {
    if !live() || std::fs::metadata("/usr/bin/rsync").is_err() {
        return;
    }
    let Some((ma, mb)) = machines() else {
        eprintln!("this kernel makes no WireGuard link: not run");
        return;
    };
    let cp = ControlPlane::start_metering(Duration::from_secs(1), Duration::from_secs(60)).await;
    let project = cp.project().await;
    let a = node_in(&cp, "move-a", &ma, Ipv4Addr::new(198, 18, 1, 1)).await;
    let agent_a = a.agent().await;
    let b = node_in(&cp, "move-b", &mb, Ipv4Addr::new(198, 18, 1, 2)).await;
    let agent_b = b.agent().await;
    let named = |n: &Node| n.state.load().unwrap().node;
    let (na, nb) = (named(&a), named(&b));
    until_available(&cp, &na).await;
    until_available(&cp, &nb).await;
    let image = push_image(&crate::recipes::test_registry(), TEST_EXTENSION).await;
    let routing = install(&cp, &push("test-router", router(&image)).await, &project).await;
    a.guard(&routing.workload);
    b.guard(&routing.workload);
    pinged_as(&cp, pb::ProviderType::PROVIDER_TYPE_ADDRESS_ROUTER, "test").await;
    pool(&cp).await;

    let game = pb::EndpointSpec {
        name: "query".into(),
        exposure: pb::Exposure::EXPOSURE_DEDICATED_IP.into(),
        port: 9000,
        ..Default::default()
    };
    let body = pb::IntentBody { endpoints: vec![game], ..Default::default() };
    let w = cp.declare_body(&project, "db", &push_sleeper().await, body).await;
    a.guard(&w);
    b.guard(&w);
    let on = cp.status(&w).await.placement.into_option().unwrap().node;
    let (from, to, nfrom, nto) =
        if on == na { (&a, &b, &ma, &mb) } else { (&b, &a, &mb, &ma) };
    runs_on(&cp, &w, &on).await;
    written(&volume_of(from, &w, "data").await);
    let address = routed(&cp, &w).await;
    assert!(translates(nfrom, &address), "{address} is translated where it runs");
    let before = console(from, &w);
    metered(&cp, &w, &on).await;

    let there = named(to);
    move_to(&cp, &w, &there, Duration::from_secs(5)).await;
    runs_on(&cp, &w, &there).await;
    holds(&volume_of(to, &w, "data").await);
    assert!(translates(nto, &address), "routed and translated where it moved");
    assert!(!translates(nfrom, &address), "and no longer where it was");
    let after = console(to, &w);
    let last = before.iter().max().copied().unwrap_or_default();
    assert!(after.first().is_some_and(|o| *o > last), "continued after {last}: {after:?}");
    let records = metered(&cp, &w, &there).await;
    let mut spans: Vec<(i64, i64, String)> = records
        .iter()
        .map(|r| {
            let (f, t) = (r.from.as_option(), r.to.as_option());
            let (f, t) = (f.map_or(0, |t| t.seconds), t.map_or(0, |t| t.seconds));
            (f, t, r.node.clone().unwrap_or_default())
        })
        .collect();
    spans.sort();
    for pair in spans.windows(2) {
        assert!(pair[0].1 <= pair[1].0, "no interval counted twice: {spans:?}");
    }

    // Moved back once the first move completed, and rolled back once the move was made.
    ended(&cp, &w).await;
    let back = move_to(&cp, &w, &on, Duration::from_secs(3600)).await;
    runs_on(&cp, &w, &on).await;
    let rollback = pb::RollbackMigrationRequest {
        mutation: mutation("back").into(),
        migration: back,
        reason: "returned".into(),
        ..Default::default()
    };
    cp.workloads().rollback_migration(rollback).await.unwrap();
    runs_on(&cp, &w, &there).await;
    holds(&volume_of(to, &w, "data").await);
    cp.retire(&w).await;
    agent_a.stop().await;
    agent_b.stop().await;
}
