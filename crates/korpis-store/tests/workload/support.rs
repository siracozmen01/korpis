// SPDX-License-Identifier: AGPL-3.0-or-later

//! A project to put workloads in, and the values to put there.

use korpis_app::ports::{Store, Tx, WorkloadWrite};
use korpis_core::labels::Labels;
use korpis_core::principal::Principal;
use korpis_core::recipe::RecipeRef;
use korpis_core::tenancy::Name;
use korpis_core::value::{Value, ValueMap};
use korpis_core::workload::spec::endpoints::{Exposure, NetworkPolicy, Protocol, ShapingSpec};
use korpis_core::workload::spec::{
    ConsoleMode, Continuity, EndpointSpec, HealthSpec, IntentBody, IsolationTier, Lifecycle,
    PlacementSpec, ResourceSpec, RuntimeSpec,
};
use korpis_core::workload::{Intent, Power, PowerRecord, PowerState, Workload};
use korpis_store::PgStore;
use korpis_store::testing::TestDb;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub struct Fixture {
    /// Held so the database outlives the test.
    _db: TestDb,
    /// The database's owner, which migrates it.
    pub owner_url: String,
    pub store: PgStore,
    pub organization: Uuid,
    pub project: Uuid,
    /// A root grant plans can be proposed under.
    pub grant: Uuid,
}

/// Now, at the database's precision, so that values read back compare equal.
pub fn now() -> SystemTime {
    let micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
    UNIX_EPOCH + Duration::from_micros(u64::try_from(micros).unwrap())
}

pub async fn fixture() -> Fixture {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let (organization, project) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, 'acme')")
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO project (id, organization_id, name) VALUES ($1, $2, 'games')")
        .bind(project)
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    let grant = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id, delegation,
                             issued_by, root)
         VALUES ($1, 'token:00000000-0000-0000-0000-000000000001', '{}', '{}', 'organization',
                 $2, '{}', 'system:bootstrap', true)",
    )
    .bind(grant)
    .bind(organization)
    .execute(store.pool())
    .await
    .unwrap();
    let owner_url = db.owner_url.clone();
    Fixture { _db: db, owner_url, store, organization, project, grant }
}

pub fn alice() -> Principal {
    Principal::User(Uuid::from_u128(1))
}

pub fn body(motd: &str) -> IntentBody {
    IntentBody {
        recipe: RecipeRef {
            name: "ghcr.io/korpis/recipes/minecraft-paper".into(),
            version: "1.21.4".into(),
            digest: format!("sha256:{}", "9d2f".repeat(16)),
            trust: Some(korpis_core::recipe::trust::Vouched::Signer("korpis".into())),
            providers: Vec::new(),
        },
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        timezone: None,
        runs: None,
        console: ConsoleMode::Interactive,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::new(),
        },
        resources: ResourceSpec::default(),
        devices: Vec::new(),
        volumes: Vec::new(),
        endpoints: vec![EndpointSpec {
            name: "game".into(),
            protocol: Protocol::Tcp,
            container_port: 25565,
            exposure: Exposure::Stable,
            domains: Vec::new(),
            tls: None,
            policy: NetworkPolicy::default(),
            accounting: false,
            shaping: ShapingSpec::default(),
        }],
        health: HealthSpec::default(),
        config: ValueMap::from([("motd".to_owned(), Value::String(motd.into()))]),
        placement: PlacementSpec::default(),
        continuity: Continuity::Derived,
        dependencies: Vec::new(),
        streams: korpis_core::workload::spec::StreamSpec::default(),
        egress: korpis_core::workload::spec::EgressPolicy::default(),
    }
}

pub fn intent(workload: Uuid, version: u64, parent: Option<Uuid>, motd: &str) -> Intent {
    Intent {
        id: Uuid::now_v7(),
        workload,
        version,
        parent,
        declared_by: alice(),
        declared_at: now(),
        body: body(motd),
        managed_by: None,
    }
}

pub fn power(workload: Uuid, power: Power) -> PowerRecord {
    PowerRecord { workload, power, set_by: alice(), set_at: now() }
}

/// A new workload named `name`, created through the port; returns it as created.
pub async fn create(f: &Fixture, name: &str) -> (Workload, Intent) {
    let id = Uuid::now_v7();
    let first = intent(id, 1, None, "hello");
    let w = Workload {
        id,
        project: f.project,
        organization: f.organization,
        name: Name::parse(name).unwrap(),
        labels: Labels::new([("tier".to_owned(), "game".to_owned())]).unwrap(),
        intent: first.id,
        intent_version: 1,
        target: None,
        target_version: 0,
        power_version: 1,
        power_target_version: 0,
        version: 1,
        created: now(),
        deleted_at: None,
        set: None,
        classification: Vec::new(),
        unreproducible: None,
    };
    let mut tx = f.store.begin().await.unwrap();
    tx.create_workload(&w, &first, &power(id, Power::initial(PowerState::Running))).await.unwrap();
    tx.commit().await.unwrap();
    (w, first)
}
