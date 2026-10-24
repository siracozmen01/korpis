// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's install into a microVM's volume on a real node (§4.4 of docs/04-runtimes.md, §4 of
//! docs/09-recipes.md): the volume is a block device the host never mounts, so the node has the
//! `firecracker` driver run the install in a service guest, before the workload's first start,
//! with what the install reads put on a content drive. Its files are read back through the
//! relay from the running guest, and a new version of the recipe stops it, installs into the
//! stopped guest's volume, and starts it again. Needs what the microVM test needs.

use crate::files::{Files, open};
use crate::install::{JAR, MIB, declaration, declared, document, push, serve_jar};
use crate::node::{Node, live};
use crate::support::{ControlPlane, converged};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::sync::atomic::Ordering;

/// What the shared session helper mints its sessions for.
const WRITE: &str = "workload.file.write";

/// What `path` holds, at most a mebibyte, read in the session `f`.
pub(crate) async fn read(f: &mut Files, path: &str) -> Vec<u8> {
    let read = pb::FileRead { path: path.into(), length: 1 << 20, ..Default::default() };
    let Response::Data(data) = f.ask(File::Read(Box::new(read))).await else {
        panic!("{path} is read");
    };
    data
}

#[tokio::test]
async fn a_recipe_installs_into_a_microvm_s_volume_in_a_service_guest_and_again_when_upgraded() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-install").await else {
        eprintln!("this machine cannot run a guest: not run");
        return;
    };
    let agent = node.agent().await;
    let project = cp.project().await;
    let (url, fetches) = serve_jar().await;
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/vm-installer-{}", uuid::Uuid::now_v7().simple());
    push(&repo, "1.0.0", document("microvm", "1.0.0", &url, "eula=true\\n")).await;

    let new = Target::Create(Box::new(pb::NewWorkload {
        project: project.clone(),
        name: "mc".into(),
        ..Default::default()
    }));
    let first = declaration((&repo, "1.0.0"), 96 * MIB, new, ("d1", None));
    let (w, _) = declared(&cp, first, pb::PlanStatus::PLAN_STATUS_APPLIED).await;
    node.guard(&w);
    converged(&cp, &w).await;
    let editor = cp.cli(Some(&cp.token_for(&[WRITE]).await));
    let mut f = open(&editor, &w).await;
    assert_eq!(read(&mut f, "/data/server.jar").await, JAR, "fetched, and written in the guest");
    assert_eq!(read(&mut f, "/data/eula.txt").await, b"eula=true\n");
    assert_eq!(read(&mut f, "/data/server.properties").await, b"motd=hi\n", "the template");
    assert_eq!(read(&mut f, "/data/pkg/run.sh").await, b"#!/bin/sh\necho ok\n", "extracted");
    drop(f);

    push(&repo, "2.0.0", document("microvm", "2.0.0", &url, "eula=true\\n# v2\\n")).await;
    let upgrade =
        declaration((&repo, "2.0.0"), 96 * MIB, Target::Workload(w.clone()), ("d2", Some(1)));
    let (_, plan) = declared(&cp, upgrade, pb::PlanStatus::PLAN_STATUS_APPLIED).await;
    let steps: Vec<_> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert!(steps.contains(&"install") && steps.contains(&"stop"), "{steps:?}");
    converged(&cp, &w).await;
    let mut f = open(&editor, &w).await;
    assert_eq!(read(&mut f, "/data/eula.txt").await, b"eula=true\n# v2\n", "installed again");
    drop(f);
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "fetched once: the node keeps it by its hash");
    cp.retire(&w).await;
    agent.stop().await;
}
