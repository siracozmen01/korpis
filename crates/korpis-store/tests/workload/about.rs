// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an effect is about (§4 of docs/15-observability.md, migration 0104): the store resolves
//! the organization and project that hold its object as it writes it, and an organization's
//! subtree, or one project, is asked for by them.

use crate::support::{create, fixture};
use korpis_app::ports::{EffectFilter, EffectRead, EffectWrite, Store, Tx};
use korpis_core::audit::{Effect, Outcome};
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::principal::{Principal, SystemActor};
use uuid::Uuid;

fn effect(action: &str) -> Effect {
    Effect::new(Principal::System(SystemActor::Reconciler), action, Outcome::Succeeded)
}

/// The actions of the effects `filter` finds, in log order.
async fn found(store: &korpis_store::PgStore, filter: &EffectFilter) -> Vec<String> {
    let found = store.effects(filter, None, 100).await.unwrap();
    found.into_iter().map(|e| e.action).collect()
}

#[tokio::test]
async fn an_organization_reads_its_subtrees_effects_by_what_they_are_about() {
    let f = fixture().await;
    let (child, sibling) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, name, parent) in
        [(child, "child", f.organization), (sibling, "sibling", f.organization)]
    {
        sqlx::query("INSERT INTO organization (id, name, parent_id) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(name)
            .bind(parent)
            .execute(f.store.pool())
            .await
            .unwrap();
    }
    let (w, _) = create(&f, "survival").await;
    let mut tx = f.store.begin().await.unwrap();
    let mut on_workload = effect("workload.restart");
    on_workload.workload = Some(w.id);
    tx.append_effect(&on_workload).await.unwrap();
    tx.append_effect(&effect("org.theme.set").about(ObjectRef::new(Kind::Organization, child)))
        .await
        .unwrap();
    let elsewhere = effect("org.theme.set").about(ObjectRef::new(Kind::Organization, sibling));
    tx.append_effect(&elsewhere).await.unwrap();
    tx.append_effect(&effect("root.recovered")).await.unwrap();
    tx.commit().await.unwrap();

    let all = found(&f.store, &EffectFilter::default()).await;
    assert_eq!(all.len(), 4, "{all:?}");
    let child_only = EffectFilter { organization: Some(child), ..EffectFilter::default() };
    assert_eq!(found(&f.store, &child_only).await, ["org.theme.set"]);
    let whole = EffectFilter { organization: Some(f.organization), ..EffectFilter::default() };
    let whole = found(&f.store, &whole).await;
    assert_eq!(whole, ["workload.restart", "org.theme.set", "org.theme.set"], "not root.recovered");
    let project = EffectFilter { project: Some(f.project), ..EffectFilter::default() };
    assert_eq!(found(&f.store, &project).await, ["workload.restart"]);

    let read = f.store.effects(&EffectFilter::default(), None, 100).await.unwrap();
    let restart = read.iter().find(|e| e.action == "workload.restart").unwrap();
    assert_eq!(restart.object, Some(ObjectRef::new(Kind::Workload, w.id)), "its workload's");
    assert_eq!(restart.organization, Some(f.organization));
    let nothing = read.iter().find(|e| e.action == "root.recovered").unwrap();
    assert_eq!((nothing.object, nothing.organization), (None, None), "about nothing");
}
