// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh's candidates (§9.1 of docs/07-networking.md): every node not decommissioned that
//! reports a mesh interface, with when it enrolled and the addresses it advertises.

use crate::PgStore;
use crate::errors::from_sqlx;
use crate::stored::{overlay, to_time};
use korpis_app::ports::StoreResult;
use korpis_core::node::mesh::Candidate;

impl PgStore {
    pub(crate) async fn candidates(&self) -> StoreResult<Vec<Candidate>> {
        let rows = sqlx::query!(
            r#"SELECT id, enrolled_at, addresses, overlay AS "overlay!"
                 FROM node WHERE admin <> 'decommissioned' AND overlay IS NOT NULL"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(Candidate {
                    node: r.id,
                    enrolled_at: to_time(r.enrolled_at),
                    addresses: r.addresses.iter().filter_map(|a| a.parse().ok()).collect(),
                    facts: overlay(r.overlay)?,
                })
            })
            .collect()
    }
}
