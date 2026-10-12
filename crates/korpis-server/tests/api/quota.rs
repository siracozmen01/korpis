// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota held at admission (§5.1 of docs/05-scheduling.md). Entries are written directly:
//! setting quota through the API arrives with quota sets in Phase 2.

use crate::recipes::{body, change, create, project, push_paper, text};
use crate::support::{Harness, detail, reason};
use korpis_core::id::{Id, IdKind};
use uuid::Uuid;

const GIB: i64 = 1 << 30;

fn uuid(kind: IdKind, wire: &str) -> Uuid {
    Id::parse(kind, wire).unwrap().uuid()
}

/// A quota entry owned by `owner`, bounding reservations; returns its id.
async fn entry(h: &Harness, kind: &str, owner: Uuid, selector: Option<&str>, res: &str) -> Uuid {
    let id = Uuid::now_v7();
    let store = h.db.store().await.unwrap();
    sqlx::query(
        "INSERT INTO quota_entry (id, owner_kind, owner_id, selector, reservations)
         VALUES ($1, $2, $3, $4::jsonb, $5::jsonb)",
    )
    .bind(id)
    .bind(kind)
    .bind(owner)
    .bind(selector)
    .bind(res)
    .execute(store.pool())
    .await
    .unwrap();
    id
}

async fn version(h: &Harness, id: Uuid) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT version FROM quota_entry WHERE id = $1")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_project_entry_admits_what_fits_and_refuses_what_does_not() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let bound = format!("{{\"memory_bytes\": {}}}", 6 * GIB);
    let e = entry(&h, "project", uuid(IdKind::Project, &project), None, &bound).await;
    let workloads = h.workloads(h.root());
    let first = workloads
        .declare_intent(create("d1", &project, "a", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    assert_eq!(version(&h, e).await, 2, "consumed in the admitting transaction");
    let mut dry = create("d2", &project, "b", body(&repo, &[]));
    dry.dry_run = true;
    let refused = workloads.declare_intent(dry).await.unwrap_err();
    assert_eq!(reason(&refused), "quota.exceeded", "a dry run shows it too");
    let refused =
        workloads.declare_intent(create("d3", &project, "b", body(&repo, &[]))).await.unwrap_err();
    assert_eq!(
        (reason(&refused), detail(&refused).field),
        ("quota.exceeded".into(), "memory_bytes".into())
    );
    assert_eq!(h.effects("workload.create").await.len(), 1, "the refused one wrote nothing");
    // A change that holds no more passes, even after the bound is lowered under it.
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE quota_entry SET reservations = '{\"memory_bytes\": 1}' WHERE id = $1")
        .bind(e)
        .execute(store.pool())
        .await
        .unwrap();
    let w = first.workload.as_option().unwrap();
    workloads
        .declare_intent(change("d4", &w.id, 1, body(&repo, &[("motd", text("still fits"))])))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_organization_entry_counts_its_whole_subtree() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let root = uuid(IdKind::Organization, &h.root_org());
    entry(&h, "organization", root, None, "{\"workloads\": 1}").await;
    let games = project(&h, &h.root_org(), "games").await;
    let web = project(&h, &h.root_org(), "web").await;
    let workloads = h.workloads(h.root());
    workloads.declare_intent(create("d1", &games, "a", body(&repo, &[]))).await.unwrap();
    let e = workloads.declare_intent(create("d2", &web, "b", body(&repo, &[]))).await.unwrap_err();
    assert_eq!(detail(&e).field, "workloads");
}

#[tokio::test]
async fn a_selector_counts_only_the_workloads_it_matches() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let selector = "{\"match_labels\": {\"tier\": \"gold\"}}";
    entry(&h, "project", uuid(IdKind::Project, &project), Some(selector), "{\"workloads\": 0}")
        .await;
    let workloads = h.workloads(h.root());
    workloads.declare_intent(create("d1", &project, "plain", body(&repo, &[]))).await.unwrap();
    let mut gold = create("d2", &project, "gold", body(&repo, &[]));
    if let Some(korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target::Create(
        n,
    )) = gold.target.as_mut()
    {
        n.labels.insert("tier".into(), "gold".into());
    }
    let e = workloads.declare_intent(gold).await.unwrap_err();
    assert_eq!(reason(&e), "quota.exceeded");
}

#[tokio::test]
async fn two_admissions_racing_for_the_last_unit_admit_exactly_one() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    entry(&h, "project", uuid(IdKind::Project, &project), None, "{\"workloads\": 1}").await;
    let workloads = h.workloads(h.root());
    let (a, b) = tokio::join!(
        workloads.declare_intent(create("a", &project, "a", body(&repo, &[]))),
        workloads.declare_intent(create("b", &project, "b", body(&repo, &[]))),
    );
    let admitted = [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count();
    assert_eq!(admitted, 1, "a quota checked without being held would admit both");
    let refused = a.err().or(b.err()).unwrap();
    assert_eq!(reason(&refused), "quota.exceeded");
}
