// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use super::*;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// A file of NVIDIA's shape: two GPUs, a MIG partition of the first, the aggregate `all`, and
/// the control node and a hook given with every device.
fn nvidia(hook: &Path) -> String {
    format!(
        r#"cdiVersion: 0.5.0
kind: nvidia.com/gpu
devices:
  - name: "0"
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
  - name: "1"
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia1
  - name: "0:0"
    annotations:
      profile: 1g.10gb
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
        - path: /dev/nvidia-caps/nvidia-cap21
  - name: all
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
        - path: /dev/nvidia1
containerEdits:
  env:
    - NVIDIA_VISIBLE_DEVICES=void
  deviceNodes:
    - path: /dev/nvidiactl
  hooks:
    - hookName: createContainer
      path: {}
      args: [nvidia-cdi-hook, update-ldcache]
"#,
        hook.display()
    )
}

/// What `dirs` describe, read as whoever owns the first, as a node's are by root.
fn read(dirs: &[&Path]) -> Described {
    let owner = std::fs::metadata(dirs[0]).unwrap().uid();
    describe(&dirs.iter().map(|d| d.to_path_buf()).collect::<Vec<_>>(), owner)
}

/// A program for hooks, writable by its owner alone.
fn program(dir: &Path) -> PathBuf {
    let p = dir.join("hook");
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Why `id`, a device or a path, is not found or read.
fn why<'a>(d: &'a Described, id: &str) -> &'a str {
    let device = d.devices.get(id).and_then(|f| f.as_ref().err());
    let unread = d.unread.iter().find(|(p, _)| p.ends_with(id)).map(|(_, w)| w);
    device.or(unread).map_or("", String::as_str)
}

#[test]
fn a_vendors_file_gives_its_gpus_and_partitions_and_not_its_aggregate() {
    let (specs, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(specs.path().join("nvidia.yaml"), nvidia(&program(bin.path()))).unwrap();
    let d = read(&[specs.path()]);
    let found: Vec<&str> =
        d.devices.iter().filter(|(_, f)| f.is_ok()).map(|(id, _)| id.as_str()).collect();
    assert_eq!(found, ["nvidia.com/gpu=0", "nvidia.com/gpu=0:0", "nvidia.com/gpu=1"]);
    let gpu = d.devices["nvidia.com/gpu=0"].as_ref().unwrap();
    assert_eq!(gpu.nodes, ["/dev/nvidia0"], "the control node is not its own");
    let partition = d.devices["nvidia.com/gpu=0:0"].as_ref().unwrap();
    assert_eq!(partition.attributes.get("profile").map(String::as_str), Some("1g.10gb"));
    assert!(why(&d, "nvidia.com/gpu=all").contains("aggregate of 2"), "{d:?}");
    let nodes: Vec<&str> = gpu.edits.device_nodes.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(nodes, ["/dev/nvidiactl", "/dev/nvidia0"], "the file's edits, then its own");
    assert_eq!((gpu.edits.env.len(), gpu.edits.hooks.len()), (1, 1));
}

#[test]
fn a_file_or_hook_others_may_write_is_not_read_and_nor_is_a_field_cdi_lacks() {
    let (specs, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let hook = program(bin.path());
    let at = specs.path().join("nvidia.yaml");
    std::fs::write(&at, nvidia(&hook)).unwrap();
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o666)).unwrap();
    let d = read(&[specs.path()]);
    assert!(d.devices.is_empty() && why(&d, "nvidia.yaml").contains("may write"), "{d:?}");
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o777)).unwrap();
    let d = read(&[specs.path()]);
    assert!(why(&d, "nvidia.com/gpu=0").contains("a hook is not run"), "{d:?}");
    let typo = r#"{"cdiVersion":"1.1.0","kind":"a.com/gpu","devices":[{"name":"0",
        "containerEdits":{"deviceNodes":[{"path":"/dev/a0"}],"devicenodes":[]}}]}"#;
    std::fs::remove_file(&at).unwrap();
    std::fs::write(specs.path().join("a.json"), typo).unwrap();
    let d = read(&[specs.path()]);
    assert!(why(&d, "a.json").contains("devicenodes"), "{d:?}");
}

/// A file of kind `kind` with one device, `name`, giving `node`.
fn one(kind: &str, name: &str, node: &str) -> String {
    format!(
        r#"{{"cdiVersion":"0.6.0","kind":"{kind}","annotations":{{"vendor":"x"}},
            "devices":[{{"name":"{name}",
                "containerEdits":{{"deviceNodes":[{{"path":"{node}"}}]}}}}]}}"#
    )
}

#[test]
fn a_later_directory_replaces_a_device_and_one_directory_describing_it_twice_gives_neither() {
    let (low, high) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(low.path().join("a.json"), one("a.com/gpu", "0", "/dev/low")).unwrap();
    std::fs::write(high.path().join("a.json"), one("a.com/gpu", "0", "/dev/high")).unwrap();
    let d = read(&[low.path(), high.path()]);
    assert_eq!(d.devices["a.com/gpu=0"].as_ref().unwrap().nodes, ["/dev/high"]);
    std::fs::write(high.path().join("b.json"), one("a.com/gpu", "0", "/dev/other")).unwrap();
    let d = read(&[low.path(), high.path()]);
    assert!(why(&d, "a.com/gpu=0").contains("twice"), "{d:?}");
}

#[test]
fn a_device_with_no_node_of_its_own_a_newer_version_and_a_missing_directory_are_said_plainly() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("n.json"),
        r#"{"cdiVersion":"1.0.0","kind":"b.com/gpu",
        "devices":[{"name":"0","containerEdits":{"env":["A=1"]}}]}"#,
    )
    .unwrap();
    let newer = one("a.com/gpu", "0", "/dev/a").replace("0.6.0", "1.2.0");
    std::fs::write(dir.path().join("a.json"), newer).unwrap();
    let d = read(&[dir.path(), &dir.path().join("missing")]);
    assert!(why(&d, "b.com/gpu=0").contains("no device node"), "{d:?}");
    assert!(why(&d, "a.json").contains("1.2.0"), "{d:?}");
    assert_eq!(d.unread.len(), 1, "a directory that is not there is not a fault");
}

#[test]
fn edits_merge_in_order_and_a_zero_group_is_dropped() {
    let file: spec::Edits =
        serde_json::from_str(r#"{"env":["A=1"],"additionalGIDs":[0,44]}"#).unwrap();
    let own: spec::Edits =
        serde_json::from_str(r#"{"env":["B=2"],"additionalGIDs":[45],"intelRdt":{}}"#).unwrap();
    let merged = file.merged(&own);
    assert_eq!(
        (merged.env, merged.additional_gids),
        (vec!["A=1".into(), "B=2".into()], vec![44, 45])
    );
    assert!(merged.intel_rdt.is_some());
}
