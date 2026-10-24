// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's files on a real node (§4.4 of docs/04-runtimes.md, §3.1 of
//! docs/06-storage.md): its volume is a block device the host never mounts, so a file session
//! reaches it through the relay, the node, and the `firecracker` driver to the file worker inside
//! the guest. The volume is listed as the guest mounts it, written, and read back; and OpenSSH's
//! `sftp` puts a file there and gets it back whole, served the same way. Stopped, its volume is
//! served by a service guest: what was written while it ran is read, a file is written, and the
//! workload started again finds it. Needs what the microVM test needs, and OpenSSH's client for
//! the sftp half.

use crate::files::open;
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::sftp::{have, person_with_key, run, serve_sftp};
use crate::support::{ControlPlane, converged, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;

const WRITE: &str = "workload.file.write";

/// A guest that only sleeps, with a volume at `/data`.
pub(crate) fn recipe() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: guest-files, version: "{VERSION}", description: a guest, license: MIT}}
runtime:
  targets:
    - {{tier: microvm, arch: [amd64], image: "{busybox}"}}
  entry:
    command: [/bin/sh, -c, "exec sleep 100000"]
preset:
  lifecycle: persistent
  console: logs
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 500m}}
"#
    )
}

/// Stops `w`, or starts it again if `start`, at its power's `version`, and waits until it has.
pub(crate) async fn power(cp: &ControlPlane, w: &str, start: bool, version: u64) {
    let client = pb::WorkloadServiceClient::new(cp.http(), cp.config());
    let m =
        pb::Mutation { expected_version: Some(version), ..mutation(&format!("power-{version}")) };
    let answer = if start {
        let asked =
            pb::StartRequest { mutation: m.into(), workload: w.into(), ..Default::default() };
        client.start(asked).await
    } else {
        let asked =
            pb::StopRequest { mutation: m.into(), workload: w.into(), ..Default::default() };
        client.stop(asked).await
    };
    let plan = answer.unwrap().into_owned().plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        cp.approve(&plan.id).await;
    }
    converged(cp, w).await;
}

/// What `path` holds, read in a session of `f`.
async fn read(f: &mut crate::files::Files, path: &str) -> Response {
    let read = pb::FileRead { path: path.into(), length: 64, ..Default::default() };
    f.ask(File::Read(Box::new(read))).await
}

#[tokio::test]
async fn a_microvm_s_files_are_reached_in_its_guest_and_stopped_in_a_service_guest() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_with_microvms(&cp, "vm-files").await else {
        eprintln!("this machine cannot run a guest: not run");
        return;
    };
    let agent = node.agent().await;
    let w = cp.declare(&project, "guest", &push("guest-files", recipe()).await).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let editor = cp.cli(Some(&cp.token_for(&[WRITE]).await));
    let mut f = open(&editor, &w).await;
    let list = pb::FileList { path: "/".into(), limit: 10, ..Default::default() };
    let Response::Entries(top) = f.ask(File::List(Box::new(list))).await else { panic!() };
    let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["data"], "the volume as the guest mounts it, and nothing of its image");
    assert_eq!(f.write("/data/hello.txt", 0, b"merhaba".to_vec()).await, Response::Written(7));
    assert_eq!(read(&mut f, "/data/hello.txt").await, Response::Data(b"merhaba".to_vec()));
    drop(f);

    if have("sftp") {
        let here = tempfile::tempdir().unwrap();
        let port = serve_sftp(&cp, here.path()).await;
        let key = here.path().join("id");
        person_with_key(&cp, &key).await;
        let world: Vec<u8> = (0..3 << 20).map(|i| u8::try_from(i % 251).unwrap()).collect();
        std::fs::write(here.path().join("world.bin"), &world).unwrap();
        let batch = "put world.bin /data/world.bin\nls -l /data\nget /data/world.bin back.bin\n";
        let (ok, said) = run(here.path(), (&w, port), &key, batch).await;
        assert!(ok, "{said}");
        assert!(said.contains("hello.txt"), "listed beside the session's file: {said}");
        assert_eq!(std::fs::read(here.path().join("back.bin")).unwrap(), world, "whole");
    }

    power(&cp, &w, false, 1).await;
    let mut f = open(&editor, &w).await;
    assert_eq!(read(&mut f, "/data/hello.txt").await, Response::Data(b"merhaba".to_vec()));
    assert_eq!(f.write("/data/stopped.txt", 0, b"kept".to_vec()).await, Response::Written(4));
    drop(f);
    power(&cp, &w, true, 2).await;
    let mut f = open(&editor, &w).await;
    let kept = read(&mut f, "/data/stopped.txt").await;
    assert_eq!(kept, Response::Data(b"kept".to_vec()), "written while it was stopped");
    drop(f);
    cp.retire(&w).await;
    agent.stop().await;
}
