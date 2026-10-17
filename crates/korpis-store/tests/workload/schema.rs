// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migrations reversible for one version (§4 of docs/18-operations.md): every migration after the
//! baseline carries its inverse, each inverse leaves exactly the schema the migrations before it
//! made, and undoing and reapplying them on a populated store keeps every row.

use crate::support::{create, fixture};
use korpis_app::ports::WorkloadRead as _;
use korpis_store::schema::{self, BASELINE, Fit, MIGRATOR};
use korpis_store::testing::TestDb;
use sqlx::{Connection as _, PgConnection};

/// What the schema is, as sorted lines: tables' columns, constraints, indexes, views, functions,
/// triggers, what each role may do to each table, and the connecting role's memberships.
async fn shape(url: &str) -> Vec<String> {
    let mut c = PgConnection::connect(url).await.unwrap();
    let queries = [
        "SELECT format('column %s.%s %s %s %s', table_name, column_name, data_type, is_nullable,
                       coalesce(column_default, '-'))
           FROM information_schema.columns WHERE table_schema = 'public'",
        "SELECT format('constraint %s %s %s', conrelid::regclass, conname,
                       pg_get_constraintdef(oid))
           FROM pg_constraint WHERE connamespace = 'public'::regnamespace",
        "SELECT format('index %s', indexdef) FROM pg_indexes WHERE schemaname = 'public'",
        "SELECT format('view %s %s', viewname, definition) FROM pg_views
          WHERE schemaname = 'public'",
        "SELECT format('function %s', pg_get_functiondef(oid)) FROM pg_proc
          WHERE pronamespace = 'public'::regnamespace AND prokind IN ('f', 'p')",
        "SELECT format('trigger %s', pg_get_triggerdef(oid)) FROM pg_trigger
          WHERE NOT tgisinternal",
        "SELECT format('grant %s %s %s', grantee, table_name, privilege_type)
           FROM information_schema.role_table_grants WHERE table_schema = 'public'",
        "SELECT format('member %s', r.rolname) FROM pg_auth_members a
           JOIN pg_roles r ON r.oid = a.roleid
           JOIN pg_roles m ON m.oid = a.member WHERE m.rolname = current_user",
    ];
    let mut lines = Vec::new();
    for q in queries {
        lines.extend(sqlx::query_scalar::<_, String>(q).fetch_all(&mut c).await.unwrap());
    }
    c.close().await.unwrap();
    lines.sort();
    lines
}

#[test]
fn every_migration_after_the_baseline_carries_its_inverse_and_none_before_it_does() {
    for m in MIGRATOR.iter().filter(|m| !m.migration_type.is_down_migration()) {
        let reversible = schema::reversible(m.version);
        if m.version > BASELINE {
            assert!(reversible, "migration {} has no .down.sql", m.version);
        } else {
            assert!(!reversible, "migration {} is in the baseline and has an inverse", m.version);
        }
    }
    assert!(schema::latest() > BASELINE, "the inverses are exercised by the tests below");
}

#[tokio::test]
async fn each_inverse_leaves_exactly_the_schema_the_migrations_before_it_made() {
    let reversible: Vec<i64> = MIGRATOR
        .iter()
        .filter(|m| m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    for v in reversible {
        let fresh = TestDb::empty().await.unwrap();
        let mut c = PgConnection::connect(&fresh.owner_url).await.unwrap();
        MIGRATOR.run_to(v - 1, &mut c).await.unwrap();
        c.close().await.unwrap();
        let undone = TestDb::new().await.unwrap();
        let change = schema::migrate_to(&undone.owner_url, Some(v - 1)).await.unwrap();
        assert!(change.apply.is_empty() && change.undo.iter().any(|(u, _)| *u == v));
        assert_eq!(shape(&fresh.owner_url).await, shape(&undone.owner_url).await, "undoing {v}");
    }
}

/// Every table's row count, by table.
async fn rows(url: &str) -> Vec<(String, i64)> {
    let mut c = PgConnection::connect(url).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT format('SELECT %L, count(*) FROM %I', table_name, table_name)
           FROM information_schema.tables
          WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
            AND table_name <> '_sqlx_migrations' ORDER BY table_name",
    )
    .fetch_all(&mut c)
    .await
    .unwrap();
    let mut counts = Vec::new();
    for counting in tables {
        // Written by `format` above, with the table's name quoted by %I.
        let q = sqlx::query_as(sqlx::AssertSqlSafe(counting));
        counts.push(q.fetch_one(&mut c).await.unwrap());
    }
    c.close().await.unwrap();
    counts
}

#[tokio::test]
async fn undoing_to_the_baseline_and_reapplying_keeps_every_row_of_a_populated_store() {
    let f = fixture().await;
    let (workload, _) = create(&f, "survives").await;
    let (before, shaped) = (rows(&f.owner_url).await, shape(&f.owner_url).await);
    assert!(before.iter().any(|(t, n)| t == "workload" && *n == 1));

    let down = schema::migrate_to(&f.owner_url, Some(BASELINE)).await.unwrap();
    assert_eq!(down.undo.first().map(|(v, _)| *v), Some(schema::latest()), "newest first");
    let applied = schema::applied(&f.owner_url).await.unwrap();
    assert!(matches!(schema::fit(&applied), Fit::Behind(_)), "an older schema than this build");
    let up = schema::migrate_to(&f.owner_url, None).await.unwrap();
    assert_eq!(up.apply.len(), down.undo.len());
    assert_eq!(schema::fit(&schema::applied(&f.owner_url).await.unwrap()), Fit::Current);

    assert_eq!(rows(&f.owner_url).await, before);
    assert_eq!(shape(&f.owner_url).await, shaped);
    let read = f.store.workload(workload.id).await.unwrap();
    assert_eq!(read.map(|w| w.name), Some(workload.name), "the store reads it back");

    let refused = schema::migrate_to(&f.owner_url, Some(BASELINE - 1)).await.unwrap_err();
    assert!(refused.to_string().contains("part of the baseline"), "{refused}");
    assert_eq!(schema::fit(&schema::applied(&f.owner_url).await.unwrap()), Fit::Current);
}
