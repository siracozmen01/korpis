// SPDX-License-Identifier: AGPL-3.0-or-later

//! The PostgreSQL adapter: the schema, its migrations, and the store ports of `korpis-app`
//! (§1 of ARCHITECTURE.md, docs/03-state.md).
//!
//! The database enforces referential integrity and immutability and nothing else; authorization
//! is decided in the application (§3.3 of docs/03-state.md). The control plane runs as
//! `korpis_app`, which may insert into the append-only tables and may not update or delete them.

#![warn(missing_docs)]

mod account;
mod assertion;
mod assignment;
mod audit;
mod authz;
mod backup;
mod binding;
mod breaker;
mod certificate;
mod condition;
mod denial;
mod device_grant;
mod domain;
mod endpoint;
mod enrollment;
mod errors;
mod events;
mod extension;
mod failure;
mod feed;
mod idempotency;
mod job;
mod journal;
mod lease;
mod member;
mod node;
mod observation;
mod placement;
mod provider;
mod provider_call;
mod prune;
mod quota;
mod rate;
mod recipe;
mod recovery;
mod relay;
mod run;
pub mod schema;
mod secret;
pub mod server;
mod session;
mod set;
mod sign_in;
mod snapshot;
mod stored;
mod stream;
mod subscription;
mod template;
mod tenancy;
#[cfg(feature = "testing")]
pub mod testing;
mod theme;
mod trust;
mod user_session;
mod volume;
mod workload;

pub use feed::{Subscription, Wakeups};

use korpis_app::ports::{IntentCodec, Store, StoreResult, Tx};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};
use std::str::FromStr;
use std::sync::Arc;

/// The role the control plane acts as. Migrations create it; [`PgStore::connect`] assumes it.
pub const APP_ROLE: &str = "korpis_app";

/// Failures of the operations outside the ports: connecting, migrating, maintenance.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database refused or could not be reached.
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    /// A migration failed.
    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The schema cannot be moved where it was asked to go.
    #[error("{0}")]
    Irreversible(String),
}

/// A pool of connections acting as [`APP_ROLE`].
#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
    bodies: Arc<dyn IntentCodec>,
    feed: Arc<feed::Feed>,
}

/// How often a watcher re-reads what it watches when nothing was announced: time alone changes
/// some statuses (a scheduled window opening).
pub const DEFAULT_WATCH_RESYNC: std::time::Duration = std::time::Duration::from_secs(15);

impl PgStore {
    /// Connects with `url` and assumes [`APP_ROLE`] on every connection, so that the
    /// immutability grants of the schema bind the control plane whatever login it used. Intent
    /// bodies are written and read with `bodies` (§3.1 of docs/03-state.md).
    pub async fn connect(
        url: &str,
        max_connections: u32,
        bodies: Arc<dyn IntentCodec>,
    ) -> Result<Self, Error> {
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
        Ok(Self { pool, bodies, feed: feed::Feed::new(DEFAULT_WATCH_RESYNC) })
    }

    /// Resyncs watchers every `every` instead of [`DEFAULT_WATCH_RESYNC`]. Call before
    /// [`PgStore::listen`] and before anything subscribes.
    #[must_use]
    pub fn with_watch_resync(mut self, every: std::time::Duration) -> Self {
        self.feed = feed::Feed::new(every);
        self
    }

    /// The pool, for tests that inspect what the schema enforces.
    #[cfg(feature = "testing")]
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
    schema::MIGRATOR.run(&pool).await?;
    pool.close().await;
    Ok(())
}

/// A transaction on a [`PgStore`].
#[derive(Debug)]
pub struct PgTx {
    tx: Transaction<'static, Postgres>,
    bodies: Arc<dyn IntentCodec>,
}

impl Store for PgStore {
    type Tx = PgTx;
    type Changes = feed::Subscription;
    type Wakeups = feed::Wakeups;

    fn subscribe(&self) -> feed::Subscription {
        self.subscription()
    }

    fn positions(&self) -> feed::Wakeups {
        self.event_positions()
    }

    fn provider_calls(&self) -> feed::Wakeups {
        self.provider_call_wakeups()
    }

    fn jobs(&self) -> feed::Wakeups {
        self.job_wakeups()
    }

    fn certificates(&self) -> feed::Wakeups {
        self.certificate_wakeups()
    }

    fn endpoint_changes(&self) -> feed::Wakeups {
        self.endpoint_wakeups()
    }

    async fn begin(&self) -> StoreResult<PgTx> {
        let tx = self.pool.begin().await.map_err(errors::from_sqlx)?;
        Ok(PgTx { tx, bodies: Arc::clone(&self.bodies) })
    }
}

impl Tx for PgTx {
    async fn commit(self) -> StoreResult<()> {
        self.tx.commit().await.map_err(errors::from_sqlx)
    }
}
