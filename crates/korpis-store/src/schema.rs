// SPDX-License-Identifier: AGPL-3.0-or-later

//! The schema's version, and moving it (§4 of docs/18-operations.md): migrations are applied
//! forward by the schema's owner, and every migration after [`BASELINE`] carries its inverse, so a
//! binary can be rolled back one version by undoing what the newer one applied. sqlx's own undo
//! passes over a migration that has no inverse without a word, which would leave a schema the
//! older binary was not built for; [`migrate_to`] refuses instead, before it changes anything.

use crate::Error;
use sqlx::migrate::Migrator;
use sqlx::{Connection as _, PgConnection};

/// The migrations this build carries.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The last migration that has no inverse: the schema every installation starts from. Rolling
/// back past it is refused.
pub const BASELINE: i64 = 74;

/// The newest migration this build carries.
#[must_use]
pub fn latest() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Whether this build carries an inverse of `version`.
#[must_use]
pub fn reversible(version: i64) -> bool {
    MIGRATOR.iter().any(|m| m.version == version && m.migration_type.is_down_migration())
}

/// A migration's description, as its file names it.
fn described(version: i64) -> String {
    let named =
        MIGRATOR.iter().find(|m| m.version == version && !m.migration_type.is_down_migration());
    named.map_or_else(|| "not carried by this build".to_owned(), |m| m.description.to_string())
}

/// How a database's schema stands against this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fit {
    /// It is the schema this build was made for.
    Current,
    /// Migrations this build carries are not applied: the binary is newer than the schema.
    Behind(Vec<i64>),
    /// Migrations this build does not carry are applied: the schema is newer than the binary.
    Ahead(Vec<i64>),
}

/// The versions applied to the database at `url`, oldest first; none before any migration.
///
/// # Errors
/// The database could not be read.
pub async fn applied(url: &str) -> Result<Vec<i64>, Error> {
    let mut c = PgConnection::connect(url).await?;
    let there: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&mut c)
        .await?;
    let versions = if there {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&mut c)
            .await?
    } else {
        Vec::new()
    };
    c.close().await?;
    Ok(versions)
}

/// How `applied` stands against this build.
#[must_use]
pub fn fit(applied: &[i64]) -> Fit {
    let carried: Vec<i64> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    let unknown: Vec<i64> = applied.iter().copied().filter(|v| !carried.contains(v)).collect();
    if !unknown.is_empty() {
        return Fit::Ahead(unknown);
    }
    let pending: Vec<i64> = carried.into_iter().filter(|v| !applied.contains(v)).collect();
    if pending.is_empty() { Fit::Current } else { Fit::Behind(pending) }
}

/// What moving the schema to a version does: the migrations applied, or undone, in order, each
/// with its description.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    /// Applied, oldest first.
    pub apply: Vec<(i64, String)>,
    /// Undone, newest first.
    pub undo: Vec<(i64, String)>,
}

/// What moving a schema with `applied` to `target` (the latest when none) would do, or why it is
/// refused: an undo past [`BASELINE`], of a migration this build has no inverse of, or of one it
/// does not carry at all.
///
/// # Errors
/// [`Error::Irreversible`], naming the migration.
pub fn plan(applied: &[i64], target: Option<i64>) -> Result<Change, Error> {
    let target = target.unwrap_or_else(latest);
    let mut change = Change::default();
    for m in MIGRATOR.iter().filter(|m| !m.migration_type.is_down_migration()) {
        if m.version <= target && !applied.contains(&m.version) {
            change.apply.push((m.version, m.description.to_string()));
        }
    }
    for &v in applied.iter().rev().filter(|&&v| v > target) {
        if v <= BASELINE || !reversible(v) {
            let why = if v <= BASELINE {
                format!("it is part of the baseline, {BASELINE}, which has no inverse")
            } else if MIGRATOR.version_exists(v) {
                "this build carries no inverse of it".to_owned()
            } else {
                "this build does not carry it: undo it with the binary that applied it".to_owned()
            };
            return Err(Error::Irreversible(format!("migration {v} cannot be undone: {why}")));
        }
        change.undo.push((v, described(v)));
    }
    Ok(change)
}

/// Moves the schema at `owner_url` to `target` (the latest when none), as [`plan`] says, and
/// returns what it did.
///
/// # Errors
/// [`plan`] refused, or a migration failed; a failed migration's transaction is rolled back.
pub async fn migrate_to(owner_url: &str, target: Option<i64>) -> Result<Change, Error> {
    let change = plan(&applied(owner_url).await?, target)?;
    let target = target.unwrap_or_else(latest);
    let mut c = PgConnection::connect(owner_url).await?;
    if change.undo.is_empty() {
        MIGRATOR.run_to(target, &mut c).await?;
    } else {
        MIGRATOR.undo(&mut c, target).await?;
    }
    c.close().await?;
    Ok(change)
}
