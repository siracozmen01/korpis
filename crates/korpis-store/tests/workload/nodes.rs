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
    (n.last_seen, n.enrolled_at) = (Some(now()), now());
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
