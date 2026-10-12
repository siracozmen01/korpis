// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared steps of the plan lifecycle tests.

use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

/// A harness with one workload created by the root token; its wire id.
pub async fn workload(h: &Harness) -> (String, String) {
    let repo = push_paper().await;
    let project = project(h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    (declared.workload.as_option().unwrap().id.clone(), repo)
}

/// The pointers of a workload, straight from the store: (target set, power head, power target).
pub async fn pointers(h: &Harness, workload: &str) -> (bool, i64, i64) {
    let id =
        korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    sqlx::query_as::<_, (Option<Uuid>, i64, i64)>(
        "SELECT target_id, power_version, power_target_version FROM workload WHERE id = $1",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .map(|(t, p, pt)| (t.is_some(), p, pt))
    .unwrap()
}

pub fn power(key: &str, workload: &str, version: u64) -> pb::StopRequest {
    pb::StopRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: Some(version),
            ..Default::default()
        }
        .into(),
        workload: workload.to_owned(),
        ..Default::default()
    }
}

pub fn approve(key: &str, plan: &str) -> pb::ApprovePlanRequest {
    pb::ApprovePlanRequest {
        mutation: mutation(key).into(),
        plan: plan.to_owned(),
        ..Default::default()
    }
}
