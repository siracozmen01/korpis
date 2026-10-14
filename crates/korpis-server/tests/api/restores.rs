// SPDX-License-Identifier: AGPL-3.0-or-later

//! Restores through the control plane (§5.4 of docs/06-storage.md): into a new directory of the
//! volume, one step that applies at once; replacing a running workload's volume, staged while it
//! runs and exchanged while it is stopped, waiting for a person because what the volume holds is
//! lost; the node holding the volume given the backup's own key and access to its workload's
//! prefix; only a whole backup restored; and `backup.restore` required to put a workload's backup
//! back into it.

use crate::agent::Stream;
use crate::backup_target::{Bucket, serving};
use crate::backups::{access, bare, create, ended, get, repository};
use crate::journal::{ack, assignment, complete, send, until};
use crate::snapshots::{running, snapshotting};
use crate::support::{Harness, mutation, reason};
use crate::volumes::holding;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::restore_backup_request::Target;

/// The node plan of the next assignment carrying a step `action`.
async fn told(s: &mut Stream, action: &'static str) -> agent::NodePlan {
    let pick = assignment(move |a| {
        a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == action))
    });
    until(s, pick).await.plan.into_option().unwrap()
}

/// A whole backup of the running workload's volume, and the key its job was given; the next
/// journal sequence after it.
async fn whole(
    h: &Harness,
    s: &mut Stream,
    (workload, volume): (&str, &str),
    next: u64,
) -> (String, Vec<u8>, u64) {
    serving(h, Bucket::default()).await;
    let repo = repository(h).await;
    let made =
        h.backups(h.root()).create_backup(create("b", volume, &repo)).await.unwrap().into_owned();
    let id = made.backup.into_option().unwrap().id;
    let plan = told(s, "backup_volume").await;
    let Outcome::Grant(g) = access(s, "w", &plan.plan, plan.steps[0].index).await else {
        panic!("refused")
    };
    send(s, vec![ended(next, workload, &bare(&id), true)]).await;
    assert_eq!(until(s, ack).await, next);
    let last = complete(s, workload, &plan, next + 1).await;
    (id, g.data_key, last + 1)
}

fn restore(key: &str, backup: &str, target: Target) -> pb::RestoreBackupRequest {
    pb::RestoreBackupRequest {
        mutation: mutation(key).into(),
        backup: backup.into(),
        target: Some(target),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_backup_is_restored_into_a_new_directory_at_once_with_its_own_key() {
    let (h, mut stream, workload, volume, next) = running(vec![snapshotting()]).await;
    let (id, key, _) = whole(&h, &mut stream, (&workload, &volume), next).await;
    let into = Target::Into("restored".into());
    let request = pb::RestoreBackupRequest { path: "world".into(), ..restore("r", &id, into) };
    let plan = h
        .backups(h.root())
        .restore_backup(request)
        .await
        .unwrap()
        .into_owned()
        .plan
        .into_option()
        .unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_RESTORE_VOLUME));
    assert_ne!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "loses nothing");
    let node_plan = told(&mut stream, "restore_volume").await;
    assert_eq!(node_plan.steps.len(), 1, "one step, the workload left running");
    let Outcome::Grant(g) =
        access(&mut stream, "r", &node_plan.plan, node_plan.steps[0].index).await
    else {
        panic!("refused")
    };
    assert_eq!(g.data_key, key, "the key the backup was taken with");
    let workload_uuid =
        korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, &workload).unwrap().uuid();
    let store = g.store.into_option().unwrap();
    assert_eq!(store.key_prefix, format!("repo/korpis/workloads/{workload_uuid}/"));

    for (key, path, target, why) in [
        ("p", "world", Target::Replace(true), "path"),
        ("d", "", Target::Into(".hidden".into()), "into"),
        ("s", "", Target::Into("a/b".into()), "into"),
        ("u", "../up", Target::Into("x".into()), "path"),
    ] {
        let bad = pb::RestoreBackupRequest { path: path.into(), ..restore(key, &id, target) };
        let refused = h.backups(h.root()).restore_backup(bad).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidArgument, "{why}: {refused:?}");
    }
    let reader = holding(&h, &["backup.read", "workload.read", "org.read"]).await;
    let e = h
        .backups(reader)
        .restore_backup(restore("n", &id, Target::Into("mine".into())))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "reading a backup is not restoring it");
}

#[tokio::test]
async fn replacing_a_running_volume_is_staged_then_exchanged_and_waits_for_a_person() {
    let (h, mut stream, workload, volume, next) = running(vec![snapshotting()]).await;
    let (id, _, _) = whole(&h, &mut stream, (&workload, &volume), next).await;
    let repo = repository(&h).await;
    let pending =
        h.backups(h.root()).create_backup(create("p", &volume, &repo)).await.unwrap().into_owned();
    let pending = pending.backup.into_option().unwrap().id;
    let e = h
        .backups(h.root())
        .restore_backup(restore("np", &pending, Target::Replace(true)))
        .await
        .unwrap_err();
    assert!(matches!(reason(&e).as_str(), "backup.not_whole" | "volume.busy"), "{e:?}");
    let plan = pending_plan(&h, &workload).await;
    h.plans(h.root())
        .cancel_plan(pb::CancelPlanRequest {
            mutation: mutation("c").into(),
            plan,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        get(&h, &pending).await.status.as_known(),
        Some(pb::BackupStatus::BACKUP_STATUS_FAILED)
    );

    let made = h
        .backups(h.root())
        .restore_backup(restore("r", &id, Target::Replace(true)))
        .await
        .unwrap()
        .into_owned();
    let plan = made.plan.into_option().unwrap();
    assert_eq!(
        plan.status.as_known(),
        Some(pb::PlanStatus::PLAN_STATUS_PROPOSED),
        "what it holds is lost"
    );
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(
        actions,
        ["restore_volume", "stop", "replace_volume", "start"],
        "down only for the exchange"
    );
    let replace = plan.steps.iter().find(|s| s.action == "replace_volume").unwrap();
    assert!(!replace.reversible, "the replacing step is the irreversible one");
    assert!(plan.steps[0].reversible, "staging loses nothing");
}

/// The open plan of `workload` operating on a volume.
async fn pending_plan(h: &Harness, workload: &str) -> String {
    let r = pb::ListPlansRequest { workload: workload.into(), ..Default::default() };
    let plans = h.plans(h.root()).list_plans(r).await.unwrap().into_owned().plans;
    let open = |p: &&pb::Plan| {
        p.kind.as_known() == Some(pb::PlanKind::PLAN_KIND_BACKUP_VOLUME)
            && p.status.as_known() != Some(pb::PlanStatus::PLAN_STATUS_APPLIED)
    };
    plans.iter().find(open).unwrap().id.clone()
}
