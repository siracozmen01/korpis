// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list's entries (§6.3 of docs/08-identity.md): grants revoked within the
//! horizon, and grants suspended within it whose suspension stands, recomputed from the rows so
//! that a lifted suspension leaves the list.

use crate::support::fixture;
use korpis_app::ports::GrantRead;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

async fn grant(f: &crate::support::Fixture, set: &'static str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id, delegation,
                             issued_by, parent_id)
         VALUES ($1, 'token:00000000-0000-0000-0000-000000000002', '{}', '{}', 'organization',
                 $2, '{}', 'system:bootstrap', $3)",
    )
    .bind(id)
    .bind(f.organization)
    .bind(f.grant)
    .execute(f.store.pool())
    .await
    .unwrap();
    sqlx::query(set).bind(id).execute(f.store.pool()).await.unwrap();
    id
}

#[tokio::test]
async fn the_list_holds_what_stopped_within_the_horizon_and_stands() {
    let f = fixture().await;
    let revoked = grant(&f, "UPDATE grants SET revoked_at = now() WHERE id = $1").await;
    let suspended = grant(&f, "UPDATE grants SET suspended_at = now() WHERE id = $1").await;
    let long_ago =
        grant(&f, "UPDATE grants SET revoked_at = now() - interval '1 hour' WHERE id = $1").await;
    let lifted = grant(&f, "UPDATE grants SET suspended_at = NULL WHERE id = $1").await;
    let live = grant(&f, "SELECT $1::uuid").await;

    let since = SystemTime::now() - Duration::from_secs(600);
    let entries = f.store.revoked_since(since).await.unwrap();
    let mut expected = vec![revoked, suspended];
    expected.sort_unstable();
    assert_eq!(entries, expected, "{long_ago} is past the horizon; {lifted} and {live} stand");

    sqlx::query("UPDATE grants SET suspended_at = NULL WHERE id = $1")
        .bind(suspended)
        .execute(f.store.pool())
        .await
        .unwrap();
    assert_eq!(f.store.revoked_since(since).await.unwrap(), [revoked], "a lifted suspension");
}
