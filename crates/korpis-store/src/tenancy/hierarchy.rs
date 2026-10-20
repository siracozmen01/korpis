// SPDX-License-Identifier: AGPL-3.0-or-later

//! The object hierarchy authorization evaluates against.

use super::labels;
use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::ports::{HierarchyRead, StoreError, StoreResult};
use korpis_core::authz::{Kind, Node, ObjectRef};
use std::time::Duration;

impl HierarchyRead for PgStore {
    async fn ancestry(&self, object: ObjectRef) -> StoreResult<Vec<Node>> {
        let rows = sqlx::query_file!("queries/ancestry.sql", object.kind.as_str(), object.id)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let kind = Kind::parse(&r.kind)
                    .ok_or_else(|| StoreError::Corrupt(format!("object kind {:?}", r.kind)))?;
                Ok(Node {
                    object: ObjectRef::new(kind, r.id),
                    labels: labels(r.labels)?.into_map(),
                    suspended: r.suspended,
                    ceiling: r
                        .ceiling_ms
                        .map(|ms| u64::try_from(ms).map(Duration::from_millis))
                        .transpose()
                        .map_err(|_| StoreError::Corrupt("delegation ceiling".into()))?,
                })
            })
            .collect()
    }
}
