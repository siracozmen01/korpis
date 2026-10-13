// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared sets (§3.3 of docs/01-model.md, §5 and §6 of docs/13-surface-cli.md): a set owns
//! what it created or adopted and touches nothing else; a change made outside it is drift, which
//! advisory mode refuses to overwrite unless asked and strict mode overwrites on the record.

use crate::recipes::{body, change, create, project, push_paper, text};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::v1 as pb;

fn through(mut d: pb::DeclareIntentRequest, set: &str) -> pb::DeclareIntentRequest {
    d.set = set.into();
    d
}

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let request = pb::GetWorkloadRequest { id: w.into(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(request).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

/// Declares `motd` for `w` against its current head, through `set` when one is named.
async fn declare(
    h: &Harness,
    repo: &str,
    w: &str,
    motd: &str,
    set: &str,
    flags: (bool, bool),
) -> Result<pb::DeclareIntentResponse, connectrpc::ConnectError> {
    let head = status(h, w).await.head.into_option().unwrap().version;
    let key = format!("{w}-{head}-{motd}-{set}");
    let mut d = through(change(&key, w, head, body(repo, &[("motd", text(motd))])), set);
    (d.adopt, d.reconcile) = flags;
    h.workloads(h.root()).declare_intent(d).await.map(connectrpc::UnaryResponse::into_owned)
}

#[tokio::test]
async fn a_set_owns_its_members_and_never_loses_a_fix_silently() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let first =
        through(create("c", &project, "mc", body(&repo, &[("motd", text("a"))])), "servers");
    let created = h.workloads(h.root()).declare_intent(first).await.unwrap().into_owned();
    let w = created.workload.as_option().unwrap().clone();
    let set = w.set.clone().unwrap();
    assert!(set.starts_with("set_"));
    assert_eq!(created.intent.as_option().unwrap().managed_by.as_deref(), Some(set.as_str()));

    declare(&h, &repo, &w.id, "b", "", (false, false)).await.unwrap();
    let drift = status(&h, &w.id).await.drift.into_option().expect("declared outside the set");
    assert_eq!(drift.set, set);
    assert!(drift.set_intent.is_some(), "what the files still say");
    assert!(drift.effect.as_deref().is_some_and(|e| e.starts_with("eff_")), "and what changed it");

    let e = declare(&h, &repo, &w.id, "c", "servers", (false, false)).await.unwrap_err();
    assert_eq!(reason(&e), "set.drifted", "advisory never overwrites a fix unasked");
    declare(&h, &repo, &w.id, "b", "servers", (false, false)).await.unwrap();
    assert!(
        status(&h, &w.id).await.drift.as_option().is_none(),
        "the files now say what the fix said"
    );

    declare(&h, &repo, &w.id, "d", "", (false, false)).await.unwrap();
    declare(&h, &repo, &w.id, "e", "servers", (false, true)).await.unwrap();
    assert_eq!(h.effects("set.drift_discarded").await.len(), 1, "reconciled on the record");

    let sets = h
        .sets(h.root())
        .list_declared_sets(pb::ListDeclaredSetsRequest {
            project: project.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned()
        .sets;
    assert_eq!(sets[0].mode.as_known(), Some(pb::DriftMode::DRIFT_MODE_ADVISORY));
    let strict = pb::SetDeclaredSetModeRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("strict") }.into(),
        project: project.clone(),
        name: "servers".into(),
        mode: pb::DriftMode::DRIFT_MODE_STRICT.into(),
        ..Default::default()
    };
    let moded = h.sets(h.root()).set_declared_set_mode(strict).await.unwrap().into_owned();
    assert_eq!(moded.set.as_option().unwrap().version, 2);
    declare(&h, &repo, &w.id, "f", "", (false, false)).await.unwrap();
    declare(&h, &repo, &w.id, "g", "servers", (false, false)).await.unwrap();
    assert_eq!(h.effects("set.drift_discarded").await.len(), 2, "strict reconciles, recorded");

    let e = declare(&h, &repo, &w.id, "h", "other", (true, true)).await.unwrap_err();
    assert_eq!(reason(&e), "set.other_set", "two sets never fight over one workload");
}

#[tokio::test]
async fn a_set_takes_a_loose_workload_only_when_asked() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let loose = create("c", &project, "mc", body(&repo, &[("motd", text("a"))]));
    let created = h.workloads(h.root()).declare_intent(loose).await.unwrap().into_owned();
    let w = created.workload.as_option().unwrap().id.clone();

    let e = declare(&h, &repo, &w, "b", "servers", (false, false)).await.unwrap_err();
    assert_eq!(reason(&e), "set.not_a_member", "hand-made workloads are never pruned by a set");
    let adopted = declare(&h, &repo, &w, "b", "servers", (true, false)).await.unwrap();
    assert!(adopted.workload.as_option().unwrap().set.is_some());

    let members = pb::ListWorkloadsRequest {
        project: Some(project.clone()),
        set: Some("servers".into()),
        ..Default::default()
    };
    let listed = h.workloads(h.root()).list_workloads(members).await.unwrap().into_owned();
    assert_eq!(listed.workloads.len(), 1);
    let none = pb::ListWorkloadsRequest {
        project: Some(project),
        set: Some("nothing".into()),
        ..Default::default()
    };
    let listed = h.workloads(h.root()).list_workloads(none).await.unwrap().into_owned();
    assert!(listed.workloads.is_empty());
}
