// SPDX-License-Identifier: AGPL-3.0-or-later

//! A grant issued to a workload, or revoked or suspended above one a workload holds, announces
//! the workload (§9.5 of docs/08-identity.md), so its node's assignment, which says whether the
//! API is relayed to it, follows at once. A grant to anyone else, and a use, announce nothing.

use crate::support::{Fixture, fixture};
use sqlx::postgres::PgListener;
use std::time::Duration;
use uuid::Uuid;

async fn issue(f: &Fixture, subject: &str, parent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id, delegation,
                             issued_by, parent_id)
         VALUES ($1, $2, '{}', '{}', 'organization', $3, '{}', 'system:bootstrap', $4)",
    )
    .bind(id)
    .bind(subject)
    .bind(f.organization)
    .bind(parent)
    .execute(f.store.pool())
    .await
    .unwrap();
    id
}

async fn set(f: &Fixture, sql: &'static str, id: Uuid) {
    sqlx::query(sql).bind(id).execute(f.store.pool()).await.unwrap();
}

async fn heard(listener: &mut PgListener) -> String {
    let next = tokio::time::timeout(Duration::from_secs(10), listener.recv());
    next.await.expect("an announcement within ten seconds").unwrap().payload().to_owned()
}

#[tokio::test]
async fn a_workload_s_grants_announce_it_and_no_one_else_s_do() {
    let f = fixture().await;
    let mut listener = PgListener::connect_with(f.store.pool()).await.unwrap();
    listener.listen("korpis_workload").await.unwrap();
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());

    // A token's grant and a use announce nothing: the next announcement is the workload's.
    let other = issue(&f, "token:00000000-0000-0000-0000-000000000002", f.grant).await;
    set(&f, "UPDATE grants SET uses = uses + 1 WHERE id = $1", other).await;
    let held = issue(&f, &format!("workload:{first}"), f.grant).await;
    assert_eq!(heard(&mut listener).await, first.to_string());

    // Revoking a grant announces every workload holding one beneath it.
    let _below = issue(&f, &format!("workload:{second}"), held).await;
    assert_eq!(heard(&mut listener).await, second.to_string());
    set(&f, "UPDATE grants SET revoked_at = now() WHERE id = $1", held).await;
    let mut both = [heard(&mut listener).await, heard(&mut listener).await];
    both.sort();
    let mut expected = [first.to_string(), second.to_string()];
    expected.sort();
    assert_eq!(both, expected);

    // Suspending a grant no workload holds anything beneath announces nothing: the next heard
    // is a workload's own issue.
    set(&f, "UPDATE grants SET suspended_at = now() WHERE id = $1", other).await;
    let third = Uuid::now_v7();
    issue(&f, &format!("workload:{third}"), f.grant).await;
    assert_eq!(heard(&mut listener).await, third.to_string());
}
