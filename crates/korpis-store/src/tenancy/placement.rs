// SPDX-License-Identifier: AGPL-3.0-or-later

//! The placement policy an organization sets for its subtree (§2.2 of docs/05-scheduling.md),
//! kept beside it in the form an explanation keeps a policy in.

use crate::errors::from_sqlx;
use crate::stored::{PolicyDoc, from_json, to_json};
use korpis_app::ports::StoreResult;
use korpis_core::schedule::PlacementPolicy;
use sqlx::PgExecutor;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The policies those of `organizations` set themselves.
pub(super) async fn policies(
    db: impl PgExecutor<'_>,
    organizations: &[Uuid],
) -> StoreResult<BTreeMap<Uuid, PlacementPolicy>> {
    let rows = sqlx::query!(
        r#"SELECT id, placement_policy AS "policy!" FROM organization
            WHERE id = ANY($1) AND placement_policy IS NOT NULL"#,
        organizations
    )
    .fetch_all(db)
    .await
    .map_err(from_sqlx)?;
    rows.into_iter()
        .map(|r| {
            let doc: PolicyDoc = from_json("placement policy", r.policy)?;
            Ok((r.id, doc.into_domain()?))
        })
        .collect()
}

/// Sets or removes organization `id`'s own policy; the one replaced, or `None` outside if the
/// organization does not exist.
pub(super) async fn set(
    db: impl PgExecutor<'_>,
    id: Uuid,
    policy: Option<&PlacementPolicy>,
) -> StoreResult<Option<Option<PlacementPolicy>>> {
    let doc =
        policy.map(|p| to_json("placement policy", &PolicyDoc::from_domain(p))).transpose()?;
    let replaced = sqlx::query_scalar!(
        "UPDATE organization o SET placement_policy = $2
           FROM (SELECT placement_policy FROM organization WHERE id = $1 FOR UPDATE) old
          WHERE o.id = $1
         RETURNING old.placement_policy",
        id,
        doc
    )
    .fetch_optional(db)
    .await
    .map_err(from_sqlx)?;
    replaced
        .map(|old| {
            old.map(|v| from_json::<PolicyDoc>("placement policy", v)?.into_domain()).transpose()
        })
        .transpose()
}
