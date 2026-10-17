// SPDX-License-Identifier: AGPL-3.0-or-later

//! What installation asks of the PostgreSQL server before it touches it (§2 of
//! docs/18-operations.md), and the login the control plane runs as: a role of its own, a member
//! of [`crate::APP_ROLE`] and nothing else, so the credential a running control plane holds can
//! neither change the schema nor undo the immutability grants, which the owner's could.

use crate::Error;
use sqlx::{Connection as _, PgConnection};

/// What the server says of itself, and of what the installing role may do on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFacts {
    /// `server_version_num`, e.g. 170004.
    pub version_num: i32,
    /// `server_version`, as it prints it.
    pub version: String,
    /// Whether the connecting role may create roles.
    pub creates_roles: bool,
    /// Whether it may create schema objects in the database.
    pub creates_objects: bool,
    /// How many of this build's migrations the database has applied.
    pub applied: i64,
    /// How many migrations this build carries.
    pub bundled: i64,
    /// Databases on the server other than this one and PostgreSQL's own.
    pub other_databases: i64,
    /// Whether the database already holds an installation: an organization exists.
    pub installed: bool,
}

/// Asks the server at `owner_url` about itself.
///
/// # Errors
/// It cannot be reached, or refused a question.
pub async fn server_facts(owner_url: &str) -> Result<ServerFacts, Error> {
    let mut c = PgConnection::connect(owner_url).await?;
    let (version_num, version): (String, String) = sqlx::query_as(
        "SELECT current_setting('server_version_num'), current_setting('server_version')",
    )
    .fetch_one(&mut c)
    .await?;
    let (creates_roles, creates_objects): (bool, bool) = sqlx::query_as(
        "SELECT r.rolcreaterole OR r.rolsuper, has_database_privilege(current_database(), 'CREATE')
           FROM pg_roles r WHERE r.rolname = current_user",
    )
    .fetch_one(&mut c)
    .await?;
    let applied: i64 =
        if sqlx::query_scalar::<_, bool>("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut c)
            .await?
        {
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&mut c)
                .await?
        } else {
            0
        };
    let other_databases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_database WHERE datallowconn
            AND datname NOT IN (current_database(), 'postgres', 'template0', 'template1')",
    )
    .fetch_one(&mut c)
    .await?;
    let installed = applied > 0
        && sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM organization)")
            .fetch_one(&mut c)
            .await?;
    c.close().await?;
    let bundled = i64::try_from(sqlx::migrate!("./migrations").iter().count()).unwrap_or(i64::MAX);
    Ok(ServerFacts {
        version_num: version_num.parse().unwrap_or(0),
        version,
        creates_roles,
        creates_objects,
        applied,
        bundled,
        other_databases,
        installed,
    })
}

/// Makes `role` a login with `password`, a member of [`crate::APP_ROLE`]: created, or, where it
/// is there, given the password again. The statements are written by the server's own `format`,
/// which quotes the name and the password, so neither is ever spliced into SQL here.
///
/// # Errors
/// The connecting role may not create or alter roles, or the server refused.
pub async fn ensure_login(owner_url: &str, role: &str, password: &str) -> Result<(), Error> {
    let mut c = PgConnection::connect(owner_url).await?;
    let statement: String = sqlx::query_scalar(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)
                THEN format('ALTER ROLE %I LOGIN PASSWORD %L', $1, $2)
                ELSE format('CREATE ROLE %I LOGIN PASSWORD %L', $1, $2) END",
    )
    .bind(role)
    .bind(password)
    .fetch_one(&mut c)
    .await?;
    // Written by `format` above, with the name quoted by %I and the password by %L.
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement)).execute(&mut c).await?;
    let grant: String = sqlx::query_scalar("SELECT format('GRANT %I TO %I', $1::text, $2::text)")
        .bind(crate::APP_ROLE)
        .bind(role)
        .fetch_one(&mut c)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(grant)).execute(&mut c).await?;
    c.close().await?;
    Ok(())
}
