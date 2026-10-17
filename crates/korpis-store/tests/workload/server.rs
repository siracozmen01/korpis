// SPDX-License-Identifier: AGPL-3.0-or-later

//! The server installation reads before it touches it, and the control plane's own login.

use korpis_store::server::{ensure_login, server_facts};
use korpis_store::testing::TestDb;
use sqlx::{Connection as _, PgConnection};

#[tokio::test]
async fn the_server_says_its_version_and_what_the_installer_may_do_on_it() {
    let db = TestDb::new().await.unwrap();
    let facts = server_facts(&db.owner_url).await.unwrap();
    assert!(facts.version_num >= 130_000, "{facts:?}");
    assert!(facts.creates_roles && facts.creates_objects, "{facts:?}");
    assert_eq!(facts.applied, facts.bundled, "a migrated database has every migration");
    assert!(facts.bundled > 70);
    assert!(!facts.installed, "migrated, and no organization yet");
}

#[tokio::test]
async fn the_control_plane_s_login_reaches_the_tables_as_the_app_role_and_cannot_change_them() {
    let db = TestDb::new().await.unwrap();
    let role = format!("korpis_server_{}", uuid::Uuid::now_v7().simple());
    ensure_login(&db.owner_url, &role, "first secret").await.unwrap();
    ensure_login(&db.owner_url, &role, "it's a 'quoted' secret").await.unwrap();
    let (base, name) = db.owner_url.rsplit_once('/').unwrap();
    let host = base.rsplit_once('@').unwrap().1;
    let url = format!("postgres://{role}:it%27s%20a%20%27quoted%27%20secret@{host}/{name}");
    let mut c = PgConnection::connect(&url).await.expect("the second password is the one kept");
    sqlx::query("SET ROLE korpis_app").execute(&mut c).await.expect("a member of the app role");
    let read: i64 =
        sqlx::query_scalar("SELECT count(*) FROM organization").fetch_one(&mut c).await.unwrap();
    assert_eq!(read, 0);
    sqlx::query("RESET ROLE").execute(&mut c).await.unwrap();
    let altered = sqlx::query("ALTER TABLE organization ADD COLUMN x int").execute(&mut c).await;
    assert!(altered.is_err(), "the login is not the owner, and cannot change the schema");
    let truncated = sqlx::query("TRUNCATE effect").execute(&mut c).await;
    assert!(truncated.is_err(), "nor undo what is append-only");
    c.close().await.unwrap();
    let mut owner = PgConnection::connect(&db.owner_url).await.unwrap();
    let drop: String = sqlx::query_scalar("SELECT format('DROP ROLE %I', $1::text)")
        .bind(&role)
        .fetch_one(&mut owner)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(drop)).execute(&mut owner).await.unwrap();
}
