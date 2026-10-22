// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a quota refusal tells whom (§5.1 of docs/05-scheduling.md, Finding 140 of
//! docs/23-walkthroughs.md): it names the set that refused, with its bound and what would be
//! held, to a caller who may read the set's owner; a customer refused by its reseller's set is
//! told only the dimension, since the reseller's bound and what all its customers hold together
//! are the reseller's own.

use crate::delegation::{Holder, delegate, delegation, organization};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail, org_scope};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

/// A quota entry of `kind` owned by `owner`, bounding the number of workloads, written as the
/// reckoning of a set writes one, so the refusal is reached without the rules of setting it.
async fn workloads(h: &Harness, kind: &str, owner: &str, bound: u64) {
    let id_kind = if kind == "project" { IdKind::Project } else { IdKind::Organization };
    let store = h.db.store().await.unwrap();
    sqlx::query(
        "INSERT INTO quota_entry (id, owner_kind, owner_id, reservations)
         VALUES ($1, $2, $3, $4::jsonb)",
    )
    .bind(Uuid::now_v7())
    .bind(kind)
    .bind(Id::parse(id_kind, owner).unwrap().uuid())
    .bind(format!("{{\"workloads\": {bound}}}"))
    .execute(store.pool())
    .await
    .unwrap();
}

/// What `config` is told when it declares paper `name` in `project`: the refusal's message.
async fn refused(
    h: &Harness,
    config: &Holder,
    (repo, project): (&str, &str),
    name: &str,
) -> String {
    let mut ask = create(name, project, name, body(repo, &[]));
    ask.dry_run = true;
    let e = h.workloads(config.config.clone()).declare_intent(ask).await.unwrap_err();
    let d = detail(&e);
    assert_eq!((d.reason.as_str(), d.field.as_str()), ("quota.exceeded", "workloads"));
    d.message
}

#[tokio::test]
async fn a_refusal_names_the_set_only_to_whoever_may_read_its_owner() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let host = Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() };
    let reseller = organization(&h, &host, &h.root_org(), "northwind").await;
    let sub = organization(&h, &host, &reseller, "pinecone").await;
    let shop = project(&h, &sub, "shop").await;
    workloads(&h, "organization", &reseller, 1).await;
    let acts = ["workload.create", "workload.read", "project.read", "org.read"];
    let ask = (&acts[..], org_scope(&sub));
    let customer = delegate(&h, (&h.root_grant(), h.root()), ask, delegation(None, &[])).await;
    let declared = create("first", &shop, "first", body(&repo, &[]));
    h.workloads(customer.config.clone()).declare_intent(declared).await.unwrap();

    let told = refused(&h, &customer, (&repo, &shop), "second").await;
    assert_eq!(
        told,
        "a quota set you may not read bounds workloads reservations, and this would exceed it"
    );
    let named = refused(&h, &host, (&repo, &shop), "second").await;
    assert_eq!(
        named,
        format!(
            "the quota set of organization {reseller} bounds workloads reservations at 1; this \
             would hold 2"
        ),
        "the host reads the reseller"
    );

    // A set the customer may read, its own project's, is named to it in full.
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE quota_entry SET reservations = '{\"workloads\": 9}'")
        .execute(store.pool())
        .await
        .unwrap();
    let lab = project(&h, &sub, "lab").await;
    workloads(&h, "project", &lab, 0).await;
    let own = refused(&h, &customer, (&repo, &lab), "third").await;
    assert_eq!(
        own,
        format!(
            "the quota set of project {lab} bounds workloads reservations at 0; this would hold 1"
        )
    );
}

/// The holdings a delegated customer is told nothing of are not in the refusal's other fields
/// either: the error carries the dimension and nothing else of the set.
#[tokio::test]
async fn a_refusal_by_a_set_the_caller_may_not_read_carries_no_number() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let host = Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() };
    let reseller = organization(&h, &host, &h.root_org(), "northwind").await;
    let sub = organization(&h, &host, &reseller, "pinecone").await;
    let shop = project(&h, &sub, "shop").await;
    workloads(&h, "organization", &reseller, 0).await;
    let acts = ["workload.create", "project.read"];
    let ask = (&acts[..], org_scope(&sub));
    let customer = delegate(&h, (&h.root_grant(), h.root()), ask, delegation(None, &[])).await;
    let mut ask = create("only", &shop, "only", body(&repo, &[]));
    ask.dry_run = true;
    let e = h.workloads(customer.config.clone()).declare_intent(ask).await.unwrap_err();
    let d: pb::Error = detail(&e);
    for leaked in [reseller.as_str(), "at 0", "would hold"] {
        assert!(!e.message.as_deref().unwrap_or("").contains(leaked), "{leaked} in {e:?}");
        assert!(!d.message.contains(leaked), "{leaked} in {d:?}");
    }
}
