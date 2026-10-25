// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes: stored and read back whole, and the operator's decision moved by compare-and-swap
//! (§3.4 of docs/01-model.md).

use crate::support::{fixture, now};
use korpis_app::Page;
use korpis_app::ports::HierarchyRead;
use korpis_app::ports::{NodeRead, NodeWrite, Store, Tx};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::{NodeAdmin, Taint};
use korpis_store::testing::{enroll, ready_node};

#[tokio::test]
async fn a_node_reads_back_as_enrolled() {
    let f = fixture().await;
    let mut n = ready_node(f.organization, "fra-3", &[("region", "eu")]).unwrap();
    n.taints = vec![Taint { key: "gpu".into(), value: "only".into() }];
    (n.last_seen, n.enrolled_at, n.probed_at) = (Some(now()), now(), Some(now()));
    enroll(&f.store, &n).await.unwrap();
    assert_eq!(f.store.node(n.id).await.unwrap(), Some(n.clone()));
    assert_eq!(f.store.nodes(f.organization, Page::default()).await.unwrap(), [n]);
}

#[tokio::test]
async fn a_node_sits_below_the_organization_it_was_enrolled_into() {
    let f = fixture().await;
    let n = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &n).await.unwrap();
    let chain = f.store.ancestry(ObjectRef::new(Kind::Node, n.id)).await.unwrap();
    let kinds: Vec<_> = chain.iter().map(|link| (link.object.kind, link.object.id)).collect();
    assert_eq!(kinds, [(Kind::Node, n.id), (Kind::Organization, f.organization)]);
}

#[tokio::test]
async fn the_admin_state_moves_by_compare_and_swap() {
    let f = fixture().await;
    let mut n = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &n).await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let locked = tx.lock_node(n.id).await.unwrap().unwrap();
    n.admin = NodeAdmin::Cordoned;
    assert!(tx.update_node_admin(&n, locked.version).await.unwrap());
    assert!(!tx.update_node_admin(&n, locked.version).await.unwrap(), "the version moved");
    tx.commit().await.unwrap();
    let read = f.store.node(n.id).await.unwrap().unwrap();
    assert_eq!((read.admin, read.version), (NodeAdmin::Cordoned, 2));
}

#[tokio::test]
async fn a_change_of_what_a_node_advertises_makes_its_reachability_unmeasured() {
    use korpis_app::ports::{NodeFacts, SessionWrite as _};
    use korpis_core::node::Reachability;
    use korpis_core::node::advertised::{Advertised, PortRange};
    let f = fixture().await;
    let n = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &n).await.unwrap();
    let advertising = |address: &str| NodeFacts {
        capacity: n.capacity.clone(),
        allocatable: n.allocatable.clone(),
        drivers: n.drivers.clone(),
        devices: Vec::new(),
        unoffered_devices: Vec::new(),
        storage_classes: None,
        agent: None,
        advertised: (!address.is_empty()).then(|| Advertised {
            addresses: vec![address.parse().unwrap()],
            ports: PortRange::DEFAULT,
            ingress: false,
        }),
    };
    let report = |facts: NodeFacts| {
        let store = f.store.clone();
        async move {
            let mut tx = store.begin().await.unwrap();
            assert!(tx.record_node_facts(n.id, &facts, now()).await.unwrap());
            tx.commit().await.unwrap();
            let read = store.node(n.id).await.unwrap().unwrap();
            (read.reachability, read.probed_at.is_some(), read.advertised)
        }
    };
    let (reach, probed, advertised) = report(advertising("203.0.113.7")).await;
    assert_eq!((reach, probed), (Reachability::EdgeOnly, false), "new addresses, unmeasured");
    assert_eq!(advertised.unwrap().addresses, ["203.0.113.7".parse::<std::net::IpAddr>().unwrap()]);

    sqlx::query("UPDATE node SET reachability = 'public', probed_at = now() WHERE id = $1")
        .bind(n.id)
        .execute(f.store.pool())
        .await
        .unwrap();
    let (reach, probed, _) = report(advertising("203.0.113.7")).await;
    assert_eq!((reach, probed), (Reachability::Public, true), "the same addresses keep it");
    let (reach, probed, _) = report(advertising("")).await;
    assert_eq!((reach, probed), (Reachability::Public, true), "a report that does not say");
    let (reach, probed, _) = report(advertising("198.51.100.9")).await;
    assert_eq!((reach, probed), (Reachability::EdgeOnly, false), "another address, unmeasured");
}

#[tokio::test]
async fn a_node_s_devices_are_kept_as_its_agent_last_reported_them() {
    use korpis_app::ports::{NodeFacts, SessionWrite as _};
    use korpis_core::node::device::{Device, UnofferedDevice};
    use korpis_core::node::driver::DeviceClass;
    let f = fixture().await;
    let n = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &n).await.unwrap();
    let gpu = Device {
        id: "nvidia.com/gpu=0".into(),
        class: DeviceClass::Gpu,
        attributes: [("model".to_owned(), "a100".to_owned())].into(),
        host_nodes: vec!["/dev/nvidia0".into()],
        shares: 4,
    };
    let all = UnofferedDevice { id: "nvidia.com/gpu=all".into(), reason: "an aggregate".into() };
    let reporting = |devices: Vec<Device>, unoffered: Vec<UnofferedDevice>| NodeFacts {
        capacity: n.capacity.clone(),
        allocatable: n.allocatable.clone(),
        drivers: n.drivers.clone(),
        devices,
        unoffered_devices: unoffered,
        storage_classes: None,
        agent: None,
        advertised: None,
    };
    for (devices, unoffered) in [(vec![gpu.clone()], vec![all.clone()]), (Vec::new(), Vec::new())] {
        let mut tx = f.store.begin().await.unwrap();
        let facts = reporting(devices.clone(), unoffered.clone());
        assert!(tx.record_node_facts(n.id, &facts, now()).await.unwrap());
        tx.commit().await.unwrap();
        let read = f.store.node(n.id).await.unwrap().unwrap();
        assert_eq!((read.devices, read.unoffered_devices), (devices, unoffered), "in full");
    }
}
