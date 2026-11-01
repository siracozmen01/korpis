// SPDX-License-Identifier: AGPL-3.0-or-later

//! The migrated database every [`super::TestDb::new`] is copied from. Copying a database is a
//! file copy on the server, where applying every migration to each test's database took most of
//! the test's time.
//!
//! The template is named for the migrations it holds, so a checkout with other migrations makes
//! its own beside it rather than copying a schema that is not its own. Whichever test needs it
//! first makes it, under an advisory lock every other test waits on, and marks it a template
//! only once every migration has applied: one that a killed test left half made is not a
//! template, and is dropped and made again.

use super::database_url;
use crate::{Error, migrate, schema::MIGRATOR};
use sqlx::Connection;
use sqlx::postgres::PgConnection;
use std::hash::{DefaultHasher, Hash, Hasher};

/// The advisory lock the template is made under, on the server's maintenance database.
const LOCK: i64 = 0x6b6f_7270_6973_7470;

/// The template's name, made first if it does not exist.
pub(super) async fn ensure(admin_url: &str) -> Result<String, Error> {
    let name = name();
    let mut admin = PgConnection::connect(admin_url).await?;
    sqlx::query("SELECT pg_advisory_lock($1)").bind(LOCK).execute(&mut admin).await?;
    let made = make(&mut admin, admin_url, &name).await;
    sqlx::query("SELECT pg_advisory_unlock($1)").bind(LOCK).execute(&mut admin).await?;
    admin.close().await?;
    made.map(|()| name)
}

/// The migrations' versions and checksums, folded. The hasher is fixed for a toolchain, and a
/// new toolchain that hashes differently costs one more template.
fn name() -> String {
    let mut hasher = DefaultHasher::new();
    for migration in MIGRATOR.iter() {
        migration.version.hash(&mut hasher);
        migration.checksum.hash(&mut hasher);
    }
    format!("korpis_template_{:016x}", hasher.finish())
}

async fn make(admin: &mut PgConnection, admin_url: &str, name: &str) -> Result<(), Error> {
    let template: Option<bool> =
        sqlx::query_scalar("SELECT datistemplate FROM pg_database WHERE datname = $1")
            .bind(name)
            .fetch_optional(&mut *admin)
            .await?;
    // The name is made from the migrations' checksums above, never from input.
    match template {
        Some(true) => return Ok(()),
        Some(false) => {
            let drop = format!("DROP DATABASE {name} WITH (FORCE)");
            sqlx::query(sqlx::AssertSqlSafe(drop)).execute(&mut *admin).await?;
        }
        None => {}
    }
    let create = format!("CREATE DATABASE {name} TEMPLATE template0");
    sqlx::query(sqlx::AssertSqlSafe(create)).execute(&mut *admin).await?;
    migrate(&database_url(admin_url, name)).await?;
    let mark = format!("ALTER DATABASE {name} WITH IS_TEMPLATE true");
    sqlx::query(sqlx::AssertSqlSafe(mark)).execute(&mut *admin).await?;
    Ok(())
}
