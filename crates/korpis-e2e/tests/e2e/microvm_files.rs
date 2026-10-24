// SPDX-License-Identifier: AGPL-3.0-or-later

//! A running microVM's files on a real node (§4.4 of docs/04-runtimes.md, §3.1 of
//! docs/06-storage.md): its volume is a block device the host never mounts, so a file session
//! reaches it through the relay, the node, and the `firecracker` driver to the file worker inside
//! the guest. The volume is listed as the guest mounts it, written, and read back; and OpenSSH's
//! `sftp` puts a file there and gets it back whole, served the same way. Needs what the microVM
//! test needs, and OpenSSH's client for the second half.

use crate::files::open;
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::sftp::{have, person_with_key, run, serve_sftp};
use crate::support::{ControlPlane, converged};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;

const WRITE: &str = "workload.file.write";

/// A guest that only sleeps, with a volume at `/data`.
fn recipe() -> String {
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

#[tokio::test]
async fn a_running_microvm_s_files_are_reached_inside_its_guest_through_the_relay_and_sftp() {
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
    let read = pb::FileRead { path: "/data/hello.txt".into(), length: 64, ..Default::default() };
    assert_eq!(f.ask(File::Read(Box::new(read))).await, Response::Data(b"merhaba".to_vec()));
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
    cp.retire(&w).await;
    agent.stop().await;
}
