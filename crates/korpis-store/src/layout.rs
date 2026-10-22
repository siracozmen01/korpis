// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' layouts (question 2 of §10 of docs/11-surface-web.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, to_count, to_json};
use crate::{PgStore, PgTx};
use korpis_app::ports::{
    LayoutChange, LayoutRead, LayoutWrite, SetLayout, StoreError, StoreResult,
};
use korpis_core::tenancy::layout::{Asked, AskedItem, Item, Layout};
use serde::{Deserialize, Serialize};

/// An item as it is stored: the component, named, with its own fields.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stored {
    Places,
    Quota,
    Notice { heading: String, text: String },
    Link { label: String, url: String },
}

impl From<&Item> for Stored {
    fn from(i: &Item) -> Self {
        match i {
            Item::Places => Self::Places,
            Item::Quota => Self::Quota,
            Item::Notice { heading, text } => {
                Self::Notice { heading: heading.clone(), text: text.clone() }
            }
            Item::Link { label, url } => Self::Link { label: label.clone(), url: url.clone() },
        }
    }
}

impl From<Stored> for AskedItem {
    fn from(s: Stored) -> Self {
        match s {
            Stored::Places => Self::Places,
            Stored::Quota => Self::Quota,
            Stored::Notice { heading, text } => Self::Notice { heading, text },
            Stored::Link { label, url } => Self::Link { label, url },
        }
    }
}

/// A stored layout back as the layout it was: the row was checked before it was written, so it
/// is checked again on the way out, and a row that fails is a corrupted store, not a layout.
fn layout_of(name: String, items: serde_json::Value) -> StoreResult<Layout> {
    let items: Vec<Stored> = from_json("layout items", items)?;
    let asked = Asked { name, items: items.into_iter().map(AskedItem::from).collect() };
    asked.checked().map_err(|e| StoreError::Corrupt(format!("layout: {e}")))
}

impl LayoutRead for PgStore {
    async fn layout(&self, organization: uuid::Uuid) -> StoreResult<Option<SetLayout>> {
        let row = sqlx::query_file!("queries/layout_effective.sql", organization)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(SetLayout { organization: r.organization_id, layout: layout_of(r.name, r.items)? })
        })
        .transpose()
    }

    async fn own_layout_version(&self, organization: uuid::Uuid) -> StoreResult<u64> {
        let version = sqlx::query_file_scalar!("queries/layout_own_version.sql", organization)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        version.map_or(Ok(0), |v| count("layout version", v))
    }
}

impl LayoutWrite for PgTx {
    async fn set_layout(&mut self, c: &LayoutChange, expected: u64) -> StoreResult<bool> {
        let (name, items) = match &c.layout {
            Some(l) => (l.name.as_str(), l.items.iter().map(Stored::from).collect::<Vec<_>>()),
            None => ("", Vec::new()),
        };
        let items = to_json("layout items", &items)?;
        let version = to_count("layout version", c.version)?;
        let (by, at) = (c.set_by.key(), from_time(c.set_at));
        let (org, inherit) = (c.organization, c.layout.is_none());
        let query = if expected == 0 {
            if inherit {
                return Ok(false);
            }
            sqlx::query_file!("queries/layout_set.sql", org, name, items, version, by, at)
        } else {
            let expected = to_count("layout version", expected)?;
            sqlx::query_file!(
                "queries/layout_update.sql",
                org,
                name,
                items,
                version,
                by,
                at,
                expected,
                inherit
            )
        };
        let done = query.execute(&mut *self.tx).await.map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
