// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control-plane store (docs/03-state.md): PostgreSQL only, migrations embedded.
//!
//! The database enforces referential integrity and immutability and nothing else; authorization
//! is decided in the gateway (§3.3). The control plane runs as `korpis_app`, which may insert
//! into the append-only tables and may not update or delete them.

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::str::FromStr;

/// The role the control plane acts as. Migrations create it; [`Store::connect`] assumes it.
pub const APP_ROLE: &str = "korpis_app";

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

/// A pool of connections acting as [`APP_ROLE`].
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Connects with `url` and assumes [`APP_ROLE`] on every connection, so that the
    /// immutability grants of the schema bind the control plane whatever login it used.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, Error> {
        let options = PgConnectOptions::from_str(url)?.application_name("korpis-server");
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE korpis_app").execute(&mut *conn).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    /// The underlying pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Ensures effect partitions exist for the coming months. Called at start and daily.
    pub async fn ensure_partitions(&self, months_ahead: i32) -> Result<(), Error> {
        sqlx::query("SELECT korpis_ensure_effect_partitions($1)")
            .bind(months_ahead)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Applies every pending migration, connecting with `owner_url` as the schema owner.
pub async fn migrate(owner_url: &str) -> Result<(), Error> {
    let pool = PgPoolOptions::new().max_connections(1).connect(owner_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    pool.close().await;
    Ok(())
}

pub mod testing;
