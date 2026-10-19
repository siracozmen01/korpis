// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nightly backup of trace §20 of docs/23-walkthroughs.md, step 2 as built: a `scheduled`
//! workload whose run calls `korpis backup create` with the workload's own credential, under
//! the grants issued to it and nothing else (`backup.create` on the world, `repository.read` on
//! the repository), with `concurrency: forbid` (§4 of docs/05-scheduling.md). The command is the
//! static `korpis` binary alone in its image; it finds the identity socket its node mounts into
//! the run's container and reaches the control plane through the relay the node serves it,
//! because the workload holds a grant (§9.5 of docs/08-identity.md). The schedule is every
//! minute rather than 03:00, so the test waits one. It needs root, containerd, and `mkfs.btrfs`.

use crate::backups::nightly;
use crate::fs_target::{installed, registered};
use crate::image::{KORPIS, push_image};
use crate::node::{Node, live};
use crate::recipes::{VERSION, push, push_sleeper, test_registry};
use crate::snapshots::lines;
use crate::support::{ControlPlane, mutation};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope::Kind as Scope;
use korpis_proto::korpis::v1::__buffa::oneof::stream_spec::Retention;
use std::time::Duration;

/// A recipe whose run takes a backup of the volume it is configured with into the repository it
/// is configured with, as the workload, and ends.
fn backer(image: &str) -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: backer, version: "{VERSION}", description: takes a backup, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{image}"}}
  entry:
    command: [{entry}, backup, create]
preset:
  lifecycle: scheduled
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 100m, recommended: 100m}}
config:
  fields:
    - {{key: volume, label: Volume, type: string, deliver: arg, deliver_as: "{{}}"}}
    - {{key: repository, label: Repository, type: string, deliver: arg,
       deliver_as: "--repository {{}}"}}
"#,
        entry = KORPIS.entry(),
    )
}

fn text(s: &str) -> pb::Value {
    pb::Value {
        kind: Some(pb::__buffa::oneof::value::Kind::StringValue(s.to_owned())),
        ..Default::default()
    }
}

/// Issues `actions` on `scope` to workload `w`.
async fn granted(cp: &ControlPlane, w: &str, actions: &[&str], scope: Scope) {
    let subject = pb::Subject {
        kind: Some(pb::__buffa::oneof::subject::Kind::Workload(w.to_owned())),
        ..Default::default()
    };
    let scope = pb::Scope { kind: Some(scope), ..Default::default() };
    pb::GrantServiceClient::new(cp.http(), cp.config())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(&format!("to-{w}-{}", actions.join("-"))).into(),
            subject: subject.into(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            scope: scope.into(),
            ..Default::default()
        })
        .await
        .unwrap();
}

/// Waits up to three minutes, a schedule's minute and a backup, for `sql` to count at least one.
async fn some(cp: &ControlPlane, sql: &'static str) {
    for _ in 0..900 {
        if cp.scalar::<i64>(sql).await > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let runs = "SELECT coalesce((SELECT string_agg(status || ' ' || coalesce(cause, '-') || ' '
                                     || coalesce(exit_status::text, '-'), '; ') FROM run), '')";
    let failed = "SELECT coalesce((SELECT string_agg(action || ' ' || coalesce(error::text, ''),
                                                     '; ' ORDER BY at)
                                     FROM effect WHERE outcome <> 'succeeded'), '')
                  || ' | plans ' || coalesce((SELECT string_agg(status, ' ') FROM plan), '')
                  || ' | unplaced ' || coalesce((SELECT string_agg(row_to_json(f)::text, ' ')
                                                  FROM placement_failure f), '')";
    let (runs, failed) = (cp.scalar::<String>(runs).await, cp.scalar::<String>(failed).await);
    panic!("{sql} counted none; the runs: {runs}; what failed: {failed}");
}

#[tokio::test]
async fn a_scheduled_run_takes_a_backup_with_its_own_credential_under_its_own_grants() {
    if !live() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Its image before any node runs: building it under a node would starve its lease.
    let image = push_image(&test_registry(), KORPIS).await;
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_on_btrfs(&cp, "disk-2").await else { return };
    let agent = node.agent().await;
    let target = installed(&cp, &project).await;
    node.guard(&target);
    registered(&cp).await;
    let repo = nightly(&cp.cli(None), "fs").await;
    let world = cp.declare(&project, "world", &push_sleeper().await).await;
    node.guard(&world);
    lines(&node.volumes.join(format!("{world}.data")).join("starts"), 1).await;
    let data = cp.volume_named(&project, "data").await;

    // Each kept run's console counts against the disk quota at its stream's bound (K-3), so a
    // nightly job keeps few runs and a small console, as the test node's small disk needs.
    let policy = pb::RunPolicy {
        concurrency: pb::Concurrency::CONCURRENCY_FORBID.into(),
        keep_successful: Some(1),
        keep_failed: Some(1),
        ..Default::default()
    };
    let console = pb::StreamSpec {
        retention: Some(Retention::RetentionBytes(8 * 1024 * 1024)),
        ..Default::default()
    };
    let body = pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_SCHEDULED.into(),
        schedule: "* * * * *".into(),
        timezone: "UTC".into(),
        runs: policy.into(),
        streams: console.into(),
        config: [("volume", text(&data)), ("repository", text(&repo))]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        ..Default::default()
    };
    let recipe = push("backer", backer(&image)).await;
    let backer = cp.declare_body(&project, "backer", &recipe, body).await;
    node.guard(&backer);
    granted(&cp, &backer, &["backup.create"], Scope::Workload(world.clone())).await;
    granted(&cp, &backer, &["repository.read"], Scope::Repository(repo.clone())).await;

    // At the next minute its run is made, calls the command, and the backup is taken whole.
    some(&cp, "SELECT count(*) FROM backup WHERE status = 'succeeded'").await;
    some(&cp, "SELECT count(*) FROM run WHERE status = 'succeeded' AND exit_status = 0").await;
    // Proposed by the workload itself, under its own grants.
    let id = Id::parse(IdKind::Workload, &backer).unwrap().uuid().to_string();
    let by: String = cp
        .scalar("SELECT string_agg(computed_by::text, ' ') FROM plan WHERE to_volume IS NOT NULL")
        .await;
    assert!(by.contains(&id), "the backup's plan was the workload's: {by}");
    // Never two of its runs at once: forbid keeps one running.
    let overlapping: i64 = cp
        .scalar(
            "SELECT count(*) FROM run a JOIN run b ON a.workload_id = b.workload_id
              AND a.id < b.id AND a.started_at IS NOT NULL AND b.started_at IS NOT NULL
              AND b.started_at < coalesce(a.ended_at, now())",
        )
        .await;
    assert_eq!(overlapping, 0, "no two runs overlapped");
    agent.stop().await;
}
