// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolved recipes, by digest.

use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::ports::{CachedRecipe, RecipeCache, StoreResult};
use korpis_core::recipe::trust::Signature;

impl RecipeCache for PgStore {
    async fn cached_recipe(&self, digest: &str) -> StoreResult<Option<CachedRecipe>> {
        let row =
            sqlx::query!("SELECT name, version, document FROM recipe WHERE digest = $1", digest)
                .fetch_optional(&self.pool)
                .await
                .map_err(from_sqlx)?;
        Ok(row.map(|r| CachedRecipe { name: r.name, version: r.version, document: r.document }))
    }

    async fn cache_recipe(&self, digest: &str, r: &CachedRecipe) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO recipe (digest, name, version, document) VALUES ($1, $2, $3, $4)
             ON CONFLICT (digest) DO NOTHING",
            digest,
            r.name,
            r.version,
            r.document
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn cached_signatures(&self, digest: &str) -> StoreResult<Vec<Signature>> {
        let rows = sqlx::query!(
            "SELECT payload, signature FROM recipe_signature WHERE digest = $1 ORDER BY found_at",
            digest
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|r| Signature { payload: r.payload, signature: r.signature })
            .collect())
    }

    async fn cache_signatures(&self, digest: &str, signatures: &[Signature]) -> StoreResult<()> {
        for s in signatures {
            sqlx::query!(
                "INSERT INTO recipe_signature (digest, payload, signature) VALUES ($1, $2, $3)
                 ON CONFLICT (digest, signature) DO NOTHING",
                digest,
                s.payload,
                s.signature
            )
            .execute(&self.pool)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }
}
