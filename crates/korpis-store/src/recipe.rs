// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolved recipes, by digest.

use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::ports::{CachedRecipe, RecipeCache, StoreError, StoreResult};
use korpis_core::recipe::trust::Signature;

impl RecipeCache for PgStore {
    async fn cached_recipe(&self, digest: &str) -> StoreResult<Option<CachedRecipe>> {
        let row = sqlx::query!(
            "SELECT name, version, document, catalogues FROM recipe WHERE digest = $1",
            digest
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let catalogues = serde_json::from_value(r.catalogues)
                .map_err(|e| StoreError::Corrupt(format!("recipe {digest} catalogues: {e}")))?;
            Ok(CachedRecipe { name: r.name, version: r.version, document: r.document, catalogues })
        })
        .transpose()
    }

    async fn cache_recipe(&self, digest: &str, r: &CachedRecipe) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO recipe (digest, name, version, document, catalogues)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (digest) DO NOTHING",
            digest,
            r.name,
            r.version,
            r.document,
            serde_json::to_value(&r.catalogues)
                .map_err(|e| StoreError::Corrupt(format!("recipe {digest} catalogues: {e}")))?
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
