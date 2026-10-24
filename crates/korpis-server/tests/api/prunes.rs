// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pruning through the control plane (§5.3 and §5.5 of docs/06-storage.md), against a real
//! object store: nothing is proposed while every backup is kept or being read; the plan names
//! what it forgets and waits for a person; applied by the control plane, it forgets that and
//! marks what only it held; the sweep waits out the grace and every writer's access, holds the
//! prefix against new backups while it runs, and deletes the marked objects; and the backup kept
//! still restores whole. It needs MinIO (`scripts/test-tools.sh`), and is skipped without it.

use crate::backup_target::serving_with;
use crate::backups::{access, bare, create, ended, get, repository};
use crate::journal::{ack, assignment, complete, plan_status, send, until};
use crate::lifecycle::approve;
use crate::snapshots::{running, snapshotting};
use crate::support::{Harness, mutation, reason};
use korpis_backup::keys::Keys;
use korpis_backup::s3::S3Store;
use korpis_backup::store::ObjectStore as _;
use korpis_backup::testing::{Minio, SECRET, USER};
use korpis_backup::{Content, NoContent, Options, Repo, Restore, backup, restore, verify};
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The first-party S3 target over a MinIO bucket: every job's access a temporary credential
/// scoped to its workload's prefix, and the prune's listing and deletion its own.
pub(crate) fn real(minio: &Minio) -> korpis_target_s3::S3Target {
    let endpoint: url::Url = minio.endpoint.parse().unwrap();
    let sts = korpis_target_s3::sts::Sts {
        endpoint: endpoint.clone(),
        region: "us-east-1".into(),
        access_key_id: USER.into(),
        secret: SECRET.into(),
        role_arn: None,
        http: reqwest::Client::new(),
    };
    let place = korpis_target_s3::Place {
        endpoint,
        region: "us-east-1".into(),
        bucket: "backups".into(),
        root: String::new(),
        path_style: true,
        access_key_id: USER.into(),
        secret: SECRET.into(),
    };
    korpis_target_s3::S3Target::new(place, sts).unwrap()
}

pub(crate) fn repo_of(g: &korpis_proto::korpis::agent::v1::JobGrant) -> Repo<S3Store> {
    let s = g.store.as_option().unwrap();
    let config = korpis_backup::s3::S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: s.key_prefix.clone(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
        certificate: s.certificate.clone(),
    };
    let key: [u8; 32] = g.data_key.as_slice().try_into().unwrap();
    Repo::new(Arc::new(S3Store::new(config).unwrap()), Arc::new(Keys::derive(&key)))
}

/// A running workload, a repository keeping daily backups, and the volume's data at `source`.
struct World {
    h: Harness,
    s: crate::agent::Stream,
    workload: String,
    volume: String,
    repo: String,
    next: u64,
}

/// Backs `source` up as the node would, with `parent`; the backup and the repository its job
/// was given.
async fn backed_up(
    w: &mut World,
    key: &str,
    source: &Path,
    parent: Option<&str>,
) -> (String, Repo<S3Store>) {
    let backups = w.h.backups(w.h.root());
    let made = backups.create_backup(create(key, &w.volume, &w.repo)).await;
    let id = made.unwrap().into_owned().backup.into_option().unwrap().id;
    let pick = assignment(|a| {
        a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == "backup_volume"))
    });
    let plan = until(&mut w.s, pick).await.plan.into_option().unwrap();
    let Outcome::Grant(g) = access(&mut w.s, key, &plan.plan, plan.steps[0].index).await else {
        panic!("refused")
    };
    let repo = repo_of(&g);
    let options = Options {
        source: source.to_owned(),
        consistent: true,
        parent: parent.map(bare),
        content: Content::default(),
        concurrency: 4,
        image_filesystem: String::new(),
    };
    backup(&repo, &bare(&id), options).await.unwrap();
    send(&mut w.s, vec![ended(w.next, &w.workload, &bare(&id), true)]).await;
    assert_eq!(until(&mut w.s, ack).await, w.next);
    w.next = complete(&mut w.s, &w.workload, &plan, w.next + 1).await + 1;
    (id, repo)
}

fn prune(key: &str, w: &World) -> pb::PruneBackupsRequest {
    pb::PruneBackupsRequest {
        mutation: mutation(key).into(),
        workload: w.workload.clone(),
        repository: w.repo.clone(),
        ..Default::default()
    }
}

/// Runs `statement` as the database's owner: moving the times the control plane recorded back
/// is how a test lets the hours pass, which the application role may not do.
pub(crate) async fn sql(h: &Harness, statement: &str) {
    use sqlx::Connection as _;
    let mut owner = sqlx::PgConnection::connect(&h.db.owner_url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(statement.to_owned())).execute(&mut owner).await.unwrap();
    owner.close().await.unwrap();
}

async fn scalar(h: &Harness, query: &str) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_owned())).fetch_one(store.pool()).await.unwrap()
}

