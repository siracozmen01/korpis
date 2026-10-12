// SPDX-License-Identifier: AGPL-3.0-or-later

//! Throwaway databases for tests. Each one is created from the superuser URL in
//! `KORPIS_TEST_PG_URL` (default: the `scripts/dev-pg.sh` instance), migrated, and dropped when
//! the [`TestDb`] is dropped.

use crate::{Error, PgStore, migrate};
use korpis_app::ports::{CodecError, IntentCodec};
use korpis_core::workload::IntentBody;
use sqlx::Connection;
use sqlx::postgres::PgConnection;
use std::sync::Arc;

/// The default superuser URL, matching `scripts/dev-pg.sh url`.
pub const DEFAULT_ADMIN_URL: &str = "postgres://postgres:korpis-dev@127.0.0.1:55432/postgres";

/// A migrated database that exists for the lifetime of the value.
#[derive(Debug)]
pub struct TestDb {
    admin_url: String,
    name: String,
    /// The owner URL for the database: superuser, connected to it.
    pub owner_url: String,
}

impl TestDb {
    /// Creates and migrates a fresh database.
    pub async fn new() -> Result<Self, Error> {
        let admin_url =
            std::env::var("KORPIS_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_owned());
        let name = format!("korpis_test_{}", uuid::Uuid::now_v7().simple());
        let mut admin = PgConnection::connect(&admin_url).await?;
        // The name is generated from a UUID above, never from input.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name} TEMPLATE template0")))
            .execute(&mut admin)
            .await?;
        admin.close().await?;
        let base = admin_url.rsplit_once('/').map_or(admin_url.as_str(), |(base, _)| base);
        let owner_url = format!("{base}/{name}");
        migrate(&owner_url).await?;
        Ok(Self { admin_url, name, owner_url })
    }

    /// A store acting as the application role, as the control plane does, with intent bodies
    /// stored as the control plane stores them.
    pub async fn store(&self) -> Result<PgStore, Error> {
        PgStore::connect(&self.owner_url, 4, Arc::new(WireBodies)).await
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let name = self.name.clone();
        // Drop on a separate thread with its own runtime: Drop cannot be async, and the test's
        // runtime may already be shutting down.
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(drop_database(&admin_url, &name));
        })
        .join();
    }
}

/// Best effort: a database a crashed test leaves behind goes with `scripts/dev-pg.sh destroy`.
async fn drop_database(admin_url: &str, name: &str) {
    let Ok(mut admin) = PgConnection::connect(admin_url).await else { return };
    // The name was generated from a UUID, never from input.
    let drop = format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
    let _ = sqlx::query(sqlx::AssertSqlSafe(drop)).execute(&mut admin).await;
    let _ = admin.close().await;
}

/// Intent bodies in their stored form (§3.1 of docs/03-state.md), as the control plane's
/// composition root configures them. Only tests construct the store here, so only tests carry
/// this second copy of a three-line adapter.
#[derive(Debug, Clone, Copy)]
struct WireBodies;

impl IntentCodec for WireBodies {
    fn schema_version(&self) -> &str {
        korpis_wire::body::SCHEMA_VERSION
    }

    fn encode(&self, b: &IntentBody) -> Result<String, CodecError> {
        korpis_wire::body::encode(b).map_err(|e| CodecError(e.to_string()))
    }

    fn decode(&self, json: &str, schema_version: &str) -> Result<IntentBody, CodecError> {
        korpis_wire::body::decode(json, schema_version).map_err(|e| CodecError(e.to_string()))
    }
}
