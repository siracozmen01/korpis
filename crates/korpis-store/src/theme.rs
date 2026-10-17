// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' themes (§7 of docs/11-surface-web.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, to_count, to_json};
use crate::{PgStore, PgTx};
use korpis_app::ports::{SetTheme, StoreResult, ThemeChange, ThemeRead, ThemeWrite};
use korpis_core::tenancy::theme::{Asked, Theme};
use std::collections::BTreeMap;

/// A stored theme back as the theme it was: the row was checked before it was written, so it is
/// checked again on the way out, and a row that fails is a corrupted store, not a theme.
fn theme_of(asked: &Asked) -> StoreResult<Theme> {
    asked.checked().map_err(|e| korpis_app::ports::StoreError::Corrupt(format!("theme: {e}")))
}

fn colours(theme: &Theme, dark: bool) -> BTreeMap<String, String> {
    let scheme = if dark { &theme.dark } else { &theme.light };
    scheme.iter().map(|(k, v)| (k.clone(), v.as_str().to_owned())).collect()
}

impl ThemeRead for PgStore {
    async fn theme(&self, organization: uuid::Uuid) -> StoreResult<Option<SetTheme>> {
        let row = sqlx::query_file!("queries/theme_effective.sql", organization)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(|r| {
            let asked = Asked {
                product_name: r.product_name,
                logo: r.logo_media_type.zip(r.logo),
                light: from_json("theme light", r.light)?,
                dark: from_json("theme dark", r.dark)?,
                sign_in_text: r.sign_in_text,
                stylesheet: r.stylesheet,
            };
            Ok(SetTheme { organization: r.organization_id, theme: theme_of(&asked)? })
        })
        .transpose()
    }

    async fn own_theme_version(&self, organization: uuid::Uuid) -> StoreResult<u64> {
        let version = sqlx::query_file_scalar!("queries/theme_own_version.sql", organization)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        version.map_or(Ok(0), |v| count("theme version", v))
    }

    async fn installation_root(&self) -> StoreResult<Option<uuid::Uuid>> {
        sqlx::query_file_scalar!("queries/installation_root.sql")
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }
}

impl ThemeWrite for PgTx {
    async fn set_theme(&mut self, c: &ThemeChange, expected: u64) -> StoreResult<bool> {
        let empty = Theme::default();
        let t = c.theme.as_ref().unwrap_or(&empty);
        let light = to_json("theme light", &colours(t, false))?;
        let dark = to_json("theme dark", &colours(t, true))?;
        let media_type = t.logo.as_ref().map(|l| l.kind.media_type());
        let logo = t.logo.as_ref().map(|l| l.data.as_slice());
        let version = to_count("theme version", c.version)?;
        let (by, at) = (c.set_by.key(), from_time(c.set_at));
        let (org, inherit) = (c.organization, c.theme.is_none());
        let fields = (&t.product_name, &t.sign_in_text, &t.stylesheet);
        let query = if expected == 0 {
            if inherit {
                return Ok(false);
            }
            sqlx::query_file!(
                "queries/theme_set.sql",
                org,
                fields.0,
                media_type,
                logo,
                light,
                dark,
                fields.1,
                fields.2,
                version,
                by,
                at
            )
        } else {
            sqlx::query_file!(
                "queries/theme_update.sql",
                org,
                fields.0,
                media_type,
                logo,
                light,
                dark,
                fields.1,
                fields.2,
                version,
                by,
                at,
                to_count("theme version", expected)?,
                inherit
            )
        };
        let done = query.execute(&mut *self.tx).await.map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
