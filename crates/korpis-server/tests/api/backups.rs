// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backups through the control plane (§5 of docs/06-storage.md): a plan of one step that applies
//! at once, under `backup.create` on the workload; one open per workload and repository; the
//! node holding the volume given the workload's data key, the same each time, and access to the
//! workload's prefix alone, while nobody else is; the job's report making the backup whole; the
//! next backup naming it as its parent; and a backup whose plan ended without it reported failed.

use crate::agent::Stream;
use crate::backup_target::{Bucket, serving};
use crate::journal::{ack, assignment, complete, send, until};
use crate::snapshots::{running, snapshotting};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::backup::v1 as backup;
use korpis_proto::korpis::v1 as pb;

pub(crate) async fn repository(h: &Harness) -> String {
    let r = pb::CreateRepositoryRequest {
        mutation: mutation("repo").into(),
        organization: h.root_org(),
        name: "nightly".into(),
        provider: "local".into(),
        prefix: "korpis/".into(),
        retention: pb::RetentionPolicy { keep_daily: 7, ..Default::default() }.into(),
        ..Default::default()
    };
    let made = h.repositories(h.root()).create_repository(r).await.unwrap().into_owned();
    made.repository.into_option().unwrap().id
}

pub(crate) fn create(key: &str, volume: &str, repository: &str) -> pb::CreateBackupRequest {
    pb::CreateBackupRequest {
        mutation: mutation(key).into(),
        volume: volume.to_owned(),
        repository: repository.to_owned(),
        ..Default::default()
    }
}

/// Asks for step `step` of `plan`'s job access; the outcome.
pub(crate) async fn access(s: &mut Stream, request: &str, plan: &str, step: u32) -> Outcome {
    let up = agent::JobAccessRequest {
        request: request.into(),
        plan: plan.into(),
        step,
        ..Default::default()
    };
    s.send(agent::AgentMessage {
        message: Some(Up::JobAccessRequest(Box::new(up))),
        ..Default::default()
    })
    .await
    .unwrap();
    let pick = |d: &Down| match d {
        Down::JobAccess(a) if a.request == request => a.outcome.clone(),
        _ => None,
    };
    until(s, pick).await
}

pub(crate) fn ended(seq: u64, workload: &str, backup: &str, ok: bool) -> agent::JournalRecord {
    let stats = backup::BackupStats { files: 3, bytes: 1 << 20, ..Default::default() };
    let record = backup::Backup {
        id: backup.into(),
        consistent: true,
        stats: stats.into(),
        ..Default::default()
    };
    let mut result = backup::JobResult { ok, ..Default::default() };
    if ok {
        result.backup = record.into();
    } else {
        result.error = "the store refused".into();
    }
    let event = agent::BackupEvent {
        workload: workload.into(),
        backup: backup.into(),
        result: result.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Backup(Box::new(event))),
        count: 1,
        ..Default::default()
    }
}

pub(crate) fn bare(id: &str) -> String {
    korpis_core::id::Id::parse(korpis_core::id::IdKind::Backup, id).unwrap().uuid().to_string()
}

pub(crate) async fn get(h: &Harness, id: &str) -> pb::Backup {
    let r = pb::GetBackupRequest { id: id.into(), ..Default::default() };
    h.backups(h.root()).get_backup(r).await.unwrap().into_owned().backup.into_option().unwrap()
}

/// Checks what the node holding the volume is given for step `step` of `plan`: the workload's
/// one key, each time, and access to its prefix alone; and nothing for a step that is no job.
async fn granted(stream: &mut Stream, plan: &str, step: u32, workload: &str) {
    let Outcome::Grant(g) = access(stream, "r1", plan, step).await else { panic!("refused") };
    assert_eq!(g.data_key.len(), 32);
    let store = g.store.into_option().unwrap();
    let workload_uuid =
        korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap().uuid();
    assert_eq!(
        store.key_prefix,
        format!("repo/korpis/workloads/{workload_uuid}/"),
        "its prefix alone"
    );
    let content = g.content_store.into_option().expect("the repository's recipe content");
    assert_eq!(content.key_prefix, "repo/korpis/content/", "once per repository, beside workloads");
    let Outcome::Grant(again) = access(stream, "r2", plan, step).await else { panic!("refused") };
    assert_eq!(again.data_key, g.data_key, "the workload's one key");
    let Outcome::Refused(not_a_job) = access(stream, "r3", plan, 7).await else {
        panic!("granted")
    };
    assert_eq!(not_a_job.reason, "job.not_a_job");
}

