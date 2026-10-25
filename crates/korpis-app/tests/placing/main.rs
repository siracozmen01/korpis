// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placing workloads that ask for devices, against the real schema (§6.3 of
//! docs/05-scheduling.md): the bind holds what the filter found free, a device held is given to no
//! one else, and a placement removed frees it. Needs the development database
//! (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_app::ports::WorkloadWrite as _;
use korpis_app::ports::{PlacementRead as _, PlacementWrite as _, Store as _, Tx as _};
use korpis_app::{App, NoRecipes};
use korpis_core::authz::token::SigningKey;
use korpis_core::labels::Labels;
use korpis_core::node::device::{Device, DeviceHold, DeviceMode};
use korpis_core::node::driver::DeviceClass;
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::schedule::Check;
use korpis_core::tenancy::Name;
use korpis_core::workload::spec::devices::DeviceRequest;
use korpis_core::workload::spec::fixture;
use korpis_core::workload::{Intent, Power, PowerRecord, PowerState, Workload};
use korpis_store::PgStore;
use korpis_store::testing::{TestDb, enroll, ready_node};
use std::sync::Arc;
use std::time::SystemTime;
use uuid::Uuid;

/// An organization and a project in it.
async fn tenancy(store: &PgStore) -> (Uuid, Uuid) {
    let (organization, project) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, 'acme')")
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO project (id, organization_id, name) VALUES ($1, $2, 'gpus')")
        .bind(project)
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    (organization, project)
}

/// A workload named `name` asking for one GPU in passthrough, its target approved.
async fn asking(store: &PgStore, (organization, project): (Uuid, Uuid), name: &str) -> Uuid {
    let (id, now) = (Uuid::now_v7(), SystemTime::now());
    let gpu = DeviceRequest {
        class: DeviceClass::Gpu,
        count: 1,
        selector: None,
        mode: DeviceMode::Passthrough,
    };
    let intent = Intent {
        id: Uuid::now_v7(),
        workload: id,
        version: 1,
        parent: None,
        declared_by: Principal::System(SystemActor::Bootstrap),
        declared_at: now,
        body: korpis_core::workload::spec::IntentBody { devices: vec![gpu], ..fixture() },
        managed_by: None,
    };
    let workload = Workload {
        id,
        project,
        organization,
        name: Name::parse(name).unwrap(),
        labels: Labels::new([]).unwrap(),
        intent: intent.id,
        intent_version: 1,
        target: None,
        target_version: 0,
        power_version: 1,
        power_target_version: 0,
        version: 1,
        created: now,
        deleted_at: None,
        set: None,
        classification: Vec::new(),
        unreproducible: None,
    };
    let power = PowerRecord {
        workload: id,
        power: Power::initial(PowerState::Running),
        set_by: Principal::System(SystemActor::Bootstrap),
        set_at: now,
    };
    let mut tx = store.begin().await.unwrap();
    tx.create_workload(&workload, &intent, &power).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE workload SET target_id = intent_id, target_version = 1, power_target_version = 1
          WHERE id = $1",
    )
    .bind(id)
    .execute(store.pool())
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn a_gpu_is_held_by_the_workload_placed_with_it_and_freed_with_its_placement() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let tenancy = tenancy(&store).await;
    let mut node = ready_node(tenancy.0, "gpu-1", &[]).unwrap();
    node.drivers[0].capabilities.devices = vec![DeviceClass::Gpu];
    node.drivers[0].capabilities.device_modes = vec![DeviceMode::Passthrough];
    node.devices = vec![Device {
        id: "nvidia.com/gpu=0".into(),
        class: DeviceClass::Gpu,
        attributes: std::collections::BTreeMap::default(),
        host_nodes: vec!["/dev/nvidia0".into()],
        shares: 0,
    }];
    enroll(&store, &node).await.unwrap();
    let (first, second) =
        (asking(&store, tenancy, "one").await, asking(&store, tenancy, "two").await);
    let app = App::new(
        store.clone(),
        korpis_app::ports::SystemClock,
        Arc::new(SigningKey::generate(1)),
        NoRecipes,
    );
    assert_eq!(app.reconcile_placements().await.unwrap(), 2);
    let held = DeviceHold { device: "nvidia.com/gpu=0".into(), mode: DeviceMode::Passthrough };
    let placed = store.placement(first).await.unwrap().expect("the first is placed");
    assert_eq!((placed.node, placed.devices), (node.id, vec![held.clone()]));
    assert!(store.placement(second).await.unwrap().is_none(), "the GPU is held");
    let unplaced = store.unplaced(second).await.unwrap().expect("and says why");
    let refusal = unplaced.explanation.filtered.iter().find(|f| f.check == Check::Device);
    assert!(refusal.is_some_and(|f| f.detail.contains("0 of 1 gpu")), "{unplaced:?}");

    // The first loses its target, as a deletion's would, and its placement goes with it.
    sqlx::query("UPDATE workload SET target_id = NULL WHERE id = $1")
        .bind(first)
        .execute(store.pool())
        .await
        .unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.unbind(first).await.unwrap();
    tx.commit().await.unwrap();
    app.reconcile_placements().await.unwrap();
    let placed = store.placement(second).await.unwrap().expect("freed, the GPU is given again");
    assert_eq!(placed.devices, [held]);
}
