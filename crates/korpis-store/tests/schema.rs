// SPDX-License-Identifier: AGPL-3.0-or-later

//! The schema enforces what docs/03-state.md says the database enforces, as the role the
//! control plane actually runs as. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_store::testing::TestDb;
use sqlx::Row;
use uuid::Uuid;

async fn seed_workload(store: &korpis_store::PgStore) -> (Uuid, Uuid) {
    let pool = store.pool();
    let (org, project, workload, intent) =
        (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, 'acme')")
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO project (id, organization_id, name) VALUES ($1, $2, 'games')")
        .bind(project)
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workload (id, project_id, organization_id, name) VALUES ($1, $2, $3, 'mc')",
    )
    .bind(workload)
    .bind(project)
    .bind(org)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO intent (id, workload_id, version, declared_by, body, schema_version)
         VALUES ($1, $2, 1, 'user:u1', '{}', 'korpis.v1')",
    )
    .bind(intent)
    .bind(workload)
    .execute(pool)
    .await
    .unwrap();
    (workload, intent)
}

#[tokio::test]
async fn intents_and_effects_are_append_only_for_the_app_role() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let (workload, intent) = seed_workload(&store).await;

    let update = sqlx::query("UPDATE intent SET body = '{\"x\":1}' WHERE id = $1")
        .bind(intent)
        .execute(store.pool())
        .await;
    assert!(update.is_err(), "the app role must not be able to rewrite an intent");
    let delete =
        sqlx::query("DELETE FROM intent WHERE id = $1").bind(intent).execute(store.pool()).await;
    assert!(delete.is_err(), "the app role must not be able to delete an intent");

    sqlx::query(
        "INSERT INTO effect (id, workload_id, actor, action, outcome)
         VALUES ($1, $2, '{\"system\":\"SYSTEM_ACTOR_RECONCILER\"}', 'workload.start', 'succeeded')",
    )
    .bind(Uuid::now_v7())
    .bind(workload)
    .execute(store.pool())
    .await
    .expect("effects are insertable into the current month's partition");
    let rewrite = sqlx::query("UPDATE effect SET outcome = 'failed'").execute(store.pool()).await;
    assert!(rewrite.is_err(), "the app role must not be able to rewrite an effect");
}

#[tokio::test]
async fn at_most_one_plan_applies_per_workload() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let (workload, _) = seed_workload(&store).await;
    let insert = |status: &'static str| {
        sqlx::query(
            "INSERT INTO plan (id, workload_id, kind, to_power, computed_by, steps, impact,
                               status, expires_at, approval, approval_reason)
             VALUES ($1, $2, 'power', 1, 'user:u1', '[]', '{}', $3, now() + interval '1 hour',
                     'auto', '')",
        )
        .bind(Uuid::now_v7())
        .bind(workload)
        .bind(status)
        .execute(store.pool())
    };
    insert("applying").await.unwrap();
    insert("queued").await.expect("a second plan queues");
    assert!(insert("applying").await.is_err(), "a second applying plan is refused by the store");
}

#[tokio::test]
async fn link_form_grants_cannot_delegate() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let org = Uuid::now_v7();
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, 'acme')")
        .bind(org)
        .execute(store.pool())
        .await
        .unwrap();
    let root = Uuid::now_v7();
    let grant = |id: Uuid, parent: Option<Uuid>, link: bool, delegation: &'static str| {
        sqlx::query(
            "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id,
                                 parent_id, delegation, issued_by, link_form)
             VALUES ($1, 'user:u1', ARRAY['workload.restart'], '{}', 'organization', $2,
                     $3, $4::jsonb, 'system:bootstrap', $5)",
        )
        .bind(id)
        .bind(org)
        .bind(parent)
        .bind(delegation)
        .bind(link)
        .execute(store.pool())
    };
    assert!(
        grant(root, None, false, r#"{"allowed":true}"#).await.is_err(),
        "a parentless grant must be marked root"
    );
    sqlx::query(
        "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id,
                             delegation, issued_by, root)
         VALUES ($1, 'token:root', ARRAY['workload.restart'], '{}', 'organization', $2,
                 '{\"allowed\":true}', 'system:bootstrap', true)",
    )
    .bind(root)
    .bind(org)
    .execute(store.pool())
    .await
    .unwrap();
    assert!(
        grant(Uuid::now_v7(), Some(root), true, r#"{"allowed":true}"#).await.is_err(),
        "a link-form grant with delegation allowed is refused by the store"
    );
    grant(Uuid::now_v7(), Some(root), true, r#"{"allowed":false}"#).await.unwrap();
}

#[tokio::test]
async fn effect_partitions_cover_the_coming_months() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    store.ensure_partitions(6).await.expect("the app role may extend partitions");
    let row =
        sqlx::query("SELECT count(*) AS n FROM pg_inherits WHERE inhparent = 'effect'::regclass")
            .fetch_one(store.pool())
            .await
            .unwrap();
    let partitions: i64 = row.get("n");
    assert!(partitions >= 8, "last month, this month, and six ahead: got {partitions}");
}
