// SPDX-License-Identifier: AGPL-3.0-or-later

//! A path of a block device's backup restored into a new directory of its volume, against a real
//! control plane (§5.4 of docs/06-storage.md), with the in-memory runtime as a driver whose
//! volumes are block devices and the in-memory helper: the plan stages the backup's image while
//! the workload runs, stops it, has its driver copy the path from the image into the volume, and
//! starts it again; the staging is the helper's reading restore and the copy the driver's restore,
//! naming the image, the path, and the new directory; and a block device's whole backup is never
//! restored into a directory.

use crate::backup::{applied, backed_up, serve_target, settled, start};
use crate::backup_read::blocks;
use crate::support::{ControlPlane, mutation, push_paper_on_microvm};
use korpis_agent::host::InMemoryHost;
use korpis_agent::runtime::memory::InMemory;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::restore_backup_request::Target;
use std::sync::Arc;

/// What a node says whose drivers take their volumes as block devices: the one it has for
/// containers, which the backup target's extension runs on, and one that runs microVMs.
fn microvms() -> korpis_agent::session::Facts {
    let mut f = blocks();
    let mut microvm = f.drivers.first().cloned().unwrap_or_default();
    microvm.name = "firecracker".into();
    let mut caps = microvm.capabilities.as_option().cloned().unwrap_or_default();
    caps.tiers = vec![pb::IsolationTier::ISOLATION_TIER_MICROVM.into()];
    microvm.capabilities = caps.into();
    f.drivers.push(microvm);
    f
}

/// Restores `path` of backup `id` into the new directory `into`; the plan, or why not.
async fn restore(
    cp: &ControlPlane,
    key: &str,
    (id, path): (&str, &str),
    into: &str,
) -> Result<pb::Plan, connectrpc::ConnectError> {
    let r = pb::RestoreBackupRequest {
        mutation: mutation(key).into(),
        backup: id.into(),
        path: path.into(),
        target: Some(Target::Into(into.into())),
        ..Default::default()
    };
    let made = cp.client(pb::BackupServiceClient::new).restore_backup(r).await?;
    Ok(made.into_owned().plan.into_option().unwrap())
}

#[tokio::test]
async fn a_path_of_a_block_device_s_backup_is_copied_in_a_guest_while_the_workload_is_stopped() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::named("firecracker", "blk"), InMemoryHost::new());
    runtime.block_devices(true);
    let (agent, root) = start(&cp, dir.path(), (&runtime, &host), Arc::new(microvms)).await;
    serve_target(&cp, &project, &root).await;
    host.offer_content(&format!("sha256:{}", "1".repeat(64)), b"a jar");
    let w = cp.declare_from(&project, "mc", push_paper_on_microvm().await, Vec::new()).await;
    let (backup, backing_up) = backed_up(&cp, &project).await;
    assert_eq!(settled(&cp, &backup).await.image_filesystem, "ext4");
    // The backup settles before its plan closes, and a restore is refused while a plan is open
    // on the volume.
    applied(&cp, &backing_up).await;

    let whole = restore(&cp, "whole", (&backup, ""), "restored").await.unwrap_err();
    assert!(whole.message.unwrap_or_default().contains("one path"), "all of it is a replace");

    let plan = restore(&cp, "path", (&backup, "saves/world"), "restored").await.unwrap();
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["restore_volume", "stop", "apply_restore", "start"], "{plan:?}");
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        let approve = pb::ApprovePlanRequest {
            mutation: mutation("approve").into(),
            plan: plan.id.clone(),
            ..Default::default()
        };
        cp.client(pb::PlanServiceClient::new).approve_plan(approve).await.unwrap();
    }
    applied(&cp, &plan.id).await;

    let staged = host.started_jobs().into_iter().find(|j| j.starts_with("rst-")).unwrap();
    let Some(Work::Restore(work)) = host.started(&staged).unwrap().work else { panic!() };
    assert!(work.reading && work.path.is_empty() && work.into.is_empty(), "staged: {work:?}");
    let image = format!("/memory/.{}.read+{staged}/block.ext4", work.volume);
    let copied = format!("restore {image} saves/world /data/restored ext4");
    let calls = runtime.calls();
    assert!(calls.contains(&copied), "copied by the driver from the image: {calls:?}");
    // The recipe's own stop, its pre-stop command, or the driver's.
    let stopped =
        calls.iter().position(|c| c.ends_with("pre_stop stop\n") || c.starts_with("stop "));
    let copying = calls.iter().position(|c| c.starts_with("start_install apl-"));
    assert!(stopped.is_some() && stopped < copying, "the workload stopped first: {calls:?}");
    let running =
        runtime.objects().into_iter().any(|(o, (of, run))| of == w && run && !o.is_empty());
    assert!(running, "and started again");
    agent.stop().await;
}
