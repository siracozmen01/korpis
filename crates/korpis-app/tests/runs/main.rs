// SPDX-License-Identifier: AGPL-3.0-or-later

//! Making runs against the real schema (§4 of docs/05-scheduling.md), at moments a test sets: a
//! nightly job's run is made once at 03:00 however many control planes look, a run its node never
//! started gives way to the next night's, and stopping the workload cancels what is active.
//! Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_app::ports::{RunRead as _, Store as _, Tx as _, WorkloadWrite as _};
use korpis_app::{App, NoRecipes};
use korpis_core::authz::token::SigningKey;
use korpis_core::labels::Labels;
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::tenancy::Name;
use korpis_core::workload::run::{Cause, RunStatus};
use korpis_core::workload::spec::{Lifecycle, fixture};
use korpis_core::workload::{Intent, Power, PowerRecord, PowerState, Workload};
use korpis_store::PgStore;
use korpis_store::testing::TestDb;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;
use uuid::Uuid;

/// A clock that reads what the test set.
#[derive(Debug, Clone)]
struct SetClock(Arc<Mutex<SystemTime>>);

impl SetClock {
    fn set(&self, at: &str) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = time(at);
    }
}

impl korpis_app::ports::Clock for SetClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn time(at: &str) -> SystemTime {
    at.parse::<jiff::Timestamp>().unwrap().into()
}

struct Fixture {
    _db: TestDb,
    store: PgStore,
    clock: SetClock,
    workload: Uuid,
}

impl Fixture {
    /// Two control planes over one database.
    fn apps(&self) -> [App<PgStore, SetClock, NoRecipes>; 2] {
        let app = || {
            App::new(
                self.store.clone(),
                self.clock.clone(),
                Arc::new(SigningKey::generate(1)),
                NoRecipes,
            )
        };
        [app(), app()]
    }

    async fn runs(&self) -> Vec<korpis_core::workload::run::Run> {
        self.store.runs(self.workload, korpis_app::page::Page::new(None, 100)).await.unwrap()
    }
}

/// An organization and a project in it.
async fn tenancy(store: &PgStore) -> (Uuid, Uuid) {
    let (organization, project) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, 'acme')")
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO project (id, organization_id, name) VALUES ($1, $2, 'jobs')")
        .bind(project)
        .bind(organization)
        .execute(store.pool())
        .await
        .unwrap();
    (organization, project)
}

/// Workload `id`, a nightly job at 03:00 UTC with the default policy, as created at `created`.
fn job(id: Uuid, (organization, project): (Uuid, Uuid), created: SystemTime) -> (Workload, Intent) {
    let body = korpis_core::workload::spec::IntentBody {
        lifecycle: Lifecycle::Scheduled,
        schedule: Some("0 3 * * *".into()),
        timezone: Some("UTC".into()),
        runs: Some(korpis_core::workload::spec::RunPolicy::default()),
        ..fixture()
    };
    let intent = Intent {
        id: Uuid::now_v7(),
        workload: id,
        version: 1,
        parent: None,
        declared_by: Principal::System(SystemActor::Bootstrap),
        declared_at: created,
        body,
        managed_by: None,
    };
    let workload = Workload {
        id,
        project,
        organization,
        name: Name::parse("backup").unwrap(),
        labels: Labels::new([]).unwrap(),
        intent: intent.id,
        intent_version: 1,
        target: None,
        target_version: 0,
        power_version: 1,
        power_target_version: 0,
        version: 1,
        created,
        deleted_at: None,
        set: None,
        classification: Vec::new(),
        unreproducible: None,
    };
    (workload, intent)
}

/// The nightly job, its target and power reached.
async fn nightly() -> Fixture {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let id = Uuid::now_v7();
    let created = time("2026-01-10T00:00:00Z");
    let (workload, intent) = job(id, tenancy(&store).await, created);
    let power = PowerRecord {
        workload: id,
        power: Power::initial(PowerState::Running),
        set_by: Principal::System(SystemActor::Bootstrap),
        set_at: created,
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
    let clock = SetClock(Arc::new(Mutex::new(created)));
    Fixture { _db: db, store, clock, workload: id }
}

#[tokio::test]
async fn a_nightly_run_is_made_once_and_a_run_never_started_gives_way_to_the_next() {
    let f = nightly().await;
    let [one, two] = f.apps();
    f.clock.set("2026-01-10T12:00:00Z");
    assert_eq!(one.tend_runs().await.unwrap(), 1, "the next occurrence is found");
    assert!(f.runs().await.is_empty());

    f.clock.set("2026-01-11T03:00:20Z");
    let (a, b) = tokio::join!(one.tend_runs(), two.tend_runs());
    assert_eq!(a.unwrap() + b.unwrap(), 1, "two control planes, one run");
    let runs = f.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].status, runs[0].scheduled_for),
        (RunStatus::Pending, time("2026-01-11T03:00:00Z"))
    );

    // The node never starts it; the next night it is recorded missed and tonight's is made.
    f.clock.set("2026-01-12T03:00:05Z");
    one.tend_runs().await.unwrap();
    let runs = f.runs().await;
    assert_eq!(runs.len(), 2);
    let missed = runs.iter().find(|r| r.status == RunStatus::Missed).unwrap();
    assert_eq!(missed.cause, Some(Cause::NotStarted));
    for (action, n) in [("run.made", 2), ("run.missed", 1)] {
        let counted: i64 = sqlx::query_scalar("SELECT count(*) FROM effect WHERE action = $1")
            .bind(action)
            .fetch_one(f.store.pool())
            .await
            .unwrap();
        assert_eq!(counted, n, "{action}");
    }
}

#[tokio::test]
async fn stopping_the_workload_cancels_its_active_run() {
    let f = nightly().await;
    let [app, _] = f.apps();
    f.clock.set("2026-01-10T12:00:00Z");
    app.tend_runs().await.unwrap();
    f.clock.set("2026-01-11T03:00:20Z");
    app.tend_runs().await.unwrap();
    let stopped = PowerRecord {
        workload: f.workload,
        power: Power { desired: PowerState::Stopped, cycle: 0, version: 2 },
        set_by: Principal::System(SystemActor::Bootstrap),
        set_at: time("2026-01-11T03:01:00Z"),
    };
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.append_power(&stopped).await.unwrap());
    assert!(tx.move_power_target(f.workload, 2, 1).await.unwrap());
    tx.commit().await.unwrap();
    f.clock.set("2026-01-11T03:05:00Z");
    assert_eq!(app.tend_runs().await.unwrap(), 1);
    let runs = f.runs().await;
    assert_eq!((runs[0].status, runs[0].cause), (RunStatus::Cancelled, Some(Cause::Stopped)));
    assert_eq!(f.store.run_state(f.workload).await.unwrap().unwrap().next_due, None);
}