/// Waits until `query` counts something.
async fn eventually(h: &Harness, query: &str) {
    for _ in 0..300 {
        if scalar(h, query).await > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never: {query}");
}

/// Two backups of the volume, the second after a file went: the daily policy keeps the second,
/// the newest of its day, and forgets the first. The repository the second's job was given.
async fn two_backups(w: &mut World, source: &Path) -> (String, String, Repo<S3Store>) {
    std::fs::create_dir_all(source.join("world")).unwrap();
    std::fs::write(source.join("server.properties"), b"motd=hello\n").unwrap();
    let noise: Vec<u8> = (0..2_000_000u32).map(|i| u8::try_from(i * 7 % 251).unwrap()).collect();
    std::fs::write(source.join("world/old.dat"), noise).unwrap();
    let (b1, _) = backed_up(w, "b1", source, None).await;
    std::fs::remove_file(source.join("world/old.dat")).unwrap();
    std::fs::write(source.join("server.properties"), b"motd=changed\n").unwrap();
    let (b2, repo) = backed_up(w, "b2", source, Some(&b1)).await;
    (b1, b2, repo)
}

/// The plan proposed once nobody is reading the first backup: it forgets that one alone, and
/// waits for a person.
async fn proposed(w: &World, b1: &str) -> pb::Plan {
    use pb::__buffa::oneof::value::Kind;
    let browse = pb::BrowseBackupRequest { backup: b1.into(), ..Default::default() };
    w.h.backups(w.h.root()).browse_backup(browse).await.unwrap();
    let e = w.h.backups(w.h.root()).prune_backups(prune("p1", w)).await.unwrap_err();
    assert_eq!(reason(&e), "prune.nothing", "the first is being read, the second is kept");
    let read_ends = "UPDATE backup_access SET expires_at = now() - interval '1 second'
                      WHERE mode = 'read'";
    sql(&w.h, read_ends).await;
    let made = w.h.backups(w.h.root()).prune_backups(prune("p2", w)).await.unwrap();
    let plan = made.into_owned().plan.into_option().unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_PRUNE_BACKUPS));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    assert_eq!(plan.to_repository.as_deref(), Some(w.repo.as_str()));
    let forget = match plan.steps[0].detail.get("forget").and_then(|v| v.kind.as_ref()) {
        Some(Kind::ListValue(l)) => l.values.clone(),
        other => panic!("{other:?}"),
    };
    let names: Vec<_> = forget.iter().map(|v| v.kind.clone()).collect();
    assert_eq!(names, [Some(Kind::StringValue(bare(b1)))]);
    plan
}

/// The sweep, once the grace has passed: it waits for the backup jobs' write access to end
/// without spending an attempt, then deletes.
async fn swept(w: &World) {
    sql(&w.h, "UPDATE prune_mark SET marked_at = marked_at - interval '14 hours'").await;
    sql(&w.h, "UPDATE job SET run_after = now() WHERE kind = 'backup_sweep'").await;
    eventually(
        &w.h,
        "SELECT count(*) FROM job WHERE kind = 'backup_sweep' AND status = 'pending'
            AND attempts = 0 AND run_after > now() + interval '11 hours'",
    )
    .await;
    sql(&w.h, "UPDATE backup_access SET expires_at = now() - interval '1 second'").await;
    sql(&w.h, "UPDATE job SET run_after = now() WHERE kind = 'backup_sweep'").await;
    eventually(
        &w.h,
        "SELECT count(*) FROM job WHERE kind = 'backup_sweep' AND status = 'succeeded'",
    )
    .await;
}

/// A backup is not proposed while a sweep holds the prefix.
async fn held_against_backups(w: &World) {
    sql(&w.h, "UPDATE workload_key SET sweeping_job = gen_random_uuid()").await;
    let e = w.h.backups(w.h.root()).create_backup(create("held", &w.volume, &w.repo)).await;
    assert_eq!(reason(&e.unwrap_err()), "backup.busy");
    sql(&w.h, "UPDATE workload_key SET sweeping_job = NULL").await;
}

#[tokio::test]
async fn a_prune_forgets_and_marks_and_after_its_grace_sweeps_leaving_what_is_kept_whole() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let (h, s, workload, volume, next) = running(vec![snapshotting()]).await;
    serving_with(&h, real(&minio)).await;
    let repo = repository(&h).await;
    let mut w = World { h, s, workload, volume, repo, next };
    let (b1, b2, store) = two_backups(&mut w, &scratch.path().join("volume")).await;
    let plan = proposed(&w, &b1).await;
    w.h.plans(w.h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    for _ in 0..300 {
        if plan_status(&w.h, &plan.id).await == Some(pb::PlanStatus::PLAN_STATUS_APPLIED) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(plan_status(&w.h, &plan.id).await, Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
    assert!(get(&w.h, &b1).await.deleted_at.as_option().is_some(), "the first is forgotten");
    assert!(get(&w.h, &b2).await.deleted_at.as_option().is_none(), "the second is kept");
    let record = format!("SELECT count(*) FROM prune_mark WHERE name = 'backups/{}'", bare(&b1));
    assert_eq!(scalar(&w.h, &record).await, 1, "the forgotten record is marked");
    assert!(scalar(&w.h, "SELECT count(*) FROM prune_mark WHERE name LIKE 'chunks/%'").await > 0);
    held_against_backups(&w).await;
    swept(&w).await;

    let left = store.store().list("backups/").await.unwrap();
    assert_eq!(left, [format!("backups/{}", bare(&b2))]);
    assert_eq!(scalar(&w.h, "SELECT count(*) FROM prune_mark").await, 0);
    let into = scratch.path().join("restored");
    std::fs::create_dir_all(&into).unwrap();
    let r = Restore {
        backup: bare(&b2),
        path: String::new(),
        target: into.clone(),
        content: Arc::new(NoContent),
        concurrency: 4,
    };
    restore(&store, r).await.unwrap();
    verify(&store, &bare(&b2), "", into).await.unwrap();
}