#[tokio::test]
async fn a_backup_is_granted_its_key_and_access_and_made_whole_by_its_report() {
    let (h, mut stream, workload, volume, next) = running(vec![snapshotting()]).await;
    serving(&h, Bucket::default()).await;
    let repo = repository(&h).await;
    let made =
        h.backups(h.root()).create_backup(create("b1", &volume, &repo)).await.unwrap().into_owned();
    let plan = made.plan.into_option().unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_BACKUP_VOLUME));
    assert_ne!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "at once");
    let id = made.backup.into_option().unwrap().id;
    assert_eq!(get(&h, &id).await.status.as_known(), Some(pb::BackupStatus::BACKUP_STATUS_PENDING));
    let busy = h.backups(h.root()).create_backup(create("b2", &volume, &repo)).await.unwrap_err();
    assert_eq!(reason(&busy), "backup.busy", "one writer per workload prefix");

    let told = until(
        &mut stream,
        assignment(|a| {
            a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == "backup_volume"))
        }),
    )
    .await;
    let node_plan = told.plan.into_option().unwrap();
    let step = node_plan.steps[0].clone();
    assert!(matches!(
        step.detail.get("consistent").and_then(|v| v.kind.as_ref()),
        Some(pb::__buffa::oneof::value::Kind::BoolValue(true))
    ));
    granted(&mut stream, &node_plan.plan, step.index, &workload).await;

    send(&mut stream, vec![ended(next, &workload, &bare(&id), true)]).await;
    assert_eq!(until(&mut stream, ack).await, next);
    complete(&mut stream, &workload, &node_plan, next + 1).await;
    let whole = get(&h, &id).await;
    assert_eq!(whole.status.as_known(), Some(pb::BackupStatus::BACKUP_STATUS_SUCCEEDED));
    assert!(whole.consistent && whole.files == 3, "{whole:?}");
    let Outcome::Refused(after) = access(&mut stream, "r4", &node_plan.plan, step.index).await
    else {
        panic!("granted")
    };
    assert!(matches!(after.reason.as_str(), "job.ended" | "job.not_assigned"), "{}", after.reason);

    let second =
        h.backups(h.root()).create_backup(create("b3", &volume, &repo)).await.unwrap().into_owned();
    let second = get(&h, &second.backup.into_option().unwrap().id).await;
    assert_eq!(second.parent.as_deref(), Some(id.as_str()), "the last whole backup is the parent");
    let listed = pb::ListBackupsRequest { workload: workload.clone(), ..Default::default() };
    let all = h.backups(h.root()).list_backups(listed).await.unwrap().into_owned().backups;
    assert_eq!(
        all.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        [second.id.as_str(), id.as_str()],
        "newest first"
    );
}

#[tokio::test]
async fn a_backup_whose_plan_ended_without_it_is_failed_and_one_reported_failed_is() {
    let (h, mut stream, workload, volume, next) = running(Vec::new()).await;
    serving(&h, Bucket::default()).await;
    let repo = repository(&h).await;
    let made =
        h.backups(h.root()).create_backup(create("b1", &volume, &repo)).await.unwrap().into_owned();
    let id = made.backup.into_option().unwrap().id;
    let told = until(&mut stream, assignment(|a| a.plan.is_set())).await;
    let step = &told.plan.as_option().unwrap().steps[0];
    assert!(
        matches!(
            step.detail.get("consistent").and_then(|v| v.kind.as_ref()),
            Some(pb::__buffa::oneof::value::Kind::BoolValue(false))
        ),
        "a class that cannot snapshot is read live"
    );
    send(&mut stream, vec![ended(next, &workload, &bare(&id), false)]).await;
    assert_eq!(until(&mut stream, ack).await, next);
    let failed = get(&h, &id).await;
    assert_eq!(failed.status.as_known(), Some(pb::BackupStatus::BACKUP_STATUS_FAILED));
    assert_eq!(failed.error, "the store refused");

    let plan = made.plan.into_option().unwrap().id;
    let cancel =
        pb::CancelPlanRequest { mutation: mutation("c").into(), plan, ..Default::default() };
    let _ = h.plans(h.root()).cancel_plan(cancel).await;
    let other =
        h.backups(h.root()).create_backup(create("b2", &volume, &repo)).await.unwrap().into_owned();
    let p2 = other.plan.into_option().unwrap().id;
    let cancel =
        pb::CancelPlanRequest { mutation: mutation("c2").into(), plan: p2, ..Default::default() };
    h.plans(h.root()).cancel_plan(cancel).await.unwrap();
    let b2 = get(&h, &other.backup.into_option().unwrap().id).await;
    assert_eq!(b2.status.as_known(), Some(pb::BackupStatus::BACKUP_STATUS_FAILED));
    assert_eq!(b2.error, "its plan ended without it");
}
